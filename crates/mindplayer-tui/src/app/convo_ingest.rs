//! Background driver for the conversation log (see [`crate::convo_log`]).
//!
//! Ingest is I/O bound and the first pass has ~4.5 GB of transcripts to walk,
//! so it never runs on the UI thread and never runs as one pass: each spawn
//! takes a bounded batch.
//!
//! The index is saved after every SESSION, not every batch. Turns are appended
//! as each session is ingested, so an unsaved watermark is not merely work to
//! redo — the next launch would append those same turns again. The worker is
//! detached, so quitting kills it wherever it is; per-session saves bound the
//! exposure to the one session in flight.

use super::{App, ConvoIngestResult};
use crate::convo_log::{self, ConvoIndex};
use mindplayer_core::Session;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Sessions per background batch. Small enough that a batch containing one of
/// the 600 MB outliers still finishes promptly and saves progress.
const BATCH: usize = 24;

/// Minimum gap between pending-scans. Matches the cadence of the other
/// interval-gated work in the tick loop; a transcript that grew is picked up
/// within this long, which is imperceptible for a background log.
const CONVO_SCAN_INTERVAL: Duration = Duration::from_secs(3);

impl App {
    /// Sessions whose source has bytes the log hasn't consumed yet.
    fn convo_pending(&self) -> Vec<Session> {
        self.all_sessions
            .iter()
            .filter(|s| self.convo_is_pending(s))
            .take(BATCH)
            .cloned()
            .collect()
    }

    /// Does this session have bytes the log has not consumed?
    ///
    /// Goes through [`convo_log::source_of`] rather than stat-ing `s.file`, so
    /// the length compared here is the length of the file `ingest` will actually
    /// read. Comparing a kiro sidecar against a jsonl watermark made all 120
    /// kiro sessions permanently pending, which starved the batch.
    fn convo_is_pending(&self, session: &Session) -> bool {
        let Some((_, len)) = convo_log::source_of(session) else {
            return false;
        };
        match self.convo_index.sessions.get(&session.id) {
            Some(m) => m.watermark != len,
            None => true,
        }
    }

    /// Kicks off one batch if none is in flight and anything needs ingesting.
    ///
    /// Interval-gated because the pending check itself is the expensive part:
    /// `.take(BATCH)` short-circuits only while a backlog exists, so in steady
    /// state it stats every discovered session to find nothing. Measured at
    /// 3.08 ms for 762 sessions — called from the 16 ms tick that would be
    /// ~19% of a core spent discovering there is no work.
    pub fn spawn_convo_ingest(&mut self) -> bool {
        if self.convo_rx.is_some() {
            return false;
        }
        if self
            .convo_last_scan
            .is_some_and(|t| t.elapsed() < CONVO_SCAN_INTERVAL)
        {
            return false;
        }
        self.convo_last_scan = Some(Instant::now());
        let batch = self.convo_pending();
        if batch.is_empty() {
            return false;
        }
        let dir = self.convo_dir.clone();
        let index = self.convo_index.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(ingest_batch(dir, index, batch));
        });
        self.convo_rx = Some(rx);
        true
    }

    /// Adopts a finished batch. Returns true when turns were actually added, so
    /// the caller only redraws when a counter changed.
    pub fn poll_convo_ingest(&mut self) -> bool {
        let Some(rx) = &self.convo_rx else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };
        self.convo_rx = None;
        self.convo_index = result.index;
        self.convo_ingested += result.turns;
        // No save here: the worker already wrote the index inside the directory
        // lock. Saving again from this thread would be an unlocked write, and
        // would re-serialize ~320 KB plus two fsyncs on every completed batch.
        result.turns > 0
    }

    /// Total turns recorded across every session the log knows about.
    ///
    /// No UI reads this yet; it is how the tests below assert ingest progress.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn convo_total_turns(&self) -> usize {
        self.convo_index.total_turns()
    }

    /// Sessions still waiting on a first or incremental ingest.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn convo_backlog(&self) -> usize {
        self.all_sessions
            .iter()
            .filter(|s| self.convo_is_pending(s))
            .count()
    }
}

/// Runs on the worker thread. A session that fails to ingest is skipped.
///
/// Two things make this safe against a second mindplayer instance:
///
/// 1. The directory lock is held for the whole ingest + save window, so no peer
///    can append to the same turn files or stage the same index temp at once.
/// 2. The index is re-loaded FROM DISK under that lock rather than trusting the
///    caller's copy, which was read when this process started and may be many
///    ingests out of date. Without the reload, this instance would re-ingest
///    from offset 0 everything a peer had already recorded.
///
/// When the lock is held elsewhere the batch is abandoned; `fallback` is handed
/// straight back so the UI keeps the index it already had, and the next pass
/// retries.
fn ingest_batch(
    dir: std::path::PathBuf,
    fallback: ConvoIndex,
    batch: Vec<Session>,
) -> ConvoIngestResult {
    let Some(_lock) = convo_log::ConvoLock::try_acquire(&dir) else {
        return ConvoIngestResult {
            index: fallback,
            turns: 0,
        };
    };
    let mut index = convo_log::load_index(&dir);
    let mut turns = 0usize;
    for session in &batch {
        match convo_log::ingest(&dir, &mut index, session) {
            Ok(got) => turns += got.turns,
            Err(_) => continue,
        }
        // Saved after EACH session, not once after the batch. Turns are appended
        // per session, so a watermark that is not yet on disk when this process
        // dies — the worker is detached, so quitting kills it mid-batch — makes
        // the next launch re-append those same turns. Per-session saves bound
        // that to the single session in flight.
        //
        // A failed save is fatal for the batch: continuing would let the caller
        // adopt an index whose advances are not on disk, and every restart would
        // re-append. Handing back `fallback` keeps the UI on a view that matches
        // the disk, and the next pass retries.
        if convo_log::save_index(&dir, &index).is_err() {
            return ConvoIngestResult {
                index: fallback,
                turns: 0,
            };
        }
    }
    ConvoIngestResult { index, turns }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::isolated_app;

    fn write_claude(path: &std::path::Path, turns: &[(&str, &str)]) {
        let mut body = String::new();
        for (role, text) in turns {
            body.push_str(&format!(
                r#"{{"type":"{role}","message":{{"role":"{role}","content":[{{"type":"text","text":{}}}]}}}}"#,
                serde_json::to_string(text).unwrap()
            ));
            body.push('\n');
        }
        std::fs::write(path, body).unwrap();
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mindplayer-convo-ingest-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn session_with(id: &str, file: std::path::PathBuf) -> Session {
        Session {
            id: id.into(),
            agent: mindplayer_core::Agent::Claude,
            cwd: std::path::PathBuf::from("/tmp/proj"),
            file,
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: mindplayer_core::TokenUsage::default(),
            title: "t".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    /// Drives one spawn/poll cycle to completion without a UI loop. Clears the
    /// scan stamp first so the interval gate doesn't suppress a deliberate pass.
    fn run_one_batch(app: &mut App) -> bool {
        app.convo_last_scan = None;
        if !app.spawn_convo_ingest() {
            return false;
        }
        for _ in 0..2000 {
            if app.poll_convo_ingest() {
                return true;
            }
            if app.convo_rx.is_none() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("ingest batch never completed");
    }

    #[test]
    fn a_batch_ingests_pending_sessions_and_clears_the_backlog() {
        let dir = scratch("basic");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");

        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "hi"), ("assistant", "hello")]);
        app.all_sessions = vec![session_with("s-a", src)];

        assert_eq!(app.convo_backlog(), 1);
        assert!(run_one_batch(&mut app), "turns were added");
        assert_eq!(app.convo_total_turns(), 2);
        assert_eq!(app.convo_ingested, 2);
        assert_eq!(app.convo_backlog(), 0, "nothing left to ingest");
    }

    #[test]
    fn a_second_pass_over_unchanged_sources_does_nothing() {
        let dir = scratch("noop");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");

        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "only")]);
        app.all_sessions = vec![session_with("s-a", src)];

        run_one_batch(&mut app);
        assert_eq!(app.convo_ingested, 1);
        // Backlog is empty, so no batch is even spawned. Gate cleared first so
        // this asserts "nothing pending", not "too soon since the last scan".
        app.convo_last_scan = None;
        assert!(!app.spawn_convo_ingest());
        assert_eq!(app.convo_ingested, 1);
        assert_eq!(app.convo_total_turns(), 1);
    }

    fn kiro_session(id: &str, dir: &std::path::Path) -> Session {
        let sidecar = dir.join(format!("{id}.json"));
        let jsonl = dir.join(format!("{id}.jsonl"));
        // Deliberately different lengths — on this machine all 120 real kiro
        // pairs differ, which is what made the sidecar comparison never match.
        std::fs::write(&sidecar, r#"{"updated_at":"2026-08-04T00:00:00Z"}"#).unwrap();
        std::fs::write(
            &jsonl,
            "{\"kind\":\"Prompt\",\"data\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let mut s = session_with(id, sidecar);
        s.agent = mindplayer_core::Agent::Kiro;
        s
    }

    /// The regression the first kiro fix left behind: `ingest` measured the
    /// adjacent `.jsonl` while the scan still stat-ed the `.json` sidecar, so a
    /// kiro session compared a sidecar length against a jsonl watermark, never
    /// matched, and stayed pending forever — filling every batch with no-ops and
    /// starving the remaining claude/codex backfill.
    #[test]
    fn a_fully_ingested_kiro_session_stops_being_pending() {
        let dir = scratch("kiropending");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");
        app.all_sessions = vec![kiro_session("k1", &dir)];

        assert_eq!(app.convo_backlog(), 1, "not ingested yet");
        assert!(run_one_batch(&mut app), "its turn is ingested");
        assert_eq!(app.convo_total_turns(), 1);
        assert_eq!(
            app.convo_backlog(),
            0,
            "a kiro session whose jsonl is fully consumed must not stay pending"
        );
        app.convo_last_scan = None;
        assert!(
            !app.spawn_convo_ingest(),
            "and must not keep occupying a batch slot"
        );
    }

    /// A kiro sidecar with no adjacent jsonl has nothing to ingest, so it must
    /// drop out of the scan rather than be reported pending on every pass.
    #[test]
    fn a_kiro_sidecar_without_a_jsonl_is_not_pending() {
        let dir = scratch("kirobare2");
        let sidecar = dir.join("k2.json");
        std::fs::write(&sidecar, r#"{"updated_at":"x"}"#).unwrap();
        let mut s = session_with("k2", sidecar);
        s.agent = mindplayer_core::Agent::Kiro;

        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");
        app.all_sessions = vec![s];

        assert_eq!(app.convo_backlog(), 0);
        assert!(!app.spawn_convo_ingest(), "nothing to do, no batch");
    }

    /// Turns are appended per session, so an unsaved watermark means the next
    /// launch re-appends. The index must be on disk before the next session.
    #[test]
    fn each_session_is_persisted_before_the_batch_moves_on() {
        let dir = scratch("persession");
        let convo = dir.join("convo");
        let mut app = isolated_app();
        app.convo_dir = convo.clone();
        let mut sessions = Vec::new();
        for i in 0..3 {
            let src = dir.join(format!("s{i}.jsonl"));
            write_claude(&src, &[("user", "turn")]);
            sessions.push(session_with(&format!("s-{i}"), src));
        }
        app.all_sessions = sessions;
        run_one_batch(&mut app);

        // Simulate a restart: nothing is re-ingested, so every session's
        // watermark reached disk, not just the last one's.
        let on_disk = convo_log::load_index(&convo);
        assert_eq!(on_disk.sessions.len(), 3, "all three persisted");
        assert_eq!(on_disk.total_turns(), 3);
        let mut restarted = isolated_app();
        restarted.convo_dir = convo;
        restarted.convo_index = on_disk;
        restarted.all_sessions = app.all_sessions.clone();
        assert_eq!(restarted.convo_backlog(), 0);
    }

    /// `save_index` failing used to be swallowed while the caller still adopted
    /// the advanced index, so the turns were on disk but the watermarks were not
    /// — and every restart re-appended them.
    #[test]
    fn a_batch_that_cannot_persist_its_index_does_not_advance_the_ui() {
        let dir = scratch("saveerr");
        let convo = dir.join("convo");
        std::fs::create_dir_all(&convo).unwrap();
        // A DIRECTORY where index.json belongs: the turn file still writes and
        // the temp still stages, but the rename onto a directory fails.
        std::fs::create_dir_all(convo.join("index.json")).unwrap();
        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "unpersistable")]);
        let mut app = isolated_app();
        app.convo_dir = convo.clone();
        app.all_sessions = vec![session_with("s-a", src)];

        run_one_batch(&mut app);

        assert!(
            convo_log::save_index(&convo, &convo_log::ConvoIndex::default()).is_err(),
            "test setup must actually make the save fail"
        );
        assert_eq!(
            app.convo_total_turns(),
            0,
            "the UI must not adopt watermarks that are not on disk"
        );
        assert_eq!(app.convo_ingested, 0);
        // Still pending, so the next pass retries rather than losing the work.
        assert_eq!(app.convo_backlog(), 1);
    }

    /// A fetch that never answers used to hold `limits_rx` for the life of the
    /// process, leaving the popup on "…" with no retry path.
    #[test]
    fn a_wedged_limits_fetch_is_abandoned_once_the_deadline_passes() {
        use crate::app::session_list::LIMITS_FETCH_DEADLINE;
        let mut app = isolated_app();

        // Stand in for a worker that will never send: a receiver whose sender is
        // gone, which `try_recv` reports as disconnected rather than ready.
        let (tx, rx) = mpsc::channel();
        drop(tx);
        app.limits_rx = Some(rx);

        // Inside the deadline: the slot is held and the clock is NOT restarted.
        let fresh = Instant::now();
        app.limits_started = Some(fresh);
        app.spawn_limits_fetch();
        assert_eq!(
            app.limits_started,
            Some(fresh),
            "a spawn refused inside the deadline must not reset the clock"
        );

        // Past the deadline: the stale channel is dropped and a new fetch starts.
        let stale = Instant::now()
            .checked_sub(LIMITS_FETCH_DEADLINE + Duration::from_secs(1))
            .expect("clock far enough from boot");
        app.limits_started = Some(stale);
        app.spawn_limits_fetch();
        assert!(
            app.limits_started.is_some_and(|t| t != stale),
            "the deadline must free the slot and restart the clock"
        );
        assert!(app.limits_rx.is_some(), "a replacement fetch is in flight");
    }

    /// The pending scan stats every discovered session, so running it on every
    /// tick cost ~19% of a core just to find nothing to do.
    #[test]
    fn the_pending_scan_is_interval_gated() {
        let dir = scratch("gate");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");
        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "x")]);
        app.all_sessions = vec![session_with("s-a", src)];

        assert!(app.spawn_convo_ingest(), "first pass runs");
        for _ in 0..2000 {
            if app.poll_convo_ingest() || app.convo_rx.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // A fresh pending item, but the gate must still suppress the scan.
        let src2 = dir.join("b.jsonl");
        write_claude(&src2, &[("user", "y")]);
        app.all_sessions.push(session_with("s-b", src2));
        assert!(
            !app.spawn_convo_ingest(),
            "a scan within the interval is refused even with work waiting"
        );
        app.convo_last_scan = None;
        assert!(
            app.spawn_convo_ingest(),
            "and proceeds once the gate expires"
        );
    }

    /// CRITICAL: two instances used to duplicate every turn. The lock makes the
    /// loser abandon its batch instead of appending over the winner's lines.
    #[test]
    fn a_batch_is_abandoned_while_another_process_holds_the_lock() {
        let dir = scratch("locked");
        let convo = dir.join("convo");
        let mut app = isolated_app();
        app.convo_dir = convo.clone();
        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "contested")]);
        app.all_sessions = vec![session_with("s-a", src)];

        // Stand in for the peer instance holding the directory lock.
        let held = convo_log::ConvoLock::try_acquire(&convo).expect("first acquire wins");
        assert!(
            convo_log::ConvoLock::try_acquire(&convo).is_none(),
            "the lock is exclusive"
        );

        assert!(!run_one_batch(&mut app), "no turns ingested while locked");
        assert_eq!(app.convo_total_turns(), 0);
        assert!(
            convo_log::read_turns(&convo, "s-a").is_empty(),
            "and nothing was appended to the turn file"
        );

        drop(held);
        assert!(
            run_one_batch(&mut app),
            "the next pass succeeds once released"
        );
        assert_eq!(app.convo_total_turns(), 1);
    }

    /// The worker must read the index from disk under the lock, not trust the
    /// copy this process loaded at startup — otherwise it re-ingests from 0
    /// everything a peer already recorded.
    #[test]
    fn the_worker_adopts_a_peers_progress_instead_of_re_ingesting_it() {
        let dir = scratch("peer");
        let convo = dir.join("convo");
        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "one"), ("assistant", "two")]);

        // A "peer" ingests the whole thing and persists the index.
        {
            let mut peer_idx = convo_log::ConvoIndex::default();
            convo_log::ingest(&convo, &mut peer_idx, &session_with("s-a", src.clone())).unwrap();
            convo_log::save_index(&convo, &peer_idx).unwrap();
        }

        // This instance still holds an empty index, as if it started earlier.
        let mut app = isolated_app();
        app.convo_dir = convo.clone();
        app.convo_index = convo_log::ConvoIndex::default();
        app.all_sessions = vec![session_with("s-a", src)];
        assert_eq!(app.convo_backlog(), 1, "it believes work is outstanding");

        run_one_batch(&mut app);

        assert_eq!(
            convo_log::read_turns(&convo, "s-a").len(),
            2,
            "the peer's turns are not duplicated"
        );
        assert_eq!(app.convo_total_turns(), 2, "and its progress is adopted");
        assert_eq!(app.convo_ingested, 0, "nothing new was appended here");
    }

    #[test]
    fn appended_transcript_bytes_are_picked_up_by_the_next_batch() {
        let dir = scratch("append");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");

        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "first")]);
        app.all_sessions = vec![session_with("s-a", src.clone())];
        run_one_batch(&mut app);

        write_claude(&src, &[("user", "first"), ("assistant", "second")]);
        assert_eq!(app.convo_backlog(), 1, "grown source is pending again");
        assert!(run_one_batch(&mut app));
        assert_eq!(app.convo_total_turns(), 2);
    }

    #[test]
    fn the_index_is_persisted_so_a_restart_does_not_re_ingest() {
        let dir = scratch("persist");
        let convo = dir.join("convo");
        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "durable")]);

        let mut app = isolated_app();
        app.convo_dir = convo.clone();
        app.all_sessions = vec![session_with("s-a", src.clone())];
        run_one_batch(&mut app);

        // Fresh App over the same log directory: the watermark must survive.
        let mut restarted = isolated_app();
        restarted.convo_dir = convo.clone();
        restarted.convo_index = convo_log::load_index(&convo);
        restarted.all_sessions = vec![session_with("s-a", src)];

        assert_eq!(restarted.convo_total_turns(), 1);
        assert_eq!(restarted.convo_backlog(), 0);
        assert!(!restarted.spawn_convo_ingest(), "nothing to redo");
    }

    #[test]
    fn only_one_batch_runs_at_a_time() {
        let dir = scratch("serial");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");

        let src = dir.join("a.jsonl");
        write_claude(&src, &[("user", "x")]);
        app.all_sessions = vec![session_with("s-a", src)];

        assert!(app.spawn_convo_ingest());
        assert!(
            !app.spawn_convo_ingest(),
            "a second spawn while one is in flight must be refused"
        );
        for _ in 0..2000 {
            if app.poll_convo_ingest() || app.convo_rx.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(app.convo_rx.is_none());
    }

    #[test]
    fn a_batch_is_capped_so_a_large_backlog_is_drained_over_several_passes() {
        let dir = scratch("cap");
        let mut app = isolated_app();
        app.convo_dir = dir.join("convo");

        let total = BATCH + 5;
        let mut sessions = Vec::new();
        for i in 0..total {
            let src = dir.join(format!("s{i}.jsonl"));
            write_claude(&src, &[("user", "turn")]);
            sessions.push(session_with(&format!("s-{i}"), src));
        }
        app.all_sessions = sessions;

        assert_eq!(app.convo_backlog(), total);
        run_one_batch(&mut app);
        assert_eq!(
            app.convo_total_turns(),
            BATCH,
            "first pass ingests exactly one batch"
        );
        assert_eq!(app.convo_backlog(), 5);
        run_one_batch(&mut app);
        assert_eq!(app.convo_total_turns(), total);
        assert_eq!(app.convo_backlog(), 0);
    }
}
