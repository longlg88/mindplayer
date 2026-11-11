use super::*;

#[test]
fn mp_review_migration_save_failure_is_visible_and_retried() {
    let root = tests::fresh_login_test_home().join("migration-retry");
    let state_path = root.join("state.json");
    std::fs::create_dir_all(&state_path).unwrap();
    let mut app = tests::isolated_app_at(state_path.clone());
    let chosen = tests::session("legacy-use", Agent::Codex, false);
    app.state.record_use(&chosen.id, Utc::now());
    let mut migrated = app.state.clone();
    assert!(migrated.bind_legacy_use_sources([&chosen]));
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            active: vec![chosen.clone()],
            use_sources: migrated.last_used_sources.clone(),
            ..Default::default()
        }),
    ));
    assert!(
        app.status.contains("migration") && app.status.contains("retry"),
        "{}",
        app.status
    );
    std::fs::remove_dir(&state_path).unwrap();
    // The next scan has no new binding because the in-memory state already has it.
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            active: vec![chosen],
            ..Default::default()
        }),
    ));
    assert!(state_path.is_file());
    assert_eq!(
        mindplayer_core::State::load_from(&state_path).last_used_sources,
        migrated.last_used_sources
    );
}

#[test]
fn mp_review_unresolved_native_switch_does_not_stamp_previous_source() {
    let mut fixture = NativeSwitch::new();
    fixture.app.ptys[&fixture.original.id].poll_routing_status();
    fixture.app.note_session_use(&fixture.original.id);
    assert!(
        !fixture
            .app
            .state
            .last_used
            .contains_key(&fixture.original.id),
        "input after native resume must not promote the previous thread"
    );
    fixture.wait_for_target();
    fixture.app.note_session_use(&fixture.original.id);
    assert!(fixture.app.state.last_used.contains_key(&fixture.target.id));
    assert!(!fixture
        .app
        .state
        .last_used
        .contains_key(&fixture.original.id));
}

#[test]
fn mp_review_source_use_survives_restart_without_promoting_collision() {
    let mut app = tests::isolated_app();
    let mut chosen = tests::session("shared-native", Agent::Codex, false);
    chosen.file = PathBuf::from("/fixture/account-a/session.jsonl");
    chosen.started_at = Some(Utc::now() - chrono::Duration::days(30));
    chosen.last_prompt_at = None;
    let mut other = chosen.clone();
    other.file = PathBuf::from("/fixture/account-b/session.jsonl");
    other.archived = true;
    app.all_sessions = vec![chosen.clone(), other.clone()];
    app.note_session_use(&chosen.id);
    let loaded: mindplayer_core::State =
        serde_json::from_str(&serde_json::to_string(&app.state).unwrap()).unwrap();
    assert!(loaded.is_active_session(&chosen, Utc::now(), &HashSet::new()));
    assert!(
        !loaded.is_active_session(&other, Utc::now(), &HashSet::new()),
        "using one source must not promote another source after restart"
    );
}

#[test]
fn mp_review_live_typing_does_not_starve_catalog_adoption() {
    let mut fixture = NativeSwitch::new();
    fixture.wait_for_target();
    let epoch = fixture.app.working_set.epoch;
    for _ in 0..20 {
        fixture.app.note_session_use(&fixture.original.id);
    }
    assert!(
        fixture.app.apply_catalog((
            epoch,
            Instant::now(),
            Ok(mindplayer_core::catalog::CatalogSnapshot {
                archive: vec![fixture.target.clone(), fixture.collision.clone()],
                ..Default::default()
            })
        )),
        "continuous input to an already protected live source must not reject every catalog scan"
    );
    assert!(fixture
        .app
        .all_sessions
        .iter()
        .any(|s| !s.archived && s.file == fixture.target.file));
}

#[test]
fn mp_review_catalog_adoption_preserves_archive_selection_identity() {
    let mut app = tests::isolated_app();
    let rows: Vec<_> = (0..30)
        .map(|i| tests::session(&format!("archive-{i:03}"), Agent::Codex, true))
        .collect();
    app.archive_catalog = Arc::new(rows.clone());
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    app.selected = 12;
    let selected = app.selected_session().unwrap().clone();
    assert!(app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            archive: rows,
            ..Default::default()
        })
    )));
    wait_for_archive(&mut app);
    assert_eq!(
        app.selected_session().unwrap().id,
        selected.id,
        "periodic catalog refresh must not change Enter's target"
    );
    assert_eq!(app.selected_session().unwrap().file, selected.file);
}

#[test]
fn mp_review_failed_archive_search_restore_preserves_query_and_result() {
    let mut app = tests::isolated_app();
    let mut missing = tests::session("missing-search-source", Agent::Codex, true);
    missing.cwd = tests::fresh_login_test_home();
    missing.file = tests::fresh_login_test_home().join("missing-search.jsonl");
    app.archive_catalog = Arc::new(vec![missing.clone()]);
    app.toggle_archived_view();
    app.begin_search();
    for ch in "missing-search".chars() {
        app.search_push(ch);
    }
    wait_for_archive(&mut app);
    app.confirm_search();
    wait_for_archive(&mut app);
    assert!(
        app.status.contains("archive restore failed"),
        "{}",
        app.status
    );
    assert_eq!(app.search_query.as_deref(), Some("missing-search"));
    assert_eq!(app.archive_matches, 1);
    assert_eq!(app.selected_session().unwrap().file, missing.file);
    assert!(app.pending.is_none());

    // Recover the exact source and retry without rebuilding the user's query.
    let metadata = serde_json::json!({"type":"session_meta","payload":{
        "id":missing.id,"cwd":missing.cwd,
        "timestamp":Utc::now()
    }});
    std::fs::write(&missing.file, format!("{metadata}\n")).unwrap();
    app.confirm_search();
    wait_for_archive(&mut app);
    assert!(app.search_query.is_none(), "{}", app.status);
    assert!(!app.show_archived);
    assert_eq!(app.focus, Focus::Terminal);
    assert_eq!(app.selected_session().unwrap().file, missing.file);
    assert!(
        app.pending.is_some(),
        "success queues the recovered source, not an inference call"
    );
    assert!(app.ptys.is_empty());
}

#[test]
fn mp_review_archive_selection_follows_source_across_page_boundary() {
    let mut app = tests::isolated_app();
    let mut rows: Vec<_> = (0..401)
        .map(|i| tests::session(&format!("archive-{i:03}"), Agent::Codex, true))
        .collect();
    app.archive_catalog = Arc::new(rows.clone());
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    app.selected = 199;
    let selected = app.selected_session().unwrap().clone();
    rows.push(tests::session("archive-000-new", Agent::Codex, true));
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            archive: rows.clone(),
            ..Default::default()
        }),
    ));
    wait_for_archive(&mut app);
    assert_eq!(
        app.archive_page, 1,
        "selection moves with source, not page offset"
    );
    assert_eq!(app.selected_session().unwrap().id, selected.id);
    rows.retain(|s| s.id != selected.id);
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            archive: rows,
            ..Default::default()
        }),
    ));
    wait_for_archive(&mut app);
    assert_eq!(
        app.selected, 0,
        "missing selection deterministically falls back to page start"
    );
    assert_ne!(app.selected_session().unwrap().id, selected.id);
}

#[test]
fn mp_review_archive_search_can_reach_all_filtered_pages() {
    let mut app = tests::isolated_app();
    app.archive_catalog = Arc::new(
        (0..450)
            .map(|i| tests::session(&format!("matching-{i:03}"), Agent::Codex, true))
            .collect(),
    );
    app.toggle_archived_view();
    app.begin_search();
    for ch in "matching".chars() {
        app.search_push(ch);
    }
    wait_for_archive(&mut app);
    let mut found = HashSet::new();
    for page in 0..3 {
        assert_eq!(app.archive_page, page);
        found.extend(app.visible_sessions().map(|s| s.id.clone()));
        if page < 2 {
            app.move_archive_page(true);
            wait_for_archive(&mut app);
        }
    }
    assert_eq!(
        found.len(),
        450,
        "all filtered results must remain reachable"
    );
    app.move_archive_page(false);
    wait_for_archive(&mut app);
    assert_eq!(app.archive_page, 1);
    assert_eq!(app.search_query.as_deref(), Some("matching"));
}

#[test]
fn mp_review_archive_refresh_keeps_same_id_source_not_just_native_id() {
    let mut app = tests::isolated_app();
    let mut one = tests::session("shared-native", Agent::Codex, true);
    one.file = PathBuf::from("/fixture/one.jsonl");
    let mut two = one.clone();
    two.file = PathBuf::from("/fixture/two.jsonl");
    app.archive_catalog = Arc::new(vec![one.clone(), two.clone()]);
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    app.selected = app
        .visible
        .iter()
        .position(|row| {
            matches!(row,
        Row::Session(index) if app.all_sessions[*index].file == two.file)
        })
        .unwrap();
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            archive: vec![two.clone(), one],
            ..Default::default()
        }),
    ));
    wait_for_archive(&mut app);
    assert_eq!(app.selected_session().unwrap().file, two.file);
}

#[test]
fn unchanged_refresh_does_not_rebuild_or_redraw() {
    let mut app = tests::isolated_app();
    let session = tests::session("unchanged", Agent::Codex, false);
    let update = ActivityUpdate {
        source: working_set::source_key(&session),
        last_active: session.last_active,
        last_prompt_at: session.last_prompt_at,
        context_pct: session.context_pct,
    };
    app.all_sessions.push(session);
    app.rebuild_visible();
    let (tx, rx) = mpsc::channel();
    tx.send(vec![update]).unwrap();
    app.refresh_rx = Some(rx);
    assert!(
        !app.poll_refresh(),
        "an unchanged refresh must not request a rebuild/redraw"
    );
}

#[test]
#[ignore = "release-profile performance evaluator"]
fn working_set_root_sort_budget() {
    let mut app = tests::isolated_app();
    app.all_sessions = (0..5_000)
        .map(|i| tests::session(&format!("session-{i:08}"), Agent::Codex, false))
        .collect();
    let start = Instant::now();
    app.rebuild_visible();
    let elapsed = start.elapsed();
    eprintln!(
        "working_set_root_sort rows=5000 elapsed_ms={:.3}",
        elapsed.as_secs_f64() * 1000.0
    );
    assert_eq!(app.visible_sessions().count(), 5_000);
    assert!(
        elapsed < Duration::from_millis(100),
        "5k root rebuild took {elapsed:?}"
    );
}

fn wait_for_archive(app: &mut App) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if app.poll_working_set() {
            break;
        }
        assert!(Instant::now() < until, "archive worker did not finish");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn archive_50k_is_paged_and_never_enters_usage_refresh() {
    let mut app = tests::isolated_app();
    app.all_sessions = (0..7)
        .map(|i| tests::session(&format!("active-{i}"), Agent::Codex, false))
        .collect();
    app.archive_catalog = Arc::new(
        (0..50_000)
            .map(|i| tests::session(&format!("archive-{i:08}"), Agent::Codex, true))
            .collect(),
    );
    app.rebuild_visible();
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    assert_eq!(app.archive_matches, 50_000);
    assert_eq!(app.all_sessions.len(), 7 + working_set::ARCHIVE_PAGE_SIZE);
    assert_eq!(
        app.visible_sessions().count(),
        working_set::ARCHIVE_PAGE_SIZE
    );
    app.start_refresh();
    let updates = app
        .refresh_rx
        .take()
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        updates.len(),
        7,
        "archive page must not enter detailed refresh"
    );
    assert!(updates.iter().all(|s| s.source.2.starts_with("active-")));
    app.move_archive_page(true);
    wait_for_archive(&mut app);
    assert_eq!(app.archive_page, 1);
    app.toggle_archived_view();
    assert_eq!(
        app.all_sessions.len(),
        7,
        "leaving archive releases its page"
    );
}

#[test]
fn archive_search_replaces_stale_query_and_respects_page_bound() {
    let mut app = tests::isolated_app();
    app.archive_catalog = Arc::new(
        (0..1_000)
            .map(|i| tests::session(&format!("arch-{i:04}"), Agent::Codex, true))
            .collect(),
    );
    app.toggle_archived_view();
    app.begin_search();
    for c in "arch-0999".chars() {
        app.search_push(c);
    }
    wait_for_archive(&mut app);
    assert_eq!(app.archive_matches, 1);
    assert_eq!(app.selected_session().unwrap().id, "arch-0999");
}

#[test]
fn changing_archive_query_cannot_resume_the_previous_result() {
    let mut app = tests::isolated_app();
    app.archive_catalog = Arc::new(vec![
        tests::session("previous-result", Agent::Codex, true),
        tests::session("next-result", Agent::Codex, true),
    ]);
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    assert!(app.selected_session().is_some());
    app.begin_search();
    for c in "next-result".chars() {
        app.search_push(c);
    }
    assert!(
        app.selected_session().is_none(),
        "pending query must not leave an old actionable row"
    );
    app.confirm_search();
    assert!(app.pending.is_none());
    assert_eq!(
        app.search_query.as_deref(),
        Some("next-result"),
        "Enter before results arrive must preserve the pending query"
    );
    wait_for_archive(&mut app);
    assert_eq!(app.selected_session().unwrap().id, "next-result");
}

#[test]
fn archive_metadata_is_available_to_thread_peers_outside_current_page() {
    let mut app = tests::isolated_app();
    app.all_sessions
        .push(tests::session("active", Agent::Codex, false));
    app.archive_catalog = Arc::new(vec![tests::session("old-parent", Agent::Claude, true)]);
    app.state.set_handoff_link(
        "active",
        "old-parent",
        PathBuf::from("/fixture/handoff.md"),
        Utc::now(),
    );
    assert_eq!(app.thread_peer_sessions("active").len(), 1);
    assert_eq!(app.known_metadata().count(), 2);
    assert_eq!(app.all_sessions.len(), 1);
}

#[test]
fn stale_catalog_cannot_reverse_a_human_promotion() {
    let mut app = tests::isolated_app();
    app.all_sessions
        .push(tests::session("chosen", Agent::Codex, false));
    let old_epoch = app.working_set.epoch;
    app.note_session_use("chosen");
    assert!(!app.apply_catalog((
        old_epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            archive: vec![tests::session("chosen", Agent::Codex, true)],
            ..Default::default()
        })
    )));
    assert_eq!(app.all_sessions.len(), 1);
    assert!(!app.all_sessions[0].archived);
    assert!(app.rescan_due.is_some());
}

#[test]
fn archive_page_source_identity_does_not_hide_another_account() {
    let mut app = tests::isolated_app();
    let mut first = tests::session("same-native-id", Agent::Codex, false);
    first.file = PathBuf::from("/fixture/account-one/session.jsonl");
    let mut second = first.clone();
    second.file = PathBuf::from("/fixture/account-two/session.jsonl");
    second.archived = true;
    app.all_sessions.push(first);
    app.archive_catalog = Arc::new(vec![second]);
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    assert_eq!(app.archive_matches, 1);
    let selected = app.selected_session().unwrap().clone();
    assert!(selected.file.ends_with("account-two/session.jsonl"));
    assert!(
        !app.can_resume_source(&selected),
        "never route a colliding id to an already active account"
    );
}

#[test]
fn hidden_routing_status_is_polled_in_bounded_fair_batches() {
    let mut app = tests::isolated_app();
    let directory = tests::fresh_login_test_home();
    for index in 0..10 {
        let id = format!("hidden-{index:02}");
        let channel =
            crate::local_router::RoutingStatusChannel::create_at(&directory.join("routing"))
                .unwrap();
        let mut status = serde_json::to_value(channel.snapshot().unwrap()).unwrap();
        status["revision"] = serde_json::json!(1);
        status["threadId"] = serde_json::json!(id);
        std::fs::write(channel.path(), serde_json::to_vec(&status).unwrap()).unwrap();
        let command = mindplayer_core::Command {
            program: "cat".into(),
            args: Vec::new(),
            cwd: directory.clone(),
            env: Default::default(),
        };
        let mut pty = PtySession::spawn(&command, &id, 6, 20).unwrap();
        pty.set_routing_status_for_test(channel);
        app.ptys.insert(id.clone(), pty);
        app.all_sessions
            .push(tests::session(&id, Agent::Codex, false));
    }
    assert!(app.panes.is_empty(), "all fixture children are hidden");
    app.poll_hook_status();
    assert_eq!(
        app.ptys
            .values()
            .filter(|p| p.routing_thread_id().is_some())
            .count(),
        4,
        "one status tick must not read every hidden routing file"
    );
    for _ in 0..2 {
        app.hook_status_due = None;
        app.poll_hook_status();
    }
    assert_eq!(
        app.ptys
            .values()
            .filter(|p| p.routing_thread_id().is_some())
            .count(),
        10,
        "round-robin polling must not starve hidden children"
    );
    assert!(
        app.ptys.values_mut().all(|p| p.is_alive()),
        "polling must never terminate hidden work"
    );
}

#[test]
fn catalog_resolves_pending_labels_against_archived_metadata() {
    let mut app = tests::isolated_app();
    let now = Utc::now();
    let mut archived = tests::session("closed-before-scan", Agent::Codex, true);
    archived.cwd = PathBuf::from("/fixture/project");
    archived.started_at = Some(now);
    app.state.set_archived(&archived.id, true);
    app.state
        .add_pending_label("codex", archived.cwd.clone(), now, "saved topic");
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            archive: vec![archived],
            ..Default::default()
        }),
    ));
    assert_eq!(
        app.state.label_for("closed-before-scan"),
        Some("saved topic")
    );
    assert!(app.state.pending_labels.is_empty());
    assert!(
        app.all_sessions.is_empty(),
        "label resolution must not hydrate archives"
    );
}

#[test]
fn catalog_cannot_retarget_a_live_native_id_to_another_account_source() {
    let mut app = tests::isolated_app();
    let directory = tests::fresh_login_test_home();
    let mut original = tests::session("live-owned-id", Agent::Codex, false);
    original.file = directory.join("original.jsonl");
    let mut other = original.clone();
    other.file = directory.join("other-account.jsonl");
    let command = mindplayer_core::Command {
        program: "cat".into(),
        args: Vec::new(),
        cwd: directory,
        env: Default::default(),
    };
    app.ptys.insert(
        original.id.clone(),
        PtySession::spawn(&command, &original.id, 6, 20).unwrap(),
    );
    app.all_sessions.push(original.clone());
    let mut loaded_archive = other.clone();
    loaded_archive.archived = true;
    app.all_sessions.push(loaded_archive);
    app.apply_catalog((
        app.working_set.epoch,
        Instant::now(),
        Ok(mindplayer_core::catalog::CatalogSnapshot {
            active: vec![other.clone()],
            ..Default::default()
        }),
    ));
    assert_eq!(
        app.all_sessions
            .iter()
            .find(|s| s.id == original.id)
            .unwrap()
            .file,
        original.file
    );
    assert!(
        app.archive_catalog.iter().any(|s| s.file == other.file),
        "keep the conflicting source discoverable"
    );
    assert!(app.ptys.get_mut(&original.id).unwrap().is_alive());
}

#[test]
fn archived_source_collision_cannot_close_a_different_live_owner() {
    let mut app = tests::isolated_app();
    let directory = tests::fresh_login_test_home();
    let mut live = tests::session("shared-id", Agent::Codex, false);
    live.file = directory.join("live.jsonl");
    let mut archived = live.clone();
    archived.file = directory.join("other.jsonl");
    archived.archived = true;
    let command = mindplayer_core::Command {
        program: "cat".into(),
        args: Vec::new(),
        cwd: directory,
        env: Default::default(),
    };
    app.ptys.insert(
        live.id.clone(),
        PtySession::spawn(&command, &live.id, 6, 20).unwrap(),
    );
    app.all_sessions = vec![live.clone(), archived];
    app.show_archived = true;
    app.rebuild_visible();
    app.close_selected();
    assert!(
        app.ptys.get_mut(&live.id).is_some_and(|p| p.is_alive()),
        "closing an archived collision must not kill the source-bound live owner"
    );
    assert!(!app.state.is_archived(&live.id));
}

#[test]
fn bulk_archive_selection_cannot_resolve_to_an_active_same_id_source() {
    let mut app = tests::isolated_app();
    let mut active = tests::session("shared-id", Agent::Codex, false);
    active.file = PathBuf::from("/fixture/active/source.jsonl");
    let mut archived = active.clone();
    archived.archived = true;
    archived.file = PathBuf::from("/fixture/other/source.jsonl");
    app.all_sessions = vec![active.clone(), archived];
    app.show_archived = true;
    app.rebuild_visible();
    app.marked.insert(active.id.clone());
    app.launch_marked();
    assert!(app.pending.is_none(), "must not queue the wrong source");
    assert!(
        app.panes.is_empty(),
        "must not focus the same-ID active source"
    );
    assert!(!app.state.last_used.contains_key(&active.id));
}

#[test]
fn in_progress_toggle_schedules_membership_reclassification() {
    let mut app = tests::isolated_app();
    app.all_sessions
        .push(tests::session("old-work", Agent::Codex, true));
    app.show_archived = true;
    app.rebuild_visible();
    app.toggle_in_progress();
    assert!(app.state.is_in_progress("old-work"));
    assert!(
        app.rescan_due.is_some(),
        "do not wait for the minute catalog timer"
    );
}

#[test]
fn synthetic_adoption_can_find_a_real_source_outside_the_current_page() {
    let mut app = tests::isolated_app();
    let now = Utc::now();
    let mut extra = tests::session("new:codex:fixture", Agent::Codex, false);
    extra.cwd = PathBuf::from("/fixture/project");
    extra.started_at = Some(now);
    let mut real = extra.clone();
    real.id = "real-native-id".into();
    real.file = PathBuf::from("/fixture/provider/real.jsonl");
    real.archived = true;
    app.extra_sessions.push(extra);
    app.archive_catalog = Arc::new(vec![real.clone()]);
    app.merge_extras();
    assert!(app.extra_sessions.is_empty());
    let active = app
        .all_sessions
        .iter()
        .find(|s| s.id == real.id)
        .expect("adopted Active detail row");
    assert!(!active.archived);
    assert_eq!(active.file, real.file);
    assert!(app.state.last_used.contains_key(&real.id));
}

#[test]
fn missing_archive_source_leaves_membership_and_launch_unchanged() {
    let mut app = tests::isolated_app();
    let mut missing = tests::session("missing-source", Agent::Codex, true);
    missing.file = tests::fresh_login_test_home().join("missing.jsonl");
    missing.cwd = PathBuf::from("/fixture/project");
    app.state.set_archived(&missing.id, true);
    app.archive_catalog = Arc::new(vec![missing]);
    app.toggle_archived_view();
    wait_for_archive(&mut app);
    app.request_resume();
    wait_for_archive(&mut app);
    assert!(
        app.status.contains("archive restore failed"),
        "{}",
        app.status
    );
    assert!(app.state.is_archived("missing-source"));
    assert!(!app.state.last_used.contains_key("missing-source"));
    assert!(app.pending.is_none());
    assert!(app.ptys.is_empty());
}

struct NativeSwitch {
    app: App,
    original: Session,
    target: Session,
    collision: Session,
}

impl NativeSwitch {
    fn new() -> Self {
        use mindplayer_core::accounts::Account;
        let directory = tests::fresh_login_test_home().join("native-switch");
        std::fs::create_dir_all(&directory).unwrap();
        let mut account = Account::isolated(&directory, Agent::Codex, "source").unwrap();
        let mut other = Account::isolated(&directory, Agent::Codex, "other").unwrap();
        // Display labels deliberately disagree with the immutable provider homes.
        account.name = "other".into();
        other.name = "source".into();
        let write_session = |id: &str, account: &Account, archived, tokens| {
            let mut session = tests::session(id, Agent::Codex, archived);
            session.cwd = directory.join("project");
            session.file = account.session_root(&directory).join(format!("{id}.jsonl"));
            session.started_at = Some(Utc::now() - chrono::Duration::days(60));
            session.last_prompt_at = Some(Utc::now() - chrono::Duration::days(30));
            std::fs::create_dir_all(session.file.parent().unwrap()).unwrap();
            let records = [
                serde_json::json!({"type":"session_meta","payload":{
                    "id":id,"cwd":session.cwd,"timestamp":session.started_at
                }}),
                serde_json::json!({"type":"response_item","timestamp":session.last_prompt_at,
                "payload":{"type":"message","role":"user","content":[{
                    "type":"input_text","text":"archived work"
                }]}}),
                serde_json::json!({"type":"event_msg","payload":{
                    "type":"token_count","info":{"total_token_usage":{
                        "input_tokens":tokens,"output_tokens":0,"total_tokens":tokens
                    }}
                }}),
            ];
            let text = records
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&session.file, format!("{text}\n")).unwrap();
            session
        };
        let original = write_session("native-origin", &account, false, 1);
        let target = write_session("native-target", &account, true, 42);
        let collision = write_session("native-target", &other, true, 999);
        let mut app = tests::isolated_app_at(directory.join("state.json"));
        app.cfg.codex_dir = directory.join("default/sessions");
        app.scope = Scope::Global;
        app.accounts = vec![account.clone(), other];
        app.all_sessions = vec![original.clone()];
        app.archive_catalog = Arc::new(vec![collision.clone(), target.clone()]);
        app.panes.push(original.id.clone());
        app.active = Some(original.id.clone());
        app.pane_accounts
            .insert(original.id.clone(), (Agent::Codex, account.name));
        let channel =
            crate::local_router::RoutingStatusChannel::create_at(&directory.join("routing"))
                .unwrap();
        let mut status = serde_json::to_value(channel.snapshot().unwrap()).unwrap();
        status["revision"] = serde_json::json!(1);
        status["threadId"] = serde_json::json!(target.id);
        std::fs::write(channel.path(), serde_json::to_vec(&status).unwrap()).unwrap();
        let command = mindplayer_core::Command {
            program: "cat".into(),
            args: Vec::new(),
            cwd: directory,
            env: Default::default(),
        };
        let mut pty = PtySession::spawn(&command, &original.id, 6, 20).unwrap();
        pty.set_routing_status_for_test(channel);
        app.ptys.insert(original.id.clone(), pty);
        app.rebuild_visible();
        Self {
            app,
            original,
            target,
            collision,
        }
    }

    fn wait_for_target(&mut self) {
        let until = Instant::now() + Duration::from_secs(5);
        while !self
            .app
            .all_sessions
            .iter()
            .any(|s| !s.archived && s.file == self.target.file && s.tokens == TokenUsage::default())
        {
            self.app.poll_working_set();
            self.app.hook_status_due = None;
            self.app.poll_hook_status();
            assert!(
                Instant::now() < until,
                "native target was not hydrated/promoted"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn native_hydration_preserves_observed_activity_by_source() {
    let mut fixture = NativeSwitch::new();
    let now = Utc::now() + chrono::Duration::minutes(1);
    let mut observed = fixture.target.clone();
    observed.last_active = Some(now);
    observed.last_prompt_at = Some(now);
    let mut collision = fixture.collision.clone();
    collision.last_active = Some(now + chrono::Duration::minutes(1));
    collision.last_prompt_at = collision.last_active;
    fixture.app.all_sessions.extend([observed, collision]);
    fixture.app.poll_hook_status();
    fixture.wait_for_target();
    let target = fixture
        .app
        .all_sessions
        .iter()
        .find(|s| working_set::source_key(s) == working_set::source_key(&fixture.target))
        .unwrap();
    assert!(!target.archived);
    assert_eq!(target.last_active, Some(now));
    assert_eq!(target.last_prompt_at, Some(now));
    assert_eq!(fixture.app.ptys.len(), 1, "only the fixture-owned cat PTY");
}

#[test]
fn native_switch_off_page_hydrates_without_rekeying_or_spawning() {
    let mut fixture = NativeSwitch::new();
    let mut catalog: Vec<_> = (0..250)
        .map(|i| {
            let mut row = tests::session(&format!("archive-{i:04}"), Agent::Codex, true);
            row.last_prompt_at = Some(Utc::now());
            row
        })
        .collect();
    catalog.extend([fixture.collision.clone(), fixture.target.clone()]);
    fixture.app.archive_catalog = Arc::new(catalog);
    fixture.app.toggle_archived_view();
    wait_for_archive(&mut fixture.app);
    assert!(!fixture
        .app
        .all_sessions
        .iter()
        .any(|s| s.id == fixture.target.id));
    let pane_account = fixture.app.pane_accounts[&fixture.original.id].clone();
    fixture.app.poll_hook_status();
    fixture.wait_for_target();
    assert_eq!(fixture.app.ptys.len(), 1);
    assert_eq!(fixture.app.panes, [fixture.original.id.clone()]);
    assert_eq!(
        fixture.app.active.as_deref(),
        Some(fixture.original.id.as_str())
    );
    assert_eq!(
        fixture.app.pane_accounts[&fixture.original.id],
        pane_account
    );
    assert!(fixture
        .app
        .ptys
        .get_mut(&fixture.original.id)
        .unwrap()
        .is_alive());
    assert!(fixture.app.pending.is_none());
    assert!(fixture.app.pending_queue.is_empty());
    assert!(fixture.app.state.last_used.contains_key(&fixture.target.id));
    assert_eq!(
        fixture.app.codex_turns[&fixture.original.id].file(),
        fixture.target.file
    );
}

#[test]
fn native_switch_rollout_never_uses_another_account_collision() {
    let mut fixture = NativeSwitch::new();
    fixture.app.all_sessions.push(fixture.collision.clone());
    fixture.app.poll_hook_status();
    assert!(
        fixture
            .app
            .codex_turns
            .get(&fixture.original.id)
            .is_none_or(|reader| reader.file() != fixture.collision.file),
        "a same-ID row on the current Archive page belongs to another provider home"
    );
    fixture.wait_for_target();
    assert_eq!(
        fixture.app.codex_turns[&fixture.original.id].file(),
        fixture.target.file
    );
    assert!(!fixture
        .app
        .all_sessions
        .iter()
        .any(|s| !s.archived && s.file == fixture.collision.file));
}

#[test]
fn native_switch_catalog_protects_the_verified_running_target() {
    let mut fixture = NativeSwitch::new();
    fixture.app.state.set_archived(&fixture.target.id, true);
    fixture.app.poll_hook_status();
    let (_, _, snapshot) = fixture
        .app
        .catalog_scan()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let snapshot = snapshot.unwrap();
    assert!(
        snapshot
            .active
            .iter()
            .any(|s| s.file == fixture.target.file),
        "the routed thread is running even though its PTY remains keyed by the original ID"
    );
    assert!(!snapshot
        .active
        .iter()
        .any(|s| s.file == fixture.collision.file));
}

#[test]
fn native_metadata_hydration_preserves_last_manual_measurement() {
    let mut fixture = NativeSwitch::new();
    fixture.app.manual_usage.insert(
        working_set::source_key(&fixture.target),
        mindplayer_core::catalog::UsageSnapshot {
            tokens: TokenUsage {
                total: 123,
                ..Default::default()
            },
            context_pct: Some(25.0),
            partial: true,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        fixture.app.poll_working_set();
        fixture.app.hook_status_due = None;
        fixture.app.poll_hook_status();
        if let Some(row) = fixture
            .app
            .all_sessions
            .iter()
            .find(|s| !s.archived && s.file == fixture.target.file)
        {
            assert_eq!(
                row.tokens.total, 123,
                "metadata must not overwrite measured tokens"
            );
            assert_eq!(row.context_pct, Some(25.0));
            break;
        }
        assert!(Instant::now() < deadline, "native target was not promoted");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fixture.app.ptys.len(), 1);
    assert!(fixture.app.pending.is_none());
}
