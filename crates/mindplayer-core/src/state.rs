//! Sidecar state: MindPlayer's own record of which sessions are archived.
//!
//! The original Codex/Claude `.jsonl` files are never modified. "Closing" a
//! session marks its source here. Writes are atomic (temp file + rename); a corrupt
//! file falls back to empty state with a warning.

use crate::session::{Agent, Session};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A label awaiting the session id the agent assigns to a session created
/// through MindPlayer. The CLIs only write the session file once the session
/// has activity, so resolution happens on a later scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingLabel {
    pub agent: String, // Agent::as_str(): "codex" | "claude" | "kiro" | "cursor"
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

/// Human use bound to a provider and its exact transcript source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUse {
    agent: Agent,
    file: PathBuf,
    used_at: DateTime<Utc>,
}

/// Explicit archive or restore decision for a provider and exact transcript source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArchive {
    agent: Agent,
    file: PathBuf,
    archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub archived: BTreeSet<String>,
    /// Source decisions override legacy ID-only archives only for an exact match.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub archived_sources: BTreeMap<String, Vec<SourceArchive>>,
    /// Latest human use recorded by MindPlayer, independent of transcript mtime.
    #[serde(default)]
    pub last_used: BTreeMap<String, DateTime<Utc>>,
    /// Source-qualified use. An empty entry prevents ambiguous legacy ID reuse.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last_used_sources: BTreeMap<String, Vec<SourceUse>>,
    /// Days without human activity before automatic archiving; zero disables expiry.
    #[serde(default = "default_active_retention_days")]
    pub active_retention_days: u32,
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
    /// Optional source provenance. Kiro's old marks measured metadata instead
    /// of JSONL, so an unbound mark must not be reused for its conversation.
    /// Kept separate so older readers can still load the numeric marks.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sync_sources: BTreeMap<String, BTreeMap<String, PathBuf>>,
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

fn default_active_retention_days() -> u32 {
    7
}

impl Default for State {
    fn default() -> Self {
        State {
            version: default_version(),
            archived: BTreeSet::new(),
            archived_sources: BTreeMap::new(),
            last_used: BTreeMap::new(),
            last_used_sources: BTreeMap::new(),
            active_retention_days: default_active_retention_days(),
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
            sync_sources: BTreeMap::new(),
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

    /// Atomic durable write: serialize to a private temp file, fsync, then rename.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        use std::io::Write as _;
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            crate::private::create_dir_private(dir)?;
        }
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let data = serde_json::to_string_pretty(self)?;
        {
            let mut file = crate::private::open_private(&tmp, false)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
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
        if let Some(sources) = self.sync_sources.get_mut(target) {
            sources.remove(peer);
            if sources.is_empty() {
                self.sync_sources.remove(target);
            }
        }
        self.sync_marks
            .entry(target.to_string())
            .or_default()
            .insert(peer.to_string(), peer_len);
    }

    /// Return a watermark only when it belongs to this transcript source.
    pub fn sync_mark_for_source(&self, target: &str, peer: &str, source: &Path) -> u64 {
        if self
            .sync_sources
            .get(target)
            .and_then(|m| m.get(peer))
            .map(PathBuf::as_path)
            == Some(source)
        {
            self.sync_mark(target, peer)
        } else {
            0
        }
    }

    /// Record the consumed length and its source together.
    pub fn set_sync_mark_for_source(&mut self, target: &str, peer: &str, source: &Path, len: u64) {
        self.set_sync_mark(target, peer, len);
        self.sync_sources
            .entry(target.to_string())
            .or_default()
            .insert(peer.to_string(), source.to_path_buf());
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
        self.sync_sources.remove(id);
        for sources in self.sync_sources.values_mut() {
            sources.remove(id);
        }
        self.sync_sources.retain(|_, m| !m.is_empty());
    }

    /// Drop watermarks for sessions that no longer exist. Returns true if
    /// anything changed. Called from `prune_categories`' caller side.
    pub fn prune_sync_marks(&mut self, known_sessions: &BTreeSet<String>) -> bool {
        let before = self.sync_marks.len();
        let inner_before: usize = self.sync_marks.values().map(|m| m.len()).sum();
        let sources_before: usize = self.sync_sources.values().map(|m| m.len()).sum();
        self.sync_marks
            .retain(|target, _| known_sessions.contains(target));
        for marks in self.sync_marks.values_mut() {
            marks.retain(|peer, _| known_sessions.contains(peer));
        }
        self.sync_marks.retain(|_, m| !m.is_empty());
        let inner_after: usize = self.sync_marks.values().map(|m| m.len()).sum();
        self.sync_sources.retain(|target, sources| {
            sources.retain(|peer, _| {
                self.sync_marks
                    .get(target)
                    .is_some_and(|marks| marks.contains_key(peer))
            });
            !sources.is_empty()
        });
        let sources_after: usize = self.sync_sources.values().map(|m| m.len()).sum();
        before != self.sync_marks.len()
            || inner_before != inner_after
            || sources_before != sources_after
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

    /// Read an exact source decision, falling back to the legacy ID-only archive.
    pub fn is_session_archived(&self, session: &Session) -> bool {
        self.archived_sources
            .get(&session.id)
            .and_then(|sources| {
                sources
                    .iter()
                    .rev()
                    .find(|source| source.agent == session.agent && source.file == session.file)
            })
            .map_or_else(|| self.is_archived(&session.id), |source| source.archived)
    }

    /// Archive or restore only this source, preserving the legacy ID-only fallback.
    pub fn set_session_archived(&mut self, session: &Session, archived: bool) {
        let sources = self.archived_sources.entry(session.id.clone()).or_default();
        // Replace every matching decision, including duplicates from persisted state.
        sources.retain(|source| source.agent != session.agent || source.file != session.file);
        sources.push(SourceArchive {
            agent: session.agent,
            file: session.file.clone(),
            archived,
        });
    }

    /// Promote a legacy ID on human use, clearing its source-specific decisions.
    pub fn record_use(&mut self, id: &str, now: DateTime<Utc>) {
        self.set_archived(id, false);
        self.last_used
            .entry(id.to_string())
            .and_modify(|last| *last = (*last).max(now))
            .or_insert(now);
    }

    /// Record human use without promoting a same-ID transcript from another source.
    pub fn record_session_use(&mut self, session: &Session, now: DateTime<Utc>) {
        self.set_session_archived(session, false);
        let uses = self
            .last_used_sources
            .entry(session.id.clone())
            .or_default();
        let mut used_at = now;
        uses.retain(|usage| {
            if usage.agent == session.agent && usage.file == session.file {
                used_at = used_at.max(usage.used_at);
                false
            } else {
                true
            }
        });
        uses.push(SourceUse {
            agent: session.agent,
            file: session.file.clone(),
            used_at,
        });
        self.last_used
            .entry(session.id.clone())
            .and_modify(|last| *last = (*last).max(used_at))
            .or_insert(used_at);
    }

    /// Bind legacy ID-only use against a metadata census, refusing ambiguous sources.
    /// Missing IDs remain unresolved until their source is discovered.
    pub fn bind_legacy_use_sources<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a Session>,
    ) -> bool {
        let mut unique: HashMap<&str, Option<&Session>> = HashMap::new();
        for session in sessions {
            if !self.last_used.contains_key(&session.id)
                || self.last_used_sources.contains_key(&session.id)
            {
                continue;
            }
            unique
                .entry(&session.id)
                .and_modify(|owner| {
                    if owner.is_some_and(|s| s.agent != session.agent || s.file != session.file) {
                        *owner = None;
                    }
                })
                .or_insert(Some(session));
        }
        let changed = !unique.is_empty();
        for (id, source) in unique {
            let uses = source
                .into_iter()
                .map(|s| SourceUse {
                    agent: s.agent,
                    file: s.file.clone(),
                    used_at: self.last_used[id],
                })
                .collect();
            self.last_used_sources.insert(id.to_string(), uses);
        }
        changed
    }

    /// Classify by human recency, with all running sessions protected from archive.
    ///
    /// Explicit archive overrides in-progress and recency. Creation time is only
    /// a fallback when neither a recorded use nor a genuine prompt is available.
    /// At exactly the retention boundary a session expires; zero disables expiry.
    pub fn is_active_session(
        &self,
        session: &Session,
        now: DateTime<Utc>,
        protected: &HashSet<String>,
    ) -> bool {
        if protected.contains(&session.id) {
            return true;
        }
        if self.is_session_archived(session) {
            return false;
        }
        if self.is_in_progress(&session.id) || self.active_retention_days == 0 {
            return true;
        }
        let used_at = match self.last_used_sources.get(&session.id) {
            Some(uses) => uses
                .iter()
                .find(|usage| usage.agent == session.agent && usage.file == session.file)
                .map(|usage| usage.used_at),
            None => self.last_used.get(&session.id).copied(),
        };
        let human_time = used_at
            .into_iter()
            .chain(session.last_prompt_at)
            .max()
            .or(session.started_at);
        human_time.is_some_and(|last| {
            now.signed_duration_since(last)
                < chrono::Duration::days(i64::from(self.active_retention_days))
        })
    }

    /// Set an ID-wide legacy decision, replacing any source-specific overrides.
    pub fn set_archived(&mut self, id: &str, archived: bool) {
        self.archived_sources.remove(id);
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
            s.archived = self.is_session_archived(s);
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

    fn old_source(file: &str) -> Session {
        Session {
            id: "shared-native".into(),
            agent: Agent::Codex,
            cwd: PathBuf::from("/fixture/project"),
            file: PathBuf::from(file),
            started_at: Some(Utc::now() - chrono::Duration::days(30)),
            last_active: None,
            last_prompt_at: None,
            tokens: Default::default(),
            title: "synthetic session".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    #[test]
    fn source_use_roundtrip_rejects_other_source_and_keeps_latest_time() {
        let chosen = old_source("/fixture/one/session.jsonl");
        let other = old_source("/fixture/two/session.jsonl");
        let now = Utc::now();
        let mut state = State::default();
        state.record_session_use(&chosen, now);
        state.record_session_use(&chosen, now - chrono::Duration::days(20));
        let loaded: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(loaded.is_active_session(&chosen, now, &HashSet::new()));
        assert!(!loaded.is_active_session(&other, now, &HashSet::new()));
        assert_eq!(loaded.last_used_sources[&chosen.id][0].used_at, now);
    }

    #[test]
    fn restoring_same_id_other_source_does_not_reopen_closed_source_after_restart() {
        let closed = old_source("/fixture/one/session.jsonl");
        let restored = old_source("/fixture/two/session.jsonl");
        let now = Utc::now();
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = State::default();
        state.record_session_use(&closed, now);
        state.set_session_archived(&closed, true);
        assert!(!state.is_active_session(&closed, now, &HashSet::new()));
        state.record_session_use(&restored, now);
        state.save_to(&path).unwrap();

        let loaded = State::load_from(&path);
        assert!(loaded.is_active_session(&restored, now, &HashSet::new()));
        assert!(
            !loaded.is_active_session(&closed, now, &HashSet::new()),
            "restoring another source must not undo the original source's close"
        );
        assert!(loaded.is_session_archived(&closed));
        assert!(!loaded.is_session_archived(&restored));
        assert!(!loaded.is_archived(&closed.id));
        let mut sessions = [closed, restored];
        loaded.apply(&mut sessions);
        assert!(sessions[0].archived);
        assert!(!sessions[1].archived);
    }

    #[test]
    fn source_archive_matches_id_provider_and_path_without_archiving_siblings() {
        let now = Utc::now();
        let mut chosen = old_source("/fixture/one/session.jsonl");
        chosen.last_prompt_at = Some(now);
        let mut other_path = chosen.clone();
        other_path.file = PathBuf::from("/fixture/two/session.jsonl");
        let mut other_provider = chosen.clone();
        other_provider.agent = Agent::Claude;
        let mut other_id = chosen.clone();
        other_id.id = "other-native".into();
        let mut state = State::default();
        state.record_session_use(&chosen, now);
        state.set_session_archived(&chosen, true);
        state.record_session_use(&other_provider, now);
        let loaded: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();

        assert!(
            loaded.archived.is_empty(),
            "closing a source must not mark its ID"
        );
        let mut sessions = [chosen, other_path, other_provider, other_id];
        loaded.apply(&mut sessions);
        for (index, session) in sessions.iter().enumerate() {
            assert_eq!(
                loaded.is_session_archived(session),
                index == 0,
                "{session:?}"
            );
            assert_eq!(session.archived, index == 0, "{session:?}");
            assert_eq!(
                loaded.is_active_session(session, now, &HashSet::new()),
                index != 0,
                "{session:?}"
            );
        }
    }

    #[test]
    fn restoring_source_preserves_legacy_archive_for_recent_unmatched_sources() {
        let now = Utc::now();
        let mut closed = old_source("/fixture/one/session.jsonl");
        closed.last_prompt_at = Some(now);
        for (agent, file) in [
            (Agent::Codex, "/fixture/two/session.jsonl"),
            (Agent::Claude, "/fixture/one/session.jsonl"),
        ] {
            let mut restored = old_source(file);
            restored.agent = agent;
            let mut state: State = serde_json::from_value(serde_json::json!({
                "version": 1,
                "archived": [closed.id],
                "last_used": {"shared-native": now},
            }))
            .unwrap();
            assert!(state.archived_sources.is_empty());
            state.record_session_use(&restored, now - chrono::Duration::minutes(1));
            let encoded = serde_json::to_value(&state).unwrap();
            assert_eq!(encoded["version"], 1);
            assert_eq!(encoded["archived"], serde_json::json!([closed.id]));
            assert_eq!(encoded["last_used"][&closed.id], serde_json::json!(now));
            let loaded: State = serde_json::from_value(encoded).unwrap();
            assert!(loaded.is_archived(&closed.id));
            assert!(loaded.is_session_archived(&closed));
            assert!(!loaded.is_session_archived(&restored));
            assert!(
                !loaded.is_active_session(&closed, now, &HashSet::new()),
                "a recent prompt must not override a legacy archive: {agent:?} {file}"
            );
            assert!(loaded.is_active_session(&restored, now, &HashSet::new()));
            let mut sessions = [closed.clone(), restored];
            loaded.apply(&mut sessions);
            assert!(sessions[0].archived);
            assert!(!sessions[1].archived);
        }
    }

    #[test]
    fn source_unarchive_does_not_fabricate_human_activity() {
        let session = old_source("/fixture/one/session.jsonl");
        let mut state: State = serde_json::from_str(r#"{"archived":["shared-native"]}"#).unwrap();
        state.set_session_archived(&session, false);
        let loaded: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(!loaded.is_session_archived(&session));
        assert!(loaded.is_archived(&session.id));
        assert!(loaded.last_used.is_empty());
        assert!(loaded.last_used_sources.is_empty());
        assert!(!loaded.is_active_session(&session, Utc::now(), &HashSet::new()));
    }

    #[test]
    fn explicit_source_archive_overrides_activity_but_not_running_protection() {
        let now = Utc::now();
        let mut session = old_source("/fixture/one/session.jsonl");
        session.last_prompt_at = Some(now);
        let mut state = State::default();
        state.record_session_use(&session, now);
        state.set_in_progress(&session.id, true);
        state.set_session_archived(&session, true);
        assert!(!state.is_active_session(&session, now, &HashSet::new()));
        state.active_retention_days = 0;
        assert!(!state.is_active_session(&session, now, &HashSet::new()));
        assert!(state.is_active_session(&session, now, &HashSet::from([session.id.clone()])));
    }

    #[test]
    fn recording_source_use_prunes_duplicates_without_losing_other_archive_decisions() {
        let now = Utc::now();
        let chosen = old_source("/fixture/one/session.jsonl");
        let other_path = old_source("/fixture/two/session.jsonl");
        let mut other_provider = chosen.clone();
        other_provider.agent = Agent::Claude;
        let mut state = State::default();
        state.set_archived(&chosen.id, true);
        state.set_session_archived(&chosen, false);
        state.set_session_archived(&other_path, true);
        state.set_session_archived(&other_provider, true);
        state
            .archived_sources
            .get_mut(&chosen.id)
            .unwrap()
            .push(SourceArchive {
                agent: chosen.agent,
                file: chosen.file.clone(),
                archived: true,
            });
        state.last_used_sources.insert(
            chosen.id.clone(),
            vec![
                SourceUse {
                    agent: chosen.agent,
                    file: chosen.file.clone(),
                    used_at: now - chrono::Duration::days(20),
                },
                SourceUse {
                    agent: chosen.agent,
                    file: chosen.file.clone(),
                    used_at: now,
                },
            ],
        );
        let mut state: State =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(
            state.is_session_archived(&chosen),
            "the last persisted decision wins"
        );
        state.record_session_use(&chosen, now - chrono::Duration::days(10));
        state.record_session_use(&chosen, now - chrono::Duration::days(5));
        let loaded: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(!loaded.is_session_archived(&chosen));
        assert!(loaded.is_session_archived(&other_path));
        assert!(loaded.is_session_archived(&other_provider));
        assert!(loaded.is_archived(&chosen.id));
        assert_eq!(loaded.archived_sources[&chosen.id].len(), 3);
        assert_eq!(loaded.last_used_sources[&chosen.id].len(), 1);
        assert_eq!(loaded.last_used_sources[&chosen.id][0].used_at, now);
        assert_eq!(loaded.last_used[&chosen.id], now);
        assert!(loaded.is_active_session(&chosen, now, &HashSet::new()));
        assert!(!loaded.is_active_session(&other_path, now, &HashSet::new()));
        assert!(!loaded.is_active_session(&other_provider, now, &HashSet::new()));
    }

    #[test]
    fn legacy_id_operations_replace_source_overrides_and_keep_latest_use() {
        let chosen = old_source("/fixture/one/session.jsonl");
        let other = old_source("/fixture/two/session.jsonl");
        let now = Utc::now();
        let mut state = State::default();
        assert!(serde_json::to_value(&state)
            .unwrap()
            .get("archived_sources")
            .is_none());
        state.record_session_use(&chosen, now);
        state.set_archived(&chosen.id, true);
        assert!(state.is_session_archived(&chosen));
        assert!(state.is_session_archived(&other));
        state.set_session_archived(&chosen, false);
        state.set_archived(&chosen.id, false);
        assert!(!state.is_session_archived(&chosen));
        assert!(!state.is_session_archived(&other));
        assert!(state.archived_sources.is_empty());
        state.set_session_archived(&chosen, true);
        state.record_use(&chosen.id, now - chrono::Duration::days(1));
        assert!(!state.is_session_archived(&chosen));
        assert!(state.archived_sources.is_empty());
        assert_eq!(state.last_used[&chosen.id], now);
    }

    #[test]
    fn legacy_use_binds_unique_source_but_never_guesses_an_ambiguous_source() {
        let chosen = old_source("/fixture/one/session.jsonl");
        let other = old_source("/fixture/two/session.jsonl");
        let now = Utc::now();
        let mut unique = State::default();
        unique.record_use(&chosen.id, now);
        assert!(unique.bind_legacy_use_sources([&chosen, &chosen]));
        assert!(unique.is_active_session(&chosen, now, &HashSet::new()));
        assert!(!unique.is_active_session(&other, now, &HashSet::new()));
        assert!(!unique.bind_legacy_use_sources([&chosen, &other]));
        let mut ambiguous = State::default();
        ambiguous.record_use(&chosen.id, now);
        assert!(ambiguous.bind_legacy_use_sources([&chosen, &other]));
        assert!(!ambiguous.is_active_session(&chosen, now, &HashSet::new()));
        assert!(!ambiguous.is_active_session(&other, now, &HashSet::new()));
        assert_eq!(
            ambiguous.last_used[&chosen.id], now,
            "retain legacy data without assigning it to a guessed owner"
        );
        ambiguous.record_session_use(&other, now);
        assert!(!ambiguous.is_active_session(&chosen, now, &HashSet::new()));
        assert!(ambiguous.is_active_session(&other, now, &HashSet::new()));
    }

    #[test]
    fn source_bound_sync_marks_do_not_reuse_legacy_or_other_sources() {
        let mut state: State =
            serde_json::from_str(r#"{"sync_marks":{"target":{"peer":45}}}"#).unwrap();
        let source = Path::new("/synthetic/conversation.jsonl");
        assert_eq!(state.sync_mark("target", "peer"), 45);
        assert_eq!(state.sync_mark_for_source("target", "peer", source), 0);
        state.set_sync_mark_for_source("target", "peer", source, 90);
        let encoded = serde_json::to_string(&state).unwrap();
        let mut loaded: State = serde_json::from_str(&encoded).unwrap();
        assert_eq!(loaded.sync_mark_for_source("target", "peer", source), 90);
        assert_eq!(
            loaded.sync_mark_for_source("target", "peer", Path::new("/synthetic/replaced.jsonl")),
            0
        );
        // Keep the original numeric field readable by older versions.
        let json: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(json["sync_marks"]["target"]["peer"], 90);
        loaded.set_sync_mark("target", "peer", 123);
        assert_eq!(loaded.sync_mark_for_source("target", "peer", source), 0);
    }

    #[test]
    fn clearing_and_pruning_sync_marks_also_forgets_their_sources() {
        let mut state = State::default();
        let source = Path::new("/synthetic/conversation.jsonl");
        state.set_sync_mark_for_source("target", "peer", source, 90);
        state.set_sync_mark_for_source("peer", "target", source, 80);
        state.clear_sync_marks_for("peer");
        assert!(state.sync_marks.is_empty());
        assert!(state.sync_sources.is_empty());
        state.set_sync_mark_for_source("target", "peer", source, 90);
        assert!(state.prune_sync_marks(&BTreeSet::from(["target".into()])));
        assert!(state.sync_marks.is_empty());
        assert!(state.sync_sources.is_empty());
        assert!(!state.prune_sync_marks(&BTreeSet::from(["target".into()])));
    }

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
