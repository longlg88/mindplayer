use super::*;

impl App {
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
        self.focus = Focus::Terminal;
        self.sync_active();
    }

    pub(crate) fn remove_pane(&mut self, sid: &str) {
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
        self.status = format!("focused pane {}/{}", self.focused + 1, self.panes.len());
    }

    pub fn cycle_layout(&mut self) {
        self.layout = match self.layout {
            PaneLayout::Single | PaneLayout::Vertical => PaneLayout::Horizontal,
            PaneLayout::Horizontal => PaneLayout::Vertical,
        };
        self.status = match self.layout {
            PaneLayout::Horizontal => "live panes split horizontally".to_string(),
            PaneLayout::Vertical => "live panes split vertically".to_string(),
            PaneLayout::Single => "single live pane".to_string(),
        };
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
                if should_stamp_activity(self.turn_submitted.contains(id), pty.looks_busy()) {
                    self.out_at.insert(id.clone(), now);
                }
                changed = true;
            }
        }
        self.out_seq
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.out_at
            .retain(|id, _| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.turn_submitted
            .retain(|id| self.ptys.contains_key(id) && !self.ended.contains(id));
        self.thread_sync_at
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

    /// Status of session `id` for the list badge.
    pub fn session_status(&self, id: &str) -> SessionStatus {
        if self.ended.contains(id) {
            SessionStatus::Ended
        } else if let Some(pty) = self.ptys.get(id) {
            classify_live_session_status(
                pty.looks_blocked(),
                pty.looks_idle(),
                pty.looks_busy(),
                self.out_at.get(id).copied(),
                Instant::now(),
            )
        } else {
            SessionStatus::Inactive
        }
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
        let reordered = bubble_urgent_to_front(&self.panes, |id| {
            self.session_status(id) == SessionStatus::Blocked
        });
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
    pub fn request_resume(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        self.focus = Focus::Terminal;
        if self.is_running(&session.id) {
            // Already live in the background — just bring it to the foreground.
            if let Some(sync) = self.prepare_thread_sync_for(&session) {
                let injected = self.ptys.get_mut(&session.id).is_some_and(|pty| {
                    if pty.looks_idle() {
                        pty.paste_and_submit(&sync.input)
                    } else {
                        false
                    }
                });
                if injected {
                    self.thread_sync_at.insert(session.id.clone(), Utc::now());
                    self.turn_submitted.insert(session.id.clone());
                    self.status = format!(
                        "synced peer lanes into {} ({} chars, {})",
                        short(&session.id),
                        sync.transcript_chars,
                        sync.artifact.display()
                    );
                }
            }
            self.focus_or_add_pane(&session.id);
            return;
        }
        // A synthetic new-session has no real id to `resume`; just show its
        // (possibly ended) pane if it still exists, otherwise stay on the list.
        if session.id.starts_with("new:") {
            if self.ptys.contains_key(&session.id) {
                self.focus_or_add_pane(&session.id);
            } else {
                self.focus = Focus::List;
            }
            return;
        }
        let initial_input = self.prepare_thread_sync_for(&session).map(|sync| {
            self.thread_sync_at.insert(session.id.clone(), Utc::now());
            self.status = format!(
                "resuming {} {} with thread sync ({} chars, {})",
                session.agent.as_str(),
                short(&session.id),
                sync.transcript_chars,
                sync.artifact.display()
            );
            sync.input
        });
        self.pending = Some(PendingSpawn {
            command: resume(&session),
            session_id: session.id.clone(),
            initial_input,
            focus_after_spawn: true,
        });
        self.focus_or_add_pane(&session.id);
        if !self.status.contains("thread sync") {
            self.status = format!("resuming {} {}", session.agent.as_str(), short(&session.id));
        }
    }

    /// Toggle the multi-select mark on the currently-selected session, then
    /// advance the cursor so several sessions can be marked in quick succession.
    pub fn toggle_mark(&mut self) {
        let Some(id) = self.selected_session().map(|s| s.id.clone()) else {
            return;
        };
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
        let marked = self.marked.len();
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
        self.status = "multi-select cancelled".to_string();
    }

    /// Launch every marked session as a live pane in one go. Falls back to the
    /// single-session resume when nothing is marked. Marked ids are taken in
    /// visible order (capped at MAX_PANES) so the pane order is predictable.
    /// Clears the marks and leaves multi-select mode afterward.
    pub fn launch_marked(&mut self) {
        self.multi_select = false;
        if self.marked.is_empty() {
            self.request_resume();
            return;
        }
        let ids: Vec<String> = self
            .visible
            .iter()
            .filter_map(|&i| self.all_sessions.get(i))
            .filter(|s| self.marked.contains(&s.id))
            .map(|s| s.id.clone())
            .take(MAX_PANES)
            .collect();
        let total = ids.len();
        self.focus = Focus::Terminal;
        for id in &ids {
            let Some(session) = self.all_sessions.iter().find(|s| &s.id == id).cloned() else {
                continue;
            };
            if self.is_running(&session.id) {
                self.focus_or_add_pane(&session.id);
                continue;
            }
            // Synthetic new-sessions have no real id to resume; only show an
            // already-spawned pane if one survives.
            if session.id.starts_with("new:") {
                if self.ptys.contains_key(&session.id) {
                    self.focus_or_add_pane(&session.id);
                }
                continue;
            }
            self.enqueue_spawn(PendingSpawn {
                command: resume(&session),
                session_id: session.id.clone(),
                initial_input: None,
                focus_after_spawn: true,
            });
            self.focus_or_add_pane(&session.id);
        }
        self.marked.clear();
        self.status = format!("launched {total} sessions into live panes");
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
                if let Some(pty) = self.ptys.get_mut(&id) {
                    pty.resize(self.pty_rows, self.pty_cols);
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
        self.focus = Focus::List;
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
            self.ended.insert(id);
        }
        true
    }

    /// True (resetting) if the displayed PTY produced new output.
    pub fn pty_dirty(&self) -> bool {
        if self.panes.is_empty() {
            return self.active_pty().is_some_and(|p| p.take_dirty());
        }
        self.panes
            .iter()
            .filter_map(|id| self.ptys.get(id))
            .any(|p| p.take_dirty())
    }

    /// Forward encoded keystrokes to the displayed PTY.
    pub fn send_to_pty(&mut self, bytes: &[u8]) {
        // Typing dismisses any drag-copy highlight, like a normal terminal.
        self.selection = None;
        if self.active_initial_input_pending() {
            return;
        }
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if let Some(pty) = self.ptys.get_mut(&id) {
                if pty.send(bytes) && input_submits_turn(bytes) {
                    self.turn_submitted.insert(id);
                }
            }
        }
    }

    /// Forward pasted text to the displayed PTY (only when a live session has
    /// focus). Returns true if it was delivered (caller redraws). Pastes go
    /// nowhere useful from the list, so they're ignored there.
    pub fn paste_to_pty(&mut self, text: &str) -> bool {
        if self.focus != Focus::Terminal {
            return false;
        }
        if self.active_initial_input_pending() {
            return true;
        }
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if let Some(pty) = self.ptys.get_mut(&id) {
                if !pty.paste(text) {
                    return false;
                }
                if input_submits_turn(text.as_bytes()) {
                    self.turn_submitted.insert(id);
                }
                return true;
            }
        }
        false
    }
}

/// Stable-partitions `ids` into "urgent first, everyone else after," keeping
/// each group's original relative order. Pulled out of `App` so the sort
/// itself can be tested without a real `PtySession` (see the `pane` test
/// module for the App-level focus-preservation smoke test).
fn bubble_urgent_to_front(ids: &[String], mut is_urgent: impl FnMut(&str) -> bool) -> Vec<String> {
    let mut ranked: Vec<(u8, usize, &String)> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (u8::from(!is_urgent(id)), i, id))
        .collect();
    ranked.sort_by_key(|(rank, i, _)| (*rank, *i));
    ranked.into_iter().map(|(_, _, id)| id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::bubble_urgent_to_front;

    #[test]
    fn bubble_urgent_to_front_preserves_relative_order_within_each_group() {
        let ids: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let out = bubble_urgent_to_front(&ids, |id| id == "c");
        assert_eq!(out, vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn bubble_urgent_to_front_is_a_no_op_when_nothing_is_urgent() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let out = bubble_urgent_to_front(&ids, |_| false);
        assert_eq!(out, ids);
    }
}
