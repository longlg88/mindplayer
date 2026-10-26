use super::*;

impl App {
    pub(super) fn sync_target_was_cleared(&self, id: &str) -> bool {
        self.ptys.get(id).is_some_and(|pty| {
            pty.poll_routing_status();
            pty.routing_clear_target(id).is_some()
        })
    }

    /// Only a pane-owned, verified /clear lineage can move a real session's
    /// identity. Cwd, file timestamps and similarly named labels are not proof.
    pub(super) fn reconcile_codex_clears(&mut self) -> bool {
        let mut changed = false;
        for pty in self.ptys.values() {
            // Hidden panes own live children too. Reading their protocol must
            // not consume terminal dirty bits or depend on a visible redraw.
            changed |= pty.poll_routing_status();
        }
        let transitions: Vec<_> = self
            .ptys
            .iter()
            .filter(|(id, _)| !mindplayer_core::session::is_synthetic_id(id))
            .filter_map(|(id, pty)| {
                pty.routing_clear_target(id)
                    .map(|target| (id.clone(), target))
            })
            .collect();
        for (previous, current) in transitions {
            let Some(old) = self.all_sessions.iter().find(|s| s.id == previous).cloned() else {
                continue;
            };
            if old.agent != Agent::Codex {
                continue;
            }
            let Some(new) = self.all_sessions.iter().find(|s| s.id == current).cloned() else {
                // The native lifecycle precedes the first rollout write. Retry
                // discovery without replacing the live pane with a guessed row.
                self.rescan_due
                    .get_or_insert(Instant::now() + Duration::from_secs(3));
                continue;
            };
            if new.agent != Agent::Codex
                || new.is_subagent
                || new.file.as_os_str().is_empty()
                || self.ptys.contains_key(&current)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.session_id == current)
                || self.pending_queue.iter().any(|p| p.session_id == current)
                || self.account_auth.owns_launch(&current)
                || self.state.label_for(&current).is_some()
                || self.state.handoff_links.contains_key(&current)
                || self.state.archived.contains(&current)
                || self.account_of_session(&old).slot != self.account_of_session(&new).slot
            {
                continue;
            }
            let selected = self.selected_session().map(|s| s.id.clone());
            let mut next = self.state.clone();
            if self
                .thread_sync_rx
                .as_ref()
                .is_some_and(|sync| sync.target_id == previous)
            {
                next.thread_synced.remove(&previous);
            }
            if let Some(label) = next.labels.remove(&previous) {
                next.set_label(&current, &label);
            }
            if let Some(category) = next.session_category.get(&previous).cloned() {
                next.session_category.insert(current.clone(), category);
            }
            if next.in_progress.remove(&previous) {
                next.in_progress.insert(current.clone());
            }
            if let Some(link) = next.handoff_links.get(&previous).cloned() {
                next.handoff_links.insert(current.clone(), link);
            }
            for link in next.handoff_links.values_mut() {
                if link.parent_id == previous {
                    link.parent_id = current.clone();
                }
            }
            for pending in &mut next.pending_handoffs {
                if pending.parent_id == previous {
                    pending.parent_id = current.clone();
                }
            }
            // Never claim restart-safe continuity until the new label target
            // has reached disk. A failed save leaves the pane intact to retry.
            if next.save_to(&self.state_path).is_err() {
                self.status = "could not save /clear session continuity; retrying".into();
                continue;
            }
            self.state = next;
            self.rekey_cleared_pane(&previous, &current);
            // Old transcripts remain available, but only the new row owns the
            // label. Its raw pre-clear title returns on the next normal scan.
            if let Some(row) = self.all_sessions.iter_mut().find(|s| s.id == previous) {
                if row.title.starts_with("🏷 ") {
                    row.title = previous.clone();
                }
            }
            self.state.apply(&mut self.all_sessions);
            self.rebuild_visible();
            let selected = selected.map(|id| if id == previous { current.clone() } else { id });
            if let Some(pos) = selected.and_then(|id| self.row_of_session(&id)) {
                self.selected = pos;
            }
            self.rescan_due
                .get_or_insert(Instant::now() + Duration::from_secs(3));
            changed = true;
        }
        changed
    }

    fn rekey_cleared_pane(&mut self, previous: &str, current: &str) {
        // Moving the PtySession preserves its child process and native input,
        // private status generation and launch-time account environment.
        macro_rules! move_entry {
            ($($field:ident),+ $(,)?) => { $(
                if let Some(value) = self.$field.remove(previous) {
                    self.$field.insert(current.to_string(), value);
                }
            )+ };
        }
        move_entry!(
            ptys,
            observers,
            pending_initial_inputs,
            pane_accounts,
            pane_sizes,
            pane_bounds,
            html_candidates,
            html_seen,
            out_seq,
            out_at,
            last_status,
            pane_error,
            hook_floor
        );
        let source = self
            .hook_source_ids
            .remove(previous)
            .unwrap_or_else(|| previous.into());
        if let Some(reading) = crate::agent_hooks::read_hook_status(&source) {
            self.hook_floor.insert(current.into(), reading);
        }
        self.hook_source_ids.insert(current.into(), source);
        self.hook_status.remove(previous);
        self.codex_turns.remove(previous);
        self.submitted_at.remove(previous);
        self.synthetic_turn_cutoffs.remove(previous);
        self.thread_sync_at.remove(previous);
        if self
            .thread_sync_rx
            .as_ref()
            .is_some_and(|sync| sync.target_id == previous)
        {
            self.thread_sync_rx = None;
        }
        if self
            .category_sync_rx
            .as_ref()
            .is_some_and(|sync| sync.target_id == previous)
        {
            self.category_sync_rx = None;
        }
        self.turn_submitted.remove(previous);
        for set in [&mut self.marked, &mut self.ended] {
            if set.remove(previous) {
                set.insert(current.into());
            }
        }
        for pane in &mut self.panes {
            if pane == previous {
                *pane = current.into();
            }
        }
        for target in [
            &mut self.active,
            &mut self.label_target,
            &mut self.catchup_confirm,
        ] {
            if target.as_deref() == Some(previous) {
                *target = Some(current.into());
            }
        }
        if let Some(selection) = &mut self.selection {
            if selection.pane_id == previous {
                selection.pane_id = current.into();
            }
        }
        if let Some(picker) = &mut self.category_picker {
            for target in &mut picker.targets {
                if target == previous {
                    *target = current.into();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::local_router::RoutingStatusChannel;

    struct ClearPane {
        app: App,
        status_path: PathBuf,
        directory: PathBuf,
        status: serde_json::Value,
    }

    impl ClearPane {
        fn new() -> Self {
            let directory = super::super::tests::fresh_login_test_home().join("clear-pane");
            std::fs::create_dir_all(&directory).unwrap();
            let mut old = super::super::tests::session("before-clear", Agent::Codex, false);
            old.file = directory.join("old.jsonl");
            std::fs::write(&old.file, b"old transcript stays intact\n").unwrap();
            let mut current = super::super::tests::session("after-clear", Agent::Codex, false);
            current.file = directory.join("current.jsonl");
            std::fs::write(&current.file, b"new transcript stays intact\n").unwrap();
            let mut app = super::super::tests::isolated_app_at(directory.join("state.json"));
            app.all_sessions = vec![old, current];
            app.state.set_label("before-clear", "topic");
            app.state.apply(&mut app.all_sessions);
            app.rebuild_visible();
            app.selected = app.row_of_session("before-clear").unwrap();
            app.panes = vec!["before-clear".into()];
            app.active = Some("before-clear".into());
            app.pane_accounts
                .insert("before-clear".into(), (Agent::Codex, "default".into()));
            let channel = RoutingStatusChannel::create_at(&directory.join("private")).unwrap();
            let status_path = channel.path().to_path_buf();
            let mut status = serde_json::to_value(channel.snapshot().unwrap()).unwrap();
            status["revision"] = serde_json::json!(1);
            status["threadId"] = serde_json::json!("after-clear");
            status["clearAncestors"] = serde_json::json!(["before-clear"]);
            let command = mindplayer_core::Command {
                program: "cat".into(),
                args: Vec::new(),
                cwd: directory.clone(),
                env: Default::default(),
            };
            let mut pty = PtySession::spawn(&command, "before-clear", 6, 20).unwrap();
            pty.set_routing_status_for_test(channel);
            app.ptys.insert("before-clear".into(), pty);
            let mut fixture = Self {
                app,
                status_path,
                directory,
                status,
            };
            fixture.publish();
            fixture
        }

        fn publish(&mut self) {
            std::fs::write(&self.status_path, serde_json::to_vec(&self.status).unwrap()).unwrap();
            for pty in self.app.ptys.values() {
                pty.take_dirty();
            }
            self.app.hook_status_due = None;
        }
    }

    #[test]
    fn native_clear_moves_the_label_pane_and_persisted_resume_target() {
        let mut fixture = ClearPane::new();
        let app = &mut fixture.app;
        let parser = app.ptys["before-clear"].parser().clone();
        app.state.in_progress.insert("before-clear".into());
        let category = app
            .state
            .create_category("synthetic topic", Utc::now())
            .unwrap();
        app.state
            .session_category
            .insert("before-clear".into(), category.clone());
        app.marked.insert("before-clear".into());
        app.pane_sizes.insert("before-clear".into(), (6, 20));
        app.queue_initial_input("before-clear".into(), b"queued input".to_vec());

        app.poll_hook_status();

        assert!(
            app.ptys.contains_key("after-clear"),
            "the native pane still uses its pre-clear ID"
        );
        assert!(!app.ptys.contains_key("before-clear"));
        assert!(
            std::sync::Arc::ptr_eq(&parser, app.ptys["after-clear"].parser()),
            "must preserve the existing PTY, not respawn it"
        );
        assert_eq!(app.active.as_deref(), Some("after-clear"));
        assert_eq!(app.panes, ["after-clear"]);
        assert_eq!(app.selected_session().unwrap().id, "after-clear");
        assert!(app.marked.contains("after-clear"));
        assert!(app.pending_initial_inputs.contains_key("after-clear"));
        assert_eq!(app.pane_sizes.get("after-clear"), Some(&(6, 20)));
        assert!(app.pane_accounts.contains_key("after-clear"));
        assert_eq!(app.state.label_for("after-clear"), Some("topic"));
        assert_eq!(app.state.label_for("before-clear"), None);
        assert_eq!(
            app.state.category_of("after-clear"),
            Some(category.as_str())
        );
        assert!(app.state.in_progress.contains("after-clear"));
        assert!(!app.state.in_progress.contains("before-clear"));

        let saved = State::load_from(&app.state_path);
        assert_eq!(saved.label_for("after-clear"), Some("topic"));
        let mut sessions = app.all_sessions.clone();
        saved.apply(&mut sessions);
        let current = sessions.iter().find(|s| s.title == "🏷 topic").unwrap();
        assert_eq!(current.id, "after-clear");
        let command = resume(current, &app.account_of_session(current));
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["resume", "after-clear"]));
        assert_eq!(
            std::fs::read(fixture.directory.join("old.jsonl")).unwrap(),
            b"old transcript stays intact\n"
        );
        assert_eq!(
            std::fs::read(fixture.directory.join("current.jsonl")).unwrap(),
            b"new transcript stays intact\n"
        );
        assert!(app.ptys.get_mut("after-clear").unwrap().is_alive());
    }

    #[test]
    fn native_clear_waits_for_discovery_and_retries_after_a_scan() {
        let mut fixture = ClearPane::new();
        let current = fixture.app.all_sessions.pop().unwrap();
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("before-clear"));
        assert!(
            fixture.app.rescan_due.is_some(),
            "a new rollout must be discovered without a manual refresh"
        );
        let (tx, rx) = mpsc::channel();
        fixture.app.bg_rescan_rx = Some(rx);
        let mut sessions = fixture.app.all_sessions.clone();
        sessions.push(current);
        tx.send(sessions).unwrap();
        assert!(fixture.app.poll_bg_rescan());
        assert!(fixture.app.ptys.contains_key("after-clear"));
        assert_eq!(fixture.app.selected_session().unwrap().id, "after-clear");
    }

    #[test]
    fn native_clear_can_skip_intermediate_clears_but_not_resume_or_fork() {
        let mut fixture = ClearPane::new();
        fixture.status["revision"] = serde_json::json!(2);
        fixture.status["clearAncestors"] = serde_json::json!([]);
        fixture.publish();
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("before-clear"));
        assert_eq!(fixture.app.state.label_for("after-clear"), None);

        fixture.status["revision"] = serde_json::json!(3);
        fixture.status["clearAncestors"] =
            serde_json::json!(["before-clear", "intermediate-clear"]);
        fixture.publish();
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("after-clear"));
        assert_eq!(fixture.app.state.label_for("after-clear"), Some("topic"));
    }

    #[test]
    fn native_clear_does_not_overwrite_another_pane_or_existing_label() {
        let mut fixture = ClearPane::new();
        fixture.app.state.set_label("after-clear", "other topic");
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("before-clear"));
        assert_eq!(
            fixture.app.state.label_for("after-clear"),
            Some("other topic")
        );
        fixture.app.state.set_label("after-clear", "");
        let command = mindplayer_core::Command {
            program: "cat".into(),
            args: Vec::new(),
            cwd: fixture.directory.clone(),
            env: Default::default(),
        };
        fixture.app.ptys.insert(
            "after-clear".into(),
            PtySession::spawn(&command, "after-clear", 6, 20).unwrap(),
        );
        fixture.app.hook_status_due = None;
        fixture.app.poll_hook_status();
        assert_eq!(fixture.app.ptys.len(), 2);
        assert_eq!(fixture.app.state.label_for("before-clear"), Some("topic"));
    }

    #[test]
    fn native_clear_retries_a_failed_save_without_losing_the_original_label() {
        let mut fixture = ClearPane::new();
        let path = fixture.app.state_path.clone();
        // An existing file cannot be the state directory. No permissions or
        // environment mutation is needed to produce a deterministic failure.
        fixture.app.state_path = fixture.directory.join("old.jsonl/state.json");
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("before-clear"));
        assert_eq!(fixture.app.state.label_for("before-clear"), Some("topic"));
        assert_eq!(fixture.app.state.label_for("after-clear"), None);
        assert!(fixture.app.status.contains("retrying"));
        fixture.app.state_path = path;
        fixture.app.hook_status_due = None;
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("after-clear"));
        assert_eq!(
            State::load_from(&fixture.app.state_path).label_for("after-clear"),
            Some("topic")
        );
    }

    #[test]
    fn native_clear_rejects_another_accounts_transcript() {
        let mut fixture = ClearPane::new();
        let other =
            mindplayer_core::accounts::Account::isolated(&fixture.directory, Agent::Codex, "other")
                .unwrap();
        fixture.app.all_sessions[1].file = other.slot_path().join("sessions/current.jsonl");
        fixture.app.accounts.push(other);
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("before-clear"));
        assert_eq!(fixture.app.state.label_for("after-clear"), None);
    }

    #[test]
    fn native_clear_rebinds_repeatedly_even_when_the_pane_is_hidden() {
        let mut fixture = ClearPane::new();
        fixture.app.poll_hook_status();
        let mut newest = fixture.app.all_sessions[1].clone();
        newest.id = "second-clear".into();
        fixture.app.all_sessions.push(newest);
        fixture.app.panes.clear();
        fixture.app.active = None;
        fixture.status["revision"] = serde_json::json!(2);
        fixture.status["threadId"] = serde_json::json!("second-clear");
        fixture.status["clearAncestors"] = serde_json::json!(["before-clear", "after-clear"]);
        // Do not take_dirty: background metadata polling must be sufficient.
        std::fs::write(
            &fixture.status_path,
            serde_json::to_vec(&fixture.status).unwrap(),
        )
        .unwrap();
        fixture.app.hook_status_due = None;
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("second-clear"));
        assert_eq!(fixture.app.state.label_for("second-clear"), Some("topic"));
        assert_eq!(fixture.app.state.label_for("after-clear"), None);
        assert!(fixture.app.panes.is_empty());
    }

    #[test]
    fn native_clear_keeps_handoff_relationships_without_reusing_cleared_context_marks() {
        let mut fixture = ClearPane::new();
        let link = |parent: &str| mindplayer_core::state::HandoffLink {
            parent_id: parent.into(),
            artifact: fixture.directory.join("synthetic-handoff.md"),
            created_at: Utc::now(),
        };
        fixture
            .app
            .state
            .handoff_links
            .insert("before-clear".into(), link("root"));
        fixture
            .app
            .state
            .handoff_links
            .insert("peer".into(), link("before-clear"));
        fixture
            .app
            .state
            .thread_synced
            .insert("before-clear".into());
        fixture
            .app
            .state
            .sync_marks
            .entry("before-clear".into())
            .or_default()
            .insert("peer".into(), 42);
        fixture.app.poll_hook_status();
        assert_eq!(fixture.app.state.thread_root("after-clear"), "root");
        assert_eq!(
            fixture.app.state.handoff_links["peer"].parent_id,
            "after-clear"
        );
        assert!(!fixture.app.state.thread_synced.contains("after-clear"));
        assert!(
            !fixture.app.state.sync_marks.contains_key("after-clear"),
            "a native clear erased the target's received peer context"
        );
        let saved = State::load_from(&fixture.app.state_path);
        assert_eq!(saved.thread_root("after-clear"), "root");
    }

    #[test]
    fn native_clear_can_finish_rebinding_after_the_native_child_has_exited() {
        let mut fixture = ClearPane::new();
        let current = fixture.app.all_sessions.pop().unwrap();
        fixture.status["revision"] = serde_json::json!(2);
        fixture.status["stage"] = serde_json::json!("closed");
        fixture.publish();
        fixture.app.ended.insert("before-clear".into());
        fixture.app.ptys.get_mut("before-clear").unwrap().kill();
        fixture.app.poll_hook_status();
        assert!(fixture.app.ptys.contains_key("before-clear"));
        let (tx, rx) = mpsc::channel();
        fixture.app.bg_rescan_rx = Some(rx);
        let mut sessions = fixture.app.all_sessions.clone();
        sessions.push(current);
        tx.send(sessions).unwrap();
        fixture.app.poll_bg_rescan();
        assert!(fixture.app.ptys.contains_key("after-clear"));
        assert!(fixture.app.ended.contains("after-clear"));
        assert_eq!(
            State::load_from(&fixture.app.state_path).label_for("after-clear"),
            Some("topic")
        );
    }

    #[test]
    fn native_clear_cannot_replace_a_target_reserved_by_a_pending_or_verified_launch() {
        for reservation in ["pending", "queued", "auth"] {
            let mut fixture = ClearPane::new();
            let target = fixture.app.all_sessions[1].clone();
            let account = fixture.app.account_of_session(&target);
            let pending = PendingSpawn {
                session_id: target.id.clone(),
                command: resume(&target, &account),
                initial_input: None,
                focus_after_spawn: true,
            };
            match reservation {
                "pending" => fixture.app.pending = Some(pending),
                "queued" => fixture.app.pending_queue.push_back(pending),
                "auth" => {
                    assert!(fixture
                        .app
                        .verify_account_launch(&account, pending)
                        .is_none());
                    assert!(fixture.app.account_auth.owns_launch(&target.id));
                }
                _ => unreachable!(),
            }
            fixture.app.poll_hook_status();
            assert!(
                fixture.app.ptys.contains_key("before-clear"),
                "{reservation} owns a later launch that would replace the adopted live PTY"
            );
            assert_eq!(fixture.app.state.label_for("before-clear"), Some("topic"));
            assert_eq!(fixture.app.state.label_for("after-clear"), None);
        }
    }

    #[test]
    fn native_clear_discards_late_old_context_sync() {
        let mut fixture = ClearPane::new();
        let (thread_tx, thread_rx) = mpsc::channel();
        let (category_tx, category_rx) = mpsc::channel();
        fixture.app.thread_sync_rx = Some(PendingSessionSync {
            target_id: "before-clear".into(),
            result: thread_rx,
        });
        fixture.app.category_sync_rx = Some(PendingSessionSync {
            target_id: "before-clear".into(),
            result: category_rx,
        });
        fixture.app.poll_hook_status();
        let sync = || crate::handoff::PreparedHandoff {
            input: b"stale pre-clear context".to_vec(),
            artifact: fixture.directory.join("synthetic.md"),
            transcript_chars: 23,
            inline_truncated: false,
        };
        let _ = thread_tx.send(("before-clear".into(), Ok(sync())));
        let _ = category_tx.send((
            "before-clear".into(),
            Ok((sync(), vec![("peer".into(), 99)])),
        ));
        fixture.app.poll_thread_sync();
        fixture.app.poll_category_sync();
        assert!(
            !fixture
                .app
                .pending_initial_inputs
                .contains_key("before-clear"),
            "a late sync must not resurrect context into the old session on a future reopen"
        );
        assert_eq!(fixture.app.state.sync_mark("before-clear", "peer"), 0);
        assert!(!fixture
            .app
            .pending_initial_inputs
            .contains_key("after-clear"));
    }

    #[test]
    fn native_clear_does_not_cancel_another_sessions_background_sync() {
        let mut fixture = ClearPane::new();
        let (thread_tx, thread_rx) = mpsc::channel();
        let (category_tx, category_rx) = mpsc::channel();
        fixture.app.thread_sync_rx = Some(PendingSessionSync {
            target_id: "other".into(),
            result: thread_rx,
        });
        fixture.app.category_sync_rx = Some(PendingSessionSync {
            target_id: "other".into(),
            result: category_rx,
        });
        fixture.app.poll_hook_status();
        let sync = || crate::handoff::PreparedHandoff {
            input: b"other session context".to_vec(),
            artifact: fixture.directory.join("synthetic.md"),
            transcript_chars: 21,
            inline_truncated: false,
        };
        thread_tx.send(("other".into(), Ok(sync()))).unwrap();
        category_tx
            .send(("other".into(), Ok((sync(), vec![("peer".into(), 99)]))))
            .unwrap();
        assert!(fixture.app.poll_thread_sync());
        assert!(fixture.app.poll_category_sync());
        assert!(fixture.app.pending_initial_inputs.contains_key("other"));
        assert_eq!(fixture.app.state.sync_mark("other", "peer"), 99);
    }

    #[test]
    fn native_clear_rejects_late_sync_even_before_discovery_or_a_successful_save() {
        for waiting_on in ["discovery", "save"] {
            let mut fixture = ClearPane::new();
            if waiting_on == "discovery" {
                fixture.app.all_sessions.pop();
            } else {
                fixture.app.state_path = fixture.directory.join("old.jsonl/state.json");
            }
            let (thread_tx, thread_rx) = mpsc::channel();
            let (category_tx, category_rx) = mpsc::channel();
            fixture.app.thread_sync_rx = Some(PendingSessionSync {
                target_id: "before-clear".into(),
                result: thread_rx,
            });
            fixture.app.category_sync_rx = Some(PendingSessionSync {
                target_id: "before-clear".into(),
                result: category_rx,
            });
            fixture.app.poll_hook_status();
            assert!(fixture.app.ptys.contains_key("before-clear"));
            let sync = || crate::handoff::PreparedHandoff {
                input: b"stale pre-clear context".to_vec(),
                artifact: fixture.directory.join("synthetic.md"),
                transcript_chars: 23,
                inline_truncated: false,
            };
            thread_tx.send(("before-clear".into(), Ok(sync()))).unwrap();
            category_tx
                .send((
                    "before-clear".into(),
                    Ok((sync(), vec![("peer".into(), 99)])),
                ))
                .unwrap();
            fixture.app.poll_thread_sync();
            fixture.app.poll_category_sync();
            assert!(
                !fixture
                    .app
                    .pending_initial_inputs
                    .contains_key("before-clear"),
                "late sync cannot target the cleared native pane while waiting on {waiting_on}"
            );
            assert_eq!(fixture.app.state.sync_mark("before-clear", "peer"), 0);
        }
    }
}
