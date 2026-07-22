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
    fn human_tokens_scales() {
        assert_eq!(human_tokens(512), "512");
        assert_eq!(human_tokens(12_000), "12.0K");
        assert_eq!(human_tokens(38_400_000), "38.4M");
    }
}
