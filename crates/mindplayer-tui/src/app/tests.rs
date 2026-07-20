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

/// A codex transcript with `line_count` turns, each padded to roughly
/// `line_len_bytes` — big enough that parsing it synchronously would take
/// measurable wall-clock time, which is exactly what the freeze regression
/// test below needs to be able to detect.
fn write_large_codex_fixture(
    name: &str,
    line_count: usize,
    line_len_bytes: usize,
) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mindplayer-app-codex-large-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("codex.jsonl");
    let padding = "x".repeat(line_len_bytes);
    let mut out = String::new();
    for i in 0..line_count {
        out.push_str(&format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"turn {i} {padding}"}}]}}}}"#
        ));
        out.push('\n');
    }
    std::fs::write(&transcript, out).unwrap();
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
fn launch_marked_clears_a_leftover_zoom_so_all_panes_are_actually_visible() {
    let now = chrono::Utc::now();
    let mut sessions = Vec::new();
    for id in ["a", "b"] {
        let mut s = session(id, Agent::Codex, false);
        s.last_active = Some(now);
        sessions.push(s);
    }
    let mut app = app_with(sessions);
    // Simulate a zoom left on from earlier in the session — with it still
    // set, a multi-launch would silently render only the focused pane full
    // screen, making it look like just one session opened.
    app.zoomed = true;

    app.selected = 0;
    app.toggle_mark();
    app.selected = 1;
    app.toggle_mark();
    app.launch_marked();

    assert_eq!(app.panes.len(), 2);
    assert!(
        !app.zoomed,
        "a multi-session launch must show the split view"
    );
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
fn toggle_html_preview_without_a_pane_is_a_noop() {
    // No focused pane → nothing to preview into; the popup must not open.
    let mut app = App::new();
    app.toggle_html_preview();
    assert!(app.html_preview_input.is_none());
    assert!(app.previewing.is_empty());
}

#[test]
fn toggle_html_preview_opens_popup_when_no_preview_exists() {
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.focus = Focus::Terminal;
    app.toggle_html_preview();
    assert_eq!(
        app.html_preview_input.as_deref(),
        Some(""),
        "first Ctrl-P with no live preview opens the path popup"
    );
    assert!(app.html_preview_error.is_none());
    assert!(!app.previewing.contains("s1"));
}

#[test]
fn toggle_html_preview_hides_a_shown_preview_without_a_popup() {
    // A pane already showing its preview toggles back to the agent view with no
    // popup and without killing (dropping) the carbonyl process entry.
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.focus = Focus::Terminal;
    app.previewing.insert("s1".to_string());

    app.toggle_html_preview();
    assert!(
        !app.previewing.contains("s1"),
        "toggling a shown preview switches back to the agent view"
    );
    assert!(
        app.html_preview_input.is_none(),
        "hiding a preview must not open the popup"
    );
}

#[test]
fn confirm_html_preview_with_a_bad_path_sets_error_and_keeps_popup_open() {
    // A nonexistent path must set the inline error, leave the popup open, and
    // spawn no process — the whole point of the in-popup validation.
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.focus = Focus::Terminal;
    app.html_preview_input = Some("/definitely/not/a/real/file.html".to_string());

    app.confirm_html_preview();

    assert!(
        app.html_preview_error.is_some(),
        "a bad path must surface an inline error"
    );
    assert!(
        app.html_preview_input.is_some(),
        "the popup stays open so the path can be corrected"
    );
    assert!(
        app.preview_ptys.is_empty(),
        "no carbonyl spawned on a bad path"
    );
    assert!(!app.previewing.contains("s1"));
}

#[test]
fn confirm_html_preview_with_a_blank_path_sets_error() {
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.focus = Focus::Terminal;
    app.html_preview_input = Some("   ".to_string());
    app.confirm_html_preview();
    assert!(app.html_preview_error.is_some());
    assert!(app.html_preview_input.is_some());
    assert!(app.preview_ptys.is_empty());
}

#[test]
fn editing_the_preview_path_clears_a_stale_error() {
    let mut app = App::new();
    app.html_preview_input = Some("/bad".to_string());
    app.html_preview_error = Some("not a file: /bad".to_string());
    app.html_preview_input_push('x');
    assert!(app.html_preview_error.is_none(), "typing clears the error");

    app.html_preview_error = Some("again".to_string());
    app.html_preview_input_backspace();
    assert!(
        app.html_preview_error.is_none(),
        "backspace clears the error too"
    );
}

#[test]
fn cancel_html_preview_clears_input_and_error_without_side_effects() {
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.html_preview_input = Some("/some/path".to_string());
    app.html_preview_error = Some("boom".to_string());
    app.cancel_html_preview();
    assert!(app.html_preview_input.is_none());
    assert!(app.html_preview_error.is_none());
    assert!(app.preview_ptys.is_empty());
    assert!(app.previewing.is_empty());
}

fn temp_html_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mindplayer-html-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn poll_html_candidates_finds_html_and_skips_vendor_dirs_at_any_depth() {
    let dir = temp_html_dir("scan");
    std::fs::write(dir.join("page.html"), "<html></html>").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("nested.html"), "<html></html>").unwrap();
    // node_modules directly under the cwd AND nested one level deeper — the walk
    // must never descend into either, at any depth.
    std::fs::create_dir_all(dir.join("node_modules")).unwrap();
    std::fs::write(dir.join("node_modules").join("dep.html"), "x").unwrap();
    std::fs::create_dir_all(dir.join("sub").join("node_modules")).unwrap();
    std::fs::write(dir.join("sub").join("node_modules").join("deep.html"), "x").unwrap();

    let mut app = app_with(vec![session_in(
        "s1",
        Agent::Codex,
        &dir.display().to_string(),
        "t",
    )]);
    app.focus_or_add_pane("s1");

    assert!(
        app.poll_html_candidates(),
        "detecting a new .html changes state"
    );
    let cands = app.html_candidates.get("s1").expect("candidates detected");
    let names: Vec<String> = cands
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"page.html".to_string()), "{names:?}");
    assert!(names.contains(&"nested.html".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n == "dep.html" || n == "deep.html"),
        "node_modules contents must be skipped at any depth: {names:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn html_seen_suppresses_candidate_until_its_mtime_advances() {
    let dir = temp_html_dir("seen");
    let file = dir.join("report.html");
    std::fs::write(&file, "<html></html>").unwrap();

    let mut app = app_with(vec![session_in(
        "s1",
        Agent::Codex,
        &dir.display().to_string(),
        "t",
    )]);
    app.focus_or_add_pane("s1");

    // First poll: the file is a fresh candidate.
    app.poll_html_candidates();
    assert!(app
        .html_candidates
        .get("s1")
        .is_some_and(|c| c.contains(&file)));

    // Seen at a FUTURE mtime → the file's real mtime is older than "seen", so it
    // stays suppressed. (Re-arm the interval gate so the next poll actually runs.)
    app.html_seen
        .entry("s1".into())
        .or_default()
        .insert(file.clone(), SystemTime::now() + Duration::from_secs(3600));
    app.html_candidates_due = None;
    app.poll_html_candidates();
    assert!(
        app.html_candidates
            .get("s1")
            .is_none_or(|c| !c.contains(&file)),
        "a file already seen must not reappear while its mtime hasn't advanced"
    );

    // Seen at a PAST mtime → the file's later mtime has advanced past it → it
    // reappears as a fresh candidate (the "edited after being dismissed" case).
    app.html_seen
        .entry("s1".into())
        .or_default()
        .insert(file.clone(), SystemTime::now() - Duration::from_secs(3600));
    app.html_candidates_due = None;
    app.poll_html_candidates();
    assert!(
        app.html_candidates
            .get("s1")
            .is_some_and(|c| c.contains(&file)),
        "a file edited after being seen (mtime advanced) reappears as a candidate"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toggle_html_preview_opens_picker_when_candidates_exist_else_blank_popup() {
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.focus = Focus::Terminal;

    // No candidates → today's fallback: the blank free-text popup.
    app.toggle_html_preview();
    assert_eq!(app.html_preview_input.as_deref(), Some(""));
    assert!(app.html_preview_picker.is_none());

    // With candidates registered, Ctrl-P opens the ranked picker instead.
    app.html_preview_input = None;
    app.html_candidates
        .insert("s1".into(), vec![PathBuf::from("/tmp/a.html")]);
    app.toggle_html_preview();
    assert_eq!(app.html_preview_picker, Some(0));
    assert!(app.html_preview_input.is_none());
}

#[test]
fn remove_pane_clears_html_candidate_state() {
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.html_candidates
        .insert("s1".into(), vec![PathBuf::from("/tmp/a.html")]);
    app.html_seen
        .entry("s1".into())
        .or_default()
        .insert(PathBuf::from("/tmp/a.html"), SystemTime::now());

    app.remove_pane("s1");
    assert!(!app.html_candidates.contains_key("s1"));
    assert!(!app.html_seen.contains_key("s1"));
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
    // The peer-transcript read now runs on a background thread (see
    // `spawn_thread_sync_for`) so it never blocks `request_resume` itself;
    // the initial PTY spawn has no inline prompt yet.
    assert!(pending.initial_input.is_none());

    // Wait for the background read to finish, then apply it like the main
    // loop's `poll_thread_sync` does every frame.
    for _ in 0..200 {
        if app.poll_thread_sync() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let queued = app
        .pending_initial_inputs
        .get("claude-1")
        .expect("thread sync queues a deferred initial input for the not-yet-spawned session");
    let input = String::from_utf8(queued.bytes.clone()).unwrap();
    assert!(input.contains("MindPlayer thread sync"));
    assert!(input.contains("codex fixed tests"));
    assert!(input.ends_with('\r'));

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn resuming_a_session_with_large_peer_transcripts_does_not_block_the_caller() {
    // Regression for the UI-freeze bug: reopening a session with several
    // handoff peers used to read + parse every peer's
    // transcript (up to `MAX_SOURCE_BYTES` each) synchronously inside
    // `request_resume`, on the main/render thread — freezing input and
    // rendering both for as long as that took. `request_resume` must now
    // return immediately regardless of peer transcript size; the read runs
    // on a background thread (see `spawn_thread_sync_for`) and is applied
    // later by `poll_thread_sync`.
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    const PEER_COUNT: usize = 6;
    // ~2MB per peer (4000 lines * ~500 bytes) — large enough that a
    // synchronous parse of all of them takes many tens of milliseconds,
    // comfortably clearing the 50ms budget asserted below on any machine.
    let mut dirs = Vec::new();
    let mut peers = Vec::new();
    for i in 0..PEER_COUNT {
        let (dir, transcript) = write_large_codex_fixture(&format!("freeze-{i}"), 4000, 500);
        let mut peer = session_in(
            &format!("codex-{i}"),
            Agent::Codex,
            "/work/project",
            &format!("(handoff)peer {i}"),
        );
        peer.file = transcript;
        dirs.push(dir);
        peers.push(peer);
    }
    let handoff_dir = dirs[0].join("handoffs");
    std::env::set_var(handoff::HANDOFF_DIR_ENV, &handoff_dir);

    let mut root = session_in("claude-root", Agent::Claude, "/work/project", "root lane");
    root.file = dirs[0].join("claude.jsonl");
    std::fs::write(
        &root.file,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}",
    )
    .unwrap();

    let mut sessions = vec![root];
    sessions.extend(peers.iter().cloned());
    let mut app = app_with(sessions);
    for peer in &peers {
        app.state.set_handoff_link(
            &peer.id,
            "claude-root",
            PathBuf::from("/tmp/handoff.md"),
            chrono::Utc::now(),
        );
    }
    app.rebuild_visible();
    app.selected = 0;

    let started = std::time::Instant::now();
    app.request_resume();
    let elapsed = started.elapsed();

    // The generous 50ms ceiling only needs to rule out "read N x ~2MB files
    // synchronously" (which the old code path did); it's not a tight
    // performance budget.
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "request_resume took {elapsed:?} — peer transcripts appear to be read synchronously again"
    );

    // The background read does eventually complete and land somewhere
    // (queued as a deferred initial input, since the session hasn't spawned
    // yet) — confirms this isn't fast merely because the sync was skipped.
    let mut applied = false;
    for _ in 0..300 {
        if app.poll_thread_sync() {
            applied = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(applied, "background thread-sync read never completed");
    assert!(
        app.pending_initial_inputs.contains_key("claude-root"),
        "completed sync should queue a deferred initial input for the not-yet-spawned session"
    );

    std::env::remove_var(handoff::HANDOFF_DIR_ENV);
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
#[ignore]
fn real_user_saav_style_data_request_resume_does_not_block() {
    // Same intent as `resuming_a_session_with_large_peer_transcripts_does_not_block_the_caller`,
    // but against this machine's actual `~/.mindplayer/state.json` handoff
    // graph and actual `~/.codex/sessions` transcripts — the exact kind of
    // data (12 real peer lanes, ~6.85MB total, confirmed via
    // `handoff::tests::real_user_data_thread_sync_completes_and_is_fast` to
    // take ~121ms to read+parse synchronously) that produced the reported
    // freeze. `#[ignore]`d for the same reason: depends on this machine's
    // home directory. Run explicitly with:
    //   cargo test -p mindplayer-tui -- --ignored real_user_saav_style_data_request_resume_does_not_block --nocapture
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _state_env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let home = std::env::var("HOME").expect("HOME must be set");
    let root_id = "019ebb9e-5083-7961-8f8d-a3bcffae5702";
    let child_ids = [
        "019ebb97-f789-7942-a058-e0e0ba9b4c2f",
        "019ebb97-f7a8-7a72-a9d0-0e640ae7745d",
        "019ebb97-f7c8-7592-8cd1-b871993c8246",
        "019ebb97-f7c9-7dd2-9358-9414c61a58c9",
        "019ebb97-f803-75a2-95b9-43ecf78e6417",
        "019ebb97-f86b-7300-bc64-9adf765a4343",
        "019ebb9e-505f-7280-a163-16d3aa758a39",
        "019ebb9e-50a5-7371-bb8e-34faae3b3c7a",
        "019ebb9e-50ad-72e1-bc34-1c56b8080b46",
        "019ebb9e-50c1-75d3-af96-ba3f89930ab1",
        "019ebb9e-50c3-74d3-9661-1d5b43250262",
        "019ebb9e-52fb-76d0-886f-8f640936b301",
    ];

    fn find_rollout(root: &std::path::Path, id: &str, depth: u32) -> Option<PathBuf> {
        if depth > 6 {
            return None;
        }
        let entries = std::fs::read_dir(root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_rollout(&path, id, depth + 1) {
                    return Some(found);
                }
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("rollout-") && name.ends_with(&format!("{id}.jsonl")) {
                    return Some(path);
                }
            }
        }
        None
    }

    let sessions_root = PathBuf::from(&home).join(".codex/sessions");
    let root_file = find_rollout(&sessions_root, root_id, 0)
        .expect("expected the real root transcript to still exist on this machine");

    let mut root = session_in(root_id, Agent::Codex, "/work/project", "real root lane");
    root.file = root_file;
    let mut sessions = vec![root];
    let mut found_children = 0;
    for id in child_ids {
        if let Some(path) = find_rollout(&sessions_root, id, 0) {
            let mut peer = session_in(id, Agent::Codex, "/work/project", "real peer lane");
            peer.file = path;
            sessions.push(peer);
            found_children += 1;
        }
    }
    assert!(
        found_children >= 10,
        "expected at least 10 of the 12 known real peer transcripts, found {found_children}"
    );

    let mut app = app_with(sessions);
    for id in child_ids {
        app.state.set_handoff_link(
            id,
            root_id,
            PathBuf::from("/tmp/handoff.md"),
            chrono::Utc::now(),
        );
    }
    app.rebuild_visible();
    app.selected = 0;
    assert_eq!(app.session_at(0).map(|s| s.id.as_str()), Some(root_id));

    let started = std::time::Instant::now();
    app.request_resume();
    let elapsed = started.elapsed();
    println!("request_resume() on real saav-style data took {elapsed:?}");

    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "request_resume took {elapsed:?} against real user data — the freeze is NOT fixed"
    );

    let mut applied = false;
    let mut wait_elapsed = std::time::Duration::ZERO;
    for _ in 0..500 {
        if app.poll_thread_sync() {
            applied = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        wait_elapsed += std::time::Duration::from_millis(10);
    }
    println!("background real-data sync resolved after {wait_elapsed:?} (roughly matches the ~121ms synchronous cost measured separately)");
    assert!(
        applied,
        "background thread-sync over real data never completed"
    );
    assert!(
        app.pending_initial_inputs.contains_key(root_id),
        "completed real-data sync should queue a deferred initial input"
    );
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

// --- usage audit instrumentation -------------------------------------------
//
// `app.audit_path` is a plain field (unlike `MINDPLAYER_STATE`, no env var or
// process-wide lock needed) — each test points it at its own temp file, so
// these never touch the real `~/.mindplayer/audit.jsonl` even without the
// `cfg!(test)` fallback in `audit_path_for_app()`.

fn audit_tmp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mp-audit-{name}-{}-{}.jsonl",
        std::process::id(),
        name.len() // cheap extra uniqueness across calls with the same name in one process
    ))
}

#[test]
fn close_selected_logs_a_session_close_event() {
    let _env = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state_tmp =
        std::env::temp_dir().join(format!("mp-audit-close-state-{}.json", std::process::id()));
    std::env::set_var("MINDPLAYER_STATE", &state_tmp);
    let audit_tmp = audit_tmp_path("close");

    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();
    app.selected = 0;
    app.close_selected();

    let events = mindplayer_core::read_events(&audit_tmp);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, mindplayer_core::AuditEvent::SessionClose);

    let _ = std::fs::remove_file(&state_tmp);
    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn confirm_catchup_logs_catchup_sent_on_a_successful_send() {
    let audit_tmp = audit_tmp_path("catchup");
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();
    app.catchup_confirm = Some("a".to_string());
    app.confirm_catchup();

    let events = mindplayer_core::read_events(&audit_tmp);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, mindplayer_core::AuditEvent::CatchupSent);

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn confirm_catchup_logs_nothing_when_the_send_fails() {
    let audit_tmp = audit_tmp_path("catchup-fail");
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();
    app.ended.insert("a".to_string()); // enqueue_or_submit_to_session bails on this
    app.catchup_confirm = Some("a".to_string());
    app.confirm_catchup();

    assert!(mindplayer_core::read_events(&audit_tmp).is_empty());
    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn begin_transition_report_requires_a_focused_pane() {
    let mut app = App::new();
    app.begin_transition_report();
    assert!(app.transition_report_input.is_none());
    assert!(app.status.contains("live pane"));
}

#[test]
fn begin_transition_report_opens_the_input_when_a_pane_is_focused() {
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    assert_eq!(app.transition_report_input, Some(String::new()));
}

#[test]
fn transition_report_input_push_and_backspace_edit_the_buffer() {
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    for c in "eks / §3".chars() {
        app.transition_report_input_push(c);
    }
    assert_eq!(app.transition_report_input.as_deref(), Some("eks / §3"));
    app.transition_report_input_backspace();
    assert_eq!(app.transition_report_input.as_deref(), Some("eks / §"));
}

#[test]
fn cancel_transition_report_clears_without_sending() {
    let audit_tmp = audit_tmp_path("transition-cancel");
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    app.transition_report_input_push('x');
    app.cancel_transition_report();

    assert!(app.transition_report_input.is_none());
    assert!(mindplayer_core::read_events(&audit_tmp).is_empty());
    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn confirm_transition_report_sends_to_the_focused_pane_and_logs_it() {
    let audit_tmp = audit_tmp_path("transition-confirm");
    let mut app = app_with(vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Claude, false),
    ]);
    app.audit_path = audit_tmp.clone();
    app.focus_or_add_pane("a");
    app.focus_or_add_pane("b");
    assert_eq!(app.focused_pane(), Some("b"));

    app.begin_transition_report();
    for c in "eks-migration / §3 / infra/eks.tf".chars() {
        app.transition_report_input_push(c);
    }
    app.confirm_transition_report_input();
    assert!(
        app.transition_report_review.is_some(),
        "enter shows a review, not an immediate send"
    );
    app.send_transition_report_review();

    assert!(app.transition_report_input.is_none());
    assert!(app.transition_report_review.is_none());
    let events = mindplayer_core::read_events(&audit_tmp);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].event,
        mindplayer_core::AuditEvent::TransitionReportSent
    );
    assert!(app.status.contains(&short("b")));

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn transition_report_uses_a_user_edited_prompt_file_when_present() {
    // The whole point of externalizing the prompt to a file: editing it on
    // disk must actually change what gets sent, with no rebuild/restart.
    let prompts_dir =
        std::env::temp_dir().join(format!("mp-prompts-edited-{}", std::process::id()));
    std::fs::create_dir_all(&prompts_dir).unwrap();
    std::fs::write(
        prompts_dir.join("transition_report.md"),
        "my custom template — {{input}}",
    )
    .unwrap();

    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.prompts_dir = prompts_dir.clone();
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    for c in "eks / §3".chars() {
        app.transition_report_input_push(c);
    }
    app.confirm_transition_report_input();
    app.send_transition_report_review();

    let sent = app
        .pending
        .as_ref()
        .expect("no live pty for 'a' — should have queued a spawn")
        .initial_input
        .as_ref()
        .expect("prompt bytes queued as initial input");
    let sent = String::from_utf8_lossy(sent);
    assert!(
        sent.contains("my custom template — eks / §3"),
        "expected the edited template with the input substituted, got: {sent:?}"
    );
    // The compiled-in default text must NOT leak through once a real file exists.
    assert!(!sent.contains("transition-<주제>.html"));

    let _ = std::fs::remove_dir_all(&prompts_dir);
}

#[test]
fn transition_report_review_starts_read_only_with_the_assembled_prompt() {
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    for c in "eks / §3".chars() {
        app.transition_report_input_push(c);
    }
    app.confirm_transition_report_input();

    let draft = app
        .transition_report_review
        .as_ref()
        .expect("review opened");
    assert!(draft.instruction.contains("eks / §3"));
    assert!(!app.transition_report_review_editing);
}

#[test]
fn editing_the_review_changes_what_gets_sent() {
    let audit_tmp = audit_tmp_path("transition-edit");
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    for c in "eks / §3".chars() {
        app.transition_report_input_push(c);
    }
    app.confirm_transition_report_input();

    app.begin_editing_transition_report_review();
    assert!(app.transition_report_review_editing);
    // Move to the very end of the (multi-line) buffer, then append a line —
    // this is the whole point of the review step: the sent text can differ
    // from what was auto-assembled.
    for _ in 0..500 {
        app.transition_report_review_move_down();
    }
    app.transition_report_review_move_end();
    app.transition_report_review_push_text("\nEXTRA HAND-EDITED LINE");
    app.send_transition_report_review();

    let sent = app
        .pending
        .as_ref()
        .expect("queued a spawn")
        .initial_input
        .as_ref()
        .expect("prompt bytes queued");
    let sent = String::from_utf8_lossy(sent);
    assert!(sent.contains("EXTRA HAND-EDITED LINE"));
    assert_eq!(mindplayer_core::read_events(&audit_tmp).len(), 1);

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn esc_cancels_the_review_without_sending() {
    let audit_tmp = audit_tmp_path("transition-review-cancel");
    let mut app = app_with(vec![session("a", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();
    app.focus_or_add_pane("a");
    app.begin_transition_report();
    app.transition_report_input_push('x');
    app.confirm_transition_report_input();
    assert!(app.transition_report_review.is_some());

    app.cancel_transition_report_review();

    assert!(app.transition_report_review.is_none());
    assert!(!app.transition_report_review_editing);
    assert!(app.pending.is_none());
    assert!(mindplayer_core::read_events(&audit_tmp).is_empty());

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn open_usage_popup_computes_stats_from_the_audit_log() {
    let audit_tmp = audit_tmp_path("open-popup");
    mindplayer_core::log_event_to(
        &audit_tmp,
        mindplayer_core::AuditEvent::SessionOpen {
            agent: "codex".to_string(),
        },
    );
    mindplayer_core::log_event_to(&audit_tmp, mindplayer_core::AuditEvent::Handoff);

    let mut app = app_with(vec![]);
    app.audit_path = audit_tmp.clone();
    assert!(!app.usage_popup);
    app.open_usage_popup();

    assert!(app.usage_popup);
    let stats = app.usage_stats.as_ref().expect("stats computed");
    assert_eq!(stats.sessions_opened_all_time.codex, 1);
    assert_eq!(stats.handoffs_all_time, 1);

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn close_usage_popup_clears_the_cached_stats() {
    let audit_tmp = audit_tmp_path("close-popup");
    let mut app = app_with(vec![]);
    app.audit_path = audit_tmp.clone();
    app.open_usage_popup();
    assert!(app.usage_popup);

    app.close_usage_popup();
    assert!(!app.usage_popup);
    assert!(app.usage_stats.is_none());

    let _ = std::fs::remove_file(&audit_tmp);
}

// --- action + status-transition instrumentation ----------------------------

#[test]
fn status_transition_logs_only_real_changes() {
    use super::status_transition;
    use SessionStatus::*;
    // First sighting is silent — SessionOpen already marks the birth.
    assert_eq!(status_transition(None, Idle), None);
    // No change tick-to-tick logs nothing.
    assert_eq!(status_transition(Some(Working), Working), None);
    // A genuine change reports (from, to) in order.
    assert_eq!(
        status_transition(Some(Idle), Working),
        Some((Idle, Working))
    );
    assert_eq!(
        status_transition(Some(Working), Ended),
        Some((Working, Ended))
    );
}

#[test]
fn multi_select_mark_then_launch_logs_the_events_in_order() {
    let audit_tmp = audit_tmp_path("multi-launch");
    let mut app = app_with(vec![
        session("s1", Agent::Codex, false),
        session("s2", Agent::Codex, false),
    ]);
    app.audit_path = audit_tmp.clone();

    app.toggle_multi_select();
    app.selected = 0;
    app.toggle_mark();
    app.selected = 1;
    app.toggle_mark();
    app.launch_marked();

    let events: Vec<_> = mindplayer_core::read_events(&audit_tmp)
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(
        events.len(),
        4,
        "expected multi-select on + two marks + launch, got {events:?}"
    );
    assert_eq!(
        events[0],
        mindplayer_core::AuditEvent::MultiSelect { on: true }
    );

    // The two marks name both sessions and count up 1 → 2, in the order they
    // were marked (never moving the cursor themselves).
    let mut marked_ids = Vec::new();
    for (i, ev) in events[1..=2].iter().enumerate() {
        match ev {
            mindplayer_core::AuditEvent::MarkToggle { id, marked, total } => {
                assert!(*marked, "mark {i} should turn the mark on");
                assert_eq!(*total, i + 1, "running marked count");
                marked_ids.push(id.clone());
            }
            other => panic!("expected a MarkToggle, got {other:?}"),
        }
    }
    marked_ids.sort();
    assert_eq!(marked_ids, vec!["s1".to_string(), "s2".to_string()]);

    // The launch carries the whole batch and its ids, so a reader sees exactly
    // which sessions opened together.
    match &events[3] {
        mindplayer_core::AuditEvent::LaunchMarked {
            ids,
            count,
            zoom_was_on,
        } => {
            assert_eq!(*count, 2);
            assert_eq!(ids.len(), 2);
            assert!(ids.contains(&"s1".to_string()) && ids.contains(&"s2".to_string()));
            assert!(!*zoom_was_on, "no zoom was set in this scenario");
        }
        other => panic!("expected LaunchMarked, got {other:?}"),
    }

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn zoom_toggle_then_multi_launch_is_reconstructable_from_the_log() {
    // The exact shape of the "only one session opened" bug (v0.15.6): zoom was
    // left on, then several sessions were launched together. The log must make
    // that correlation visible on its own — a ZoomToggle{on:true} with no
    // later off, and a LaunchMarked that records zoom_was_on:true.
    let audit_tmp = audit_tmp_path("zoom-launch");
    let mut app = app_with(vec![
        session("s1", Agent::Codex, false),
        session("s2", Agent::Codex, false),
    ]);
    app.audit_path = audit_tmp.clone();

    app.focus_or_add_pane("s1"); // a live pane so zoom is meaningful
    app.toggle_zoom();
    assert!(app.zoomed);
    app.toggle_multi_select();
    app.selected = 0;
    app.toggle_mark();
    app.selected = 1;
    app.toggle_mark();
    app.launch_marked();
    assert!(!app.zoomed, "launch must clear the leftover zoom");

    let events: Vec<_> = mindplayer_core::read_events(&audit_tmp)
        .into_iter()
        .map(|r| r.event)
        .collect();

    let zoom_on_at = events
        .iter()
        .position(|e| matches!(e, mindplayer_core::AuditEvent::ZoomToggle { on: true }));
    let launch_at = events.iter().position(|e| {
        matches!(
            e,
            mindplayer_core::AuditEvent::LaunchMarked {
                zoom_was_on: true,
                ..
            }
        )
    });
    let zoom_on_at = zoom_on_at.expect("a ZoomToggle{on:true} must be logged");
    let launch_at = launch_at.expect("a LaunchMarked{zoom_was_on:true} must be logged");
    assert!(
        zoom_on_at < launch_at,
        "zoom-on must precede the launch that happened while it was still on"
    );
    // No zoom-off between the two — the whole point is the reader can see zoom
    // was never cleared before the multi-launch.
    assert!(
        !events[zoom_on_at..launch_at]
            .iter()
            .any(|e| matches!(e, mindplayer_core::AuditEvent::ZoomToggle { on: false })),
        "no ZoomToggle{{on:false}} should sit between zoom-on and the launch"
    );

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn search_begin_confirm_records_the_resulting_terminal_focus() {
    // The shape behind the swallowed-Tab bug (v0.15.5): search active, then a
    // resume that flips focus to the terminal. Action-level logging can't see
    // the individual dropped keystrokes, but it can record this setup.
    let audit_tmp = audit_tmp_path("search-confirm");
    let mut app = app_with(vec![session("s1", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();

    app.begin_search();
    app.search_push('s');
    app.search_push('1');
    assert_eq!(app.visible, vec![0], "search still matches s1");
    app.confirm_search();
    assert_eq!(app.focus, Focus::Terminal);

    let events: Vec<_> = mindplayer_core::read_events(&audit_tmp)
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(
        events,
        vec![
            mindplayer_core::AuditEvent::SearchBegin,
            mindplayer_core::AuditEvent::SessionResume {
                id: "s1".to_string()
            },
            mindplayer_core::AuditEvent::SearchConfirm {
                focus: "terminal".to_string()
            },
        ]
    );

    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn zoom_layout_and_view_toggles_log_their_resulting_state() {
    let audit_tmp = audit_tmp_path("toggles");
    let mut app = app_with(vec![session("s1", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();

    app.focus_or_add_pane("s1");
    app.toggle_zoom(); // on
    app.toggle_zoom(); // off
    app.cycle_layout(); // Horizontal -> Vertical
    app.toggle_archived_view(); // on
    app.toggle_subagents(); // on

    let events: Vec<_> = mindplayer_core::read_events(&audit_tmp)
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(
        events,
        vec![
            mindplayer_core::AuditEvent::ZoomToggle { on: true },
            mindplayer_core::AuditEvent::ZoomToggle { on: false },
            mindplayer_core::AuditEvent::LayoutCycle {
                layout: "vertical".to_string()
            },
            mindplayer_core::AuditEvent::ViewToggle {
                view: "archived".to_string(),
                on: true
            },
            mindplayer_core::AuditEvent::ViewToggle {
                view: "subagents".to_string(),
                on: true
            },
        ]
    );

    let _ = std::fs::remove_file(&audit_tmp);
}
