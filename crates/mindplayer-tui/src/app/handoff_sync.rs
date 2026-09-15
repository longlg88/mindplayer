use super::*;

pub(crate) fn handoff_label(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty() {
        None
    } else if label.starts_with("(handoff)") {
        Some(label.to_string())
    } else {
        Some(format!("(handoff){label}"))
    }
}

/// Read each peer from its watermark, dropping the ones with nothing new. An
/// empty result is the normal quiet case: it is what makes re-entering a session
/// silent instead of a repeated briefing.
pub(crate) fn deltas_for(marks: Vec<(Session, u64)>) -> Vec<handoff::PeerDelta> {
    marks
        .into_iter()
        .filter_map(|(peer, mark)| handoff::extract_transcript_delta(&peer, mark))
        .collect()
}

impl App {
    /// Every login a handoff can land on, grouped by provider in a fixed
    /// order.
    ///
    /// Cursor holds no second account yet but is still a target, so it
    /// contributes the login this machine already had.
    pub(crate) fn handoff_targets(&self) -> Vec<mindplayer_core::accounts::Account> {
        use mindplayer_core::accounts::{Account, MULTI_ACCOUNT_AGENTS};
        let mut out = Vec::new();
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            if MULTI_ACCOUNT_AGENTS.contains(&agent) {
                out.extend(
                    self.accounts
                        .iter()
                        .filter(|a| a.provider == agent && !a.disabled)
                        .cloned(),
                );
            } else {
                out.push(Account::inherited(agent));
            }
        }
        out
    }

    pub fn begin_handoff(&mut self) {
        if self.selected_session().is_none() {
            return;
        }
        self.handoff_picker = Some(0);
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::HandoffBegin);
        self.status = "handoff: choose target agent".to_string();
    }

    pub fn cancel_handoff(&mut self) {
        if self.handoff_picker.take().is_some() {
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::HandoffCancel,
            );
        }
    }

    /// Hand the selected session's context to a new one on `account`.
    ///
    /// The account matters as much as the provider here: a session cannot be
    /// resumed on an account whose home never held it, so handing it over is
    /// the only way to carry a conversation to another login.
    pub fn confirm_handoff_on(&mut self, account: &mindplayer_core::accounts::Account) {
        let target = account.provider;
        let Some(source) = self.selected_session().cloned() else {
            self.handoff_picker = None;
            return;
        };
        self.handoff_picker = None;
        // Same provider is fine as long as it is a different login — that is
        // what carrying a conversation to another account means. Only landing
        // back where it started is refused.
        if source.agent == target && self.account_of_session(&source).name == account.name {
            self.status = format!(
                "this session is already on {} {}",
                target.as_str(),
                account.name
            );
            return;
        }

        let prepared = match handoff::prepare_initial_input(&source, target) {
            Ok(prepared) => prepared,
            Err(e) => {
                self.status = format!("handoff failed: {e}");
                return;
            }
        };
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::Handoff);
        let command = handoff::command_for(&source, target, account);
        let parent_id = self.state.thread_root(&source.id).to_string();
        let now = Utc::now();
        self.new_counter += 1;
        let session_id = format!(
            "handoff:{}:{}:{}",
            source.agent.as_str(),
            target.as_str(),
            self.new_counter
        );
        let baseline: HashSet<String> = self
            .all_sessions
            .iter()
            .filter(|s| !s.id.starts_with("new:") && !s.id.starts_with("handoff:"))
            .map(|s| s.id.clone())
            .collect();
        self.new_baselines.insert(session_id.clone(), baseline);
        let handoff_label = self.state.label_for(&source.id).and_then(handoff_label);
        self.state
            .set_handoff_link(&session_id, &parent_id, prepared.artifact.clone(), now);
        let initial_input = if target == Agent::Kiro {
            // Kiro accepts the first question as a positional `chat [INPUT]`
            // argument. Passing handoff context there is more reliable than
            // racing startup and pasting into the interactive prompt.
            let mut prompt = prepared.input.clone();
            trim_submit(&mut prompt);
            let mut command = command;
            command
                .args
                .push(String::from_utf8_lossy(&prompt).into_owned());
            self.pending = Some(PendingSpawn {
                command,
                session_id: session_id.clone(),
                initial_input: None,
                focus_after_spawn: true,
            });
            false
        } else {
            self.pending = Some(PendingSpawn {
                command,
                session_id: session_id.clone(),
                initial_input: Some(prepared.input),
                focus_after_spawn: true,
            });
            true
        };
        self.focus_or_add_pane(&session_id);

        let synthetic = Session {
            id: session_id,
            agent: target,
            cwd: source.cwd.clone(),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            last_prompt_at: None,
            tokens: Default::default(),
            title: handoff_label
                .as_ref()
                .map(|label| format!("🏷 {label}"))
                .unwrap_or_else(|| handoff::title_for(&source, target)),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        self.extra_sessions.push(synthetic.clone());
        self.all_sessions.push(synthetic);
        self.rebuild_visible();
        if let Some(id) = self.active.clone() {
            if let Some(pos) = self.row_of_session(&id) {
                self.selected = pos;
            }
        }
        let trunc = if prepared.inline_truncated {
            "artifact only"
        } else {
            "full inline"
        };
        if let Some(label) = &handoff_label {
            self.state.add_pending_label(
                target.as_str(),
                source.cwd.clone(),
                now - chrono::Duration::seconds(5),
                label,
            );
        }
        self.state.add_pending_handoff(
            &parent_id,
            target.as_str(),
            source.cwd.clone(),
            now - chrono::Duration::seconds(5),
            prepared.artifact.clone(),
        );
        let _ = self.save_state();
        let delivery = if initial_input {
            "queued initial paste"
        } else {
            "sent as first input"
        };
        self.status = format!(
            "handoff {} -> {} ({} chars, {trunc}, {delivery}, {})",
            source.agent.as_str(),
            target.as_str(),
            prepared.transcript_chars,
            prepared.artifact.display()
        );
        self.rescan_due = Some(Instant::now() + Duration::from_secs(3));
    }

    // --- session search ----------------------------------------------------

    pub(crate) fn thread_peer_sessions(&self, id: &str) -> Vec<Session> {
        let root = self.state.thread_root(id).to_string();
        self.all_sessions
            .iter()
            .filter(|s| s.id != id && self.state.thread_root(&s.id) == root)
            .cloned()
            .collect()
    }

    /// Peers for a *category* sync: everything in the same category, which
    /// unlike `thread_peer_sessions` needs no handoff link between them. A
    /// session created with `n` and dropped into a category has no lineage at
    /// all, which is exactly why lineage-only peering never fired for it.
    ///
    /// Handoff lanes come along too: a lane's category is resolved through its
    /// thread root, so a thread inside a category contributes every lane.
    pub(crate) fn category_peer_sessions(&self, id: &str) -> Vec<Session> {
        let Some(cat) = self.category_of_session(id) else {
            return Vec::new();
        };
        self.all_sessions
            .iter()
            .filter(|s| s.id != id && self.category_of_session(&s.id).as_deref() == Some(&cat))
            .cloned()
            .collect()
    }

    /// A session's category, following its handoff root when the session itself
    /// carries none — the same rule the list uses to place a lane under a topic.
    pub(crate) fn category_of_session(&self, id: &str) -> Option<String> {
        if let Some(c) = self.state.category_of(id) {
            return Some(c.to_string());
        }
        let root = self.state.thread_root(id);
        self.state.category_of(root).map(str::to_string)
    }

    /// Per-peer deltas, same call the worker thread makes. Test-only because
    /// production reaches it through [`deltas_for`] on the worker (this needs
    /// `&self`, which cannot cross the thread boundary) — it delegates rather
    /// than reimplementing, so the two can never drift.
    #[cfg(test)]
    pub(crate) fn category_deltas(&self, id: &str) -> Vec<handoff::PeerDelta> {
        let marks = self
            .category_peer_sessions(id)
            .into_iter()
            .map(|p| {
                let m = self.state.sync_mark(id, &p.id);
                (p, m)
            })
            .collect();
        deltas_for(marks)
    }

    pub(crate) fn thread_sync_needed(&self, id: &str, peers: &[Session]) -> bool {
        if peers.is_empty() {
            return false;
        }
        // Sync once, the first time you resume back into the session — not on
        // every re-entry. Comparing against the peer's last-active timestamp
        // instead would keep re-triggering forever, since the source session
        // the user handed off from keeps advancing while they keep working in
        // it — which is exactly the repeated re-summary bug this guards against.
        //
        // Checked against BOTH the in-memory `thread_sync_at` (covers a reopen
        // within this same run) and the persisted `state.thread_synced` (covers
        // a reopen after quitting and restarting MindPlayer entirely) — the
        // in-memory map alone is wiped on every restart, so without the
        // persisted half the very next resume after a restart looked like a
        // fresh reopen and handed off the same content again.
        !self.thread_sync_at.contains_key(id) && !self.state.thread_synced.contains(id)
    }

    /// Non-blocking read of a session's peer-lane thread-sync context (see
    /// `spawn_thread_sync_for`/`poll_thread_sync`). The "is a sync even
    /// needed" check runs here on the main thread (cheap — no file I/O),
    /// but the actual peer-transcript read/parse (`extract_transcript`, up to
    /// `MAX_SOURCE_BYTES` per peer) happens on a background thread. Reading
    /// several peer lanes' transcripts synchronously used to freeze the whole
    /// UI — both input and rendering — for as long as the read took, every
    /// time a thread-synced session was reopened. Returns `false` if no sync
    /// is needed (nothing was spawned) so the caller can fall through to its
    /// normal resume path immediately instead of waiting on a channel that
    /// will never produce anything.
    pub(crate) fn spawn_thread_sync_for(&mut self, session: &Session) -> bool {
        if self.thread_sync_rx.is_some() {
            // A previous sync is still in flight; don't start a second one
            // (poll_thread_sync drops it once resolved).
            return false;
        }
        let peers = self.thread_peer_sessions(&session.id);
        if !self.thread_sync_needed(&session.id, &peers) {
            return false;
        }
        let target = session.clone();
        let id = session.id.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = handoff::prepare_thread_sync_input(&target, &peers);
            let _ = tx.send((target.id, result));
        });
        self.thread_sync_rx = Some(rx);
        // Mark it needed-and-in-flight immediately so a second reopen before
        // this one resolves doesn't queue a duplicate read for the same id.
        self.thread_sync_at.insert(id.clone(), Utc::now());
        // Persist the same marker so it survives a MindPlayer restart (see the
        // doc comment on `thread_sync_needed` and on `State::thread_synced`).
        self.state.thread_synced.insert(id);
        let _ = self.save_state();
        true
    }

    /// Apply a finished background thread-sync read (see
    /// `spawn_thread_sync_for`). If the target session is still live and idle,
    /// paste-and-submit it directly like the old synchronous path did;
    /// otherwise queue it as a deferred initial input so it goes out via the
    /// normal `flush_initial_inputs` path once the prompt is ready. Returns
    /// true if anything changed (caller redraws).
    pub fn poll_thread_sync(&mut self) -> bool {
        let Some(rx) = &self.thread_sync_rx else {
            return false;
        };
        let Ok((id, result)) = rx.try_recv() else {
            return false;
        };
        self.thread_sync_rx = None;
        let Ok(sync) = result else {
            return false;
        };
        if self.ended.contains(&id) {
            return false;
        }
        let injected = self.ptys.get_mut(&id).is_some_and(|pty| {
            if pty.looks_idle() {
                pty.paste_and_submit(&sync.input)
            } else {
                false
            }
        });
        if injected {
            self.turn_submitted.insert(id.clone());
            self.status = format!(
                "synced peer lanes into {} ({} chars, {})",
                short(&id),
                sync.transcript_chars,
                sync.artifact.display()
            );
        } else {
            // Either live but not idle right now, or not spawned yet (a fresh
            // resume's PTY is still pending on pane-size). Either way, hand it
            // to the same deferred-input path a fresh spawn's initial prompt
            // uses, so it still goes out (via `flush_initial_inputs`) once the
            // target prompt is ready instead of being silently dropped.
            self.queue_initial_input(id.clone(), sync.input);
            self.status = format!("peer-lane sync for {} queued", short(&id));
        }
        true
    }

    /// Start a category sync for `session`. `force` bypasses the category's
    /// auto-sync toggle (that is what `sync now` does). Returns false when
    /// there is nothing to start, so callers fall straight through.
    ///
    /// Unlike thread-sync there is no once-ever guard: the watermarks make a
    /// repeat call with no peer activity produce zero deltas, so it is naturally
    /// silent instead of needing to be blocked.
    pub(crate) fn spawn_category_sync_for(&mut self, session: &Session, force: bool) -> bool {
        if self.category_sync_rx.is_some() {
            return false; // one in flight; poll_category_sync clears it
        }
        let Some(cat) = self.category_of_session(&session.id) else {
            return false;
        };
        if !force && !self.state.category_auto_sync(&cat) {
            return false;
        }
        let peers = self.category_peer_sessions(&session.id);
        if peers.is_empty() {
            return false;
        }
        // Snapshot (peer, watermark) pairs on the main thread; the file reads
        // themselves happen on the worker.
        let marks: Vec<(Session, u64)> = peers
            .into_iter()
            .map(|p| {
                let m = self.state.sync_mark(&session.id, &p.id);
                (p, m)
            })
            .collect();
        let target = session.clone();
        let cat_name = self.category_label(&cat);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let deltas = deltas_for(marks);
            let result = if deltas.is_empty() {
                Err("no peer lane has anything new".to_string())
            } else {
                let advance: Vec<(String, u64)> = deltas
                    .iter()
                    .map(|d| (d.session.id.clone(), d.new_len))
                    .collect();
                handoff::prepare_category_sync_input(&target, &cat_name, &deltas)
                    .map(|prep| (prep, advance))
            };
            let _ = tx.send((target.id, result));
        });
        self.category_sync_rx = Some(rx);
        true
    }

    /// Apply a finished category sync. Watermarks only advance once the prompt
    /// has actually been handed over (submitted, or queued for the deferred
    /// path) — advancing on a dropped sync would silently lose that peer
    /// context forever.
    pub fn poll_category_sync(&mut self) -> bool {
        let Some(rx) = &self.category_sync_rx else {
            return false;
        };
        let Ok((id, result)) = rx.try_recv() else {
            return false;
        };
        self.category_sync_rx = None;
        let Ok((sync, advance)) = result else {
            return false; // nothing new, or unreadable peers
        };
        if self.ended.contains(&id) {
            return false;
        }
        let injected = self.ptys.get_mut(&id).is_some_and(|pty| {
            if pty.looks_idle() {
                pty.paste_and_submit(&sync.input)
            } else {
                false
            }
        });
        if injected {
            self.turn_submitted.insert(id.clone());
            self.status = format!(
                "category sync into {} ({} chars)",
                short(&id),
                sync.transcript_chars
            );
        } else {
            // Live but mid-turn, or the PTY is still pending on pane size. The
            // deferred path sends it once the prompt is ready rather than
            // dropping it, so a busy target is delayed, never skipped.
            self.queue_initial_input(id.clone(), sync.input);
            self.status = format!("category sync for {} queued", short(&id));
        }
        for (peer_id, new_len) in advance {
            self.state.set_sync_mark(&id, &peer_id, new_len);
        }
        let _ = self.save_state();
        true
    }
}
