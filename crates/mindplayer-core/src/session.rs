//! Core data model: agents, token usage, and a discovered session.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;

/// Which CLI produced a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Codex,
    Claude,
    Kiro,
}

impl Agent {
    /// Short display / matching key (also the serde tag). Stable across the
    /// pending-label and reconciliation paths, so keep it in sync everywhere.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Kiro => "kiro",
        }
    }

    /// The actual CLI binary to spawn. Differs from [`Self::as_str`] for Kiro,
    /// whose CLI binary is `kiro-cli` (the desktop IDE is `kiro`).
    pub fn program(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Kiro => "kiro-cli",
        }
    }
}

/// Token usage for a single session (or an aggregate).
///
/// `cached` counts cached/cache-read input tokens. `total` is the authoritative
/// grand total: for Codex it is the CLI-reported cumulative `total_tokens`; for
/// Claude it is the sum of every component across all assistant messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub total: u64,
}

impl TokenUsage {
    /// Accumulate another usage into this one (used for aggregation).
    /// Saturating so absurd values in untrusted files can never panic/wrap.
    pub fn add(&mut self, other: &TokenUsage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cached = self.cached.saturating_add(other.cached);
        self.total = self.total.saturating_add(other.total);
    }
}

/// A discovered Codex, Claude, or Kiro session.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    /// Codex UUID / Claude sessionId.
    pub id: String,
    pub agent: Agent,
    /// Working directory the session ran in. `resume` is launched here.
    pub cwd: PathBuf,
    /// Path to the backing `.jsonl` rollout/transcript file.
    pub file: PathBuf,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active: Option<DateTime<Utc>>,
    /// Timestamp of the last genuine user-authored prompt — excludes
    /// automated tool-result round-trips (claude turns whose content is a
    /// `tool_result` block) and codex's own `function_call_output` events, so
    /// it reflects when a human last actually typed something, not merely
    /// when the transcript file was last appended to. `None` for kiro, whose
    /// sidecar only records a whole-session `updated_at` with no per-turn
    /// role breakdown to derive this from.
    pub last_prompt_at: Option<DateTime<Utc>>,
    pub tokens: TokenUsage,
    /// First user prompt, cleaned and truncated for the list. `(empty)` if none.
    pub title: String,
    /// Marked closed in the MindPlayer sidecar state (original jsonl untouched).
    pub archived: bool,
    /// A spawned helper session (codex `thread_source=subagent`, claude
    /// sidechain, or a `/team` worker) rather than a top-level user session.
    /// Hidden from the default list.
    pub is_subagent: bool,
    /// Current context-window occupancy as a percentage (0–100), when the agent
    /// reports it. Kiro records `context_usage_percentage` instead of cumulative
    /// token counts; codex/claude leave this `None` (they report tokens). Shown
    /// in the list's usage column for kiro since it has no token total.
    pub context_pct: Option<f64>,
}
