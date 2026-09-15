use super::handoff_sync::handoff_label;
use super::*;
use mindplayer_core::session::TokenUsage;
use std::path::PathBuf;

pub(crate) fn session(id: &str, agent: Agent, archived: bool) -> Session {
    Session {
        id: id.into(),
        agent,
        cwd: PathBuf::new(),
        file: PathBuf::new(),
        started_at: None,
        last_active: None,
        last_prompt_at: None,
        tokens: TokenUsage::default(),
        title: id.into(),
        archived,
        is_subagent: false,
        context_pct: None,
    }
}

/// Every test App writes to a scratch path, never `~/.mindplayer/state.json`.
/// Two separate bugs had tests persisting into the developer's real state file
/// (a walker pick, then a category + collapse), so isolation lives in the shared
/// helper rather than being remembered at each call site. No env mutation: the
/// path is a plain field on `App` (see `App::state_path`).
fn scratch_state_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mp-test-{}-{tag}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("state.json")
}

thread_local! {
    /// Where the next `App` built on THIS thread should persist its sidecar.
    ///
    /// Thread-local, not an env var, because `cargo test` gives every test its
    /// own thread: this is per-test by construction. `MINDPLAYER_STATE` is
    /// process-global, so while one test had it set, every other test calling
    /// `App::new()` concurrently inherited that path — and then persisted its own
    /// state over the first test's assertion target. `STATE_ENV_LOCK` could not
    /// help, because the tests doing the inheriting never took it.
    static TEST_STATE_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Point this test's `App`s at `path` instead of a shared scratch file. Call it
/// before building the `App` when the test asserts on what got persisted.
pub(crate) fn use_state_path(path: PathBuf) {
    TEST_STATE_PATH.with(|c| *c.borrow_mut() = Some(path));
}

pub(crate) fn isolated_app() -> App {
    let mut app = App::new();
    match TEST_STATE_PATH.with(|c| c.borrow().clone()) {
        // The test named a path: load whatever is there, so it can assert that a
        // fresh `App` reads back what an earlier one persisted.
        Some(path) => {
            app.state = mindplayer_core::State::load_from(&path);
            app.walker_choice = resolve_walker(app.state.walker.as_deref());
            app.state_path = path;
        }
        // No path named: a scratch file, and a previous run's leftovers there
        // must not leak into this one.
        None => {
            app.state_path = scratch_state_path("app");
            app.state = mindplayer_core::State::default();
            app.walker_choice = crate::walker::index_of(crate::walker::DEFAULT_ID);
        }
    }
    app
}

/// An isolated `App` whose sidecar path is given outright.
///
/// Equivalent to [`use_state_path`] followed by [`isolated_app`]; use whichever
/// reads better at the call site.
pub(crate) fn isolated_app_at(state_path: PathBuf) -> App {
    let mut app = App::new();
    app.state_path = state_path;
    app.state = mindplayer_core::State::default();
    app.walker_choice = crate::walker::index_of(crate::walker::DEFAULT_ID);
    app
}

/// A sidecar path unique to one test, so no sibling can collide with it.
/// [`scratch_state_path`] is keyed on the pid alone and therefore shared.
fn test_state_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mp-test-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("state.json")
}

fn app_with(sessions: Vec<Session>) -> App {
    let mut app = isolated_app();
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
        last_prompt_at: None,
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
    let tmp = std::env::temp_dir().join(format!("mp-newstate-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    let mut app = isolated_app();
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
        last_prompt_at: None,
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
}

/// A real session written by the agent for a new session created just now.
fn late_disk_session(id: &str, agent: Agent, cwd: &str) -> Session {
    Session {
        id: id.into(),
        agent,
        cwd: PathBuf::from(cwd),
        file: PathBuf::new(),
        started_at: Some(chrono::Utc::now()),
        last_active: Some(chrono::Utc::now()),
        last_prompt_at: None,
        tokens: TokenUsage::default(),
        title: "(codex session)".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    }
}

/// Regression: closing a new session left its label queued for an hour, so the
/// name landed on whatever the agent wrote next and the closed session looked
/// like it had come back.
#[test]
fn closing_a_new_session_unqueues_its_label() {
    let tmp = test_state_path("close-new-unqueues-label");
    let mut app = isolated_app_at(tmp.clone());
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.request_new(Agent::Codex, "bar");
    assert_eq!(app.state.pending_labels.len(), 1, "label queued on create");

    app.selected = 0;
    app.close_selected();
    assert!(
        app.state.pending_labels.is_empty(),
        "closing the row must un-queue its label"
    );
    let saved = mindplayer_core::State::load_from(&tmp);
    assert!(saved.pending_labels.is_empty(), "un-queue is persisted");
    let _ = std::fs::remove_file(&tmp);
}

/// Regression: the agent writes its rollout file only after the first turn, so
/// a new session closed before that left a file behind that the next scan
/// showed as a brand-new row — the closed session, back again.
#[test]
fn a_closed_new_sessions_late_file_is_archived_not_shown() {
    let tmp = test_state_path("close-new-late-file");
    let mut app = isolated_app_at(tmp.clone());
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.request_new(Agent::Codex, "bar");
    app.selected = 0;
    app.close_selected();

    // The agent writes the file after the close; the scan picks it up.
    app.all_sessions = vec![late_disk_session("real-late", Agent::Codex, "/work")];
    app.merge_extras();
    app.rebuild_visible();

    assert!(app.state.is_archived("real-late"), "late file is archived");
    assert!(
        app.visible_sessions().all(|s| s.id != "real-late"),
        "and never shown"
    );
    assert!(app.closed_extras.is_empty(), "the placeholder is consumed");
    let _ = std::fs::remove_file(&tmp);
}

/// Past the grace window the placeholder must claim nothing: a session
/// appearing that much later is far more likely one the user started in the
/// same directory, and archiving that silently is the worse failure.
#[test]
fn a_closed_new_session_stops_claiming_files_after_the_grace_window() {
    let tmp = test_state_path("close-new-grace-expiry");
    let mut app = isolated_app_at(tmp.clone());
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.request_new(Agent::Codex, "bar");
    app.selected = 0;
    app.close_selected();

    // Backdate the placeholder past the grace window.
    for extra in &mut app.closed_extras {
        extra.started_at = Some(chrono::Utc::now() - chrono::Duration::minutes(6));
    }
    app.all_sessions = vec![late_disk_session("started-by-hand", Agent::Codex, "/work")];
    app.merge_extras();
    app.rebuild_visible();

    assert!(
        !app.state.is_archived("started-by-hand"),
        "an expired placeholder must not archive anything"
    );
    assert!(app.visible_sessions().any(|s| s.id == "started-by-hand"));
    assert!(app.closed_extras.is_empty(), "and is dropped");
    let _ = std::fs::remove_file(&tmp);
}

/// The reaper must only ever claim the file its own closed session produced —
/// a session that already existed is off-limits, exactly as for adoption.
#[test]
fn reaping_never_archives_a_pre_existing_session() {
    let tmp = test_state_path("close-new-preexisting");
    let mut app = isolated_app_at(tmp.clone());
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    // Present before the new session is created → in its baseline.
    app.all_sessions = vec![late_disk_session("older", Agent::Codex, "/work")];
    app.request_new(Agent::Codex, "bar");
    app.selected = app.row_of_session("new:codex:1").unwrap();
    app.close_selected();

    app.all_sessions = vec![late_disk_session("older", Agent::Codex, "/work")];
    app.merge_extras();
    app.rebuild_visible();

    assert!(
        !app.state.is_archived("older"),
        "pre-existing session spared"
    );
    assert!(app.visible_sessions().any(|s| s.id == "older"));
    let _ = std::fs::remove_file(&tmp);
}

/// Every command that had no audit event now writes one. The usage question
/// this answers — "which commands do I actually reach for" — is only as good as
/// its coverage, and the gaps were invisible until the log was ranked.
#[test]
fn the_newly_instrumented_commands_each_log_once() {
    let audit_tmp = audit_tmp_path("instrumented");
    let tmp = test_state_path("instrumented-state");
    let mut app = isolated_app_at(tmp.clone());
    app.audit_path = audit_tmp.clone();
    app.all_sessions = vec![session("s1", Agent::Codex, false)];
    app.rebuild_visible();
    app.selected = 0;

    app.toggle_help(); // ? open
    app.toggle_help(); // ? close
    app.move_selection(1); // down
    app.begin_category_pick(); // t on a session
    let cat = app
        .state
        .create_category("pulse", chrono::Utc::now())
        .unwrap();
    app.apply_category_for_test(&["s1".to_string()], Some(&cat));

    let kinds: Vec<String> = mindplayer_core::read_events(&audit_tmp)
        .into_iter()
        .map(|r| format!("{:?}", r.event))
        .map(|d| d.split_whitespace().next().unwrap_or("?").to_string())
        .collect();
    for want in [
        "HelpToggle",
        "ListMove",
        "CategoryPickBegin",
        "CategoryAssign",
    ] {
        assert!(
            kinds.iter().any(|k| k.starts_with(want)),
            "{want} was not logged; got {kinds:?}"
        );
    }
    assert_eq!(
        kinds.iter().filter(|k| k.starts_with("HelpToggle")).count(),
        2,
        "open and close are separate events"
    );

    let _ = std::fs::remove_file(&audit_tmp);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn pane_focus_and_close_update_active() {
    let mut app = App::new();
    app.focus_or_add_pane("a");
    app.focus_or_add_pane("b");
    assert_eq!(app.active.as_deref(), Some("b"));

    app.cycle_focus();
    assert_eq!(app.active.as_deref(), Some("a"));

    app.close_focused_pane();
    assert_eq!(app.panes, vec!["b"]);
    assert_eq!(app.active.as_deref(), Some("b"));
    app.close_focused_pane();
    assert!(app.panes.is_empty());
    assert_eq!(app.active, None);
    assert_eq!(app.focus, Focus::List);
}

/// Zoom and the archived filter both log the state they landed in, so the log
/// reads as "what the user did" rather than "a key was pressed".
#[test]
fn zoom_and_view_toggles_log_their_resulting_state() {
    let audit_tmp = audit_tmp_path("toggles");
    let mut app = app_with(vec![session("s1", Agent::Codex, false)]);
    app.audit_path = audit_tmp.clone();

    app.focus_or_add_pane("s1");
    app.toggle_zoom(); // on
    app.toggle_zoom(); // off
    app.toggle_archived_view(); // on

    let events: Vec<_> = mindplayer_core::read_events(&audit_tmp)
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(
        events,
        vec![
            mindplayer_core::AuditEvent::ZoomToggle { on: true },
            mindplayer_core::AuditEvent::ZoomToggle { on: false },
            mindplayer_core::AuditEvent::ViewToggle {
                view: "archived".to_string(),
                on: true
            },
        ]
    );
    let _ = std::fs::remove_file(&audit_tmp);
}

#[test]
fn refresh_applies_token_updates_to_existing_row() {
    let mut app = app_with(vec![session("s1", Agent::Codex, false)]);
    assert_eq!(app.session_at(0).unwrap().tokens.total, 0);

    let (tx, rx) = mpsc::channel();
    tx.send(vec![ActivityUpdate {
        id: "s1".into(),
        last_active: Some(chrono::Utc::now()),
        last_prompt_at: None,
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
    let tmp = std::env::temp_dir().join(format!("mp-newstate2-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    let mut app = isolated_app();
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
}

#[test]
fn pasting_while_initial_input_pending_is_never_held() {
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

    // A dropped screenshot path must not land in held_input: buffered, the child
    // never sees the paste and the drop is silently lost. Asserted before the
    // return value so a regression names the swallow, not the delivery.
    let delivered = app.paste_to_pty("/tmp/shot.png");
    assert!(app
        .pending_initial_inputs
        .get("a")
        .unwrap()
        .held_input
        .is_empty());
    // False only because the test harness has no live PTY to write to.
    assert!(!delivered);
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
fn thread_root_last_prompt_reflects_freshest_lane_prompt() {
    let now = chrono::Utc::now();
    // Root's own last prompt is old…
    let mut parent = session("p2", Agent::Claude, false);
    parent.last_prompt_at = Some(now - chrono::Duration::weeks(2));
    // …but a lane was prompted today.
    let mut lane = session("c2", Agent::Claude, false);
    lane.last_prompt_at = Some(now);
    let mut app = app_with(vec![parent, lane]);
    app.state
        .set_handoff_link("c2", "p2", PathBuf::from("/tmp/h2.md"), now);
    app.rebuild_visible();

    let p = app
        .all_sessions
        .iter()
        .find(|s| s.id == "p2")
        .unwrap()
        .clone();
    assert_eq!(
        app.row_last_prompt(&p, app.thread_child_count("p2")),
        Some(now),
        "root's last-prompt time reflects the lane's fresher prompt, not its own 2w-old one"
    );

    // A standalone session with no thread still uses its own value.
    let c = app
        .all_sessions
        .iter()
        .find(|s| s.id == "c2")
        .unwrap()
        .clone();
    assert_eq!(
        app.row_last_prompt(&c, app.thread_child_count("c2")),
        Some(now)
    );
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
    // No `MINDPLAYER_STATE`, no lock: the sidecar path is this test's alone, so
    // a concurrent `App::new()` in another test cannot inherit it and persist
    // over the assertion below. See `isolated_app_at`.
    let tmp = test_state_path("close-selected-archives");
    let mut app = isolated_app_at(tmp.clone());
    app.all_sessions = vec![
        session("a", Agent::Codex, false),
        session("b", Agent::Codex, false),
    ];
    app.rebuild_visible();
    app.selected = 0;
    app.close_selected();

    let saved = mindplayer_core::State::load_from(&tmp);
    assert!(saved.is_archived("a"), "archive persisted to sidecar");
    let _ = std::fs::remove_file(&tmp);
    assert!(
        app.all_sessions
            .iter()
            .find(|s| s.id == "a")
            .unwrap()
            .archived
    );
    assert!(app.visible_sessions().all(|s| s.id != "a"));
}

#[test]
fn toggle_in_progress_marks_persists_and_unmarks() {
    let tmp = std::env::temp_dir().join(format!("mp-inprog-{}.json", std::process::id()));
    use_state_path(tmp.clone());

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
    // No focused pane → nothing to act on; the popup must not open.
    let mut app = App::new();
    app.toggle_html_preview();
    assert!(app.html_preview_input.is_none());
    assert!(app.html_preview_picker.is_none());
}

#[test]
fn toggle_html_preview_opens_popup_when_no_candidates_exist() {
    let mut app = App::new();
    app.focus_or_add_pane("s1");
    app.focus = Focus::Terminal;
    app.toggle_html_preview();
    assert_eq!(
        app.html_preview_input.as_deref(),
        Some(""),
        "Ctrl-P with no detected candidates opens the path popup"
    );
    assert!(app.html_preview_error.is_none());
    assert!(app.html_preview_picker.is_none());
}

#[test]
fn confirm_html_preview_with_a_bad_path_sets_error_and_keeps_popup_open() {
    // A nonexistent path must set the inline error, leave the popup open, and
    // launch nothing — the whole point of the in-popup validation.
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
    // A bad path returns before any launch, so nothing is marked seen.
    assert!(!app.html_seen.contains_key("s1"));
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
}

fn temp_html_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mindplayer-html-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Block (bounded) until an in-flight background `.html` sweep lands and its
/// batch is consumed by `apply_html_scan`. The receiver is cleared exactly when
/// a batch has been applied, so that — not the `changed` return value — is the
/// completion signal (a sweep that finds nothing new still consumes its batch).
fn finish_html_scan(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.apply_html_scan();
        if app.html_scan_rx.is_none() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "background html scan did not complete within 5s"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Force the interval gate open, kick off a background sweep, and drive it to
/// completion — so candidate assertions are deterministic without depending on
/// the poll interval or a fixed sleep.
fn run_html_scan(app: &mut App) {
    app.html_candidates_due = None;
    assert!(app.spawn_html_scan(), "a sweep should start");
    finish_html_scan(app);
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

    // The walk runs on a background thread: kicking off the sweep must not
    // populate candidates synchronously on the spawning (input/render) thread.
    app.html_candidates_due = None;
    assert!(app.spawn_html_scan(), "a sweep starts");
    assert!(
        !app.html_candidates.contains_key("s1"),
        "the background walk must not block-populate on the spawning thread"
    );
    // Once the background walk lands, applying its finished batch surfaces the
    // detected files.
    finish_html_scan(&mut app);
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

    // First sweep: the file is a fresh candidate.
    run_html_scan(&mut app);
    assert!(app
        .html_candidates
        .get("s1")
        .is_some_and(|c| c.contains(&file)));

    // Seen at a FUTURE mtime → the file's real mtime is older than "seen", so it
    // stays suppressed.
    app.html_seen
        .entry("s1".into())
        .or_default()
        .insert(file.clone(), SystemTime::now() + Duration::from_secs(3600));
    run_html_scan(&mut app);
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
    run_html_scan(&mut app);
    assert!(
        app.html_candidates
            .get("s1")
            .is_some_and(|c| c.contains(&file)),
        "a file edited after being seen (mtime advanced) reappears as a candidate"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recency_floor_keeps_fresh_html_and_drops_stale() {
    use std::fs::OpenOptions;

    let dir = temp_html_dir("recency");
    let fresh = dir.join("fresh.html");
    let stale = dir.join("stale.html");
    std::fs::write(&fresh, "<html></html>").unwrap();
    std::fs::write(&stale, "<html></html>").unwrap();
    // Backdate the stale file well past HTML_CANDIDATE_MAX_AGE (2h): a large
    // monorepo root's dated-report dump is exactly this — old .html files that
    // must NOT compete for the picker's slots with something written just now.
    let old = SystemTime::now() - Duration::from_secs(6 * 60 * 60);
    OpenOptions::new()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();

    let mut app = app_with(vec![session_in(
        "s1",
        Agent::Codex,
        &dir.display().to_string(),
        "t",
    )]);
    app.focus_or_add_pane("s1");

    run_html_scan(&mut app);
    let cands = app
        .html_candidates
        .get("s1")
        .expect("the fresh file is a candidate");
    assert!(
        cands.contains(&fresh),
        "a file written just now must be a candidate: {cands:?}"
    );
    assert!(
        !cands.contains(&stale),
        "a file older than the recency floor must be dropped: {cands:?}"
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
    let tmp = std::env::temp_dir().join(format!("mp-merge-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    let now = chrono::Utc::now();
    let pre = Session {
        id: "pre-real".into(),
        agent: Agent::Codex,
        cwd: PathBuf::from("/work"),
        file: PathBuf::new(),
        started_at: Some(now),
        last_active: Some(now),
        last_prompt_at: None,
        tokens: TokenUsage::default(),
        title: "already running".into(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    };

    let mut app = isolated_app();
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
}

#[test]
fn close_selected_keeps_cursor_on_neighbor() {
    // Regression: after archiving a middle row the cursor must land on a
    // deliberate neighbor by id, so a repeated 'x' can't archive+kill a
    // session the user never moved onto.
    let tmp = std::env::temp_dir().join(format!("mp-neigh-{}.json", std::process::id()));
    use_state_path(tmp.clone());

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
        .position(|r| {
            r.session_index()
                .is_some_and(|i| app.all_sessions[i].id == "c")
        })
        .unwrap();
    app.close_selected();
    assert_eq!(app.selected_session().unwrap().id, "a");

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn label_edit_sets_and_persists() {
    let tmp = std::env::temp_dir().join(format!("mp-label-{}.json", std::process::id()));
    use_state_path(tmp.clone());

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
}

#[test]
fn label_edit_skips_synthetic_placeholder() {
    let tmp = std::env::temp_dir().join(format!("mp-labelsyn-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    let mut app = isolated_app();
    app.scope = Scope::WorkingDir(PathBuf::from("/work"));
    app.request_new(Agent::Codex, "");
    app.selected = 0; // the synthetic new: row
    app.begin_label_edit();
    // Synthetic placeholders use the new-session label flow, not this modal.
    assert!(app.label_target.is_none());
    assert!(app.new_label.is_none());

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn handoff_queues_target_agent_with_initial_prompt() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-handoff-label-{}.json", std::process::id()));
    use_state_path(tmp.clone());
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
    app.confirm_handoff_on(&mindplayer_core::accounts::Account::inherited(Agent::Codex));

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
}

#[test]
fn handoff_into_kiro_sends_context_as_first_input_argument() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-handoff-kiro-{}.json", std::process::id()));
    use_state_path(tmp.clone());
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

    app.confirm_handoff_on(&mindplayer_core::accounts::Account::inherited(Agent::Kiro));

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
}

#[test]
fn kiro_handoff_to_codex_creates_child_lane() {
    let _handoff_env = handoff::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("mp-kiro-handoff-{}.json", std::process::id()));
    use_state_path(tmp.clone());
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
    app.confirm_handoff_on(&mindplayer_core::accounts::Account::inherited(Agent::Codex));

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
    let state_path = dir.join("state.json");
    use_state_path(state_path.clone());
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
    let state_path = dirs[0].join("state.json");
    use_state_path(state_path.clone());
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

    app.request_resume();

    // Deliberately NO wall-clock ceiling here. There used to be a 50ms one,
    // justified as "enough to rule out reading N x ~2MB synchronously" — but
    // measured, that synchronous read+parse of this exact fixture (6 peers,
    // 14 MB total) costs 23ms unloaded and 30ms under full-core load, i.e. it
    // fits *under* the ceiling. The ceiling could not tell the two code paths
    // apart; all it detected was scheduler noise, and it failed ~25% of loaded
    // parallel runs.
    //
    // The assertion below is the real regression test and needs no timing: the
    // result can only arrive through `poll_thread_sync`'s channel, which exists
    // only because the read runs on a worker. The old synchronous path never
    // sent anything through it, so `applied` would stay false.

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
fn thread_sync_needed_stays_quiet_across_a_mindplayer_restart() {
    // Regression: `thread_sync_at` alone is in-memory only, so it's wiped every
    // time MindPlayer restarts — a fresh `App` after quitting and reopening
    // looked identical to "never synced before" and fired the sync again. This
    // is the exact bug the user reported: leaving MindPlayer and coming back
    // into an in-progress handoff session re-triggers the handoff content.
    // A restart is simulated here by a brand-new `App` (empty `thread_sync_at`,
    // like right after startup) whose loaded `state.thread_synced` already
    // carries the id from before the restart.
    let mut app = App::new();
    app.state.thread_synced.insert("handoff-target".to_string());
    let mut peer = session("main-session", Agent::Claude, false);
    peer.last_active = Some(chrono::Utc::now());
    let peers = vec![peer];

    assert!(
        !app.thread_sync_needed("handoff-target", &peers),
        "a session already marked synced in persisted state must not re-fire \
         just because this process's in-memory thread_sync_at is fresh"
    );
}

/// Exercises the exact resolution `App::new_in` uses, without touching
/// `MINDPLAYER_STATE` — an earlier version of this test set that env var and
/// went flaky under load, reading the developer's real state file instead of
/// its temp one (see `resolve_walker`'s note).
#[test]
fn walker_defaults_to_the_rubber_duck_when_nothing_is_stored() {
    assert_eq!(
        crate::walker::get(resolve_walker(None)).id,
        "duck",
        "a fresh install must start on the documented default"
    );
    assert_eq!(
        crate::walker::get(resolve_walker(Some("octopus"))).id,
        "octopus",
        "a stored id must be honored"
    );
}

#[test]
fn walker_picker_only_commits_on_confirm_and_persists_the_choice() {
    let dir = std::env::temp_dir().join(format!("mp-walker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    let _ = std::fs::remove_file(&path);
    use_state_path(path.clone());

    let mut app = isolated_app();
    // `confirm_walker_pick` calls `save_state()`, which writes to this field.
    // Asserted rather than assumed: this test wrote to the developer's real
    // `~/.mindplayer/state.json` once, back when the path came from a
    // process-global env var another test could clear mid-run.
    assert_eq!(
        app.state_path, path,
        "refusing to run a state-saving test that is not isolated"
    );
    // Pinned rather than read from whatever `App::new` resolved, so the
    // expectations below don't depend on ambient state at all.
    app.walker_choice = 0;
    let start = 0;

    // Cancelling leaves the pick untouched and writes nothing.
    app.open_walker_picker();
    app.move_walker_pick(1);
    app.cancel_walker_picker();
    assert_eq!(app.walker_choice, start, "cancel must not change the pick");
    assert!(app.walker_picker.is_none());

    // Confirming commits and persists.
    app.open_walker_picker();
    app.move_walker_pick(1);
    let wanted = app.walker_picker.unwrap();
    assert_eq!(wanted, 1, "one step down from the first entry");
    app.confirm_walker_pick();
    assert_eq!(app.walker_choice, wanted);
    assert!(app.walker_picker.is_none(), "picker closes on confirm");
    let expected_id = crate::walker::get(wanted).id.to_string();
    assert_eq!(app.state.walker.as_deref(), Some(expected_id.as_str()));

    // A fresh App reads it back from disk.
    let reopened = isolated_app();
    assert_eq!(reopened.walker().id, expected_id);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn walker_pick_wraps_at_both_ends() {
    let mut app = App::new();
    app.walker_choice = 0;
    app.open_walker_picker();
    app.move_walker_pick(-1);
    assert_eq!(
        app.walker_picker,
        Some(crate::walker::ALL.len() - 1),
        "up from the first entry wraps to the last"
    );
    app.move_walker_pick(1);
    assert_eq!(app.walker_picker, Some(0), "and back down again");
}

/// Reads a real state file through `load_from` (an explicit path, so no env
/// mutation) to prove a character id removed in a later release still starts up
/// on the default rather than panicking or rendering nothing.
#[test]
fn a_corrupt_or_removed_character_id_falls_back_instead_of_breaking_startup() {
    let dir = std::env::temp_dir().join(format!("mp-walker-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    std::fs::write(&path, r#"{"version":1,"walker":"a-character-we-deleted"}"#).unwrap();

    let state = mindplayer_core::State::load_from(&path);
    assert_eq!(
        state.walker.as_deref(),
        Some("a-character-we-deleted"),
        "the unknown id should round-trip into State untouched"
    );
    assert_eq!(
        crate::walker::get(resolve_walker(state.walker.as_deref())).id,
        crate::walker::DEFAULT_ID,
        "and only be replaced at resolution time"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn status_rank_orders_by_urgency() {
    // A just-finished session (Ended) is the single highest priority —
    // nothing left for the agent to do, everything left is on the user —
    // which outranks even a live approval prompt. Historical Inactive
    // sessions rank last.
    use SessionStatus::*;
    assert!(status_rank(Ended) < status_rank(Blocked));
    assert!(status_rank(Blocked) < status_rank(Working));
    assert!(status_rank(Working) < status_rank(Idle));
    assert!(status_rank(Idle) < status_rank(Inactive));
}

#[test]
fn done_sessions_bubble_above_other_agent_types_in_the_recent_list() {
    // A finished (Ended) session must sort above other agent types' groups
    // in the recent list, not just within its own agent's section — status
    // urgency is the group sort's secondary key, ahead of agent clustering.
    let mut done = session("done-claude", Agent::Claude, false);
    done.last_active = Some(chrono::Utc::now());
    let mut codex_untouched = session("plain-codex", Agent::Codex, false);
    codex_untouched.last_active = Some(chrono::Utc::now());

    let mut app = app_with(vec![done, codex_untouched]);
    app.ended.insert("done-claude".to_string());
    app.rebuild_visible();

    let ids: Vec<&str> = app
        .visible
        .iter()
        .filter_map(|r| r.session_index())
        .map(|i| app.all_sessions[i].id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["done-claude", "plain-codex"],
        "codex's agent_rank would otherwise win the tie and sort first, even \
         though the claude session already finished"
    );
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
    let tmp = std::env::temp_dir().join(format!("mp-dirstate-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    // A real directory that exists on every machine.
    let target = std::env::temp_dir();
    let mut app = isolated_app();
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
    let tmp = std::env::temp_dir().join(format!("mp-dirstate2-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    let mut app = isolated_app();
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
    let tmp = std::env::temp_dir().join(format!("mp-dirstate3-{}.json", std::process::id()));
    use_state_path(tmp.clone());

    let mut app = isolated_app();
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
    let state_tmp =
        std::env::temp_dir().join(format!("mp-audit-close-state-{}.json", std::process::id()));
    use_state_path(state_tmp.clone());
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
    assert_eq!(
        app.visible,
        vec![Row::Session(0)],
        "search still matches s1"
    );
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

// --- pane bands -------------------------------------------------------------

/// Panes launched in an interleaved order end up grouped, because the grid draws
/// one band per category and `focused` indexes `panes` — so Tab must walk the
/// grid the way it looks.
#[test]
fn opening_panes_groups_them_by_category() {
    let mut app = app_with(vec![
        session("m1", Agent::Claude, false),
        session("s1", Agent::Codex, false),
        session("m2", Agent::Codex, false),
        session("loose", Agent::Claude, false),
        session("s2", Agent::Claude, false),
    ]);
    categorize(&mut app, "mindplayer", &["m1", "m2"]);
    categorize(&mut app, "soda-nest", &["s1", "s2"]);

    for id in ["m1", "s1", "m2", "loose", "s2"] {
        app.focus_or_add_pane(id);
    }

    assert_eq!(
        app.panes,
        vec!["m1", "m2", "s1", "s2", "loose"],
        "same-category panes adjacent, uncategorized trailing"
    );
}

/// A category's run of panes sits where its first pane landed, so the grid does
/// not reshuffle as later panes arrive.
#[test]
fn category_order_follows_the_first_pane_opened_into_each() {
    let mut app = app_with(vec![
        session("s1", Agent::Codex, false),
        session("m1", Agent::Claude, false),
        session("m2", Agent::Codex, false),
    ]);
    categorize(&mut app, "mindplayer", &["m1", "m2"]);
    categorize(&mut app, "soda-nest", &["s1"]);

    app.focus_or_add_pane("s1");
    app.focus_or_add_pane("m1");
    app.focus_or_add_pane("m2");

    assert_eq!(
        app.panes,
        vec!["s1", "m1", "m2"],
        "soda-nest opened first, so its panes stay first"
    );
}

/// Regrouping reorders `panes`, so the focus index has to follow the session it
/// was on — otherwise opening a pane silently moves focus to a different one.
#[test]
fn regrouping_keeps_focus_on_the_same_session() {
    let mut app = app_with(vec![
        session("m1", Agent::Claude, false),
        session("loose", Agent::Codex, false),
        session("m2", Agent::Codex, false),
    ]);
    categorize(&mut app, "mindplayer", &["m1", "m2"]);

    app.focus_or_add_pane("m1");
    app.focus_or_add_pane("loose");
    assert_eq!(app.focused_pane(), Some("loose"));

    // Opening m2 pushes `loose` to the trailing band; focus must stay on m2,
    // the pane just opened, and `loose` must not be silently focused instead.
    app.focus_or_add_pane("m2");
    assert_eq!(app.panes, vec!["m1", "m2", "loose"]);
    assert_eq!(app.focused_pane(), Some("m2"));
    assert_eq!(app.active.as_deref(), Some("m2"));
}

/// A handoff child inherits its parent's category through the thread root, so it
/// sits beside the parent in the grid rather than trailing with the
/// uncategorized ones.
#[test]
fn a_handoff_child_sits_beside_its_parent() {
    let mut app = app_with(vec![
        session("parent", Agent::Claude, false),
        session("child", Agent::Codex, false),
        session("loose", Agent::Claude, false),
    ]);
    categorize(&mut app, "mindplayer", &["parent"]);
    app.state.set_handoff_link(
        "child",
        "parent",
        PathBuf::from("/tmp/handoff.md"),
        chrono::Utc::now(),
    );
    assert_eq!(
        app.category_of_session("child"),
        app.category_of_session("parent"),
        "the child resolves to the parent's category"
    );

    app.focus_or_add_pane("loose");
    app.focus_or_add_pane("parent");
    app.focus_or_add_pane("child");

    assert_eq!(
        app.panes,
        vec!["parent", "child", "loose"],
        "the child sits with its parent, not at the end"
    );
}

// --- categorize -------------------------------------------------------------

/// Assign helper: create-or-reuse a category and put `ids` in it.
fn categorize(app: &mut App, name: &str, ids: &[&str]) -> String {
    let cat = app
        .state
        .create_category(name, chrono::Utc::now())
        .expect("non-blank name");
    for id in ids {
        assert!(app.state.assign_category(id, &cat), "assign {id}");
    }
    app.rebuild_visible();
    cat
}

#[test]
fn a_category_groups_its_sessions_under_one_header() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let mut loose = session("loose", Agent::Codex, false);
    loose.last_active = Some(now);
    let mut app = app_with(vec![a, b, loose]);
    categorize(&mut app, "mindplayer", &["a", "b"]);

    let ids: Vec<String> = app
        .visible
        .iter()
        .map(|r| match r {
            Row::Header(Some(id)) => format!("H:{}", app.category_label(id)),
            Row::Header(None) => "H:uncategorized".to_string(),
            Row::Session(i) => app.all_sessions[*i].id.clone(),
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            "H:mindplayer".to_string(),
            "a".to_string(),
            "b".to_string(),
            "H:uncategorized".to_string(),
            "loose".to_string()
        ],
        "the topic gets a header, the leftovers get their own"
    );
}

#[test]
fn no_headers_at_all_until_a_category_exists() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let app = app_with(vec![a]);
    assert!(
        app.visible.iter().all(|r| !r.is_header()),
        "a fresh install must not show an 'uncategorized' header over everything"
    );
}

/// The bug that made the first cut of this wrong: bucketing the uncategorized
/// pile atomically meant one recent session dragged all of them above the
/// divider, and the `older` split disappeared.
#[test]
fn uncategorized_sessions_keep_their_own_recent_older_split() {
    let now = chrono::Utc::now();
    let mut fresh = session("fresh", Agent::Codex, false);
    fresh.last_active = Some(now);
    let mut stale = session("stale", Agent::Codex, false);
    stale.last_active = Some(now - chrono::Duration::days(9));
    let mut topic = session("topic", Agent::Claude, false);
    topic.last_active = Some(now);
    let mut app = app_with(vec![fresh, stale, topic]);
    categorize(&mut app, "pulse", &["topic"]);

    // recent band: pulse header + topic + uncategorized header + fresh = 4
    assert_eq!(app.recent_count, 4, "stale must stay below the divider");
    let older: Vec<String> = app.visible[app.recent_count..]
        .iter()
        .filter_map(|r| r.session_index())
        .map(|i| app.all_sessions[i].id.clone())
        .collect();
    assert_eq!(older, vec!["stale".to_string()]);
}

#[test]
fn a_recent_lane_pulls_its_whole_category_above_the_divider() {
    let now = chrono::Utc::now();
    let mut fresh = session("fresh", Agent::Codex, false);
    fresh.last_active = Some(now);
    let mut ancient = session("ancient", Agent::Codex, false);
    ancient.last_active = Some(now - chrono::Duration::days(20));
    let mut app = app_with(vec![fresh, ancient]);
    categorize(&mut app, "one-topic", &["fresh", "ancient"]);

    // Category is atomic, matching how handoff threads already behave.
    assert_eq!(app.recent_count, app.visible.len());
    assert!(
        app.visible.iter().filter(|r| r.is_header()).count() == 1,
        "a topic must not appear in both bands"
    );
}

#[test]
fn collapsing_hides_the_sessions_but_keeps_the_header_selectable() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Codex, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    let cat = categorize(&mut app, "topic", &["a", "b"]);

    app.selected = 0;
    assert_eq!(app.selected_category(), Some(cat.as_str()));
    assert!(
        app.collapse_category_at_cursor(),
        "collapse from the header"
    );
    assert!(app.state.is_collapsed(&cat));
    assert_eq!(app.visible.len(), 1, "only the header is left");
    assert!(app.visible[0].is_header());
    // And it can be reopened, which needs the header to still be there.
    app.selected = 0;
    assert!(app.expand_selected_category());
    assert_eq!(app.visible.len(), 3);
}

#[test]
fn left_arrow_from_a_session_folds_its_category_and_steps_out_to_the_header() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    let cat = categorize(&mut app, "topic", &["a"]);

    // Cursor on the session, not the header.
    app.selected = app.row_of_session("a").expect("session row");
    assert!(app.selected_category().is_none());
    assert!(app.collapse_category_at_cursor());
    assert!(app.state.is_collapsed(&cat));
    assert_eq!(
        app.selected, 0,
        "cursor must land on the header it just folded, not dangle"
    );
    assert_eq!(app.selected_category(), Some(cat.as_str()));
}

#[test]
fn uncategorized_rows_cannot_be_folded() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut loose = session("loose", Agent::Codex, false);
    loose.last_active = Some(now);
    let mut app = app_with(vec![a, loose]);
    categorize(&mut app, "topic", &["a"]);

    let row = app
        .visible
        .iter()
        .position(|r| matches!(r, Row::Header(None)))
        .expect("uncategorized header");
    app.selected = row;
    assert!(app.selected_category().is_none(), "not a foldable category");
    assert!(!app.collapse_category_at_cursor());
    assert!(!app.toggle_selected_category());
}

#[test]
fn a_header_row_has_no_session_so_single_row_actions_are_inert() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    categorize(&mut app, "topic", &["a"]);

    app.selected = 0;
    assert!(app.selected_session().is_none());
    // `x` on a header must not archive whatever happens to be nearby.
    let archived_before = app.state.archived.len();
    app.close_selected();
    assert_eq!(app.state.archived.len(), archived_before);
}

#[test]
fn the_picker_assigns_every_marked_session_at_once() {
    let dir = std::env::temp_dir().join(format!("mp-cat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    let _ = std::fs::remove_file(&path);
    use_state_path(path.clone());

    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Codex, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    // This test persists categories, so prove it is isolated before it writes.
    assert_eq!(
        app.state_path, path,
        "refusing to run a state-saving test that is not isolated"
    );
    app.toggle_multi_select();
    app.selected = 0;
    app.toggle_mark();
    app.selected = 1;
    app.toggle_mark();

    app.begin_category_pick();
    let picker = app.category_picker.clone().expect("picker open");
    assert_eq!(picker.targets.len(), 2, "both marked rows are targets");
    // "+ new category" is second from the end.
    let rows = app.category_picker_rows();
    app.category_picker.as_mut().unwrap().selected = rows.len() - 2;
    app.confirm_category_pick();
    // Now typing the name, then enter creates and assigns.
    for c in "shared".chars() {
        app.category_name_push(c);
    }
    app.confirm_category_pick();

    assert!(app.category_picker.is_none(), "closes after assigning");
    let ca = app.state.category_of("a").map(str::to_string);
    let cb = app.state.category_of("b").map(str::to_string);
    assert!(ca.is_some() && ca == cb, "both land in the same category");
    assert_eq!(app.state.category_name(&ca.unwrap()), Some("shared"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn creating_the_same_topic_name_twice_reuses_it_instead_of_forking() {
    let mut state = mindplayer_core::State::default();
    let now = chrono::Utc::now();
    let a = state.create_category("pulse", now).unwrap();
    let b = state.create_category("  PULSE ", now).unwrap();
    assert_eq!(a, b, "same topic, case/space insensitive");
    assert_eq!(state.categories.len(), 1);
    assert!(
        state.create_category("   ", now).is_none(),
        "blank rejected"
    );
}

#[test]
fn renaming_a_category_keeps_its_membership() {
    let mut state = mindplayer_core::State::default();
    let now = chrono::Utc::now();
    let cat = state.create_category("mindplayer", now).unwrap();
    state.assign_category("s1", &cat);
    assert!(state.rename_category(&cat, "mindplayer-tui"));
    assert_eq!(state.category_of("s1"), Some(cat.as_str()));
    assert_eq!(state.category_name(&cat), Some("mindplayer-tui"));
    assert!(!state.rename_category(&cat, "  "), "blank rename refused");
}

#[test]
fn pruning_drops_empty_categories_and_stale_membership() {
    let mut state = mindplayer_core::State::default();
    let now = chrono::Utc::now();
    let keep = state.create_category("keep", now).unwrap();
    let gone = state.create_category("gone", now).unwrap();
    state.assign_category("alive", &keep);
    state.assign_category("deleted", &gone);
    state.set_collapsed(&gone, true);

    let known: std::collections::BTreeSet<String> = ["alive".to_string()].into_iter().collect();
    assert!(state.prune_categories(&known));
    assert_eq!(state.categories.len(), 1, "empty category removed");
    assert!(state.categories.contains_key(&keep));
    assert!(!state.session_category.contains_key("deleted"));
    assert!(
        !state.is_collapsed(&gone),
        "collapse entry for a removed category must go too"
    );
    // Idempotent.
    assert!(!state.prune_categories(&known));
}

#[test]
fn an_unknown_category_id_is_refused_rather_than_dangling() {
    let mut state = mindplayer_core::State::default();
    assert!(!state.assign_category("s1", "cat_does_not_exist"));
    assert!(state.category_of("s1").is_none());
}

#[test]
fn a_handoff_thread_stays_in_one_category_via_its_root() {
    let now = chrono::Utc::now();
    let mut parent = session("p", Agent::Codex, false);
    parent.last_active = Some(now);
    let mut child = session("c", Agent::Claude, false);
    child.last_active = Some(now);
    let mut app = app_with(vec![parent, child]);
    app.state
        .set_handoff_link("c", "p", std::path::PathBuf::from("artifact.md"), now);
    // Only the root is categorized; the lane must follow it.
    categorize(&mut app, "topic", &["p"]);

    let ids: Vec<String> = app
        .visible
        .iter()
        .filter_map(|r| r.session_index())
        .map(|i| app.all_sessions[i].id.clone())
        .collect();
    assert_eq!(ids, vec!["p".to_string(), "c".to_string()]);
    assert!(
        app.visible.iter().filter(|r| r.is_header()).count() == 1,
        "the lane must not spawn a second, uncategorized group"
    );
}

#[test]
fn folding_drops_marks_for_rows_that_went_away() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    let cat = categorize(&mut app, "topic", &["a"]);
    app.toggle_multi_select();
    app.selected = app.row_of_session("a").unwrap();
    app.toggle_mark();
    assert!(app.marked.contains("a"));

    app.state.set_collapsed(&cat, true);
    app.rebuild_visible();
    assert!(
        !app.marked.contains("a"),
        "a folded-away session must not stay marked for a bulk launch"
    );
}

/// A folded topic must still say how many sessions it holds — counting visible
/// rows reported zero, which read as "empty" rather than "closed".
#[test]
fn a_folded_category_still_reports_its_session_count() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Codex, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    let cat = categorize(&mut app, "topic", &["a", "b"]);
    assert_eq!(app.category_session_count(Some(&cat)), 2);

    app.state.set_collapsed(&cat, true);
    app.rebuild_visible();
    assert_eq!(app.visible.len(), 1, "sessions are hidden");
    assert_eq!(
        app.category_session_count(Some(&cat)),
        2,
        "but the tally still describes what is inside"
    );
}

// --- category context sync --------------------------------------------------

/// Write a claude-shaped JSONL transcript and return its path.
fn write_claude_transcript(dir: &std::path::Path, id: &str, turns: &[(&str, &str)]) -> PathBuf {
    let path = dir.join(format!("{id}.jsonl"));
    let mut out = String::new();
    for (role, text) in turns {
        out.push_str(&format!(
            r#"{{"type":"{role}","message":{{"role":"{role}","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        ));
        out.push('\n');
    }
    std::fs::write(&path, out).unwrap();
    path
}

fn transcript_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mp-sync-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn category_peers_need_no_handoff_link_between_them() {
    // The reported flow: an existing session, then `n` a new one into the same
    // category. There is no lineage, which is why thread-sync never fired.
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    categorize(&mut app, "topic", &["a", "b"]);

    assert!(
        app.thread_peer_sessions("b").is_empty(),
        "no handoff link, so lineage peering finds nothing — the original gap"
    );
    let peers = app.category_peer_sessions("b");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].id, "a");
}

#[test]
fn a_handoff_lane_inherits_its_roots_category_for_peering() {
    let now = chrono::Utc::now();
    let mut root = session("p", Agent::Codex, false);
    root.last_active = Some(now);
    let mut lane = session("c", Agent::Claude, false);
    lane.last_active = Some(now);
    let mut other = session("solo", Agent::Kiro, false);
    other.last_active = Some(now);
    let mut app = app_with(vec![root, lane, other]);
    app.state
        .set_handoff_link("c", "p", PathBuf::from("artifact.md"), now);
    // Only the root is categorized.
    categorize(&mut app, "topic", &["p", "solo"]);

    assert_eq!(
        app.category_of_session("c").as_deref(),
        app.category_of_session("p").as_deref(),
        "a lane follows its root into the topic"
    );
    let ids: Vec<String> = app
        .category_peer_sessions("c")
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert!(ids.contains(&"p".to_string()) && ids.contains(&"solo".to_string()));
}

#[test]
fn an_uncategorized_session_has_no_category_peers() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let app = app_with(vec![a, b]);
    assert!(app.category_peer_sessions("a").is_empty());
    assert!(app.category_of_session("a").is_none());
}

#[test]
fn a_second_sync_with_no_peer_activity_produces_nothing() {
    // This is what replaces the old sync-once-ever rule: re-entering is silent
    // because the watermark has already covered everything.
    let dir = transcript_dir("quiet");
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Claude, false);
    a.file = write_claude_transcript(&dir, "a", &[("user", "first task"), ("assistant", "done")]);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    categorize(&mut app, "topic", &["a", "b"]);

    let first = app.category_deltas("b");
    assert_eq!(first.len(), 1, "peer a has unseen content");
    assert!(first[0].added_bytes > 0);

    // Record the watermark the way poll_category_sync would.
    for d in &first {
        app.state.set_sync_mark("b", &d.session.id, d.new_len);
    }
    assert!(
        app.category_deltas("b").is_empty(),
        "nothing new since the mark, so a re-entry says nothing"
    );
}

#[test]
fn only_new_bytes_are_included_after_the_peer_advances() {
    let dir = transcript_dir("delta");
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Claude, false);
    a.file = write_claude_transcript(&dir, "a", &[("user", "ALPHA")]);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    categorize(&mut app, "topic", &["a", "b"]);

    let first = app.category_deltas("b");
    let mark = first[0].new_len;
    app.state.set_sync_mark("b", "a", mark);

    // Peer appends a new turn.
    let path = app
        .all_sessions
        .iter()
        .find(|s| s.id == "a")
        .unwrap()
        .file
        .clone();
    let mut extra = std::fs::read_to_string(&path).unwrap();
    extra.push_str(
        &format!("{}\n", r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"BRAVO"}]}}"#),
    );
    std::fs::write(&path, extra).unwrap();

    let second = app.category_deltas("b");
    assert_eq!(second.len(), 1);
    assert!(
        second[0].text.contains("BRAVO"),
        "the new turn is present: {}",
        second[0].text
    );
    assert!(
        !second[0].text.contains("ALPHA"),
        "already-seen content must not be resent — that repeat was the actual bug: {}",
        second[0].text
    );
    assert!(!second[0].reset);
}

#[test]
fn a_shrunk_peer_transcript_falls_back_to_a_full_read() {
    let dir = transcript_dir("shrunk");
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Claude, false);
    a.file = write_claude_transcript(&dir, "a", &[("user", "REWRITTEN")]);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    categorize(&mut app, "topic", &["a", "b"]);

    // Pretend we had synced a much larger file that has since been rewritten.
    app.state.set_sync_mark("b", "a", 10_000_000);
    let deltas = app.category_deltas("b");
    assert_eq!(deltas.len(), 1, "must not silently skip a rewritten peer");
    assert!(deltas[0].reset, "flagged as a full re-read");
    assert!(deltas[0].text.contains("REWRITTEN"));
}

#[test]
fn auto_sync_defaults_on_and_survives_a_state_round_trip() {
    let mut state = mindplayer_core::State::default();
    let cat = state.create_category("topic", chrono::Utc::now()).unwrap();
    assert!(
        state.category_auto_sync(&cat),
        "new categories sync by default"
    );

    // A category written before this field existed must load as ON, not OFF —
    // a bare #[serde(default)] on a bool would give false here.
    let legacy = r#"{"version":1,"categories":{"cat_9":{"name":"legacy","created_at":"2026-07-01T00:00:00Z"}}}"#;
    let loaded: mindplayer_core::State = serde_json::from_str(legacy).unwrap();
    assert!(
        loaded.category_auto_sync("cat_9"),
        "existing categories must not silently load with sync disabled"
    );

    state.set_category_auto_sync(&cat, false);
    let json = serde_json::to_string(&state).unwrap();
    let back: mindplayer_core::State = serde_json::from_str(&json).unwrap();
    assert!(
        !back.category_auto_sync(&cat),
        "an explicit off round-trips"
    );
}

#[test]
fn the_category_menu_opens_only_on_a_real_category_header() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut loose = session("loose", Agent::Codex, false);
    loose.last_active = Some(now);
    let mut app = app_with(vec![a, loose]);
    let cat = categorize(&mut app, "topic", &["a"]);

    // On the category header: opens.
    app.selected = app
        .visible
        .iter()
        .position(|r| matches!(r, Row::Header(Some(_))))
        .unwrap();
    assert!(app.open_category_menu());
    assert_eq!(app.category_menu.as_ref().unwrap().cat_id, cat);
    app.cancel_category_menu();

    // On the uncategorized header: refused, there is nothing to configure.
    app.selected = app
        .visible
        .iter()
        .position(|r| matches!(r, Row::Header(None)))
        .unwrap();
    assert!(!app.open_category_menu());

    // On a session row: refused (that keypress assigns a category instead).
    app.selected = app.row_of_session("a").unwrap();
    assert!(!app.open_category_menu());
}

#[test]
fn the_menu_toggles_auto_sync_and_renames() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    let cat = categorize(&mut app, "topic", &["a"]);
    app.selected = 0;
    assert!(app.open_category_menu());

    // auto-sync row toggles.
    assert!(app.state.category_auto_sync(&cat));
    app.confirm_category_menu();
    assert!(!app.state.category_auto_sync(&cat));
    app.confirm_category_menu();
    assert!(app.state.category_auto_sync(&cat));

    // rename: first enter opens the field pre-filled, second commits.
    app.move_category_menu(CategoryMenu::RENAME as isize);
    app.confirm_category_menu();
    assert_eq!(
        app.category_menu.as_ref().unwrap().rename.as_deref(),
        Some("topic"),
        "pre-filled so it can be edited rather than retyped"
    );
    app.category_rename_backspace();
    app.category_rename_push('!');
    app.confirm_category_menu();
    assert_eq!(app.state.category_name(&cat), Some("topi!"));
    assert!(app.category_menu.is_none());
}

#[test]
fn removing_a_category_keeps_the_sessions_and_clears_watermarks() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Claude, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    let cat = categorize(&mut app, "topic", &["a", "b"]);
    app.state.set_sync_mark("a", "b", 500);

    app.selected = 0;
    assert!(app.open_category_menu());
    app.move_category_menu(CategoryMenu::REMOVE as isize);
    app.confirm_category_menu(); // asks first
    assert!(
        app.category_menu.as_ref().unwrap().confirm_remove,
        "destructive action confirms rather than firing on one keypress"
    );
    app.confirm_category_menu(); // confirmed

    assert!(app.state.categories.is_empty());
    assert!(app.state.category_of("a").is_none());
    assert_eq!(app.all_sessions.len(), 2, "sessions themselves are kept");
    assert_eq!(
        app.state.sync_mark("a", "b"),
        0,
        "stale watermarks would hide peer content if these were regrouped later"
    );
    assert!(!app.state.categories.contains_key(&cat));
}

#[test]
fn moving_the_menu_cursor_cancels_a_pending_remove() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    categorize(&mut app, "topic", &["a"]);
    app.selected = 0;
    app.open_category_menu();
    app.move_category_menu(CategoryMenu::REMOVE as isize);
    app.confirm_category_menu();
    assert!(app.category_menu.as_ref().unwrap().confirm_remove);
    app.move_category_menu(-1);
    assert!(
        !app.category_menu.as_ref().unwrap().confirm_remove,
        "walking away must not leave a live confirmation armed"
    );
}

#[test]
fn sync_marks_are_pruned_for_sessions_that_no_longer_exist() {
    let mut state = mindplayer_core::State::default();
    state.set_sync_mark("alive", "gone", 10);
    state.set_sync_mark("gone", "alive", 20);
    let known: std::collections::BTreeSet<String> = ["alive".to_string()].into_iter().collect();
    assert!(state.prune_sync_marks(&known));
    assert_eq!(state.sync_mark("alive", "gone"), 0);
    assert_eq!(state.sync_mark("gone", "alive"), 0);
    assert!(!state.prune_sync_marks(&known), "idempotent");
}

/// Reported bug: `→` was wired to a toggle, so pressing it on an already-open
/// category *closed* it. The key then never went deeper, and continuing to press
/// it eventually landed on a session row and opened the session.
#[test]
fn right_arrow_on_a_category_never_folds_it() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut b = session("b", Agent::Codex, false);
    b.last_active = Some(now);
    let mut app = app_with(vec![a, b]);
    let cat = categorize(&mut app, "topic", &["a", "b"]);

    // Folded: → opens it and the cursor stays on the header.
    app.state.set_collapsed(&cat, true);
    app.rebuild_visible();
    app.selected = 0;
    assert!(app.enter_selected_category());
    assert!(!app.state.is_collapsed(&cat), "→ unfolds");
    assert_eq!(
        app.selected, 0,
        "cursor stays on the header after unfolding"
    );

    // Open: → steps INSIDE instead of folding it back up.
    assert!(app.enter_selected_category());
    assert!(
        !app.state.is_collapsed(&cat),
        "→ must never fold — that is ←'s job"
    );
    assert_eq!(app.selected, 1, "cursor descends onto the first session");
    assert!(app.selected_session().is_some());
}

#[test]
fn enter_on_a_category_header_still_toggles_it() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    let cat = categorize(&mut app, "topic", &["a"]);
    app.selected = 0;

    // Enter is the explicit open/shut gesture, unlike →.
    assert!(app.toggle_selected_category());
    assert!(app.state.is_collapsed(&cat));
    app.selected = 0;
    assert!(app.toggle_selected_category());
    assert!(!app.state.is_collapsed(&cat));
}

#[test]
fn right_arrow_on_an_empty_or_folded_last_category_does_not_move_off_the_list() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    categorize(&mut app, "topic", &["a"]);
    // Cursor on the last header with the session below it; stepping in is fine,
    // but stepping in again from the session row must not run past the end.
    app.selected = app.visible.len() - 1;
    let before = app.selected;
    // On a session row this is not a category action at all.
    assert!(!app.enter_selected_category());
    assert_eq!(app.selected, before, "no movement from a session row");
}

/// `→` walks the tree; it must not launch anything. Opening is `enter`'s job.
#[test]
fn right_arrow_on_a_session_row_does_not_open_it() {
    let now = chrono::Utc::now();
    let mut a = session("a", Agent::Codex, false);
    a.last_active = Some(now);
    let mut app = app_with(vec![a]);
    categorize(&mut app, "topic", &["a"]);
    app.selected = app.row_of_session("a").unwrap();

    // What `→` routes to on a session row.
    assert!(
        !app.enter_selected_category(),
        "a session row is not a category action"
    );
    assert_eq!(app.focus, Focus::List, "must not switch to the terminal");
    assert!(app.pending.is_none(), "must not queue a resume");
    assert!(app.panes.is_empty(), "must not add a pane");
}

#[test]
fn an_agent_with_no_token_counts_is_named_but_never_drawn() {
    let mut app = App::new();
    app.visible_aggregate.claude.total = 1_000;
    app.visible_aggregate.claude_count = 1;
    // Kiro and Cursor stores carry no comparable token totals.
    app.visible_aggregate.kiro_count = 3;
    app.visible_aggregate.cursor_count = 2;

    // Account quotas are rows of their own now, so the summary line carries
    // only where the counts came from.
    assert!(
        !app.summary_tail().contains("kiro"),
        "{}",
        app.summary_tail()
    );
    assert!(
        !app.summary_tail().contains("cursor"),
        "{}",
        app.summary_tail()
    );
    assert!(app.quota_rows().is_empty(), "no reading, no rows");
}

/// Every account gets a row, and only a row with a real percentage gets a
/// gauge: an empty gauge would read as "plenty left" when the truth is that
/// the provider reported no window at all.
#[test]
fn each_account_gets_a_row_and_only_real_readings_get_a_gauge() {
    let mut app = App::new();
    app.limits = Some(
        mindplayer_core::limits::Limits {
            claude: Ok(mindplayer_core::limits::ClaudeLimits {
                five_hour: Some(12.0),
                seven_day: Some(31.0),
                ..Default::default()
            }),
            // A business plan reports no window at all — only a balance.
            codex: Ok(mindplayer_core::limits::CodexLimits {
                credit_balance: Some(0.0),
                plan_type: Some("business".into()),
                ..Default::default()
            }),
            kiro: Ok(mindplayer_core::limits::KiroLimits {
                plan_name: Some("KIRO POWER".into()),
                credits_used: Some(185.5),
                credits_total: Some(10_000.0),
                used_percent: Some(1.855),
                reset_date: Some("2026-10-01".into()),
            }),
            cursor: Ok(mindplayer_core::limits::CursorLimits {
                used_percent: Some(30.0),
                used_cents: Some(1500),
                limit_cents: Some(5000),
                remaining_cents: Some(3500),
                billing_cycle_end: Some("2026-10-01T00:00:00.000Z".into()),
                source: Some(mindplayer_core::limits::CursorQuotaSource::Plan),
                ..Default::default()
            }),
        }
        .quota_rows(),
    );

    let rows = app.quota_rows();
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["claude 5h", "claude wk", "codex", "kiro", "cursor included"],
        "claude reports two windows, so it gets two rows"
    );

    let gauged: Vec<&str> = rows
        .iter()
        .filter(|r| r.has_gauge())
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(
        gauged,
        vec!["claude 5h", "claude wk", "kiro", "cursor included"],
        "codex reported no window, so it must not be gauged"
    );

    let codex = rows.iter().find(|r| r.label == "codex").unwrap();
    assert!(
        codex.detail.contains("credits 0"),
        "the balance is still named: {codex:?}"
    );
    let kiro = rows.iter().find(|r| r.label == "kiro").unwrap();
    assert_eq!(kiro.detail, "185.5/10000 cr");
    assert_eq!(kiro.resets.as_deref(), Some("2026-10-01"));
    let cursor = rows.iter().find(|r| r.label == "cursor included").unwrap();
    assert_eq!(cursor.detail, "$15.00/$50");
}

/// A reading that fails must not wipe a number that was true minutes ago.
///
/// The account APIs rate-limit — cursor answers 429 under the refresh cadence —
/// and replacing a good figure with "request failed" loses information for
/// nothing. The failed row yields to the stored one and the footer says how old
/// it is.
#[test]
fn a_failed_reading_falls_back_to_the_last_good_one() {
    let mut app = App::new();
    let taken = Utc::now() - chrono::Duration::minutes(7);
    app.quota_cache = Some((
        vec![
            mindplayer_core::limits::QuotaRow {
                label: "cursor".into(),
                used_percent: Some(12.5),
                detail: "$2.50/$20".into(),
                resets: Some("2026-10-04".into()),
                ..Default::default()
            },
            mindplayer_core::limits::QuotaRow {
                label: "kiro".into(),
                used_percent: Some(6.4),
                detail: "639.7/10000 cr".into(),
                resets: None,
                ..Default::default()
            },
        ],
        taken,
    ));
    // This run: cursor is rate-limited, kiro answered.
    app.limits = Some(
        mindplayer_core::limits::Limits {
            claude: Err("not under test".into()),
            codex: Err("not under test".into()),
            kiro: Ok(mindplayer_core::limits::KiroLimits {
                used_percent: Some(9.9),
                credits_used: Some(990.0),
                credits_total: Some(10_000.0),
                ..Default::default()
            }),
            cursor: Err("curl: (56) The requested URL returned error: 429".into()),
        }
        .quota_rows(),
    );

    let rows = app.quota_rows();
    let cursor = rows.iter().find(|r| r.label == "cursor").unwrap();
    assert_eq!(
        cursor.used_percent,
        Some(12.5),
        "the rate-limited row keeps the last good reading: {cursor:?}"
    );
    let kiro = rows.iter().find(|r| r.label == "kiro").unwrap();
    assert_eq!(
        kiro.used_percent,
        Some(9.9),
        "a provider that answered is current, not cached: {kiro:?}"
    );
    assert_eq!(
        app.quota_cached_at(),
        Some(taken),
        "the footer says how old the stale half is"
    );
}

#[test]
fn a_failed_claude_probe_restores_both_cached_windows() {
    let mut app = isolated_app();
    app.quota_cache = Some((
        vec![
            mindplayer_core::limits::QuotaRow {
                label: "claude 5h".into(),
                used_percent: Some(42.0),
                detail: String::new(),
                resets: Some("14:30".into()),
                ..Default::default()
            },
            mindplayer_core::limits::QuotaRow {
                label: "claude wk".into(),
                used_percent: Some(7.0),
                detail: String::new(),
                resets: Some("09-20".into()),
                ..Default::default()
            },
        ],
        Utc::now() - chrono::Duration::minutes(7),
    ));
    app.limits = Some(
        mindplayer_core::limits::Limits {
            claude: Err("curl failed: HTTP 429".into()),
            codex: Err("not under test".into()),
            kiro: Err("not under test".into()),
            cursor: Err("not under test".into()),
        }
        .quota_rows(),
    );

    let rows = app.quota_rows();

    assert!(
        rows.iter()
            .any(|row| row.label == "claude 5h" && row.used_percent == Some(42.0)),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.label == "claude wk" && row.used_percent == Some(7.0)),
        "{rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.label != "claude"),
        "the failure placeholder must yield to both stored windows: {rows:?}"
    );
}

#[test]
fn a_recent_account_cache_prevents_an_immediate_duplicate_fetch() {
    let mut app = isolated_app();
    app.quota_cache = Some((
        vec![mindplayer_core::limits::QuotaRow {
            label: "cursor".into(),
            used_percent: Some(12.5),
            detail: "$2.50/$20".into(),
            resets: None,
            ..Default::default()
        }],
        Utc::now(),
    ));

    app.spawn_limits_fetch();

    assert!(
        app.limits_rx.is_none(),
        "a second process starting against a fresh shared cache must not hit the account APIs again"
    );
}

/// Every pane runs its own `App`, and the account limit is per account, not per
/// process: two instances on independent five-minute timers ask twice as often
/// as one, which is how claude ends up answering 429 while cursor and kiro are
/// fine. The cache on disk is the only record that a sibling just asked, so a
/// reading already in hand does not license a fetch of its own.
#[test]
fn a_sibling_refresh_holds_back_an_instance_that_already_has_a_reading() {
    let mut app = isolated_app();
    app.limits = Some(
        mindplayer_core::limits::Limits {
            claude: Err("curl failed: curl: (56) The requested URL returned error: 429".into()),
            codex: Err("not under test".into()),
            kiro: Err("not under test".into()),
            cursor: Err("not under test".into()),
        }
        .quota_rows(),
    );
    app.quota_cache = Some((
        vec![mindplayer_core::limits::QuotaRow {
            label: "claude wk".into(),
            used_percent: Some(98.0),
            detail: String::new(),
            resets: Some("09-20".into()),
            ..Default::default()
        }],
        Utc::now(),
    ));

    app.spawn_limits_fetch();

    assert!(
        app.limits_rx.is_none(),
        "a sibling's fresh reading must satisfy this instance instead of doubling the request rate"
    );
}

/// A long-running process keeps its startup cache in memory. If a sibling
/// refreshes the shared file later, this instance must re-read it before its
/// own timer spends the same account budget again.
#[test]
fn a_sibling_refresh_replaces_a_stale_in_memory_cache_before_fetching() {
    let home = std::env::temp_dir().join(format!(
        "mp-shared-limits-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut app = isolated_app();
    app.quota_cache = Some((
        vec![mindplayer_core::limits::QuotaRow {
            label: "cursor".into(),
            used_percent: Some(12.5),
            detail: "$2.50/$20".into(),
            resets: None,
            ..Default::default()
        }],
        Utc::now() - chrono::Duration::minutes(10),
    ));
    app.limits = Some(
        mindplayer_core::limits::Limits {
            claude: Ok(Default::default()),
            codex: Ok(Default::default()),
            kiro: Ok(Default::default()),
            cursor: Ok(Default::default()),
        }
        .quota_rows(),
    );
    let sibling_rows = vec![mindplayer_core::limits::QuotaRow {
        label: "cursor".into(),
        used_percent: Some(27.5),
        detail: "$5.50/$20".into(),
        resets: Some("2026-10-04".into()),
        ..Default::default()
    }];
    mindplayer_core::limits::save_quota_cache(&home, &sibling_rows, BUILD);

    app.spawn_limits_fetch_from(home.clone());

    assert!(
        app.limits_rx.is_none(),
        "the newer shared reading must suppress this process's duplicate fetch"
    );
    assert_eq!(app.quota_rows(), sibling_rows);
    assert!(
        app.limits.is_none(),
        "the older live snapshot must yield to the newer shared reading"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_429_account_response_defers_the_next_fetch() {
    let mut app = isolated_app();
    let (tx, rx) = mpsc::channel();
    tx.send(
        mindplayer_core::limits::Limits {
            claude: Ok(Default::default()),
            codex: Ok(Default::default()),
            kiro: Ok(Default::default()),
            cursor: Err("curl failed: response status 429".into()),
        }
        .quota_rows(),
    )
    .unwrap();
    app.limits_rx = Some(rx);
    app.limits_started = Some(Instant::now());
    assert!(app.poll_limits(), "the completed response is adopted");
    assert_eq!(
        app.limits_backoff,
        LIMITS_REFRESH_INTERVAL * 2,
        "the first 429 doubles the ordinary account refresh delay"
    );

    app.spawn_limits_fetch();

    assert!(
        app.limits_rx.is_none(),
        "a rate-limited response must back off instead of retrying on the next three-second tick"
    );
}

/// Doubling has to stop somewhere, and where it stops decides how long a row
/// keeps saying "rate limited" after the account's budget has reopened. The
/// endpoint's own answer carries `retry-after: 0`, so nothing tells us when to
/// come back; the cap is the only promise that we will look again soon enough.
#[test]
fn a_run_of_429s_stops_growing_the_wait_at_a_cap_that_still_looks_again_within_the_hour() {
    let mut app = isolated_app();
    for _ in 0..8 {
        let (tx, rx) = mpsc::channel();
        tx.send(
            mindplayer_core::limits::Limits {
                claude: Err("rate limited by the account API (HTTP 429)".into()),
                codex: Ok(Default::default()),
                kiro: Ok(Default::default()),
                cursor: Ok(Default::default()),
            }
            .quota_rows(),
        )
        .unwrap();
        app.limits_rx = Some(rx);
        app.limits_started = Some(Instant::now());
        assert!(app.poll_limits());
    }

    assert_eq!(
        app.limits_backoff, LIMITS_MAX_BACKOFF,
        "the wait grows to the cap and no further"
    );
    assert!(
        LIMITS_MAX_BACKOFF * 4 <= Duration::from_secs(60 * 60),
        "a refused reading is still retried several times an hour: {LIMITS_MAX_BACKOFF:?}"
    );
}

/// A provider that failed still gets a row saying why, so an agent never
/// silently disappears from the footer.
#[test]
fn a_failed_reading_becomes_a_row_that_explains_itself() {
    let mut app = App::new();
    app.limits = Some(
        mindplayer_core::limits::Limits {
            claude: Err("keychain item has no claudeAiOauth token".into()),
            codex: Err("no codex rollouts found".into()),
            kiro: Err("no local Kiro profile".into()),
            cursor: Err("no Cursor Agent access token in macOS Keychain".into()),
        }
        .quota_rows(),
    );
    let rows = app.quota_rows();
    assert_eq!(rows.len(), 4, "one row per provider even when all failed");
    assert!(
        rows.iter().all(|r| !r.has_gauge()),
        "a failure must never be drawn as a gauge: {rows:?}"
    );
    assert!(rows[0].detail.contains("claudeAiOauth"), "{rows:?}");
}

#[test]
fn the_footer_shortens_the_working_dir_but_the_scan_screen_does_not() {
    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/Users/eden.jang/Work/SendBird/working2"));
    app.cwd = PathBuf::from("/Users/eden.jang/Work/SendBird/working2");

    assert_eq!(app.scope_label_short(), "working dir (SendBird/working2)");
    assert_eq!(
        app.scope_label(),
        "working dir (/Users/eden.jang/Work/SendBird/working2)"
    );
    assert!(app.summary_tail().contains("SendBird/working2"));
    assert!(!app.summary_tail().contains("/Users/eden.jang"));
}

#[test]
fn a_short_path_survives_shortening_unchanged() {
    let mut app = App::new();
    app.scope = Scope::WorkingDir(PathBuf::from("/tmp"));
    app.cwd = PathBuf::from("/tmp");
    assert_eq!(app.scope_label_short(), "working dir (/tmp)");

    app.scope = Scope::Global;
    assert_eq!(app.scope_label_short(), "global");
    assert!(
        app.summary_tail().contains("· global"),
        "the smoke test greps for this exact marker: {}",
        app.summary_tail()
    );
}

/// Every pane opened at a monorepo root shares one cwd. The sweep used to walk
/// that tree once per pane, which on this workspace meant six walks of ~32,000
/// entries every three seconds — the sweep never finished before the next was
/// due, so it ran continuously and burned a core.
#[test]
fn panes_sharing_one_directory_are_walked_once_not_once_each() {
    use std::cell::RefCell;
    let shared = PathBuf::from("/tmp/one-root");
    let other = PathBuf::from("/tmp/other-root");
    let targets = vec![
        ("s1".to_string(), shared.clone()),
        ("s2".to_string(), shared.clone()),
        ("s3".to_string(), shared.clone()),
        ("s4".to_string(), other.clone()),
    ];

    let walked: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
    let batch = crate::app::pane::walk_targets_once(targets, |dir| {
        walked.borrow_mut().push(dir.to_path_buf());
        vec![(dir.join("page.html"), SystemTime::UNIX_EPOCH)]
    });

    // Four panes, two distinct directories: two walks, not four.
    assert_eq!(walked.borrow().len(), 2, "walked: {:?}", walked.borrow());
    assert_eq!(
        batch.len(),
        4,
        "every pane still gets its own entry in the batch"
    );
    // And the three panes sharing a root all got that root's finding.
    for (id, found) in batch.iter().filter(|(id, _)| id != "s4") {
        assert_eq!(found.len(), 1, "{id}");
        assert!(found[0].0.starts_with(&shared), "{id}: {found:?}");
    }
}

/// The interval is measured from when a sweep LANDS, not from when it starts.
/// Re-arming at spawn time meant a walk slower than the interval finished into
/// an already-expired timer, so the next sweep began immediately and the walk
/// ran back to back forever.
#[test]
fn the_next_sweep_is_scheduled_from_when_the_last_one_landed() {
    let dir = temp_html_dir("interval");
    std::fs::write(dir.join("page.html"), "<html></html>").unwrap();

    let mut app = app_with(vec![session_in(
        "s1",
        Agent::Codex,
        &dir.display().to_string(),
        "t",
    )]);
    app.focus_or_add_pane("s1");

    app.html_candidates_due = None;
    assert!(app.spawn_html_scan(), "a sweep starts");
    assert!(
        app.html_candidates_due.is_none(),
        "an in-flight sweep leaves nothing scheduled"
    );

    finish_html_scan(&mut app);
    let due = app
        .html_candidates_due
        .expect("landing schedules the next sweep");
    assert!(due > Instant::now(), "the next sweep is in the future");
    assert!(
        !app.spawn_html_scan(),
        "and it does not start again until that interval has passed"
    );
}

/// A pane opened somewhere pathological must not make the sweep unbounded.
#[test]
fn the_walk_stops_at_its_entry_ceiling() {
    let dir = temp_html_dir("ceiling");
    // Far below the real ceiling, so this asserts the loop's shape, not the
    // constant: with no cap the walk would return every one of these.
    for i in 0..40 {
        std::fs::write(dir.join(format!("f{i}.html")), "<html></html>").unwrap();
    }
    let found = crate::app::pane::scan_html_candidates(&dir);
    assert_eq!(found.len(), 40, "a small tree is never truncated");
    assert!(
        found.len() <= crate::app::pane::HTML_WALK_MAX_ENTRIES,
        "the walk never returns more than it is allowed to visit"
    );
}

/// A pane must run on the account that owns the session, not on whichever one
/// a new pane of that agent would have started on.
mod account_selection {
    use super::*;
    use mindplayer_core::accounts::{Account, Role, DEFAULT_ACCOUNT};

    fn app_with_two_codex_accounts(home: &std::path::Path) -> (App, Account) {
        let mut app = App::new();
        let second = Account::isolated(home, Agent::Codex, "overflow").unwrap();
        app.accounts = vec![Account::inherited(Agent::Codex), second.clone()];
        (app, second)
    }

    #[test]
    fn a_new_pane_takes_the_primary_account() {
        let home = std::env::temp_dir().join("mp-account-pick");
        let (app, _) = app_with_two_codex_accounts(&home);
        assert_eq!(app.account_for(Agent::Codex).name, DEFAULT_ACCOUNT);
    }

    #[test]
    fn a_disabled_primary_is_passed_over() {
        let home = std::env::temp_dir().join("mp-account-pick");
        let (mut app, second) = app_with_two_codex_accounts(&home);
        app.accounts[0].disabled = true;
        assert_eq!(app.account_for(Agent::Codex).name, second.name);
    }

    #[test]
    fn a_fallback_account_waits_for_the_primaries_to_run_out() {
        let home = std::env::temp_dir().join("mp-account-pick");
        let (mut app, second) = app_with_two_codex_accounts(&home);
        app.accounts[1].role = Role::Fallback;
        assert_eq!(app.account_for(Agent::Codex).name, DEFAULT_ACCOUNT);
        app.accounts[0].disabled = true;
        assert_eq!(app.account_for(Agent::Codex).name, second.name);
    }

    #[test]
    fn resuming_uses_the_account_whose_store_holds_the_session() {
        let home = super::super::limits_home_for_app();
        let (app, second) = app_with_two_codex_accounts(&home);

        let mut owned = session("sid-1", Agent::Codex, false);
        owned.file = second
            .session_root(&home)
            .join("2026/09/15/rollout-z.jsonl");
        assert_eq!(
            app.account_of_session(&owned).name,
            second.name,
            "a session in the second account's store would have resumed on the first"
        );
        assert_ne!(
            app.account_for(Agent::Codex).name,
            second.name,
            "this test proves nothing unless the two answers differ"
        );
    }

    #[test]
    fn a_second_account_adds_its_store_to_the_scan() {
        let home = super::super::limits_home_for_app();
        let (app, second) = app_with_two_codex_accounts(&home);
        let roots = app.scan_roots();
        assert!(
            roots.iter().any(|r| r.dir == second.session_root(&home)),
            "the second account's store is never scanned: {roots:?}"
        );
        assert_eq!(
            roots.iter().filter(|r| r.agent == Agent::Cursor).count(),
            1,
            "cursor holds no second account but must still be scanned"
        );
    }
}
