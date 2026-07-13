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

impl App {
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

    pub fn confirm_handoff(&mut self, target: Agent) {
        let Some(source) = self.selected_session().cloned() else {
            self.handoff_picker = None;
            return;
        };
        self.handoff_picker = None;
        if source.agent == target {
            self.status = format!("handoff target is already {}", target.as_str());
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
        let command = handoff::command_for(&source, target);
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
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.all_sessions[i].id == id)
            {
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
        let _ = self.state.save();
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

    pub(crate) fn thread_sync_needed(&self, id: &str, peers: &[Session]) -> bool {
        if peers.is_empty() {
            return false;
        }
        // Sync once, the first time you resume back into the session — not on
        // every re-entry. Comparing against the peer's last-active timestamp
        // instead would keep re-triggering forever, since the source session
        // the user handed off from keeps advancing while they keep working in
        // it — which is exactly the repeated re-summary bug this guards against.
        !self.thread_sync_at.contains_key(id)
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
        self.thread_sync_at.insert(id, Utc::now());
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
}
