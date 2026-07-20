use super::*;
use std::path::Path;
use std::time::SystemTime;
use walkdir::WalkDir;

/// How often the per-pane `.html`-candidate directory walk runs. A few seconds
/// is responsive enough to notice a file the agent just wrote without turning
/// into a hot loop of filesystem walks while a pane sits open.
const HTML_CANDIDATE_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Keep the candidate walk shallow: it's meant to catch a file the agent just
/// wrote *near* where it's working, not to index an entire repo.
const HTML_WALK_MAX_DEPTH: usize = 3;
/// Directory names never descended into during the candidate walk — heavy
/// vendor/build/VCS trees that can hold thousands of files and would make a
/// periodic recursive scan a real performance/battery problem.
const HTML_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "__pycache__",
];

impl App {
    /// The session id currently shown in the right pane, if it has a PTY.
    pub fn active_pty(&self) -> Option<&PtySession> {
        self.active.as_ref().and_then(|id| self.ptys.get(id))
    }

    /// The PTY currently DISPLAYED for `id`: its backgrounded carbonyl preview
    /// when the pane is showing a preview, otherwise its agent PTY. This is the
    /// one that should be rendered, resized, and fed input for that pane.
    pub fn displayed_pty(&self, id: &str) -> Option<&PtySession> {
        if self.previewing.contains(id) {
            self.preview_ptys.get(id)
        } else {
            self.ptys.get(id)
        }
    }

    pub(crate) fn displayed_pty_mut(&mut self, id: &str) -> Option<&mut PtySession> {
        if self.previewing.contains(id) {
            self.preview_ptys.get_mut(id)
        } else {
            self.ptys.get_mut(id)
        }
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
        // A preview (carbonyl) process must never outlive its pane: kill and
        // forget it whenever the pane is actually removed. Toggling back to the
        // agent view does NOT go through here (it only drops the id from
        // `previewing`), so a mere toggle never kills the process.
        if let Some(mut preview) = self.preview_ptys.remove(sid) {
            preview.kill();
        }
        self.previewing.remove(sid);
        // Per-pane HTML-candidate detection state must not outlive the pane
        // (mirrors the preview cleanup above); the interval gate is global, so
        // there's nothing pane-scoped to clear for it.
        self.html_candidates.remove(sid);
        self.html_seen.remove(sid);
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

    pub fn cycle_layout(&mut self) {
        self.layout = match self.layout {
            PaneLayout::Single | PaneLayout::Vertical => PaneLayout::Horizontal,
            PaneLayout::Horizontal => PaneLayout::Vertical,
        };
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::LayoutCycle {
                layout: layout_label(self.layout).to_string(),
            },
        );
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
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::SessionResume {
                id: session.id.clone(),
            },
        );
        self.focus = Focus::Terminal;
        if self.is_running(&session.id) {
            // Already live in the background — just bring it to the
            // foreground now; any peer-lane sync reads happen off the main
            // thread (see `spawn_thread_sync_for`/`poll_thread_sync`) so
            // reopening a session with several handoff peers never freezes
            // the UI while their transcripts are read.
            self.spawn_thread_sync_for(&session);
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
        // Same off-main-thread treatment for a fresh spawn: kick off the
        // background sync read (if one is needed) and spawn immediately
        // without it. `poll_thread_sync` queues the result as a deferred
        // initial input once it's ready, same as a handoff's own initial
        // prompt already does.
        self.spawn_thread_sync_for(&session);
        self.pending = Some(PendingSpawn {
            command: resume(&session),
            session_id: session.id.clone(),
            initial_input: None,
            focus_after_spawn: true,
        });
        self.focus_or_add_pane(&session.id);
        self.status = format!("resuming {} {}", session.agent.as_str(), short(&session.id));
    }

    /// `Ctrl-T` from a live pane: open the one-line "topic / RUNBOOK §n /
    /// files" input. Works the same with one pane or several open — it only
    /// ever targets whichever pane is currently focused, never every pane
    /// (unlike broadcast).
    pub fn begin_transition_report(&mut self) {
        if self.focused_pane().is_none() {
            self.status = "transition report needs a live pane".to_string();
            return;
        }
        self.transition_report_input = Some(String::new());
        self.status = "transition report: topic / RUNBOOK §n / files, then enter".to_string();
    }

    pub fn cancel_transition_report(&mut self) {
        self.transition_report_input = None;
    }

    pub fn transition_report_input_push(&mut self, c: char) {
        if let Some(s) = self.transition_report_input.as_mut() {
            s.push(c);
        }
    }

    pub fn transition_report_input_backspace(&mut self) {
        if let Some(s) = self.transition_report_input.as_mut() {
            s.pop();
        }
    }

    /// Enter on the one-line input: assemble the real prompt (template +
    /// typed specifics) and show it read-only rather than sending it —
    /// `send_transition_report_review` / `begin_editing_transition_report_review`
    /// take it from here. Nothing is sent to the session yet.
    pub fn confirm_transition_report_input(&mut self) {
        let Some(input) = self.transition_report_input.take() else {
            return;
        };
        if self.focused_pane().is_none() {
            self.status = "transition report failed: no focused pane".to_string();
            return;
        }
        let assembled = transition_report_prompt(&self.prompts_dir, &input);
        let mut draft = text_input::BroadcastDraft::default();
        draft.push_text(&assembled);
        self.transition_report_review = Some(draft);
        self.transition_report_review_editing = false;
        self.status = "transition report: enter send · e edit · esc cancel".to_string();
    }

    pub fn cancel_transition_report_review(&mut self) {
        self.transition_report_review = None;
        self.transition_report_review_editing = false;
    }

    pub fn begin_editing_transition_report_review(&mut self) {
        if self.transition_report_review.is_some() {
            self.transition_report_review_editing = true;
        }
    }

    /// Enter on the review screen (read-only or mid-edit): send whatever
    /// text is currently in the buffer — the untouched assembled prompt if
    /// the user never pressed `e`, or their edited version otherwise.
    pub fn send_transition_report_review(&mut self) {
        let Some(draft) = self.transition_report_review.take() else {
            return;
        };
        self.transition_report_review_editing = false;
        let Some(id) = self.focused_pane().map(str::to_string) else {
            self.status = "transition report failed: no focused pane".to_string();
            return;
        };
        let Some(session) = self.all_sessions.iter().find(|s| s.id == id).cloned() else {
            self.status = "transition report failed: session no longer tracked".to_string();
            return;
        };
        let mut prompt = draft.instruction;
        prompt.push('\r');
        if self.enqueue_or_submit_to_session(&session, prompt.into_bytes()) {
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::TransitionReportSent,
            );
            self.status = format!("transition report prompt sent to {}", short(&session.id));
        } else {
            self.status = format!("transition report failed to send to {}", short(&session.id));
        }
    }

    pub fn transition_report_review_push_char(&mut self, c: char) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.push_char(c);
        }
    }

    pub fn transition_report_review_push_text(&mut self, text: &str) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.push_text(text);
        }
    }

    pub fn transition_report_review_backspace(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.backspace();
        }
    }

    pub fn transition_report_review_delete(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.delete();
        }
    }

    pub fn transition_report_review_move_left(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.move_left();
        }
    }

    pub fn transition_report_review_move_right(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.move_right();
        }
    }

    pub fn transition_report_review_move_up(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.move_up();
        }
    }

    pub fn transition_report_review_move_down(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.move_down();
        }
    }

    pub fn transition_report_review_move_home(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.move_home();
        }
    }

    pub fn transition_report_review_move_end(&mut self) {
        if let Some(draft) = self.transition_report_review.as_mut() {
            draft.move_end();
        }
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
        let ids: Vec<String> = self
            .visible
            .iter()
            .filter_map(|&i| self.all_sessions.get(i))
            .filter(|s| self.marked.contains(&s.id))
            .map(|s| s.id.clone())
            .take(MAX_PANES)
            .collect();
        let total = ids.len();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::LaunchMarked {
                ids: ids.clone(),
                count: total,
                zoom_was_on,
            },
        );
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
                let (r, c) = (self.pty_rows, self.pty_cols);
                if let Some(pty) = self.displayed_pty_mut(&id) {
                    pty.resize(r, c);
                }
            }
            return;
        }
        // Resize whichever PTY (agent or preview) is currently displayed for
        // each pane, so a preview pane tracks pane-grid changes exactly like an
        // agent pane. A backgrounded (hidden) preview or agent is intentionally
        // left at its current size until it's shown again.
        for (id, (rows, cols)) in targets {
            if let Some(pty) = self.displayed_pty_mut(&id) {
                pty.resize(rows, cols);
            }
        }
    }

    pub fn detach_terminal(&mut self) {
        self.selection = None;
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
            .filter_map(|id| self.displayed_pty(id))
            .any(|p| p.take_dirty())
    }

    /// Forward encoded keystrokes to the displayed PTY.
    pub fn send_to_pty(&mut self, bytes: &[u8]) {
        // Typing dismisses any drag-copy highlight, like a normal terminal.
        self.selection = None;
        // While previewing, keystrokes drive carbonyl (the browser), not the
        // agent — so its own turn/hold bookkeeping is skipped entirely.
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if self.previewing.contains(&id) {
                if let Some(pty) = self.preview_ptys.get_mut(&id) {
                    pty.send(bytes);
                }
                return;
            }
        }
        if self.hold_for_pending_initial_input(bytes) {
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
        // A preview pane pastes into carbonyl, bypassing the agent's hold/turn
        // bookkeeping (same reasoning as `send_to_pty`).
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if self.previewing.contains(&id) {
                return self
                    .preview_ptys
                    .get_mut(&id)
                    .is_some_and(|pty| pty.paste(text));
            }
        }
        if self.hold_for_pending_initial_input(text.as_bytes()) {
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

    /// The detected `.html` candidates for the focused pane, most-recent-first
    /// (empty when there are none). Used by the Ctrl-P picker's key handling and
    /// its renderer so both read the same list.
    pub fn html_candidates_for_focused(&self) -> &[PathBuf] {
        self.focused_pane()
            .and_then(|id| self.html_candidates.get(id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Periodically walk each live pane's cwd for newly-written `.html` files and
    /// refresh [`Self::html_candidates`]. Interval-gated (see
    /// [`HTML_CANDIDATE_POLL_INTERVAL`]) so it never runs every `run()` tick.
    /// Only panes that still exist AND aren't already previewing are scanned —
    /// there's nothing to "notice" for a pane already showing a preview. A file
    /// already in [`Self::html_seen`] stays suppressed until its mtime advances
    /// past when it was seen. Returns true if any pane's candidate list changed
    /// (needs a redraw to update the badge).
    pub fn poll_html_candidates(&mut self) -> bool {
        match self.html_candidates_due {
            Some(at) if Instant::now() < at => return false,
            _ => {}
        }
        self.html_candidates_due = Some(Instant::now() + HTML_CANDIDATE_POLL_INTERVAL);

        let mut changed = false;
        let ids: Vec<String> = self
            .panes
            .iter()
            .filter(|id| !self.previewing.contains(*id))
            .cloned()
            .collect();
        for id in ids {
            let Some(cwd) = self
                .all_sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.cwd.clone())
            else {
                continue;
            };
            let found = scan_html_candidates(&cwd);
            let candidates: Vec<PathBuf> = {
                let seen = self.html_seen.get(&id);
                found
                    .into_iter()
                    .filter(|(path, mtime)| {
                        seen.and_then(|m| m.get(path))
                            .is_none_or(|seen_mtime| mtime > seen_mtime)
                    })
                    .map(|(path, _)| path)
                    .collect()
            };
            let new_val = if candidates.is_empty() {
                None
            } else {
                Some(candidates)
            };
            if self.html_candidates.get(&id).map(Vec::as_slice) != new_val.as_deref() {
                changed = true;
                match new_val {
                    Some(v) => {
                        self.html_candidates.insert(id.clone(), v);
                    }
                    None => {
                        self.html_candidates.remove(&id);
                    }
                }
            }
        }
        // Drop candidate lists for panes that no longer exist or have since
        // started previewing (owned set, so `self` isn't double-borrowed).
        let valid: HashSet<String> = self
            .panes
            .iter()
            .filter(|id| !self.previewing.contains(*id))
            .cloned()
            .collect();
        let before = self.html_candidates.len();
        self.html_candidates.retain(|id, _| valid.contains(id));
        if self.html_candidates.len() != before {
            changed = true;
        }
        changed
    }
}

/// Walk `cwd` (shallowly, skipping heavy vendor/build/VCS dirs) for `.html`
/// files, returning `(path, mtime)` sorted most-recently-modified first. Pulled
/// out of `App` so the scan itself can be unit-tested against a real temp dir
/// without constructing panes/PTYs.
pub(crate) fn scan_html_candidates(cwd: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut out: Vec<(PathBuf, SystemTime)> = Vec::new();
    let walker = WalkDir::new(cwd)
        .max_depth(HTML_WALK_MAX_DEPTH)
        .into_iter()
        .filter_entry(|entry| {
            // Always descend the root; below it, never enter a skip-listed
            // directory (by name, at any depth). Non-directories always pass so
            // their `.html`-ness is judged below.
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                return true;
            }
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !HTML_SKIP_DIRS.contains(&name))
        });
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        if let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) {
            out.push((path.to_path_buf(), mtime));
        }
    }
    // Most-recently-modified first.
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// Default content for `~/.mindplayer/prompts/transition_report.md` —
/// seeded there on first use so it can be rewritten at any time without a
/// rebuild. Handed to *any* provider (codex/claude/kiro — it's plain text,
/// nothing provider-specific) by `Ctrl-T`. The bracketed placeholders are
/// intentionally left for the agent itself to fill in from whatever the
/// user typed in `input` — mindplayer never parses "topic / §n / files"
/// itself, since splitting on "/" would break the moment a file path
/// contains one. `{{input}}` marks where that typed text is substituted in;
/// if a rewritten template drops the token, it's appended instead so
/// nothing the user typed is silently lost.
const DEFAULT_TRANSITION_REPORT_PROMPT: &str = "\
transition-<주제>.html을 _assets/transition-template.html 구조로 만들어줘.
소스: [RUNBOOK.md §n] + [실제 대상 파일들 경로].
TL;DR/Topology/검증/Glossary는 항상, 핵심판단·wiring토글·steps는
실제로 그 구조(2단 비교/현재-목표/순서)가 있을 때만 써줘.

이번 건 세부사항: {{input}}";

fn transition_report_prompt(prompts_dir: &std::path::Path, input: &str) -> String {
    let template = mindplayer_core::load_prompt_from(
        prompts_dir,
        "transition_report",
        DEFAULT_TRANSITION_REPORT_PROMPT,
    );
    if template.contains("{{input}}") {
        template.replace("{{input}}", input)
    } else {
        format!("{template}\n\n이번 건 세부사항: {input}")
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
