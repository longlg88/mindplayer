use super::*;

impl App {
    /// Picker -> label input: remember the agent, start an empty label buffer.
    pub fn choose_new_agent(&mut self, agent: Agent) {
        self.new_agent = Some(agent);
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
        let agent = self.new_agent.unwrap_or(Agent::Codex);
        let label = self.new_label.take().unwrap_or_default();
        self.request_new(agent, &label);
    }

    pub fn cancel_new_session(&mut self) {
        self.new_picker = None;
        self.handoff_picker = None;
        self.new_label = None;
        self.new_agent = None;
        self.label_target = None;
    }

    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    pub fn close_help(&mut self) {
        self.help_visible = false;
    }

    pub fn begin_search(&mut self) {
        self.search_query = Some(String::new());
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::SearchBegin);
        self.rebuild_visible();
    }

    pub fn search_push(&mut self, c: char) {
        if let Some(query) = self.search_query.as_mut() {
            query.push(c);
            self.rebuild_visible();
        }
    }

    pub fn search_backspace(&mut self) {
        if let Some(query) = self.search_query.as_mut() {
            query.pop();
            self.rebuild_visible();
        }
    }

    pub fn cancel_search(&mut self) {
        self.search_query = None;
        mindplayer_core::log_event_to(&self.audit_path, mindplayer_core::AuditEvent::SearchCancel);
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
        self.search_query = None;
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
        let _ = self.state.save();
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
            let _ = self.state.save();
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
        let _ = self.state.save();
        mindplayer_core::log_event_to(
            &self.audit_path,
            mindplayer_core::AuditEvent::WorkingDirConfirm {
                scope: self.scope.label(),
            },
        );
        self.status = format!("working dir → {}", resolved.display());
        self.start_bg_rescan();
    }

    // --- HTML preview (carbonyl) ------------------------------------------

    /// `Ctrl-P` from a live pane. Three behaviors, keyed off the focused pane:
    /// - already showing its preview → switch back to the agent view (the
    ///   carbonyl process is left running in the background, not killed);
    /// - a live preview process exists but is hidden → re-show it instantly
    ///   (no re-spawn, no popup);
    /// - otherwise → open the path-input popup to spawn a fresh carbonyl.
    pub fn toggle_html_preview(&mut self) {
        let Some(id) = self.focused_pane().map(str::to_string) else {
            self.status = "html preview needs a live pane".to_string();
            return;
        };
        if self.previewing.remove(&id) {
            // Hide the preview; the carbonyl process keeps running.
            self.selection = None;
            self.status = format!("preview hidden — agent view restored for {}", short(&id));
            return;
        }
        // Re-show an existing preview only if its carbonyl is still alive; a
        // dead one is dropped so the popup opens to spawn a fresh process.
        let alive = self
            .preview_ptys
            .get_mut(&id)
            .map(|p| p.is_alive())
            .unwrap_or(false);
        if alive {
            self.previewing.insert(id.clone());
            self.selection = None;
            self.status = format!("preview shown for {}", short(&id));
            return;
        }
        if let Some(mut dead) = self.preview_ptys.remove(&id) {
            dead.kill();
        }
        self.html_preview_input = Some(String::new());
        self.html_preview_error = None;
        self.status = "html preview: type a path to a local .html file".to_string();
    }

    pub fn html_preview_input_push(&mut self, c: char) {
        if let Some(buf) = self.html_preview_input.as_mut() {
            buf.push(c);
        }
        // Any edit invalidates a stale "bad path" error.
        self.html_preview_error = None;
    }

    pub fn html_preview_input_backspace(&mut self) {
        if let Some(buf) = self.html_preview_input.as_mut() {
            buf.pop();
        }
        self.html_preview_error = None;
    }

    pub fn cancel_html_preview(&mut self) {
        self.html_preview_input = None;
        self.html_preview_error = None;
    }

    /// Confirm the preview popup: resolve + validate the path, spawn `carbonyl`
    /// as the focused pane's preview child, and start showing it. On any
    /// failure (blank/nonexistent path, or carbonyl not on PATH) the popup is
    /// left open with `html_preview_error` set so the user can correct it — no
    /// process is spawned and the agent's own PTY is never touched.
    ///
    /// The carbonyl child runs in the resolved file's parent directory, so a
    /// page's relative asset references (`./style.css`, images) resolve the way
    /// they would if opened from that folder.
    pub fn confirm_html_preview(&mut self) {
        let Some(raw) = self.html_preview_input.clone() else {
            return;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.html_preview_error = Some("enter a path to an .html file".to_string());
            return;
        }
        let path = expand_tilde(trimmed);
        let resolved = path.canonicalize().unwrap_or(path);
        if !resolved.is_file() {
            self.html_preview_error = Some(format!("not a file: {}", resolved.display()));
            return;
        }
        let Some(id) = self.focused_pane().map(str::to_string) else {
            self.html_preview_error = Some("no focused pane to preview into".to_string());
            return;
        };
        let (rows, cols) = self
            .pane_sizes
            .get(&id)
            .copied()
            .unwrap_or((self.pty_rows, self.pty_cols));
        let cwd = resolved
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let command = mindplayer_core::Command {
            program: "carbonyl".to_string(),
            args: vec![resolved.display().to_string()],
            cwd,
        };
        // A `preview:`-prefixed id keeps carbonyl's stderr log distinct from the
        // agent's own log for the same session id.
        match PtySession::spawn(&command, &format!("preview:{id}"), rows, cols) {
            Ok(pty) => {
                self.preview_ptys.insert(id.clone(), pty);
                self.previewing.insert(id.clone());
                self.html_preview_input = None;
                self.html_preview_error = None;
                self.selection = None;
                self.status = format!("previewing {} in {}", resolved.display(), short(&id));
            }
            Err(e) => {
                self.html_preview_error = Some(format!("failed to start carbonyl: {e}"));
            }
        }
    }

    // --- scope + scanning -------------------------------------------------
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
