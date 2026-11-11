use super::*;

impl App {
    /// Picker -> label input, on the login the picker row named.
    pub fn choose_new_account(&mut self, account: &mindplayer_core::accounts::Account) {
        self.new_agent = Some(account.provider);
        self.new_account = Some(account.clone());
        self.new_fresh_login = false;
        self.new_label = Some(String::new());
        self.new_picker = None;
    }

    /// Picker -> label input, for a session that signs in to an account of
    /// its own. The home is only made on confirm, so backing out leaves none.
    pub fn choose_new_fresh_login(&mut self, agent: Agent) {
        self.new_agent = Some(agent);
        self.new_account = None;
        self.new_fresh_login = true;
        self.new_label = Some(String::new());
        self.new_picker = None;
    }

    pub fn label_input_push(&mut self, c: char) {
        if let Some(buf) = self.new_label.as_mut() {
            buf.push(c);
        }
    }

    pub fn label_input_backspace(&mut self) {
        if let Some(buf) = self.new_label.as_mut() {
            buf.pop();
        }
    }

    /// Confirm the label input and spawn the new session.
    pub fn confirm_new_session(&mut self) {
        let label = self.new_label.take().unwrap_or_default();
        if std::mem::take(&mut self.new_fresh_login) {
            let agent = self.new_agent.unwrap_or(Agent::Codex);
            self.request_new_fresh_login(agent, &label);
            return;
        }
        match self.new_account.take() {
            Some(account) => self.request_new_on(&account, &label),
            None => {
                let agent = self.new_agent.unwrap_or(Agent::Codex);
                self.request_new(agent, &label);
            }
        }
    }

    pub fn cancel_new_session(&mut self) {
        self.new_picker = None;
        self.handoff_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.new_account = None;
        self.new_fresh_login = false;
        self.label_target = None;
    }

    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::HelpToggle {
                open: self.help_visible,
            },
        );
    }

    pub fn close_help(&mut self) {
        // Esc closes without going through `toggle_help`, so the close has to
        // log here or the log shows opens with no matching closes.
        if self.help_visible {
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::HelpToggle { open: false },
            );
        }
        self.help_visible = false;
    }

    pub fn begin_search(&mut self) {
        self.search_query = Some(String::new());
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::SearchBegin);
        self.refresh_search_results();
    }

    pub fn search_push(&mut self, c: char) {
        if let Some(query) = self.search_query.as_mut() {
            query.push(c);
            self.refresh_search_results();
        }
    }

    pub fn search_backspace(&mut self) {
        if let Some(query) = self.search_query.as_mut() {
            query.pop();
            self.refresh_search_results();
        }
    }

    pub fn cancel_search(&mut self) {
        self.search_query = None;
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::SearchCancel);
        self.refresh_search_results();
    }

    fn refresh_search_results(&mut self) {
        if self.show_archived && self.archive_available() {
            self.archive_page = 0;
            self.request_archive_page();
        }
        self.rebuild_visible();
    }

    /// Enter from an active search: resume the matched session and close the
    /// search modal. Must clear `search_query` before handing off to
    /// `request_resume` — otherwise it survives the switch to `Focus::Terminal`
    /// and the search-modal key branch (checked ahead of `Focus::Terminal` in
    /// `handle_main_key`) keeps swallowing every later key: Tab falls to its
    /// `_ => {}` arm instead of cycling panes, and typed characters get pushed
    /// into the now-invisible search buffer instead of reaching the pty.
    pub fn confirm_search(&mut self) {
        if self.show_archived && self.selected_session().is_none() {
            // A pending worker still owns this query. Do not hide the query
            // while allowing its filtered page to arrive as an unfiltered view.
            self.status =
                "no archived session selected; wait for results or refine the search".to_string();
            return;
        }
        // Archive restore is asynchronous. Its success path clears the query
        // before focusing the terminal; failure must retain the matching page.
        if !self
            .selected_session()
            .is_some_and(|session| session.archived)
        {
            self.search_query = None;
        }
        self.request_resume();
        // Logged after the resume so the `focus` field reflects where the app
        // actually landed ("terminal" when a match resumed). Together with the
        // preceding `SessionResume`, this is what makes a "search active →
        // resume → focus terminal" setup — the shape behind the swallowed-Tab
        // bug — visible in the log without any source.
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::SearchConfirm {
                focus: focus_label(self.focus).to_string(),
            },
        );
    }

    /// Open the label-input modal for the currently-selected session so an
    /// existing session (one created outside MindPlayer, or without a label)
    /// can be tagged. Pre-fills the current label so it can be edited or
    /// cleared. Synthetic placeholders use the new-session flow instead.
    pub fn begin_label_edit(&mut self) {
        let Some(s) = self.selected_session() else {
            return;
        };
        if s.id.starts_with("new:") {
            self.status = "label is set when you create a new session".to_string();
            return;
        }
        let id = s.id.clone();
        let existing = self.state.label_for(&id).unwrap_or_default().to_string();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::LabelEditBegin { id: id.clone() },
        );
        self.label_target = Some(id);
        self.new_label = Some(existing);
    }

    /// Confirm the label-input modal when editing an existing session: persist
    /// the label and update the in-memory title. A blank label clears it (the
    /// auto-extracted title is restored on the next scan).
    pub fn confirm_label_edit(&mut self) {
        let Some(id) = self.label_target.take() else {
            return;
        };
        let buf = self.new_label.take().unwrap_or_default();
        let label = buf.trim();
        self.state.set_label(&id, label);
        let _ = self.save_state();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::LabelEditConfirm {
                id: id.clone(),
                label: label.to_string(),
            },
        );
        if label.is_empty() {
            self.status = format!("label cleared for {}", short(&id));
            // Re-extract the original title from disk shortly.
            self.rescan_due = Some(Instant::now());
        } else {
            if let Some(s) = self.all_sessions.iter_mut().find(|s| s.id == id) {
                s.title = format!("🏷 {label}");
            }
            self.status = format!("labeled: {label}");
        }
    }

    // --- working-dir input ------------------------------------------------

    /// Open the working-dir modal, pre-filled with the current directory so it
    /// can be edited or replaced.
    pub fn begin_dir_input(&mut self) {
        self.dir_input = Some(self.cwd.display().to_string());
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::WorkingDirBegin,
        );
    }

    pub fn dir_input_push(&mut self, c: char) {
        if let Some(buf) = self.dir_input.as_mut() {
            buf.push(c);
        }
    }

    pub fn dir_input_backspace(&mut self) {
        if let Some(buf) = self.dir_input.as_mut() {
            buf.pop();
        }
    }

    pub fn cancel_dir_input(&mut self) {
        self.dir_input = None;
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::WorkingDirCancel,
        );
    }

    /// Confirm the working-dir modal: validate the path, re-point the scope at
    /// it, and kick a fresh scan in place. Invalid paths keep the modal open
    /// with an error in the status line. A blank entry switches to global scope.
    pub fn confirm_dir_input(&mut self) {
        let raw = self.dir_input.clone().unwrap_or_default();
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            self.scope = Scope::Global;
            self.dir_input = None;
            self.state.last_scope = Some(self.scope.label());
            let _ = self.save_state();
            mindplayer_core::log_event_to(
                &self.audit_path,
                mindplayer_core::AuditEvent::WorkingDirConfirm {
                    scope: self.scope.label(),
                },
            );
            self.status = "scope → global".to_string();
            self.start_bg_rescan();
            return;
        }

        let path = expand_tilde(trimmed);
        let resolved = path.canonicalize().unwrap_or(path);
        if !resolved.is_dir() {
            self.status = format!("not a directory: {}", resolved.display());
            return; // keep the modal open so the user can fix it
        }

        self.cwd = resolved.clone();
        self.scope = Scope::WorkingDir(resolved.clone());
        self.dir_input = None;
        self.state.last_scope = Some(self.scope.label());
        let _ = self.save_state();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::WorkingDirConfirm {
                scope: self.scope.label(),
            },
        );
        self.status = format!("working dir → {}", resolved.display());
        self.start_bg_rescan();
    }
}

/// Expand a leading `~` / `~/` to the user's home directory. Other paths are
/// returned unchanged (relative paths resolve against the process cwd later).
pub(crate) fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(input)
}
