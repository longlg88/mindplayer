//! MindPlayer TUI entry point: terminal setup, the event loop, and key routing.

mod agent_hooks;
mod app;
mod convo_log;
mod handoff;
mod kiro_patterns;
mod links;
mod local_router;
mod mascot;
mod observe;
mod pty;
mod render_writer;
mod startup_routing;
mod terminal_view;
mod text_input;
mod turn_status;
mod ui;
mod walker;

use anyhow::Result;
use app::{App, Focus, Screen};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use mindplayer_core::Agent;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::Terminal;
use render_writer::FrameSink;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const HELP: &str = "\
mindplayer — a session manager for Codex / Claude / Kiro / Cursor / OMP

USAGE:
    mindplayer [DIR]

ARGS:
    DIR    Directory whose sessions to manage (the working-dir scope, and where
           new sessions launch). Defaults to the current directory. Pass a path
           to target another project without cd-ing there, e.g.:
               mindplayer ~/code/my-project

OPTIONS:
    -h, --help                Print this help
    -v, -V, --version         Print version
    --install-agent-hooks     Show what registering mindplayer's Claude
                              Code/Codex lifecycle hooks would change in
                              ~/.claude/settings.json and ~/.codex/hooks.json
                              (dry run — prints the plan, changes nothing)
    --install-agent-hooks --apply
                              Actually write those two files (additive —
                              never removes or reorders your existing hooks)

On the first screen choose 'working dir' (this DIR) or 'global' (all sessions).
After scanning, open a session directly. Codex starts with Jev ON when a Jev key is
configured, and OFF without one: F6 toggles Jev ON/OFF in the native input row for the
same session. The mode remains for subsequent prompts;
selection alone never submits. Enter submits using the current mode. Jev ON sends
the bounded judgment context externally. OFF keeps the current native model and
effort and makes no Jev call. Claude / Kiro / Cursor sessions keep their native
input and controls. OMP runs independently; Ctrl-X returns to the list,
Ctrl-Z zooms, Alt-W switches panes, and Ctrl-Q or Alt-Q closes its pane.";

const IDENTITY_HELPER_FLAG: &str = "--mindplayer-verify-codex-identity";
const IDENTITY_HELPER_MAX_BYTES: u64 = 1024 * 1024;

fn verify_codex_identity_input(input: impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    input
        .take(IDENTITY_HELPER_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > IDENTITY_HELPER_MAX_BYTES {
        return None;
    }
    let request: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let account = request.get("accountRead")?;
    let quota = request.get("quotaRead")?;
    mindplayer_core::limits::codex_exchange_fingerprint(
        account,
        if quota.is_null() { None } else { Some(quota) },
    )
    .ok()
}

fn run_identity_helper() -> bool {
    let Some(fingerprint) = verify_codex_identity_input(io::stdin().lock()) else {
        return false;
    };
    serde_json::to_writer(
        io::stdout().lock(),
        &serde_json::json!({ "fingerprint": fingerprint }),
    )
    .is_ok()
}

#[cfg(test)]
mod identity_helper_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn verifies_workspace_and_legacy_exchanges() {
        let account = json!({"result":{"account":{"type":"chatgpt","email":" One@Example.COM "},
            "workspaceRouting":{"chatgptAccountId":"workspace-shared"}}});
        let quota = json!({"result":{"accountId":"workspace-shared"}});
        let request = json!({"accountRead":account,"quotaRead":quota});
        let modern = verify_codex_identity_input(request.to_string().as_bytes()).unwrap();
        assert_eq!(modern, "codex-user-48ba19b0360d2c16");
        let mut no_quota = request.clone();
        no_quota["quotaRead"] = serde_json::Value::Null;
        assert_eq!(
            verify_codex_identity_input(no_quota.to_string().as_bytes()),
            Some(modern.clone())
        );
        let mut incomplete = request.clone();
        incomplete["accountRead"]["result"]["workspaceRouting"] = json!({});
        assert!(verify_codex_identity_input(incomplete.to_string().as_bytes()).is_none());
        let mut legacy = request.clone();
        legacy["accountRead"]["result"]
            .as_object_mut()
            .unwrap()
            .remove("workspaceRouting");
        assert_eq!(
            verify_codex_identity_input(legacy.to_string().as_bytes()),
            Some(modern)
        );
        let mut conflict = request;
        conflict["quotaRead"]["result"]["accountId"] = json!("another");
        assert!(verify_codex_identity_input(conflict.to_string().as_bytes()).is_none());
    }

    #[test]
    fn rejects_malformed_missing_and_oversized_input() {
        assert!(verify_codex_identity_input(&b"not json"[..]).is_none());
        assert!(verify_codex_identity_input(&b"{\"accountRead\":{}}"[..]).is_none());
        assert!(
            verify_codex_identity_input(&b"{\"accountRead\":{},\"quotaRead\":null}"[..]).is_none()
        );
        let oversized = vec![b' '; IDENTITY_HELPER_MAX_BYTES as usize + 1];
        assert!(verify_codex_identity_input(oversized.as_slice()).is_none());
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new(IDENTITY_HELPER_FLAG)) {
        let ok = args.next().is_none() && run_identity_helper();
        std::process::exit(if ok { 0 } else { 1 });
    }
    // Resolve the target directory from args BEFORE touching the terminal, so
    // --help/--version print normally.
    let explicit_dir = explicit_dir_from_args();

    // Restore the terminal even on a panic — otherwise a crash inside the event
    // loop leaves the real terminal in raw + mouse-reporting + alt-screen mode,
    // which breaks click/selection/copy-paste *outside* MindPlayer until the
    // user runs `reset`. The hook runs before the default panic printer so the
    // backtrace lands on a sane screen.
    install_panic_hook();
    // Everything mindplayer keeps is a record of the user's own sessions. Files
    // written before that rule existed are still on disk at the ambient umask,
    // and there are far too many to rewrite; narrowing the directory they live
    // in is what puts them out of reach. Best-effort: a home directory we
    // cannot chmod is not a reason to refuse to start.
    if let Err(e) =
        mindplayer_core::private::migrate_data_root(&mindplayer_core::private::data_root())
    {
        eprintln!("mindplayer: could not narrow ~/.mindplayer permissions: {e}");
    }
    let mut terminal = setup()?;
    let mut app = match explicit_dir {
        Some(dir) => App::new_in(dir),
        None => App::new(), // current directory
    };
    // `std::process::id()` is the join key for pairing this run's start/stop
    // in the audit log — needed because more than one mindplayer instance is
    // routinely open at once (one per project), so chronological order alone
    // can't tell overlapping runs apart.
    let run_id = std::process::id();
    mindplayer_core::log_event_to(
        &app.audit_path,
        mindplayer_core::AuditEvent::AppStart { run_id },
    );
    let res = run(&mut terminal, &mut app);
    mindplayer_core::log_event_to(
        &app.audit_path,
        mindplayer_core::AuditEvent::AppStop { run_id },
    );
    teardown(&mut terminal)?;
    res
}

/// The directory passed as the first positional CLI arg, if any (so
/// `mindplayer <dir>` targets that project from anywhere). `None` means no
/// directory was given and the current directory should be used.
/// Handles `--help`/`--version` by printing and exiting.
fn explicit_dir_from_args() -> Option<PathBuf> {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "-v" | "-V" | "--version" => {
                println!("mindplayer {}", env!("MINDPLAYER_VERSION"));
                std::process::exit(0);
            }
            "--install-agent-hooks" => {
                let apply = std::env::args().any(|a| a == "--apply");
                run_install_agent_hooks(apply);
                std::process::exit(0);
            }
            s if !s.starts_with('-') => {
                // Resolve to an absolute path so the scope matches the cwd the
                // CLIs record in their session files; fall back to the raw path.
                let p = PathBuf::from(s);
                return Some(std::fs::canonicalize(&p).unwrap_or(p));
            }
            _ => {} // ignore unknown flags
        }
    }
    None
}

/// `--install-agent-hooks` (dry run, prints the plan) / `--install-agent-hooks
/// --apply` (writes it). Registers mindplayer's lifecycle-hook script for
/// Claude Code and Codex additively — existing hooks for the same events are
/// left exactly as they are; only a new matcher-group entry is appended, and
/// only for events that don't already reference our script.
fn run_install_agent_hooks(apply: bool) {
    let (claude, codex) = agent_hooks::plan_install();
    let script_path = agent_hooks::hook_script_path();

    println!(
        "mindplayer agent-hook installer{}",
        if apply {
            " (applying)"
        } else {
            " (dry run — nothing written yet)"
        }
    );
    println!();
    println!("hook script: {}", script_path.display());
    println!();
    for (name, path, merged) in [
        ("Claude Code", agent_hooks::claude_settings_path(), &claude),
        ("Codex", agent_hooks::codex_hooks_path(), &codex),
    ] {
        println!("{name}: {}", path.display());
        if merged.added.is_empty() {
            println!("  (already fully registered, nothing to add)");
        } else {
            println!("  would add for: {}", merged.added.join(", "));
        }
        if !merged.already_present.is_empty() {
            println!(
                "  already registered for: {}",
                merged.already_present.join(", ")
            );
        }
    }
    println!();

    if !apply {
        println!("Nothing was written. Re-run with --apply to write these two files.");
        println!("Both merges are additive — your existing hooks for these events are kept as-is.");
        return;
    }

    match agent_hooks::apply_install(&claude, &codex) {
        Ok(()) => {
            println!("Done.");
            println!(
                "Codex may still require trusting the new hook once: run `codex`, then `/hooks`."
            );
        }
        Err(e) => {
            eprintln!("Failed to install: {e}");
            std::process::exit(1);
        }
    }
}

/// Best-effort: undo every terminal mode `setup()` turned on. Writes directly to
/// stdout so it works from a panic hook (no Terminal handle needed). Each step
/// is independent so one failure never blocks the rest.
fn restore_terminal() {
    // Wait for any frame still in flight through the render-writer thread
    // (see `render_writer`) before writing anything to stdout ourselves —
    // otherwise these escape codes can race a queued frame and corrupt the
    // terminal right as the user quits, exactly the scenario that module
    // exists to protect against mid-session. A no-op if the render writer
    // never started (e.g. a panic before `setup()` ran).
    render_writer::drain_global();
    let mut out = io::stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, DisableMouseCapture);
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default(info);
    }));
}

fn setup() -> Result<Terminal<CrosstermBackend<FrameSink>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Best-effort kitty keyboard protocol so we can distinguish Shift/Alt+Enter
    // (for newline-without-submit). Ignored by terminals that don't support it.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    // Capture the mouse so MindPlayer receives wheel/click events: it forwards
    // them to mouse-aware children and otherwise scrolls its own scrollback.
    let _ = execute!(stdout, EnableMouseCapture);
    // Advertise bracketed paste so the terminal delivers pastes as one
    // Event::Paste (no per-key replay) AND stops showing its own "this paste
    // may be dangerous" protection prompt — we forward the paste safely below.
    let _ = execute!(stdout, EnableBracketedPaste);
    // From here on the real stdout is owned by the render-writer thread; the
    // Terminal itself only ever renders into an in-memory buffer (see
    // `render_writer` for why: a stuck terminal must not stall input polling).
    let backend = CrosstermBackend::new(FrameSink::spawn(stdout));
    Ok(Terminal::new(backend)?)
}

fn teardown(terminal: &mut Terminal<CrosstermBackend<FrameSink>>) -> Result<()> {
    // Best-effort and unconditional: every mode is undone even if an earlier
    // step errors, so we never leave the terminal half-restored. Writes
    // directly to a fresh stdout handle (not through the render-writer),
    // which is also what makes this safe to call from the panic hook.
    restore_terminal();
    let _ = terminal.show_cursor();
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<FrameSink>>, app: &mut App) -> Result<()> {
    let mut summary_since: Option<Instant> = None;
    let mut last_anim = Instant::now();
    let mut last_refresh = Instant::now();
    // Kicked off here rather than on the first three-second tick: reading the
    // accounts takes seconds of its own, and waiting for a tick before even
    // starting adds that delay to it.
    app.spawn_limits_fetch();
    // Render only when something actually changed (input, PTY output, list
    // re-order, animation). This is the key to smoothness: an idle screen or a
    // live session with no new bytes costs zero redraws instead of 60fps of
    // rebuilding the whole list + vt100 grid.
    let mut needs_draw = true;

    while !app.should_quit {
        // A lone Escape is held briefly by each PTY to distinguish a real key
        // from a fragmented cursor-position report. Release genuine Escapes on
        // the normal event-loop clock even when no further key arrives.
        app.flush_terminal_reply_guards();
        if app.poll_account_auth() {
            needs_draw = true;
        }
        if app.startup_routing.poll() {
            needs_draw = true;
        }
        if !app.startup_routing.active() && app.raise_unusable_prompt() {
            needs_draw = true;
        }
        if app.screen == Screen::Main && !app.startup_routing.active() && app.pending.is_some() {
            // Pane sizes are normally recorded by `ui::render`, but publishing
            // that first layout used to expose a transient `starting...` frame
            // before the PTY existed. Measure the exact same layout against an
            // in-memory backend, spawn at those sizes, then publish only the
            // real terminal frame.
            let size = terminal.size()?;
            let mut measurement = Terminal::new(TestBackend::new(size.width, size.height))?;
            measurement.draw(|f| ui::render(f, app))?;
            while app.pending.is_some() {
                app.spawn_pending();
            }
            needs_draw = true;
        }
        if needs_draw {
            // `FrameSink` (see `render_writer`) buffers this in memory and
            // hands it to a background writer thread on flush, so a
            // terminal that's fallen behind under load can never stall the
            // input-polling loop below.
            terminal.draw(|f| ui::render(f, app))?;
            needs_draw = false;
        }

        // Keep existing PTYs aligned with the pane geometry recorded by the
        // visible draw. Pending PTYs were already measured and spawned above.
        if app.screen == Screen::Main {
            app.sync_pty_size();
            if app.reap_pty() {
                needs_draw = true;
            }
            // Track which sessions are actively producing output (status badge).
            if app.poll_activity() && !app.observer_open() {
                needs_draw = true;
            }
            if app.poll_observer() {
                needs_draw = true;
            }
            // Refresh Claude/Codex hook-derived status (see `agent_hooks`) —
            // takes over the badge from the screen-text heuristic whenever a
            // pane has one.
            if app.poll_hook_status() {
                needs_draw = true;
            }
            // Keep panes that need attention (blocked, then working) at the
            // front of the grid regardless of when they were opened.
            if app.reorder_panes_by_status() {
                needs_draw = true;
            }
            // Record any idle/working/blocked/ended transition to the audit log
            // (change-only). Purely a side effect — status is already reflected
            // on screen, so this never forces a redraw.
            app.poll_status_transitions();
            // Interval-gated walk of each live pane's cwd for newly-written
            // `.html` files (drives the "🌐 N new" badge and the Ctrl-P picker).
            if app.poll_html_candidates() {
                needs_draw = true;
            }
            if app.flush_initial_inputs() {
                needs_draw = true;
            }
            // Live re-ordering from the background mtime refresh.
            if app.poll_refresh() {
                needs_draw = true;
            }
            // Apply a finished background peer-lane transcript read (see
            // `spawn_thread_sync_for`) — kept off this thread so reopening a
            // thread-synced session never freezes input/rendering while its
            // peers' transcripts are read.
            if app.poll_category_sync() {
                needs_draw = true;
            }
            if app.poll_thread_sync() {
                needs_draw = true;
            }
            // Conversation log: adopt a finished batch, then queue the next one.
            // Both are cheap here — the file walking happens on the worker.
            if app.poll_convo_ingest() {
                needs_draw = true;
            }
            app.spawn_convo_ingest();
            if app.poll_limits() {
                needs_draw = true;
            }
            if last_refresh.elapsed() >= Duration::from_secs(3) {
                if app.reload_accounts_if_changed() {
                    needs_draw = true;
                }
                app.start_refresh();
                // Account fetches enforce their own slower cadence and 429 backoff.
                app.spawn_limits_fetch();
                last_refresh = Instant::now();
            }
            // Pick up newly created sessions (and resolve their labels).
            if app.poll_bg_rescan() {
                needs_draw = true;
            }
            if app.rescan_due() {
                app.start_bg_rescan();
            }
        }

        // New PTY output → redraw the live pane.
        if app.pty_dirty() && !app.observer_open() {
            needs_draw = true;
        }

        // Poll fast (~60fps) while typing into a live session so keystrokes and
        // their echoes feel as immediate as a native terminal; idle screens
        // poll lazily to stay easy on the CPU.
        let live = app.screen == Screen::Main && app.focus == Focus::Terminal && app.has_live_pty();
        let poll = Duration::from_millis(if live { 16 } else { 50 });
        if event::poll(poll)? {
            // Drain everything queued this frame so pastes / fast typing aren't
            // throttled to one event per frame.
            loop {
                let startup_input = app.screen == Screen::Main && app.startup_routing.active();
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        handle_key(app, key);
                        needs_draw = true;
                    }
                    Event::Mouse(me) => {
                        if handle_mouse(app, me) {
                            needs_draw = true;
                        }
                    }
                    Event::Paste(text) => {
                        if !app.startup_routing.active() && app.paste_to_pty(&text) {
                            needs_draw = true;
                        }
                    }
                    Event::Resize(_, _) => needs_draw = true,
                    _ => {}
                }
                // Publish the Jev disclosure after a choice change before
                // accepting a queued Enter as consent. Normal native typing
                // continues to drain the whole event batch below.
                // The Unix TTY backend checks its deadline before buffered
                // events. A zero timeout would leave those keys for later frames.
                let drain_poll = if cfg!(unix) {
                    Duration::from_millis(1)
                } else {
                    Duration::ZERO
                };
                if startup_input || !event::poll(drain_poll)? {
                    break;
                }
            }
        }

        // A finished drag-copy selection queued text for the clipboard; push it
        // now that input handling is done.
        if let Some(text) = app.take_clipboard() {
            set_clipboard(&text);
            needs_draw = true;
        }

        // Animation tick is time-based (~12fps); only the spinner / summary
        // screens animate, so only they force a redraw here.
        if last_anim.elapsed() >= Duration::from_millis(80) {
            app.tick();
            last_anim = Instant::now();
            // Redraw to animate the mascot / spinner wherever it's shown.
            if app.mascot_visible() {
                needs_draw = true;
            }
            // Keep redrawing briefly after output so a "working" badge can decay
            // back to "idle" even when no new events arrive.
            if app.any_recent_activity() && !app.observer_open() {
                needs_draw = true;
            }
        }

        match app.screen {
            Screen::Scanning => {
                if app.poll_scan() {
                    needs_draw = true;
                }
            }
            Screen::ScanSummary => {
                let since = summary_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(800) {
                    app.open_main();
                    summary_since = None;
                    needs_draw = true;
                }
            }
            _ => summary_since = None,
        }
    }
    Ok(())
}

/// Mouse wheel: scroll the live session's scrollback (so history that ran off
/// the top is readable) or move the list selection. Returns true to redraw.
fn handle_mouse(app: &mut App, me: MouseEvent) -> bool {
    if app.screen != Screen::Main || app.startup_routing.active() {
        return false;
    }
    const STEP: isize = 3;

    // NOTE: mouse events never leave the live view — moving the mouse over a
    // neighbor pane (left of the focused pane) used to flip focus back to the
    // list, so shaking the mouse or drifting across a multi-pane split kept
    // dropping the user out. Leaving the live view is keyboard-only (ctrl-x).

    // In the list, the wheel moves the selection.
    if app.focus == Focus::List {
        return match me.kind {
            MouseEventKind::ScrollUp => {
                app.move_selection(-1);
                true
            }
            MouseEventKind::ScrollDown => {
                app.move_selection(1);
                true
            }
            _ => false,
        };
    }

    // The prompt rail and execution history have distinct wheel behavior.
    // Test these rendered hit targets before the outer Trace rectangle so a
    // mouse-aware Codex child can never receive input intended for Trace.
    if app.observer_prompt_contains(me.column, me.row) {
        return match me.kind {
            MouseEventKind::ScrollUp => app.move_observer_turn(-1),
            MouseEventKind::ScrollDown => app.move_observer_turn(1),
            _ => true,
        };
    }
    if app.observer_detail_contains(me.column, me.row) {
        return match me.kind {
            MouseEventKind::ScrollUp => app.scroll_observer(-STEP),
            MouseEventKind::ScrollDown => app.scroll_observer(STEP),
            _ => true,
        };
    }
    if app.observer_contains(me.column, me.row) {
        return true;
    }

    // Left-drag is reserved for MindPlayer pane-local copy, even when the child
    // is a full-screen mouse app. That prevents Ghostty/native selection from
    // copying the whole terminal row across neighboring panes.
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.selection_start(me.column, me.row);
            return true;
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.selection_update(me.column, me.row);
            return true;
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // A real drag copies; a plain click (no drag) does not — instead
            // forward it as a genuine click to a mouse-aware child (codex etc.)
            // so its UI still responds, and never copies a stray cell.
            if app.selection_finish() {
                return true;
            }
            if app.active_wants_mouse() {
                let (col, row) = app.pane_relative(me.column, me.row);
                app.forward_mouse_to_pty(0, false, false, col, row); // press
                app.forward_mouse_to_pty(0, true, false, col, row); // release
            }
            return true;
        }
        _ => {}
    }

    // Live session focused. If the child requested mouse reporting (full-screen
    // TUIs like codex), forward non-selection events so IT scrolls — its
    // scrollback lives inside the app, not MindPlayer's vt100 buffer.
    if app.active_wants_mouse() {
        if let Some((cb, release, motion)) = encode_mouse_kind(me.kind) {
            let (col, row) = app.pane_relative(me.column, me.row);
            return app.forward_mouse_to_pty(cb, release, motion, col, row);
        }
        return false;
    }
    // Non-mouse pane (e.g. claude on the normal screen): left-drag selects text
    // WITHIN the focused pane for copy (so a neighbor pane is never included),
    // and the wheel scrolls MindPlayer's own scrollback.
    match me.kind {
        MouseEventKind::ScrollUp => app.scroll_active(STEP),
        MouseEventKind::ScrollDown => app.scroll_active(-STEP),
        _ => false,
    }
}

/// Push text to the system clipboard via the OSC 52 escape sequence (supported
/// Copy `text` to the system clipboard. Prefers the OS clipboard tool (pbcopy on
/// macOS, wl-copy / xclip on Linux) because that works regardless of the
/// terminal's OSC 52 policy (Ghostty and others may silently ignore clipboard
/// escapes). Falls back to OSC 52 when no tool is available.
fn set_clipboard(text: &str) {
    #[cfg(target_os = "macos")]
    {
        if copy_via_command("pbcopy", &[], text).is_ok() {
            return;
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if copy_via_command("wl-copy", &[], text).is_ok()
            || copy_via_command("xclip", &["-selection", "clipboard"], text).is_ok()
        {
            return;
        }
    }
    let _ = write_clipboard_osc52(text);
}

/// Pipe `text` into an external clipboard helper's stdin. stdout/stderr are
/// discarded so it never disturbs the live terminal.
fn copy_via_command(prog: &str, args: &[&str], text: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    child.wait()?;
    Ok(())
}

/// by Ghostty, iTerm2, kitty, tmux, and most modern terminals). Dependency-free
/// — we write straight to the terminal we already own.
fn write_clipboard_osc52(text: &str) -> io::Result<()> {
    let b64 = base64_encode(text.as_bytes());
    let mut out = io::stdout();
    out.write_all(format!("\x1b]52;c;{b64}\x07").as_bytes())?;
    out.flush()
}

/// Minimal standard-alphabet base64 (no padding shortcuts) — avoids pulling in a
/// crate just for the OSC 52 payload.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// xterm button code for a mouse button (left/middle/right = 0/1/2).
fn mouse_button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Map a crossterm mouse kind to `(button_code, release, motion)` for
/// forwarding, or `None` for events we don't forward (horizontal wheel).
fn encode_mouse_kind(kind: MouseEventKind) -> Option<(u16, bool, bool)> {
    Some(match kind {
        MouseEventKind::ScrollUp => (64, false, false),
        MouseEventKind::ScrollDown => (65, false, false),
        MouseEventKind::Down(b) => (mouse_button_code(b), false, false),
        MouseEventKind::Up(b) => (mouse_button_code(b), true, false),
        MouseEventKind::Drag(b) => (mouse_button_code(b), false, true),
        MouseEventKind::Moved => (3, false, true),
        _ => return None,
    })
}

fn handle_key(app: &mut App, key: KeyEvent) {
    match app.screen {
        Screen::ScopeSelect => {
            // The character picker owns every key while it's open, so enter
            // commits the pick instead of starting the scan and q/esc closes
            // the popup instead of quitting the app.
            if app.walker_picker.is_some() {
                match normalize_shortcut(key.code) {
                    KeyCode::Up => app.move_walker_pick(-1),
                    KeyCode::Down => app.move_walker_pick(1),
                    KeyCode::Enter => app.confirm_walker_pick(),
                    KeyCode::Esc | KeyCode::Char('q') => app.cancel_walker_picker(),
                    _ => {}
                }
                return;
            }
            match normalize_shortcut(key.code) {
                KeyCode::Up => app.scope_choice = 0,
                KeyCode::Down => app.scope_choice = 1,
                KeyCode::Enter => app.start_scan(),
                KeyCode::Char('c') => app.open_walker_picker(),
                KeyCode::Char('q') | KeyCode::Esc => app.quit(),
                _ => {}
            }
        }
        Screen::Scanning => {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                app.quit();
            }
        }
        Screen::ScanSummary => match key.code {
            KeyCode::Enter => app.open_main(),
            KeyCode::Char('q') | KeyCode::Esc => app.quit(),
            _ => {}
        },
        Screen::Main => handle_main_key(app, key),
    }
}

fn handle_main_key(app: &mut App, key: KeyEvent) {
    if app.startup_routing.active() {
        use startup_routing::StartupStage;
        let stage = app.startup_routing.stage;
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && normalize_shortcut(key.code) == KeyCode::Char('c')
        {
            app.quit();
        } else if (key.modifiers & !KeyModifiers::SHIFT).is_empty() {
            match normalize_shortcut(key.code) {
                KeyCode::Esc | KeyCode::Char('q') if stage == StartupStage::Choice => app.quit(),
                KeyCode::Esc => app.startup_routing.back(),
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                    app.startup_routing.toggle();
                }
                KeyCode::Enter => app.startup_routing.confirm(),
                _ => {}
            }
        }
        return;
    }
    // Checked first because it is drawn over every other popup, so the key goes where the eye is.
    if app.unusable_prompt.is_some() {
        match key.code {
            KeyCode::Char('y' | 'Y') => app.confirm_unusable_prompt(),
            KeyCode::Char('n' | 'N') => app.decline_unusable_prompt(),
            _ => app.dismiss_unusable_prompt(),
        }
        return;
    }

    if app.help_visible {
        match key.code {
            KeyCode::Esc => app.close_help(),
            code if is_help_key(code, key.modifiers) => app.close_help(),
            _ => {}
        }
        return;
    }

    // Category menu (`t` on a header). Owns every key while open, and while
    // renaming the letters are text rather than shortcuts.
    if app.category_menu.is_some() {
        let renaming = app
            .category_menu
            .as_ref()
            .is_some_and(|m| m.rename.is_some());
        if renaming {
            match key.code {
                KeyCode::Enter => app.confirm_category_menu(),
                KeyCode::Esc => app.cancel_category_menu(),
                KeyCode::Backspace => app.category_rename_backspace(),
                KeyCode::Char(c) => app.category_rename_push(c),
                _ => {}
            }
        } else {
            match normalize_shortcut(key.code) {
                KeyCode::Up => app.move_category_menu(-1),
                KeyCode::Down => app.move_category_menu(1),
                KeyCode::Enter | KeyCode::Char(' ') => app.confirm_category_menu(),
                KeyCode::Esc | KeyCode::Char('q') => app.cancel_category_menu(),
                _ => {}
            }
        }
        return;
    }

    // Accounts screen (`A`). Owns every key while open, and while typing a
    // name the letters are text rather than shortcuts.
    if app.accounts_panel.is_some() {
        let typing = app
            .accounts_panel
            .as_ref()
            .is_some_and(|p| p.new_name.is_some());
        if typing {
            match key.code {
                KeyCode::Enter => app.accounts_confirm_add(),
                KeyCode::Esc => app.accounts_cancel_add(),
                KeyCode::Backspace => app.accounts_name_backspace(),
                KeyCode::Char(c) => app.accounts_name_push(c),
                _ => {}
            }
        } else {
            // Anything but a second `x` calls the pending erase off, so it can
            // only happen on two deliberate presses in a row.
            let shortcut = normalize_shortcut(key.code);
            if !matches!(shortcut, KeyCode::Char('x')) {
                app.accounts_cancel_remove();
            }
            match shortcut {
                KeyCode::Esc | KeyCode::Char('q') => app.close_accounts(),
                KeyCode::Up | KeyCode::Char('k') => app.accounts_move(-1),
                KeyCode::Down | KeyCode::Char('j') => app.accounts_move(1),
                KeyCode::Enter => app.accounts_start_session(),
                KeyCode::Char('a') => app.accounts_start_add(),
                KeyCode::Char('r') => app.accounts_start_rename(),
                KeyCode::Char('l') => app.accounts_relogin(),
                KeyCode::Char('v') => app.accounts_verify_identity(),
                KeyCode::Char('w') => app.accounts_make_primary(),
                KeyCode::Char('d') => app.accounts_toggle_disabled(),
                KeyCode::Char('x') => app.accounts_remove(),
                _ => {}
            }
        }
        return;
    }

    // Category picker (`t`). Owns every key while open — including `q`, which
    // must not quit the app mid-pick — and while typing a new name the letters
    // are text, not shortcuts.
    if app.category_picker.is_some() {
        let typing = app
            .category_picker
            .as_ref()
            .is_some_and(|p| p.new_name.is_some());
        if typing {
            match key.code {
                KeyCode::Enter => app.confirm_category_pick(),
                KeyCode::Esc => app.cancel_category_pick(),
                KeyCode::Backspace => app.category_name_backspace(),
                KeyCode::Char(c) => app.category_name_push(c),
                _ => {}
            }
        } else {
            match normalize_shortcut(key.code) {
                KeyCode::Up => app.move_category_pick(-1),
                KeyCode::Down => app.move_category_pick(1),
                KeyCode::Enter => app.confirm_category_pick(),
                KeyCode::Esc | KeyCode::Char('q') => app.cancel_category_pick(),
                _ => {}
            }
        }
        return;
    }

    // Cross-agent handoff picker.
    if let Some(choice) = app.handoff_picker {
        let targets = app.handoff_targets();
        let last = targets.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => app.handoff_picker = Some(choice.saturating_sub(1)),
            KeyCode::Down => app.handoff_picker = Some((choice + 1).min(last)),
            KeyCode::Enter => {
                if let Some(target) = targets.get(choice).cloned() {
                    app.confirm_handoff_on(&target);
                }
            }
            KeyCode::Esc => app.cancel_handoff(),
            _ => {}
        }
        return;
    }

    // Step 1 (modal): pick a provider and account.
    if let Some(choice) = app.new_picker {
        // One row per login, so the list is as long as the accounts make it.
        let choices = app.new_session_choices();
        let last = choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => app.new_picker = Some(choice.saturating_sub(1)),
            KeyCode::Down => app.new_picker = Some((choice + 1).min(last)),
            KeyCode::Enter => {
                if let Some(account) = choices.get(choice).cloned() {
                    app.choose_new_account(&account);
                }
            }
            // Another account for this one session: it signs in to a home of its own, and running panes keep theirs.
            KeyCode::Char('l') => {
                if let Some(account) = choices.get(choice) {
                    if account.provider == Agent::Omp {
                        app.status = "OMP manages its own login and model settings".to_string();
                    } else {
                        app.choose_new_fresh_login(account.provider);
                    }
                }
            }
            KeyCode::Esc => app.cancel_new_session(),
            _ => {}
        }
        return;
    }

    // Step 2 (modal): type an optional label, then Enter. Shared by new-session
    // creation and editing an existing session's label (label_target tells them
    // apart).
    if app.new_label.is_some() {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.label_input_push(c)
            }
            KeyCode::Backspace => app.label_input_backspace(),
            KeyCode::Enter => {
                if app.label_target.is_some() {
                    app.confirm_label_edit();
                } else {
                    app.confirm_new_session();
                }
            }
            KeyCode::Esc => app.cancel_new_session(),
            _ => {}
        }
        return;
    }

    // Working-dir modal: type a path, Enter to re-point the scope, Esc cancels.
    if app.dir_input.is_some() {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.dir_input_push(c)
            }
            KeyCode::Backspace => app.dir_input_backspace(),
            KeyCode::Enter => app.confirm_dir_input(),
            KeyCode::Esc => app.cancel_dir_input(),
            _ => {}
        }
        return;
    }

    // Link picker: opened by Ctrl-Y from a live pane when the answer it found
    // held two or more links (one is copied outright, with no popup). Checked
    // before `match app.focus` so its keys don't fall through to the pty — `a`
    // in particular would otherwise be typed into the agent.
    if app.link_picker.is_some() {
        match key.code {
            KeyCode::Up => app.move_link_pick(-1),
            KeyCode::Down => app.move_link_pick(1),
            KeyCode::Enter => app.confirm_link_pick(),
            KeyCode::Char('a') => app.copy_all_links(),
            KeyCode::Esc => app.cancel_link_pick(),
            _ => {}
        }
        return;
    }

    // HTML candidate picker: opened by Ctrl-P from a live pane when the passive
    // poll has detected `.html` files. Checked before the free-text popup (and
    // before `match app.focus`) so its keys don't fall through to the pty.
    // Up/Down move the selection (clamped, mirroring new_picker/handoff_picker),
    // Enter opens the selected file in the browser, Tab is the escape hatch to
    // the free-text path popup, Esc cancels.
    if let Some(choice) = app.html_preview_picker {
        let last = app.html_candidates_for_focused().len().saturating_sub(1);
        match key.code {
            KeyCode::Up => app.html_preview_picker = Some(choice.saturating_sub(1)),
            KeyCode::Down => app.html_preview_picker = Some((choice + 1).min(last)),
            KeyCode::Enter => app.confirm_html_preview_pick(),
            KeyCode::Tab => app.html_preview_picker_to_input(),
            KeyCode::Esc => app.cancel_html_preview_picker(),
            _ => {}
        }
        return;
    }

    // HTML path input: opened by Ctrl-P from a live pane. Checked here, before
    // `match app.focus`, so its keystrokes don't fall through to the raw
    // pty-forwarding path. Enter validates + opens the file in the browser (or
    // re-shows the inline error); Esc cancels.
    if app.html_preview_input.is_some() {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.html_preview_input_push(c)
            }
            KeyCode::Backspace => app.html_preview_input_backspace(),
            KeyCode::Enter => app.confirm_html_preview(),
            KeyCode::Esc => app.cancel_html_preview(),
            _ => {}
        }
        return;
    }

    // Transition-report input: opened by Ctrl-T from a live pane (see the
    // Focus::Terminal chord handling below) — checked here, before that
    // match, so its keystrokes never fall through to the raw pty-forwarding
    // path.
    if app.transition_report_input.is_some() {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.transition_report_input_push(c)
            }
            KeyCode::Backspace => app.transition_report_input_backspace(),
            KeyCode::Enter => app.confirm_transition_report_input(),
            KeyCode::Esc => app.cancel_transition_report(),
            _ => {}
        }
        return;
    }

    // Transition-report review: the assembled prompt, read-only until `e`
    // switches it into the same multi-line editor as the input step above.
    // Enter sends in both modes — only whether plain typing edits the buffer
    // or falls through differs.
    if app.transition_report_review.is_some() {
        if app.transition_report_review_editing {
            match key.code {
                KeyCode::Backspace => app.transition_report_review_backspace(),
                KeyCode::Delete => app.transition_report_review_delete(),
                KeyCode::Left => app.transition_report_review_move_left(),
                KeyCode::Right => app.transition_report_review_move_right(),
                KeyCode::Up => app.transition_report_review_move_up(),
                KeyCode::Down => app.transition_report_review_move_down(),
                KeyCode::Home => app.transition_report_review_move_home(),
                KeyCode::End => app.transition_report_review_move_end(),
                KeyCode::Enter if text_newline_key(key) => {
                    app.transition_report_review_push_text("\n")
                }
                KeyCode::Enter => app.send_transition_report_review(),
                KeyCode::Esc => app.cancel_transition_report_review(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.transition_report_review_push_char(c)
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Enter => app.send_transition_report_review(),
                KeyCode::Char('e') => app.begin_editing_transition_report_review(),
                KeyCode::Esc => app.cancel_transition_report_review(),
                _ => {}
            }
        }
        return;
    }

    // Catch-up confirm: only reached for a Working/Blocked target (Idle sends
    // at once in begin_catchup) — asks before queuing in behind its turn.
    if app.catchup_confirm.is_some() {
        match key.code {
            KeyCode::Enter => app.confirm_catchup(),
            KeyCode::Esc => app.cancel_catchup(),
            _ => {}
        }
        return;
    }

    if app.search_query.is_some() {
        match key.code {
            KeyCode::Backspace => app.search_backspace(),
            KeyCode::Enter => app.confirm_search(),
            KeyCode::Esc => app.cancel_search(),
            KeyCode::Up => app.move_selection(-1),
            KeyCode::Down => app.move_selection(1),
            KeyCode::PageUp => app.move_page(-1),
            KeyCode::PageDown => app.move_page(1),
            // Marking still wins over typing while both modes are on —
            // otherwise Space/v would silently go into the filter text
            // instead of marking/leaving multi-select, with no way back
            // except abandoning the search.
            KeyCode::Char(' ') if app.multi_select => app.toggle_mark(),
            KeyCode::Char('v') if app.multi_select => app.toggle_multi_select(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.search_push(c)
            }
            _ => {}
        }
        return;
    }

    match app.focus {
        Focus::Terminal => {
            // Trace owns the whole viewport and is deliberately read-only.
            // Consume navigation before any key can reach the child PTY.
            if app.observer_open() {
                let ctrl_g = key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
                    && matches!(key.code, KeyCode::Char('g') | KeyCode::Char('ㅎ'));
                if ctrl_g || matches!(key.code, KeyCode::Esc) {
                    app.toggle_observer();
                    return;
                }
                match key.code {
                    KeyCode::Up => {
                        app.move_observer_turn(-1);
                    }
                    KeyCode::Down => {
                        app.move_observer_turn(1);
                    }
                    KeyCode::PageUp => {
                        app.scroll_observer(-8);
                    }
                    KeyCode::PageDown => {
                        app.scroll_observer(8);
                    }
                    _ => {}
                }
                return;
            }
            let omp_pane = app
                .focused_pane()
                .and_then(|sid| ui::pane_provider(app, sid))
                == Some(Agent::Omp);
            if omp_pane && key.modifiers == KeyModifiers::ALT {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('ㅂ') => {
                        app.close_focused_pane();
                        return;
                    }
                    KeyCode::Char('w') | KeyCode::Char('ㅈ') => {
                        app.cycle_focus();
                        return;
                    }
                    _ => {}
                }
            }
            // App-level pane/window chords are intercepted before forwarding
            // remaining control keys to the focused child PTY.
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                match key.code {
                    KeyCode::Char('g') if !omp_pane => {
                        app.toggle_observer();
                        return;
                    }
                    KeyCode::Char('x') | KeyCode::Char('ㅌ') => {
                        app.detach_terminal();
                        return;
                    }
                    KeyCode::Char('w') | KeyCode::Char('ㅈ') if !omp_pane => {
                        app.cycle_focus();
                        return;
                    }
                    KeyCode::Char('q') | KeyCode::Char('ㅂ') => {
                        app.close_focused_pane();
                        return;
                    }
                    KeyCode::Char('z') | KeyCode::Char('ㅋ') => {
                        app.toggle_zoom();
                        return;
                    }
                    KeyCode::Char('y') | KeyCode::Char('ㅛ') if !omp_pane => {
                        app.copy_links_from_focused();
                        return;
                    }
                    KeyCode::Char('t') | KeyCode::Char('ㅅ') if !omp_pane => {
                        app.begin_transition_report();
                        return;
                    }
                    KeyCode::Char('p') | KeyCode::Char('ㅔ') if !omp_pane => {
                        app.toggle_html_preview();
                        return;
                    }
                    _ => {}
                }
            }
            // OMP uses Tab for completion and Shift-Tab for thinking level;
            // its multi-pane focus shortcut is Alt-W instead.
            if !omp_pane
                && app.panes.len() >= 2
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                match key.code {
                    KeyCode::Tab => {
                        app.cycle_focus();
                        return;
                    }
                    KeyCode::BackTab => {
                        app.cycle_focus_back();
                        return;
                    }
                    _ => {}
                }
            }
            if let Some(bytes) = encode_key(key) {
                app.send_to_pty(&bytes);
            }
        }
        Focus::List => {
            // Ignore control-modified letters here so stray Ctrl-x (the
            // terminal detach key) can never trigger a destructive archive —
            // EXCEPT ctrl-x itself, which toggles back into the live view the
            // user just detached from (the pane set is still alive).
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
                    && matches!(key.code, KeyCode::Char('x') | KeyCode::Char('ㅌ'))
                {
                    app.resume_live_view();
                }
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
                    && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('ㄱ'))
                {
                    app.refresh_limits_now();
                }
                return;
            }
            // Normalize Korean 2-beolsik jamo to the QWERTY letter so the list
            // shortcuts work regardless of the active input source.
            match normalize_shortcut(key.code) {
                KeyCode::Up => app.move_selection(-1),
                KeyCode::Down => app.move_selection(1),
                KeyCode::PageUp => app.move_page(-1),
                KeyCode::PageDown => app.move_page(1),
                KeyCode::Enter if app.multi_select => app.launch_marked(),
                // Opening is `enter`'s job; the arrows only move around the
                // tree. On a header `enter` toggles it open/shut, since there is
                // no session on that row to open.
                KeyCode::Enter if app.selected_category().is_some() => {
                    app.toggle_selected_category();
                }
                KeyCode::Enter => app.request_resume(),
                // `→`/`l` descend only: unfold a folded category, else step onto
                // its first session. Deliberately does NOT open a session — a key
                // that both walks the tree and launches things is why pressing it
                // on a category used to end up inside a session.
                KeyCode::Char('l') | KeyCode::Right => {
                    app.enter_selected_category();
                }
                // `←` folds the category the cursor is in and steps out to its
                // header, the mirror of `→`.
                KeyCode::Left => {
                    app.collapse_category_at_cursor();
                }
                KeyCode::Char('v') => app.toggle_multi_select(),
                KeyCode::Char(' ') if app.multi_select => app.toggle_mark(),
                KeyCode::Esc if app.multi_select => app.cancel_multi_select(),
                // These four act on "the one row under the cursor," which is
                // meaningless (and, for 'x', destructive) while several rows
                // are marked — block them instead of silently acting on
                // whichever row happens to be highlighted.
                // `t` is deliberately NOT in this block: assigning a category to
                // every marked row at once is the whole point of multi-select
                // here (that is how a topic gets several sessions in one go).
                KeyCode::Char('n' | 'e' | 'h' | 'x' | 'i' | 'c') if app.multi_select => {
                    app.status =
                        "multi-select: finish (enter) or cancel (esc) before this".to_string();
                }
                // On a header `t` configures the category; on a session row it
                // assigns one. Same key, resolved by what the cursor is on.
                KeyCode::Char('t') if app.selected_category().is_some() => {
                    app.open_category_menu();
                }
                KeyCode::Char('t') => app.begin_category_pick(),
                KeyCode::Char('n') => app.new_picker = Some(0),
                // Not a capital: `normalize_shortcut` folds those to lowercase
                // on purpose, so `A` would arrive here as `a` and toggle the
                // archived view instead.
                KeyCode::Char('u') => app.open_accounts(),
                KeyCode::Char('i') => app.toggle_in_progress(),
                KeyCode::Char('c') => app.begin_catchup(),
                code if is_help_key(code, key.modifiers) => app.toggle_help(),
                KeyCode::Char('/') => app.begin_search(),
                KeyCode::Char('d') => app.begin_dir_input(),
                KeyCode::Char('e') => app.begin_label_edit(),
                KeyCode::Char('h') => app.begin_handoff(),
                KeyCode::Char('x') => app.close_selected(),
                KeyCode::Char('a') => app.toggle_archived_view(),
                KeyCode::Char('q') => app.quit(),
                _ => {}
            }
        }
    }
}

fn is_help_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('?'))
        || (matches!(code, KeyCode::Char('/')) && modifiers.contains(KeyModifiers::SHIFT))
}

fn text_newline_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Enter)
        && key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
}

/// Map a Korean 2-beolsik jamo back to the QWERTY letter on the same physical
/// key, so single-letter shortcuts fire even when the IME is in Korean mode.
/// Non-Korean keys pass through unchanged. Only used for command shortcuts,
/// never for text entry.
fn normalize_shortcut(code: KeyCode) -> KeyCode {
    let KeyCode::Char(c) = code else {
        return code;
    };
    let mapped = match c {
        'ㅂ' => 'q',
        'ㅈ' => 'w',
        'ㄷ' => 'e',
        'ㄱ' => 'r',
        'ㅅ' => 't',
        'ㅛ' => 'y',
        'ㅕ' => 'u',
        'ㅑ' => 'i',
        'ㅐ' => 'o',
        'ㅔ' => 'p',
        'ㅁ' => 'a',
        'ㄴ' => 's',
        'ㅇ' => 'd',
        'ㄹ' => 'f',
        'ㅎ' => 'g',
        'ㅗ' => 'h',
        'ㅓ' => 'j',
        'ㅏ' => 'k',
        'ㅣ' => 'l',
        'ㅋ' => 'z',
        'ㅌ' => 'x',
        'ㅊ' => 'c',
        'ㅍ' => 'v',
        'ㅠ' => 'b',
        'ㅜ' => 'n',
        'ㅡ' => 'm',
        // Caps Lock (or Shift) delivers an uppercase ASCII letter with no
        // modifier in legacy input mode; fold it to lowercase so single-letter
        // commands still fire. Only affects A-Z; other chars pass through.
        other => other.to_ascii_lowercase(),
    };
    KeyCode::Char(mapped)
}

/// Encode a key event into the byte sequence a terminal application expects.
fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            // Alt sends the key prefixed with ESC (meta), matching xterm.
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                // Control characters: Ctrl-A=0x01 .. Ctrl-Z=0x1a, etc.
                let upper = c.to_ascii_uppercase();
                if upper.is_ascii() && (0x40..=0x5f).contains(&(upper as u8)) {
                    out.push((upper as u8) & 0x1f);
                } else {
                    let mut b = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                }
            } else {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            out
        }
        KeyCode::F(n) => {
            // xterm function-key sequences (F1-F4 SS3, F5-F12 CSI).
            match n {
                1 => vec![0x1b, b'O', b'P'],
                2 => vec![0x1b, b'O', b'Q'],
                3 => vec![0x1b, b'O', b'R'],
                4 => vec![0x1b, b'O', b'S'],
                5 => vec![0x1b, b'[', b'1', b'5', b'~'],
                6 => vec![0x1b, b'[', b'1', b'7', b'~'],
                7 => vec![0x1b, b'[', b'1', b'8', b'~'],
                8 => vec![0x1b, b'[', b'1', b'9', b'~'],
                9 => vec![0x1b, b'[', b'2', b'0', b'~'],
                10 => vec![0x1b, b'[', b'2', b'1', b'~'],
                11 => vec![0x1b, b'[', b'2', b'3', b'~'],
                12 => vec![0x1b, b'[', b'2', b'4', b'~'],
                _ => return None,
            }
        }
        KeyCode::Enter => {
            // Shift/Alt+Enter inserts a newline without submitting (verified:
            // codex/claude treat a bare LF as a soft newline, CR as submit).
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
            {
                vec![b'\n']
            } else {
                vec![b'\r']
            }
        }
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => vec![0x1b, b'[', b'A'],
        KeyCode::Down => vec![0x1b, b'[', b'B'],
        KeyCode::Right => vec![0x1b, b'[', b'C'],
        KeyCode::Left => vec![0x1b, b'[', b'D'],
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mindplayer_core::{Session, TokenUsage};
    use std::path::PathBuf;

    fn main_app() -> App {
        let mut app = App::new_in(std::env::temp_dir());
        app.screen = Screen::Main;
        app.focus = Focus::List;
        app.startup_routing =
            startup_routing::StartupRouting::confirmed(startup_routing::RoutingMode::Native);
        app
    }

    fn main_app_with_session(id: &str) -> App {
        let mut app = main_app();
        app.all_sessions = vec![Session {
            id: id.to_string(),
            agent: Agent::Codex,
            cwd: std::env::temp_dir(),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: TokenUsage::default(),
            title: id.to_string(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }];
        app.visible = vec![app::Row::Session(0)];
        app
    }

    #[test]
    fn omp_picker_does_not_offer_a_managed_fresh_login() {
        let mut app = main_app();
        let choice = app
            .new_session_choices()
            .iter()
            .position(|account| account.provider == Agent::Omp)
            .expect("OMP is available as a new session provider");
        app.new_picker = Some(choice);
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        assert_eq!(app.new_picker, Some(choice));
        assert!(!app.new_fresh_login);
        assert!(app.new_label.is_none());
    }

    #[test]
    fn default_app_does_not_gate_list_actions_on_startup_consent() {
        let mut app = App::new_in(std::env::temp_dir());
        app.screen = Screen::Main;
        app.focus = Focus::List;
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert_eq!(app.new_picker, Some(0));
        assert_eq!(
            app.startup_routing.mode(),
            Some(startup_routing::RoutingMode::PerPrompt)
        );
        assert!(!app.startup_routing.active());
        assert!(app.pending.is_none());
        assert!(app.ptys.is_empty());
    }

    #[test]
    fn codex_choice_keys_keep_the_native_child_encoding() {
        for (code, expected) in [
            (KeyCode::F(6), b"\x1b[17~".as_slice()),
            (KeyCode::F(7), b"\x1b[18~".as_slice()),
        ] {
            assert_eq!(
                encode_key(KeyEvent::new(code, KeyModifiers::NONE)).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).as_deref(),
            Some(b"\r".as_slice())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)).as_deref(),
            Some(b"\n".as_slice())
        );
    }

    #[test]
    fn f6_and_f7_reach_only_the_focused_codex_or_claude_child_unchanged() {
        fn captured_hex(text: &str) -> Vec<&str> {
            text.split_once("KEYS_READY")
                .map(|(_, hex)| hex.split_whitespace().collect())
                .unwrap_or_default()
        }

        let mut app = main_app();
        app.startup_routing = startup_routing::StartupRouting::per_prompt();
        app.focus = Focus::Terminal;
        for agent in [Agent::Codex, Agent::Claude] {
            let id = format!("choice-keys-{}-{}", agent.as_str(), std::process::id());
            let command = mindplayer_core::Command {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    "stty -echo -icanon min 1 time 0; printf KEYS_READY; od -An -tx1 -N 10".into(),
                ],
                cwd: std::env::temp_dir(),
                env: Default::default(),
            };
            let child = pty::PtySession::spawn_with_routing(
                &command,
                &id,
                10,
                100,
                startup_routing::RoutingMode::Native,
            )
            .unwrap();
            app.ptys.insert(id.clone(), child);
            app.pane_accounts
                .insert(id.clone(), (agent, "native-key-test".into()));
            app.panes.push(id);
        }
        for focused in [0, 1] {
            app.focused = focused;
            app.active = Some(app.panes[focused].clone());
            let id = app.panes[focused].clone();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !app.ptys[&id]
                .parser()
                .lock()
                .unwrap()
                .screen()
                .contents()
                .contains("KEYS_READY")
            {
                assert!(Instant::now() < deadline, "child did not become ready");
                std::thread::sleep(Duration::from_millis(10));
            }
            for code in [KeyCode::F(6), KeyCode::F(7)] {
                handle_main_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let text = app.ptys[&id].parser().lock().unwrap().screen().contents();
                if captured_hex(&text)
                    == ["1b", "5b", "31", "37", "7e", "1b", "5b", "31", "38", "7e"]
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "focused child did not receive unchanged keys: {text}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            if focused == 0 {
                let other = app.ptys[&app.panes[1]]
                    .parser()
                    .lock()
                    .unwrap()
                    .screen()
                    .contents();
                assert!(
                    captured_hex(&other).is_empty(),
                    "keys leaked to the unfocused Claude child"
                );
            }
            assert_eq!(app.focus, Focus::Terminal);
            assert_eq!(app.focused, focused);
            assert!(app.pending.is_none());
            assert!(!app.observer_open());
        }
        app.ptys.clear(); // PtySession::drop terminates and reaps each owned child.
    }

    #[test]
    fn omp_keys_reach_the_native_pane_in_a_split_view() {
        let mut app = main_app();
        app.focus = Focus::Terminal;
        let id = format!("omp-keys-{}", std::process::id());
        let command = mindplayer_core::Command {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "stty raw -echo; printf KEYS_READY; od -An -tx1 -N 9".into(),
            ],
            cwd: std::env::temp_dir(),
            env: Default::default(),
        };
        let child = pty::PtySession::spawn_with_routing(
            &command,
            &id,
            10,
            100,
            startup_routing::RoutingMode::Native,
        )
        .unwrap();
        app.ptys.insert(id.clone(), child);
        app.pane_accounts
            .insert(id.clone(), (Agent::Omp, "native-key-test".into()));
        app.panes = vec![id.clone(), "other-pane".into()];
        app.focused = 0;
        app.active = Some(id.clone());

        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.ptys[&id]
            .parser()
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("KEYS_READY")
        {
            assert!(Instant::now() < deadline, "OMP child did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        for key in ['g', 'p', 't', 'w', 'y'] {
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL),
            );
        }
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = app.ptys[&id].parser().lock().unwrap().screen().contents();
            let received: Vec<_> = text
                .split_once("KEYS_READY")
                .map(|(_, hex)| hex.split_whitespace().collect())
                .unwrap_or_default();
            if received == ["07", "10", "14", "17", "19", "09", "1b", "5b", "5a"] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "OMP keys did not reach the child: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.focused, 0, "Tab and Shift-Tab belong to OMP");
        assert!(!app.observer_open());
        assert!(app.transition_report_input.is_none());
        assert!(app.html_preview_input.is_none());
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
        );
        assert_eq!(app.focused, 1, "Alt-W changes panes");
        app.focused = 0;
        app.active = Some(id.clone());
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
        );
        assert!(!app.panes.contains(&id), "Ctrl-Q closes the OMP pane");
        app.ptys.clear();
    }

    #[test]
    fn session_jev_help_describes_the_key_gated_default_without_f7() {
        assert!(HELP.contains("Codex"));
        assert!(HELP.contains("F6"));
        assert!(HELP.contains("starts with Jev ON when a Jev key is"));
        assert!(HELP.contains("OFF without one"));
        assert!(HELP.contains("same session"));
        assert!(HELP.contains("subsequent prompts"));
        assert!(!HELP.contains("F7"));
        assert!(!HELP.contains("fresh input-row choice"));
        assert!(!HELP.contains("sessions inherit the choice"));
        assert!(!HELP.contains("startup configuration sheet"));
    }

    #[test]
    fn startup_sheet_owns_list_keys_until_one_run_local_choice() {
        let mut app = main_app_with_session("startup-readonly");
        app.startup_routing = startup_routing::StartupRouting::default();
        for code in [
            KeyCode::Char('n'),
            KeyCode::Char('u'),
            KeyCode::Char('h'),
            KeyCode::Char(' '),
            KeyCode::Char('/'),
            KeyCode::Char('v'),
        ] {
            handle_main_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
        }
        assert!(app.startup_routing.active());
        assert!(app.pending.is_none());
        assert!(app.accounts_panel.is_none());
        assert!(app.ptys.is_empty());
        assert!(app.search_query.is_none());
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.startup_routing.selection,
            startup_routing::RoutingMode::Jev
        );
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.startup_routing.mode(),
            Some(startup_routing::RoutingMode::Native)
        );
        assert!(
            app.pending.is_none(),
            "choice must not also open selected session"
        );
        assert!(app.ptys.is_empty());
    }

    #[test]
    fn startup_mouse_cannot_open_or_focus_a_session() {
        let mut app = main_app_with_session("startup-mouse");
        app.startup_routing = startup_routing::StartupRouting::default();
        assert!(!handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 4,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
        ));
        assert!(app.pending.is_none());
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn startup_confirmation_requires_unmodified_enter_and_c_has_no_consent_action() {
        let mut app = main_app();
        app.startup_routing = startup_routing::StartupRouting::default();
        app.startup_routing.selection = startup_routing::RoutingMode::Jev;
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
        );
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        );
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(app.startup_routing.active());
        assert_eq!(
            app.startup_routing.stage,
            startup_routing::StartupStage::Choice
        );
        assert_eq!(app.startup_routing.mode(), None);
        assert!(app.pending.is_none());
        handle_main_key(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('C'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert!(app.should_quit);
        assert!(app.startup_routing.active());
        assert!(app.pending.is_none());
    }

    #[test]
    fn ctrl_r_refreshes_account_usage_from_the_list() {
        let mut app = main_app();
        app.accounts.clear();
        app.quota_cache = Some((Vec::new(), chrono::Utc::now()));
        app.limits_retry_at = Some(Instant::now() + Duration::from_secs(60));

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );

        assert_eq!(app.status, "refreshing account usage");
        assert!(app.limits_rx.is_some(), "ctrl-r did not start a refresh");
        assert!(
            !app.limits_force_refresh,
            "one-shot refresh flag was not consumed"
        );
    }

    #[test]
    fn repeated_ctrl_r_reuses_inflight_refresh_and_clears_status() {
        let mut app = main_app();
        let (tx, rx) = std::sync::mpsc::channel();
        app.limits_rx = Some(rx);
        app.limits_started = Some(Instant::now());
        app.refresh_limits_now();
        assert!(!app.limits_force_refresh);
        tx.send(crate::app::UsageReading {
            rows: Vec::new(),
            unusable: Vec::new(),
        })
        .unwrap();
        assert!(app.poll_limits());
        assert_eq!(app.status, "account usage refreshed");
        assert!(app.limits_rx.is_none());
    }

    /// `u` has to reach the Accounts screen, and the screen has to own every
    /// key while it is open — `q` there must close it, not quit MindPlayer.
    ///
    /// A capital cannot be the key: `normalize_shortcut` folds uppercase to
    /// lowercase so Caps Lock does not break the single-letter commands, which
    /// means `A` arrives as `a` and toggles the archived view.
    #[test]
    fn u_opens_the_accounts_screen_which_then_owns_the_keys() {
        let mut app = main_app();
        assert!(app.accounts_panel.is_none());

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
        );
        assert!(
            app.accounts_panel.is_none(),
            "a capital reaches the list folded to lowercase, so it cannot be this key"
        );

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        assert!(app.accounts_panel.is_some(), "u did not open the screen");

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(
            app.accounts_panel.as_ref().and_then(|p| p.new_name.clone()),
            Some(String::new()),
            "a did not start a new account"
        );

        // Typing a name: the letters are text, not shortcuts.
        for c in "work".chars() {
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        assert_eq!(
            app.accounts_panel.as_ref().and_then(|p| p.new_name.clone()),
            Some("work".to_string())
        );
        assert!(!app.should_quit, "a typed q would have quit the app");

        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app
            .accounts_panel
            .as_ref()
            .is_some_and(|p| p.new_name.is_none()));

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );
        assert!(app.accounts_panel.is_none(), "q did not close the screen");
        assert!(!app.should_quit, "q closed the app instead of the screen");
    }

    #[test]
    fn u_then_korean_r_starts_account_rename() {
        let mut app = main_app();
        app.accounts = vec![mindplayer_core::accounts::Account::inherited(Agent::Codex)];

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('ㄱ'), KeyModifiers::NONE),
        );

        assert_eq!(
            app.accounts_panel
                .as_ref()
                .and_then(|p| p.new_name.as_deref()),
            Some("default"),
            "the Accounts screen must normalize the Korean-IME r shortcut"
        );
    }

    /// The list's single-letter shortcuts have to survive a Korean IME, which
    /// sends the jamo on the same physical key rather than the latin letter.
    #[test]
    fn list_shortcuts_accept_korean_ime_keys() {
        let mut app = main_app();
        assert!(!app.show_archived);

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('ㅁ'), KeyModifiers::NONE),
        );
        assert!(app.show_archived, "ㅁ is the a key");

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('ㅁ'), KeyModifiers::NONE),
        );
        assert!(!app.show_archived);
    }

    #[test]
    fn ctrl_t_opens_transition_report_input_from_a_live_pane_and_esc_cancels() {
        let mut app = main_app_with_session("s1");
        app.focus_or_add_pane("s1");
        app.focus = Focus::Terminal;

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.transition_report_input, Some(String::new()));

        // While open, plain typing must fill the input, not fall through to
        // the raw pty-forwarding path Focus::Terminal normally takes.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.transition_report_input.as_deref(), Some("x"));

        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.transition_report_input.is_none());
    }

    #[test]
    fn ctrl_g_toggles_full_screen_trace_and_esc_returns() {
        let mut app = main_app_with_session("s1");
        app.focus_or_add_pane("s1");
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        );
        assert!(app.observers["s1"].enabled);

        // The retired Ctrl-O shortcut does not close Trace.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        );
        assert!(app.observers["s1"].enabled);

        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.observers["s1"].enabled);
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        );
        assert!(app.observers["s1"].enabled);
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        );
        assert!(!app.observers["s1"].enabled);
    }

    #[test]
    fn detaching_live_session_selects_it_before_label_edit() {
        let mut app = main_app_with_session("s1");
        app.state.labels.remove("s2");
        let mut second = app.all_sessions[0].clone();
        second.id = "s2".to_string();
        second.title = "s2".to_string();
        app.all_sessions.push(second);
        // Reproduce a stale cursor on a category header while s2 is live.
        app.visible = vec![
            app::Row::Header(None),
            app::Row::Session(0),
            app::Row::Session(1),
        ];
        app.selected = 0;
        app.focus_or_add_pane("s2");

        app.detach_terminal();
        assert_eq!(app.selected_session().map(|s| s.id.as_str()), Some("s2"));

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );
        assert_eq!(app.label_target.as_deref(), Some("s2"));

        for ch in "renamed session".chars() {
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.state.labels.get("s2").map(String::as_str),
            Some("renamed session")
        );
    }

    #[test]
    fn mouse_wheel_selects_prompts_or_scrolls_execution_by_rendered_region() {
        let mut app = main_app_with_session("s1");
        app.focus_or_add_pane("s1");
        app.toggle_observer();
        app.observers.get_mut("s1").unwrap().lines = vec![
            "user: older prompt".into(),
            "assistant: first answer".into(),
            "user: newer prompt".into(),
            "assistant: second answer".into(),
        ];
        // `(x, y, rows, cols)` from the rendered read-only trace.
        app.observe_bounds = Some((0, 0, 30, 100));
        app.observe_prompt_bounds = Some((0, 3, 26, 32));
        app.observe_detail_bounds = Some((32, 3, 26, 68));

        let over_prompts = |kind| MouseEvent {
            kind,
            column: 10,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        assert!(handle_mouse(
            &mut app,
            over_prompts(MouseEventKind::ScrollDown)
        ));
        assert_eq!(app.observe_turn, 1);
        assert!(handle_mouse(
            &mut app,
            over_prompts(MouseEventKind::ScrollUp)
        ));
        assert_eq!(app.observe_turn, 0);

        let over_execution = |kind| MouseEvent {
            kind,
            column: 60,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        app.observe_scroll = 3;
        assert!(handle_mouse(
            &mut app,
            over_execution(MouseEventKind::ScrollUp)
        ));
        assert_eq!(app.observe_scroll, 0);
        assert!(handle_mouse(
            &mut app,
            over_execution(MouseEventKind::ScrollDown)
        ));
        assert_eq!(app.observe_scroll, 3);

        // A wheel event outside the observer does not alter its history.
        app.observe_scroll = 3;
        assert!(!handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 110,
                row: 8,
                modifiers: KeyModifiers::NONE,
            }
        ));
        assert_eq!(app.observe_scroll, 3);
    }

    #[test]
    fn ctrl_p_opens_html_preview_popup_and_typing_fills_it_then_esc_cancels() {
        let mut app = main_app_with_session("s1");
        app.focus_or_add_pane("s1");
        app.focus = Focus::Terminal;

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            app.html_preview_input,
            Some(String::new()),
            "Ctrl-P opens the preview path popup"
        );

        // While the popup is open plain typing fills the path buffer instead of
        // falling through to the raw pty-forwarding path.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert_eq!(app.html_preview_input.as_deref(), Some("a"));

        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.html_preview_input.is_none());
    }

    #[test]
    fn ctrl_p_opens_candidate_picker_when_candidates_exist_and_nav_tab_esc_behave() {
        let mut app = main_app_with_session("s1");
        app.focus_or_add_pane("s1");
        app.focus = Focus::Terminal;
        // Two detected candidates for the focused pane → Ctrl-P opens the picker,
        // not the blank path popup.
        app.html_candidates.insert(
            "s1".to_string(),
            vec![PathBuf::from("/tmp/a.html"), PathBuf::from("/tmp/b.html")],
        );

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.html_preview_picker, Some(0));
        assert!(
            app.html_preview_input.is_none(),
            "the picker opens instead of the blank path popup"
        );

        // Down moves the selection and clamps at the last row (like new_picker).
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.html_preview_picker, Some(1));
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.html_preview_picker, Some(1), "clamps at the last row");
        // Up moves back.
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.html_preview_picker, Some(0));

        // Tab is the escape hatch to the free-text path popup.
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.html_preview_picker.is_none());
        assert_eq!(
            app.html_preview_input.as_deref(),
            Some(""),
            "Tab falls through to the free-text path popup"
        );

        // Re-open the picker and confirm Esc cancels it without side effects.
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.html_preview_input.is_none());
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.html_preview_picker, Some(0));
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.html_preview_picker.is_none());
        assert!(app.html_preview_input.is_none(), "cancel opens nothing");
    }

    #[test]
    fn search_then_enter_clears_search_query_so_tab_still_cycles_panes() {
        let mut app = main_app_with_session("s1");

        app.begin_search();
        for c in "s1".chars() {
            app.search_push(c);
        }
        assert_eq!(
            app.visible,
            vec![app::Row::Session(0)],
            "search must still match session s1"
        );

        handle_main_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.focus,
            Focus::Terminal,
            "enter must resume into the live pane"
        );
        assert!(
            app.search_query.is_none(),
            "search state must not survive the resume — otherwise every later key \
             (Tab, typed characters) keeps hitting the search-modal branch instead \
             of Focus::Terminal, since that branch is checked ahead of it"
        );

        // Regression for the reported freeze: with search_query still set, Tab
        // never reaches the Focus::Terminal chord handling and is silently
        // dropped even with 2+ live panes open.
        app.focus_or_add_pane("s2");
        assert_eq!(app.panes.len(), 2);
        let before = app.focused;
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_ne!(
            app.focused, before,
            "Tab must cycle pane focus, not be swallowed by a stale search buffer"
        );
    }

    #[test]
    fn transition_report_review_e_edits_plain_enter_sends_esc_cancels() {
        let mut app = main_app_with_session("s1");
        app.focus_or_add_pane("s1");
        app.focus = Focus::Terminal;
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        for c in "topic".chars() {
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.transition_report_review.is_some());
        assert!(!app.transition_report_review_editing);

        // Read-only: 'e' switches to editing instead of typing a literal "e".
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );
        assert!(app.transition_report_review_editing);
        let before = app
            .transition_report_review
            .as_ref()
            .unwrap()
            .instruction
            .clone();

        // Editing: plain characters now edit the buffer.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        assert_ne!(
            app.transition_report_review.as_ref().unwrap().instruction,
            before
        );

        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.transition_report_review.is_none());
    }

    #[test]
    fn list_session_shortcuts_keep_label_and_handoff() {
        let mut app = main_app_with_session("session-1");

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );
        assert_eq!(app.label_target.as_deref(), Some("session-1"));
        assert_eq!(app.new_label.as_deref(), Some(""));

        let mut app = main_app_with_session("session-1");
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert_eq!(app.handoff_picker, Some(0));
    }

    #[test]
    fn mouse_over_neighbor_pane_does_not_exit_to_list() {
        let mut app = main_app();
        app.focus = Focus::Terminal;
        // Focused pane starts mid-screen (e.g. pane 2 of a horizontal split).
        app.pty_x = 40;

        // Moving the mouse left of the focused pane must NOT drop to the list…
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 2,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.focus, Focus::Terminal, "mouse move must not exit");

        // …and neither must a click there.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.focus, Focus::Terminal, "mouse click must not exit");
    }

    #[test]
    fn ctrl_x_from_list_reenters_live_view() {
        let mut app = main_app(); // Main screen, Focus::List

        // No live panes: ctrl-x is a no-op (stays in the list).
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.focus, Focus::List);

        // A detached-but-running live view: ctrl-x toggles back into it.
        app.panes = vec!["sess".to_string()];
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn multi_select_mode_gates_space_marking() {
        let mut app = main_app_with_session("s1");
        app.all_sessions.push(Session {
            id: "s2".to_string(),
            agent: Agent::Codex,
            cwd: std::env::temp_dir(),
            file: PathBuf::new(),
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: TokenUsage::default(),
            title: "s2".to_string(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        });
        app.visible = vec![app::Row::Session(0), app::Row::Session(1)];

        // Outside multi-select, space must NOT mark (no accidental multi-launch).
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert!(
            app.marked.is_empty(),
            "space is a no-op outside multi-select"
        );
        assert!(!app.multi_select);

        // `v` enters multi-select; now space marks.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        assert!(app.multi_select);
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert_eq!(app.marked.len(), 1, "space marks inside multi-select");
        assert_eq!(app.selected, 0, "space must not move the cursor");

        // Esc cancels multi-select and drops the marks.
        handle_main_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.multi_select);
        assert!(app.marked.is_empty());
    }

    #[test]
    fn search_mode_gives_space_and_v_to_multi_select_instead_of_the_query() {
        let mut app = main_app_with_session("s1");
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        assert!(app.multi_select);
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        assert!(app.search_query.is_some(), "search can still open");

        // Space marks the row instead of typing a space into the query.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert_eq!(app.marked.len(), 1);
        assert_eq!(app.search_query.as_deref(), Some(""));

        // 'v' leaves multi-select instead of typing "v" into the query.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        assert!(!app.multi_select);
        assert_eq!(app.search_query.as_deref(), Some(""));

        // Search itself still works for any other character.
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.search_query.as_deref(), Some("x"));
    }

    #[test]
    fn multi_select_blocks_single_session_shortcuts_instead_of_acting_on_the_cursor_row() {
        let mut app = main_app_with_session("s1");
        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
        );
        assert!(app.multi_select);

        for c in ['n', 'e', 'h', 'x', 'i', 'c'] {
            app.status.clear();
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
            assert!(
                !app.status.is_empty(),
                "{c} should explain why it did nothing"
            );
            assert!(app.multi_select, "{c} must not cancel multi-select");
            // Each key's own real action, checked directly rather than by
            // inference — in particular 'x' actually archives on its normal
            // path, so this is the one that matters most to get right.
            match c {
                'n' => assert!(
                    app.new_picker.is_none(),
                    "n must not open the new-session picker"
                ),
                'e' => assert!(app.new_label.is_none(), "e must not open the label editor"),
                'h' => assert!(
                    app.handoff_picker.is_none(),
                    "h must not open the handoff picker"
                ),
                'x' => assert!(!app.state.is_archived("s1"), "x must not archive the row"),
                'i' => assert!(
                    !app.state.is_in_progress("s1"),
                    "i must not toggle the in-progress mark"
                ),
                'c' => assert!(
                    app.catchup_confirm.is_none(),
                    "c must not open the catch-up confirm"
                ),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        // Padding boundaries (RFC 4648).
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // Multibyte UTF-8 round-trips through the byte encoder.
        assert_eq!(base64_encode("안녕".as_bytes()), "7JWI64WV");
    }
}
