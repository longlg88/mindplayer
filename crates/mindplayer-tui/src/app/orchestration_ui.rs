use super::orchestration_lanes::is_orchestration_main_session;
use super::*;

impl App {
    pub fn begin_orchestration(&mut self) {
        self.orchestration = Some(orchestration::Draft::default());
        self.status = "orchestration: choose provider".to_string();
    }

    pub fn cancel_orchestration(&mut self) {
        self.orchestration = None;
    }

    pub fn orchestration_input_push(&mut self, c: char) {
        let Some(draft) = self.orchestration.as_mut() else {
            return;
        };
        if draft.step == orchestration::Step::Provider {
            // Provider is navigated with ↑↓ only now (same control model as
            // the New Session / Handoff pickers) — plain chars are a no-op.
        } else if draft.step == orchestration::Step::Children {
            if c == '+' || c == '=' {
                draft.adjust_children(1);
            } else if c == '-' {
                draft.adjust_children(-1);
            } else {
                draft.set_children_digit(c);
            }
        } else {
            draft.push_char(c);
        }
    }

    pub fn orchestration_input_text(&mut self, text: &str) {
        if let Some(draft) = self.orchestration.as_mut() {
            draft.push_text(text);
        }
    }

    pub fn orchestration_input_backspace(&mut self) {
        if let Some(draft) = self.orchestration.as_mut() {
            draft.backspace();
        }
    }

    /// Up/Down means something different per step — move to the previous
    /// provider in the list, increase the child-lane count, or move the
    /// cursor up a line — never more than one of those for a single
    /// keypress. Each step keeps its own natural "up" direction rather than
    /// sharing one signed delta (list-nav "up" means the previous index,
    /// but a lane-count stepper's "up" means +1 — they aren't the same
    /// direction numerically).
    pub fn orchestration_up(&mut self) {
        let Some(draft) = self.orchestration.as_mut() else {
            return;
        };
        match draft.step {
            orchestration::Step::Provider => draft.adjust_provider(-1),
            orchestration::Step::Children => draft.adjust_children(1),
            orchestration::Step::Skill | orchestration::Step::Instruction => draft.move_up(),
        }
    }

    pub fn orchestration_down(&mut self) {
        let Some(draft) = self.orchestration.as_mut() else {
            return;
        };
        match draft.step {
            orchestration::Step::Provider => draft.adjust_provider(1),
            orchestration::Step::Children => draft.adjust_children(-1),
            orchestration::Step::Skill | orchestration::Step::Instruction => draft.move_down(),
        }
    }

    pub fn begin_broadcast(&mut self) {
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "broadcast needs an orchestration main/thread row".to_string();
            return;
        };
        let child_count = self.thread_child_ids(&root_id).len();
        if child_count == 0 {
            self.status = "broadcast needs child lanes".to_string();
            return;
        }
        self.broadcast = Some(orchestration::BroadcastDraft::default());
        self.status = format!("broadcast: enter instruction for {child_count} child lanes");
    }

    pub fn cancel_broadcast(&mut self) {
        self.broadcast = None;
    }

    pub fn begin_main_dispatch(&mut self) {
        self.repair_title_based_orchestration_links();
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "dispatch needs an orchestration main/thread row".to_string();
            return;
        };
        let child_count = self.thread_child_ids(&root_id).len();
        if child_count == 0 {
            self.status = "dispatch needs child lanes".to_string();
            return;
        }
        self.dispatch = Some(orchestration::BroadcastDraft::default());
        self.status = format!("dispatch: enter topic for main to route across {child_count} lanes");
    }

    pub fn cancel_main_dispatch(&mut self) {
        self.dispatch = None;
    }

    pub fn dispatch_input_text(&mut self, text: &str) {
        if let Some(draft) = self.dispatch.as_mut() {
            draft.push_text(text);
        }
    }

    pub fn dispatch_input_push(&mut self, c: char) {
        if let Some(draft) = self.dispatch.as_mut() {
            draft.push_char(c);
        }
    }

    pub fn dispatch_input_backspace(&mut self) {
        if let Some(draft) = self.dispatch.as_mut() {
            draft.backspace();
        }
    }

    pub fn confirm_main_dispatch(&mut self) {
        let Some(draft) = self.dispatch.take() else {
            return;
        };
        self.repair_title_based_orchestration_links();
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "dispatch failed: no orchestration root".to_string();
            return;
        };
        let Some(main) = self.all_sessions.iter().find(|s| s.id == root_id).cloned() else {
            self.status = "dispatch failed: no orchestration main session".to_string();
            return;
        };
        let child_ids = self.thread_child_ids(&root_id);
        if child_ids.is_empty() {
            self.status = "dispatch failed: no child lanes".to_string();
            return;
        }
        let roster = self.child_lane_roster(&root_id);
        let cycle = self.next_orchestration_cycle(&root_id);
        let input = orchestration::dispatch_request_prompt(&draft.instruction, cycle, &roster);
        if self.enqueue_or_submit_to_session(&main, input) {
            mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::Dispatch);
            self.thread_sync_at.insert(main.id.clone(), Utc::now());
            self.status = format!(
                "dispatch planning cycle #{cycle} sent to main; press M after main answers to apply"
            );
        } else {
            self.status = "dispatch failed: main lane is not resumable".to_string();
        }
    }

    pub fn begin_dispatch_apply_input(&mut self) {
        self.repair_title_based_orchestration_links();
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "dispatch apply needs an orchestration main/thread row".to_string();
            return;
        };
        if self.thread_child_ids(&root_id).is_empty() {
            self.status = "dispatch apply needs child lanes".to_string();
            return;
        }
        self.dispatch_apply = Some(orchestration::BroadcastDraft::default());
        self.status =
            "dispatch apply: paste MINDPLAYER_DISPATCH block, then press enter".to_string();
    }

    pub fn cancel_dispatch_apply(&mut self) {
        self.dispatch_apply = None;
    }

    pub fn dispatch_apply_input_text(&mut self, text: &str) {
        if let Some(draft) = self.dispatch_apply.as_mut() {
            draft.push_text(text);
        }
    }

    pub fn dispatch_apply_input_push(&mut self, c: char) {
        if let Some(draft) = self.dispatch_apply.as_mut() {
            draft.push_char(c);
        }
    }

    pub fn dispatch_apply_input_backspace(&mut self) {
        if let Some(draft) = self.dispatch_apply.as_mut() {
            draft.backspace();
        }
    }

    pub fn confirm_dispatch_apply_input(&mut self) {
        let Some(draft) = self.dispatch_apply.take() else {
            return;
        };
        let plan = orchestration::parse_dispatch_plan(&draft.instruction);
        if plan.is_empty() {
            self.status =
                "dispatch apply failed: pasted block has no lane instructions".to_string();
            return;
        }
        let Some(root_id) = self.best_dispatch_root_for_plan(&plan) else {
            self.status = "dispatch apply failed: no matching child lanes".to_string();
            return;
        };
        self.apply_dispatch_plan_to_root(&root_id, plan);
    }

    #[cfg(test)]
    pub fn apply_main_dispatch(&mut self) {
        self.repair_title_based_orchestration_links();
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "dispatch apply needs an orchestration main/thread row".to_string();
            return;
        };
        let Some(main) = self.all_sessions.iter().find(|s| s.id == root_id).cloned() else {
            self.status = "dispatch apply failed: no orchestration main session".to_string();
            return;
        };
        let Some((dispatch_root_id, plan)) = self.dispatch_plan_for_root(&root_id, &main) else {
            self.status = "dispatch apply failed: no MINDPLAYER_DISPATCH block".to_string();
            return;
        };
        self.apply_dispatch_plan_to_root(&dispatch_root_id, plan);
    }

    pub(crate) fn apply_dispatch_plan_to_root(
        &mut self,
        root_id: &str,
        plan: Vec<orchestration::DispatchItem>,
    ) {
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::Dispatch);
        let cycle = self.current_orchestration_cycle(root_id);
        let mut delivered = 0usize;
        let mut queued = 0usize;
        let mut spawned = 0usize;
        let mut skipped = 0usize;
        let mut skipped_lanes = Vec::new();
        for item in plan {
            let Some(child) = self.child_session_by_lane(root_id, item.lane) else {
                skipped += 1;
                skipped_lanes.push(format!("#{}", item.lane));
                continue;
            };
            let input = orchestration::dispatch_child_prompt(&item.instruction, cycle, item.lane);
            if self.enqueue_or_submit_to_session(&child, input) {
                if self.active.as_deref() == Some(child.id.as_str())
                    && self.ptys.contains_key(&child.id)
                {
                    delivered += 1;
                } else if self.pending_initial_inputs.contains_key(&child.id) {
                    queued += 1;
                } else {
                    spawned += 1;
                }
            } else {
                skipped += 1;
                skipped_lanes.push(format!("#{}", item.lane));
            }
        }
        let retry_note = if skipped_lanes.is_empty() {
            String::new()
        } else {
            format!("; skipped lanes {}", skipped_lanes.join(","))
        };
        self.status = format!(
            "dispatch applied cycle #{cycle}: {} lanes targeted ({delivered} sent, {queued} queued, {spawned} resumed, {skipped} skipped)",
            delivered + queued + spawned
        );
        self.status.push_str(&retry_note);
    }

    pub(crate) fn best_dispatch_root_for_plan(
        &self,
        plan: &[orchestration::DispatchItem],
    ) -> Option<String> {
        let selected = self.selected_session();
        let selected_root = selected.and_then(|s| self.orchestration_root_for_session(s));
        if selected_root
            .as_deref()
            .is_some_and(|root| self.dispatch_root_matches_plan(root, plan))
        {
            return selected_root;
        }
        let selected_cwd = selected.map(|s| s.cwd.clone());
        let selected_agent = selected.map(|s| s.agent);
        self.all_sessions
            .iter()
            .filter(|s| {
                is_orchestration_main_session(s)
                    && selected_cwd.as_ref().is_none_or(|cwd| s.cwd == *cwd)
                    && selected_agent.is_none_or(|agent| s.agent == agent)
                    && self.dispatch_root_matches_plan(&s.id, plan)
            })
            .max_by_key(|s| s.last_active)
            .map(|s| s.id.clone())
    }

    pub(crate) fn dispatch_root_matches_plan(
        &self,
        root_id: &str,
        plan: &[orchestration::DispatchItem],
    ) -> bool {
        plan.iter()
            .any(|item| self.child_session_by_lane(root_id, item.lane).is_some())
    }

    #[cfg(test)]
    pub(crate) fn dispatch_plan_for_root(
        &self,
        root_id: &str,
        main: &Session,
    ) -> Option<(String, Vec<orchestration::DispatchItem>)> {
        let mut root_ids = vec![root_id.to_string()];
        root_ids.extend(
            self.all_sessions
                .iter()
                .filter(|s| {
                    s.id != root_id
                        && s.cwd == main.cwd
                        && s.agent == main.agent
                        && is_orchestration_main_session(s)
                        && !self.thread_child_ids(&s.id).is_empty()
                })
                .map(|s| s.id.clone()),
        );
        root_ids.sort();
        root_ids.dedup();
        root_ids.sort_by_key(|id| {
            std::cmp::Reverse(
                self.all_sessions
                    .iter()
                    .find(|s| s.id == *id)
                    .and_then(|s| s.last_active),
            )
        });
        if let Some(pos) = root_ids.iter().position(|id| id == root_id) {
            let selected = root_ids.remove(pos);
            root_ids.insert(0, selected);
        }

        for candidate_id in root_ids {
            let Some(candidate) = self.all_sessions.iter().find(|s| s.id == candidate_id) else {
                continue;
            };
            if let Some(screen) = self.live_screen_text(&candidate.id) {
                let plan = orchestration::parse_dispatch_plan(&screen);
                if !plan.is_empty() {
                    return Some((candidate.id.clone(), plan));
                }
            }
            if let Ok(transcript) = handoff::extract_session_transcript(candidate) {
                let plan = orchestration::parse_dispatch_plan(&transcript);
                if !plan.is_empty() {
                    return Some((candidate.id.clone(), plan));
                }
            }
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn live_screen_text(&self, id: &str) -> Option<String> {
        let pty = self.ptys.get(id)?;
        let parser = pty.parser().lock().ok()?;
        Some(parser.screen().contents())
    }

    pub fn broadcast_input_text(&mut self, text: &str) {
        if let Some(draft) = self.broadcast.as_mut() {
            draft.push_text(text);
        }
    }

    pub fn broadcast_input_push(&mut self, c: char) {
        if let Some(draft) = self.broadcast.as_mut() {
            draft.push_char(c);
        }
    }

    pub fn broadcast_input_backspace(&mut self) {
        if let Some(draft) = self.broadcast.as_mut() {
            draft.backspace();
        }
    }

    pub fn confirm_broadcast(&mut self) {
        let Some(draft) = self.broadcast.take() else {
            return;
        };
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "broadcast failed: no orchestration root".to_string();
            return;
        };
        let child_ids = self.thread_child_ids(&root_id);
        if child_ids.is_empty() {
            self.status = "broadcast failed: no child lanes".to_string();
            return;
        }
        let cycle = self.next_orchestration_cycle(&root_id);
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::Broadcast {
                children: child_ids.len(),
            },
        );
        let mut delivered = 0usize;
        let mut queued = 0usize;
        let mut spawned = 0usize;
        let mut skipped = 0usize;
        for child_id in child_ids {
            let Some(child) = self.all_sessions.iter().find(|s| s.id == child_id).cloned() else {
                skipped += 1;
                continue;
            };
            let input = orchestration::broadcast_prompt(&draft.instruction, cycle);
            if self.enqueue_or_submit_to_session(&child, input) {
                if self.ptys.contains_key(&child.id) {
                    if self.pending_initial_inputs.contains_key(&child.id) {
                        queued += 1;
                    } else {
                        delivered += 1;
                    }
                } else {
                    spawned += 1;
                }
            } else {
                skipped += 1;
            }
        }
        self.status = format!(
            "cycle #{cycle} broadcasted to {} child lanes ({delivered} sent, {queued} queued, {spawned} resumed, {skipped} skipped)",
            delivered + queued + spawned
        );
    }

    pub fn run_peer_review_cycle(&mut self) {
        self.repair_title_based_orchestration_links();
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "peer review needs an orchestration main/thread row".to_string();
            return;
        };
        let child_ids = self.thread_child_ids(&root_id);
        if child_ids.is_empty() {
            self.status = "peer review needs child lanes".to_string();
            return;
        }
        let cycle = self.next_orchestration_cycle(&root_id);
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::PeerReview);
        let mut delivered = 0usize;
        let mut queued = 0usize;
        let mut spawned = 0usize;
        let mut skipped = 0usize;
        let mut skipped_ids = Vec::new();
        for child_id in child_ids {
            let Some(child) = self.all_sessions.iter().find(|s| s.id == child_id).cloned() else {
                skipped += 1;
                skipped_ids.push(short(&child_id));
                continue;
            };
            let peers = self.thread_peer_sessions(&child.id);
            let command = orchestration::peer_review_prompt(cycle);
            let Ok(sync) = handoff::prepare_thread_command_input(&child, &peers, command) else {
                skipped += 1;
                skipped_ids.push(short(&child.id));
                continue;
            };
            if self.enqueue_or_submit_to_session(&child, sync.input) {
                self.thread_sync_at.insert(child.id.clone(), Utc::now());
                if self.ptys.contains_key(&child.id) {
                    if self.pending_initial_inputs.contains_key(&child.id) {
                        queued += 1;
                    } else {
                        delivered += 1;
                    }
                } else {
                    spawned += 1;
                }
            } else {
                skipped += 1;
                skipped_ids.push(short(&child.id));
            }
        }
        let retry_note = if skipped_ids.is_empty() {
            String::new()
        } else {
            format!("; skipped {}: press p to retry", skipped_ids.join(","))
        };
        self.status = format!(
            "peer review cycle #{cycle} sent to {} child lanes ({delivered} sent, {queued} queued, {spawned} resumed, {skipped} skipped)",
            delivered + queued + spawned
        );
        self.status.push_str(&retry_note);
    }

    pub fn run_synthesis_cycle(&mut self) {
        self.repair_title_based_orchestration_links();
        let Some(root_id) = self.selected_orchestration_root() else {
            self.status = "synthesis needs an orchestration main/thread row".to_string();
            return;
        };
        if self.thread_child_ids(&root_id).is_empty() {
            self.status = "synthesis needs child lanes".to_string();
            return;
        }
        self.pending_synthesis_root = Some(root_id.clone());
        if self.try_run_pending_synthesis() {
            return;
        }
        let waiting = self.waiting_child_count(&root_id);
        self.status = format!("synthesis waiting for {waiting} child lanes to become idle");
    }

    pub fn poll_pending_synthesis(&mut self) -> bool {
        self.try_run_pending_synthesis()
    }

    pub(crate) fn try_run_pending_synthesis(&mut self) -> bool {
        let Some(root_id) = self.pending_synthesis_root.clone() else {
            return false;
        };
        if self.waiting_child_count(&root_id) > 0 {
            return false;
        }
        self.pending_synthesis_root = None;
        self.send_synthesis_cycle_now(&root_id);
        true
    }

    pub(crate) fn send_synthesis_cycle_now(&mut self, root_id: &str) {
        let Some(main) = self.all_sessions.iter().find(|s| s.id == root_id).cloned() else {
            self.status = "synthesis failed: no orchestration main session".to_string();
            return;
        };
        let peers = self.thread_peer_sessions(root_id);
        if peers.is_empty() {
            self.status = "synthesis needs child lanes".to_string();
            return;
        }
        let cycle = self.current_orchestration_cycle(root_id);
        let command = orchestration::synthesis_prompt(cycle);
        let Ok(sync) = handoff::prepare_thread_command_input(&main, &peers, command) else {
            self.status = "synthesis failed: no readable child lane context".to_string();
            return;
        };
        if self.enqueue_or_submit_to_session(&main, sync.input) {
            mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::Synthesis);
            self.thread_sync_at.insert(main.id.clone(), Utc::now());
            self.status = format!(
                "synthesis cycle #{cycle} sent to main ({} chars, {})",
                sync.transcript_chars,
                sync.artifact.display()
            );
        } else {
            self.status = "synthesis failed: main lane is not resumable".to_string();
        }
    }

    pub fn confirm_orchestration_step(&mut self) {
        let Some(draft) = self.orchestration.as_mut() else {
            return;
        };
        if draft.advance() {
            let draft = self.orchestration.take().unwrap_or_default();
            self.start_orchestration(draft);
        }
    }

    // --- cross-agent handoff -----------------------------------------------

    pub(crate) fn enqueue_or_submit_to_session(
        &mut self,
        session: &Session,
        input: Vec<u8>,
    ) -> bool {
        if self.ended.contains(&session.id) {
            return false;
        }
        if let Some(pty) = self.ptys.get_mut(&session.id) {
            if pty.looks_idle() {
                if !pty.paste_and_submit(&input) {
                    self.status = format!("failed to submit input to {}", short(&session.id));
                    return false;
                }
                self.turn_submitted.insert(session.id.clone());
                self.out_at.insert(session.id.clone(), Instant::now());
            } else {
                self.queue_initial_input(session.id.clone(), input);
            }
            return true;
        }
        if session.id.starts_with("new:")
            || session.id.starts_with("handoff:")
            || session.id.starts_with("orch:")
        {
            return false;
        }
        self.enqueue_spawn(PendingSpawn {
            command: resume(session),
            session_id: session.id.clone(),
            initial_input: Some(input),
            focus_after_spawn: false,
        });
        true
    }
}
