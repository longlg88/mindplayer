//! Active detail ownership and asynchronous archive browsing/restoration.

use super::*;
use mindplayer_core::catalog::{self, CatalogSnapshot};
use mindplayer_core::discovery::SessionRoot;
use std::path::Path;

pub(crate) const ARCHIVE_PAGE_SIZE: usize = 200;
const CATALOG_INTERVAL: Duration = Duration::from_secs(60);
pub(super) type CatalogMessage = (u64, Result<CatalogSnapshot, String>);
type SourceKey = (Agent, PathBuf, String);

fn source_key(session: &Session) -> SourceKey {
    (session.agent, session.file.clone(), session.id.clone())
}

fn codex_source_root<'a>(roots: &'a [SessionRoot], file: &Path) -> Option<&'a PathBuf> {
    if !file.is_absolute()
        || file
            .components()
            .any(|p| p == std::path::Component::ParentDir)
    {
        return None;
    }
    roots
        .iter()
        .filter(|root| root.agent == Agent::Codex && file.starts_with(&root.dir))
        .max_by_key(|root| root.dir.components().count())
        .map(|root| &root.dir)
}

struct NativeRoute {
    thread: String,
    root: PathBuf,
    source: Option<Session>,
    result: Option<Receiver<Result<Session, String>>>,
    retry_at: Instant,
}

struct Hydration {
    source: Session,
    epoch: u64,
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
    hydration: Option<Hydration>,
    hydrate_queue: VecDeque<Session>,
    next_scan: Option<Instant>,
    saved_use_at: HashMap<SourceKey, Instant>,
    use_migration_pending: bool,
    hook_cursor: usize,
    native_routes: HashMap<String, NativeRoute>,
}

impl App {
    pub(super) fn cancel_catalog_work(&mut self) {
        self.working_set.hydration = None;
        self.working_set.hydrate_queue.clear();
        self.working_set.archive_rx = None;
        self.working_set.archive_due = None;
        self.working_set.archive_selection = None;
        self.working_set.native_routes.clear();
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

    /// Registry operations must not mistake an active set/page for a census.
    pub(super) fn known_metadata(&self) -> impl Iterator<Item = &Session> {
        let mut seen = HashSet::new();
        self.all_sessions
            .iter()
            .chain(self.working_set.archive_overrides.values())
            .chain(self.archive_catalog.iter())
            .filter(move |s| seen.insert((s.agent, &s.file, &s.id)))
    }

    pub(super) fn apply_row_state(&mut self) {
        for session in &mut self.all_sessions {
            let archived = session.archived;
            self.state.apply(std::slice::from_mut(session));
            session.archived |= archived;
        }
    }

    pub(super) fn hook_poll_batch(&mut self) -> Vec<String> {
        const BATCH: usize = 4;
        // A child's final protocol event may arrive after exit. Include ended
        // owners until their pane is removed, still under the same I/O budget.
        let mut ids: Vec<_> = self.ptys.keys().cloned().collect();
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

    fn codex_pane_source(&self, id: &str) -> Option<&Session> {
        self.all_sessions
            .iter()
            .chain(&self.extra_sessions)
            .find(|s| s.id == id && s.agent == Agent::Codex && !s.archived)
    }

    /// Compare transcript ownership, never labels or the candidate's ID-based pane binding.
    pub(super) fn same_codex_source_account(&self, pane: &str, source: &Session) -> bool {
        let Some(owner) = self.codex_pane_source(pane) else {
            return false;
        };
        let home = limits_home_for_app();
        let account = |s: &Session| {
            mindplayer_core::accounts::owner_of(&self.accounts, s.agent, &s.file, &home)
        };
        source.agent == Agent::Codex && account(owner).slot == account(source).slot
    }

    pub(super) fn native_rollout_file(&self, pane: &str, thread: &str) -> Option<&Path> {
        self.working_set
            .native_routes
            .get(pane)
            .filter(|route| route.thread == thread)
            .and_then(|route| route.source.as_ref())
            .map(|source| source.file.as_path())
    }

    fn native_source_is_running(&self, source: &Session) -> bool {
        self.working_set.native_routes.iter().any(|(pane, route)| {
            self.is_running(pane)
                && self.ptys[pane].routing_thread_id().as_deref() == Some(&route.thread)
                && route.source.as_ref().is_some_and(|s| {
                    s.agent == source.agent && s.id == source.id && s.file == source.file
                })
        })
    }

    /// Called only for the bounded hook batch. Catalog lookup and detail I/O
    /// run once per changed native target, away from input and status rendering.
    pub(super) fn poll_native_switch(&mut self, pane: &str) -> bool {
        let Some(pty) = self.ptys.get(pane).filter(|_| self.is_running(pane)) else {
            self.working_set.native_routes.remove(pane);
            return false;
        };
        let Some(thread) = pty.routing_thread_id().filter(|thread| thread != pane) else {
            self.working_set.native_routes.remove(pane);
            return false;
        };
        if pty.routing_clear_target(pane).is_some() {
            self.working_set.native_routes.remove(pane);
            return false;
        }
        let roots = self.scan_roots();
        let Some(root) = self
            .codex_pane_source(pane)
            .and_then(|s| codex_source_root(&roots, &s.file))
            .cloned()
        else {
            return false;
        };
        let now = Instant::now();
        let current = self
            .working_set
            .native_routes
            .get(pane)
            .is_some_and(|route| route.thread == thread && route.root == root);
        if !current {
            self.working_set.epoch = self.working_set.epoch.wrapping_add(1);
            self.working_set.native_routes.insert(
                pane.into(),
                NativeRoute {
                    thread: thread.clone(),
                    root: root.clone(),
                    source: None,
                    result: None,
                    retry_at: now,
                },
            );
        }
        let route = &self.working_set.native_routes[pane];
        if route.source.is_some() {
            return false;
        }
        if route.result.is_none() && now >= route.retry_at {
            // Copy only matching detail/page rows; the large catalog stays shared.
            let candidates: Vec<_> = self
                .all_sessions
                .iter()
                .chain(self.working_set.archive_overrides.values())
                .filter(|s| s.id == thread && s.agent == Agent::Codex)
                .cloned()
                .collect();
            let catalog = Arc::clone(&self.archive_catalog);
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let mut matches = candidates.iter().chain(catalog.iter()).filter(|s| {
                    s.id == thread
                        && s.agent == Agent::Codex
                        && !s.is_subagent
                        && codex_source_root(&roots, &s.file) == Some(&root)
                });
                let result = match matches.next() {
                    Some(source) if !matches.any(|s| s.file != source.file) => {
                        catalog::hydrate(source)
                            .map_err(|_| "native session source could not be hydrated".into())
                    }
                    _ => {
                        Err("native session source is missing or ambiguous in this account".into())
                    }
                };
                let _ = tx.send(result);
            });
            self.working_set
                .native_routes
                .get_mut(pane)
                .expect("native route was inserted")
                .result = Some(rx);
        }
        let route = self
            .working_set
            .native_routes
            .get_mut(pane)
            .expect("native route was inserted");
        let Some(result) = route.result.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("native session worker stopped".into()))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }) else {
            return false;
        };
        route.result = None;
        route.retry_at = now + Duration::from_secs(3);
        let mut session = match result {
            Ok(session) => session,
            Err(message) => {
                self.status = message;
                self.rescan_due.get_or_insert(now + Duration::from_secs(3));
                return true;
            }
        };
        if !self.can_resume_source(&session) {
            return true;
        }
        let previous = self.state.clone();
        self.state.record_session_use(&session, Utc::now());
        if self.save_state().is_err() {
            self.state = previous;
            self.status = "could not save native session Active membership; retrying".into();
            return true;
        }
        self.working_set.epoch = self.working_set.epoch.wrapping_add(1);
        self.state.apply(std::slice::from_mut(&mut session));
        session.archived = false;
        self.working_set
            .native_routes
            .get_mut(pane)
            .expect("native route was inserted")
            .source = Some(session.clone());
        if let Some(row) = self
            .all_sessions
            .iter_mut()
            .find(|s| s.agent == session.agent && s.id == session.id && s.file == session.file)
        {
            *row = session;
        } else {
            self.all_sessions.push(session);
        }
        if self.show_archived {
            self.request_archive_page();
        } else {
            self.rebuild_visible();
        }
        true
    }

    pub(super) fn catalog_scan(&self) -> Receiver<CatalogMessage> {
        let scope = self.scope.clone();
        let roots = self.scan_roots();
        let state = self.state.clone();
        let mut protected: HashSet<_> = self
            .ptys
            .keys()
            .filter(|id| self.is_running(id))
            .cloned()
            .collect();
        let mut routed: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for pane in &protected {
            let Some(root) = self
                .codex_pane_source(pane)
                .and_then(|s| codex_source_root(&roots, &s.file))
            else {
                continue;
            };
            if let Some(thread) = self.ptys[pane].routing_thread_id() {
                routed.entry(thread).or_default().push(root.clone());
            }
        }
        protected.extend(routed.keys().cloned());
        let cache = self.state_path.with_file_name("session-catalog.json");
        let epoch = self.working_set.epoch;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = catalog::scan(&scope, &roots, &state, &protected, &cache, Utc::now())
                .map(|mut snapshot| {
                    // Core protection is ID-based. Do not let it activate a
                    // duplicate transcript from a different provider home.
                    let mut active = Vec::with_capacity(snapshot.active.len());
                    for mut session in snapshot.active {
                        if routed.get(&session.id).is_some_and(|owners| {
                            !owners
                                .iter()
                                .any(|root| codex_source_root(&roots, &session.file) == Some(root))
                        }) {
                            session.archived = true;
                            snapshot.archive.push(session);
                        } else {
                            active.push(session);
                        }
                    }
                    snapshot.active = active;
                    snapshot
                })
                .map_err(|_| "session catalog could not be read or saved".to_string());
            let _ = tx.send((epoch, result));
        });
        rx
    }

    pub(super) fn apply_catalog(&mut self, message: CatalogMessage) -> bool {
        let (epoch, result) = message;
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
        let selected = self.selected_session().map(source_key);
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
            if self.state.resolve_pending(&candidates) {
                let _ = self.save_state();
            }
        }
        // A live pane may have appeared while discovery ran. Its detail/PTY
        // ownership takes precedence over a stale filesystem observation.
        let owners: HashMap<_, _> = self
            .all_sessions
            .iter()
            .filter(|s| !s.archived && (self.is_running(&s.id) || self.native_source_is_running(s)))
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
                active.push((*owner).clone());
            }
        }
        let active_sources: HashSet<_> = active.iter().map(source_key).collect();
        snapshot
            .archive
            .retain(|s| !active_sources.contains(&source_key(s)));
        snapshot.active = active;
        self.state.apply(&mut snapshot.active);
        for session in &mut snapshot.active {
            session.archived = false;
        }
        // Archive labels are stamped only on its bounded page in the worker.
        self.aggregate = Aggregate::of(&snapshot.active);
        self.all_sessions = snapshot.active;
        self.archive_matches = snapshot.archive.len();
        self.archive_catalog = Arc::new(snapshot.archive);
        self.working_set.archive_overrides.clear();
        self.working_set.next_scan = Some(Instant::now() + CATALOG_INTERVAL);
        self.merge_extras();
        self.rebuild_visible();
        if self.show_archived {
            self.request_archive_page();
            self.working_set.archive_selection = selected;
        } else if let Some(key) = selected {
            if let Some(row) = self.row_of_source(&key) {
                self.selected = row;
            }
        }
        self.reconcile_codex_clears();
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

    fn row_of_source(&self, key: &SourceKey) -> Option<usize> {
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
                    matches
                        .iter()
                        .position(|s| s.agent == key.0 && s.file == key.1 && s.id == key.2)
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
            result: rx,
        });
    }

    pub(super) fn note_session_use(&mut self, id: &str) {
        if mindplayer_core::session::is_synthetic_id(id) {
            return;
        }
        let thread = self.ptys.get(id).and_then(|pty| {
            pty.poll_routing_status();
            pty.routing_thread_id()
        });
        let source = if let Some(thread) = thread.as_deref().filter(|thread| *thread != id) {
            // A changed native target may not have been hydrated yet. Never
            // attribute its input to the previous route or launch-time source.
            self.working_set
                .native_routes
                .get(id)
                .filter(|route| route.thread == thread)
                .and_then(|route| route.source.as_ref())
        } else {
            self.all_sessions
                .iter()
                .chain(&self.extra_sessions)
                .find(|session| session.id == id && !session.archived)
        };
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
