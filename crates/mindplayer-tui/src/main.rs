//! MindPlayer TUI entry point: terminal setup, the event loop, and key routing.

mod app;
mod handoff;
mod mascot;
mod pty;
mod render_writer;
mod terminal_view;
mod text_input;
mod ui;

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
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use render_writer::FrameSink;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const HELP: &str = "\
mindplayer — a session manager for Codex / Claude / Kiro

USAGE:
    mindplayer [DIR]

ARGS:
    DIR    Directory whose sessions to manage (the working-dir scope, and where
           new sessions launch). Defaults to the current directory. Pass a path
           to target another project without cd-ing there, e.g.:
               mindplayer ~/code/my-project

OPTIONS:
    -h, --help           Print this help
    -v, -V, --version    Print version

On the first screen choose 'working dir' (this DIR) or 'global' (all sessions).";

fn main() -> Result<()> {
    // Resolve the target directory from args BEFORE touching the terminal, so
    // --help/--version print normally.
    let explicit_dir = explicit_dir_from_args();

    // Restore the terminal even on a panic — otherwise a crash inside the event
    // loop leaves the real terminal in raw + mouse-reporting + alt-screen mode,
    // which breaks click/selection/copy-paste *outside* MindPlayer until the
    // user runs `reset`. The hook runs before the default panic printer so the
    // backtrace lands on a sane screen.
    install_panic_hook();
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
    // Render only when something actually changed (input, PTY output, list
    // re-order, animation). This is the key to smoothness: an idle screen or a
    // live session with no new bytes costs zero redraws instead of 60fps of
    // rebuilding the whole list + vt100 grid.
    let mut needs_draw = true;

    while !app.should_quit {
        if needs_draw {
            // `FrameSink` (see `render_writer`) buffers this in memory and
            // hands it to a background writer thread on flush, so a
            // terminal that's fallen behind under load can never stall the
            // input-polling loop below.
            terminal.draw(|f| ui::render(f, app))?;
            needs_draw = false;
        }

        // After a Main draw the right-pane size is known, so spawn any pending
        // PTY and keep an existing one sized correctly.
        if app.screen == Screen::Main {
            if app.pending.is_some() {
                app.spawn_pending();
                needs_draw = true;
            } else {
                app.sync_pty_size();
                if app.reap_pty() {
                    needs_draw = true;
                }
            }
            // Track which sessions are actively producing output (status badge).
            if app.poll_activity() {
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
            if app.poll_thread_sync() {
                needs_draw = true;
            }
            if last_refresh.elapsed() >= Duration::from_secs(3) {
                app.start_refresh();
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
        if app.pty_dirty() {
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
                        if app.paste_to_pty(&text) {
                            needs_draw = true;
                        }
                    }
                    Event::Resize(_, _) => needs_draw = true,
                    _ => {}
                }
                if !event::poll(Duration::from_millis(0))? {
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
            if app.any_recent_activity() {
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
    if app.screen != Screen::Main {
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
        Screen::ScopeSelect => match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.scope_choice = 0,
            KeyCode::Down | KeyCode::Char('j') => app.scope_choice = 1,
            KeyCode::Enter => app.start_scan(),
            KeyCode::Char('q') | KeyCode::Esc => app.quit(),
            _ => {}
        },
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
    if app.help_visible {
        match key.code {
            KeyCode::Esc => app.close_help(),
            code if is_help_key(code, key.modifiers) => app.close_help(),
            _ => {}
        }
        return;
    }
    if app.usage_popup {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('u') => app.close_usage_popup(),
            _ => {}
        }
        return;
    }

    // Cross-agent handoff picker.
    if let Some(choice) = app.handoff_picker {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.handoff_picker = Some(choice.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => app.handoff_picker = Some((choice + 1).min(2)),
            KeyCode::Enter => {
                let target = handoff::target_for_choice(choice);
                app.confirm_handoff(target);
            }
            KeyCode::Esc => app.cancel_handoff(),
            _ => {}
        }
        return;
    }

    // Step 1 (modal): pick codex/claude.
    if let Some(choice) = app.new_picker {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.new_picker = Some(choice.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => app.new_picker = Some((choice + 1).min(2)),
            KeyCode::Enter => {
                let agent = match choice {
                    0 => Agent::Codex,
                    1 => Agent::Claude,
                    _ => Agent::Kiro,
                };
                app.choose_new_agent(agent);
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

    // HTML-preview candidate picker: opened by Ctrl-P from a live pane when the
    // passive poll has detected `.html` files. Checked before the free-text
    // popup (and before `match app.focus`) so its keys don't fall through to the
    // pty. Up/Down move the selection (clamped, mirroring new_picker/
    // handoff_picker), Enter previews the selected file, Tab is the escape hatch
    // to the free-text path popup, Esc cancels.
    if let Some(choice) = app.html_preview_picker {
        let last = app.html_candidates_for_focused().len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.html_preview_picker = Some(choice.saturating_sub(1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.html_preview_picker = Some((choice + 1).min(last))
            }
            KeyCode::Enter => app.confirm_html_preview_pick(),
            KeyCode::Tab => app.html_preview_picker_to_input(),
            KeyCode::Esc => app.cancel_html_preview_picker(),
            _ => {}
        }
        return;
    }

    // HTML-preview path input: opened by Ctrl-P from a live pane. Checked here,
    // before `match app.focus`, so its keystrokes don't fall through to the raw
    // pty-forwarding path. Enter validates + spawns carbonyl (or re-shows the
    // inline error); Esc cancels.
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
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.transition_report_review_push_text("\n")
                }
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
            // App-level pane/window chords are intercepted before forwarding
            // remaining control keys to the focused child PTY.
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                match key.code {
                    KeyCode::Char('x') | KeyCode::Char('ㅌ') => {
                        app.detach_terminal();
                        return;
                    }
                    KeyCode::Char('w') | KeyCode::Char('ㅈ') => {
                        app.cycle_focus();
                        return;
                    }
                    KeyCode::Char('o') | KeyCode::Char('ㅐ') => {
                        app.cycle_layout();
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
                    KeyCode::Char('t') | KeyCode::Char('ㅅ') => {
                        app.begin_transition_report();
                        return;
                    }
                    KeyCode::Char('p') | KeyCode::Char('ㅔ') => {
                        app.toggle_html_preview();
                        return;
                    }
                    _ => {}
                }
            }
            // Tab cycles pane focus, but only when 2+ live panes are open — with
            // a single pane Tab still falls through to the child PTY so agent /
            // shell autocompletion keeps working. Shift+Tab (BackTab) reverses.
            if app.panes.len() >= 2
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
                return;
            }
            // Normalize Korean 2-beolsik jamo to the QWERTY letter so the list
            // shortcuts work regardless of the active input source.
            match normalize_shortcut(key.code) {
                KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                KeyCode::PageUp => app.move_page(-1),
                KeyCode::PageDown => app.move_page(1),
                KeyCode::Enter if app.multi_select => app.launch_marked(),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => app.request_resume(),
                KeyCode::Char('v') => app.toggle_multi_select(),
                KeyCode::Char(' ') if app.multi_select => app.toggle_mark(),
                KeyCode::Esc if app.multi_select => app.cancel_multi_select(),
                // These four act on "the one row under the cursor," which is
                // meaningless (and, for 'x', destructive) while several rows
                // are marked — block them instead of silently acting on
                // whichever row happens to be highlighted.
                KeyCode::Char('n' | 'e' | 'h' | 'x' | 'i' | 'c') if app.multi_select => {
                    app.status =
                        "multi-select: finish (enter) or cancel (esc) before this".to_string();
                }
                KeyCode::Char('n') => app.new_picker = Some(0),
                KeyCode::Char('i') => app.toggle_in_progress(),
                KeyCode::Char('c') => app.begin_catchup(),
                code if is_help_key(code, key.modifiers) => app.toggle_help(),
                KeyCode::Char('/') => app.begin_search(),
                KeyCode::Char('d') => app.begin_dir_input(),
                KeyCode::Char('e') => app.begin_label_edit(),
                KeyCode::Char('h') => app.begin_handoff(),
                KeyCode::Char('x') => app.close_selected(),
                KeyCode::Char('a') => app.toggle_archived_view(),
                KeyCode::Char('g') => app.toggle_subagents(),
                KeyCode::Char('r') => app.rescan(),
                KeyCode::Char('u') => app.open_usage_popup(),
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
            tokens: TokenUsage::default(),
            title: id.to_string(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }];
        app.visible = vec![0];
        app
    }

    #[test]
    fn u_opens_the_usage_popup_and_esc_enter_or_u_closes_it() {
        for closing_key in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        ] {
            let mut app = main_app();
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
            );
            assert!(app.usage_popup);
            assert!(app.usage_stats.is_some());

            // While open, unrelated keys (e.g. rescan) are swallowed rather
            // than acting on the list underneath the popup.
            handle_main_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            );
            assert!(app.usage_popup, "unrelated key must not close the popup");

            handle_main_key(&mut app, closing_key);
            assert!(!app.usage_popup, "{closing_key:?} should close the popup");
            assert!(app.usage_stats.is_none());
        }
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
        assert!(app.previewing.is_empty(), "cancel spawns nothing");
    }

    #[test]
    fn search_then_enter_clears_search_query_so_tab_still_cycles_panes() {
        let mut app = main_app_with_session("s1");

        app.begin_search();
        for c in "s1".chars() {
            app.search_push(c);
        }
        assert_eq!(app.visible, vec![0], "search must still match session s1");

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
    fn list_shortcuts_accept_korean_ime_keys() {
        let mut app = main_app();
        assert!(!app.show_subagents);

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('ㅎ'), KeyModifiers::NONE),
        );
        assert!(app.show_subagents);

        handle_main_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('ㅎ'), KeyModifiers::NONE),
        );
        assert!(!app.show_subagents);
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
            tokens: TokenUsage::default(),
            title: "s2".to_string(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        });
        app.visible = vec![0, 1];

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
