use super::orchestration_lanes::{is_orchestration_child_session, is_orchestration_main_session};
use super::*;

pub(crate) fn matches_search(s: &Session, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || s.title.to_lowercase().contains(&query)
        || s.id.to_lowercase().contains(&query)
        || s.agent.as_str().contains(&query)
}

impl App {
    /// Spawn a scan of the current scope on a background thread.
    pub(crate) fn spawn_scan(&self) -> Receiver<Vec<Session>> {
        let scope = self.scope.clone();
        let cfg = self.cfg.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(scan(&scope, &cfg));
        });
        rx
    }

    /// Confirm the scope choice and kick off the initial scan (scan screen).
    pub fn start_scan(&mut self) {
        self.scope = if self.scope_choice == 0 {
            Scope::WorkingDir(self.cwd.clone())
        } else {
            Scope::Global
        };
        self.state.last_scope = Some(self.scope.label());
        let _ = self.state.save();

        self.scan_rx = Some(self.spawn_scan());
        self.screen = Screen::Scanning;
        self.spinner = 0;
    }

    /// Re-scan in the background without leaving the main view — used to pick up
    /// newly created sessions (and resolve their pending labels). No-op if one
    /// is already running.
    pub fn start_bg_rescan(&mut self) {
        if self.bg_rescan_rx.is_none() {
            self.bg_rescan_rx = Some(self.spawn_scan());
        }
    }

    /// Apply a finished background re-scan in place (keeps the main view and the
    /// cursor on the same session), resolving any pending labels against the
    /// fresh session set. Returns true if anything changed.
    pub fn poll_bg_rescan(&mut self) -> bool {
        let Some(rx) = &self.bg_rescan_rx else {
            return false;
        };
        let Ok(mut sessions) = rx.try_recv() else {
            return false;
        };
        self.bg_rescan_rx = None;

        let selected_id = self.selected_session().map(|s| s.id.clone());
        // Resolve labels against the raw scan, persist, then stamp titles.
        if self.state.resolve_pending(&sessions) {
            let _ = self.state.save();
        }
        self.state.apply(&mut sessions);
        self.aggregate = Aggregate::of(&sessions);
        self.all_sessions = sessions;
        self.merge_extras();
        self.repair_title_based_orchestration_links();
        self.rebuild_visible();
        if let Some(id) = selected_id {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }
        // Keep retrying (until matched or expired) while labels are unresolved.
        if !self.state.pending_labels.is_empty() {
            self.rescan_due = Some(Instant::now() + Duration::from_secs(6));
        }
        true
    }

    /// Poll the scan thread; when finished, populate state and show the summary.
    /// Returns true if results arrived (needs redraw).
    pub fn poll_scan(&mut self) -> bool {
        if let Some(rx) = &self.scan_rx {
            if let Ok(mut sessions) = rx.try_recv() {
                // Resolve labels queued in a previous run before stamping titles.
                if self.state.resolve_pending(&sessions) {
                    let _ = self.state.save();
                }
                self.state.apply(&mut sessions);
                self.aggregate = Aggregate::of(&sessions);
                self.all_sessions = sessions;
                self.merge_extras();
                self.repair_title_based_orchestration_links();
                self.rebuild_visible();
                self.scan_rx = None;
                self.screen = Screen::ScanSummary;
                // If labels are still unresolved (their sessions don't exist
                // yet), keep trying via background re-scans.
                if !self.state.pending_labels.is_empty() {
                    self.rescan_due = Some(Instant::now() + Duration::from_secs(6));
                }
                return true;
            }
        }
        false
    }

    pub(crate) fn rebuild_visible(&mut self) {
        let show_archived = self.show_archived;
        let show_subagents = self.show_subagents;
        let query = self.search_query.as_deref();
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        let mut by_root: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in self.all_sessions.iter().enumerate() {
            let root = self.state.thread_root(&s.id).to_string();
            by_root.entry(root).or_default().push(i);
        }
        let mut roots: Vec<String> = by_root.keys().cloned().collect();
        roots.sort_by_key(|root| {
            self.all_sessions
                .iter()
                .position(|s| s.id == *root)
                .unwrap_or(usize::MAX)
        });
        for root in roots {
            let Some(indices) = by_root.remove(&root) else {
                continue;
            };
            let has_visible_match = indices.iter().any(|&i| {
                let s = &self.all_sessions[i];
                show_archived == s.archived
                    && (show_subagents || !s.is_subagent)
                    && query.is_none_or(|query| matches_search(s, query))
            });
            if !has_visible_match {
                continue;
            }
            let mut ordered = indices
                .into_iter()
                .filter(|&i| {
                    let s = &self.all_sessions[i];
                    show_archived == s.archived
                        && (show_subagents || !s.is_subagent)
                        && query.is_none_or(|query| {
                            matches_search(s, query)
                                || self
                                    .all_sessions
                                    .iter()
                                    .find(|root_session| root_session.id == root)
                                    .is_some_and(|root_session| matches_search(root_session, query))
                        })
                })
                .collect::<Vec<_>>();
            ordered.sort_by_key(|&i| {
                let s = &self.all_sessions[i];
                (
                    if s.id == root { 0 } else { 1 },
                    agent_rank(s.agent),
                    std::cmp::Reverse(s.last_active),
                )
            });
            groups.push((root, ordered));
        }
        // Top-level list category: sessions touched within the last 24h (a
        // rolling window, not a calendar day) sort above everything older, so
        // recent work is always at the top on startup. Agent grouping is
        // preserved as the secondary key, so within each "Recent" / "Older"
        // band rows still cluster by agent.
        let now = Utc::now();
        // A group counts as "recent" if any of its sessions was touched in the
        // last 24h OR is running live in MindPlayer right now — a session you
        // opened and are driving belongs at the top, even if its transcript
        // file's mtime is stale (e.g. an orchestration parent whose lanes
        // write their own files).
        let group_is_recent = |app: &Self, indices: &[usize]| -> bool {
            indices.iter().any(|&i| {
                let s = &app.all_sessions[i];
                app.is_running(&s.id) || touched_recently(s, now)
            })
        };
        groups.sort_by_cached_key(|(root, indices)| {
            let recent_rank = u8::from(!group_is_recent(self, indices));
            let section_agent = self.thread_root_agent_for_indices(root, indices);
            let best_status = indices
                .iter()
                .map(|&i| status_rank(self.session_status(&self.all_sessions[i].id)))
                .min()
                .unwrap_or(u8::MAX);
            let latest = indices
                .iter()
                .filter_map(|&i| self.all_sessions[i].last_active)
                .max();
            (
                recent_rank,
                agent_rank(section_agent),
                best_status,
                std::cmp::Reverse(latest),
            )
        });
        // The sort puts every "recent" group first, so the visible list is a
        // [recent…][older…] split. Record the boundary so the renderer can draw
        // the section headers from one source of truth — never recomputing per
        // row (which could disagree with the sort order and emit duplicate
        // headers).
        let mut recent_count = 0usize;
        self.visible = Vec::new();
        for (_, indices) in &groups {
            if group_is_recent(self, indices) {
                recent_count += indices.len();
            }
            self.visible.extend(indices.iter().copied());
        }
        self.recent_count = recent_count;
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        // Keep the status-bar totals in sync with what's actually listed.
        self.visible_aggregate = Aggregate::of_refs(
            self.visible
                .iter()
                .filter_map(|&i| self.all_sessions.get(i)),
        );
        // Drop marks for rows no longer visible (filtered out / archived) so a
        // bulk launch never targets a hidden session.
        if !self.marked.is_empty() {
            let visible_ids: HashSet<&str> = self
                .visible
                .iter()
                .filter_map(|&i| self.all_sessions.get(i))
                .map(|s| s.id.as_str())
                .collect();
            self.marked.retain(|id| visible_ids.contains(id.as_str()));
        }
    }

    pub(crate) fn repair_title_based_orchestration_links(&mut self) {
        let mut repairs = Vec::new();
        for child in self.all_sessions.iter() {
            if self.state.handoff_parent(&child.id).is_some()
                || !is_orchestration_child_session(child)
            {
                continue;
            }
            let parent = self
                .all_sessions
                .iter()
                .filter(|candidate| {
                    candidate.id != child.id
                        && candidate.agent == child.agent
                        && candidate.cwd == child.cwd
                        && is_orchestration_main_session(candidate)
                })
                .max_by_key(|candidate| candidate.started_at);
            if let Some(parent) = parent {
                repairs.push((child.id.clone(), parent.id.clone()));
            }
        }
        if repairs.is_empty() {
            return;
        }
        let now = Utc::now();
        for (child_id, parent_id) in repairs {
            self.state.set_handoff_link(
                &child_id,
                &parent_id,
                PathBuf::from("mindplayer-orchestration-title-fallback"),
                now,
            );
        }
        let _ = self.state.save();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    /// Move the selection by a small page step (PageUp/PageDown). Unlike
    /// single-step movement this clamps at the ends instead of wrapping.
    pub fn move_page(&mut self, dir: isize) {
        if self.visible.is_empty() {
            return;
        }
        let page = 4;
        let last = self.visible.len() as isize - 1;
        let next = (self.selected as isize + dir * page).clamp(0, last);
        self.selected = next as usize;
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.visible
            .get(self.selected)
            .and_then(|&i| self.all_sessions.get(i))
    }

    /// The session at a visible row (used by the renderer).
    pub fn session_at(&self, row: usize) -> Option<&Session> {
        self.visible
            .get(row)
            .and_then(|&i| self.all_sessions.get(i))
    }

    pub fn session_display_name(&self, id: &str, max_chars: usize) -> String {
        let label = self.state.label_for(id).map(str::to_string);
        let title = self
            .all_sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.title.trim().to_string())
            .filter(|s| !s.is_empty());
        let name = label.or(title).unwrap_or_else(|| short(id));
        truncate_chars(&name, max_chars.max(8))
    }

    pub fn session_depth(&self, id: &str) -> usize {
        usize::from(self.state.handoff_parent(id).is_some())
    }

    pub(crate) fn thread_root_agent_for_indices(&self, root: &str, indices: &[usize]) -> Agent {
        self.all_sessions
            .iter()
            .find(|s| s.id == root)
            .map(|s| s.agent)
            .or_else(|| {
                indices
                    .first()
                    .and_then(|&i| self.all_sessions.get(i))
                    .map(|s| s.agent)
            })
            .unwrap_or(Agent::Codex)
    }

    pub fn thread_child_count(&self, id: &str) -> usize {
        self.all_sessions
            .iter()
            .filter(|s| self.state.handoff_parent(&s.id) == Some(id))
            .count()
    }

    /// Activity for a list row's time column: `(live_now, effective_last_active)`.
    /// Every session in a thread (root, a middle handoff link, or a leaf child
    /// lane) reflects the WHOLE thread's freshest activity, not just its own
    /// transcript — a handoff child whose own file hasn't been touched since
    /// it was created still reads the parent/sibling's recent time when the
    /// thread was worked on since. Only a session with no parent AND no
    /// children (truly standalone) skips the scan and stays O(1).
    pub fn row_activity(&self, s: &Session, child_count: usize) -> (bool, Option<DateTime<Utc>>) {
        if self.is_running(&s.id) {
            return (true, s.last_active);
        }
        let root = self.state.thread_root(&s.id);
        if child_count == 0 && root == s.id.as_str() {
            return (false, s.last_active);
        }
        let mut latest = s.last_active;
        for other in &self.all_sessions {
            if other.id == s.id || self.state.thread_root(&other.id) != root {
                continue;
            }
            if self.is_running(&other.id) {
                return (true, latest.max(other.last_active));
            }
            latest = latest.max(other.last_active);
        }
        (false, latest)
    }

    pub fn toggle_archived_view(&mut self) {
        self.show_archived = !self.show_archived;
        self.selected = 0;
        self.rebuild_visible();
    }

    pub fn toggle_subagents(&mut self) {
        self.show_subagents = !self.show_subagents;
        self.selected = 0;
        self.rebuild_visible();
    }

    pub fn rescan(&mut self) {
        self.start_scan();
    }

    /// Kick off a background usage refresh (no-op if one is already running).
    /// File stats and token parsing happen off the main thread so input and
    /// rendering never stall; results are applied in [`Self::poll_refresh`].
    pub fn start_refresh(&mut self) {
        if self.refresh_rx.is_some() || self.all_sessions.is_empty() {
            return;
        }
        let mut sessions = self.all_sessions.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            refresh_activity_and_usage(&mut sessions);
            let out: Vec<ActivityUpdate> = sessions
                .into_iter()
                .map(|s| ActivityUpdate {
                    id: s.id,
                    last_active: s.last_active,
                    tokens: s.tokens,
                    context_pct: s.context_pct,
                })
                .collect();
            let _ = tx.send(out);
        });
        self.refresh_rx = Some(rx);
    }

    /// Apply a finished background refresh: update activity/usage, re-sort
    /// newest-first, and keep the cursor on the same session by id. Returns true
    /// if the list changed (needs redraw).
    pub fn poll_refresh(&mut self) -> bool {
        let Some(rx) = &self.refresh_rx else {
            return false;
        };
        let Ok(updates_raw) = rx.try_recv() else {
            return false;
        };
        self.refresh_rx = None;

        // Defensive: if two discovered sessions ever share an id (e.g. a data
        // source that embeds the wrong id in a nested transcript), a plain
        // `collect()` into a HashMap would silently keep whichever update
        // happened to be last, possibly stamping a freshly-active session with
        // a stale sibling's timestamp. Keep the most-recently-active update
        // per id instead, so a collision can only ever look "too fresh", never
        // corrupt a genuinely active session backwards in time.
        let mut updates: HashMap<String, ActivityUpdate> = HashMap::new();
        for u in updates_raw {
            match updates.get(&u.id) {
                Some(existing) if existing.last_active >= u.last_active => {}
                _ => {
                    updates.insert(u.id.clone(), u);
                }
            }
        }
        for s in self.all_sessions.iter_mut() {
            if let Some(update) = updates.get(&s.id) {
                s.last_active = update.last_active;
                s.tokens = update.tokens;
                s.context_pct = update.context_pct;
            }
        }
        let selected_id = self.selected_session().map(|s| s.id.clone());
        sort_by_recency(&mut self.all_sessions);
        self.rebuild_visible();
        if let Some(id) = selected_id {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }
        true
    }

    // --- PTY lifecycle ----------------------------------------------------

    /// Close the selected session: stop its PTY (if any) and archive it. A
    /// brand-new session with no disk file yet is simply dropped (nothing to
    /// archive).
    pub fn close_selected(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        // Remember a deliberate neighbor (the row that will slide under the
        // cursor) by id, so after the list shrinks the selection lands on it
        // instead of silently inheriting whatever shifted into the old index —
        // important because the next 'x' archives + SIGKILLs the selected row.
        let neighbor_id = self
            .visible
            .get(self.selected + 1)
            .or_else(|| {
                self.selected
                    .checked_sub(1)
                    .and_then(|i| self.visible.get(i))
            })
            .and_then(|&i| self.all_sessions.get(i))
            .map(|s| s.id.clone());
        if let Some(mut pty) = self.ptys.remove(&session.id) {
            pty.kill();
        }
        self.ended.remove(&session.id);
        self.pending_initial_inputs.remove(&session.id);
        self.turn_submitted.remove(&session.id);
        if self.active.as_deref() == Some(session.id.as_str()) {
            self.remove_pane(&session.id);
            self.focus = Focus::List;
        } else {
            self.remove_pane(&session.id);
        }
        if session.id.starts_with("new:") {
            // Synthetic placeholder (no rollout file): just remove it.
            self.extra_sessions.retain(|s| s.id != session.id);
            self.all_sessions.retain(|s| s.id != session.id);
            self.new_baselines.remove(&session.id);
            self.status = "closed new session".to_string();
        } else {
            self.state.set_archived(&session.id, true);
            let _ = self.state.save();
            if let Some(s) = self.all_sessions.iter_mut().find(|s| s.id == session.id) {
                s.archived = true;
            }
            self.status = format!("archived {}", short(&session.id));
        }
        self.rebuild_visible();
        // Restore the cursor onto the remembered neighbor by id.
        if let Some(nid) = neighbor_id {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == nid)
            {
                self.selected = pos;
            }
        }
    }
}
