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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    AppStart { run_id: u32 },
    AppStop { run_id: u32 },
    SessionOpen { agent: String },
    SessionClose,
    Handoff,
    CatchupSent,
    TransitionReportSent,
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
