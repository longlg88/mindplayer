use super::*;

/// How long a new session closed before its rollout file existed keeps waiting
/// for that file. See `reap_closed_extras`.
const CLOSED_EXTRA_GRACE: chrono::Duration = chrono::Duration::minutes(5);

impl App {
    /// Open a pane that signs `account` in.
    ///
    /// It gets a row like any other pane so it is visible and closable, but it
    /// is never reconciled to a session — see `adopt_match`.
    pub fn request_login(&mut self, account: &mindplayer_core::accounts::Account) {
        let dir = self.account_recovery_cwd(account);
        let agent = account.provider;
        let command = mindplayer_core::login(agent, dir.clone(), account);
        self.open_login_pane(account, command, dir, "signing in");
    }

    /// Sign `account` out and straight back in, so a slot can change hands
    /// without signing out by hand first.
    ///
    /// Refused for the login this machine came with: clearing that one is not
    /// MindPlayer's to do.
    pub fn request_relogin(&mut self, account: &mindplayer_core::accounts::Account) {
        let dir = match &self.scope {
            Scope::WorkingDir(p) => p.clone(),
            Scope::Global => self.cwd.clone(),
        };
        match mindplayer_core::resume::relogin(dir.clone(), account) {
            Ok(command) => self.open_login_pane(account, command, dir, "signing in again"),
            Err(why) => self.status = why,
        }
    }

    fn open_login_pane(
        &mut self,
        account: &mindplayer_core::accounts::Account,
        command: mindplayer_core::Command,
        dir: PathBuf,
        verb: &str,
    ) {
        let agent = account.provider;
        let session_id = format!(
            "{}{}:{}",
            super::accounts_panel::LOGIN_PREFIX,
            agent.as_str(),
            account.name
        );
        let now = Utc::now();
        let synthetic = Session {
            id: session_id.clone(),
            agent,
            cwd: dir,
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            last_prompt_at: None,
            tokens: Default::default(),
            title: format!("🔑 sign in {} {}", agent.as_str(), account.name),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        // Re-opening a login for the same account reuses its row rather than
        // stacking a second one that looks like a second attempt.
        self.extra_sessions.retain(|s| s.id != session_id);
        self.all_sessions.retain(|s| s.id != session_id);
        self.extra_sessions.push(synthetic.clone());
        self.all_sessions.push(synthetic);
        self.rebuild_visible();
        self.pane_accounts
            .insert(session_id.clone(), (agent, account.name.clone()));
        self.track_account_login(account, &session_id);
        self.enqueue_spawn(PendingSpawn {
            command,
            session_id: session_id.clone(),
            initial_input: None,
            focus_after_spawn: true,
        });
        self.accounts_panel = None;
        self.focus_or_add_pane(&session_id);
        self.status = format!("{verb} {} {}", agent.as_str(), account.name);
    }

    /// Spawn a new Codex/Claude session in the current scope dir, optionally
    /// tagging the resulting session with a user label.
    pub fn request_new(&mut self, agent: Agent, label: &str) {
        self.request_new_on(&self.account_for(agent).clone(), label)
    }

    /// The same, on a named account rather than whichever one this provider
    /// currently starts on.
    pub fn request_new_on(&mut self, account: &mindplayer_core::accounts::Account, label: &str) {
        let dir = self.new_session_dir();
        let command = mindplayer_core::new_session(account.provider, dir, account);
        self.request_new_with(account, label, command);
    }

    /// A new session on a sign-in of its own, in a home made for it.
    ///
    /// Nothing is signed out and no other home is touched, so every pane
    /// already running keeps the account it had. The home is registered before
    /// the pane starts: discovery only looks where accounts live, and a session
    /// it cannot find is one the reconciler would lose.
    pub fn request_new_fresh_login(&mut self, agent: Agent, label: &str) {
        let Some(routing_mode) = self.startup_routing.mode() else {
            self.status = "시작바에서 이번 실행의 판단 방식을 먼저 선택하세요.".into();
            return;
        };
        #[cfg(not(test))]
        let home = super::limits_home_for_app();
        #[cfg(test)]
        let home = super::tests::fresh_login_test_home();
        let account =
            match mindplayer_core::accounts::reserve_fresh_account(&home, agent, &self.accounts) {
                Ok(account) => account,
                Err(e) => {
                    self.status = format!("could not make a home for this sign-in: {e}");
                    return;
                }
            };
        self.accounts.push(account.clone());
        self.persist_accounts();
        if matches!(agent, Agent::Codex | Agent::Claude) {
            let command = mindplayer_core::new_session(agent, self.new_session_dir(), &account);
            self.request_new_with(&account, label, command);
            if let Some(pending) = self.pending.take() {
                self.hold_account_launch(&account, pending);
            }
            self.request_login(&account);
            self.status = format!(
                "{} login opened for {}; 로그인 뒤 계정 확인을 마치면 요청한 세션을 이어 엽니다",
                agent.as_str(),
                account.name
            );
            return;
        }
        // Prepare only the conversation command; the provider's login still
        // runs directly before the bridge is started.
        let start = mindplayer_core::new_session(agent, self.new_session_dir(), &account);
        let start = match crate::local_router::prepare_for_mode(&start, routing_mode) {
            Ok(start) => start,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
        let command = mindplayer_core::resume::login_then_start_with(&account, start);
        self.request_new_with(&account, label, command);
        self.status = format!(
            "signing in a new {} login ({}) — the session starts once it finishes",
            agent.as_str(),
            account.name
        );
    }

    fn new_session_dir(&self) -> PathBuf {
        match &self.scope {
            Scope::WorkingDir(p) => p.clone(),
            Scope::Global => self.cwd.clone(),
        }
    }

    fn request_new_with(
        &mut self,
        account: &mindplayer_core::accounts::Account,
        label: &str,
        command: mindplayer_core::Command,
    ) {
        let agent = account.provider;
        let dir = self.new_session_dir();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::NewSession {
                agent: agent.as_str().to_string(),
            },
        );
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
        self.pane_accounts
            .insert(session_id.clone(), (agent, account.name.clone()));
        self.pending = Some(PendingSpawn {
            command,
            session_id: session_id.clone(),
            initial_input: None,
            focus_after_spawn: true,
        });
        self.new_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.new_account = None;
        self.new_fresh_login = false;
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
            last_prompt_at: None,
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
            if let Some(pos) = self.row_of_session(&id) {
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
            let _ = self.save_state();
        }
        // Pick the new session up in the list shortly after it's created.
        self.rescan_due = Some(Instant::now() + Duration::from_secs(3));
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
                    held_input: Vec::new(),
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

    /// Deliver `input` to `session`'s CLI: paste-and-submit if its PTY is
    /// live and idle, defer it if live but busy, or resume/spawn it with the
    /// input queued as the first turn if it has no PTY yet at all.
    pub(crate) fn enqueue_or_submit_to_session(
        &mut self,
        session: &Session,
        input: Vec<u8>,
    ) -> bool {
        if self.ended.contains(&session.id) {
            return false;
        }
        if self.ptys.contains_key(&session.id) {
            if self.ptys[&session.id].looks_idle() {
                if !self.dispatch_turn_input(&session.id, true, |p| p.paste_and_submit(&input)) {
                    self.status = format!("failed to submit input to {}", short(&session.id));
                    return false;
                }
                self.turn_submitted.insert(session.id.clone());
                self.submitted_at.insert(session.id.clone(), Instant::now());
                self.out_at.insert(session.id.clone(), Instant::now());
            } else {
                self.queue_initial_input(session.id.clone(), input);
            }
            return true;
        }
        if mindplayer_core::session::is_synthetic_id(&session.id) {
            return false;
        }
        self.enqueue_spawn(PendingSpawn {
            command: resume(session, &self.account_of_session(session)),
            session_id: session.id.clone(),
            initial_input: Some(input),
            focus_after_spawn: false,
        });
        true
    }

    /// Re-attach background-created sessions after a fresh scan: drop the
    /// synthetic placeholder once its real disk session appears (re-keying the
    /// live PTY to the real id), and re-append the ones still unmatched so they
    /// stay visible.
    pub(crate) fn merge_extras(&mut self) {
        // A live placeholder claims first. Both kinds match on agent, cwd and
        // "newer than me", so a closed one going first takes the session the
        // open pane just wrote and archives it — close a new session, start
        // another in the same directory within the grace window, and the second
        // one lands already archived. Claiming here re-keys the PTY onto the
        // real id, which is what then makes the reaper's live-PTY guard refuse
        // it.
        let taken = self.claim_live_extras();
        self.reap_closed_extras(&taken);
    }

    /// Let the open panes claim their own sessions, and report which ones they
    /// took so nothing else can.
    fn claim_live_extras(&mut self) -> HashSet<String> {
        if self.extra_sessions.is_empty() {
            return HashSet::new();
        }
        let mut claimed: HashSet<String> = HashSet::new();
        let mut remaining = Vec::new();
        for extra in std::mem::take(&mut self.extra_sessions) {
            let matched = self.adopt_match(&extra, &claimed);
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
                    if let Some(observer) = self.observers.remove(&extra.id) {
                        self.observers.insert(real_id.clone(), observer);
                    }
                    if let Some(input) = self.pending_initial_inputs.remove(&extra.id) {
                        self.pending_initial_inputs.insert(real_id.clone(), input);
                    }
                    if self.turn_submitted.remove(&extra.id) {
                        self.turn_submitted.insert(real_id.clone());
                    }
                    if let Some(at) = self.submitted_at.remove(&extra.id) {
                        self.submitted_at.insert(real_id.clone(), at);
                    }
                    if let Some(reading) = self.hook_status.remove(&extra.id) {
                        self.hook_status.insert(real_id.clone(), reading);
                    }
                    if let Some(reading) = self.hook_floor.remove(&extra.id) {
                        self.hook_floor.insert(real_id.clone(), reading);
                    }
                    let source = self
                        .hook_source_ids
                        .remove(&extra.id)
                        .unwrap_or_else(|| extra.id.clone());
                    self.hook_source_ids.insert(real_id.clone(), source);
                    if self.ptys.contains_key(&real_id) {
                        if let Some(s) = self.all_sessions.iter().find(|s| {
                            s.id == real_id
                                && s.agent == Agent::Codex
                                && !s.file.as_os_str().is_empty()
                        }) {
                            let reader = match self.synthetic_turn_cutoffs.remove(&extra.id) {
                                Some(cutoff) => {
                                    crate::turn_status::TurnStatusReader::new_from_start_since(
                                        s.file.clone(),
                                        cutoff,
                                    )
                                }
                                None => crate::turn_status::TurnStatusReader::new_from_start(
                                    s.file.clone(),
                                ),
                            };
                            self.codex_turns.insert(real_id.clone(), reader);
                        }
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
                        let _ = self.save_state();
                    }
                    self.new_baselines.remove(&extra.id);
                    if let Some(account) = self.pane_accounts.remove(&extra.id) {
                        self.pane_accounts.insert(real_id.clone(), account);
                    }
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
        claimed
    }

    /// The real disk session a synthetic new-session row should become, if it
    /// has appeared yet. `claimed` holds ids already taken this pass.
    fn adopt_match(&self, extra: &Session, claimed: &HashSet<String>) -> Option<String> {
        // Signing in writes no transcript, so a login pane has nothing of its
        // own to become. Without this it carries no baseline, and a pane with
        // no baseline adopts any new session of its agent and cwd — which
        // would be some other pane's work.
        if extra.id.starts_with(super::accounts_panel::LOGIN_PREFIX) {
            return None;
        }
        // A conversation waiting for authentication has not produced any
        // rollout yet. It must not adopt another process's same-directory work.
        if self.account_auth.owns_launch(&extra.id) {
            return None;
        }
        let after = extra
            .started_at
            .map(|t| t - chrono::Duration::seconds(30))
            .unwrap_or_else(Utc::now);
        let baseline = self.new_baselines.get(&extra.id);
        let ptys = &self.ptys;
        self.all_sessions
            .iter()
            .filter(|s| {
                !s.id.starts_with("new:")
                    && !s.id.starts_with("handoff:")
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
                    && self.pane_accounts.get(&extra.id).is_none_or(|(provider, name)| {
                        let owner = self.account_of_session(s);
                        owner.provider == *provider && owner.name == *name
                    })
                    && s.started_at.is_some_and(|t| t >= after)
            })
            .max_by_key(|s| s.started_at)
            .map(|s| s.id.clone())
    }

    /// Archive the disk sessions belonging to new sessions the user already
    /// closed. Without this the agent's late-written rollout file surfaces as a
    /// brand-new row, so a closed session comes back — under its old name, once
    /// the queued label lands on it.
    ///
    /// A closed session is only owed one file and the agent writes it within
    /// seconds of the first turn, so the wait is short: past
    /// `CLOSED_EXTRA_GRACE` the placeholder is dropped unmatched, because any
    /// session appearing that late is far more likely one the user started
    /// themselves in the same directory — and archiving that silently is worse
    /// than letting a stale row through.
    pub(crate) fn reap_closed_extras(&mut self, taken: &HashSet<String>) {
        if self.closed_extras.is_empty() {
            return;
        }
        let now = Utc::now();
        // Whatever an open pane just claimed is its own work, not a closed
        // pane's late file.
        let claimed = taken.clone();
        let mut still = Vec::new();
        let mut archived_any = false;
        for extra in std::mem::take(&mut self.closed_extras) {
            // Checked before matching: an expired placeholder must claim
            // nothing, or narrowing the window would buy no safety at all.
            if extra
                .started_at
                .is_some_and(|t| now.signed_duration_since(t) > CLOSED_EXTRA_GRACE)
            {
                self.new_baselines.remove(&extra.id);
                self.pane_accounts.remove(&extra.id);
                continue;
            }
            match self.adopt_match(&extra, &claimed) {
                Some(real_id) => {
                    self.state.set_archived(&real_id, true);
                    if let Some(s) = self.all_sessions.iter_mut().find(|s| s.id == real_id) {
                        s.archived = true;
                    }
                    self.new_baselines.remove(&extra.id);
                    archived_any = true;
                    self.pane_accounts.remove(&extra.id);
                }
                None => still.push(extra),
            }
        }
        self.closed_extras = still;
        if archived_any {
            let _ = self.save_state();
            self.rebuild_visible();
        }
    }

    /// Consume a pending spawn now that the pane size is known. Other sessions'
    /// PTYs are left running in the background.
    pub fn spawn_pending(&mut self) {
        let Some(routing_mode) = self.startup_routing.mode() else {
            self.status = "시작바에서 이번 실행의 판단 방식을 먼저 선택하세요.".into();
            return;
        };
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        let id = pending.session_id.clone();
        let account = self
            .all_sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| self.account_of_session(session));
        // All new/resume/handoff paths share a required asynchronous live
        // account check. Login itself is exempt, never conversation execution.
        if !id.starts_with(super::accounts_panel::LOGIN_PREFIX)
            && ([Agent::Codex.program(), Agent::Claude.program()]
                .contains(&pending.command.program.as_str())
                || account.as_ref().is_some_and(|account| {
                    matches!(account.provider, Agent::Codex | Agent::Claude)
                }))
        {
            let account = account
                .clone()
                .or_else(|| {
                    self.pane_accounts.get(&id).and_then(|(agent, name)| {
                        self.accounts
                            .iter()
                            .find(|a| a.provider == *agent && a.name == *name)
                            .cloned()
                    })
                })
                .unwrap_or_else(|| {
                    mindplayer_core::accounts::Account::inherited(
                        if pending.command.program == Agent::Claude.program() {
                            Agent::Claude
                        } else {
                            Agent::Codex
                        },
                    )
                });
            let Some(verified) = self.verify_account_launch(&account, pending) else {
                self.pending = self.pending_queue.pop_front();
                return;
            };
            pending = verified;
        }
        crate::local_router::forward_availability(
            &mut pending.command,
            account.as_ref(),
            self.limits.as_deref(),
            self.limits_observed_at,
        );
        // Replace only a previous (ended) PTY for this same id.
        if let Some(mut old) = self.ptys.remove(&id) {
            old.kill();
        }
        self.ended.remove(&id);
        // A retry starts clean: the previous failure must not shadow the new
        // pane if this attempt succeeds.
        self.clear_pane_error(&id);
        // Peer/category synchronization can finish while authentication is
        // pending. Preserve it until the verified PTY exists instead of
        // clearing it at the end of that (potentially long) wait.
        let waiting_input = self.pending_initial_inputs.remove(&id);
        self.out_seq.remove(&id);
        self.out_at.remove(&id);
        self.turn_submitted.remove(&id);
        self.submitted_at.remove(&id);
        self.hook_status.remove(&id);
        self.hook_floor.remove(&id);
        self.hook_source_ids.insert(id.clone(), id.clone());
        self.codex_turns.remove(&id);
        self.synthetic_turn_cutoffs.remove(&id);
        if let Some(reading) = crate::agent_hooks::read_hook_status(&id) {
            self.hook_floor.insert(id.clone(), reading);
        }
        if let Some(s) = self
            .all_sessions
            .iter()
            .find(|s| s.id == id && s.agent == Agent::Codex && !s.file.as_os_str().is_empty())
        {
            self.codex_turns.insert(
                id.clone(),
                crate::turn_status::TurnStatusReader::new(s.file.clone()),
            );
        }
        let (rows, cols) = self
            .pane_sizes
            .get(&id)
            .copied()
            .unwrap_or((self.pty_rows, self.pty_cols));
        match PtySession::spawn_with_routing(&pending.command, &id, rows, cols, routing_mode) {
            Ok(pty) => {
                if let Some(input) = pending.initial_input {
                    self.pending_initial_inputs.insert(
                        id.clone(),
                        DeferredInitialInput {
                            bytes: input,
                            queued_at: Instant::now(),
                            held_input: Vec::new(),
                        },
                    );
                }
                if let Some(waiting) = waiting_input {
                    self.queue_initial_input(id.clone(), waiting.bytes);
                    if let Some(input) = self.pending_initial_inputs.get_mut(&id) {
                        input.held_input.extend(waiting.held_input);
                    }
                }
                self.ptys.insert(id.clone(), pty);
                // The one choke point every new/resumed/handoff PTY passes
                // through — one log line here covers all of them.
                if let Some(session) = self.all_sessions.iter().find(|s| s.id == id) {
                    mindplayer_core::log_event_to(
                        &self.audit_path,
                        mindplayer_core::AuditEvent::SessionOpen {
                            agent: session.agent.as_str().to_string(),
                        },
                    );
                }
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
            let Some(input) = self.pending_initial_inputs.get(&id).cloned() else {
                continue;
            };
            if !self.ptys.contains_key(&id) {
                continue;
            }
            if !self.dispatch_turn_input(&id, true, |p| p.paste_and_submit(&input.bytes)) {
                self.status = format!("failed to submit initial context to {}", short(&id));
                continue;
            }
            // Replay whatever the user typed while this was held, now that it
            // can no longer land in the middle of the queued prompt.
            if !input.held_input.is_empty() {
                if let Some(pty) = self.ptys.get_mut(&id) {
                    pty.send(&input.held_input);
                }
            }
            self.pending_initial_inputs.remove(&id);
            self.turn_submitted.insert(id.clone());
            self.submitted_at.insert(id.clone(), Instant::now());
            self.out_at.insert(id.clone(), Instant::now());
            self.status = format!("submitted initial context to {}", short(&id));
            sent = true;
        }
        sent
    }

    /// If the focused pane still has a deferred initial-input (handoff /
    /// thread-sync prompt) waiting to go out, buffer these
    /// bytes on it instead of forwarding them to the child now — so typing
    /// right after opening such a session is held and replayed afterward
    /// (see `flush_initial_inputs`) rather than silently dropped.
    pub(crate) fn hold_for_pending_initial_input(&mut self, bytes: &[u8]) -> bool {
        let Some(id) = self.focused_pane().map(str::to_string) else {
            return false;
        };
        let Some(entry) = self.pending_initial_inputs.get_mut(&id) else {
            return false;
        };
        entry.held_input.extend_from_slice(bytes);
        self.status =
            "waiting for target prompt to submit initial context; input is held".to_string();
        true
    }
}
