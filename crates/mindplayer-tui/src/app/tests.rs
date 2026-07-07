use super::handoff_sync::handoff_label;
use super::*;
use mindplayer_core::session::TokenUsage;
use std::path::PathBuf;
use std::sync::Mutex;

/// Serializes tests that set the process-global `MINDPLAYER_STATE` env var,
/// so concurrent tests can't clobber each other's sidecar path.
static STATE_ENV_LOCK: Mutex<()> = Mutex::new(());

fn session(id: &str, agent: Agent, archived: bool) -> Session {
    Session {
        id: id.into(),
        agent,
        cwd: PathBuf::new(),
        file: PathBuf::new(),
        started_at: None,
        last_active: None,
        tokens: TokenUsage::default(),
        title: id.into(),
        archived,
        is_subagent: false,
        context_pct: None,
    }
}

fn app_with(sessions: Vec<Session>) -> App {
    let mut app = App::new();
    app.all_sessions = sessions;
    app.rebuild_visible();
    app
}

fn session_in(id: &str, agent: Agent, cwd: &str, title: &str) -> Session {
    Session {
        id: id.into(),
        agent,
        cwd: PathBuf::from(cwd),
        file: PathBuf::new(),
        started_at: Some(chrono::Utc::now()),
        last_active: Some(chrono::Utc::now()),
        tokens: TokenUsage::default(),
        title: title.into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    }
}

fn write_handoff_fixture(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-app-handoff-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("claude.jsonl");
    std::fs::write(
            &transcript,
            r#"{"type":"user","message":{"role":"user","content":"continue deploy investigation"}}
{"type":"assistant","message":{"role":"assistant","content":"I found the failing health check in deploy.yaml."}}"#,
        )
        .unwrap();
    (dir, transcript)
}

fn write_codex_fixture(name: &str, text: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-app-codex-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("codex.jsonl");
    std::fs::write(
            &transcript,
            format!(
                r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{text}"}}]}}}}"#
            ),
        )
        .unwrap();
    (dir, transcript)
}

#[test]
fn new_session_persists_then_reconciles() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-newstate-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));

    // New labeled session shows up immediately (no disk file yet).
    app.request_new(Agent::Codex, "deploy check");
    assert_eq!(app.visible.len(), 1);
    let syn = app.session_at(0).unwrap();
    assert!(syn.id.starts_with("new:"));
    assert_eq!(syn.title, "🏷 deploy check");

    // A later scan discovers the real session (same agent/cwd, started after).
    let real = Session {
        id: "real-1234".into(),
        agent: Agent::Codex,
        cwd: PathBuf::from("/work"),
        file: PathBuf::new(),
        started_at: Some(chrono::Utc::now()),
        last_active: Some(chrono::Utc::now()),
        tokens: TokenUsage::default(),
        title: "deploy check".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    };
    app.all_sessions = vec![real];
    app.merge_extras();
    app.rebuild_visible();

    // Placeholder reconciled away; the real session remains.
    assert!(app.extra_sessions.is_empty());
    assert!(app.all_sessions.iter().all(|s| !s.id.starts_with("new:")));
    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.session_at(0).unwrap().id, "real-1234");

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn refresh_applies_token_updates_to_existing_row() {
    let mut app = app_with(vec![session("s1", Agent::Codex, false)]);
    assert_eq!(app.session_at(0).unwrap().tokens.total, 0);

    let (tx, rx) = mpsc::channel();
    tx.send(vec![ActivityUpdate {
        id: "s1".into(),
        last_active: Some(chrono::Utc::now()),
        tokens: TokenUsage {
            input: 7,
            cached: 2,
            output: 3,
            total: 10,
        },
        context_pct: None,
    }])
    .unwrap();
    app.refresh_rx = Some(rx);

    assert!(app.poll_refresh());
    assert_eq!(app.session_at(0).unwrap().tokens.total, 10);
    assert_eq!(app.visible_aggregate.codex.total, 10);
}

#[test]
fn new_session_stays_until_reconciled() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-newstate2-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.request_new(Agent::Claude, "");

    // A scan that finds nothing matching must NOT drop the new session.
    app.all_sessions = vec![session("unrelated", Agent::Codex, false)];
    app.merge_extras();
    app.rebuild_visible();
    assert_eq!(app.extra_sessions.len(), 1);
    assert!(app
        .all_sessions
        .iter()
        .any(|s| s.id.starts_with("new:claude")));

    std::env::remove_var("MINDPLAYER_STATE");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn visible_excludes_archived_by_default() {
    let app = app_with(vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Claude, true),
    ]);
    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.session_at(0).unwrap().id, "a");
}

#[test]
fn panes_cap_at_max_and_replace_focused_pane() {
    let mut app = App::new();
    let ids: Vec<String> = (0..MAX_PANES).map(|i| format!("s{i}")).collect();
    for id in &ids {
        app.focus_or_add_pane(id);
    }
    assert_eq!(app.panes, ids);
    assert_eq!(app.panes.len(), MAX_PANES);
    assert_eq!(app.focused_pane(), Some(ids.last().unwrap().as_str()));

    // Wrap focus back to the first pane, then a new pane replaces it once
    // the pane list is full (cap reached).
    app.cycle_focus();
    assert_eq!(app.focused_pane(), Some(ids[0].as_str()));
    app.focus_or_add_pane("new-overflow-pane");
    let mut expected = ids.clone();
    expected[0] = "new-overflow-pane".to_string();
    assert_eq!(app.panes, expected);
    assert_eq!(app.focused_pane(), Some("new-overflow-pane"));
}

#[test]
fn reorder_panes_by_status_is_quiet_unless_a_pane_is_actually_blocked() {
    let mut app = App::new();
    for id in ["a", "b", "c"] {
        app.focus_or_add_pane(id);
    }
    // None of these panes have a real pty, so none can classify as Blocked —
    // Ended/Inactive/Idle/Working no longer trigger a reorder on their own
    // (see bubble_urgent_to_front's unit tests for the actual sort logic).
    app.ended.insert("b".to_string());
    assert_eq!(app.focused_pane(), Some("c"));

    assert!(!app.reorder_panes_by_status());
    assert_eq!(
        app.panes,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert_eq!(app.focused_pane(), Some("c"));
}

#[test]
fn pane_focus_layout_and_close_update_active() {
    let mut app = App::new();
    app.focus_or_add_pane("a");
    app.focus_or_add_pane("b");
    assert_eq!(app.active.as_deref(), Some("b"));

    app.cycle_focus();
    assert_eq!(app.active.as_deref(), Some("a"));
    app.cycle_layout();
    assert_eq!(app.layout, PaneLayout::Vertical);

    app.close_focused_pane();
    assert_eq!(app.panes, vec!["b"]);
    assert_eq!(app.active.as_deref(), Some("b"));
    app.close_focused_pane();
    assert!(app.panes.is_empty());
    assert_eq!(app.active, None);
    assert_eq!(app.focus, Focus::List);
}

#[test]
fn typing_while_initial_input_pending_is_held_not_dropped() {
    let mut app = App::new();
    app.focus_or_add_pane("a");
    app.pending_initial_inputs.insert(
        "a".to_string(),
        DeferredInitialInput {
            bytes: b"handoff context".to_vec(),
            queued_at: Instant::now(),
            held_input: Vec::new(),
        },
    );

    app.send_to_pty(b"h");
    app.send_to_pty(b"i");
    assert_eq!(
        app.pending_initial_inputs.get("a").unwrap().held_input,
        b"hi"
    );
    assert!(app.status.contains("input is held"));

    assert!(app.paste_to_pty("more"));
    assert_eq!(
        app.pending_initial_inputs.get("a").unwrap().held_input,
        b"himore"
    );
}

#[test]
fn pane_selection_bounds_normalize_row_major() {
    // Anchor after cursor (drag up-left) normalizes to start <= end.
    let s = PaneSelection {
        pane_id: "x".to_string(),
        anchor: (3, 5),
        cursor: (1, 2),
    };
    assert_eq!(s.bounds(), (1, 2, 3, 5));
    // Same row, cursor before anchor.
    let s2 = PaneSelection {
        pane_id: "x".to_string(),
        anchor: (2, 8),
        cursor: (2, 3),
    };
    assert_eq!(s2.bounds(), (2, 3, 2, 8));
}

#[test]
fn plain_click_selection_does_not_copy() {
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    // anchor == cursor means the mouse went down and up without dragging.
    app.selection = Some(PaneSelection {
        pane_id: "a".to_string(),
        anchor: (2, 3),
        cursor: (2, 3),
    });
    let copied = app.selection_finish();
    assert!(!copied, "a click without dragging must not copy");
    assert!(app.selection.is_none(), "selection is cleared either way");
    assert!(
        app.pending_clipboard.is_none(),
        "nothing queued to clipboard"
    );
}

#[test]
fn cycle_focus_back_reverses_pane_focus() {
    let mut app = App::new();
    for id in ["a", "b", "c"] {
        app.focus_or_add_pane(id);
    }
    assert_eq!(app.focused_pane(), Some("c"));
    app.cycle_focus_back();
    assert_eq!(app.focused_pane(), Some("b"));
    app.cycle_focus_back();
    assert_eq!(app.focused_pane(), Some("a"));
    app.cycle_focus_back();
    assert_eq!(app.focused_pane(), Some("c"), "wraps around to last");
}

#[test]
fn toggle_zoom_toggles_and_resets_when_panes_close() {
    let mut app = App::new();

    // No panes open: toggling zoom is a no-op.
    app.toggle_zoom();
    assert!(!app.zoomed);

    app.focus_or_add_pane("a");
    app.focus_or_add_pane("b");
    app.toggle_zoom();
    assert!(app.zoomed);
    // Cycling focus while zoomed keeps zoom on (it should follow whichever
    // pane is now focused, not drop back to the split).
    app.cycle_focus();
    assert!(app.zoomed);
    app.toggle_zoom();
    assert!(!app.zoomed, "toggling again returns to the split view");

    // Zoom auto-resets once every pane is closed, so a fresh live view
    // never starts pre-zoomed.
    app.toggle_zoom();
    assert!(app.zoomed);
    app.close_focused_pane();
    app.close_focused_pane();
    assert!(app.panes.is_empty());
    assert!(!app.zoomed, "zoom resets once the last pane closes");
}

#[test]
fn drag_selection_starts_in_pane_under_mouse() {
    let mut app = App::new();
    app.focus_or_add_pane("left");
    app.focus_or_add_pane("right");
    app.focused = 0;
    app.sync_active();
    app.pane_bounds.insert("left".into(), (1, 1, 10, 20));
    app.pane_bounds.insert("right".into(), (25, 1, 10, 20));

    app.selection_start(30, 4);

    assert_eq!(app.focused_pane(), Some("right"));
    let selection = app.selection.as_ref().expect("selection started");
    assert_eq!(selection.pane_id, "right");
    assert_eq!(selection.anchor, (3, 5));
    assert_eq!(selection.cursor, (3, 5));
}

#[test]
fn session_display_name_prefers_label_then_title() {
    let mut session = session("session-abcdef", Agent::Codex, false);
    session.title = "raw transcript title".into();
    let mut app = app_with(vec![session]);

    assert_eq!(
        app.session_display_name("session-abcdef", 80),
        "raw transcript title"
    );

    app.state.set_label("session-abcdef", "customer migration");
    assert_eq!(
        app.session_display_name("session-abcdef", 80),
        "customer migration"
    );
    assert_eq!(app.session_display_name("session-abcdef", 9), "customer…");
}

#[test]
fn launch_marked_opens_all_marked_sessions_as_panes() {
    let now = chrono::Utc::now();
    let mut sessions = Vec::new();
    for id in ["a", "b", "c"] {
        let mut s = session(id, Agent::Codex, false);
        s.last_active = Some(now);
        sessions.push(s);
    }
    let mut app = app_with(sessions);

    app.selected = 0;
    app.toggle_mark(); // mark "a"
    app.selected = 2;
    app.toggle_mark(); // mark "c"
    assert_eq!(app.marked.len(), 2);

    app.launch_marked();
    assert!(app.marked.is_empty(), "marks cleared after launch");
    assert_eq!(app.focus, Focus::Terminal);
    assert_eq!(app.panes.len(), 2);
    assert!(app.panes.contains(&"a".to_string()));
    assert!(app.panes.contains(&"c".to_string()));
}

#[test]
fn launch_marked_falls_back_to_single_resume_when_nothing_marked() {
    let now = chrono::Utc::now();
    let mut s = session("solo", Agent::Codex, false);
    s.last_active = Some(now);
    let mut app = app_with(vec![s]);
    app.selected = 0;
    app.launch_marked();
    assert_eq!(app.panes, vec!["solo"]);
}

#[test]
fn enter_adds_session_to_the_live_view() {
    // Enter opens the selected session, ADDING it to the current live view
    // (so returning via ctrl-x and opening another grows the split); panes
    // are pruned individually with ctrl-q, not by replacing on each open.
    let now = chrono::Utc::now();
    let mut sessions = Vec::new();
    for id in ["a", "b", "c"] {
        let mut s = session(id, Agent::Codex, false);
        s.last_active = Some(now);
        sessions.push(s);
    }
    let mut app = app_with(sessions);

    app.selected = 0;
    app.request_resume();
    assert_eq!(app.panes, vec!["a"]);
    app.selected = 1;
    app.request_resume();
    assert_eq!(app.panes, vec!["a", "b"], "Enter adds to the live view");
    app.selected = 2;
    app.request_resume();
    assert_eq!(app.panes, vec!["a", "b", "c"]);
    // Re-opening one already shown just focuses it (no duplicate pane).
    app.selected = 0;
    app.request_resume();
    assert_eq!(app.panes, vec!["a", "b", "c"]);
    assert_eq!(app.focused, 0);
}

#[test]
fn recent_sessions_sort_above_older_regardless_of_agent() {
    let now = chrono::Utc::now();
    // Codex normally ranks above Kiro; an older Codex must still fall below
    // a Kiro session touched in the last 24h, because the "recent" category wins.
    let mut old_codex = session("old-codex", Agent::Codex, false);
    old_codex.last_active = Some(now - chrono::Duration::days(3));
    let mut recent_kiro = session("recent-kiro", Agent::Kiro, false);
    recent_kiro.last_active = Some(now);
    let mut app = app_with(vec![old_codex, recent_kiro]);
    app.rebuild_visible();

    let ids: Vec<_> = (0..app.visible.len())
        .map(|row| app.session_at(row).unwrap().id.as_str())
        .collect();
    assert_eq!(ids, vec!["recent-kiro", "old-codex"]);
    // Only the kiro session is recent, so the recent/older boundary is 1.
    assert_eq!(app.recent_count, 1);
}

#[test]
fn thread_root_time_reflects_freshest_lane_activity() {
    let now = chrono::Utc::now();
    // Orchestration parent whose own transcript is 2 weeks stale…
    let mut parent = session("p", Agent::Claude, false);
    parent.last_active = Some(now - chrono::Duration::weeks(2));
    // …but a lane was active today.
    let mut lane = session("c", Agent::Claude, false);
    lane.last_active = Some(now);
    let mut app = app_with(vec![parent, lane]);
    app.state
        .set_handoff_link("c", "p", PathBuf::from("/tmp/h.md"), now);
    app.rebuild_visible();

    let p = app
        .all_sessions
        .iter()
        .find(|s| s.id == "p")
        .unwrap()
        .clone();
    let (live, eff) = app.row_activity(&p, app.thread_child_count("p"));
    assert!(!live, "no live PTY in a unit test");
    assert_eq!(
        eff,
        Some(now),
        "parent's time reflects the lane's recent activity, not its own 2w mtime"
    );

    // A standalone session still uses its own activity.
    let c = app
        .all_sessions
        .iter()
        .find(|s| s.id == "c")
        .unwrap()
        .clone();
    let (_, eff_c) = app.row_activity(&c, app.thread_child_count("c"));
    assert_eq!(eff_c, Some(now));
}

#[test]
fn handoff_child_leaf_reflects_parent_activity_even_though_it_has_no_children() {
    // Regression: a handoff child (e.g. "(handoff)pulse") is a thread LEAF — it
    // has zero children of its own, so `thread_child_count` is 0. The old code
    // used that as a shortcut to skip the whole-thread scan and show only the
    // child's own (possibly long-stale) transcript mtime — even though the
    // parent it was handed off from was worked on minutes ago. A session with
    // a parent is just as much "part of a thread" as one with children.
    let now = chrono::Utc::now();
    let mut parent = session("parent", Agent::Claude, false);
    parent.last_active = Some(now); // touched moments ago
    let mut child = session("child", Agent::Codex, false);
    child.last_active = Some(now - chrono::Duration::hours(25)); // "1d" by itself
    let mut app = app_with(vec![parent, child]);
    app.state
        .set_handoff_link("child", "parent", PathBuf::from("/tmp/h.md"), now);
    app.rebuild_visible();

    let child = app
        .all_sessions
        .iter()
        .find(|s| s.id == "child")
        .unwrap()
        .clone();
    assert_eq!(
        app.thread_child_count("child"),
        0,
        "the child has no children of its own"
    );
    let (_, eff) = app.row_activity(&child, app.thread_child_count("child"));
    assert_eq!(
        eff,
        Some(now),
        "the child's row must reflect the parent's recent activity, not its own 25h-stale mtime"
    );
}

#[test]
fn search_filters_visible_sessions_by_label_or_title() {
    let mut labeled = session("a", Agent::Codex, false);
    labeled.title = "🏷 msk cohome".into();
    let mut titled = session("b", Agent::Claude, false);
    titled.title = "deploy rollback notes".into();
    let mut app = app_with(vec![labeled, titled]);

    app.begin_search();
    for c in "msk".chars() {
        app.search_push(c);
    }

    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.session_at(0).unwrap().id, "a");

    for _ in 0.."msk".len() {
        app.search_backspace();
    }
    for c in "rollback".chars() {
        app.search_push(c);
    }

    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.session_at(0).unwrap().id, "b");

    app.cancel_search();
    assert_eq!(app.visible.len(), 2);
}

#[test]
fn visible_groups_thread_roots_by_agent_type() {
    let now = chrono::Utc::now();
    // Keep every session within a few seconds of `now` so they're all
    // "recent" (the rolling 24h window, not a calendar day) and land in the
    // same band — this test checks agent grouping, not the recent/older split.
    let mut codex_old = session("codex-old", Agent::Codex, false);
    codex_old.last_active = Some(now - chrono::Duration::seconds(20));
    let mut codex_new = session("codex-new", Agent::Codex, false);
    codex_new.last_active = Some(now - chrono::Duration::seconds(10));
    let mut claude_parent = session("claude-parent", Agent::Claude, false);
    claude_parent.last_active = Some(now - chrono::Duration::seconds(5));
    let mut codex_child = session("codex-child", Agent::Codex, false);
    codex_child.last_active = Some(now);
    let mut kiro = session("kiro-one", Agent::Kiro, false);
    kiro.last_active = Some(now);

    let mut app = app_with(vec![kiro, claude_parent, codex_old, codex_new, codex_child]);
    app.state.set_handoff_link(
        "codex-child",
        "claude-parent",
        PathBuf::from("/tmp/handoff.md"),
        now,
    );
    app.rebuild_visible();

    let ids: Vec<_> = (0..app.visible.len())
        .map(|row| app.session_at(row).unwrap().id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "codex-new",
            "codex-old",
            "claude-parent",
            "codex-child",
            "kiro-one"
        ]
    );
}

#[test]
fn toggle_archived_view_swaps_set() {
    let mut app = app_with(vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Claude, true),
    ]);
    app.toggle_archived_view();
    assert!(app.show_archived);
    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.session_at(0).unwrap().id, "b");
}

#[test]
fn move_selection_wraps() {
    let mut app = app_with(vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Codex, false),
    ]);
    assert_eq!(app.selected, 0);
    app.move_selection(-1);
    assert_eq!(app.selected, 1, "wrap to last");
    app.move_selection(1);
    assert_eq!(app.selected, 0, "wrap to first");
}

#[test]
fn move_page_steps_and_clamps() {
    let mut app = app_with(
        (0..20)
            .map(|i| session(&format!("s{i}"), Agent::Codex, false))
            .collect(),
    );
    app.list_rows = 10; // PageUp/PageDown use a fixed 4-row step.
    assert_eq!(app.selected, 0);
    app.move_page(1);
    assert_eq!(app.selected, 4, "down one page step");
    app.move_page(1);
    assert_eq!(app.selected, 8, "down another page step");
    for _ in 0..4 {
        app.move_page(1);
    }
    assert_eq!(app.selected, 19, "clamp at last (no wrap)");
    app.move_page(-1);
    assert_eq!(app.selected, 15, "up one page step from last");
    app.move_page(-1);
    assert_eq!(app.selected, 11, "up another page step");
    app.move_page(-1);
    app.move_page(-1);
    app.move_page(-1);
    assert_eq!(app.selected, 0, "clamp at first (no wrap)");
}

#[test]
fn close_selected_archives_and_hides() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Redirect the sidecar write to a temp file so real state is untouched.
    let tmp = std::env::temp_dir().join(format!("mp-state-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = app_with(vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Codex, false),
    ]);
    app.selected = 0;
    app.close_selected();

    let saved = mindplayer_core::State::load_from(&tmp);
    assert!(saved.is_archived("a"), "archive persisted to sidecar");
    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
    assert!(
        app.all_sessions
            .iter()
            .find(|s| s.id == "a")
            .unwrap()
            .archived
    );
    assert!(app.visible.iter().all(|&i| app.all_sessions[i].id != "a"));
}

#[test]
fn toggle_in_progress_marks_persists_and_unmarks() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-inprog-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.selected = 0;

    app.toggle_in_progress();
    assert!(app.state.is_in_progress("a"));
    let saved = mindplayer_core::State::load_from(&tmp);
    assert!(saved.is_in_progress("a"), "mark persisted to sidecar");

    app.toggle_in_progress();
    assert!(!app.state.is_in_progress("a"));
    let saved = mindplayer_core::State::load_from(&tmp);
    assert!(!saved.is_in_progress("a"), "unmark persisted to sidecar");

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn begin_catchup_on_a_session_with_no_live_pty_leaves_no_confirm() {
    // No PTY registered → session_status reads Inactive, not one of the
    // Blocked/Working/Idle states this feature is scoped to — it should
    // explain why instead of resuming the session just to deliver a prompt.
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.selected = 0;
    app.begin_catchup();
    assert!(app.catchup_confirm.is_none());
    assert!(
        app.status.contains("live session"),
        "status should explain why nothing was sent: {}",
        app.status
    );
}

#[test]
fn cancel_catchup_clears_the_confirm_without_sending() {
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.catchup_confirm = Some("a".to_string());
    app.cancel_catchup();
    assert!(app.catchup_confirm.is_none());
}

#[test]
fn merge_extras_ignores_preexisting_session() {
    // Regression for the HIGH bug: a new session must never be reconciled
    // onto a session that already existed when it was created (e.g. one the
    // user just resumed in the same dir) — doing so would re-key its live
    // PTY over the running one and silently kill it.
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-merge-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let now = chrono::Utc::now();
    let pre = Session {
        id: "pre-real".into(),
        agent: Agent::Codex,
        cwd: PathBuf::from("/work"),
        file: PathBuf::new(),
        started_at: Some(now),
        last_active: Some(now),
        tokens: TokenUsage::default(),
        title: "already running".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    };

    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.all_sessions = vec![pre.clone()];
    app.rebuild_visible();

    // User starts a brand-new session in the SAME dir/agent.
    app.request_new(Agent::Codex, "");
    // A rescan: the new session's rollout file isn't on disk yet, so the
    // scan still only sees the pre-existing session.
    app.all_sessions = vec![pre];
    app.merge_extras();

    // The synthetic survives (not adopted onto the pre-existing session).
    assert_eq!(
        app.extra_sessions.len(),
        1,
        "new session not reconciled away"
    );
    assert!(app.all_sessions.iter().any(|s| s.id.starts_with("new:")));
    assert!(app.all_sessions.iter().any(|s| s.id == "pre-real"));

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn close_selected_keeps_cursor_on_neighbor() {
    // Regression: after archiving a middle row the cursor must land on a
    // deliberate neighbor by id, so a repeated 'x' can't archive+kill a
    // session the user never moved onto.
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-neigh-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = app_with(vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Codex, false),
        session("c", Agent::Codex, false),
    ]);
    app.selected = 1; // "b"
    app.close_selected();
    // "b" archived → visible [a, c]; cursor lands on the next neighbor "c".
    assert_eq!(app.selected_session().unwrap().id, "c");

    // Closing the last row falls back to the previous neighbor.
    app.selected = app
        .visible
        .iter()
        .position(|&i| app.all_sessions[i].id == "c")
        .unwrap();
    app.close_selected();
    assert_eq!(app.selected_session().unwrap().id, "a");

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn label_edit_sets_and_persists() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-label-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = app_with(vec![session("real-1", Agent::Codex, false)]);
    app.selected = 0;
    app.begin_label_edit();
    assert_eq!(app.label_target.as_deref(), Some("real-1"));
    assert_eq!(app.new_label.as_deref(), Some(""), "no existing label");

    for c in "deploy check".chars() {
        app.label_input_push(c);
    }
    app.confirm_label_edit();

    assert!(app.label_target.is_none() && app.new_label.is_none());
    assert_eq!(app.all_sessions[0].title, "🏷 deploy check");
    let saved = mindplayer_core::State::load_from(&tmp);
    assert_eq!(saved.label_for("real-1"), Some("deploy check"));

    // Re-opening pre-fills the existing label so it can be edited/cleared.
    app.begin_label_edit();
    assert_eq!(app.new_label.as_deref(), Some("deploy check"));

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn label_edit_skips_synthetic_placeholder() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-labelsyn-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.request_new(Agent::Codex, "");
    app.selected = 0; // the synthetic new: row
    app.begin_label_edit();
    // Synthetic placeholders use the new-session label flow, not this modal.
    assert!(app.label_target.is_none());
    assert!(app.new_label.is_none());

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn handoff_queues_target_agent_with_initial_prompt() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-handoff-label-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let (dir, transcript) = write_handoff_fixture("queue");
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut source = session_in(
        "claude-1",
        Agent::Claude,
        "/work/project",
        "finish deployment",
    );
    source.file = transcript;
    let mut app = app_with(vec![source]);
    app.state.set_label("claude-1", "msk cohome");

    app.begin_handoff();
    assert_eq!(app.handoff_picker, Some(0));
    app.confirm_handoff(Agent::Codex);

    let pending = app.pending.as_ref().expect("handoff queues PTY spawn");
    assert!(pending.session_id.starts_with("handoff:claude:codex:"));
    assert_eq!(pending.command.program, "codex");
    assert_eq!(pending.command.cwd, PathBuf::from("/work/project"));
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("from claude to codex"));
    assert!(input.contains("session id: claude-1"));
    assert!(input.contains("read the handoff artifact"));
    assert!(input.contains("continue deploy investigation"));
    assert!(input.contains("failing health check"));
    assert!(input.ends_with('\r'));
    assert!(app
        .all_sessions
        .iter()
        .any(|s| s.id.starts_with("handoff:claude:codex:") && s.title == "🏷 (handoff)msk cohome"));
    assert!(app.state.pending_labels.iter().any(|p| p.agent == "codex"
        && p.cwd == std::path::Path::new("/work/project")
        && p.label == "(handoff)msk cohome"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn handoff_into_kiro_sends_context_as_first_input_argument() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-handoff-kiro-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let (dir, transcript) = write_handoff_fixture("kiro-target");
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut source = session_in(
        "claude-1",
        Agent::Claude,
        "/work/project",
        "finish deployment",
    );
    source.file = transcript;
    let mut app = app_with(vec![source]);

    app.confirm_handoff(Agent::Kiro);

    let pending = app.pending.as_ref().expect("handoff queues PTY spawn");
    assert!(pending.session_id.starts_with("handoff:claude:kiro:"));
    assert_eq!(pending.command.program, "kiro-cli");
    assert!(
        pending
            .command
            .args
            .iter()
            .any(|arg| arg == "--trust-all-tools"),
        "kiro handoff must keep trusted-tools mode"
    );
    assert!(
        pending.initial_input.is_none(),
        "kiro gets the handoff as chat [INPUT], not delayed paste"
    );
    let first_input = pending.command.args.last().expect("first input argument");
    assert!(first_input.contains("from claude to kiro"));
    assert!(first_input.contains("continue deploy investigation"));
    assert!(first_input.contains("failing health check"));
    assert!(!first_input.ends_with('\r'));

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn kiro_handoff_to_codex_creates_child_lane() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-kiro-handoff-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let dir = std::env::temp_dir().join(format!("mindplayer-kiro-handoff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut source = session_in("kiro-1", Agent::Kiro, "/work/project", "test handoff");
    source.file = PathBuf::new();
    let mut app = app_with(vec![source]);
    app.state.set_label("kiro-1", "test handoff");

    app.begin_handoff();
    assert_eq!(app.handoff_picker, Some(0));
    assert!(app.status.contains("choose target"));
    app.confirm_handoff(Agent::Codex);

    let pending = app.pending.as_ref().expect("handoff queues PTY spawn");
    assert!(pending.session_id.starts_with("handoff:kiro:codex:"));
    assert_eq!(pending.command.program, "codex");
    assert_eq!(pending.command.args.len(), 0);
    assert_eq!(pending.command.cwd, PathBuf::from("/work/project"));
    assert!(pending.initial_input.is_some());
    assert_eq!(
        app.state.handoff_parent(&pending.session_id),
        Some("kiro-1")
    );
    assert!(app.state.pending_handoffs.iter().any(|p| {
        p.parent_id == "kiro-1"
            && p.agent == "codex"
            && p.cwd == std::path::Path::new("/work/project")
    }));
    assert_eq!(app.visible.len(), 2);
    assert_eq!(app.session_at(0).unwrap().id, "kiro-1");
    assert_eq!(app.session_at(1).unwrap().id, pending.session_id);
    assert_eq!(app.session_depth(&pending.session_id), 1);
    assert_eq!(app.thread_child_count("kiro-1"), 1);

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn orchestration_creates_main_and_child_lanes() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-orchestration-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/work/project"));

    app.begin_orchestration();
    app.confirm_orchestration_step();
    for c in "$ralplan".chars() {
        app.orchestration_input_push(c);
    }
    app.confirm_orchestration_step();
    for c in "compare options".chars() {
        app.orchestration_input_push(c);
    }
    app.confirm_orchestration_step();
    app.orchestration_input_push('2');
    app.confirm_orchestration_step();

    assert!(app.orchestration.is_none());
    assert_eq!(app.all_sessions.len(), 3);
    assert_eq!(app.visible.len(), 3);
    let main = app.session_at(0).unwrap();
    assert!(main.id.starts_with("orch:main:"));
    assert_eq!(main.title, "🏷 (orch codex)$ralplan");
    let main_id = main.id.clone();
    assert_eq!(app.session_depth(&main_id), 0);
    assert_eq!(app.thread_child_count(&main_id), 2);
    assert!(app
        .all_sessions
        .iter()
        .filter(|s| s.id.starts_with("orch:child:"))
        .all(|s| app.state.handoff_parent(&s.id) == Some(main_id.as_str())));
    assert_eq!(app.pending.as_ref().unwrap().session_id, main_id);
    assert_eq!(app.pending_queue.len(), 2);
    assert!(app.pending.as_ref().unwrap().focus_after_spawn);
    assert!(app.pending_queue.iter().all(|p| !p.focus_after_spawn));

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn orchestration_reconciles_synthetic_rows_to_real_usage_rows() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-orchestration-merge-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/work/project"));

    app.begin_orchestration();
    app.confirm_orchestration_step();
    app.orchestration_input_push('m');
    app.confirm_orchestration_step();
    for c in "task".chars() {
        app.orchestration_input_push(c);
    }
    app.confirm_orchestration_step();
    app.orchestration_input_push('1');
    app.confirm_orchestration_step();

    let main_synthetic = app.session_at(0).unwrap().id.clone();
    let child_synthetic = app.session_at(1).unwrap().id.clone();
    let real_main = Session {
        id: "real-main".into(),
        agent: Agent::Codex,
        cwd: PathBuf::from("/work/project"),
        file: PathBuf::from("/tmp/main.jsonl"),
        started_at: Some(chrono::Utc::now()),
        last_active: Some(chrono::Utc::now()),
        tokens: TokenUsage {
            input: 10,
            output: 5,
            cached: 0,
            total: 15,
        },
        title: "MindPlayer orchestration main session. Skill / mode to use:".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    };
    let real_child = Session {
        id: "real-child".into(),
        agent: Agent::Codex,
        cwd: PathBuf::from("/work/project"),
        file: PathBuf::from("/tmp/child.jsonl"),
        started_at: Some(chrono::Utc::now()),
        last_active: Some(chrono::Utc::now()),
        tokens: TokenUsage {
            input: 20,
            output: 5,
            cached: 0,
            total: 25,
        },
        title: "MindPlayer orchestration child lane #1. Skill / mode to use:".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    };
    app.all_sessions = vec![real_main, real_child];
    app.merge_extras();
    app.rebuild_visible();

    assert!(!app.all_sessions.iter().any(|s| s.id == main_synthetic));
    assert!(!app.all_sessions.iter().any(|s| s.id == child_synthetic));
    assert_eq!(app.state.handoff_parent("real-child"), Some("real-main"));
    assert_eq!(app.thread_child_count("real-main"), 1);
    assert_eq!(app.session_at(0).unwrap().id, "real-main");
    assert_eq!(app.session_at(1).unwrap().id, "real-child");
    assert_eq!(app.session_at(0).unwrap().tokens.total, 15);
    assert_eq!(app.session_at(1).unwrap().tokens.total, 25);
    assert_eq!(app.session_at(0).unwrap().title, "🏷 (orch codex)m");
    assert_eq!(app.session_at(1).unwrap().title, "🏷 (orch codex child 1)m");
    assert_eq!(app.state.label_for("real-main"), Some("(orch codex)m"));
    assert_eq!(
        app.state.label_for("real-child"),
        Some("(orch codex child 1)m")
    );

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn broadcast_queues_multiline_instruction_for_child_lanes() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-broadcast-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let mut app = app_with(vec![
        session_in("main", Agent::Codex, "/work/project", "(orch codex)mode"),
        session_in(
            "child-1",
            Agent::Codex,
            "/work/project",
            "(orch codex child 1)mode",
        ),
        session_in(
            "child-2",
            Agent::Codex,
            "/work/project",
            "(orch codex child 2)mode",
        ),
    ]);
    app.state.set_handoff_link(
        "child-1",
        "main",
        PathBuf::from("mindplayer-orchestration"),
        chrono::Utc::now(),
    );
    app.state.set_handoff_link(
        "child-2",
        "main",
        PathBuf::from("mindplayer-orchestration"),
        chrono::Utc::now(),
    );
    app.rebuild_visible();
    app.selected = 0;

    app.begin_broadcast();
    assert!(app.broadcast.is_some());
    app.broadcast_input_text("다음 사이클\nreview risks");
    app.confirm_broadcast();

    assert!(app.broadcast.is_none());
    let queued_ids = std::iter::once(app.pending.as_ref().unwrap().session_id.clone())
        .chain(app.pending_queue.iter().map(|p| p.session_id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        queued_ids,
        ["child-1".to_string(), "child-2".to_string()]
            .into_iter()
            .collect()
    );
    assert_eq!(app.pending_queue.len(), 1);
    assert!(app.pending_queue.iter().all(|p| !p.focus_after_spawn));
    let first =
        String::from_utf8(app.pending.as_ref().unwrap().initial_input.clone().unwrap()).unwrap();
    let second = String::from_utf8(app.pending_queue[0].initial_input.clone().unwrap()).unwrap();
    assert!(first.contains("cycle #2"));
    assert!(first.contains("다음 사이클\nreview risks"));
    assert!(second.contains("다음 사이클\nreview risks"));
    assert!(app.status.contains("cycle #2 broadcasted"));

    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn peer_review_cycle_queues_peer_context_for_child_lanes() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("mindplayer-peer-review-{}", std::process::id()));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in("main", Agent::Codex, "/work/project", "(orch codex)mode"),
        session_in(
            "child-1",
            Agent::Codex,
            "/work/project",
            "(orch codex child 1)mode",
        ),
        session_in(
            "child-2",
            Agent::Codex,
            "/work/project",
            "(orch codex child 2)mode",
        ),
    ]);
    let now = chrono::Utc::now();
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), now);
    app.state
        .set_handoff_link("child-2", "main", PathBuf::from("orch"), now);
    app.rebuild_visible();
    app.select_session_id("main");

    app.run_peer_review_cycle();

    assert_eq!(app.pending_queue.len(), 1);
    let queued_ids = std::iter::once(app.pending.as_ref().unwrap().session_id.clone())
        .chain(app.pending_queue.iter().map(|p| p.session_id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        queued_ids,
        ["child-1".to_string(), "child-2".to_string()]
            .into_iter()
            .collect()
    );
    let first =
        String::from_utf8(app.pending.as_ref().unwrap().initial_input.clone().unwrap()).unwrap();
    assert!(first.contains("MindPlayer orchestration peer context"));
    assert!(!first.contains("Before answering the user's next request"));
    assert!(first.contains("peer-review cycle #2"));
    assert!(first.contains("Do not implement"));
    assert!(app.status.contains("peer review cycle #2 sent"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
}

#[test]
fn peer_review_cycle_excludes_linked_non_orchestration_sessions() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-peer-review-filter-{}",
        std::process::id()
    ));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in("main", Agent::Codex, "/work/project", "(orch codex)mode"),
        session_in(
            "child-1",
            Agent::Codex,
            "/work/project",
            "(orch codex child 1)mode",
        ),
        session_in(
            "reviewer-old",
            Agent::Codex,
            "/work/project",
            "You are an adversarial reviewer for feature area P2",
        ),
    ]);
    let now = chrono::Utc::now();
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), now);
    app.state
        .set_handoff_link("reviewer-old", "main", PathBuf::from("old-review"), now);
    app.rebuild_visible();
    app.select_session_id("main");

    app.run_peer_review_cycle();

    let pending = app.pending.as_ref().expect("peer review queues child lane");
    assert_eq!(pending.session_id, "child-1");
    assert!(app.pending_queue.is_empty());
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("peer-review cycle #2"));
    assert!(!input.contains("adversarial reviewer"));
    assert!(!input.contains("reviewer-old"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
}

#[test]
fn dispatch_roster_recovers_child_lane_from_saved_label_when_title_changes() {
    let child = session_in(
        "child-1",
        Agent::Codex,
        "/work/project",
        "You are implementing dedicated portal tasks",
    );
    let mut app = app_with(vec![
        session_in("main", Agent::Codex, "/work/project", "(orch codex)mode"),
        child,
        session_in(
            "reviewer-old",
            Agent::Codex,
            "/work/project",
            "You are an adversarial reviewer for feature area P2",
        ),
    ]);
    let now = chrono::Utc::now();
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), now);
    app.state.set_label("child-1", "(orch codex child 1)mode");
    app.state
        .set_handoff_link("reviewer-old", "main", PathBuf::from("old-review"), now);

    let roster = app.child_lane_roster("main");

    assert!(roster.contains("- lane #1: codex child-1"));
    assert!(roster.contains("You are implementing dedicated portal tasks"));
    assert!(!roster.contains("reviewer-old"));
    assert!(!roster.contains("adversarial reviewer"));
}

#[test]
fn peer_review_cycle_falls_back_to_orchestration_titles_without_links() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-peer-review-title-fallback-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-peer-review-title-fallback-{}",
        std::process::id()
    ));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in(
            "real-main",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration main session. Provider: Claude Code",
        ),
        session_in(
            "real-child",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration child lane #6. Provider: Claude Code",
        ),
    ]);
    app.select_session_id("real-child");

    app.run_peer_review_cycle();

    assert_eq!(app.pending.as_ref().unwrap().session_id, "real-child");
    let input =
        String::from_utf8(app.pending.as_ref().unwrap().initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("MindPlayer orchestration peer context"));
    assert!(!input.contains("Before answering the user's next request"));
    assert!(input.contains("peer-review cycle #2"));
    assert!(app.status.contains("peer review cycle #2 sent"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn peer_review_cycle_includes_linked_and_title_fallback_lanes() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-peer-review-mixed-links-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-peer-review-mixed-links-{}",
        std::process::id()
    ));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in(
            "real-main",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration main session. Provider: Claude Code",
        ),
        session_in(
            "linked-child",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration child lane #1. Provider: Claude Code",
        ),
        session_in(
            "fallback-child",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration child lane #4. Provider: Claude Code",
        ),
    ]);
    app.state.set_handoff_link(
        "linked-child",
        "real-main",
        PathBuf::from("orch"),
        chrono::Utc::now(),
    );
    app.rebuild_visible();
    app.select_session_id("real-main");

    app.run_peer_review_cycle();

    let queued_ids = std::iter::once(app.pending.as_ref().unwrap().session_id.clone())
        .chain(app.pending_queue.iter().map(|p| p.session_id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        queued_ids,
        ["fallback-child".to_string(), "linked-child".to_string()]
            .into_iter()
            .collect()
    );
    assert!(app.status.contains("0 skipped"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn peer_review_cycle_repairs_missing_title_based_links() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-peer-review-repair-links-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-peer-review-repair-links-{}",
        std::process::id()
    ));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in(
            "real-main",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration main session. Provider: Claude Code",
        ),
        session_in(
            "fallback-child",
            Agent::Claude,
            "/work/project",
            "MindPlayer orchestration child lane #4. Provider: Claude Code",
        ),
    ]);
    app.select_session_id("real-main");

    app.run_peer_review_cycle();

    assert_eq!(
        app.state.handoff_parent("fallback-child"),
        Some("real-main")
    );
    assert!(app.status.contains("0 skipped"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn synthesis_cycle_queues_child_context_for_main_lane() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("mindplayer-synthesis-{}", std::process::id()));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in("main", Agent::Codex, "/work/project", "(orch codex)mode"),
        session_in(
            "child-1",
            Agent::Codex,
            "/work/project",
            "(orch codex child 1)mode",
        ),
    ]);
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.rebuild_visible();
    app.select_session_id("child-1");

    app.run_synthesis_cycle();

    let pending = app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, "main");
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("MindPlayer orchestration peer context"));
    assert!(!input.contains("Before answering the user's next request"));
    assert!(input.contains("synthesis cycle #1"));
    assert!(input.contains("latest child lane transcripts"));
    assert!(input.contains("files changed"));
    assert!(input.contains("latest observed\nimplementation results"));
    assert!(app.status.contains("synthesis cycle #1 sent"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
}

#[test]
fn synthesis_cycle_waits_until_child_lanes_are_idle() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir =
        std::env::temp_dir().join(format!("mindplayer-synthesis-wait-{}", std::process::id()));
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let mut app = app_with(vec![
        session_in("main", Agent::Codex, "/work/project", "(orch codex)mode"),
        session_in(
            "child-1",
            Agent::Codex,
            "/work/project",
            "(orch codex child 1)mode",
        ),
    ]);
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.rebuild_visible();
    app.select_session_id("main");
    app.pending = Some(PendingSpawn {
        command: mindplayer_core::new_session(Agent::Codex, PathBuf::from("/work/project")),
        session_id: "child-1".into(),
        initial_input: None,
        focus_after_spawn: false,
    });

    app.run_synthesis_cycle();

    assert_eq!(app.pending.as_ref().unwrap().session_id, "child-1");
    assert_eq!(app.pending_synthesis_root.as_deref(), Some("main"));
    assert!(app.status.contains("synthesis waiting for 1 child lanes"));

    app.pending = None;
    assert!(app.poll_pending_synthesis());

    let pending = app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, "main");
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("synthesis cycle #1"));
    assert!(app.pending_synthesis_root.is_none());

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
}

#[test]
fn main_dispatch_queues_routing_request_for_main_lane() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-main-dispatch-state-{}.json",
        std::process::id()
    ));
    let dir = std::env::temp_dir().join(format!("mindplayer-main-dispatch-{}", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));
    let (child_dir, child_transcript) =
        write_codex_fixture("main-dispatch-child", "child lane found stale UI wiring");
    let main = session_in("main", Agent::Codex, "/work/project", "(orch codex)mode");
    let mut child = session_in(
        "child-1",
        Agent::Codex,
        "/work/project",
        "(orch codex child 1)mode",
    );
    child.file = child_transcript;
    let mut app = app_with(vec![main.clone(), child]);
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.rebuild_visible();
    app.select_session_id("main");

    app.begin_main_dispatch();
    app.dispatch_input_text("route only the verification work");
    app.confirm_main_dispatch();

    let pending = app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, main.id);
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.starts_with("MindPlayer orchestration dispatch planning cycle #2"));
    assert!(!input.contains("MindPlayer orchestration peer context"));
    assert!(!input.contains("MindPlayer thread sync"));
    assert!(!input.contains("Before answering the user's next request"));
    assert!(input.contains("dispatch planning cycle #2"));
    assert!(input.contains("route only the verification work"));
    assert!(input.contains("MINDPLAYER_DISPATCH"));
    assert!(input.contains("lane #1"));
    assert!(!input.contains("Full peer context artifact"));
    assert!(app.status.contains("press M after main answers"));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(child_dir);
    let _ = std::fs::remove_file(tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn apply_main_dispatch_targets_only_lanes_in_dispatch_block() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-apply-dispatch-state-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let dir =
        std::env::temp_dir().join(format!("mindplayer-apply-dispatch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("main.jsonl");
    let dispatch_text = "\
Planning result.

MINDPLAYER_DISPATCH
lane #1:
idle
lane #2:
Implement the focused fix and report tests.
END_MINDPLAYER_DISPATCH";
    std::fs::write(
        &transcript,
        serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": dispatch_text}]
            }
        })
        .to_string(),
    )
    .unwrap();
    let original = session_in(
        "original-pulse",
        Agent::Claude,
        "/work/project",
        "pulse original handoff source",
    );
    let mut main = session_in("main", Agent::Codex, "/work/project", "(orch codex)mode");
    main.file = transcript;
    let child1 = session_in(
        "child-1",
        Agent::Codex,
        "/work/project",
        "(orch codex child 1)mode",
    );
    let child2 = session_in(
        "child-2",
        Agent::Codex,
        "/work/project",
        "Implementing backend wiring",
    );
    let mut app = app_with(vec![original, main, child1, child2]);
    app.state.set_handoff_link(
        "main",
        "original-pulse",
        PathBuf::from("handoff"),
        chrono::Utc::now(),
    );
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.state
        .set_handoff_link("child-2", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.state.set_label("child-2", "(orch codex child 2)mode");
    app.rebuild_visible();
    app.select_session_id("main");

    app.apply_main_dispatch();

    let pending = app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, "child-2");
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("dispatch cycle #1 for child lane #2"));
    assert!(input.contains("Implement the focused fix and report tests."));
    assert!(app.pending_queue.is_empty());
    assert!(app.status.contains("1 lanes targeted"));

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn manual_dispatch_apply_pastes_block_and_targets_matching_lane() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-manual-dispatch-state-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let main = session_in("main", Agent::Codex, "/work/project", "(orch codex)mode");
    let child1 = session_in(
        "child-1",
        Agent::Codex,
        "/work/project",
        "(orch codex child 1)mode",
    );
    let child2 = session_in(
        "child-2",
        Agent::Codex,
        "/work/project",
        "(orch codex child 2)mode",
    );
    let mut app = app_with(vec![main, child1, child2]);
    app.state
        .set_handoff_link("child-1", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.state
        .set_handoff_link("child-2", "main", PathBuf::from("orch"), chrono::Utc::now());
    app.rebuild_visible();
    app.select_session_id("main");

    app.begin_dispatch_apply_input();
    assert!(app.dispatch_apply.is_some());
    app.dispatch_apply_input_text(
        "\
MINDPLAYER_DISPATCH
lane #1:
idle
lane #2:
Implement only the incident chip fix.
END_MINDPLAYER_DISPATCH",
    );
    app.confirm_dispatch_apply_input();

    assert!(app.dispatch_apply.is_none());
    let pending = app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, "child-2");
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("Implement only the incident chip fix."));
    assert!(app.status.contains("1 lanes targeted"));

    let _ = std::fs::remove_file(tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn apply_main_dispatch_falls_back_to_matching_orchestration_main_with_block() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!(
        "mp-apply-dispatch-fallback-state-{}.json",
        std::process::id()
    ));
    std::env::set_var("MINDPLAYER_STATE", &tmp);
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-apply-dispatch-fallback-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let empty_transcript = dir.join("empty-main.jsonl");
    let dispatch_transcript = dir.join("dispatch-main.jsonl");
    std::fs::write(
        &empty_transcript,
        serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "no dispatch here"}]
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
            &dispatch_transcript,
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "MINDPLAYER_DISPATCH\nlane #2:\nDo the focused work.\nEND_MINDPLAYER_DISPATCH"}]
                }
            })
            .to_string(),
        )
        .unwrap();
    let mut selected_main = session_in(
        "selected-main",
        Agent::Codex,
        "/work/project",
        "(orch codex)old",
    );
    selected_main.file = empty_transcript;
    selected_main.last_active = Some(chrono::Utc::now() - chrono::Duration::minutes(10));
    let mut dispatch_main = session_in(
        "dispatch-main",
        Agent::Codex,
        "/work/project",
        "(orch codex)new",
    );
    dispatch_main.file = dispatch_transcript;
    dispatch_main.last_active = Some(chrono::Utc::now());
    let selected_child = session_in(
        "selected-child",
        Agent::Codex,
        "/work/project",
        "(orch codex child 1)old",
    );
    let dispatch_child = session_in(
        "dispatch-child",
        Agent::Codex,
        "/work/project",
        "(orch codex child 2)new",
    );
    let mut app = app_with(vec![
        selected_main,
        selected_child,
        dispatch_main,
        dispatch_child,
    ]);
    app.state.set_handoff_link(
        "selected-child",
        "selected-main",
        PathBuf::from("orch"),
        chrono::Utc::now(),
    );
    app.state.set_handoff_link(
        "dispatch-child",
        "dispatch-main",
        PathBuf::from("orch"),
        chrono::Utc::now(),
    );
    app.rebuild_visible();
    app.select_session_id("selected-main");

    app.apply_main_dispatch();

    let pending = app.pending.as_ref().unwrap();
    assert_eq!(pending.session_id, "dispatch-child");
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("Do the focused work."));
    assert!(app.status.contains("1 lanes targeted"));

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(tmp);
    std::env::remove_var("MINDPLAYER_STATE");
}

#[test]
fn modal_paste_supports_multiline_text() {
    let mut app = App::new();
    app.begin_orchestration();
    app.confirm_orchestration_step();
    assert!(app.paste_to_modal("mode"));
    app.confirm_orchestration_step();
    assert!(app.paste_to_modal("한글 instruction\nenglish line"));
    assert_eq!(
        app.orchestration.as_ref().unwrap().instruction,
        "한글 instruction\nenglish line"
    );

    app.cancel_orchestration();
    app.broadcast = Some(orchestration::BroadcastDraft::default());
    assert!(app.paste_to_modal("방송\nbroadcast"));
    assert_eq!(
        app.broadcast.as_ref().unwrap().instruction,
        "방송\nbroadcast"
    );

    app.broadcast = None;
    app.dispatch = Some(orchestration::BroadcastDraft::default());
    assert!(app.paste_to_modal("라우팅\nroute"));
    assert_eq!(app.dispatch.as_ref().unwrap().instruction, "라우팅\nroute");
}

#[test]
fn handoff_child_is_grouped_under_parent_thread() {
    let mut parent = session_in("claude-1", Agent::Claude, "/work/project", "msk cohome");
    parent.last_active = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    let child = session_in(
        "codex-1",
        Agent::Codex,
        "/work/project",
        "(handoff)msk cohome",
    );
    let mut app = app_with(vec![child, parent]);
    app.state.set_handoff_link(
        "codex-1",
        "claude-1",
        PathBuf::from("/tmp/handoff.md"),
        chrono::Utc::now(),
    );
    app.rebuild_visible();

    assert_eq!(app.visible.len(), 2);
    assert_eq!(app.session_at(0).unwrap().id, "claude-1");
    assert_eq!(app.session_at(1).unwrap().id, "codex-1");
    assert_eq!(app.session_depth("claude-1"), 0);
    assert_eq!(app.session_depth("codex-1"), 1);
    assert_eq!(app.thread_child_count("claude-1"), 1);
}

#[test]
fn resuming_thread_lane_injects_peer_context() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (dir, codex_transcript) = write_codex_fixture("sync", "codex fixed tests");
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));

    let mut parent = session_in("claude-1", Agent::Claude, "/work/project", "msk cohome");
    parent.file = dir.join("claude.jsonl");
    let mut child = session_in(
        "codex-1",
        Agent::Codex,
        "/work/project",
        "(handoff)msk cohome",
    );
    child.file = codex_transcript;
    let mut app = app_with(vec![parent, child]);
    app.state.set_handoff_link(
        "codex-1",
        "claude-1",
        PathBuf::from("/tmp/handoff.md"),
        chrono::Utc::now(),
    );
    app.rebuild_visible();
    app.selected = 0;

    app.request_resume();

    let pending = app.pending.as_ref().expect("resume queues PTY spawn");
    assert_eq!(pending.session_id, "claude-1");
    let input = String::from_utf8(pending.initial_input.clone().unwrap()).unwrap();
    assert!(input.contains("MindPlayer thread sync"));
    assert!(input.contains("codex fixed tests"));
    assert!(input.ends_with('\r'));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn thread_sync_needed_fires_once_then_stays_quiet_even_as_peer_keeps_working() {
    // Regression: a plain 1:1 handoff must only get its peer-context summary
    // the first time you resume back into it. The old logic compared the
    // peer's last-active timestamp against the last sync time, which kept
    // re-triggering forever because the session you handed off FROM keeps
    // advancing while you keep working in it — reported as "a summary
    // prompt gets injected every single time I re-enter the handoff pane."
    let app = App::new();
    let mut peer = session("main-session", Agent::Claude, false);
    peer.last_active = Some(chrono::Utc::now());
    let peers = vec![peer.clone()];

    assert!(
        app.thread_sync_needed("handoff-target", &peers),
        "never synced before: the first entry should sync"
    );

    let mut app = app;
    app.thread_sync_at
        .insert("handoff-target".to_string(), chrono::Utc::now());

    // The peer (main session) keeps producing new activity well after the
    // sync — this must NOT re-trigger another summary injection.
    peer.last_active = Some(chrono::Utc::now() + chrono::Duration::hours(1));
    let peers = vec![peer];
    assert!(
        !app.thread_sync_needed("handoff-target", &peers),
        "already synced once: must not re-fire on later re-entries"
    );
}

#[test]
fn resuming_orchestration_main_does_not_auto_submit_thread_sync() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (dir, child_transcript) = write_codex_fixture("orch-sync", "child finished work");
    std::env::set_var(handoff::HANDOFF_DIR_ENV, dir.join("handoffs"));

    let mut main = session_in(
        "main",
        Agent::Codex,
        "/work/project",
        "MindPlayer orchestration main session. Provider: Codex",
    );
    main.file = dir.join("main.jsonl");
    let mut child = session_in(
        "child-1",
        Agent::Codex,
        "/work/project",
        "MindPlayer orchestration child lane #1. Provider: Codex",
    );
    child.file = child_transcript;
    let mut app = app_with(vec![main, child]);
    app.state.set_handoff_link(
        "child-1",
        "main",
        PathBuf::from("mindplayer-orchestration"),
        chrono::Utc::now(),
    );
    app.rebuild_visible();
    app.select_session_id("main");

    app.request_resume();

    let pending = app.pending.as_ref().expect("resume queues PTY spawn");
    assert_eq!(pending.session_id, "main");
    assert!(pending.initial_input.is_none());

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn status_rank_orders_by_urgency() {
    // herdr-style rollup: most urgent (blocked) first, finished (done) last.
    use SessionStatus::*;
    assert!(status_rank(Blocked) < status_rank(Working));
    assert!(status_rank(Working) < status_rank(Idle));
    assert!(status_rank(Idle) < status_rank(Inactive));
    assert!(status_rank(Inactive) < status_rank(Ended));
}

#[test]
fn working_hold_keeps_status_through_brief_silence() {
    let now = Instant::now();
    // Just produced output → working.
    assert!(working_within_hold(Some(now), now, WORKING_HOLD));
    // Quiet for less than the hold → still working (hysteresis, no bounce).
    assert!(working_within_hold(
        Some(now - Duration::from_secs(3)),
        now,
        WORKING_HOLD
    ));
    // Quiet past the hold → no longer working.
    assert!(!working_within_hold(
        Some(now - WORKING_HOLD - Duration::from_secs(1)),
        now,
        WORKING_HOLD
    ));
    // Never produced output → not working.
    assert!(!working_within_hold(None, now, WORKING_HOLD));
}

#[test]
fn trusted_busy_marker_overrides_visible_idle_prompt() {
    let now = Instant::now();

    assert_eq!(
        classify_live_session_status(false, true, true, Some(now - Duration::from_secs(1)), now),
        SessionStatus::Working
    );
    assert_eq!(
        classify_live_session_status(
            false,
            true,
            true,
            Some(now - BUSY_TRUST - Duration::from_secs(1)),
            now
        ),
        SessionStatus::Idle
    );
}

#[test]
fn idle_prompt_overrides_recent_non_busy_output() {
    let now = Instant::now();

    assert_eq!(
        classify_live_session_status(false, true, false, Some(now), now),
        SessionStatus::Idle
    );
    assert_eq!(
        classify_live_session_status(false, false, false, Some(now), now),
        SessionStatus::Working
    );
}

#[test]
fn blocked_prompt_has_status_priority() {
    let now = Instant::now();

    assert_eq!(
        classify_live_session_status(true, true, true, Some(now), now),
        SessionStatus::Blocked
    );
}

#[test]
fn initial_terminal_paint_does_not_count_as_working_activity() {
    assert!(!should_stamp_activity(false, false));
    assert!(should_stamp_activity(true, false));
    assert!(should_stamp_activity(false, true));
}

#[test]
fn handoff_label_prefixes_once() {
    assert_eq!(
        handoff_label("msk cohome").as_deref(),
        Some("(handoff)msk cohome")
    );
    assert_eq!(
        handoff_label("(handoff)msk cohome").as_deref(),
        Some("(handoff)msk cohome")
    );
    assert_eq!(handoff_label("   "), None);
}

#[test]
fn only_submit_keys_mark_user_turn_submitted() {
    assert!(!input_submits_turn(b"a"));
    assert!(!input_submits_turn(b"\x1b[A"));
    assert!(input_submits_turn(b"\r"));
    assert!(input_submits_turn(b"hello\n"));
}

#[test]
fn initial_input_waits_for_prompt() {
    assert!(should_send_initial_input(
        true,
        0,
        Duration::from_millis(10)
    ));
    assert!(!should_send_initial_input(false, 0, Duration::from_secs(3)));
    assert!(should_send_initial_input(false, 1, Duration::from_secs(3)));
    assert!(should_send_initial_input(false, 0, Duration::from_secs(10)));
}

#[test]
fn busy_marker_is_only_trusted_while_output_is_recent() {
    // A screen "busy" marker is frozen at the last output, so it must be
    // gated on output recency: trusted within BUSY_TRUST, ignored after.
    let now = Instant::now();
    assert!(
        BUSY_TRUST > WORKING_HOLD,
        "busy grace must exceed the work hold"
    );
    // Just-finished turn with a marker still on screen → trust it.
    assert!(working_within_hold(
        Some(now - Duration::from_secs(5)),
        now,
        BUSY_TRUST
    ));
    // Finished long ago (e.g. 6 min) with a stale marker → do NOT trust it,
    // so the session reads idle/done instead of "working" forever.
    assert!(!working_within_hold(
        Some(now - Duration::from_secs(360)),
        now,
        BUSY_TRUST
    ));
}

#[test]
fn ended_sessions_do_not_keep_recent_activity_alive() {
    let mut app = App::new();
    app.ended.insert("done".into());
    app.out_at.insert("done".into(), Instant::now());

    assert!(
        !app.any_recent_activity(),
        "ended PTYs keep their final frame, but must not keep working redraws alive"
    );
}

#[test]
fn dir_input_repoints_scope_to_valid_dir() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-dirstate-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    // A real directory that exists on every machine.
    let target = std::env::temp_dir();
    let mut app = App::new();
    app.begin_dir_input();
    assert!(app.dir_input.is_some());
    // Replace the prefilled buffer with the target path.
    app.dir_input = Some(target.display().to_string());
    app.confirm_dir_input();

    assert!(app.dir_input.is_none(), "modal closes on success");
    match &app.scope {
        Scope::WorkingDir(p) => {
            assert_eq!(p, &target.canonicalize().unwrap_or(target.clone()));
        }
        other => panic!("expected WorkingDir scope, got {other:?}"),
    }
}

#[test]
fn dir_input_rejects_nonexistent_dir() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-dirstate2-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = App::new();
    let original = app.scope.clone();
    app.begin_dir_input();
    app.dir_input = Some("/no/such/path/mindplayer-xyz".to_string());
    app.confirm_dir_input();

    // Invalid path: scope unchanged and modal stays open for correction.
    assert!(app.dir_input.is_some(), "modal stays open on bad path");
    assert_eq!(format!("{:?}", app.scope), format!("{original:?}"));
}

#[test]
fn dir_input_blank_switches_to_global() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-dirstate3-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &tmp);

    let mut app = App::new();
    app.begin_dir_input();
    app.dir_input = Some("   ".to_string());
    app.confirm_dir_input();

    assert!(app.dir_input.is_none());
    assert!(matches!(app.scope, Scope::Global));
}
