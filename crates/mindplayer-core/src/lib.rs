//! MindPlayer shared core.
//!
//! UI-agnostic logic shared by the TUI (`mindplayer-tui`) and the Tauri app:
//! session discovery, token aggregation, sidecar archive state, and resume
//! command building.

pub mod discovery;
pub mod resume;
pub mod session;
pub mod state;
pub mod tokens;

pub use discovery::{
    refresh_activity, refresh_activity_and_usage, scan, sort_by_recency, touched_recently,
    ScanConfig, Scope,
};
pub use resume::{new_session, resume, Command};
pub use session::{Agent, Session, TokenUsage};
pub use state::State;
pub use tokens::Aggregate;
