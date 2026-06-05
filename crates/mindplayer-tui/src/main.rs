//! MindPlayer TUI entry point: terminal setup, the event loop, and key routing.

mod app;
mod mascot;
mod pty;
mod terminal_view;
mod ui;

use anyhow::Result;
use app::{App, Focus, Screen};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEvent,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use mindplayer_core::Agent;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};
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
    -h, --help       Print this help
    -V, --version    Print version

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
    let res = run(&mut terminal, &mut app);
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
            "-V" | "--version" => {
                println!("mindplayer {}", env!("CARGO_PKG_VERSION"));
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

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Best-effort kitty keyboard protocol so we can distinguish Shift/Alt+Enter
    // (for newline-without-submit). Ignored by terminals that don't support it.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    // Capture the mouse so the wheel scrolls MindPlayer's own scrollback
    // instead of being forwarded to codex/claude.
    let _ = execute!(stdout, EnableMouseCapture);
    // Advertise bracketed paste so the terminal delivers pastes as one
    // Event::Paste (no per-key replay) AND stops showing its own "this paste
    // may be dangerous" protection prompt — we forward the paste safely below.
    let _ = execute!(stdout, EnableBracketedPaste);
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn teardown(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    // Best-effort and unconditional: every mode is undone even if an earlier
    // step errors, so we never leave the terminal half-restored.
    restore_terminal();
    let _ = terminal.show_cursor();
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
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
            // Live re-ordering from the background mtime refresh.
            if app.poll_refresh() {
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
    match me.kind {
        MouseEventKind::ScrollUp => {
            if app.focus == Focus::Terminal {
                app.scroll_active(STEP)
            } else {
                app.move_selection(-1);
                true
            }
        }
        MouseEventKind::ScrollDown => {
            if app.focus == Focus::Terminal {
                app.scroll_active(-STEP)
            } else {
                app.move_selection(1);
                true
            }
        }
        _ => false,
    }
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

    match app.focus {
        Focus::Terminal => {
            // Ctrl-x detaches back to the list; everything else goes to the PTY.
            // Also accept the Korean-layout key in the same physical position
            // (2-beolsik: the `x` key produces ㅌ when the IME is active).
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('x') | KeyCode::Char('ㅌ'))
            {
                app.detach_terminal();
                return;
            }
            if let Some(bytes) = encode_key(key) {
                app.send_to_pty(&bytes);
            }
        }
        Focus::List => {
            // Ignore control-modified letters here so stray Ctrl-x (the
            // terminal detach key) can never trigger a destructive archive.
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return;
            }
            // Normalize Korean 2-beolsik jamo to the QWERTY letter so the list
            // shortcuts work regardless of the active input source.
            match normalize_shortcut(key.code) {
                KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => app.request_resume(),
                KeyCode::Char('n') => app.new_picker = Some(0),
                KeyCode::Char('e') => app.begin_label_edit(),
                KeyCode::Char('x') => app.close_selected(),
                KeyCode::Char('a') => app.toggle_archived_view(),
                KeyCode::Char('g') => app.toggle_subagents(),
                KeyCode::Char('r') => app.rescan(),
                KeyCode::Char('q') => app.quit(),
                _ => {}
            }
        }
    }
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
