//! Active detail ownership and asynchronous archive browsing/restoration.

use super::*;
use mindplayer_core::catalog::{self, CatalogSnapshot};
use std::sync::Weak;

pub(crate) const ARCHIVE_PAGE_SIZE: usize = 200;
const CATALOG_INTERVAL: Duration = Duration::from_secs(60);
pub(super) type CatalogMessage = (u64, Instant, Result<CatalogSnapshot, String>);
pub(super) type SourceKey = (Agent, PathBuf, String, PathBuf);

pub(super) fn source_key(session: &Session) -> SourceKey {
    (
        session.agent,
        session.file.clone(),
        session.id.clone(),
        session.cwd.clone(),
    )
}

// Timestamps are accumulated evidence, but context can decrease after compaction.
// Order overlapping reads by request start, never by delivery or percentage.
pub(super) fn merge_kiro_context(
    observations: &mut HashMap<SourceKey, (Instant, f64)>,
    session: &mut Session,
    started_at: Instant,
) {
    if session.agent != Agent::Kiro {
        return;
    }
    let key = source_key(session);
    if let Some((observed_at, context)) = observations.get_mut(&key) {
        if let Some(value) = session.context_pct.filter(|_| started_at >= *observed_at) {
            *observed_at = started_at;
            *context = value;
        }
        session.context_pct = Some(*context);
    } else if let Some(value) = session.context_pct {
        observations.insert(key, (started_at, value));
    }
}

fn preserve_activity(session: &mut Session, previous: &Session) {
    session.last_active = session.last_active.max(previous.last_active);
    session.last_prompt_at = session.last_prompt_at.max(previous.last_prompt_at);
}

struct Hydration {
    source: Session,
    epoch: u64,
    started_at: Instant,
    result: Receiver<Result<Session, String>>,
}

struct ArchivePage {
    request: u64,
    page: usize,
    total: usize,
    rows: Vec<Session>,
}

#[derive(Default)]
pub(super) struct WorkingSet {
    pub(super) epoch: u64,
    archive_request: u64,
    archive_rx: Option<Receiver<ArchivePage>>,
    archive_due: Option<Instant>,
    archive_selection: Option<SourceKey>,
    archive_overrides: HashMap<SourceKey, Session>,
    indexed_archive: Weak<Vec<Session>>,
    archive_source_indices: Vec<usize>,
    hydration: Option<Hydration>,
    hydrate_queue: VecDeque<Session>,
    next_scan: Option<Instant>,
    saved_use_at: HashMap<SourceKey, Instant>,
    use_migration_pending: bool,
    hook_cursor: usize,
}

impl App {
    pub(super) fn cancel_catalog_work(&mut self) {
        self.invalidate_manual_usage();
        self.working_set.hydration = None;
        self.working_set.hydrate_queue.clear();
        self.working_set.archive_rx = None;
        self.working_set.archive_due = None;
        self.working_set.archive_selection = None;
        self.show_archived = false;
        self.archive_page = 0;
    }

    pub(super) fn can_resume_source(&mut self, session: &Session) -> bool {
        if self
            .all_sessions
            .iter()
            .any(|s| !s.archived && s.id == session.id && source_key(s) != source_key(session))
        {
            self.status =
                "session action refused: native ID belongs to another Active source".into();
            return false;
        }
        true
    }

    /// Reuse an ID-sorted index until the immutable archive snapshot changes.
    pub(super) fn refresh_handoff_archive_index(&mut self) {
        if self.state.handoff_links.is_empty()
            || self.working_set.indexed_archive.as_ptr() == Arc::as_ptr(&self.archive_catalog)
        {
            return;
        }
        let catalog = &self.archive_catalog;
        let indices = &mut self.working_set.archive_source_indices;
        indices.clear();
        indices.extend(0..catalog.len());
        indices.sort_unstable_by(|&left, &right| catalog[left].id.cmp(&catalog[right].id));
        self.working_set.indexed_archive = Arc::downgrade(catalog);
    }

    /// ID-only handoff links cannot choose among distinct off-page sources.
    pub(crate) fn handoff_archive_source_conflicts(&self, source: &Session) -> bool {
        let differs = |other: &Session| {
            other.id == source.id
                && (other.agent != source.agent
                    || other.file != source.file
                    || other.cwd != source.cwd)
        };
        if self.working_set.archive_overrides.values().any(differs) {
            return true;
        }
        if self.archive_catalog.is_empty() {
            return false;
        }
        if self.working_set.indexed_archive.as_ptr() != Arc::as_ptr(&self.archive_catalog) {
            return true;
        }
        let catalog = &self.archive_catalog;
        let indices = &self.working_set.archive_source_indices;
        let start = indices.partition_point(|&index| catalog[index].id < source.id);
        indices[start..]
            .iter()
            .map(|&index| &catalog[index])
            .take_while(|other| other.id == source.id)
            .any(differs)
    }

    /// Registry operations must not mistake an active set/page for a census.
    pub(super) fn known_metadata(&self) -> impl Iterator<Item = &Session> {
        let mut seen = HashSet::new();
        self.all_sessions
            .iter()
            .chain(self.working_set.archive_overrides.values())
            .chain(self.archive_catalog.iter())
            .filter(move |s| seen.insert((s.agent, &s.file, &s.id, &s.cwd)))
    }

    pub(super) fn hook_poll_batch(&mut self) -> Vec<String> {
        const BATCH: usize = 4;
        // Bound native hook and rollout reads while cycling hidden live panes.
        let mut ids: Vec<_> = self
            .ptys
            .keys()
            .filter(|id| !self.ended.contains(*id))
            .cloned()
            .collect();
        ids.sort();
        if ids.is_empty() {
            return ids;
        }
        let start = self.working_set.hook_cursor % ids.len();
        let count = BATCH.min(ids.len());
        self.working_set.hook_cursor = (start + count) % ids.len();
        ids.iter()
            .cycle()
            .skip(start)
            .take(count)
            .cloned()
            .collect()
    }

    pub(super) fn catalog_scan(&self) -> Receiver<CatalogMessage> {
        let started_at = Instant::now();
        let scope = self.scope.clone();
        let roots = self.scan_roots();
        let state = self.state.clone();
        let protected: HashSet<_> = self
            .ptys
            .keys()
            .filter(|id| self.is_running(id))
            .cloned()
            .collect();
        let cache = self.state_path.with_file_name("session-catalog.json");
        let epoch = self.working_set.epoch;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = catalog::scan(&scope, &roots, &state, &protected, &cache, Utc::now())
                .map_err(|_| "session catalog could not be read or saved".to_string());
            let _ = tx.send((epoch, started_at, result));
        });
        rx
    }

    pub(super) fn apply_catalog(&mut self, message: CatalogMessage) -> bool {
        let (epoch, started_at, result) = message;
        if epoch != self.working_set.epoch {
            self.rescan_due = Some(Instant::now());
            return false;
        }
        let mut snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(message) => {
                self.status = message;
                self.working_set.next_scan = Some(Instant::now() + CATALOG_INTERVAL);
                return false;
            }
        };
        {
            let previous: HashMap<_, _> = self
                .known_metadata()
                .map(|s| ((s.agent, &s.file, &s.id, &s.cwd), s))
                .collect();
            for session in snapshot.active.iter_mut().chain(&mut snapshot.archive) {
                if let Some(previous) =
                    previous.get(&(session.agent, &session.file, &session.id, &session.cwd))
                {
                    preserve_activity(session, previous);
                }
            }
        }
        for session in snapshot.active.iter_mut().chain(&mut snapshot.archive) {
            merge_kiro_context(&mut self.context_observations, session, started_at);
        }
        let mut selected = self.selected_session().map(source_key);
        let mut migrated_use = false;
        for (id, sources) in std::mem::take(&mut snapshot.use_sources) {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                self.state.last_used_sources.entry(id)
            {
                entry.insert(sources);
                migrated_use = true;
            }
        }
        self.working_set.use_migration_pending |= migrated_use;
        if self.working_set.use_migration_pending {
            const RETRY: &str = "session source migration could not be saved; will retry";
            if self.save_state().is_ok() {
                self.working_set.use_migration_pending = false;
                if self.status == RETRY {
                    self.status.clear();
                }
            } else {
                self.status = RETRY.into();
            }
        }
        let pending_after = self
            .state
            .pending_labels
            .iter()
            .map(|p| p.after)
            .chain(self.state.pending_handoffs.iter().map(|p| p.after))
            .min();
        if let Some(after) = pending_after {
            // A just-closed session can already be archived when its first
            // file arrives. Resolve only recent candidates, across both sets.
            let candidates: Vec<_> = snapshot
                .active
                .iter()
                .chain(&snapshot.archive)
                .filter(|s| s.started_at.is_some_and(|at| at >= after))
                .cloned()
                .collect();
            // In-process placeholders already own these records. Generic
            // timestamp matching cannot distinguish their baseline neighbors.
            // Leave records from earlier runs eligible for normal recovery.
            let extras = &self.extra_sessions;
            let held_labels: Vec<_> = self
                .state
                .pending_labels
                .extract_if(.., |pending| {
                    extras
                        .iter()
                        .any(|extra| spawn::owns_pending_label(extra, pending))
                })
                .collect();
            let links = &self.state.handoff_links;
            let held_handoffs: Vec<_> = self
                .state
                .pending_handoffs
                .extract_if(.., |pending| {
                    extras.iter().any(|extra| {
                        links
                            .get(&extra.id)
                            .is_some_and(|link| spawn::owns_pending_handoff(extra, link, pending))
                    })
                })
                .collect();
            let resolved = self.state.resolve_pending(&candidates);
            self.state.pending_labels.extend(held_labels);
            self.state.pending_handoffs.extend(held_handoffs);
            if resolved {
                let _ = self.save_state();
            }
        }
        // A live pane may have appeared while discovery ran. Its detail/PTY
        // ownership takes precedence over a stale filesystem observation.
        let owners: HashMap<_, _> = self
            .all_sessions
            .iter()
            .filter(|s| !s.archived && self.is_running(&s.id))
            .map(|s| (s.id.as_str(), s))
            .collect();
        let mut active = Vec::with_capacity(snapshot.active.len());
        for mut session in snapshot.active {
            if owners
                .get(session.id.as_str())
                .is_some_and(|owner| source_key(owner) != source_key(&session))
            {
                session.archived = true;
                snapshot.archive.push(session);
            } else {
                active.push(session);
            }
        }
        for owner in owners.values() {
            if !active.iter().any(|s| source_key(s) == source_key(owner)) {
                let mut session = (*owner).clone();
                if let Some((_, context)) = self.context_observations.get(&source_key(&session)) {
                    session.context_pct = Some(*context);
                }
                active.push(session);
            }
        }
        let active_sources: HashSet<_> = active.iter().map(source_key).collect();
        snapshot
            .archive
            .retain(|s| !active_sources.contains(&source_key(s)));
        snapshot.active = active;
        self.state.apply(&mut snapshot.active);
        for session in &mut snapshot.active {
            usage::restore(&self.manual_usage, session);
            session.archived = false;
        }
        // Archive labels are stamped only on its bounded page in the worker.
        self.all_sessions = snapshot.active;
        self.archive_matches = snapshot.archive.len();
        self.archive_catalog = Arc::new(snapshot.archive);
        self.working_set.archive_overrides.clear();
        self.working_set.next_scan = Some(Instant::now() + CATALOG_INTERVAL);
        self.merge_extras(&mut selected);
        self.rebuild_visible();
        if self.show_archived {
            self.request_archive_page();
            self.working_set.archive_selection = selected;
        } else if let Some(key) = selected {
            if let Some(row) = self.row_of_source(&key) {
                self.selected = row;
            }
        }
        true
    }

    /// Debounce archive queries without ever searching the catalog on input.
    pub(super) fn request_archive_page(&mut self) {
        self.working_set.archive_selection = None;
        self.working_set.archive_request = self.working_set.archive_request.wrapping_add(1);
        self.working_set.archive_due = Some(Instant::now() + Duration::from_millis(40));
        // Enter must never act on a previous query/page while its replacement
        // is pending. Active detail ownership stays intact during the search.
        if self.show_archived {
            self.all_sessions.retain(|s| !s.archived);
            self.selected = 0;
            self.rebuild_visible();
        }
    }

    pub(super) fn archive_available(&self) -> bool {
        !self.archive_catalog.is_empty() || !self.working_set.archive_overrides.is_empty()
    }

    pub(super) fn row_of_source(&self, key: &SourceKey) -> Option<usize> {
        self.visible.iter().position(|row| match row {
            Row::Session(index) => source_key(&self.all_sessions[*index]) == *key,
            _ => false,
        })
    }

    pub fn move_archive_page(&mut self, forward: bool) {
        if !self.show_archived {
            return;
        }
        if forward {
            if (self.archive_page + 1) * ARCHIVE_PAGE_SIZE >= self.archive_matches {
                return;
            }
            self.archive_page += 1;
        } else {
            self.archive_page = self.archive_page.saturating_sub(1);
        }
        self.request_archive_page();
    }

    fn start_archive_page(&mut self) {
        if self.working_set.archive_rx.is_some() || !self.show_archived {
            return;
        }
        let catalog = Arc::clone(&self.archive_catalog);
        let overrides = self.working_set.archive_overrides.clone();
        let active: HashSet<_> = self
            .all_sessions
            .iter()
            .filter(|s| !s.archived)
            .map(source_key)
            .collect();
        let query = self
            .search_query
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let state = self.state.clone();
        let page = self.archive_page;
        let selected = self.working_set.archive_selection.clone();
        let request = self.working_set.archive_request;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let root_titles: HashMap<_, _> = catalog
                .iter()
                .chain(overrides.values())
                .map(|s| {
                    (
                        s.id.as_str(),
                        state
                            .labels
                            .get(&s.id)
                            .map(String::as_str)
                            .unwrap_or(&s.title),
                    )
                })
                .collect();
            let mut matches: Vec<&Session> = overrides
                .values()
                .chain(
                    catalog
                        .iter()
                        .filter(|s| !overrides.contains_key(&source_key(s))),
                )
                .filter(|s| !active.contains(&source_key(s)) && !s.is_subagent)
                .filter(|s| {
                    let title = state
                        .labels
                        .get(&s.id)
                        .map(String::as_str)
                        .unwrap_or(&s.title);
                    query.is_empty()
                        || title.to_lowercase().contains(&query)
                        || s.id.to_lowercase().contains(&query)
                        || s.agent.as_str().contains(&query)
                        || root_titles
                            .get(state.thread_root(&s.id))
                            .is_some_and(|title| title.to_lowercase().contains(&query))
                })
                .collect();
            matches.sort_by_cached_key(|s| {
                (
                    std::cmp::Reverse(s.last_prompt_at.or(s.last_active)),
                    &s.id,
                    s.agent.as_str(),
                    &s.file,
                )
            });
            let total = matches.len();
            let page = selected
                .as_ref()
                .and_then(|key| {
                    matches.iter().position(|s| {
                        s.agent == key.0 && s.file == key.1 && s.id == key.2 && s.cwd == key.3
                    })
                })
                .map(|position| position / ARCHIVE_PAGE_SIZE)
                .unwrap_or(page)
                .min(total.saturating_sub(1) / ARCHIVE_PAGE_SIZE);
            let mut rows: Vec<_> = matches
                .into_iter()
                .skip(page * ARCHIVE_PAGE_SIZE)
                .take(ARCHIVE_PAGE_SIZE)
                .cloned()
                .collect();
            state.apply(&mut rows);
            for row in &mut rows {
                row.archived = true;
            }
            let _ = tx.send(ArchivePage {
                request,
                page,
                total,
                rows,
            });
        });
        self.working_set.archive_rx = Some(rx);
        self.working_set.archive_due = None;
    }

    pub(super) fn queue_archive_restore(&mut self, session: Session) {
        if self
            .working_set
            .hydration
            .as_ref()
            .is_some_and(|p| source_key(&p.source) == source_key(&session))
            || self
                .working_set
                .hydrate_queue
                .iter()
                .any(|s| source_key(s) == source_key(&session))
        {
            return;
        }
        self.status = format!("loading archived session {}", short(&session.id));
        self.working_set.hydrate_queue.push_back(session);
    }

    fn start_archive_restore(&mut self) {
        if self.working_set.hydration.is_some() {
            return;
        }
        let Some(session) = self.working_set.hydrate_queue.pop_front() else {
            return;
        };
        let source = session.clone();
        let started_at = Instant::now();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = catalog::hydrate(&session).map_err(|_| {
                "archive restore failed: source missing, unreadable, or changed identity"
                    .to_string()
            });
            let _ = tx.send(result);
        });
        self.working_set.hydration = Some(Hydration {
            source,
            epoch: self.working_set.epoch,
            started_at,
            result: rx,
        });
    }

    pub(super) fn note_session_use(&mut self, id: &str) {
        if mindplayer_core::session::is_synthetic_id(id) {
            return;
        }
        let source = self
            .all_sessions
            .iter()
            .chain(&self.extra_sessions)
            .find(|session| session.id == id && !session.archived);
        let Some(source) = source else {
            return;
        };
        let key = source_key(source);
        let live_owner = self
            .pty_owner_for_source(source)
            .is_some_and(|owner| self.is_running(&owner));
        self.state.record_session_use(source, Utc::now());
        // Live owners are retained when a catalog is adopted. Invalidating on
        // every keystroke would starve scans during continuous typing. A use
        // without that protection can change membership and must invalidate.
        if !live_owner {
            self.working_set.epoch = self.working_set.epoch.wrapping_add(1);
        }
        if self
            .working_set
            .saved_use_at
            .get(&key)
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
            && self.save_state().is_ok()
        {
            self.working_set.saved_use_at.insert(key, Instant::now());
        }
    }

    pub(super) fn remember_archived(&mut self, mut session: Session) {
        self.working_set.epoch = self.working_set.epoch.wrapping_add(1);
        self.invalidate_manual_usage();
        session.archived = true;
        self.working_set
            .hydrate_queue
            .retain(|s| s.id != session.id);
        if self
            .working_set
            .hydration
            .as_ref()
            .is_some_and(|p| p.source.id == session.id)
        {
            self.working_set.hydration = None;
        }
        self.working_set
            .archive_overrides
            .insert(source_key(&session), session);
        self.request_archive_page();
    }

    /// Bounded adoption: archive results contain at most one page; hydration
    /// handles one requested session at a time. Neither performs source I/O.
    pub fn poll_working_set(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.working_set.archive_rx {
            match rx.try_recv() {
                Ok(page) => {
                    self.working_set.archive_rx = None;
                    if page.request == self.working_set.archive_request && self.show_archived {
                        self.all_sessions.retain(|s| !s.archived);
                        self.all_sessions.extend(page.rows);
                        self.archive_page = page.page;
                        self.archive_matches = page.total;
                        self.selected = 0;
                        self.rebuild_visible();
                        if let Some(key) = self.working_set.archive_selection.take() {
                            if let Some(row) = self.row_of_source(&key) {
                                self.selected = row;
                            }
                        }
                        changed = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.working_set.archive_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self
            .working_set
            .archive_due
            .is_some_and(|at| Instant::now() >= at)
        {
            self.start_archive_page();
        }
        let hydrated = self.working_set.hydration.as_ref().and_then(|pending| {
            match pending.result.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("archive restore worker stopped".to_string()))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            }
        });
        if let Some(result) = hydrated {
            let request = self
                .working_set
                .hydration
                .take()
                .expect("hydration result has a request");
            if request.epoch != self.working_set.epoch {
                // Re-evaluate this explicit request after membership changes.
                // Closing its source removes the request before this point.
                self.working_set.hydrate_queue.push_front(request.source);
                self.start_archive_restore();
                return changed;
            }
            match result {
                Ok(mut session) => {
                    let id = session.id.clone();
                    if self
                        .all_sessions
                        .iter()
                        .any(|s| s.id == id && !s.archived && source_key(s) != source_key(&session))
                    {
                        self.status = "archive restore refused: this native ID is already active from another source".to_string();
                        return true;
                    }
                    // Persist promotion before launch, and do not falsely
                    // report a durable restore if the sidecar is unwritable.
                    let previous = self.state.clone();
                    self.state.record_session_use(&session, Utc::now());
                    if self.save_state().is_err() {
                        self.state = previous;
                        self.status =
                            "archive restore failed: could not save Active membership".to_string();
                    } else {
                        self.working_set.epoch = self.working_set.epoch.wrapping_add(1);
                        self.state.apply(std::slice::from_mut(&mut session));
                        if let Some(previous) = self.known_metadata().find(|s| {
                            s.agent == session.agent
                                && s.file == session.file
                                && s.id == session.id
                                && s.cwd == session.cwd
                        }) {
                            preserve_activity(&mut session, previous);
                        }
                        merge_kiro_context(
                            &mut self.context_observations,
                            &mut session,
                            request.started_at,
                        );
                        self.restore_manual_usage(&mut session);
                        session.archived = false;
                        self.all_sessions
                            .retain(|s| !s.archived && source_key(s) != source_key(&session));
                        self.all_sessions.push(session);
                        self.show_archived = false;
                        self.search_query = None;
                        self.rebuild_visible();
                        if let Some(row) = self.row_of_session(&id) {
                            self.selected = row;
                            self.request_resume();
                        }
                    }
                }
                Err(message) => self.status = message,
            }
            changed = true;
        }
        self.start_archive_restore();
        if self
            .working_set
            .next_scan
            .is_some_and(|at| Instant::now() >= at)
        {
            self.start_bg_rescan();
            self.working_set.next_scan = Some(Instant::now() + CATALOG_INTERVAL);
        }
        changed
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn handoff_archive_identity_checks_track_generations_and_live_overrides() {
        let mut app = crate::app::tests::isolated_app();
        let source = crate::app::tests::session("same-id", Agent::Claude, false);
        app.state.set_handoff_link(
            "child",
            &source.id,
            PathBuf::from("/fixture/handoff"),
            Utc::now(),
        );
        app.archive_catalog = Arc::new(vec![source.clone()]);
        app.refresh_handoff_archive_index();
        assert!(!app.handoff_archive_source_conflicts(&source));
        for dimension in ["provider", "file", "cwd"] {
            let mut other = source.clone();
            match dimension {
                "provider" => other.agent = Agent::Codex,
                "file" => other.file = PathBuf::from("/other-account/session"),
                "cwd" => other.cwd = PathBuf::from("/other-workspace"),
                _ => unreachable!(),
            }
            let key = source_key(&other);
            app.working_set.archive_overrides.insert(key.clone(), other);
            assert!(
                app.handoff_archive_source_conflicts(&source),
                "{dimension} differs without replacing the catalog"
            );
            app.working_set.archive_overrides.remove(&key);
            assert!(!app.handoff_archive_source_conflicts(&source));
        }
        let unrelated = crate::app::tests::session("other-id", Agent::Claude, true);
        app.archive_catalog = Arc::new(vec![unrelated, source.clone()]);
        assert!(
            app.handoff_archive_source_conflicts(&source),
            "an unindexed generation cannot prove uniqueness"
        );
        app.refresh_handoff_archive_index();
        assert!(!app.handoff_archive_source_conflicts(&source));
    }

    #[test]
    fn archive_hydration_preserves_newer_activity_and_context() {
        let mut app = crate::app::tests::isolated_app();
        let started_at = Instant::now();
        let now = Utc::now();
        let mut source = crate::app::tests::session("kiro-restore", Agent::Kiro, true);
        source.last_active = Some(now);
        source.last_prompt_at = Some(now);
        source.context_pct = Some(30.0);
        app.context_observations.insert(
            source_key(&source),
            (started_at + Duration::from_secs(1), 30.0),
        );
        app.all_sessions.push(source.clone());
        app.archive_catalog = Arc::new(vec![source.clone()]);
        let mut hydrated = source.clone();
        hydrated.last_active = Some(now - chrono::Duration::minutes(1));
        hydrated.last_prompt_at = None;
        hydrated.context_pct = Some(80.0);
        let (tx, rx) = mpsc::channel();
        app.working_set.hydration = Some(Hydration {
            source,
            epoch: app.working_set.epoch,
            started_at,
            result: rx,
        });
        tx.send(Ok(hydrated)).unwrap();
        assert!(app.poll_working_set());
        let restored = &app.all_sessions[0];
        assert!(!restored.archived);
        assert_eq!(restored.last_active, Some(now));
        assert_eq!(restored.last_prompt_at, Some(now));
        assert_eq!(restored.context_pct, Some(30.0));
        assert!(app.ptys.is_empty());
    }
}
