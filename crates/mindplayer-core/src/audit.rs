//! Append-only usage-audit log: "how much/how often mindplayer was used",
//! never "what was said in a session". Kept separate from `state.rs` (a
//! snapshot of current archived ids/labels) because answering "how many
//! sessions did I open today" needs history, not just current state.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// An abandoned run (killed/crashed — no matching `app_stop`) is credited at
/// most this much active time, so one hard-killed instance can't inflate
/// "all-time" by however many days it sat dead before this file was next
/// read. The *current* process's own still-open run is exempt (see
/// `compute_stats`) since its true end really is "now".
const ABANDONED_RUN_CAP: Duration = Duration::hours(8);

/// How many trailing days the usage popup's trend line covers.
const SPARKLINE_DAYS: i64 = 14;

/// The "today" boundary everywhere in this module — a rolling 24h window,
/// matching `discovery::touched_recently`'s definition, not a calendar day.
const TODAY_WINDOW: Duration = Duration::hours(24);

/// A single line in the audit log. Two families of events share this enum:
///
/// * **Usage-stats events** (`AppStart`/`AppStop`, `SessionOpen`,
///   `SessionClose`, `Handoff`, `CatchupSent`, `TransitionReportSent`) —
///   aggregated by [`compute_stats`] and surfaced in the `u` usage popup.
///   These predate the incident-reconstruction events and are the only ones
///   `compute_stats` looks at.
/// * **Action + status-transition events** (everything below the first group)
///   — a chronological breadcrumb trail added so that when a user reports an
///   in-the-moment misbehavior ("it froze", "only one session opened") the log
///   can be read after the fact to reconstruct *what the user did, in what
///   order, and what the app thought was happening*. They are deliberately
///   NOT counted by `compute_stats` (they'd distort nothing user-visible in
///   the popup); they exist purely for post-hoc debugging.
///
/// Action events group related keybindings rather than minting one variant per
/// key: a modal flow is a `*Begin` / `*Confirm` / `*Cancel` triplet, and a
/// toggle carries its resulting state (`on: bool`) instead of separate
/// on/off variants. Each carries enough context (session id, old/new state,
/// batch counts/ids) to stand alone without source access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    // --- usage-stats events (the only ones `compute_stats` aggregates) ------
    AppStart {
        run_id: u32,
    },
    AppStop {
        run_id: u32,
    },
    SessionOpen {
        agent: String,
    },
    SessionClose,
    Handoff,
    CatchupSent,
    TransitionReportSent,

    // --- user-action events (reconstruct "what the user did, in what order") -
    /// A session was brought to the foreground as a live pane (Enter / l / →,
    /// or confirming a search) — whether freshly resumed or an already-running
    /// background pane switched to.
    SessionResume {
        id: String,
    },
    /// Multi-select launch: every marked session opened as a live pane at once.
    /// `zoom_was_on` is the full-screen-zoom state captured *before* the launch
    /// clears it — the exact condition behind the "only one session opened"
    /// bug, so a reader can see a bulk launch that happened while zoom was
    /// still stuck on.
    LaunchMarked {
        ids: Vec<String>,
        count: usize,
        zoom_was_on: bool,
    },
    /// A single live pane was closed (ctrl-q); `remaining` panes stay open.
    PaneClose {
        id: String,
        remaining: usize,
    },
    /// A new Codex/Claude/Kiro session was requested (its real `SessionOpen`
    /// lands once the PTY actually spawns).
    NewSession {
        agent: String,
    },
    /// Multi-select mode entered/left (`v` / esc).
    MultiSelect {
        on: bool,
    },
    /// A row's multi-select mark toggled (space); `total` is the resulting
    /// number of marked rows.
    MarkToggle {
        id: String,
        marked: bool,
        total: usize,
    },
    /// Full-screen zoom of the focused pane toggled (ctrl-z).
    ZoomToggle {
        on: bool,
    },
    /// Pane grid layout cycled (ctrl-o); `layout` is the new layout.
    LayoutCycle {
        layout: String,
    },
    /// Focused pane cycled (Tab / Shift-Tab / ctrl-w); `focused` is the new
    /// 1-based pane index out of `count` open panes.
    PaneFocusCycle {
        focused: usize,
        count: usize,
    },
    /// Left the live view for the list, or re-entered it (ctrl-x); `focus` is
    /// where focus landed ("list" / "terminal").
    FocusChange {
        focus: String,
    },
    /// The manual "in progress" mark toggled on a session (`i`).
    InProgressToggle {
        id: String,
        in_progress: bool,
    },
    /// A list view filter toggled: `view` is "archived" (`a`) or "subagents"
    /// (`g`).
    ViewToggle {
        view: String,
        on: bool,
    },
    /// Manual full rescan of the current scope (`r`).
    Rescan,
    /// Usage-stats popup opened/closed (`u`).
    UsagePopup {
        open: bool,
    },
    /// Session-list search opened (`/`).
    SearchBegin,
    /// Search confirmed (Enter) — `focus` is where the app landed afterward
    /// ("terminal" once it resumed the match). Logged after the resume, so a
    /// "search active → resume → focus terminal" setup reads straight off the
    /// log.
    SearchConfirm {
        focus: String,
    },
    /// Search dismissed (esc) without resuming.
    SearchCancel,
    /// Cross-agent handoff picker opened (`h`); the handoff itself is `Handoff`.
    HandoffBegin,
    /// Handoff picker dismissed (esc) without handing off.
    HandoffCancel,
    /// Catch-up initiated on a session (`c`); `awaiting_confirm` is true when
    /// the session was busy and a "send anyway?" confirm was shown (the send
    /// itself is `CatchupSent`).
    CatchupBegin {
        id: String,
        awaiting_confirm: bool,
    },
    /// Catch-up "send anyway?" confirm dismissed (esc).
    CatchupCancel,
    /// Label-edit modal opened for a session (`e`).
    LabelEditBegin {
        id: String,
    },
    /// Label edit confirmed; `label` is the new label ("" = cleared).
    LabelEditConfirm {
        id: String,
        label: String,
    },
    /// Working-dir / scope-change modal opened (`d`).
    WorkingDirBegin,
    /// Working-dir change confirmed; `scope` is the new scope label.
    WorkingDirConfirm {
        scope: String,
    },
    /// Working-dir modal dismissed (esc).
    WorkingDirCancel,

    // --- app-computed status transitions ("what the app thought") -----------
    /// A session's computed live status changed since the last poll
    /// (idle/working/blocked, and → ended when its child exits). Logged only on
    /// an actual change, never every poll.
    SessionStatusChange {
        id: String,
        from: String,
        to: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub event: AuditEvent,
}

/// `~/.mindplayer/audit.jsonl`, overridable via `MINDPLAYER_AUDIT`.
pub fn default_audit_path() -> PathBuf {
    if let Ok(p) = std::env::var("MINDPLAYER_AUDIT") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("audit.jsonl")
}

/// Best-effort: append one event to the default path. Errors are swallowed —
/// a missed stats line should never interrupt the feature it's attached to.
pub fn log_event(event: AuditEvent) {
    log_event_to(&default_audit_path(), event);
}

pub fn log_event_to(path: &Path, event: AuditEvent) {
    let record = AuditRecord {
        ts: Utc::now(),
        event,
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Parses every well-formed line; a line a crash cut off mid-write is skipped
/// rather than invalidating every event around it.
pub fn read_events(path: &Path) -> Vec<AuditRecord> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentCounts {
    pub codex: usize,
    pub claude: usize,
    pub kiro: usize,
}

impl AgentCounts {
    fn add(&mut self, agent: &str) {
        match agent {
            "codex" => self.codex += 1,
            "claude" => self.claude += 1,
            "kiro" => self.kiro += 1,
            _ => {}
        }
    }

    pub fn total(&self) -> usize {
        self.codex + self.claude + self.kiro
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageStats {
    pub active_secs_today: i64,
    pub active_secs_all_time: i64,
    pub sessions_opened_today: AgentCounts,
    pub sessions_opened_all_time: AgentCounts,
    pub sessions_closed_all_time: usize,
    pub handoffs_all_time: usize,
    pub catchups_all_time: usize,
    pub transition_reports_all_time: usize,
    /// One active-seconds total per rolling 24h bucket, oldest first,
    /// covering the last [`SPARKLINE_DAYS`] days (today included).
    pub daily_active_secs: Vec<i64>,
}

/// `current_run_id` is the calling process's own `std::process::id()` — the
/// one run_id allowed to count as "still running" all the way to `now`
/// instead of being capped as abandoned.
pub fn compute_stats(
    events: &[AuditRecord],
    now: DateTime<Utc>,
    current_run_id: u32,
) -> UsageStats {
    let today_cutoff = now - TODAY_WINDOW;
    let mut stats = UsageStats::default();

    let mut open_starts: HashMap<u32, DateTime<Utc>> = HashMap::new();
    let mut intervals: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    for rec in events {
        match &rec.event {
            AuditEvent::AppStart { run_id } => {
                open_starts.insert(*run_id, rec.ts);
            }
            AuditEvent::AppStop { run_id } => {
                if let Some(start) = open_starts.remove(run_id) {
                    intervals.push((start, rec.ts));
                }
            }
            _ => {}
        }
    }
    for (run_id, start) in open_starts {
        let end = if run_id == current_run_id {
            now
        } else {
            (start + ABANDONED_RUN_CAP).min(now)
        };
        intervals.push((start, end));
    }

    for &(start, end) in &intervals {
        stats.active_secs_all_time += (end - start).num_seconds().max(0);
        let overlap_start = start.max(today_cutoff);
        let overlap_end = end.min(now);
        if overlap_end > overlap_start {
            stats.active_secs_today += (overlap_end - overlap_start).num_seconds();
        }
    }

    let mut daily = vec![0i64; SPARKLINE_DAYS as usize];
    for &(start, end) in &intervals {
        for i in 0..SPARKLINE_DAYS {
            let bucket_end = now - Duration::hours(24 * i);
            let bucket_start = bucket_end - TODAY_WINDOW;
            let overlap_start = start.max(bucket_start);
            let overlap_end = end.min(bucket_end);
            if overlap_end > overlap_start {
                let idx = (SPARKLINE_DAYS - 1 - i) as usize;
                daily[idx] += (overlap_end - overlap_start).num_seconds();
            }
        }
    }
    stats.daily_active_secs = daily;

    for rec in events {
        let is_today = now - rec.ts < TODAY_WINDOW;
        match &rec.event {
            AuditEvent::SessionOpen { agent } => {
                stats.sessions_opened_all_time.add(agent);
                if is_today {
                    stats.sessions_opened_today.add(agent);
                }
            }
            AuditEvent::SessionClose => stats.sessions_closed_all_time += 1,
            AuditEvent::Handoff => stats.handoffs_all_time += 1,
            AuditEvent::CatchupSent => stats.catchups_all_time += 1,
            AuditEvent::TransitionReportSent => stats.transition_reports_all_time += 1,
            AuditEvent::AppStart { .. } | AuditEvent::AppStop { .. } => {}
            // Action + status-transition events are breadcrumbs for post-hoc
            // incident reconstruction, not usage metrics — they contribute
            // nothing to what the usage popup shows.
            _ => {}
        }
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mp-audit-test-{name}-{}.jsonl", std::process::id()))
    }

    #[test]
    fn append_and_read_round_trip() {
        let path = tmp_path("roundtrip");
        log_event_to(
            &path,
            AuditEvent::SessionOpen {
                agent: "codex".to_string(),
            },
        );
        log_event_to(&path, AuditEvent::Handoff);
        let events = read_events(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].event,
            AuditEvent::SessionOpen {
                agent: "codex".to_string()
            }
        );
        assert_eq!(events[1].event, AuditEvent::Handoff);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn action_and_transition_events_round_trip_through_jsonl() {
        let path = tmp_path("actions");
        let events = vec![
            AuditEvent::ZoomToggle { on: true },
            AuditEvent::MarkToggle {
                id: "s1".to_string(),
                marked: true,
                total: 2,
            },
            AuditEvent::LaunchMarked {
                ids: vec!["s1".to_string(), "s2".to_string()],
                count: 2,
                zoom_was_on: true,
            },
            AuditEvent::SessionStatusChange {
                id: "s1".to_string(),
                from: "idle".to_string(),
                to: "working".to_string(),
            },
        ];
        for e in &events {
            log_event_to(&path, e.clone());
        }
        let read = read_events(&path);
        assert_eq!(read.len(), events.len());
        for (want, got) in events.iter().zip(read.iter()) {
            assert_eq!(*want, got.event);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn action_events_do_not_perturb_usage_stats() {
        // The breadcrumb events must be invisible to the usage popup: a log
        // full of them, with one real SessionOpen, still reports exactly one
        // opened session and nothing else.
        let now = Utc::now();
        let events = vec![
            rec(now, AuditEvent::ZoomToggle { on: true }),
            rec(
                now,
                AuditEvent::LaunchMarked {
                    ids: vec!["s1".to_string()],
                    count: 1,
                    zoom_was_on: true,
                },
            ),
            rec(
                now,
                AuditEvent::SessionOpen {
                    agent: "codex".to_string(),
                },
            ),
            rec(
                now,
                AuditEvent::SessionStatusChange {
                    id: "s1".to_string(),
                    from: "idle".to_string(),
                    to: "working".to_string(),
                },
            ),
        ];
        let stats = compute_stats(&events, now, 0);
        assert_eq!(stats.sessions_opened_all_time.total(), 1);
        assert_eq!(stats.sessions_closed_all_time, 0);
        assert_eq!(stats.handoffs_all_time, 0);
    }

    #[test]
    fn corrupt_line_is_skipped_without_losing_its_neighbors() {
        let path = tmp_path("corrupt");
        log_event_to(&path, AuditEvent::CatchupSent);
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{not valid json").unwrap();
        }
        log_event_to(&path, AuditEvent::SessionClose);
        let events = read_events(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, AuditEvent::CatchupSent);
        assert_eq!(events[1].event, AuditEvent::SessionClose);
        let _ = std::fs::remove_file(&path);
    }

    fn rec(ts: DateTime<Utc>, event: AuditEvent) -> AuditRecord {
        AuditRecord { ts, event }
    }

    #[test]
    fn active_time_pairs_overlapping_runs_by_run_id_not_order() {
        let now = Utc::now();
        let events = vec![
            rec(now - Duration::hours(3), AuditEvent::AppStart { run_id: 1 }),
            rec(now - Duration::hours(2), AuditEvent::AppStart { run_id: 2 }),
            rec(now - Duration::hours(1), AuditEvent::AppStop { run_id: 1 }),
            rec(now, AuditEvent::AppStop { run_id: 2 }),
        ];
        let stats = compute_stats(&events, now, 999);
        // run 1: 2h, run 2: 2h -> 4h total, regardless of the interleaving.
        assert_eq!(stats.active_secs_all_time, 4 * 3600);
    }

    #[test]
    fn unmatched_current_run_counts_to_now() {
        let now = Utc::now();
        let events = vec![rec(
            now - Duration::hours(1),
            AuditEvent::AppStart { run_id: 42 },
        )];
        let stats = compute_stats(&events, now, 42);
        assert_eq!(stats.active_secs_all_time, 3600);
    }

    #[test]
    fn unmatched_foreign_run_is_capped_not_trusted() {
        let now = Utc::now();
        let events = vec![rec(
            now - Duration::days(5),
            AuditEvent::AppStart { run_id: 7 },
        )];
        // Some other (dead) process's run_id — not the caller's.
        let stats = compute_stats(&events, now, 999);
        assert_eq!(stats.active_secs_all_time, ABANDONED_RUN_CAP.num_seconds());
    }

    #[test]
    fn today_vs_all_time_respects_the_rolling_24h_window() {
        let now = Utc::now();
        let events = vec![
            rec(
                now - Duration::hours(1),
                AuditEvent::SessionOpen {
                    agent: "codex".to_string(),
                },
            ),
            rec(
                now - Duration::hours(30),
                AuditEvent::SessionOpen {
                    agent: "codex".to_string(),
                },
            ),
        ];
        let stats = compute_stats(&events, now, 0);
        assert_eq!(stats.sessions_opened_today.codex, 1);
        assert_eq!(stats.sessions_opened_all_time.codex, 2);
    }

    #[test]
    fn agent_breakdown_counts_each_agent_independently() {
        let now = Utc::now();
        let events = vec![
            rec(
                now,
                AuditEvent::SessionOpen {
                    agent: "codex".to_string(),
                },
            ),
            rec(
                now,
                AuditEvent::SessionOpen {
                    agent: "codex".to_string(),
                },
            ),
            rec(
                now,
                AuditEvent::SessionOpen {
                    agent: "claude".to_string(),
                },
            ),
            rec(
                now,
                AuditEvent::SessionOpen {
                    agent: "kiro".to_string(),
                },
            ),
        ];
        let stats = compute_stats(&events, now, 0);
        assert_eq!(
            stats.sessions_opened_all_time,
            AgentCounts {
                codex: 2,
                claude: 1,
                kiro: 1
            }
        );
        assert_eq!(stats.sessions_opened_all_time.total(), 4);
    }

    #[test]
    fn daily_series_has_the_expected_length_and_sums_to_all_time() {
        let now = Utc::now();
        let events = vec![rec(
            now - Duration::hours(1),
            AuditEvent::AppStart { run_id: 1 },
        )];
        let stats = compute_stats(&events, now, 1);
        assert_eq!(stats.daily_active_secs.len(), SPARKLINE_DAYS as usize);
        assert_eq!(
            stats.daily_active_secs.iter().sum::<i64>(),
            stats.active_secs_all_time
        );
    }
}
