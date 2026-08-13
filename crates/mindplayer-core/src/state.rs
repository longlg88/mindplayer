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
    /// Sessions the user has manually marked as "my work here isn't done yet" —
    /// independent of the live PTY status (a session can be Idle/Ended and
    /// still be in progress, or Working and not be something you're tracking).
    #[serde(default)]
    pub in_progress: BTreeSet<String>,
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
    /// Session ids whose peer-lane thread-sync (see MindPlayer's
    /// `thread_sync_needed`/`spawn_thread_sync_for`) has already run once.
    /// Persisted rather than tracked only in memory: an in-memory-only marker
    /// is wiped every time MindPlayer restarts, so the very next resume after a
    /// restart looked like a fresh reopen and re-triggered the sync — the
    /// "keeps trying to hand off the same content again" bug reported after
    /// leaving and reopening MindPlayer. Once a session id is in here, it stays
    /// synced for good; nothing currently removes an entry.
    #[serde(default)]
    pub thread_synced: BTreeSet<String>,
    #[serde(default)]
    pub last_scope: Option<String>,
    /// Id of the walking character shown on the browse screens (see the TUI's
    /// `walker` module). `None` means "never picked one" and resolves to the
    /// default at render time — an unknown id does too, so removing a
    /// character in a later release can't break an existing state file.
    #[serde(default)]
    pub walker: Option<String>,
    /// Topic groups, keyed by a stable id. The id is what sessions point at, so
    /// renaming a category keeps its membership — and an auto-prefixed label
    /// like `(handoff)pulse` no longer splits a topic in two the way grouping on
    /// the label text would.
    #[serde(default)]
    pub categories: BTreeMap<String, Category>,
    /// session id -> category id. Exclusive: a session belongs to at most one
    /// category, which is what lets the list render it exactly once.
    #[serde(default)]
    pub session_category: BTreeMap<String, String>,
    /// Category ids whose rows are collapsed in the list. Persisted so a folded
    /// topic stays folded across restarts.
    #[serde(default)]
    pub collapsed_categories: BTreeSet<String>,
    /// How much of each peer's transcript a target session has already been
    /// shown: `target id -> peer id -> peer transcript byte length at last sync`.
    ///
    /// This is what replaces the old sync-once-ever rule. Only bytes past the
    /// mark are injected, so a re-sync with no peer activity sends nothing and
    /// the loop terminates on its own — the original problem was not the
    /// re-trigger but re-sending the whole transcript every time.
    #[serde(default)]
    pub sync_marks: BTreeMap<String, BTreeMap<String, u64>>,
}

/// A user-named topic. Sessions join by id, never by name — see
/// [`State::categories`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Whether entering a session in this category pulls in what its peers did
    /// since the last sync. Per-category so a tightly-coupled topic can stay in
    /// lockstep while a loose one stays quiet.
    ///
    /// The default MUST stay explicit: a bare `#[serde(default)]` on a `bool`
    /// yields `false`, which would silently load every category written before
    /// this field as disabled.
    #[serde(default = "default_auto_sync")]
    pub auto_sync: bool,
}

fn default_auto_sync() -> bool {
    true
}

fn default_version() -> u32 {
    1
}

impl Default for State {
    fn default() -> Self {
        State {
            version: default_version(),
            archived: BTreeSet::new(),
            in_progress: BTreeSet::new(),
            labels: BTreeMap::new(),
            pending_labels: Vec::new(),
            pending_handoffs: Vec::new(),
            handoff_links: BTreeMap::new(),
            thread_synced: BTreeSet::new(),
            last_scope: None,
            walker: None,
            categories: BTreeMap::new(),
            session_category: BTreeMap::new(),
            collapsed_categories: BTreeSet::new(),
            sync_marks: BTreeMap::new(),
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
            crate::private::create_dir_private(dir)?;
        }
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let data = serde_json::to_string_pretty(self)?;
        {
            // The temp file carries the same content as the final one, so it is
            // created owner-only too rather than being narrowed after the fact.
            let mut f = crate::private::open_private(&tmp, false)?;
            f.write_all(data.as_bytes())?;
            f.sync_all()?; // durable before the rename
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    // --- categories ---------------------------------------------------------

    /// The category a session belongs to, if any.
    pub fn category_of(&self, session_id: &str) -> Option<&str> {
        self.session_category.get(session_id).map(String::as_str)
    }

    pub fn category_name(&self, cat_id: &str) -> Option<&str> {
        self.categories.get(cat_id).map(|c| c.name.as_str())
    }

    /// Categories as `(id, name)`, ordered by name so the picker and the list
    /// agree and neither depends on map iteration order.
    pub fn categories_by_name(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = self
            .categories
            .iter()
            .map(|(id, c)| (id.as_str(), c.name.as_str()))
            .collect();
        out.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then(a.0.cmp(b.0))
        });
        out
    }

    /// Create a category and return its new id. A blank name is rejected
    /// (`None`) rather than creating an unnameable group. Reuses an existing
    /// category when the trimmed name already matches one, so typing the same
    /// topic twice joins it instead of forking it.
    pub fn create_category(&mut self, name: &str, now: DateTime<Utc>) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if let Some((id, _)) = self
            .categories
            .iter()
            .find(|(_, c)| c.name.eq_ignore_ascii_case(name))
        {
            return Some(id.clone());
        }
        // Ids are `cat_N` past the highest N in use, so they stay unique even
        // after deletions and never collide with a hand-edited state file.
        let next = self
            .categories
            .keys()
            .filter_map(|k| k.strip_prefix("cat_"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .map_or(1, |n| n + 1);
        let id = format!("cat_{next}");
        self.categories.insert(
            id.clone(),
            Category {
                name: name.to_string(),
                created_at: now,
                auto_sync: default_auto_sync(),
            },
        );
        Some(id)
    }

    /// Put a session in a category, replacing any previous one (membership is
    /// exclusive). Unknown category ids are ignored rather than creating a
    /// dangling pointer the list would have to defend against.
    pub fn assign_category(&mut self, session_id: &str, cat_id: &str) -> bool {
        if !self.categories.contains_key(cat_id) {
            return false;
        }
        self.session_category
            .insert(session_id.to_string(), cat_id.to_string());
        true
    }

    /// Take a session out of its category. The session itself is untouched.
    pub fn clear_category(&mut self, session_id: &str) {
        self.session_category.remove(session_id);
    }

    pub fn rename_category(&mut self, cat_id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        match self.categories.get_mut(cat_id) {
            Some(c) => {
                c.name = name.to_string();
                true
            }
            None => false,
        }
    }

    pub fn category_auto_sync(&self, cat_id: &str) -> bool {
        self.categories.get(cat_id).is_some_and(|c| c.auto_sync)
    }

    pub fn set_category_auto_sync(&mut self, cat_id: &str, on: bool) -> bool {
        match self.categories.get_mut(cat_id) {
            Some(c) => {
                c.auto_sync = on;
                true
            }
            None => false,
        }
    }

    // --- sync watermarks ----------------------------------------------------

    /// Peer transcript length `target` was last shown. 0 = never synced.
    pub fn sync_mark(&self, target: &str, peer: &str) -> u64 {
        self.sync_marks
            .get(target)
            .and_then(|m| m.get(peer))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_sync_mark(&mut self, target: &str, peer: &str, peer_len: u64) {
        self.sync_marks
            .entry(target.to_string())
            .or_default()
            .insert(peer.to_string(), peer_len);
    }

    /// Forget every watermark involving a session, so a re-created or
    /// re-categorized session starts clean instead of inheriting a stale mark
    /// that would hide real content from it.
    pub fn clear_sync_marks_for(&mut self, id: &str) {
        self.sync_marks.remove(id);
        for marks in self.sync_marks.values_mut() {
            marks.remove(id);
        }
        self.sync_marks.retain(|_, m| !m.is_empty());
    }

    /// Drop watermarks for sessions that no longer exist. Returns true if
    /// anything changed. Called from `prune_categories`' caller side.
    pub fn prune_sync_marks(&mut self, known_sessions: &BTreeSet<String>) -> bool {
        let before = self.sync_marks.len();
        let inner_before: usize = self.sync_marks.values().map(|m| m.len()).sum();
        self.sync_marks
            .retain(|target, _| known_sessions.contains(target));
        for marks in self.sync_marks.values_mut() {
            marks.retain(|peer, _| known_sessions.contains(peer));
        }
        self.sync_marks.retain(|_, m| !m.is_empty());
        let inner_after: usize = self.sync_marks.values().map(|m| m.len()).sum();
        before != self.sync_marks.len() || inner_before != inner_after
    }

    pub fn is_collapsed(&self, cat_id: &str) -> bool {
        self.collapsed_categories.contains(cat_id)
    }

    pub fn set_collapsed(&mut self, cat_id: &str, collapsed: bool) {
        if collapsed {
            self.collapsed_categories.insert(cat_id.to_string());
        } else {
            self.collapsed_categories.remove(cat_id);
        }
    }

    /// Drop categories nothing points at, and membership/collapse entries whose
    /// category is gone. Returns true if anything changed, so the caller only
    /// writes when it must. Called after a scan, where a session id that no
    /// longer exists would otherwise keep an empty group on screen forever.
    pub fn prune_categories(&mut self, known_sessions: &BTreeSet<String>) -> bool {
        let before = (
            self.categories.len(),
            self.session_category.len(),
            self.collapsed_categories.len(),
        );
        // Membership only for sessions that still exist and categories that do.
        self.session_category
            .retain(|sid, cid| known_sessions.contains(sid) && self.categories.contains_key(cid));
        let in_use: BTreeSet<&String> = self.session_category.values().collect();
        self.categories.retain(|id, _| in_use.contains(id));
        self.collapsed_categories
            .retain(|id| self.categories.contains_key(id));
        before
            != (
                self.categories.len(),
                self.session_category.len(),
                self.collapsed_categories.len(),
            )
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

    pub fn is_in_progress(&self, id: &str) -> bool {
        self.in_progress.contains(id)
    }

    pub fn set_in_progress(&mut self, id: &str, in_progress: bool) {
        if in_progress {
            self.in_progress.insert(id.to_string());
        } else {
            self.in_progress.remove(id);
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

    /// Drop the newest queued label for this agent/dir/label. Called when a new
    /// session is closed before its rollout file ever appeared: without this the
    /// queue outlives the session for an hour and stamps the closed session's
    /// name onto whatever the agent writes next, so the closed session appears
    /// to come back. Returns true if an entry was removed.
    pub fn remove_pending_label(&mut self, agent: &str, cwd: &Path, label: &str) -> bool {
        let label = label.trim();
        let newest = self
            .pending_labels
            .iter()
            .enumerate()
            .filter(|(_, p)| p.agent == agent && p.cwd == cwd && p.label == label)
            .max_by_key(|(_, p)| p.after)
            .map(|(i, _)| i);
        match newest {
            Some(i) => {
                self.pending_labels.remove(i);
                true
            }
            None => false,
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
            last_prompt_at: None,
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
            last_prompt_at: None,
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
