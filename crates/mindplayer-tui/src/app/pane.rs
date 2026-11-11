use super::*;
use std::path::Path;
/// How often each Claude/Codex pane's tiny hook-state file is re-read.
const HOOK_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(125);

fn restore_tracking<T>(map: &mut HashMap<String, T>, id: &str, previous: Option<T>) {
    match previous {
        Some(value) => {
            map.insert(id.to_string(), value);
        }
        None => {
            map.remove(id);
        }
    }
}

fn point_in_bounds(bounds: Option<(u16, u16, u16, u16)>, col: u16, row: u16) -> bool {
    bounds.is_some_and(|(x, y, rows, cols)| {
        col >= x && col < x.saturating_add(cols) && row >= y && row < y.saturating_add(rows)
    })
}

impl App {
    pub fn toggle_observer(&mut self) {
        let Some(id) = self.focused_pane().map(str::to_owned) else {
            return;
        };
        let observer = self.observers.entry(id).or_default();
        observer.enabled = !observer.enabled;
        self.observe_turn = 0;
        self.observe_scroll = 0;
        self.observe_bounds = None;
        self.observe_prompt_bounds = None;
        self.observe_detail_bounds = None;
    }

    pub fn observer_open(&self) -> bool {
        self.focused_pane()
            .and_then(|id| self.observers.get(id))
            .is_some_and(|observer| observer.enabled)
    }

    pub fn move_observer_turn(&mut self, delta: isize) -> bool {
        let Some(count) = self
            .focused_pane()
            .and_then(|id| self.observers.get(id))
            .map(crate::observe::Observer::turn_count)
        else {
            return false;
        };
        if count == 0 {
            return false;
        }
        self.observe_turn = (self.observe_turn as isize + delta)
            .clamp(0, count.saturating_sub(1) as isize) as usize;
        self.observe_scroll = 0;
        true
    }

    /// Consume scrolling that happens over the observer rather than sending it
    /// to the focused agent PTY. The observer is deliberately read-only, so it
    /// must win even if a Codex full-screen UI requested mouse reporting.
    pub fn scroll_observer(&mut self, delta: isize) -> bool {
        let enabled = self
            .focused_pane()
            .and_then(|id| self.observers.get(id))
            .is_some_and(|observer| observer.enabled);
        if !enabled {
            return false;
        }
        self.observe_scroll = if delta.is_negative() {
            self.observe_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.observe_scroll
                .saturating_add(delta as usize)
                .min(10_000)
        };
        true
    }

    /// Whether a terminal cell falls inside the observer's latest frame.
    pub fn observer_contains(&self, col: u16, row: u16) -> bool {
        point_in_bounds(self.observe_bounds, col, row)
    }

    pub fn observer_prompt_contains(&self, col: u16, row: u16) -> bool {
        point_in_bounds(self.observe_prompt_bounds, col, row)
    }

    pub fn observer_detail_contains(&self, col: u16, row: u16) -> bool {
        point_in_bounds(self.observe_detail_bounds, col, row)
    }

    pub fn poll_observer(&mut self) -> bool {
        // Closed panes cannot accumulate observers indefinitely.
        self.observers.retain(|id, _| self.panes.contains(id));
        if self.focus != Focus::Terminal {
            return false;
        }
        let Some(id) = self.focused_pane().map(str::to_owned) else {
            return false;
        };
        let session = self
            .all_sessions
            .iter()
            .chain(self.extra_sessions.iter())
            .find(|s| s.id == id)
            .cloned();
        let Some(observer) = self.observers.get_mut(&id).filter(|o| o.enabled) else {
            return false;
        };
        match session {
            Some(session) => observer.poll(&session),
            None => {
                let message = "Waiting for provider transcript";
                let changed = observer.notice != message;
                observer.notice = message.into();
                changed
            }
        }
    }

    /// The session id currently shown in the right pane, if it has a PTY.
    pub fn active_pty(&self) -> Option<&PtySession> {
        self.active.as_ref().and_then(|id| self.ptys.get(id))
    }

    pub fn focused_pane(&self) -> Option<&str> {
        self.panes.get(self.focused).map(String::as_str)
    }

    pub(crate) fn sync_active(&mut self) {
        if self.panes.is_empty() {
            self.focused = 0;
            self.active = None;
            return;
        }
        if self.focused >= self.panes.len() {
            self.focused = self.panes.len().saturating_sub(1);
        }
        self.active = self.panes.get(self.focused).cloned();
    }

    pub fn focus_or_add_pane(&mut self, sid: &str) {
        if let Some(pos) = self.panes.iter().position(|id| id == sid) {
            self.focused = pos;
        } else if self.panes.len() < MAX_PANES {
            self.panes.push(sid.to_string());
            self.focused = self.panes.len() - 1;
        } else if self.panes.is_empty() {
            self.panes.push(sid.to_string());
            self.focused = 0;
        } else {
            let old = std::mem::replace(&mut self.panes[self.focused], sid.to_string());
            self.pane_sizes.remove(&old);
            self.pane_bounds.remove(&old);
        }
        self.regroup_panes();
        self.focus = Focus::Terminal;
        self.sync_active();
    }

    /// Sort `panes` so panes in the same category sit next to each other.
    ///
    /// The grid draws one band per category, so this ordering is what the eye
    /// sees — and `focused` indexes `panes`, so keeping the two in one order is
    /// what makes <kbd>Tab</kbd> walk the grid the way it looks rather than
    /// jumping between bands in launch order.
    ///
    /// Bands are ordered by the first pane opened into each, and uncategorized
    /// panes trail, so a band never jumps position as panes come and go. Order
    /// within a band stays as launched.
    pub(crate) fn regroup_panes(&mut self) {
        if self.panes.len() < 2 {
            return;
        }
        let focused_id = self.panes.get(self.focused).cloned();
        let ids = self.panes.clone();
        // Follows the thread root, so a handoff child lands in its parent's band.
        let keys: Vec<Option<String>> = ids.iter().map(|id| self.category_of_session(id)).collect();
        let mut order: Vec<Option<String>> = Vec::new();
        for k in keys.iter() {
            if k.is_some() && !order.contains(k) {
                order.push(k.clone());
            }
        }
        order.push(None);
        let mut idx: Vec<usize> = (0..ids.len()).collect();
        idx.sort_by_key(|&i| {
            let rank = order
                .iter()
                .position(|o| o == &keys[i])
                .unwrap_or(usize::MAX);
            (rank, i)
        });
        self.panes = idx.into_iter().map(|i| ids[i].clone()).collect();
        if let Some(fid) = focused_id {
            if let Some(p) = self.panes.iter().position(|id| id == &fid) {
                self.focused = p;
            }
        }
    }

    pub(crate) fn remove_pane(&mut self, sid: &str) {
        if self.is_running(sid) {
            // Ctrl-Q only hides a live PTY. Its login still owns this home
            // until reap_pty observes exit and resumes any held launch.
            self.account_auth.cancel_launch(sid);
        } else {
            self.account_auth.cancel(sid);
        }
        self.invalidate_manual_usage();
        if let Some(pos) = self.panes.iter().position(|id| id == sid) {
            self.panes.remove(pos);
            self.pane_sizes.remove(sid);
            self.pane_bounds.remove(sid);
            if self.focused >= self.panes.len() {
                self.focused = self.panes.len().saturating_sub(1);
            }
            if self.panes.is_empty() {
                self.zoomed = false;
            }
            self.sync_active();
        } else if self.active.as_deref() == Some(sid) {
            self.active = self.focused_pane().map(str::to_string);
        }
    }

    pub fn cycle_focus(&mut self) {
        if self.panes.len() < 2 {
            return;
        }
        self.selection = None;
        self.focused = (self.focused + 1) % self.panes.len();
        self.sync_active();
        self.log_pane_focus_cycle();
        self.status = format!("focused pane {}/{}", self.focused + 1, self.panes.len());
    }

    /// Cycle pane focus in reverse (Shift+Tab / BackTab).
    pub fn cycle_focus_back(&mut self) {
        if self.panes.len() < 2 {
            return;
        }
        self.selection = None;
        self.focused = (self.focused + self.panes.len() - 1) % self.panes.len();
        self.sync_active();
        self.log_pane_focus_cycle();
        self.status = format!("focused pane {}/{}", self.focused + 1, self.panes.len());
    }

    fn log_pane_focus_cycle(&self) {
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::PaneFocusCycle {
                focused: self.focused + 1,
                count: self.panes.len(),
            },
        );
    }

    pub fn effective_layout(&self) -> PaneLayout {
        if self.panes.len() <= 1 {
            PaneLayout::Single
        } else {
            self.layout
        }
    }

    /// Toggle full-screen zoom of the focused pane, so a cramped multi-pane
    /// split can be read at full size. Tab / ctrl-w still cycle which pane is
    /// focused (and therefore shown) while zoomed; toggling again returns to
    /// the normal split showing every pane.
    pub fn toggle_zoom(&mut self) {
        if self.panes.is_empty() {
            return;
        }
        self.zoomed = !self.zoomed;
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::ZoomToggle { on: self.zoomed },
        );
        self.status = if self.zoomed {
            "zoomed — tab/ctrl-w to browse panes, ctrl-z for the split view".to_string()
        } else {
            "back to the split view".to_string()
        };
    }

    pub fn close_focused_pane(&mut self) {
        let Some(sid) = self.focused_pane().map(str::to_string) else {
            self.focus = Focus::List;
            return;
        };
        self.remove_pane(&sid);
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::PaneClose {
                id: sid.clone(),
                remaining: self.panes.len(),
            },
        );
        if self.panes.is_empty() {
            self.focus = Focus::List;
            self.status = "closed live pane".to_string();
        } else {
            self.status = format!("closed pane {}", short(&sid));
        }
    }

    /// True if a session is alive and is the one being displayed.
    pub fn has_live_pty(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|id| self.ptys.contains_key(id) && !self.ended.contains(id))
    }

    pub fn live_pty_count(&self) -> usize {
        self.ptys
            .keys()
            .filter(|id| !self.ended.contains(*id))
            .count()
    }

    /// Whether session `id` is running (has a PTY that hasn't ended).
    pub fn is_running(&self, id: &str) -> bool {
        self.ptys.contains_key(id) && !self.ended.contains(id)
    }

    pub(super) fn refresh_native_owners(&self) {
        // List actions may arrive between interval-gated hook polls. Refresh
        // only the private ownership channels before choosing a child to act on.
        for pty in self.ptys.values() {
            pty.poll_routing_status();
        }
    }

    /// Resolve a source row to its PTY without moving the child's stable pane key.
    pub(super) fn pty_owner_for_source(&self, source: &Session) -> Option<String> {
        let mut owners = self.ptys.iter().filter_map(|(id, pty)| {
            if pty
                .routing_thread_id()
                .is_some_and(|thread| thread != source.id)
            {
                return None;
            }
            let matches = if id == &source.id {
                self.is_unique_runtime_source(source)
            } else {
                source.agent == Agent::Codex
                    && pty.routing_thread_id().as_deref() == Some(source.id.as_str())
                    && self.native_rollout_file(id, &source.id) == Some(source.file.as_path())
            };
            matches.then_some(id)
        });
        let owner = owners.next()?;
        owners.next().is_none().then(|| owner.clone())
    }

    fn is_unique_runtime_source(&self, source: &Session) -> bool {
        let mut candidates = self
            .all_sessions
            .iter()
            .chain(&self.extra_sessions)
            .filter(|row| row.id == source.id && !row.archived);
        candidates.next().is_some_and(|owner| {
            owner.agent == source.agent
                && owner.file == source.file
                && !candidates.any(|row| row.agent != owner.agent || row.file != owner.file)
        })
    }

    /// An unresolved or moved native thread must not be overwritten or closed
    /// through an old ID while its source lookup is still pending.
    pub(super) fn source_runtime_conflicts(&self, source: &Session) -> bool {
        self.ptys.iter().any(|(id, pty)| {
            !self.ended.contains(id)
                && (id == &source.id
                    || pty.routing_thread_id().as_deref() == Some(source.id.as_str()))
        }) && self.pty_owner_for_source(source).is_none()
    }

    /// Refresh per-session output-activity tracking from each PTY's read
    /// counter. Returns true if any session's activity changed (needs redraw to
    /// flip its status badge). Also drops tracking for closed sessions.
    pub fn poll_activity(&mut self) -> bool {
        let mut changed = false;
        let now = Instant::now();
        for (id, pty) in self.ptys.iter() {
            if self.ended.contains(id) {
                continue;
            }
            let seq = pty.output_seq();
            if self.out_seq.get(id) != Some(&seq) {
                self.out_seq.insert(id.clone(), seq);
                if should_stamp_activity(
                    self.session_status(id) == SessionStatus::Working,
                    pty.looks_busy(),
                ) {
                    self.out_at.insert(id.clone(), now);
                }
                changed |= self.focus == Focus::List || self.panes.contains(id);
            }
        }
        self.out_seq
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.out_at
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.turn_submitted
            .retain(|id| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.thread_sync_at.retain(|id, _| {
            (self.ptys.contains_key(id) && !self.ended.contains(id))
                || self
                    .thread_sync_rx
                    .as_ref()
                    .is_some_and(|pending| pending.target.id == *id)
                || self.pending_initial_inputs.values().any(|pending| {
                    pending
                        .sync
                        .as_ref()
                        .is_some_and(|sync| sync.thread && sync.source.id == *id)
                })
        });
        self.codex_turns
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.hook_floor
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.hook_source_ids
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.submitted_at
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.synthetic_turn_cutoffs
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        changed
    }

    /// True while any session is within its "working" window — the loop uses
    /// this to keep redrawing so a badge can decay from working → idle even
    /// with no new events.
    pub fn any_recent_activity(&self) -> bool {
        self.out_at
            .iter()
            .any(|(id, t)| !self.ended.contains(id) && t.elapsed() < WORKING_HOLD)
    }

    /// Status of session `id` for the list badge. A Claude/Codex pane with a
    /// live hook reading (see `agent_hooks`) uses that directly instead of
    /// guessing from screen text — it's the agent itself reporting its state,
    /// not a heuristic. Kiro, and any Claude/Codex pane with no hook data yet
    /// (hooks not installed, or no event has fired for this pane), falls back
    /// to the existing screen-text classifier unchanged.
    pub fn session_status(&self, id: &str) -> SessionStatus {
        self.session_status_with_source(id, None)
    }

    /// Rendering already has the source row; do not look it up for every badge.
    pub fn session_status_for(&self, session: &Session) -> SessionStatus {
        if self.ended.contains(&session.id)
            && !self.ptys.contains_key(&session.id)
            && self.is_unique_runtime_source(session)
        {
            return SessionStatus::Ended;
        }
        self.pty_owner_for_source(session)
            .map(|id| self.session_status_with_source(&id, Some(session)))
            .unwrap_or(SessionStatus::Inactive)
    }

    fn session_status_with_source(&self, id: &str, source: Option<&Session>) -> SessionStatus {
        if self.ended.contains(id) {
            return SessionStatus::Ended;
        }
        let Some(pty) = self.ptys.get(id) else {
            return SessionStatus::Inactive;
        };
        let native = self
            .codex_turns
            .get(id)
            .filter(|reader| {
                let owned = pty.routing_thread_id();
                owned.is_none()
                    || source
                        .filter(|s| {
                            Some(s.id.as_str()) == owned.as_deref() && s.agent == Agent::Codex
                        })
                        .map(|s| s.file.as_path())
                        .or_else(|| self.codex_rollout_file(id))
                        == Some(reader.file())
            })
            .and_then(|r| r.status());
        if pty.looks_blocked()
            || self
                .hook_status
                .get(id)
                .is_some_and(|r| r.status == SessionStatus::Blocked)
        {
            return SessionStatus::Blocked;
        }
        if let Some(status) = native {
            return status;
        }
        // The pane-owned router already correlates actual turn events, even
        // when rollout discovery/tailing has not caught up. Pending/ACK stages
        // stay idle until actual start; only a terminal turn event earns green.
        if let Some(status) = pty.routing_status() {
            use crate::local_router::RoutingStage;
            match status.stage {
                RoutingStage::Completed if status.turn_id.is_some() => {
                    return SessionStatus::Completed
                }
                RoutingStage::Running => return SessionStatus::Working,
                RoutingStage::Judging
                | RoutingStage::Requested
                | RoutingStage::Accepted
                | RoutingStage::Failed
                | RoutingStage::Interrupted => return SessionStatus::Idle,
                _ => {}
            }
        }
        if let Some(reading) = self.hook_status.get(id) {
            let codex = source.map(|s| s.agent == Agent::Codex).unwrap_or_else(|| {
                self.all_sessions
                    .iter()
                    .any(|s| s.id == id && s.agent == Agent::Codex)
            });
            // Stop alone cannot distinguish successful Codex completion from
            // interruption. Codex green requires its correlated rollout event.
            return if codex && reading.status == SessionStatus::Completed {
                SessionStatus::Idle
            } else {
                reading.status
            };
        }
        classify_live_session_status(
            pty.looks_blocked(),
            pty.looks_idle(),
            pty.looks_busy(),
            self.out_at.get(id).copied(),
            Instant::now(),
        )
    }

    /// Native /resume and /fork can change the thread without replacing its
    /// PTY/pane ID. Only the pane-owned protocol can select its current source.
    fn codex_rollout_file(&self, id: &str) -> Option<&Path> {
        let owned = self.ptys.get(id).and_then(|pty| pty.routing_thread_id());
        let thread = owned.as_deref().unwrap_or(id);
        if let Some(file) = self.native_rollout_file(id, thread) {
            return Some(file);
        }
        self.all_sessions
            .iter()
            .find(|session| {
                session.id == thread
                    && session.agent == Agent::Codex
                    && !session.archived
                    && !session.file.as_os_str().is_empty()
                    && (thread == id || self.same_codex_source_account(id, session))
            })
            .map(|session| session.file.as_path())
    }

    /// Refresh `hook_status` for every live Claude/Codex pane from its
    /// hook-state file. Interval-gated to keep filesystem reads bounded;
    /// returns true if any pane's cached reading changed (needs redraw since
    /// it can flip the list badge/urgency sort without any new PTY output).
    pub fn poll_hook_status(&mut self) -> bool {
        match self.hook_status_due {
            Some(at) if Instant::now() < at => return false,
            _ => {}
        }
        self.hook_status_due = Some(Instant::now() + HOOK_STATUS_POLL_INTERVAL);

        let live_ids = self.hook_poll_batch();
        let mut changed = false;
        for id in &live_ids {
            if let Some(pty) = self.ptys.get(id) {
                changed |= pty.poll_routing_status();
            }
        }
        changed |= self.reconcile_codex_clears();
        for id in &live_ids {
            changed |= self.poll_native_switch(id);
            if self.ended.contains(id) {
                continue;
            }
            if let Some(file) = self.codex_rollout_file(id).map(Path::to_path_buf) {
                if self.codex_turns.get(id).is_none_or(|r| r.file() != file) {
                    self.codex_turns
                        .insert(id.clone(), crate::turn_status::TurnStatusReader::new(file));
                }
            } else if self
                .ptys
                .get(id)
                .and_then(|pty| pty.routing_thread_id())
                .is_some()
            {
                self.codex_turns.remove(id);
            }
            if let Some(reader) = self.codex_turns.get_mut(id) {
                changed |= reader.poll();
            }
            let supports = self
                .all_sessions
                .iter()
                .find(|s| &s.id == id)
                .is_some_and(|s| crate::agent_hooks::supports_hooks(s.agent));
            if !supports {
                continue;
            }
            let source = self
                .hook_source_ids
                .get(id)
                .map(String::as_str)
                .unwrap_or(id);
            let reading = crate::agent_hooks::read_hook_status(source)
                .filter(|r| self.hook_floor.get(id) != Some(r));
            match (reading, self.hook_status.get(id).copied()) {
                (Some(r), Some(prev)) if r == prev => {}
                (None, None) => {}
                _ => changed = true,
            }
            match reading {
                Some(r) => {
                    self.hook_status.insert(id.clone(), r);
                }
                None => {
                    self.hook_status.remove(id);
                }
            }
        }
        // Drop cached readings for panes that no longer exist (closed/ended).
        self.hook_status.retain(|id, _| self.ptys.contains_key(id));
        changed
    }

    /// Capture provenance before dispatch, so a fast agent cannot write a new
    /// completion before the old event has been invalidated. No file is deleted.
    fn reset_turn_status(
        &mut self,
        id: &str,
        force_reset: bool,
    ) -> Option<crate::local_router::RoutingStatus> {
        // Enter can approve a tool in the current turn, not start a new turn.
        // Keep its correlation ID until a native start event supersedes it.
        let preserve_active = !force_reset
            && matches!(
                self.session_status(id),
                SessionStatus::Working | SessionStatus::Blocked
            );
        if !preserve_active && mindplayer_core::session::is_synthetic_id(id) {
            self.synthetic_turn_cutoffs
                .insert(id.to_string(), Utc::now());
        }
        let source = self
            .hook_source_ids
            .get(id)
            .map(String::as_str)
            .unwrap_or(id);
        if let Some(reading) = crate::agent_hooks::read_hook_status(source) {
            self.hook_floor.insert(id.to_string(), reading);
        }
        self.hook_status.remove(id);
        if let Some(reader) = self.codex_turns.get_mut(id).filter(|_| !preserve_active) {
            reader.reset_at_end();
        }
        if !preserve_active {
            self.out_at.remove(id);
        }
        self.submitted_at.remove(id);
        self.hook_status_due = None;
        (!preserve_active)
            .then(|| self.ptys.get(id).and_then(|pty| pty.routing_status()))
            .flatten()
    }

    /// Reset against the pre-delivery file boundary, but roll back if the
    /// writer did not accept the input. Automatic new prompts force a reset;
    /// ordinary Enter may instead approve/steer the currently running turn.
    pub(super) fn dispatch_turn_input(
        &mut self,
        id: &str,
        force_reset: bool,
        send: impl FnOnce(&mut PtySession) -> bool,
    ) -> bool {
        if !self.ptys.contains_key(id) {
            return false;
        }
        let previous = (
            self.hook_status.get(id).copied(),
            self.hook_floor.get(id).copied(),
            self.codex_turns.get(id).cloned(),
            self.submitted_at.get(id).copied(),
            self.synthetic_turn_cutoffs.get(id).copied(),
            self.hook_status_due,
            self.out_at.get(id).copied(),
        );
        let routing_to_retire = self.reset_turn_status(id, force_reset);
        if self.ptys.get_mut(id).is_some_and(send) {
            if let Some(status) = routing_to_retire {
                self.ptys.get_mut(id).unwrap().retire_routing_status(status);
            }
            return true;
        }
        restore_tracking(&mut self.hook_status, id, previous.0);
        restore_tracking(&mut self.hook_floor, id, previous.1);
        restore_tracking(&mut self.codex_turns, id, previous.2);
        restore_tracking(&mut self.submitted_at, id, previous.3);
        restore_tracking(&mut self.synthetic_turn_cutoffs, id, previous.4);
        self.hook_status_due = previous.5;
        restore_tracking(&mut self.out_at, id, previous.6);
        false
    }

    /// Log a `SessionStatusChange` for every live session whose computed
    /// status changed since the last poll — idle/working/blocked flips, and the
    /// transition to `ended` when a child exits. Only actual changes are logged
    /// (never every poll), and the first sighting of a session is recorded
    /// silently since its birth is already captured by `SessionOpen`. History
    /// rows (no PTY) are always `Inactive` and uninteresting, so they're
    /// skipped. Returns true if any transition was logged.
    ///
    /// This is the audit counterpart to the badge/sort classification in
    /// [`Self::session_status`]: it turns the app's own live read of each
    /// session into a durable trail, so an incident like "it says it's been
    /// working for ages but nothing's happening" can be reconstructed from the
    /// log after the fact.
    pub fn poll_status_transitions(&mut self) -> bool {
        // `ended` ids stay in `ptys` until the pane is closed, so iterating the
        // pty map alone covers both live and just-ended sessions.
        let ids: Vec<String> = self.ptys.keys().cloned().collect();
        let mut logged = false;
        for id in ids {
            let status = self.session_status(&id);
            let prev = self.last_status.get(&id).copied();
            if let Some((from, to)) = status_transition(prev, status) {
                mindplayer_core::log_event_to(
                    &self.audit_path,
                    mindplayer_core::AuditEvent::SessionStatusChange {
                        id: id.clone(),
                        from: status_label(from).to_string(),
                        to: status_label(to).to_string(),
                    },
                );
                logged = true;
            }
            // Record the current status either way — a first sighting seeds it
            // silently (`SessionOpen` already marks the birth), a real change
            // updates it after logging above.
            self.last_status.insert(id, status);
        }
        // Forget sessions that are entirely gone (closed/removed) so a later
        // reuse of the same id starts fresh instead of diffing a stale status.
        self.last_status.retain(|id, _| self.ptys.contains_key(id));
        logged
    }

    /// Bubble only *blocked* panes to the front of the grid — a rare, stable
    /// event that genuinely needs you now. Working/idle panes are drawn with
    /// their own glow/color (see `ui::render_pane`) but never reordered:
    /// "working" turns on the instant any pty emits a byte, so with several
    /// panes streaming at once it is far too noisy a signal to reposition on.
    /// A no-op ordering returns `false` without touching `self.panes` or
    /// forcing a redraw.
    pub fn reorder_panes_by_status(&mut self) -> bool {
        if self.panes.len() < 2 {
            return false;
        }
        let focused_id = self.focused_pane().map(str::to_string);
        let reordered = bubble_urgent_within_bands(
            &self.panes,
            |id| self.category_of_session(id),
            |id| self.session_status(id) == SessionStatus::Blocked,
        );
        if reordered == self.panes {
            return false;
        }
        self.panes = reordered;
        if let Some(id) = focused_id {
            if let Some(pos) = self.panes.iter().position(|p| p == &id) {
                self.focused = pos;
            }
        }
        self.sync_active();
        true
    }

    /// Request a resume of the selected session in the right pane. If it is
    /// already running, just switch to it (keeping every other session alive).
    /// Forget a pane's captured failure, so a fresh attempt starts clean.
    pub(crate) fn clear_pane_error(&mut self, sid: &str) {
        self.pane_error.remove(sid);
    }

    pub fn request_resume(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        self.refresh_native_owners();
        if !self.can_resume_source(&session) {
            return;
        }
        let owner = self
            .pty_owner_for_source(&session)
            .filter(|id| self.is_running(id));
        if self.source_runtime_conflicts(&session) {
            self.status =
                "session action refused: live pane has moved or its source is unresolved".into();
            return;
        }
        if session.archived && owner.is_none() {
            self.queue_archive_restore(session);
            return;
        }
        self.note_session_use(&session.id);
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::SessionResume {
                id: session.id.clone(),
            },
        );
        self.focus = Focus::Terminal;
        if let Some(owner) = owner {
            // Already live in the background — just bring it to the
            // foreground now; any peer-lane sync reads happen off the main
            // thread (see `spawn_thread_sync_for`/`poll_thread_sync`) so
            // reopening a session with several handoff peers never freezes
            // the UI while their transcripts are read.
            self.spawn_thread_sync_for(&session);
            // Category peers need their own pass: thread-sync only ever looks at
            // handoff lineage, so a session dropped into a category with `n` has no
            // peers there at all.
            self.spawn_category_sync_for(&session, false);
            self.focus_or_add_pane(&owner);
            return;
        }
        // A row MindPlayer invented has no real id to `resume`; just show its
        // (possibly ended) pane if it still exists, otherwise stay on the list.
        if mindplayer_core::session::is_synthetic_id(&session.id) {
            if self.ptys.contains_key(&session.id) {
                self.focus_or_add_pane(&session.id);
            } else {
                self.focus = Focus::List;
            }
            return;
        }
        // Same off-main-thread treatment for a fresh spawn: kick off the
        // background sync read (if one is needed) and spawn immediately
        // without it. `poll_thread_sync` queues the result as a deferred
        // initial input once it's ready, same as a handoff's own initial
        // prompt already does.
        self.spawn_thread_sync_for(&session);
        // Category peers need their own pass: thread-sync only ever looks at
        // handoff lineage, so a session dropped into a category with `n` has no
        // peers there at all.
        self.spawn_category_sync_for(&session, false);
        self.pending = Some(PendingSpawn {
            command: resume(&session, &self.account_of_session(&session)),
            session_id: session.id.clone(),
            initial_input: None,
            focus_after_spawn: true,
        });
        self.focus_or_add_pane(&session.id);
        self.status = format!("resuming {} {}", session.agent.as_str(), short(&session.id));
    }

    /// Toggle the multi-select mark on the currently-selected session, then
    /// advance the cursor so several sessions can be marked in quick succession.
    pub fn toggle_mark(&mut self) {
        let Some(id) = self.selected_session().map(|s| s.id.clone()) else {
            return;
        };
        let now_marked = if self.marked.remove(&id) {
            false
        } else {
            self.marked.insert(id.clone());
            true
        };
        let marked = self.marked.len();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::MarkToggle {
                id,
                marked: now_marked,
                total: marked,
            },
        );
        self.status = if marked == 0 {
            "selection cleared".to_string()
        } else {
            format!("{marked} marked · enter to launch all")
        };
    }

    /// Enter or leave multi-select mode (the `v` shortcut). Toggling always
    /// clears pending marks so each multi-launch starts clean. Only in this mode
    /// does Space mark sessions; the default Enter stays single-session.
    pub fn toggle_multi_select(&mut self) {
        self.multi_select = !self.multi_select;
        self.marked.clear();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::MultiSelect {
                on: self.multi_select,
            },
        );
        self.status = if self.multi_select {
            "multi-select: space marks · enter launches all · esc cancels".to_string()
        } else {
            "multi-select off".to_string()
        };
    }

    /// Leave multi-select mode without launching, dropping any marks.
    pub fn cancel_multi_select(&mut self) {
        if !self.multi_select {
            return;
        }
        self.multi_select = false;
        self.marked.clear();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::MultiSelect { on: false },
        );
        self.status = "multi-select cancelled".to_string();
    }

    /// Launch every marked session as a live pane in one go. Falls back to the
    /// single-session resume when nothing is marked. Marked ids are taken in
    /// visible order (capped at MAX_PANES) so the pane order is predictable.
    /// Clears the marks and leaves multi-select mode afterward.
    pub fn launch_marked(&mut self) {
        self.refresh_native_owners();
        self.multi_select = false;
        if self.marked.is_empty() {
            self.request_resume();
            return;
        }
        // The whole point of marking several sessions is to see them side by
        // side — a leftover zoom (single pane fullscreen) from earlier would
        // otherwise silently hide every pane but the focused one, making a
        // multi-launch look like it only opened one session.
        //
        // Capture that leftover-zoom state *before* clearing it: logging it on
        // the launch event is what lets a reader later see "a bulk launch fired
        // while zoom was stuck on" — the shape of the "only one session opened"
        // bug — straight off the log, without needing the source.
        let zoom_was_on = self.zoomed;
        self.zoomed = false;
        let sessions: Vec<Session> = self
            .visible_sessions()
            .filter(|s| self.marked.contains(&s.id))
            .take(MAX_PANES)
            .cloned()
            .collect();
        let ids: Vec<_> = sessions.iter().map(|s| s.id.clone()).collect();
        let total = sessions.len();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::LaunchMarked {
                ids: ids.clone(),
                count: total,
                zoom_was_on,
            },
        );
        self.focus = Focus::Terminal;
        let mut rejected = 0;
        for session in sessions {
            if !self.can_resume_source(&session) {
                rejected += 1;
                continue;
            }
            let owner = self
                .pty_owner_for_source(&session)
                .filter(|id| self.is_running(id));
            if self.source_runtime_conflicts(&session) {
                rejected += 1;
                continue;
            }
            if session.archived && owner.is_none() {
                self.queue_archive_restore(session);
                continue;
            }
            self.note_session_use(&session.id);
            if let Some(owner) = owner {
                self.focus_or_add_pane(&owner);
                continue;
            }
            // A row MindPlayer invented has no real id to resume; only show
            // an already-spawned pane if one survives.
            if mindplayer_core::session::is_synthetic_id(&session.id) {
                if self.ptys.contains_key(&session.id) {
                    self.focus_or_add_pane(&session.id);
                }
                continue;
            }
            self.enqueue_spawn(PendingSpawn {
                command: resume(&session, &self.account_of_session(&session)),
                session_id: session.id.clone(),
                initial_input: None,
                focus_after_spawn: true,
            });
            self.focus_or_add_pane(&session.id);
        }
        self.marked.clear();
        self.status = if rejected == 0 {
            format!("launched {total} sessions into live panes")
        } else {
            if rejected == total {
                self.focus = Focus::List;
            }
            format!(
                "{} session requests; {rejected} source conflicts refused",
                total - rejected
            )
        };
    }

    /// Keep each displayed PTY sized to its pane.
    pub fn sync_pty_size(&mut self) {
        let targets: Vec<(String, (u16, u16))> = self
            .panes
            .iter()
            .filter_map(|id| {
                self.pane_sizes
                    .get(id)
                    .copied()
                    .map(|size| (id.clone(), size))
            })
            .collect();
        if targets.is_empty() {
            if let Some(id) = self.active.clone() {
                let (r, c) = (self.pty_rows, self.pty_cols);
                if let Some(pty) = self.ptys.get_mut(&id) {
                    pty.resize(r, c);
                }
            }
            return;
        }
        for (id, (rows, cols)) in targets {
            if let Some(pty) = self.ptys.get_mut(&id) {
                pty.resize(rows, cols);
            }
        }
    }

    pub fn detach_terminal(&mut self) {
        self.selection = None;
        // Keep list actions anchored to the pane the user just left. Before
        // this synchronization, `e` could silently target an old row or a
        // category header after Ctrl-X, which looked like label editing was
        // broken even though the modal itself worked.
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if let Some(row) = self.row_of_session(&id) {
                self.selected = row;
            }
        }
        self.focus = Focus::List;
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::FocusChange {
                focus: focus_label(self.focus).to_string(),
            },
        );
    }

    /// Re-enter the live view left behind by `detach_terminal` (ctrl-x toggle).
    /// The pane set survives detaching, so this just brings focus back to it —
    /// works for a single pane or a multi-pane split alike. To show a *different*
    /// set, pick sessions in multi-select (`v` + space) and launch them instead.
    pub fn resume_live_view(&mut self) {
        if self.panes.is_empty() {
            self.status = "no live session open — enter to start one".to_string();
            return;
        }
        self.focus = Focus::Terminal;
        self.sync_active();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::FocusChange {
                focus: focus_label(self.focus).to_string(),
            },
        );
        self.status = if self.panes.len() > 1 {
            format!("back to {} live panes", self.panes.len())
        } else {
            "back to live session".to_string()
        };
    }

    // --- pane drag-to-copy selection ---------------------------------------

    /// Detect children that have exited across ALL sessions. A finished session
    /// keeps its final frame (so output/errors stay readable); if it was the
    /// displayed one, focus returns to the list. Returns true if anything
    /// changed (needs redraw — e.g. the live ● dot).
    pub fn reap_pty(&mut self) -> bool {
        let mut newly_dead = Vec::new();
        for (id, pty) in self.ptys.iter_mut() {
            if !self.ended.contains(id) && !pty.is_alive() {
                // The leader just exited; clean up its group (MCP / language
                // servers) now, while the pgid is still alive, instead of
                // orphaning them.
                pty.signal_group();
                newly_dead.push(id.clone());
            }
        }
        if newly_dead.is_empty() {
            return false;
        }
        for id in newly_dead {
            if self.active.as_deref() == Some(id.as_str()) {
                // Keep showing the ended session's final frame instead of
                // yanking the user back to the list without their input — they
                // leave with ctrl-x (or ctrl-q to close the pane) when ready.
                self.status =
                    "session ended — ctrl-x for the list · ctrl-q closes the pane".to_string();
            }
            // A child that exited without ever drawing anything leaves an
            // empty pane, which says nothing about why. Its stderr is
            // redirected to a file (sharing the PTY makes codex abort), so the
            // reason is on disk and nowhere the user can see. Lift it out.
            if self.ptys.get(&id).is_some_and(|p| p.output_seq() == 0) {
                if let Some(text) = read_stderr_tail(&id) {
                    self.pane_error.insert(id.clone(), text);
                }
            }
            self.ended.insert(id.clone());
            let succeeded = self.ptys.get(&id).is_some_and(|p| p.exited_successfully());
            self.account_login_finished(&id, succeeded);
        }
        true
    }

    /// True (resetting) if the displayed PTY produced new output.
    pub fn pty_dirty(&self) -> bool {
        if self.panes.is_empty() {
            return self.active_pty().is_some_and(|p| p.take_dirty());
        }
        let mut dirty = false;
        for pty in self.panes.iter().filter_map(|id| self.ptys.get(id)) {
            // take_dirty also polls ownership; never starve a later pane just
            // because an earlier pane continuously produces output.
            dirty |= pty.take_dirty();
        }
        dirty
    }

    /// Forward encoded keystrokes to the focused pane's PTY.
    pub fn send_to_pty(&mut self, bytes: &[u8]) {
        // Typing dismisses any drag-copy highlight, like a normal terminal.
        self.selection = None;
        if self.hold_for_pending_initial_input(bytes) {
            return;
        }
        if let Some(id) = self.focused_pane().map(str::to_string) {
            let chunks = self
                .ptys
                .get_mut(&id)
                .map(|pty| pty.filtered_key_chunks(bytes))
                .unwrap_or_default();
            for chunk in chunks {
                if !self.deliver_key_chunk(&id, &chunk) {
                    break;
                }
            }
        }
    }

    /// Only bytes released by the terminal-reply guard can request submission or interrupt
    /// a turn. Buffered Escape and consumed CPR must leave every status source
    /// intact; a failed enqueue uses the same tracking rollback as other input.
    fn deliver_key_chunk(&mut self, id: &str, bytes: &[u8]) -> bool {
        let interrupt = bytes == b"\x1b" || bytes.contains(&0x03);
        let delivered = if input_requests_submit(bytes) || interrupt {
            self.dispatch_turn_input(id, interrupt, |p| p.send(bytes))
        } else {
            self.ptys.get_mut(id).is_some_and(|p| p.send(bytes))
        };
        if delivered && input_requests_submit(bytes) {
            self.note_session_use(id);
            self.submitted_at.insert(id.into(), Instant::now());
            self.turn_submitted.insert(id.into());
        }
        delivered
    }

    /// Release a standalone Escape held briefly to distinguish it from a
    /// fragmented outer-terminal CPR. Each pane owns its own guard, so changing
    /// focus while Escape is pending can never deliver it to another agent.
    pub fn flush_terminal_reply_guards(&mut self) {
        let due: Vec<_> = self
            .ptys
            .iter_mut()
            .filter_map(|(id, pty)| pty.pending_key_due().map(|bytes| (id.clone(), bytes)))
            .collect();
        for (id, bytes) in due {
            self.deliver_key_chunk(&id, &bytes);
        }
    }

    /// Forward pasted text to the displayed PTY (only when a live session has
    /// focus). Returns true if it was delivered (caller redraws). Pastes go
    /// nowhere useful from the list, so they're ignored there. A drag-and-
    /// dropped file path arrives here, and only counts as an attachment if the
    /// child sees it as a paste while it is still the current input — so this
    /// never defers it the way [`Self::send_to_pty`] defers typing.
    pub fn paste_to_pty(&mut self, text: &str) -> bool {
        if self.focus != Focus::Terminal {
            return false;
        }
        // Never held for a pending initial input, unlike typing: a dropped path only reaches the child as a paste, now.
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if self.pending_initial_inputs.contains_key(&id)
                && self.ptys.get(&id).and_then(|pty| pty.omp_model_selected()) == Some(true)
            {
                self.status = "waiting for target prompt to submit initial context".to_string();
                return false;
            }
            // A pasted newline belongs to the native editor. Native lifecycle
            // events, not successful enqueue, determine whether it starts work.
            return self.ptys.get_mut(&id).is_some_and(|p| p.paste(text));
        }
        false
    }
}

/// Stable-partitions `ids` into "urgent first, everyone else after" within each
/// category's run of panes, keeping each group's original relative order.
///
/// The grid draws one band per category and reads the bands off consecutive runs
/// in `panes`, so pulling an urgent pane to the very front would split its
/// category and put the wrong header over the wrong panes. Bands keep their
/// order — first appearance wins, and uncategorized panes group too; urgency only
/// decides position inside one band.
///
/// Pulled out of `App` so the sort can be tested without a real `PtySession`
/// (a pane with no pty never classifies as Blocked).
fn bubble_urgent_within_bands(
    ids: &[String],
    mut band_of: impl FnMut(&str) -> Option<String>,
    mut is_urgent: impl FnMut(&str) -> bool,
) -> Vec<String> {
    let mut ranked: Vec<(usize, u8, usize, &String)> = Vec::with_capacity(ids.len());
    let mut bands: Vec<Option<String>> = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let key = band_of(id);
        let band = match bands.iter().position(|b| b == &key) {
            Some(p) => p,
            None => {
                bands.push(key);
                bands.len() - 1
            }
        };
        ranked.push((band, u8::from(!is_urgent(id)), i, id));
    }
    ranked.sort_by_key(|(band, rank, i, _)| (*band, *rank, *i));
    ranked.into_iter().map(|(_, _, _, id)| id.clone()).collect()
}

/// The last few lines a dead child wrote to stderr, if it wrote any.
///
/// Capped at both ends: only the tail of the file is read, and only the last
/// few lines of that are kept, because a crash loop can append the same message
/// hundreds of times and the pane has room for one.
fn read_stderr_tail(session_id: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL_BYTES: u64 = 16 * 1024;
    const MAX_LINES: usize = 6;

    let path = crate::pty::stderr_log_path(session_id);
    let mut f = std::fs::File::open(&path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut buf = Vec::new();
    f.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    // A child that fails the same way every time writes the same line every
    // time; showing it six times would crowd out nothing but itself.
    lines.dedup();
    let tail: Vec<&str> = lines.iter().rev().take(MAX_LINES).rev().copied().collect();
    (!tail.is_empty()).then(|| tail.join("\n"))
}

#[cfg(test)]
mod stderr_tail_tests {
    use super::read_stderr_tail;

    fn write_log(id: &str, body: &str) {
        let path = crate::pty::stderr_log_path(id);
        if let Some(dir) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&path, body).unwrap();
    }

    /// The failure that started this: codex refuses to resume a session another
    /// process already holds, writes one line to stderr, and exits. That line
    /// is the whole explanation, and it was going only to a file.
    #[test]
    fn the_reason_a_child_died_is_recovered_from_its_log() {
        let id = format!("tail-basic-{}", std::process::id());
        write_log(
            &id,
            "Error: thread 019f already has an active writer (code -32600)\n",
        );
        let got = read_stderr_tail(&id).expect("a log with a line yields it");
        assert!(got.contains("already has an active writer"), "{got}");
        let _ = std::fs::remove_file(crate::pty::stderr_log_path(&id));
    }

    /// Retrying appends the same line every time. Six copies of one message
    /// would fill the pane with no more information than one.
    #[test]
    fn a_message_repeated_by_every_retry_is_shown_once() {
        let id = format!("tail-dedup-{}", std::process::id());
        write_log(&id, &"Error: same failure\n".repeat(20));
        let got = read_stderr_tail(&id).unwrap();
        assert_eq!(got, "Error: same failure");
        let _ = std::fs::remove_file(crate::pty::stderr_log_path(&id));
    }

    #[test]
    fn only_the_last_few_lines_survive() {
        let id = format!("tail-cap-{}", std::process::id());
        let body: String = (0..50).map(|i| format!("line {i}\n")).collect();
        write_log(&id, &body);
        let got = read_stderr_tail(&id).unwrap();
        assert_eq!(got.lines().count(), 6, "capped");
        assert!(got.ends_with("line 49"), "keeps the newest: {got}");
        let _ = std::fs::remove_file(crate::pty::stderr_log_path(&id));
    }

    #[test]
    fn a_child_that_wrote_nothing_yields_nothing() {
        let id = format!("tail-empty-{}", std::process::id());
        write_log(&id, "   \n\n");
        assert!(read_stderr_tail(&id).is_none());
        assert!(read_stderr_tail("tail-never-existed-xyz").is_none());
        let _ = std::fs::remove_file(crate::pty::stderr_log_path(&id));
    }
}

#[cfg(test)]
mod tests {
    use super::bubble_urgent_within_bands;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Bands are the outer structure; urgency only reorders inside one. Pulling an
    /// urgent pane to the very front would split its category, and the grid reads
    /// bands off consecutive runs — so it would draw the wrong header over it.
    #[test]
    fn urgent_bubbles_inside_its_band_not_to_the_front_of_the_grid() {
        let panes = ids(&["m1", "m2", "s1", "s2"]);
        let band = |id: &str| Some(id[..1].to_string());
        // The last pane is the urgent one — furthest from the front.
        let out = bubble_urgent_within_bands(&panes, band, |id| id == "s2");
        assert_eq!(out, ids(&["m1", "m2", "s2", "s1"]));
        // Band membership is still two consecutive runs.
        let bands: Vec<String> = out.iter().map(|id| id[..1].to_string()).collect();
        assert_eq!(bands, vec!["m", "m", "s", "s"]);
    }

    #[test]
    fn band_order_is_first_appearance_and_survives_bubbling() {
        // soda-nest appears first, so it stays the first band even though the
        // urgent pane is in mindplayer.
        let panes = ids(&["s1", "m1", "m2", "s2"]);
        let out =
            bubble_urgent_within_bands(&panes, |id| Some(id[..1].to_string()), |id| id == "m2");
        assert_eq!(out, ids(&["s1", "s2", "m2", "m1"]));
    }

    #[test]
    fn uncategorized_panes_stay_one_band_at_the_position_they_first_appear() {
        let panes = ids(&["a", "m1", "b", "m2"]);
        let band = |id: &str| id.starts_with('m').then(|| "m".to_string());
        let out = bubble_urgent_within_bands(&panes, band, |_| false);
        assert_eq!(
            out,
            ids(&["a", "b", "m1", "m2"]),
            "the None band groups too, ordered by where it first appeared"
        );
    }

    #[test]
    fn nothing_urgent_leaves_a_grouped_order_untouched() {
        let panes = ids(&["m1", "m2", "s1"]);
        let out = bubble_urgent_within_bands(&panes, |id| Some(id[..1].to_string()), |_| false);
        assert_eq!(
            out, panes,
            "no reorder means reorder_panes_by_status stays quiet"
        );
    }

    #[test]
    fn several_urgent_panes_keep_their_relative_order_within_a_band() {
        let panes = ids(&["m1", "m2", "m3", "m4"]);
        let out = bubble_urgent_within_bands(
            &panes,
            |_| Some("m".to_string()),
            |id| id == "m2" || id == "m4",
        );
        assert_eq!(out, ids(&["m2", "m4", "m1", "m3"]));
    }

    #[test]
    fn urgent_bubbles_to_the_front_when_there_is_only_one_band() {
        // Nothing categorized: a single band, so this is the plain "urgent first"
        // behaviour the flat grid had.
        let panes = ids(&["a", "b", "c", "d"]);
        let out = bubble_urgent_within_bands(&panes, |_| None, |id| id == "c");
        assert_eq!(out, ids(&["c", "a", "b", "d"]));
    }
}
