//! Aggregate token usage and session counts across a set of sessions.

use crate::session::{Agent, Session, TokenUsage};
use serde::Serialize;

/// Totals shown on the scan screen and the main status bar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Aggregate {
    pub total: TokenUsage,
    pub codex: TokenUsage,
    pub claude: TokenUsage,
    pub kiro: TokenUsage,
    pub codex_count: usize,
    pub claude_count: usize,
    pub kiro_count: usize,
}

impl Aggregate {
    /// Aggregate over the full in-scope set, so the scan numbers reflect every
    /// collected session (regardless of the UI's view filters).
    pub fn of(sessions: &[Session]) -> Self {
        Self::of_refs(sessions.iter())
    }

    /// Aggregate over an arbitrary set of sessions by reference — used to total
    /// just the currently-visible (filtered) rows for the status bar.
    pub fn of_refs<'a>(sessions: impl IntoIterator<Item = &'a Session>) -> Self {
        let mut a = Aggregate::default();
        for s in sessions {
            a.total.add(&s.tokens);
            match s.agent {
                Agent::Codex => {
                    a.codex.add(&s.tokens);
                    a.codex_count += 1;
                }
                Agent::Claude => {
                    a.claude.add(&s.tokens);
                    a.claude_count += 1;
                }
                Agent::Kiro => {
                    a.kiro.add(&s.tokens);
                    a.kiro_count += 1;
                }
            }
        }
        a
    }

    pub fn session_count(&self) -> usize {
        self.codex_count + self.claude_count + self.kiro_count
    }
}

/// Split `cells` proportionally across `parts`, exactly.
///
/// Largest remainder, so the pieces always sum to `cells` instead of drifting
/// low the way independent rounding does. Any part with usage is guaranteed at
/// least one cell — a share too small to round up is still not nothing, and a
/// bar that renders it as nothing while the label names it reads as a bug.
pub fn apportion(parts: &[u64], cells: usize) -> Vec<usize> {
    let sum: u128 = parts.iter().map(|p| u128::from(*p)).sum();
    if sum == 0 || cells == 0 {
        return vec![0; parts.len()];
    }
    let exact: Vec<f64> = parts
        .iter()
        .map(|p| *p as f64 / sum as f64 * cells as f64)
        .collect();
    let mut out: Vec<usize> = exact.iter().map(|e| *e as usize).collect();
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|a, b| {
        (exact[*b] - out[*b] as f64)
            .partial_cmp(&(exact[*a] - out[*a] as f64))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut left = cells.saturating_sub(out.iter().sum::<usize>());
    for i in order.iter().cycle().take(left) {
        out[*i] += 1;
    }
    // Lift every used part off zero, paying for it from the widest segment.
    left = 0;
    for i in 0..out.len() {
        if parts[i] > 0 && out[i] == 0 {
            out[i] = 1;
            left += 1;
        }
    }
    while left > 0 {
        let Some(widest) = (0..out.len()).max_by_key(|i| out[*i]) else {
            break;
        };
        if out[widest] <= 1 {
            break;
        }
        out[widest] -= 1;
        left -= 1;
    }
    out
}

/// Format a token count compactly: `38.4M`, `12.0K`, `512`.
pub fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(agent: Agent, total: u64) -> Session {
        Session {
            id: "x".into(),
            agent,
            cwd: PathBuf::new(),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: TokenUsage {
                input: total,
                output: 0,
                cached: 0,
                total,
            },
            title: String::new(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    #[test]
    fn aggregate_splits_by_agent() {
        let s = vec![
            session(Agent::Codex, 100),
            session(Agent::Codex, 50),
            session(Agent::Claude, 25),
        ];
        let a = Aggregate::of(&s);
        assert_eq!(a.codex.total, 150);
        assert_eq!(a.claude.total, 25);
        assert_eq!(a.total.total, 175);
        assert_eq!(a.codex_count, 2);
        assert_eq!(a.claude_count, 1);
        assert_eq!(a.session_count(), 3);
    }

    #[test]
    fn apportion_sums_to_the_bar_width() {
        // The real footer figures: claude 18223.7M, codex 1827.1M, kiro unread.
        let got = apportion(&[18_223_700_000, 1_827_100_000], 24);
        assert_eq!(got.iter().sum::<usize>(), 24);
        assert_eq!(got, vec![22, 2]);
    }

    #[test]
    fn apportion_never_hides_a_part_that_has_usage() {
        // 0.2% would floor to zero cells while the label still names it.
        let got = apportion(&[9_980, 20], 20);
        assert_eq!(got.iter().sum::<usize>(), 20);
        assert_eq!(got[1], 1, "a used part must occupy at least one cell");
        assert_eq!(got[0], 19);
    }

    #[test]
    fn apportion_leaves_unused_parts_empty() {
        let got = apportion(&[100, 0, 50], 12);
        assert_eq!(got[1], 0);
        assert_eq!(got.iter().sum::<usize>(), 12);
    }

    #[test]
    fn apportion_is_empty_when_there_is_nothing_to_show() {
        assert_eq!(apportion(&[0, 0], 20), vec![0, 0]);
        assert_eq!(apportion(&[5, 5], 0), vec![0, 0]);
    }

    #[test]
    fn human_tokens_scales() {
        assert_eq!(human_tokens(512), "512");
        assert_eq!(human_tokens(12_000), "12.0K");
        assert_eq!(human_tokens(38_400_000), "38.4M");
    }
}
