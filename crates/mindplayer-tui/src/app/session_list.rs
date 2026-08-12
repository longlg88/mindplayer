use super::*;

/// How long an in-flight rate-limit fetch may run before it is abandoned.
/// Generous next to curl's own 8 s cap; the case this covers is `security(1)`
/// blocking on a Keychain approval dialog, which has no timeout of its own.
pub(crate) const LIMITS_FETCH_DEADLINE: Duration = Duration::from_secs(30);

/// Default content for `~/.mindplayer/prompts/catchup.md` — seeded there on
/// first use (see `mindplayer_core::load_prompt`) so it can be rewritten at
/// any time without a rebuild. Sent verbatim into a live session's CLI by
/// `c` (catch-up) — the target agent answers using its own project/backlog/
/// transcript, mindplayer never reads or summarizes any of it itself.
const DEFAULT_CATCHUP_PROMPT: &str = "\
잠깐 다른 일 하다가 돌아왔어. 아래 정리해서 알려줘:
1. 지금 이 프로젝트가 뭐 하는 프로젝트인지 간단히 소개
2. 이 프로젝트에 backlog.html이 있으면 열어서 보여주고, 없으면 지금까지 진행 상황 기반으로 하나 만들어줘
3. 최근에 내가 뭘 물어봤고 네가 뭘 했는지 요약";

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
        let _ = self.save_state();

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
            let _ = self.save_state();
        }
        self.state.apply(&mut sessions);
        self.aggregate = Aggregate::of(&sessions);
        self.all_sessions = sessions;
        self.merge_extras();
        self.rebuild_visible();
        if let Some(id) = selected_id {
            if let Some(pos) = self.row_of_session(&id) {
                self.selected = pos;
            }
        }
        // Keep retrying (until matched or expired) while labels are unresolved.
        if self.awaits_rescan() {
            self.rescan_due = Some(Instant::now() + Duration::from_secs(6));
        }
        true
    }

    /// Whether a session file is still expected to appear — either to carry a
    /// queued label, or to be archived because its new session was closed
    /// before the agent wrote it.
    fn awaits_rescan(&self) -> bool {
        !self.state.pending_labels.is_empty() || !self.closed_extras.is_empty()
    }

    /// Poll the scan thread; when finished, populate state and show the summary.
    /// Returns true if results arrived (needs redraw).
    pub fn poll_scan(&mut self) -> bool {
        if let Some(rx) = &self.scan_rx {
            if let Ok(mut sessions) = rx.try_recv() {
                // Resolve labels queued in a previous run before stamping titles.
                if self.state.resolve_pending(&sessions) {
                    let _ = self.save_state();
                }
                self.state.apply(&mut sessions);
                self.aggregate = Aggregate::of(&sessions);
                self.all_sessions = sessions;
                self.merge_extras();
                self.rebuild_visible();
                self.scan_rx = None;
                self.screen = Screen::ScanSummary;
                // If labels are still unresolved (their sessions don't exist
                // yet), keep trying via background re-scans.
                if self.awaits_rescan() {
                    self.rescan_due = Some(Instant::now() + Duration::from_secs(6));
                }
                return true;
            }
        }
        false
    }

    pub(crate) fn rebuild_visible(&mut self) {
        let show_archived = self.show_archived;
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
                    && !s.is_subagent
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
                        && !s.is_subagent
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
        // recent work is always at the top on startup. Status urgency is the
        // secondary key (see `status_rank` — a just-finished group outranks
        // even a currently-blocked one), agent grouping breaks ties after
        // that, so same-agent clustering still holds among groups that are
        // otherwise equally urgent.
        let now = Utc::now();
        // A group counts as "recent" if any of its sessions was touched in the
        // last 24h OR is running live in MindPlayer right now — a session you
        // opened and are driving belongs at the top, even if its transcript
        // file's mtime is stale.
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
                best_status,
                agent_rank(section_agent),
                std::cmp::Reverse(latest),
            )
        });
        // Second tier: bucket the thread groups by category, preserving the
        // order just established. A category inherits the best rank among its
        // threads, so an urgent/recent topic still floats to the top and the
        // whole topic moves together (same atomic rule threads already had —
        // one recent lane pulls the group up, so a topic is never split across
        // the recent/older divider).
        // Categorized threads bucket by category; uncategorized ones stay loose.
        // Bucketing the leftovers too would make them atomic as well, and with
        // hundreds of uncategorized sessions a single recent one would drag the
        // whole pile above the divider — the `older` split would vanish.
        let mut buckets: Vec<(String, Vec<Vec<usize>>)> = Vec::new();
        let mut loose: Vec<Vec<usize>> = Vec::new();
        for (root, indices) in groups {
            match self.category_for_thread(&root, &indices) {
                Some(cat) => match buckets.iter_mut().find(|(c, _)| *c == cat) {
                    Some((_, threads)) => threads.push(indices),
                    None => buckets.push((cat, vec![indices])),
                },
                None => loose.push(indices),
            }
        }
        // Only label the leftovers when there is something to contrast them
        // with; on a fresh install every row would otherwise sit under a
        // pointless "no category" header.
        let has_real_category = !buckets.is_empty();

        // Within each band, named topics come first and loose sessions follow —
        // a predictable rule that also keeps every category contiguous.
        let (recent_buckets, older_buckets): (Vec<_>, Vec<_>) = buckets
            .into_iter()
            .partition(|(_, threads)| threads.iter().any(|t| group_is_recent(self, t)));
        let (recent_loose, older_loose): (Vec<_>, Vec<_>) =
            loose.into_iter().partition(|t| group_is_recent(self, t));

        // Tally before emitting, so folded categories still report their size.
        self.category_counts.clear();
        for (cat, threads) in recent_buckets.iter().chain(older_buckets.iter()) {
            let n: usize = threads.iter().map(|t| t.len()).sum();
            *self.category_counts.entry(Some(cat.clone())).or_insert(0) += n;
        }
        let loose_total: usize = recent_loose
            .iter()
            .chain(older_loose.iter())
            .map(|t| t.len())
            .sum();
        if loose_total > 0 {
            self.category_counts.insert(None, loose_total);
        }

        self.visible = Vec::new();
        for (cat, threads) in &recent_buckets {
            self.push_category_rows(Some(cat), threads);
        }
        if has_real_category && !recent_loose.is_empty() {
            self.visible.push(Row::Header(None));
        }
        for t in &recent_loose {
            self.visible.extend(t.iter().copied().map(Row::Session));
        }
        // Everything emitted so far is the `recent` band; the renderer draws the
        // divider at this boundary.
        self.recent_count = self.visible.len();

        for (cat, threads) in &older_buckets {
            self.push_category_rows(Some(cat), threads);
        }
        if has_real_category && !older_loose.is_empty() {
            self.visible.push(Row::Header(None));
        }
        for t in &older_loose {
            self.visible.extend(t.iter().copied().map(Row::Session));
        }
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        // Keep the status-bar totals in sync with what's actually listed. Rows
        // hidden inside a collapsed category are deliberately excluded — the
        // totals describe what you can see.
        self.visible_aggregate = Aggregate::of_refs(
            self.visible
                .iter()
                .filter_map(|r| r.session_index())
                .filter_map(|i| self.all_sessions.get(i)),
        );
        // Drop marks for rows no longer visible (filtered out / archived /
        // folded away) so a bulk launch never targets a hidden session.
        if !self.marked.is_empty() {
            let visible_ids: HashSet<&str> = self
                .visible
                .iter()
                .filter_map(|r| r.session_index())
                .filter_map(|i| self.all_sessions.get(i))
                .map(|s| s.id.as_str())
                .collect();
            self.marked.retain(|id| visible_ids.contains(id.as_str()));
        }
    }

    // --- category menu (`t` on a header) -----------------------------------

    /// `t` on a category header. Refused for the uncategorized header, which is
    /// not a real category and has nothing to configure.
    pub fn open_category_menu(&mut self) -> bool {
        let Some(id) = self.selected_category().map(str::to_string) else {
            return false;
        };
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::CategoryMenuBegin { cat_id: id.clone() },
        );
        self.category_menu = Some(CategoryMenu {
            cat_id: id,
            selected: 0,
            rename: None,
            confirm_remove: false,
        });
        true
    }

    pub fn cancel_category_menu(&mut self) {
        self.category_menu = None;
    }

    pub fn move_category_menu(&mut self, delta: isize) {
        if let Some(m) = self.category_menu.as_mut() {
            if m.rename.is_some() {
                return; // typing; arrows belong to the text field
            }
            m.confirm_remove = false; // moving away cancels a pending confirm
            m.selected =
                (m.selected as isize + delta).rem_euclid(CategoryMenu::ROWS as isize) as usize;
        }
    }

    pub fn category_rename_push(&mut self, c: char) {
        if let Some(buf) = self.category_menu.as_mut().and_then(|m| m.rename.as_mut()) {
            buf.push(c);
        }
    }

    pub fn category_rename_backspace(&mut self) {
        if let Some(buf) = self.category_menu.as_mut().and_then(|m| m.rename.as_mut()) {
            buf.pop();
        }
    }

    /// Enter in the category menu.
    pub fn confirm_category_menu(&mut self) {
        let Some(menu) = self.category_menu.clone() else {
            return;
        };
        let cat = menu.cat_id;

        // Second enter on rename: commit the typed name.
        if let Some(name) = menu.rename {
            if self.state.rename_category(&cat, &name) {
                let _ = self.save_state();
                self.status = format!("renamed to {}", self.category_label(&cat));
                self.category_menu = None;
                self.rebuild_visible();
            } else {
                self.status = "category: name cannot be blank".to_string();
            }
            return;
        }

        match menu.selected {
            CategoryMenu::AUTO_SYNC => {
                let now_on = !self.state.category_auto_sync(&cat);
                self.state.set_category_auto_sync(&cat, now_on);
                let _ = self.save_state();
                self.status = format!(
                    "{}: auto-sync {}",
                    self.category_label(&cat),
                    if now_on { "on" } else { "off" }
                );
            }
            CategoryMenu::SYNC_NOW => {
                self.category_menu = None;
                self.sync_category_now(&cat);
            }
            CategoryMenu::RENAME => {
                let current = self.category_label(&cat);
                if let Some(m) = self.category_menu.as_mut() {
                    m.rename = Some(current);
                }
                self.status = "category: edit the name, enter to save".to_string();
            }
            CategoryMenu::REMOVE => {
                if !menu.confirm_remove {
                    if let Some(m) = self.category_menu.as_mut() {
                        m.confirm_remove = true;
                    }
                    self.status =
                        "remove category? enter again to confirm (sessions are kept)".to_string();
                    return;
                }
                self.remove_category(&cat);
            }
            _ => {}
        }
    }

    /// Force a sync for every live session in a category, ignoring the toggle.
    /// Only one sync runs at a time, so this starts with the focused session
    /// when it belongs to the category and otherwise takes the first live one —
    /// the rest pick theirs up as they are entered.
    pub fn sync_category_now(&mut self, cat: &str) {
        let candidates: Vec<Session> = self
            .all_sessions
            .iter()
            .filter(|s| self.category_of_session(&s.id).as_deref() == Some(cat))
            .filter(|s| self.is_running(&s.id))
            .cloned()
            .collect();
        if candidates.is_empty() {
            self.status = "sync now: no live session in this category".to_string();
            return;
        }
        let focused = self.active.clone();
        let target = candidates
            .iter()
            .find(|s| Some(&s.id) == focused.as_ref())
            .or_else(|| candidates.first())
            .cloned();
        if let Some(target) = target {
            if self.spawn_category_sync_for(&target, true) {
                self.status = format!("syncing {} …", short(&target.id));
            } else {
                self.status = "sync now: nothing new from peers".to_string();
            }
        }
    }

    /// Drop a category. Its sessions are untouched — only the grouping goes,
    /// along with the watermarks, which would otherwise hide peer content from
    /// those sessions if they were regrouped later.
    fn remove_category(&mut self, cat: &str) {
        let members: Vec<String> = self
            .state
            .session_category
            .iter()
            .filter(|(_, c)| c.as_str() == cat)
            .map(|(s, _)| s.clone())
            .collect();
        for id in &members {
            self.state.clear_category(id);
            self.state.clear_sync_marks_for(id);
        }
        self.state.categories.remove(cat);
        self.state.collapsed_categories.remove(cat);
        let _ = self.save_state();
        self.category_menu = None;
        self.rebuild_visible();
        self.status = format!("category removed ({} sessions kept)", members.len());
    }

    // --- category picker (`t`) ---------------------------------------------

    /// Rows offered by the picker: existing categories, then "new", then
    /// "clear". Indices line up with `CategoryPicker::selected`.
    pub fn category_picker_rows(&self) -> Vec<(Option<String>, String)> {
        let mut rows: Vec<(Option<String>, String)> = self
            .state
            .categories_by_name()
            .into_iter()
            .map(|(id, name)| (Some(id.to_string()), name.to_string()))
            .collect();
        rows.push((None, "+ new category…".to_string()));
        rows.push((None, "− remove from category".to_string()));
        rows
    }

    /// `t`: open the picker for the marked rows, or for the row under the cursor.
    /// A header row has nothing to categorize, so it is refused rather than
    /// silently doing nothing.
    pub fn begin_category_pick(&mut self) {
        let targets: Vec<String> = if self.multi_select && !self.marked.is_empty() {
            self.marked.iter().cloned().collect()
        } else {
            match self.selected_session() {
                Some(s) => vec![s.id.clone()],
                None => {
                    self.status = "category: pick a session row first".to_string();
                    return;
                }
            }
        };
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::CategoryPickBegin {
                targets: targets.len(),
            },
        );
        // Start on the category the (first) target already has, so re-opening
        // shows where it currently sits.
        let current = self.state.category_of(&targets[0]).map(str::to_string);
        let selected = current
            .and_then(|id| {
                self.category_picker_rows()
                    .iter()
                    .position(|(rid, _)| rid.as_deref() == Some(id.as_str()))
            })
            .unwrap_or(0);
        let n = targets.len();
        self.category_picker = Some(CategoryPicker {
            targets,
            selected,
            new_name: None,
        });
        self.status = if n > 1 {
            format!("category: {n} sessions")
        } else {
            "category: pick one, or + to create".to_string()
        };
    }

    pub fn cancel_category_pick(&mut self) {
        self.category_picker = None;
    }

    pub fn move_category_pick(&mut self, delta: isize) {
        let len = self.category_picker_rows().len() as isize;
        if let Some(p) = self.category_picker.as_mut() {
            if p.new_name.is_some() {
                return; // typing a name; arrows are for the text field
            }
            p.selected = (p.selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    pub fn category_name_push(&mut self, c: char) {
        if let Some(name) = self
            .category_picker
            .as_mut()
            .and_then(|p| p.new_name.as_mut())
        {
            name.push(c);
        }
    }

    pub fn category_name_backspace(&mut self) {
        if let Some(name) = self
            .category_picker
            .as_mut()
            .and_then(|p| p.new_name.as_mut())
        {
            name.pop();
        }
    }

    /// Enter in the picker. On an existing category this assigns and closes; on
    /// "new" it opens the name field first; on "clear" it unassigns.
    pub fn confirm_category_pick(&mut self) {
        let Some(p) = self.category_picker.clone() else {
            return;
        };
        let rows = self.category_picker_rows();

        // Second enter, with a typed name: create and assign.
        if let Some(name) = p.new_name.clone() {
            let Some(id) = self.state.create_category(&name, Utc::now()) else {
                self.status = "category: name cannot be blank".to_string();
                return;
            };
            self.apply_category(&p.targets, Some(&id));
            return;
        }

        let is_new = p.selected == rows.len().saturating_sub(2);
        let is_clear = p.selected == rows.len().saturating_sub(1);
        if is_new {
            if let Some(picker) = self.category_picker.as_mut() {
                picker.new_name = Some(String::new());
            }
            self.status = "category: type a name, enter to create".to_string();
            return;
        }
        if is_clear {
            self.apply_category(&p.targets, None);
            return;
        }
        let Some((Some(id), _)) = rows.get(p.selected).cloned() else {
            return;
        };
        self.apply_category(&p.targets, Some(&id));
    }

    /// Assign (or clear) the category for every target, persist, and rebuild.
    #[cfg(test)]
    pub(crate) fn apply_category_for_test(&mut self, targets: &[String], cat: Option<&str>) {
        self.apply_category(targets, cat);
    }

    fn apply_category(&mut self, targets: &[String], cat: Option<&str>) {
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::CategoryAssign {
                targets: targets.len(),
                category: cat.map(|c| self.category_label(c)).unwrap_or_default(),
            },
        );
        for id in targets {
            let moved = self.state.category_of(id).map(str::to_string) != cat.map(str::to_string);
            match cat {
                Some(cat) => {
                    self.state.assign_category(id, cat);
                }
                None => self.state.clear_category(id),
            }
            // A session that changed group has different peers now. Watermarks
            // from the old grouping would make the new peers' earlier work look
            // already-seen, so it would never be shown.
            if moved {
                self.state.clear_sync_marks_for(id);
            }
        }
        // Clearing can empty a category; drop it rather than leaving a header
        // with nothing under it.
        let known: std::collections::BTreeSet<String> =
            self.all_sessions.iter().map(|s| s.id.clone()).collect();
        self.state.prune_categories(&known);
        self.state.prune_sync_marks(&known);
        let _ = self.save_state();
        self.category_picker = None;
        // Keep the cursor on the session that was just categorized, which has
        // usually moved to a different part of the list.
        let follow = targets.first().cloned();
        self.rebuild_visible();
        if let Some(id) = follow {
            if let Some(pos) = self.row_of_session(&id) {
                self.selected = pos;
            }
        }
        self.status = match cat {
            Some(cat) => format!(
                "{} → {}",
                if targets.len() > 1 {
                    format!("{} sessions", targets.len())
                } else {
                    "session".to_string()
                },
                self.category_label(cat)
            ),
            None => "removed from category".to_string(),
        };
        if self.multi_select {
            self.cancel_multi_select();
        }
    }

    /// Emit a category's header plus its sessions, skipping the sessions when it
    /// is folded. The header is always emitted so a collapsed category still has
    /// a row to put the cursor on — otherwise it could never be reopened.
    fn push_category_rows(&mut self, cat: Option<&str>, threads: &[Vec<usize>]) {
        self.visible.push(Row::Header(cat.map(str::to_string)));
        if cat.is_some_and(|id| self.state.is_collapsed(id)) {
            return;
        }
        for t in threads {
            self.visible.extend(t.iter().copied().map(Row::Session));
        }
    }

    /// The category a whole handoff thread belongs to: its root's, falling back
    /// to the first lane that has one. Membership is per session, but a thread
    /// must land in exactly one bucket to stay intact under its own header.
    fn category_for_thread(&self, root: &str, indices: &[usize]) -> Option<String> {
        if let Some(cat) = self.state.category_of(root) {
            return Some(cat.to_string());
        }
        indices
            .iter()
            .filter_map(|&i| self.all_sessions.get(i))
            .find_map(|s| self.state.category_of(&s.id))
            .map(str::to_string)
    }

    pub fn move_selection(&mut self, delta: isize) {
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::ListMove { delta },
        );
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

    /// The selected session, or `None` when the cursor sits on a category
    /// header. Every single-session action returns early in that case, which is
    /// what makes a header row inert for `x`/`e`/`h`/`i`/`c`.
    pub fn selected_session(&self) -> Option<&Session> {
        self.visible
            .get(self.selected)
            .and_then(|r| r.session_index())
            .and_then(|i| self.all_sessions.get(i))
    }

    /// The session at a visible row (used by the renderer). `None` for headers.
    pub fn session_at(&self, row: usize) -> Option<&Session> {
        self.visible
            .get(row)
            .and_then(|r| r.session_index())
            .and_then(|i| self.all_sessions.get(i))
    }

    /// The row at a visible index, for the renderer to tell headers apart.
    pub fn row_at(&self, row: usize) -> Option<&Row> {
        self.visible.get(row)
    }

    /// Visible row index of a session, for restoring the cursor by id after a
    /// rebuild. Header rows are skipped, so this never lands the cursor on one.
    pub fn row_of_session(&self, id: &str) -> Option<usize> {
        self.visible.iter().position(|r| {
            r.session_index()
                .and_then(|i| self.all_sessions.get(i))
                .is_some_and(|s| s.id == id)
        })
    }

    /// Every session currently listed, headers skipped and folded-away rows
    /// excluded (they are not in `visible` at all).
    pub fn visible_sessions(&self) -> impl Iterator<Item = &Session> + '_ {
        self.visible
            .iter()
            .filter_map(|r| r.session_index())
            .filter_map(|i| self.all_sessions.get(i))
    }

    /// The category id the cursor is on, when it sits on a real category's
    /// header. `None` for session rows and for the "no category" header, neither
    /// of which can be folded.
    pub fn selected_category(&self) -> Option<&str> {
        match self.visible.get(self.selected) {
            Some(Row::Header(Some(id))) => Some(id.as_str()),
            _ => None,
        }
    }

    /// The category owning the cursor's row, whether the cursor is on the header
    /// itself or on one of its sessions. Drives `←` from inside a group.
    pub fn category_at_cursor(&self) -> Option<&str> {
        if let Some(id) = self.selected_category() {
            return Some(id);
        }
        let session = self.selected_session()?;
        self.state.category_of(&session.id)
    }

    /// Visible index of a category's header row.
    fn header_row_of(&self, cat_id: &str) -> Option<usize> {
        self.visible
            .iter()
            .position(|r| matches!(r, Row::Header(Some(id)) if id == cat_id))
    }

    /// `→` on a category header: unfold it. Returns false when there is nothing
    /// to unfold, so the caller can fall through to resuming a session.
    pub fn expand_selected_category(&mut self) -> bool {
        let Some(id) = self.selected_category().map(str::to_string) else {
            return false;
        };
        if !self.state.is_collapsed(&id) {
            return false;
        }
        self.state.set_collapsed(&id, false);
        let _ = self.save_state();
        self.rebuild_visible();
        self.status = format!("expanded {}", self.category_label(&id));
        true
    }

    /// `→` on a category header: unfold it when folded, otherwise step *into*
    /// the group by moving onto its first session. Deliberately never folds —
    /// that is `←`'s job. Wiring `→` to a toggle made it close a category that
    /// was already open, so the key never went deeper and reading the list felt
    /// unpredictable.
    pub fn enter_selected_category(&mut self) -> bool {
        let Some(id) = self.selected_category().map(str::to_string) else {
            return false;
        };
        if self.state.is_collapsed(&id) {
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::CategoryFold {
                    cat_id: id,
                    folded: false,
                },
            );
            return self.expand_selected_category();
        }
        // Already open: descend to the first row under this header, if it has one.
        if self
            .visible
            .get(self.selected + 1)
            .is_some_and(|r| !r.is_header())
        {
            self.selected += 1;
        }
        true
    }

    /// `←`: fold the category the cursor is in, and park the cursor on its
    /// header — the same "step out" gesture a file tree has.
    pub fn collapse_category_at_cursor(&mut self) -> bool {
        let Some(id) = self.category_at_cursor().map(str::to_string) else {
            return false;
        };
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::CategoryFold {
                cat_id: id.clone(),
                folded: true,
            },
        );
        self.state.set_collapsed(&id, true);
        let _ = self.save_state();
        self.rebuild_visible();
        if let Some(row) = self.header_row_of(&id) {
            self.selected = row;
        }
        self.status = format!("collapsed {}", self.category_label(&id));
        true
    }

    /// Fold/unfold from a single key (enter on a header). Returns false when the
    /// cursor is not on a foldable header.
    pub fn toggle_selected_category(&mut self) -> bool {
        let Some(id) = self.selected_category().map(str::to_string) else {
            return false;
        };
        if self.state.is_collapsed(&id) {
            self.expand_selected_category()
        } else {
            self.collapse_category_at_cursor()
        }
    }

    pub fn category_label(&self, cat_id: &str) -> String {
        self.state
            .category_name(cat_id)
            .unwrap_or("(unnamed)")
            .to_string()
    }

    /// How many sessions a category holds, folded or not — see
    /// [`App::category_counts`].
    pub fn category_session_count(&self, cat_id: Option<&str>) -> usize {
        self.category_counts
            .get(&cat_id.map(str::to_string))
            .copied()
            .unwrap_or(0)
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

    /// The freshest `last_prompt_at` across a row's whole thread (root, a
    /// handoff link, or a leaf lane) — mirrors [`Self::row_activity`]'s
    /// same-thread rollup, but for "when did a human last actually type
    /// something" rather than raw file activity. `None` when no lane in the
    /// thread has one (e.g. an all-kiro thread, which can't derive it at all).
    pub fn row_last_prompt(&self, s: &Session, child_count: usize) -> Option<DateTime<Utc>> {
        let root = self.state.thread_root(&s.id);
        if child_count == 0 && root == s.id.as_str() {
            return s.last_prompt_at;
        }
        let mut latest = s.last_prompt_at;
        for other in &self.all_sessions {
            if other.id == s.id || self.state.thread_root(&other.id) != root {
                continue;
            }
            latest = latest.max(other.last_prompt_at);
        }
        latest
    }

    pub fn toggle_archived_view(&mut self) {
        self.show_archived = !self.show_archived;
        self.selected = 0;
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::ViewToggle {
                view: "archived".to_string(),
                on: self.show_archived,
            },
        );
        self.rebuild_visible();
    }

    /// Toggle the manual "my work here isn't done yet" mark on the selected
    /// session — orthogonal to its live PTY status (see [`SessionStatus`]),
    /// so it survives the session going Idle/Ended and stays visible even
    /// buried in the older group.
    pub fn toggle_in_progress(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let id = session.id.clone();
        let now_in_progress = !self.state.is_in_progress(&id);
        self.state.set_in_progress(&id, now_in_progress);
        let _ = self.save_state();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::InProgressToggle {
                id,
                in_progress: now_in_progress,
            },
        );
        self.status = if now_in_progress {
            "marked in progress".to_string()
        } else {
            "unmarked in progress".to_string()
        };
    }

    /// `c` on the selected session. Idle sessions get the catch-up prompt
    /// right away; Working/Blocked ones confirm first since it queues in
    /// behind whatever turn is already running. Ended/Inactive sessions have
    /// no live PTY to receive it, so they're left alone rather than resumed
    /// just to deliver this.
    pub fn begin_catchup(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let id = session.id.clone();
        match self.session_status(&id) {
            SessionStatus::Idle => {
                mindplayer_core::log_event_to(
                    &self.audit_path,
                    mindplayer_core::AuditEvent::CatchupBegin {
                        id: id.clone(),
                        awaiting_confirm: false,
                    },
                );
                self.send_catchup(&id);
            }
            SessionStatus::Working | SessionStatus::Blocked => {
                mindplayer_core::log_event_to(
                    &self.audit_path,
                    mindplayer_core::AuditEvent::CatchupBegin {
                        id: id.clone(),
                        awaiting_confirm: true,
                    },
                );
                self.catchup_confirm = Some(id);
                self.status = "catch-up: session is busy — send anyway? (enter/esc)".to_string();
            }
            SessionStatus::Ended | SessionStatus::Inactive => {
                self.status = "catch-up only works on a live session".to_string();
            }
        }
    }

    pub fn confirm_catchup(&mut self) {
        if let Some(id) = self.catchup_confirm.take() {
            self.send_catchup(&id);
        }
    }

    pub fn cancel_catchup(&mut self) {
        if self.catchup_confirm.take().is_some() {
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::CatchupCancel,
            );
        }
    }

    fn send_catchup(&mut self, id: &str) {
        let Some(session) = self.all_sessions.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        let mut input =
            mindplayer_core::load_prompt_from(&self.prompts_dir, "catchup", DEFAULT_CATCHUP_PROMPT);
        input.push('\r');
        if self.enqueue_or_submit_to_session(&session, input.into_bytes()) {
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::CatchupSent,
            );
            self.status = format!("catch-up prompt sent to {}", short(&session.id));
        } else {
            self.status = format!("catch-up failed to send to {}", short(&session.id));
        }
    }

    /// Refresh the rate-limit readout on a worker thread. Skipped when one is
    /// already in flight; the popup shows the previous values until it lands.
    pub(crate) fn spawn_limits_fetch(&mut self) {
        // A fetch that never returns — `security(1)` can block on an interactive
        // Keychain prompt — would otherwise hold `limits_rx` for the rest of the
        // process and leave the popup stuck on "…" with no way to retry. After
        // the deadline the channel is abandoned so a later open can try again.
        if self
            .limits_started
            .is_some_and(|t| t.elapsed() >= LIMITS_FETCH_DEADLINE)
        {
            self.limits_rx = None;
            self.limits_started = None;
        }
        if self.limits_rx.is_some() {
            return;
        }
        self.limits_started = Some(Instant::now());
        let home = super::limits_home_for_app();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(mindplayer_core::limits::fetch(&home));
        });
        self.limits_rx = Some(rx);
    }

    /// Adopt a finished rate-limit fetch. True when the popup needs redrawing.
    pub fn poll_limits(&mut self) -> bool {
        let Some(rx) = &self.limits_rx else {
            return false;
        };
        let Ok(limits) = rx.try_recv() else {
            return false;
        };
        self.limits_rx = None;
        self.limits_started = None;
        self.limits = Some(limits);
        true
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
                    last_prompt_at: s.last_prompt_at,
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
                // A refresh pass only re-derives this from whatever tail
                // window it happened to scan; a `None` here means "no new
                // prompt observed this pass", not "there never was one" — so
                // it must never regress an already-known timestamp.
                if update.last_prompt_at.is_some() {
                    s.last_prompt_at = update.last_prompt_at;
                }
                s.tokens = update.tokens;
                s.context_pct = update.context_pct;
            }
        }
        let selected_id = self.selected_session().map(|s| s.id.clone());
        sort_by_recency(&mut self.all_sessions);
        self.rebuild_visible();
        if let Some(id) = selected_id {
            if let Some(pos) = self.row_of_session(&id) {
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
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::SessionClose);
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
            .and_then(|r| r.session_index())
            .and_then(|i| self.all_sessions.get(i))
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
            // Synthetic placeholder: drop the row, but the agent may still write
            // the rollout file for it. Un-queue the label so it can't be stamped
            // onto the next session in this dir, and keep the placeholder aside
            // so `reap_closed_extras` archives the file when it lands — either
            // one alone would let the closed session reappear.
            if let Some(label) = session.title.strip_prefix("🏷 ") {
                if self
                    .state
                    .remove_pending_label(session.agent.as_str(), &session.cwd, label)
                {
                    let _ = self.save_state();
                }
            }
            self.extra_sessions.retain(|s| s.id != session.id);
            self.all_sessions.retain(|s| s.id != session.id);
            self.closed_extras.push(session.clone());
            self.status = "closed new session".to_string();
        } else {
            self.state.set_archived(&session.id, true);
            let _ = self.save_state();
            if let Some(s) = self.all_sessions.iter_mut().find(|s| s.id == session.id) {
                s.archived = true;
            }
            self.status = format!("archived {}", short(&session.id));
        }
        self.rebuild_visible();
        // Restore the cursor onto the remembered neighbor by id.
        if let Some(nid) = neighbor_id {
            if let Some(pos) = self.row_of_session(&nid) {
                self.selected = pos;
            }
        }
    }
}
