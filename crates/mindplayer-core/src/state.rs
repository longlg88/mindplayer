//! Sidecar state: MindPlayer's own record of which sessions are archived.
//!
//! The original Codex/Claude `.jsonl` files are never modified. "Closing" a
//! session marks its id here. Writes are atomic (temp file + rename); a corrupt
//! file falls back to empty state with a warning.

use crate::session::Session;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A label awaiting the session id the agent assigns to a session created
/// through MindPlayer. The CLIs only write the session file once the session
/// has activity, so resolution happens on a later scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingLabel {
    pub agent: String, // Agent::as_str(): "codex" | "claude" | "kiro"
    pub cwd: PathBuf,
    pub after: DateTime<Utc>,
    pub label: String,
}

/// A handoff child session that has not yet been assigned its real CLI session
/// id. It is resolved with the same scan-time matching used by pending labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingHandoff {
    pub parent_id: String,
    pub agent: String,
    pub cwd: PathBuf,
    pub after: DateTime<Utc>,
    pub artifact: PathBuf,
}

/// MindPlayer's logical thread link. The original agent session files stay
/// untouched; this sidecar only records that `child_id` should be presented as
/// a lane under `parent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffLink {
    pub parent_id: String,
    pub artifact: PathBuf,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub archived: BTreeSet<String>,
    /// User-chosen labels for sessions created through MindPlayer (sessionId -> label).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Labels not yet matched to a session id.
    #[serde(default)]
    pub pending_labels: Vec<PendingLabel>,
    /// Handoff links waiting for the target CLI to write its real session file.
    #[serde(default)]
    pub pending_handoffs: Vec<PendingHandoff>,
    /// child session id -> parent/root session id.
    #[serde(default)]
    pub handoff_links: BTreeMap<String, HandoffLink>,
    #[serde(default)]
    pub last_scope: Option<String>,
}

fn default_version() -> u32 {
    1
}

impl Default for State {
    fn default() -> Self {
        State {
            version: default_version(),
            archived: BTreeSet::new(),
            labels: BTreeMap::new(),
            pending_labels: Vec::new(),
            pending_handoffs: Vec::new(),
            handoff_links: BTreeMap::new(),
            last_scope: None,
        }
    }
}

/// `~/.mindplayer/state.json`, overridable via `MINDPLAYER_STATE`.
pub fn default_state_path() -> PathBuf {
    if let Ok(p) = std::env::var("MINDPLAYER_STATE") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("state.json")
}

impl State {
    /// Load from the default path, falling back to empty state on miss/corrupt.
    pub fn load() -> Self {
        Self::load_from(&default_state_path())
    }

    /// Load from an explicit path (used by tests).
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!(
                    "mindplayer: corrupt state at {} ({e}); using empty state",
                    path.display()
                );
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&default_state_path())
    }

    /// Atomic durable write: serialize to a unique temp file in the same dir,
    /// fsync it, then rename over the target. The unique temp name (pid-tagged)
    /// avoids two MindPlayer processes (TUI + app) clobbering a shared temp.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        use std::io::Write as _;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let data = serde_json::to_string_pretty(self)?;
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(data.as_bytes())?;
            f.sync_all()?; // durable before the rename
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn is_archived(&self, id: &str) -> bool {
        self.archived.contains(id)
    }

    pub fn set_archived(&mut self, id: &str, archived: bool) {
        if archived {
            self.archived.insert(id.to_string());
        } else {
            self.archived.remove(id);
        }
    }

    pub fn label_for(&self, id: &str) -> Option<&str> {
        self.labels.get(id).map(String::as_str)
    }

    /// Queue a label to be matched to whatever session id codex/claude assigns
    /// to a session created now in `cwd`. No-op for a blank label.
    pub fn add_pending_label(
        &mut self,
        agent: &str,
        cwd: PathBuf,
        after: DateTime<Utc>,
        label: &str,
    ) {
        let label = label.trim();
        if !label.is_empty() {
            self.pending_labels.push(PendingLabel {
                agent: agent.to_string(),
                cwd,
                after,
                label: label.to_string(),
            });
        }
    }

    pub fn add_pending_handoff(
        &mut self,
        parent_id: &str,
        agent: &str,
        cwd: PathBuf,
        after: DateTime<Utc>,
        artifact: PathBuf,
    ) {
        self.pending_handoffs.push(PendingHandoff {
            parent_id: parent_id.to_string(),
            agent: agent.to_string(),
            cwd,
            after,
            artifact,
        });
    }

    pub fn set_handoff_link(
        &mut self,
        child_id: &str,
        parent_id: &str,
        artifact: PathBuf,
        created_at: DateTime<Utc>,
    ) {
        self.handoff_links.insert(
            child_id.to_string(),
            HandoffLink {
                parent_id: parent_id.to_string(),
                artifact,
                created_at,
            },
        );
    }

    pub fn handoff_parent(&self, child_id: &str) -> Option<&str> {
        self.handoff_links
            .get(child_id)
            .map(|link| link.parent_id.as_str())
    }

    pub fn thread_root<'a>(&'a self, session_id: &'a str) -> &'a str {
        let mut current = session_id;
        for _ in 0..16 {
            let Some(parent) = self.handoff_parent(current) else {
                break;
            };
            current = parent;
        }
        current
    }

    /// Try to match queued labels to freshly scanned sessions; expire entries
    /// older than an hour. Returns true if anything changed (label assigned or
    /// expired) so the caller can persist + re-apply.
    pub fn resolve_pending(&mut self, sessions: &[Session]) -> bool {
        let now = Utc::now();
        let mut changed = false;

        if !self.pending_labels.is_empty() {
            changed |= self.resolve_pending_labels(sessions, now);
        }
        if !self.pending_handoffs.is_empty() {
            changed |= self.resolve_pending_handoffs(sessions, now);
        }
        changed
    }

    fn resolve_pending_labels(&mut self, sessions: &[Session], now: DateTime<Utc>) -> bool {
        let mut changed = false;
        let mut still = Vec::new();
        for p in std::mem::take(&mut self.pending_labels) {
            if now.signed_duration_since(p.after) > chrono::Duration::hours(1) {
                changed = true; // expired, drop
                continue;
            }
            let matched = sessions
                .iter()
                .filter(|s| {
                    s.agent.as_str() == p.agent
                        && s.cwd == p.cwd
                        && s.started_at.is_some_and(|t| t >= p.after)
                        && !self.labels.contains_key(&s.id)
                })
                .max_by_key(|s| s.started_at)
                .map(|s| s.id.clone());
            match matched {
                Some(id) => {
                    self.labels.insert(id, p.label);
                    changed = true;
                }
                None => still.push(p),
            }
        }
        self.pending_labels = still;
        changed
    }

    fn resolve_pending_handoffs(&mut self, sessions: &[Session], now: DateTime<Utc>) -> bool {
        let mut changed = false;
        let mut still = Vec::new();
        for p in std::mem::take(&mut self.pending_handoffs) {
            if p.parent_id.starts_with("orch:") {
                changed = true;
                continue;
            }
            if now.signed_duration_since(p.after) > chrono::Duration::hours(1) {
                changed = true;
                continue;
            }
            let matched = sessions
                .iter()
                .filter(|s| {
                    s.agent.as_str() == p.agent
                        && s.cwd == p.cwd
                        && s.started_at.is_some_and(|t| t >= p.after)
                        && !self.handoff_links.contains_key(&s.id)
                })
                .max_by_key(|s| s.started_at)
                .map(|s| s.id.clone());
            match matched {
                Some(id) => {
                    self.set_handoff_link(&id, &p.parent_id, p.artifact, p.after);
                    changed = true;
                }
                None => still.push(p),
            }
        }
        self.pending_handoffs = still;
        changed
    }

    pub fn set_label(&mut self, id: &str, label: &str) {
        let label = label.trim();
        if label.is_empty() {
            self.labels.remove(id);
        } else {
            self.labels.insert(id.to_string(), label.to_string());
        }
    }

    /// Stamp each session's `archived` flag and user label from this state.
    /// A label replaces the auto-extracted title (shown with a 🏷 marker).
    pub fn apply(&self, sessions: &mut [Session]) {
        for s in sessions.iter_mut() {
            s.archived = self.is_archived(&s.id);
            if let Some(label) = self.label_for(&s.id) {
                s.title = format!("🏷 {label}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_atomic_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        let mut s = State::default();
        s.set_archived("abc", true);
        s.last_scope = Some("global".into());
        s.save_to(&path).unwrap();

        let loaded = State::load_from(&path);
        assert!(loaded.is_archived("abc"));
        assert_eq!(loaded.last_scope.as_deref(), Some("global"));
        assert_eq!(loaded.version, 1);
    }

    #[test]
    fn missing_file_is_empty_state() {
        let dir = tempdir().unwrap();
        let s = State::load_from(&dir.path().join("nope.json"));
        assert!(s.archived.is_empty());
    }

    #[test]
    fn corrupt_file_falls_back_to_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "{not json").unwrap();
        let s = State::load_from(&path);
        assert!(s.archived.is_empty());
    }

    #[test]
    fn label_set_persist_and_apply() {
        use crate::session::{Agent, Session, TokenUsage};
        use std::path::PathBuf;
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = State::default();
        s.set_label("sid-1", "  My deploy session  "); // trimmed
        s.set_label("sid-2", "   "); // blank => no-op
        s.save_to(&path).unwrap();

        let loaded = State::load_from(&path);
        assert_eq!(loaded.label_for("sid-1"), Some("My deploy session"));
        assert_eq!(loaded.label_for("sid-2"), None);

        let mut sessions = vec![Session {
            id: "sid-1".into(),
            agent: Agent::Codex,
            cwd: PathBuf::new(),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            tokens: TokenUsage::default(),
            title: "auto title".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }];
        loaded.apply(&mut sessions);
        assert_eq!(sessions[0].title, "🏷 My deploy session");
    }

    #[test]
    fn set_archived_toggles() {
        let mut s = State::default();
        s.set_archived("id1", true);
        assert!(s.is_archived("id1"));
        s.set_archived("id1", false);
        assert!(!s.is_archived("id1"));
    }

    #[test]
    fn pending_handoff_resolves_to_thread_link() {
        use crate::session::{Agent, Session, TokenUsage};
        use std::path::PathBuf;

        let now = Utc::now();
        let mut state = State::default();
        state.add_pending_handoff(
            "claude-root",
            "codex",
            PathBuf::from("/work"),
            now - chrono::Duration::seconds(5),
            PathBuf::from("/tmp/handoff.md"),
        );
        let sessions = vec![Session {
            id: "codex-child".into(),
            agent: Agent::Codex,
            cwd: PathBuf::from("/work"),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            tokens: TokenUsage::default(),
            title: "child".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }];

        assert!(state.resolve_pending(&sessions));
        assert!(state.pending_handoffs.is_empty());
        assert_eq!(state.handoff_parent("codex-child"), Some("claude-root"));
        assert_eq!(state.thread_root("codex-child"), "claude-root");
    }

    #[test]
    fn pending_orchestration_handoffs_are_discarded() {
        let now = Utc::now();
        let mut state = State::default();
        state.add_pending_handoff(
            "orch:main:1",
            "codex",
            std::path::PathBuf::from("/work"),
            now,
            std::path::PathBuf::from("mindplayer-orchestration"),
        );

        assert!(state.resolve_pending(&[]));
        assert!(state.pending_handoffs.is_empty());
        assert!(state.handoff_links.is_empty());
    }
}
