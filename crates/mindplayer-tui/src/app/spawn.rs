use super::orchestration_lanes::orchestration_title_marker;
use super::*;

impl App {
    /// Spawn a new Codex/Claude session in the current scope dir, optionally
    /// tagging the resulting session with a user label.
    pub fn request_new(&mut self, agent: Agent, label: &str) {
        let dir = match &self.scope {
            Scope::WorkingDir(p) => p.clone(),
            Scope::Global => self.cwd.clone(),
        };
        let command = mindplayer_core::new_session(agent, dir.clone());
        // Synthetic, unique id so it never collides with a real session or a
        // previous new session of the same agent.
        self.new_counter += 1;
        let session_id = format!("new:{}:{}", agent.as_str(), self.new_counter);
        // Snapshot the real sessions that already exist, so reconciliation can
        // only ever attach this new session's PTY to a genuinely-new disk
        // session — never to one that was already present (or freshly resumed).
        let baseline: HashSet<String> = self
            .all_sessions
            .iter()
            .filter(|s| !s.id.starts_with("new:"))
            .map(|s| s.id.clone())
            .collect();
        self.new_baselines.insert(session_id.clone(), baseline);
        self.pending = Some(PendingSpawn {
            command,
            session_id: session_id.clone(),
            initial_input: None,
            focus_after_spawn: true,
        });
        self.new_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.focus_or_add_pane(&session_id);

        let label = label.trim();
        let now = Utc::now();
        let title = if label.is_empty() {
            format!("(new {} session)", agent.as_str())
        } else {
            format!("🏷 {label}")
        };
        // Show the new session in the list immediately so it never disappears,
        // even before codex/claude writes its rollout file. Reconciled to the
        // real session (and its PTY re-keyed) once that file appears.
        let synthetic = Session {
            id: session_id,
            agent,
            cwd: dir.clone(),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            tokens: Default::default(),
            title,
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        self.extra_sessions.push(synthetic.clone());
        self.all_sessions.push(synthetic);
        self.rebuild_visible();
        // The synthetic row is Inactive, so the urgency sort sinks it down the
        // list. Keep the cursor on it by id so returning to the list and
        // pressing `x` can't archive+kill a different session.
        if let Some(id) = self.active.clone() {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
                self.selected = pos;
            }
        }

        if label.is_empty() {
            self.status = format!("new {} session", agent.as_str());
        } else {
            self.status = format!("new {} session: {label}", agent.as_str());
            // Persisted: codex/claude only write the rollout file after the
            // first interaction, so the label is matched on a later scan (and
            // survives restarts). A small margin absorbs clock skew.
            self.state.add_pending_label(
                agent.as_str(),
                dir,
                now - chrono::Duration::seconds(5),
                label,
            );
            let _ = self.state.save();
        }
        // Pick the new session up in the list shortly after it's created.
        self.rescan_due = Some(Instant::now() + Duration::from_secs(3));
    }

    pub(crate) fn real_session_ids(&self) -> HashSet<String> {
        self.all_sessions
            .iter()
            .filter(|s| {
                !s.id.starts_with("new:")
                    && !s.id.starts_with("handoff:")
                    && !s.id.starts_with("orch:")
            })
            .map(|s| s.id.clone())
            .collect()
    }

    pub(crate) fn queue_initial_input(&mut self, id: String, input: Vec<u8>) {
        let now = Instant::now();
        if let Some(existing) = self.pending_initial_inputs.get_mut(&id) {
            trim_submit(&mut existing.bytes);
            existing.bytes.extend_from_slice(b"\n\n---\n\n");
            existing.bytes.extend(input);
            existing.queued_at = now;
        } else {
            self.pending_initial_inputs.insert(
                id,
                DeferredInitialInput {
                    bytes: input,
                    queued_at: now,
                },
            );
        }
    }

    pub(crate) fn enqueue_spawn(&mut self, spawn: PendingSpawn) {
        if self.pending.is_none() {
            self.pending = Some(spawn);
        } else {
            self.pending_queue.push_back(spawn);
        }
    }

    pub(crate) fn push_synthetic_session(&mut self, session: Session) {
        self.extra_sessions.push(session.clone());
        self.all_sessions.push(session);
        self.rebuild_visible();
    }

    pub(crate) fn select_session_id(&mut self, id: &str) {
        if let Some(pos) = self
            .visible
            .iter()
            .position(|&i| self.all_sessions[i].id == id)
        {
            self.selected = pos;
        }
    }

    /// Re-attach background-created sessions after a fresh scan: drop the
    /// synthetic placeholder once its real disk session appears (re-keying the
    /// live PTY to the real id), and re-append the ones still unmatched so they
    /// stay visible.
    pub(crate) fn merge_extras(&mut self) {
        if self.extra_sessions.is_empty() {
            return;
        }
        let mut claimed: HashSet<String> = HashSet::new();
        let mut remaining = Vec::new();
        for extra in std::mem::take(&mut self.extra_sessions) {
            let after = extra
                .started_at
                .map(|t| t - chrono::Duration::seconds(30))
                .unwrap_or_else(Utc::now);
            let baseline = self.new_baselines.get(&extra.id);
            let ptys = &self.ptys;
            let matched = self.orchestration_real_match(&extra, &claimed).or_else(|| {
                self.all_sessions
                    .iter()
                    .filter(|s| {
                        !s.id.starts_with("new:")
                                && !s.id.starts_with("handoff:")
                                && !s.id.starts_with("orch:")
                                && !claimed.contains(&s.id)
                                // Never re-key onto a session that already owns a live
                                // PTY (e.g. one the user resumed) — that would drop the
                                // displaced PtySession and silently SIGKILL its child.
                                && !ptys.contains_key(&s.id)
                                // Only adopt a session that did NOT exist when this new
                                // session was created — i.e. the one codex/claude just
                                // wrote — never to a pre-existing same-dir/same-agent one.
                                && baseline.is_none_or(|b| !b.contains(&s.id))
                                && s.agent == extra.agent
                                && s.cwd == extra.cwd
                                && s.started_at.is_some_and(|t| t >= after)
                    })
                    .max_by_key(|s| s.started_at)
                    .map(|s| s.id.clone())
            });
            match matched {
                Some(real_id) => {
                    // Move the live PTY / state from the synthetic id to the real
                    // one. The filter guarantees `real_id` is not already a live
                    // PTY, so this insert never clobbers a running session.
                    if let Some(real) = self.all_sessions.iter_mut().find(|s| s.id == real_id) {
                        real.title = extra.title.clone();
                    }
                    if let Some(label) = extra.title.strip_prefix("🏷 ") {
                        self.state.set_label(&real_id, label);
                    }
                    if let Some(pty) = self.ptys.remove(&extra.id) {
                        self.ptys.insert(real_id.clone(), pty);
                    }
                    if let Some(input) = self.pending_initial_inputs.remove(&extra.id) {
                        self.pending_initial_inputs.insert(real_id.clone(), input);
                    }
                    if self.turn_submitted.remove(&extra.id) {
                        self.turn_submitted.insert(real_id.clone());
                    }
                    if let Some(synced_at) = self.thread_sync_at.remove(&extra.id) {
                        self.thread_sync_at.insert(real_id.clone(), synced_at);
                    }
                    if self.ended.remove(&extra.id) {
                        self.ended.insert(real_id.clone());
                    }
                    for pane in &mut self.panes {
                        if *pane == extra.id {
                            *pane = real_id.clone();
                        }
                    }
                    if let Some(size) = self.pane_sizes.remove(&extra.id) {
                        self.pane_sizes.insert(real_id.clone(), size);
                    }
                    if self.active.as_deref() == Some(extra.id.as_str()) {
                        self.active = Some(real_id.clone());
                    }
                    if let Some(cycle) = self.orchestration_cycles.remove(&extra.id) {
                        self.orchestration_cycles.insert(real_id.clone(), cycle);
                    }
                    for link in self.state.handoff_links.values_mut() {
                        if link.parent_id == extra.id {
                            link.parent_id = real_id.clone();
                        }
                    }
                    for pending in &mut self.state.pending_handoffs {
                        if pending.parent_id == extra.id {
                            pending.parent_id = real_id.clone();
                        }
                    }
                    if let Some(link) = self.state.handoff_links.remove(&extra.id) {
                        let parent_id = link.parent_id.clone();
                        self.state.handoff_links.insert(real_id.clone(), link);
                        self.state.pending_handoffs.retain(|p| {
                            !(p.agent == extra.agent.as_str()
                                && p.cwd == extra.cwd
                                && p.parent_id == parent_id)
                        });
                        let _ = self.state.save();
                    }
                    self.new_baselines.remove(&extra.id);
                    claimed.insert(real_id);
                    // The real session is already in `all_sessions`; drop the extra.
                }
                None => {
                    self.all_sessions.push(extra.clone());
                    remaining.push(extra);
                }
            }
        }
        self.extra_sessions = remaining;
    }

    pub(crate) fn orchestration_real_match(
        &self,
        extra: &Session,
        claimed: &HashSet<String>,
    ) -> Option<String> {
        let marker = orchestration_title_marker(&extra.id)?;
        self.all_sessions
            .iter()
            .filter(|s| {
                !s.id.starts_with("new:")
                    && !s.id.starts_with("handoff:")
                    && !s.id.starts_with("orch:")
                    && !claimed.contains(&s.id)
                    && !self.ptys.contains_key(&s.id)
                    && s.agent == extra.agent
                    && s.cwd == extra.cwd
                    && s.title.contains(&marker)
            })
            .max_by_key(|s| s.started_at)
            .map(|s| s.id.clone())
    }

    /// Consume a pending spawn now that the pane size is known. Other sessions'
    /// PTYs are left running in the background.
    pub fn spawn_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let id = pending.session_id.clone();
        // Replace only a previous (ended) PTY for this same id.
        if let Some(mut old) = self.ptys.remove(&id) {
            old.kill();
        }
        self.ended.remove(&id);
        self.pending_initial_inputs.remove(&id);
        self.out_seq.remove(&id);
        self.out_at.remove(&id);
        self.turn_submitted.remove(&id);
        let (rows, cols) = self
            .pane_sizes
            .get(&id)
            .copied()
            .unwrap_or((self.pty_rows, self.pty_cols));
        match PtySession::spawn(&pending.command, &id, rows, cols) {
            Ok(pty) => {
                if let Some(input) = pending.initial_input {
                    self.pending_initial_inputs.insert(
                        id.clone(),
                        DeferredInitialInput {
                            bytes: input,
                            queued_at: Instant::now(),
                        },
                    );
                }
                self.ptys.insert(id.clone(), pty);
                if pending.focus_after_spawn {
                    self.focus_or_add_pane(&id);
                }
            }
            Err(e) => {
                self.status = format!("failed to start {}: {e}", pending.command.program);
                if pending.focus_after_spawn {
                    self.focus = Focus::List;
                    self.remove_pane(&id);
                }
            }
        }
        self.pending = self.pending_queue.pop_front();
    }

    /// Submit queued first-turn prompts only after the child has rendered an
    /// input prompt. Sending immediately after spawn can race the agent TUI
    /// startup and lose the handoff prompt before it reaches the transcript.
    pub fn flush_initial_inputs(&mut self) -> bool {
        if self.pending_initial_inputs.is_empty() {
            return false;
        }
        let now = Instant::now();
        let ready: Vec<String> = self
            .pending_initial_inputs
            .iter()
            .filter_map(|(id, input)| {
                let pty = self.ptys.get(id)?;
                if self.ended.contains(id) {
                    return None;
                }
                should_send_initial_input(
                    pty.looks_idle(),
                    pty.output_seq(),
                    now.saturating_duration_since(input.queued_at),
                )
                .then(|| id.clone())
            })
            .collect();
        let mut sent = false;
        for id in ready {
            let Some(input) = self.pending_initial_inputs.get(&id) else {
                continue;
            };
            if let Some(pty) = self.ptys.get_mut(&id) {
                if !pty.paste_and_submit(&input.bytes) {
                    self.status = format!("failed to submit initial context to {}", short(&id));
                    continue;
                }
                self.pending_initial_inputs.remove(&id);
                self.turn_submitted.insert(id.clone());
                self.out_at.insert(id.clone(), Instant::now());
                self.status = format!("submitted initial context to {}", short(&id));
                sent = true;
            }
        }
        sent
    }

    pub(crate) fn active_initial_input_pending(&mut self) -> bool {
        let Some(id) = self.focused_pane().map(str::to_string) else {
            return false;
        };
        if !self.pending_initial_inputs.contains_key(&id) {
            return false;
        }
        self.status =
            "waiting for target prompt to submit initial context; input is held".to_string();
        true
    }
}
