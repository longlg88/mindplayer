//! MindPlayer shared core.
//!
//! UI-agnostic logic shared by the TUI (`mindplayer-tui`) and the Tauri app:
//! session discovery, token aggregation, sidecar archive state, and resume
//! command building.

pub mod audit;
pub mod discovery;
pub mod prompts;
pub mod resume;
pub mod session;
pub mod state;
pub mod tokens;

pub use audit::{
    compute_stats, default_audit_path, log_event, log_event_to, read_events, AgentCounts,
    AuditEvent, AuditRecord, UsageStats,
};
pub use discovery::{
    refresh_activity, refresh_activity_and_usage, scan, sort_by_recency, touched_recently,
    ScanConfig, Scope,
};
pub use prompts::{default_prompts_dir, load_prompt, load_prompt_from};
pub use resume::{new_session, resume, Command};
pub use session::{Agent, Session, TokenUsage};
pub use state::State;
pub use tokens::Aggregate;
