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
        self.status = "handoff: choose target agent".to_string();
    }

    pub fn cancel_handoff(&mut self) {
        self.handoff_picker = None;
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
        if let Some(session) = self.all_sessions.iter().find(|s| s.id == id) {
            if let Some(root_id) = self.orchestration_root_for_session(session) {
                let mut peer_ids = self.thread_child_ids(&root_id);
                if id != root_id {
                    peer_ids.push(root_id);
                }
                peer_ids.sort();
                peer_ids.dedup();
                return peer_ids
                    .into_iter()
                    .filter(|peer_id| peer_id != id)
                    .filter_map(|peer_id| {
                        self.all_sessions.iter().find(|s| s.id == peer_id).cloned()
                    })
                    .collect();
            }
        }
        let root = self.state.thread_root(id).to_string();
        let linked = self
            .all_sessions
            .iter()
            .filter(|s| s.id != id && self.state.thread_root(&s.id) == root)
            .cloned()
            .collect::<Vec<_>>();
        if !linked.is_empty() {
            return linked;
        }
        let Some(session) = self.all_sessions.iter().find(|s| s.id == id) else {
            return Vec::new();
        };
        let Some(root_id) = self.orchestration_fallback_root(session) else {
            return Vec::new();
        };
        let mut peer_ids = self.fallback_child_ids(&root_id);
        peer_ids.push(root_id);
        peer_ids.sort();
        peer_ids.dedup();
        peer_ids
            .into_iter()
            .filter(|peer_id| peer_id != id)
            .filter_map(|peer_id| self.all_sessions.iter().find(|s| s.id == peer_id).cloned())
            .collect()
    }

    pub(crate) fn thread_sync_needed(&self, id: &str, peers: &[Session]) -> bool {
        if peers.is_empty() {
            return false;
        }
        // Sync once, the first time you resume back into the session — not on
        // every re-entry. This only ever fires for a plain 1:1 handoff (an
        // orchestration lane's peers always resolve through
        // `orchestration_root_for_session`, which `prepare_thread_sync_for`
        // already skips). Comparing against the peer's last-active timestamp
        // instead would keep re-triggering forever, since the source session
        // the user handed off from keeps advancing while they keep working in
        // it — which is exactly the repeated re-summary bug this guards against.
        !self.thread_sync_at.contains_key(id)
    }

    pub(crate) fn prepare_thread_sync_for(
        &self,
        session: &Session,
    ) -> Option<handoff::PreparedHandoff> {
        if self.orchestration_root_for_session(session).is_some() {
            return None;
        }
        let peers = self.thread_peer_sessions(&session.id);
        if !self.thread_sync_needed(&session.id, &peers) {
            return None;
        }
        handoff::prepare_thread_sync_input(session, &peers).ok()
    }
}
