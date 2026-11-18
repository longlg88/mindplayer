use super::*;
use std::io::Write;

/// Each fixture owns its provider roots, state and shell child.
/// App drops/reaps children before the test directory's guard is dropped.
struct LifecyclePane {
    app: App,
    original: Session,
    target: Session,
    collision: Session,
    directory: FreshLoginTestHome,
}

impl LifecyclePane {
    fn new() -> Self {
        use mindplayer_core::accounts::Account;
        let directory = FreshLoginTestHome::new();
        let account = Account::isolated(&directory.0, Agent::Codex, "first").unwrap();
        let other = Account::isolated(&directory.0, Agent::Codex, "second").unwrap();
        let make_source = |id: &str, account: &Account| {
            let mut source = session(id, Agent::Codex, false);
            source.cwd = directory.0.clone();
            source.file = account
                .session_root(&directory.0)
                .join(format!("{id}.jsonl"));
            std::fs::create_dir_all(source.file.parent().unwrap()).unwrap();
            let metadata = serde_json::json!({"type":"session_meta","payload":{
                "id": id, "cwd": source.cwd, "timestamp": Utc::now()
            }});
            std::fs::write(&source.file, format!("{metadata}\n")).unwrap();
            source
        };
        let original = make_source("mp-owner", &account);
        let target = make_source("mp-target", &account);
        let collision = make_source("mp-target", &other);
        let mut app = isolated_app_at(directory.0.join("state.json"));
        app.accounts = vec![account, other];
        app.cfg.codex_dir = directory.0.join("default/sessions");
        app.scope = Scope::Global;
        app.all_sessions = vec![original.clone(), target.clone()];
        app.archive_catalog = Arc::new(vec![collision.clone()]);
        app.rebuild_visible();
        let mut fixture = Self {
            app,
            original,
            target,
            collision,
            directory,
        };
        let id = fixture.original.id.clone();
        let pty = fixture.spawn(&id);
        fixture.app.ptys.insert(id.clone(), pty);
        fixture.app.focus_or_add_pane(&id);
        fixture
    }

    fn spawn(&self, id: &str) -> PtySession {
        let command = mindplayer_core::Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "stty -echo -icanon; printf '%s' \"$$\" > \"$MP_TEST_PID\"; exec tee \"$MP_TEST_RECEIPT\"".into()],
            cwd: self.directory.0.clone(),
            env: mindplayer_core::accounts::LaunchEnv { set: vec![
                ("MP_TEST_PID".into(), self.directory.0.join(format!("{id}.pid")).display().to_string()),
                ("MP_TEST_RECEIPT".into(), self.directory.0.join(format!("{id}.input")).display().to_string()),
            ], unset: Vec::new() },
        };
        let pty = PtySession::spawn(&command, id, 6, 40).unwrap();
        wait_until(|| {
            std::fs::read_to_string(self.directory.0.join(format!("{id}.pid")))
                .is_ok_and(|text| text.parse::<u32>().is_ok())
        });
        pty
    }

    fn append_event(&mut self, kind: &str, turn: &str) {
        let event = serde_json::json!({"type":"event_msg","payload":{
            "type": kind, "turn_id": turn
        }});
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&self.original.file)
                .unwrap(),
            "{event}"
        )
        .unwrap();
        self.app
            .codex_turns
            .get_mut(&self.original.id)
            .unwrap()
            .poll();
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach the expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn mp02_resume_reuses_native_owner_and_close_reaps_only_that_child() {
    let mut fixture = LifecyclePane::new();
    let other = fixture.spawn("unrelated");
    fixture
        .app
        .all_sessions
        .push(session("unrelated", Agent::Kiro, false));
    fixture.app.ptys.insert("unrelated".into(), other);
    fixture.app.focus_or_add_pane("unrelated");
    fixture.app.selected = fixture.app.row_of_session(&fixture.original.id).unwrap();
    let pid: i32 = std::fs::read_to_string(fixture.directory.0.join("mp-owner.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let parser = Arc::clone(fixture.app.ptys[&fixture.original.id].parser());

    fixture.app.request_resume();
    assert!(
        fixture.app.pending.is_none(),
        "reopening the native target must not spawn another PTY"
    );
    assert!(fixture.app.pending_queue.is_empty());
    assert_eq!(
        fixture.app.active.as_deref(),
        Some(fixture.original.id.as_str())
    );
    assert_eq!(fixture.app.ptys.len(), 2);
    assert!(Arc::ptr_eq(
        &parser,
        fixture.app.active_pty().unwrap().parser()
    ));
    fixture.app.send_to_pty(b"same-child\n");
    wait_until(|| {
        std::fs::read(fixture.directory.0.join("mp-owner.input"))
            .is_ok_and(|bytes| bytes == b"same-child\n")
    });

    fixture.app.close_selected();
    assert!(!fixture.app.ptys.contains_key(&fixture.original.id));
    assert!(!fixture.app.panes.contains(&fixture.original.id));
    assert!(fixture.app.ptys.get_mut("unrelated").unwrap().is_alive());
    // SAFETY: PID was recorded by this fixture's own child; signal zero only probes it.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "closed child must have been reaped"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(fixture.original.file.exists());
    assert!(fixture.target.file.exists());
}

#[test]
fn mp01_late_closed_placeholder_archives_only_its_resolved_source() {
    let mut fixture = LifecyclePane::new();
    let mut placeholder = session("new:codex:late", Agent::Codex, false);
    placeholder.cwd = fixture.directory.0.clone();
    placeholder.started_at = Some(Utc::now());
    fixture.target.started_at = placeholder.started_at;
    fixture.collision.started_at = placeholder.started_at;
    fixture
        .app
        .pane_accounts
        .insert(placeholder.id.clone(), (Agent::Codex, "first".into()));
    fixture.app.closed_extras.push(placeholder);
    fixture.app.all_sessions = vec![fixture.target.clone(), fixture.collision.clone()];
    fixture
        .app
        .state
        .record_session_use(&fixture.collision, Utc::now());
    fixture.app.reap_closed_extras(&HashSet::new());
    assert!(fixture.app.closed_extras.is_empty());
    let saved = State::load_from(&fixture.app.state_path);
    assert!(saved.is_session_archived(&fixture.target));
    assert!(
        !saved.is_session_archived(&fixture.collision),
        "late archive must not affect another account's same-ID transcript"
    );
    assert!(fixture
        .app
        .all_sessions
        .iter()
        .any(|s| s.file == fixture.collision.file));
}

#[test]
fn mp02_source_collision_cannot_activate_or_close_the_native_owner() {
    let mut fixture = LifecyclePane::new();
    fixture.collision.id = fixture.original.id.clone();
    fixture.app.all_sessions.push(fixture.collision.clone());
    fixture.app.rebuild_visible();
    fixture.app.selected = fixture
        .app
        .visible
        .iter()
        .position(|row| {
            row.session_index()
                .is_some_and(|i| fixture.app.all_sessions[i].file == fixture.collision.file)
        })
        .unwrap();
    fixture.app.request_resume();
    assert!(fixture.app.pending.is_none());
    fixture.app.close_selected();
    assert!(fixture
        .app
        .ptys
        .get_mut(&fixture.original.id)
        .unwrap()
        .is_alive());
    assert_eq!(fixture.app.ptys.len(), 1);
    assert!(fixture.app.status.contains("refused"));
}

#[test]
fn mp03_delivered_cr_lf_and_unsubmitted_paste_never_start_work() {
    for bytes in [b"\n".as_slice(), b"\r", b"draft\r", b"first\nsecond\n"] {
        let mut fixture = LifecyclePane::new();
        let id = fixture.original.id.clone();
        fixture.app.send_to_pty(bytes);
        fixture.app.poll_activity();
        assert_eq!(
            fixture.app.session_status(&id),
            SessionStatus::Idle,
            "enqueued {bytes:?} is not a start event"
        );
        assert!(fixture.app.paste_to_pty("unsubmitted\nmultiline"));
        fixture.app.poll_activity();
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
    }
}

#[test]
fn mp03_lf_and_paste_preserve_completion_until_a_real_followup() {
    let mut fixture = LifecyclePane::new();
    let id = fixture.original.id.clone();
    fixture.app.codex_turns.insert(
        id.clone(),
        crate::turn_status::TurnStatusReader::new(fixture.original.file.clone()),
    );
    fixture.append_event("task_started", "first");
    fixture.append_event("task_complete", "first");
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Completed);
    fixture.app.send_to_pty(b"\n");
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Completed);
    assert!(fixture.app.paste_to_pty("two\nlines"));
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Completed);
    fixture.app.send_to_pty(b"next\r");
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
    fixture.append_event("task_complete", "first");
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
    assert!(
        !fixture.app.poll_status_transitions(),
        "first sighting only seeds the status audit"
    );
    for turn in ["second", "third"] {
        fixture.append_event("task_started", turn);
        fixture.app.poll_activity();
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Working);
        assert!(fixture.app.poll_status_transitions());
        assert!(!fixture.app.poll_status_transitions());
        fixture.append_event("task_complete", turn);
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Completed);
        fixture.app.poll_status_transitions();
        fixture.app.send_to_pty(b"followup\r");
        assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
        fixture.app.poll_status_transitions();
    }
}

#[test]
fn mp03_uninstrumented_cli_uses_busy_output_and_genuine_exit() {
    let mut fixture = LifecyclePane::new();
    let id = fixture.original.id.clone();
    let command = mindplayer_core::Command {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), r"stty -echo; printf '> '; read -r first; printf '\033[2J\033[Hesc to interrupt'; read -r second; printf '\033[2J\033[H> '; read -r finished".into()],
        cwd: fixture.directory.0.clone(),
        env: Default::default(),
    };
    fixture
        .app
        .ptys
        .insert(id.clone(), PtySession::spawn(&command, &id, 6, 40).unwrap());
    wait_until(|| fixture.app.ptys[&id].looks_idle());
    fixture.app.poll_activity();
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
    fixture.app.send_to_pty(b"start\r");
    wait_until(|| fixture.app.ptys[&id].looks_busy());
    fixture.app.poll_activity();
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Working);
    fixture.app.send_to_pty(b"complete\r");
    wait_until(|| fixture.app.ptys[&id].looks_idle() && !fixture.app.ptys[&id].looks_busy());
    fixture.app.poll_activity();
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Idle);
    fixture.app.send_to_pty(b"exit\r");
    wait_until(|| {
        fixture.app.reap_pty();
        fixture.app.ended.contains(&id)
    });
    fixture.app.poll_activity();
    assert_eq!(fixture.app.session_status(&id), SessionStatus::Ended);
    assert!(!fixture.app.any_recent_activity());
}

fn assert_automatic_delivery_stays_idle(deferred: bool) {
    let mut fixture = LifecyclePane::new();
    let id = fixture.original.id.clone();
    let receipt = fixture.directory.0.join("automatic.input");
    let command = mindplayer_core::Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), r#"stty -echo -icanon; printf '> '; dd bs=1 count=1 > /dev/null 2>&1; printf '\033[2J\033[H'; exec cat > "$MP_TEST_RECEIPT""#.into()],
            cwd: fixture.directory.0.clone(),
            env: mindplayer_core::accounts::LaunchEnv {
                set: vec![("MP_TEST_RECEIPT".into(), receipt.display().to_string())],
                unset: Vec::new(),
            },
        };
    fixture
        .app
        .ptys
        .insert(id.clone(), PtySession::spawn(&command, &id, 6, 40).unwrap());
    wait_until(|| fixture.app.ptys[&id].looks_idle());
    fixture.app.poll_activity();
    let input = b"automatic context\r".to_vec();
    if deferred {
        fixture.app.queue_initial_input(id.clone(), input);
        assert!(fixture.app.flush_initial_inputs());
    } else {
        assert!(fixture
            .app
            .enqueue_or_submit_to_session(&fixture.original, input));
    }
    wait_until(|| {
        !fixture.app.ptys[&id].looks_idle()
            && std::fs::read_to_string(&receipt)
                .is_ok_and(|bytes| bytes.contains("automatic context"))
    });
    fixture.app.poll_activity();
    assert!(fixture.app.turn_submitted.contains(&id));
    assert!(fixture.app.submitted_at.contains_key(&id));
    assert_eq!(
        fixture.app.session_status(&id),
        SessionStatus::Idle,
        "automatic delivery (deferred={deferred}) is not actual work"
    );
    assert!(!fixture.app.any_recent_activity());
}

#[test]
fn mp03_immediate_automatic_delivery_does_not_create_working_activity() {
    assert_automatic_delivery_stays_idle(false);
}

#[test]
fn mp03_deferred_automatic_delivery_does_not_create_working_activity() {
    assert_automatic_delivery_stays_idle(true);
}

#[test]
fn mp05_activity_refresh_preserves_duplicate_archive_source_and_restore() {
    let mut fixture = LifecyclePane::new();
    fixture.app.all_sessions = vec![fixture.original.clone()];
    let mut first = fixture.target.clone();
    first.archived = true;
    let mut second = fixture.collision.clone();
    second.archived = true;
    fixture.app.archive_catalog = Arc::new(vec![first, second]);
    fixture.app.toggle_archived_view();
    wait_until(|| {
        fixture.app.poll_working_set();
        fixture.app.visible_sessions().count() == 2
    });
    fixture.app.selected = fixture
        .app
        .visible
        .iter()
        .enumerate()
        .filter(|(_, row)| row.session_index().is_some())
        .nth(1)
        .map(|(index, _)| index)
        .unwrap();
    let selected = fixture.app.selected_session().unwrap().clone();
    assert_eq!(
        fixture.app.visible_sessions().next().unwrap().id,
        selected.id
    );
    assert_ne!(
        fixture.app.visible_sessions().next().unwrap().file,
        selected.file
    );
    let (tx, rx) = mpsc::channel();
    tx.send(vec![ActivityUpdate {
        source: working_set::source_key(&fixture.original),
        last_active: Some(Utc::now()),
        last_prompt_at: None,
        context_pct: None,
    }])
    .unwrap();
    fixture.app.refresh_rx = Some(rx);
    assert!(fixture.app.poll_refresh());
    assert_eq!(
        fixture.app.selected_session().unwrap().file,
        selected.file,
        "unrelated Active refresh must preserve the second same-ID Archive source"
    );
    let expected = resume(&selected, &fixture.app.account_of_session(&selected));
    fixture.app.request_resume();
    wait_until(|| {
        fixture.app.poll_working_set();
        fixture.app.pending.is_some()
    });
    assert_eq!(fixture.app.selected_session().unwrap().file, selected.file);
    let pending = fixture.app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, selected.id);
    assert_eq!(pending.command.env.set, expected.env.set);
    assert_eq!(pending.command.args, expected.args);
}
