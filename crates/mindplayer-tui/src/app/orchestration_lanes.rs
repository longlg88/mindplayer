use super::*;

impl App {
    pub(crate) fn start_orchestration(&mut self, draft: orchestration::Draft) {
        let dir = match &self.scope {
            Scope::WorkingDir(p) => p.clone(),
            Scope::Global => self.cwd.clone(),
        };
        let now = Utc::now();
        let artifact = PathBuf::from("mindplayer-orchestration");

        self.new_counter += 1;
        let group_id = self.new_counter;
        let main_agent = orchestration::agent_for_main(&draft);
        let main_id = format!("orch:main:{group_id}");
        let baseline = self.real_session_ids();
        self.new_baselines.insert(main_id.clone(), baseline.clone());
        let main_label = orchestration::main_label(&draft);
        self.enqueue_spawn(PendingSpawn {
            command: orchestration::main_command(&draft, dir.clone()),
            session_id: main_id.clone(),
            initial_input: Some(orchestration::main_prompt(&draft, &dir)),
            focus_after_spawn: true,
        });
        self.push_synthetic_session(Session {
            id: main_id.clone(),
            agent: main_agent,
            cwd: dir.clone(),
            file: PathBuf::new(),
            started_at: Some(now),
            last_active: Some(now),
            tokens: Default::default(),
            title: format!("🏷 {main_label}"),
            archived: false,
            is_subagent: false,
            context_pct: None,
        });

        for index in 1..=draft.children {
            let child_agent = orchestration::agent_for_child(&draft, index);
            let child_id = format!("orch:child:{group_id}:{index}");
            self.new_baselines
                .insert(child_id.clone(), baseline.clone());
            self.state
                .set_handoff_link(&child_id, &main_id, artifact.clone(), now);
            self.enqueue_spawn(PendingSpawn {
                command: orchestration::child_command(&draft, dir.clone(), index),
                session_id: child_id.clone(),
                initial_input: Some(orchestration::child_prompt(&draft, &dir, index)),
                focus_after_spawn: false,
            });
            self.push_synthetic_session(Session {
                id: child_id,
                agent: child_agent,
                cwd: dir.clone(),
                file: PathBuf::new(),
                started_at: Some(now + chrono::Duration::milliseconds(index as i64)),
                last_active: Some(now + chrono::Duration::milliseconds(index as i64)),
                tokens: Default::default(),
                title: format!("🏷 {}", orchestration::child_label(&draft, index)),
                archived: false,
                is_subagent: false,
                context_pct: None,
            });
        }

        self.focus_or_add_pane(&main_id);
        self.select_session_id(&main_id);
        self.orchestration_cycles.insert(main_id.clone(), 1);
        let _ = self.state.save();
        self.status = format!(
            "orchestration started: main + {} child lanes",
            draft.children
        );
        self.rescan_due = Some(Instant::now() + Duration::from_secs(3));
    }

    pub(crate) fn selected_orchestration_root(&self) -> Option<String> {
        let selected = self.selected_session()?;
        self.orchestration_root_for_session(selected)
            .filter(|root| !self.thread_child_ids(root).is_empty())
    }

    pub(crate) fn orchestration_root_for_session(&self, session: &Session) -> Option<String> {
        if is_orchestration_main_session(session) {
            return Some(session.id.clone());
        }
        if self.child_lane_index(session).is_some() {
            if let Some(parent_id) = self.state.handoff_parent(&session.id) {
                if self
                    .all_sessions
                    .iter()
                    .any(|s| s.id == parent_id && is_orchestration_main_session(s))
                {
                    return Some(parent_id.to_string());
                }
            }
            return self.orchestration_fallback_root(session);
        }

        let root = self.state.thread_root(&session.id).to_string();
        self.all_sessions
            .iter()
            .any(|s| s.id == root && is_orchestration_main_session(s))
            .then_some(root)
    }

    pub(crate) fn thread_child_ids(&self, root_id: &str) -> Vec<String> {
        let mut child_ids = self
            .all_sessions
            .iter()
            .filter(|s| !s.archived)
            .filter(|s| self.child_lane_index(s).is_some())
            .filter(|s| self.state.handoff_parent(&s.id) == Some(root_id))
            .map(|s| s.id.clone())
            .collect::<Vec<_>>();
        child_ids.extend(self.fallback_child_ids(root_id));
        child_ids.sort();
        child_ids.dedup();
        child_ids
    }

    pub(crate) fn child_lane_roster(&self, root_id: &str) -> String {
        let mut rows = self
            .thread_child_ids(root_id)
            .into_iter()
            .filter_map(|id| {
                let session = self.all_sessions.iter().find(|s| s.id == id)?;
                let lane = self.child_lane_index(session)?;
                Some(format!(
                    "- lane #{lane}: {} {} [{}] {}",
                    session.agent.as_str(),
                    short(&session.id),
                    match self.session_status(&session.id) {
                        SessionStatus::Blocked => "blocked",
                        SessionStatus::Working => "working",
                        SessionStatus::Idle => "idle",
                        SessionStatus::Ended => "done",
                        SessionStatus::Inactive => "inactive",
                    },
                    session.title
                ))
            })
            .collect::<Vec<_>>();
        rows.sort();
        if rows.is_empty() {
            "- no numbered child lanes found".to_string()
        } else {
            rows.join("\n")
        }
    }

    pub(crate) fn child_session_by_lane(&self, root_id: &str, lane: usize) -> Option<Session> {
        self.thread_child_ids(root_id)
            .into_iter()
            .filter_map(|id| self.all_sessions.iter().find(|s| s.id == id).cloned())
            .find(|s| self.child_lane_index(s) == Some(lane))
    }

    pub(crate) fn child_lane_index(&self, session: &Session) -> Option<usize> {
        orchestration_child_index(session).or_else(|| {
            self.state
                .label_for(&session.id)
                .and_then(orchestration_child_index_from_text)
        })
    }

    pub(crate) fn orchestration_fallback_root(&self, selected: &Session) -> Option<String> {
        if is_orchestration_main_session(selected) {
            return Some(selected.id.clone());
        }
        self.child_lane_index(selected)?;
        self.all_sessions
            .iter()
            .filter(|s| {
                s.id != selected.id
                    && s.agent == selected.agent
                    && s.cwd == selected.cwd
                    && is_orchestration_main_session(s)
            })
            .max_by_key(|s| s.started_at)
            .map(|s| s.id.clone())
    }

    pub(crate) fn fallback_child_ids(&self, root_id: &str) -> Vec<String> {
        let Some(root) = self.all_sessions.iter().find(|s| s.id == root_id) else {
            return Vec::new();
        };
        if !is_orchestration_main_session(root) {
            return Vec::new();
        }
        self.all_sessions
            .iter()
            .filter(|s| !s.archived)
            .filter(|s| {
                s.id != root.id
                    && s.agent == root.agent
                    && s.cwd == root.cwd
                    && self.child_lane_index(s).is_some()
            })
            .map(|s| s.id.clone())
            .collect()
    }

    pub(crate) fn waiting_child_count(&self, root_id: &str) -> usize {
        self.thread_child_ids(root_id)
            .iter()
            .filter(|id| !self.child_lane_ready_for_synthesis(id))
            .count()
    }

    pub(crate) fn child_lane_ready_for_synthesis(&self, id: &str) -> bool {
        if self.pending.as_ref().is_some_and(|p| p.session_id == id)
            || self.pending_queue.iter().any(|p| p.session_id == id)
            || self.pending_initial_inputs.contains_key(id)
        {
            return false;
        }
        matches!(
            self.session_status(id),
            SessionStatus::Idle | SessionStatus::Ended | SessionStatus::Inactive
        )
    }

    pub(crate) fn next_orchestration_cycle(&mut self, root_id: &str) -> u64 {
        let entry = self
            .orchestration_cycles
            .entry(root_id.to_string())
            .or_insert(1);
        *entry += 1;
        *entry
    }

    pub(crate) fn current_orchestration_cycle(&self, root_id: &str) -> u64 {
        self.orchestration_cycles.get(root_id).copied().unwrap_or(1)
    }
}

pub(crate) fn orchestration_title_marker(id: &str) -> Option<String> {
    if id.starts_with("orch:main:") {
        return Some("MindPlayer orchestration main session".to_string());
    }
    let child = id.strip_prefix("orch:child:")?;
    let index = child.rsplit(':').next()?;
    Some(format!("MindPlayer orchestration child lane #{index}"))
}

pub(crate) fn is_orchestration_main_session(session: &Session) -> bool {
    session
        .title
        .contains("MindPlayer orchestration main session")
        || (session.title.contains("(orch ") && !session.title.contains(" child "))
}

pub(crate) fn is_orchestration_child_session(session: &Session) -> bool {
    session
        .title
        .contains("MindPlayer orchestration child lane #")
        || (session.title.contains("(orch ") && session.title.contains(" child "))
}

pub(crate) fn orchestration_child_index(session: &Session) -> Option<usize> {
    orchestration_child_index_from_text(&session.title)
        .or_else(|| orchestration_child_index_from_file(session))
}

pub(crate) fn orchestration_child_index_from_text(text: &str) -> Option<usize> {
    if let Some((_, rest)) = text.split_once("MindPlayer orchestration child lane #") {
        return parse_leading_usize(rest);
    }
    if let Some((_, rest)) = text.split_once(" child ") {
        return parse_leading_usize(rest);
    }
    None
}

pub(crate) fn orchestration_child_index_from_file(session: &Session) -> Option<usize> {
    if session.file.as_os_str().is_empty() {
        return None;
    }
    let mut file = std::fs::File::open(&session.file).ok()?;
    let mut buf = String::new();
    file.by_ref()
        .take(ORCHESTRATION_MARKER_READ_BYTES)
        .read_to_string(&mut buf)
        .ok()?;
    let (_, rest) = buf.split_once("MindPlayer orchestration child lane #")?;
    parse_leading_usize(rest)
}

pub(crate) fn parse_leading_usize(s: &str) -> Option<usize> {
    let digits = s
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}
