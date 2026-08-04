//! Durable conversation log: mirrors every user/assistant turn out of the
//! agents' own transcripts into one normalized store under `~/.mindplayer/convo`.
//!
//! Why a copy at all, when the CLIs already keep transcripts: theirs are
//! per-agent formats that rotate and get cleared, and reading them means
//! re-parsing 4.5 GB. This store keeps only the conversation — measured at
//! 1.5–8% of transcript bytes, ~320 MB for the full history here — in one
//! shape across claude/codex/kiro, and grows incrementally by byte watermark.
//!
//! Every entry point takes `dir` explicitly. Nothing here resolves
//! `MINDPLAYER_CONVO_DIR` or `$HOME` on its own, so a test can never append to
//! the real user's log by forgetting to set an env var.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mindplayer_core::{Agent, Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::handoff::{neutralize_controls, parse_turn_for, safe_id};

/// Bump when the on-disk shape changes incompatibly.
pub const INDEX_VERSION: u32 = 1;

/// Refuse an index file larger than this BEFORE parsing it, so a corrupt or
/// hostile file can't exhaust memory during deserialization. Measured: 1,365
/// real sessions cost 572 KB (~429 bytes each), so this leaves room for well
/// over a hundred thousand sessions while still bounding the damage.
pub const MAX_INDEX_BYTES: u64 = 64 << 20;

/// Defense-in-depth cap on session count after parse. The byte cap above
/// already bounds this; this catches a file that is small but pathological.
pub const MAX_SESSIONS: usize = 200_000;

/// One recorded turn. No timestamp: the agents' turn records don't carry one
/// consistently, so order is the only reliable signal and `seq` carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvoTurn {
    pub seq: usize,
    pub role: String,
    pub text: String,
}

/// Per-session ingest bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvoMeta {
    pub agent: String,
    pub cwd: PathBuf,
    /// Transcript this was ingested from, for provenance and re-ingest.
    pub source: PathBuf,
    /// Source length already consumed — the next read starts here.
    pub watermark: u64,
    pub turns: usize,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvoIndex {
    pub version: u32,
    #[serde(default)]
    pub sessions: BTreeMap<String, ConvoMeta>,
}

impl Default for ConvoIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            sessions: BTreeMap::new(),
        }
    }
}

impl ConvoIndex {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn total_turns(&self) -> usize {
        self.sessions.values().map(|m| m.turns).sum()
    }
}

/// What one `ingest` call did. `turns == 0` means the source had nothing new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ingested {
    pub turns: usize,
    pub bytes: u64,
}

/// Real-build log directory: `$MINDPLAYER_CONVO_DIR`, else `~/.mindplayer/convo`.
pub fn default_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MINDPLAYER_CONVO_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("convo")
}

fn index_path(dir: &Path) -> PathBuf {
    dir.join("index.json")
}

/// Exclusive advisory lock over one convo directory, held across ingest + save.
///
/// Several mindplayer instances routinely run at once. Without this, each loads
/// the index at its own startup, each sees watermark 0 for a session it hasn't
/// ingested, and both append the same turns to the same file: a measured 74,045
/// duplicated turns across 1,729 of 1,952 files in a 45-second two-instance run,
/// plus mutually truncated index temp files.
///
/// `flock` is released by the kernel when the fd closes, so an instance that
/// crashes mid-ingest cannot leave the store locked forever — which a
/// pid-file scheme would.
///
/// Unix only. On any other target [`Self::try_acquire`] succeeds without taking
/// a lock, so concurrent instances there would still corrupt the store. That is
/// stated rather than fixed because the rest of the crate is already unix-bound
/// (pty handling, `libc`), so a non-unix build does not currently exist.
pub struct ConvoLock {
    #[allow(dead_code)]
    file: File,
}

impl ConvoLock {
    /// `None` when another process holds the lock. The caller skips this pass
    /// instead of waiting, so no UI thread ever blocks on a peer's ingest.
    pub fn try_acquire(dir: &Path) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join(".lock"))
            .ok()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                return None;
            }
        }
        Some(Self { file })
    }
}

fn turns_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.jsonl", safe_id(id)))
}

/// Missing, empty, or corrupt index reads back as empty rather than failing —
/// a lost index only costs a re-ingest, and refusing to start would be worse.
///
/// The file is untrusted input: its size is checked against [`MAX_INDEX_BYTES`]
/// before any parsing, and [`MAX_SESSIONS`] is enforced after, so neither a
/// truncated nor a deliberately huge file can turn startup into an OOM.
pub fn load_index(dir: &Path) -> ConvoIndex {
    let path = index_path(dir);
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > MAX_INDEX_BYTES => return ConvoIndex::default(),
        Ok(_) => {}
        Err(_) => return ConvoIndex::default(),
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return ConvoIndex::default();
    };
    match serde_json::from_str::<ConvoIndex>(&raw) {
        Ok(idx) if idx.version == INDEX_VERSION && idx.sessions.len() <= MAX_SESSIONS => idx,
        _ => ConvoIndex::default(),
    }
}

/// Durable write: temp file → `fsync` → rename → `fsync` the parent directory.
///
/// The parent `fsync` is the part a plain temp+rename misses — without it the
/// rename itself can be lost on power failure, leaving the previous index even
/// though the new one was written. A crash mid-write always leaves the previous
/// good index intact rather than a half-parsed one.
pub fn save_index(dir: &Path, idx: &ConvoIndex) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = index_path(dir);
    // Per-process temp name. A shared `index.json.tmp` let two instances
    // truncate each other's staging file, so one could rename the other's
    // half-written content over the live index.
    let tmp = dir.join(format!("index.json.tmp-{}", std::process::id()));
    let body = serde_json::to_string_pretty(idx)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut f = File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &target)?;
    // Best-effort: a filesystem that can't fsync a directory handle must not
    // turn a successful save into an error.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Appends whatever the source gained since its watermark.
///
/// Kiro keeps a whole-file JSON sidecar rather than an append-only jsonl, so a
/// byte offset means nothing there: its log is rewritten from scratch whenever
/// the source length changes, and skipped when it hasn't.
pub fn ingest(dir: &Path, idx: &mut ConvoIndex, session: &Session) -> std::io::Result<Ingested> {
    let Some((source, len)) = source_of(session) else {
        return Ok(Ingested::default());
    };
    let prev = idx.sessions.get(&session.id);
    let mark = prev.map(|m| m.watermark).unwrap_or(0);

    // A shorter source was rotated; a different source path is a different
    // conversation under a reused id. Either way the recorded turns are stale,
    // so the log is rebuilt instead of appended to — resuming at the old offset
    // would read a file this session has never seen from the middle.
    let rebuild = len < mark || prev.is_some_and(|m| m.source != source);
    if !rebuild && len == mark {
        return Ok(Ingested::default());
    }

    let from = if rebuild { 0 } else { mark };
    let start_seq = if rebuild {
        0
    } else {
        prev.map(|m| m.turns).unwrap_or(0)
    };

    // The read is bounded by the same `len` the watermark is derived from, and
    // `consumed` counts only whole lines. Without both, a turn appended between
    // the `metadata` call above and EOF would be recorded here yet left outside
    // the watermark, so the next pass would read and append it a second time
    // under a fresh `seq`.
    // `saturating_sub`, not `-`: the guards above do keep `from <= len`, but an
    // underflow here would wrap to a huge bound in release mode and silently
    // restore the unbounded read this whole block exists to prevent. Not worth
    // leaving that to a two-line-away invariant.
    let (turns, consumed) = read_turns_from(session.agent, &source, from, len.saturating_sub(from));
    let watermark = from + consumed;

    if turns.is_empty() && !rebuild {
        // Consumed bytes carried nothing quotable. Advance so they are not
        // re-scanned, and leave the existing log alone.
        upsert(idx, session, &source, watermark, start_seq);
        return Ok(Ingested::default());
    }

    std::fs::create_dir_all(dir)?;
    let path = turns_path(dir, &session.id);
    let file = if rebuild {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?
    } else {
        OpenOptions::new().create(true).append(true).open(&path)?
    };
    // Buffered, and the newline is part of the same buffer, so a turn is not
    // split into payload-then-"\n" the way `writeln!` on a bare `File` splits it.
    // That is a reduction in syscalls, NOT an atomicity guarantee: `write_all`
    // still loops on a short write, and a line past the 8 KiB buffer goes
    // straight through. What actually prevents two appending processes from
    // interleaving is [`ConvoLock`].
    let mut out = BufWriter::new(file);
    let mut written = 0u64;
    let mut count = 0usize;
    for (i, (role, text)) in turns.into_iter().enumerate() {
        let turn = ConvoTurn {
            seq: start_seq + i,
            role,
            text,
        };
        let Ok(mut line) = serde_json::to_string(&turn) else {
            continue;
        };
        line.push('\n');
        out.write_all(line.as_bytes())?;
        written += line.len() as u64;
        count += 1;
    }
    out.flush()?;

    upsert(idx, session, &source, watermark, start_seq + count);
    Ok(Ingested {
        turns: count,
        bytes: written,
    })
}

/// The file this session's turns come from, together with its current length.
///
/// The single place that answers "which file, and how big is it". `ingest` and
/// the pending scan both go through here so they cannot disagree — they did:
/// `ingest` was fixed to measure kiro's adjacent `.jsonl` while the scan kept
/// stat-ing the `.json` sidecar, so every kiro session compared a sidecar length
/// against a jsonl watermark, never matched, and stayed pending forever.
///
/// `None` when there is nothing to ingest: no path, no adjacent jsonl for a kiro
/// sidecar, an unreadable file, or an empty one.
pub(crate) fn source_of(session: &Session) -> Option<(PathBuf, u64)> {
    let path = source_file(session)?;
    let len = std::fs::metadata(&path).ok()?.len();
    (len > 0).then_some((path, len))
}

/// The file whose bytes are actually parsed for turns.
///
/// Kiro's [`Session::file`] points at a metadata sidecar — discovery collects
/// only `*.json` — while the conversation lives in an adjacent `<id>.jsonl`.
/// Measuring and watermarking the sidecar while parsing the jsonl meant a
/// sidecar rewrite of unchanged length hid new turns permanently, and any
/// sidecar length change re-read and rewrote the whole log. Both now key off
/// this single path, and the jsonl is append-only like the other agents'.
fn source_file(session: &Session) -> Option<PathBuf> {
    if session.file.as_os_str().is_empty() {
        return None;
    }
    if session.agent == Agent::Kiro {
        let adjacent = session.file.with_extension("jsonl");
        return adjacent.exists().then_some(adjacent);
    }
    Some(session.file.clone())
}

/// Record where this session's ingest got to. Every field that can drift is
/// refreshed on all paths, so a session never keeps a stale agent or cwd.
fn upsert(idx: &mut ConvoIndex, session: &Session, source: &Path, watermark: u64, turns: usize) {
    let now = Utc::now();
    let entry = idx
        .sessions
        .entry(session.id.clone())
        .or_insert_with(|| ConvoMeta {
            agent: session.agent.as_str().to_string(),
            cwd: session.cwd.clone(),
            source: source.to_path_buf(),
            watermark: 0,
            turns: 0,
            first_seen: now,
            last_seen: now,
        });
    entry.agent = session.agent.as_str().to_string();
    entry.cwd = session.cwd.clone();
    entry.source = source.to_path_buf();
    entry.watermark = watermark;
    entry.turns = turns;
    entry.last_seen = now;
}

/// Every recorded turn for one session, in order. Unreadable or absent → empty.
///
/// The TUI has no reader surface yet — the store is plain JSONL, so it is read
/// with ordinary tools today. This is what the tests above verify writes with.
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_turns(dir: &Path, id: &str) -> Vec<ConvoTurn> {
    let Ok(file) = File::open(turns_path(dir, id)) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<ConvoTurn>(&l).ok())
        .collect()
}

/// Pulls `(role, text)` pairs out of the source starting at `from`, using the
/// same per-agent parsers the handoff path already relies on.
/// Returns the turns found plus the number of bytes consumed, counting only
/// COMPLETE lines.
///
/// A trailing chunk with no newline is a line the writer is still appending: its
/// bytes stay outside the returned count so the next pass re-reads the line
/// whole. Without that, the watermark could land mid-line and the remainder
/// would be seeked past and lost forever.
fn read_turns_from(agent: Agent, path: &Path, from: u64, max: u64) -> (Vec<(String, String)>, u64) {
    let parse = parse_turn_for(agent);
    let Ok(mut file) = File::open(path) else {
        return (Vec::new(), 0);
    };
    if from > 0 && file.seek(SeekFrom::Start(from)).is_err() {
        return (Vec::new(), 0);
    }
    let mut reader = BufReader::new(file.take(max));
    let mut out = Vec::new();
    let mut consumed = 0u64;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if buf.last() != Some(&b'\n') {
                    break; // partial final line — leave it for the next pass
                }
                consumed += n as u64;
            }
            Err(_) => break,
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some((role, text)) = parse(&v) {
            let text = neutralize_controls(text.trim());
            if text.is_empty() {
                continue;
            }
            out.push((role, text));
        }
    }
    (out, consumed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mindplayer-convo-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn claude_session(file: PathBuf) -> Session {
        Session {
            id: "sess-1".into(),
            agent: Agent::Claude,
            cwd: PathBuf::from("/tmp/proj"),
            file,
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: mindplayer_core::TokenUsage::default(),
            title: "convo log test".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    fn write_claude(path: &Path, turns: &[(&str, &str)]) {
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

    fn append_claude(path: &Path, turns: &[(&str, &str)]) {
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        for (role, text) in turns {
            writeln!(
                f,
                r#"{{"type":"{role}","message":{{"role":"{role}","content":[{{"type":"text","text":{}}}]}}}}"#,
                serde_json::to_string(text).unwrap()
            )
            .unwrap();
        }
    }

    #[test]
    fn ingests_turns_and_records_a_watermark() {
        let dir = scratch("basic");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "hello"), ("assistant", "hi there")]);
        let session = claude_session(src.clone());

        let mut idx = ConvoIndex::default();
        let got = ingest(&dir, &mut idx, &session).unwrap();

        assert_eq!(got.turns, 2);
        let meta = idx.sessions.get("sess-1").expect("indexed");
        assert_eq!(meta.turns, 2);
        assert_eq!(meta.watermark, std::fs::metadata(&src).unwrap().len());
        assert_eq!(meta.agent, "claude");

        let turns = read_turns(&dir, "sess-1");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].seq, 0);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].text, "hello");
        assert_eq!(turns[1].seq, 1);
        assert_eq!(turns[1].text, "hi there");
    }

    #[test]
    fn re_ingesting_an_unchanged_source_is_a_no_op() {
        let dir = scratch("idempotent");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "only turn")]);
        let session = claude_session(src);

        let mut idx = ConvoIndex::default();
        assert_eq!(ingest(&dir, &mut idx, &session).unwrap().turns, 1);
        // Second and third passes must not duplicate the turn.
        assert_eq!(ingest(&dir, &mut idx, &session).unwrap().turns, 0);
        assert_eq!(ingest(&dir, &mut idx, &session).unwrap().turns, 0);

        assert_eq!(read_turns(&dir, "sess-1").len(), 1);
        assert_eq!(idx.sessions["sess-1"].turns, 1);
    }

    #[test]
    fn appended_source_bytes_add_only_the_new_turns_and_keep_numbering() {
        let dir = scratch("append");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "first")]);
        let session = claude_session(src.clone());

        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();
        append_claude(&src, &[("assistant", "second"), ("user", "third")]);
        let got = ingest(&dir, &mut idx, &session).unwrap();

        assert_eq!(got.turns, 2, "only the appended turns");
        let turns = read_turns(&dir, "sess-1");
        assert_eq!(turns.len(), 3);
        assert_eq!(
            turns.iter().map(|t| t.seq).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "seq continues across ingests"
        );
        assert_eq!(turns[2].text, "third");
    }

    #[test]
    fn a_truncated_source_is_rebuilt_rather_than_appended_to() {
        let dir = scratch("truncate");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "a"), ("assistant", "b"), ("user", "c")]);
        let session = claude_session(src.clone());

        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();
        assert_eq!(read_turns(&dir, "sess-1").len(), 3);

        // Rotated: shorter file, different content.
        write_claude(&src, &[("user", "fresh")]);
        let got = ingest(&dir, &mut idx, &session).unwrap();

        assert_eq!(got.turns, 1);
        let turns = read_turns(&dir, "sess-1");
        assert_eq!(turns.len(), 1, "old turns are dropped, not kept alongside");
        assert_eq!(turns[0].seq, 0);
        assert_eq!(turns[0].text, "fresh");
        assert_eq!(idx.sessions["sess-1"].turns, 1);
    }

    #[test]
    fn tool_only_bytes_advance_the_watermark_without_recording_turns() {
        let dir = scratch("toolonly");
        let src = dir.join("src.jsonl");
        std::fs::write(
            &src,
            "{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"summary\"}\n",
        )
        .unwrap();
        let session = claude_session(src.clone());

        let mut idx = ConvoIndex::default();
        let got = ingest(&dir, &mut idx, &session).unwrap();

        assert_eq!(got.turns, 0);
        let meta = idx.sessions.get("sess-1").expect("still indexed");
        assert_eq!(
            meta.watermark,
            std::fs::metadata(&src).unwrap().len(),
            "consumed bytes are not re-scanned next pass"
        );
        assert!(read_turns(&dir, "sess-1").is_empty());
    }

    #[test]
    fn index_survives_a_save_load_round_trip() {
        let dir = scratch("roundtrip");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "persisted")]);
        let session = claude_session(src);

        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();
        save_index(&dir, &idx).unwrap();

        let back = load_index(&dir);
        assert_eq!(back, idx);
        assert_eq!(back.total_turns(), 1);
    }

    /// The defect this pins: `len` was sampled before an unbounded read, so a
    /// turn appended in between was written to the log but left outside the
    /// watermark, and the next pass appended it again under a fresh `seq`.
    #[test]
    fn a_turn_appended_mid_read_is_ingested_exactly_once() {
        let dir = scratch("midread");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "first")]);
        let session = claude_session(src.clone());
        let mut idx = ConvoIndex::default();

        // Pass 1 sees only "first"; "second" lands after the length was taken.
        let len_at_sample = std::fs::metadata(&src).unwrap().len();
        append_claude(&src, &[("assistant", "second")]);
        let (turns, consumed) = read_turns_from(Agent::Claude, &src, 0, len_at_sample);
        assert_eq!(turns.len(), 1, "the read is bounded by the sampled length");
        assert_eq!(
            consumed, len_at_sample,
            "consumed must equal what the watermark will record"
        );

        // Drive the real path twice and prove no turn is recorded twice.
        ingest(&dir, &mut idx, &session).unwrap();
        ingest(&dir, &mut idx, &session).unwrap();
        let recorded = read_turns(&dir, "sess-1");
        let texts: Vec<&str> = recorded.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second"], "{recorded:?}");
        assert_eq!(
            recorded.iter().map(|t| t.seq).collect::<Vec<_>>(),
            vec![0, 1],
            "seq must be contiguous with no repeats"
        );
        assert_eq!(idx.sessions["sess-1"].turns, 2);
    }

    /// A watermark landing mid-line would make the next pass seek past the
    /// remainder, losing that turn permanently.
    #[test]
    fn a_partial_final_line_is_left_for_the_next_pass() {
        let dir = scratch("partial");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "complete")]);
        let whole = std::fs::metadata(&src).unwrap().len();
        // Append a line the "writer" has not terminated yet.
        {
            let mut f = OpenOptions::new().append(true).open(&src).unwrap();
            f.write_all(br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"half"#)
                .unwrap();
        }
        let len = std::fs::metadata(&src).unwrap().len();
        let (turns, consumed) = read_turns_from(Agent::Claude, &src, 0, len);

        assert_eq!(turns.len(), 1, "only the finished line is a turn");
        assert_eq!(
            consumed, whole,
            "the unterminated tail must stay outside the watermark"
        );

        // Now the writer finishes the line; the turn must appear, not be skipped.
        {
            let mut f = OpenOptions::new().append(true).open(&src).unwrap();
            f.write_all(b"\"}]}}\n").unwrap();
        }
        let len2 = std::fs::metadata(&src).unwrap().len();
        let (turns2, _) = read_turns_from(Agent::Claude, &src, consumed, len2 - consumed);
        assert_eq!(turns2.len(), 1);
        assert_eq!(turns2[0].1, "half");
    }

    /// MEDIUM 7: the same id resolving to a different file is a different
    /// conversation. Resuming at the old offset would read a never-seen file
    /// from the middle and append the garbage behind the old turns.
    #[test]
    fn a_changed_source_path_rebuilds_instead_of_resuming_at_the_old_offset() {
        let dir = scratch("moved");
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        write_claude(&a, &[("user", "from a"), ("assistant", "still a")]);
        let mut session = claude_session(a);
        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();
        assert_eq!(read_turns(&dir, "sess-1").len(), 2);

        // Same id, different (longer) file — so the shrink heuristic cannot see it.
        write_claude(
            &b,
            &[("user", "from b"), ("assistant", "b2"), ("user", "b3")],
        );
        session.file = b.clone();
        ingest(&dir, &mut idx, &session).unwrap();

        let turns = read_turns(&dir, "sess-1");
        assert_eq!(
            turns.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["from b", "b2", "b3"],
            "old turns dropped, new file read from the start"
        );
        assert_eq!(idx.sessions["sess-1"].turns, 3);
        assert_eq!(idx.sessions["sess-1"].source, b);
    }

    /// MEDIUM 8: rotation to a source with nothing quotable used to return
    /// early, leaving the previous incarnation's turns and count in place.
    #[test]
    fn rotation_to_a_quoteless_source_clears_the_old_turns() {
        let dir = scratch("rotempty");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "a"), ("assistant", "b"), ("user", "c")]);
        let session = claude_session(src.clone());
        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();
        assert_eq!(idx.sessions["sess-1"].turns, 3);

        // Rotated to a shorter file carrying no user/assistant turns at all.
        std::fs::write(&src, "{\"type\":\"system\",\"subtype\":\"init\"}\n").unwrap();
        ingest(&dir, &mut idx, &session).unwrap();

        assert!(
            read_turns(&dir, "sess-1").is_empty(),
            "stale turns from the previous incarnation must be gone"
        );
        assert_eq!(idx.sessions["sess-1"].turns, 0, "and so must the count");

        // A later real turn must number from 0, not from the stale 3.
        append_claude(&src, &[("user", "fresh")]);
        ingest(&dir, &mut idx, &session).unwrap();
        let turns = read_turns(&dir, "sess-1");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].seq, 0);
        assert_eq!(turns[0].text, "fresh");
    }

    /// HIGH 3: kiro's `Session::file` is a metadata sidecar; the conversation is
    /// in an adjacent jsonl. Keying the watermark off the sidecar hid new turns
    /// whenever a sidecar rewrite kept the same byte length.
    #[test]
    fn kiro_is_measured_by_its_adjacent_jsonl_not_its_sidecar() {
        let dir = scratch("kiro");
        let sidecar = dir.join("k1.json");
        let jsonl = dir.join("k1.jsonl");
        std::fs::write(&sidecar, r#"{"updated_at":"2026-08-04T00:00:00Z"}"#).unwrap();
        std::fs::write(
            &jsonl,
            "{\"kind\":\"Prompt\",\"data\":{\"content\":\"hello kiro\"}}\n",
        )
        .unwrap();
        let mut session = claude_session(sidecar.clone());
        session.agent = Agent::Kiro;
        session.id = "kiro-1".into();

        let mut idx = ConvoIndex::default();
        assert_eq!(ingest(&dir, &mut idx, &session).unwrap().turns, 1);
        assert_eq!(
            idx.sessions["kiro-1"].source, jsonl,
            "the jsonl is what gets watermarked"
        );
        assert_eq!(
            idx.sessions["kiro-1"].watermark,
            std::fs::metadata(&jsonl).unwrap().len()
        );

        // Sidecar rewritten to the SAME length; the jsonl gains a turn.
        std::fs::write(&sidecar, r#"{"updated_at":"2026-08-04T11:11:11Z"}"#).unwrap();
        let mut f = OpenOptions::new().append(true).open(&jsonl).unwrap();
        f.write_all(b"{\"kind\":\"AssistantMessage\",\"data\":{\"content\":\"reply\"}}\n")
            .unwrap();
        drop(f);

        assert_eq!(
            ingest(&dir, &mut idx, &session).unwrap().turns,
            1,
            "a new jsonl turn is picked up even though the sidecar length is unchanged"
        );
        let turns = read_turns(&dir, "kiro-1");
        assert_eq!(turns.len(), 2, "and it appends rather than rewriting");
        assert_eq!(turns[1].text, "reply");
        assert_eq!(turns[1].seq, 1);
    }

    #[test]
    fn a_kiro_session_without_an_adjacent_jsonl_is_skipped() {
        let dir = scratch("kirobare");
        let sidecar = dir.join("k2.json");
        std::fs::write(&sidecar, r#"{"updated_at":"x"}"#).unwrap();
        let mut session = claude_session(sidecar);
        session.agent = Agent::Kiro;
        session.id = "kiro-2".into();

        let mut idx = ConvoIndex::default();
        assert_eq!(ingest(&dir, &mut idx, &session).unwrap().turns, 0);
        assert!(idx.sessions.is_empty(), "nothing to record without a jsonl");
    }

    #[test]
    fn an_oversized_index_is_rejected_before_it_is_parsed() {
        let dir = scratch("oversize");
        std::fs::create_dir_all(&dir).unwrap();
        // Valid JSON, but past the byte cap: must be refused on size alone.
        let mut idx = ConvoIndex::default();
        let now = Utc::now();
        idx.sessions.insert(
            "s".into(),
            ConvoMeta {
                agent: "claude".into(),
                cwd: PathBuf::from("/tmp"),
                source: PathBuf::from("/tmp/x.jsonl"),
                watermark: 1,
                turns: 1,
                first_seen: now,
                last_seen: now,
            },
        );
        save_index(&dir, &idx).unwrap();
        let real = std::fs::read_to_string(index_path(&dir)).unwrap();
        // Pad past the cap without breaking JSON validity of the prefix check.
        let padded = format!("{real}{}", " ".repeat((MAX_INDEX_BYTES as usize) + 1));
        std::fs::write(index_path(&dir), padded).unwrap();

        assert!(
            std::fs::metadata(index_path(&dir)).unwrap().len() > MAX_INDEX_BYTES,
            "test set up a file past the cap"
        );
        assert_eq!(
            load_index(&dir),
            ConvoIndex::default(),
            "oversized index must read back empty, not be parsed"
        );
    }

    #[test]
    fn an_index_claiming_too_many_sessions_is_rejected() {
        let dir = scratch("toomany");
        std::fs::create_dir_all(&dir).unwrap();
        // Small file, pathological content: sessions past MAX_SESSIONS.
        let mut body = String::from(r#"{"version":1,"sessions":{"#);
        for i in 0..(MAX_SESSIONS + 1) {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!(
                r#""s{i}":{{"agent":"claude","cwd":"/","source":"/x","watermark":0,"turns":0,"first_seen":"2026-01-01T00:00:00Z","last_seen":"2026-01-01T00:00:00Z"}}"#
            ));
        }
        body.push_str("}}");
        std::fs::write(index_path(&dir), body).unwrap();

        assert_eq!(load_index(&dir), ConvoIndex::default());
    }

    #[test]
    fn a_leftover_temp_file_does_not_become_the_index() {
        let dir = scratch("tmpleft");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "good")]);
        let session = claude_session(src);
        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();
        save_index(&dir, &idx).unwrap();

        // Simulate a crash mid-write: a stale .tmp beside a good index.
        std::fs::write(index_path(&dir).with_extension("json.tmp"), "{ truncated").unwrap();

        let back = load_index(&dir);
        assert_eq!(back.total_turns(), 1, "the good index still loads");
    }

    #[test]
    fn a_future_index_version_reads_back_empty_instead_of_erroring() {
        let dir = scratch("version");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            index_path(&dir),
            format!(r#"{{"version":{},"sessions":{{}}}}"#, INDEX_VERSION + 99),
        )
        .unwrap();
        assert_eq!(load_index(&dir), ConvoIndex::default());
    }

    #[test]
    fn a_missing_source_file_is_skipped_quietly() {
        let dir = scratch("missing");
        let session = claude_session(dir.join("does-not-exist.jsonl"));
        let mut idx = ConvoIndex::default();
        assert_eq!(ingest(&dir, &mut idx, &session).unwrap().turns, 0);
        assert!(idx.sessions.is_empty());
    }

    #[test]
    fn control_characters_are_neutralized_before_storage() {
        let dir = scratch("controls");
        let src = dir.join("src.jsonl");
        write_claude(&src, &[("user", "before\u{1b}[31mafter")]);
        let session = claude_session(src);

        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &session).unwrap();

        let turns = read_turns(&dir, "sess-1");
        assert_eq!(turns.len(), 1);
        assert!(
            !turns[0].text.contains('\u{1b}'),
            "escape sequence stored raw: {:?}",
            turns[0].text
        );
    }

    #[test]
    fn two_sessions_land_in_separate_files() {
        let dir = scratch("two");
        let a_src = dir.join("a.jsonl");
        let b_src = dir.join("b.jsonl");
        write_claude(&a_src, &[("user", "from a")]);
        write_claude(&b_src, &[("user", "from b")]);
        let a = claude_session(a_src);
        let mut b = claude_session(b_src);
        b.id = "sess-2".into();

        let mut idx = ConvoIndex::default();
        ingest(&dir, &mut idx, &a).unwrap();
        ingest(&dir, &mut idx, &b).unwrap();

        assert_eq!(idx.sessions.len(), 2);
        assert_eq!(read_turns(&dir, "sess-1")[0].text, "from a");
        assert_eq!(read_turns(&dir, "sess-2")[0].text, "from b");
    }
}
