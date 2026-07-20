//! End-to-end smoke tests that drive the REAL compiled `mindplayer` binary
//! inside a real pseudo-terminal.
//!
//! Unlike the in-crate unit tests (which construct `App` and call its methods
//! directly), these spawn `env!("CARGO_BIN_EXE_mindplayer")` in a PTY via
//! `portable-pty`, feed it real key bytes, and parse the raw bytes it renders
//! back with the same vendored `vt100` emulator the binary uses internally.
//! Assertions are made against the actual rendered screen grid.
//!
//! Two regressions are guarded (both fixed in git history — this test must fail
//! if either is reintroduced):
//!
//! 1. `search_then_typed_input_reaches_the_live_pane` — commit 70acfda: a search
//!    confirmed with Enter must clear `search_query`, or every later key (Tab,
//!    typed characters) keeps hitting the search-modal branch instead of the
//!    live pane and the session appears frozen.
//! 2. `zoom_left_on_does_not_hide_a_multi_launch` — commit e6aa682: launching
//!    several marked sessions must clear a leftover zoom, or only the focused
//!    pane renders full-screen and the other launched sessions are hidden.
//!
//! The synthetic sessions resume through a fake `codex` on `PATH` that prints a
//! readiness banner and then `exec cat`s, so its PTY stays open and the child
//! tty echoes typed input back onto its own screen — which is exactly the
//! observable scenario 1 needs.

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: u16 = 40;
const COLS: u16 = 140;

/// Printed by the fake `codex` when a pane comes up, so a resume can be waited
/// on by content rather than a fixed sleep.
const PANE_READY: &str = "PANE-READY";

/// Distinctive session titles — unlikely to collide with any static UI text, so
/// finding them on screen unambiguously means a session (list row or pane
/// title) rendered them.
const TITLE_A: &str = "MPTESTALFA";
const TITLE_B: &str = "MPTESTBRVO";

/// Distinctive token typed into a live pane in scenario 1. Must not appear in
/// any static UI chrome, so seeing it proves the keystrokes reached the child.
const TYPED_TOKEN: &str = "zqxj";

/// Distinctive token typed into the agent pane, so the open-in-browser
/// scenarios can assert the pane still shows the agent (never a preview) after
/// Ctrl-P fires — the browser opens in a separate window, never in the pane.
/// Only used by those macOS-only scenarios (see their cfg note) — dead code
/// on any other target.
#[cfg(target_os = "macos")]
const AGENT_TOKEN: &str = "agenttok";

// Per-scenario overall bounds. A genuine hang trips these and fails the test
// instead of hanging CI forever.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const STEP_TIMEOUT: Duration = Duration::from_secs(10);

static DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A fresh, unique temp directory rooted at Cargo's per-crate test tmp dir.
fn unique_tmp() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = root.join(format!("mp-smoke-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Write an executable shell script.
fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

/// Seed one minimal-but-valid synthetic Codex transcript that `discovery.rs`
/// recognizes: a first `session_meta` line (id + cwd) and a `response_item`
/// user message that becomes the session title.
fn seed_codex_session(codex_dir: &Path, scope_cwd: &Path, id: &str, title: &str) {
    let cwd = scope_cwd.display();
    let contents = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{cwd}\",\"timestamp\":\"2026-07-13T10:00:00Z\"}}}}\n\
         {{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\"content\":[{{\"text\":\"{title}\"}}]}}}}\n"
    );
    // Path shape mirrors the real store, but discovery walks recursively so only
    // the `.jsonl` extension actually matters.
    let dir = codex_dir.join("2026").join("07").join("13");
    std::fs::create_dir_all(&dir).expect("create codex dir");
    let file = dir.join(format!("rollout-2026-07-13T10-00-00-{id}.jsonl"));
    std::fs::write(file, contents).expect("write codex session");
}

/// A live mindplayer process running in a PTY, with a background reader feeding
/// a shared vt100 parser we can snapshot at any time.
struct Mp {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    _tmp: PathBuf,
    /// Absolute path to a real `.html` fixture the open-in-browser scenarios can
    /// type into the Ctrl-P popup / select from the picker. Only read by the
    /// macOS-only open-in-browser tests (`open_in_browser` itself is
    /// `#[cfg(target_os = "macos")]`), so this is too — otherwise it's dead code
    /// on any other target.
    #[cfg(target_os = "macos")]
    html_fixture: PathBuf,
    /// File the fake `open` on PATH appends its argv to, so a test can assert
    /// which path the browser launch was invoked with. See the cfg note above.
    #[cfg(target_os = "macos")]
    open_marker: PathBuf,
}

impl Mp {
    /// Spawn the compiled binary against a temp home seeded with two synthetic
    /// Codex sessions and a fake `codex` on PATH.
    fn launch() -> Mp {
        let tmp = unique_tmp();

        // Fully isolate every store the binary touches from the developer's real
        // home, via both a temp HOME and the explicit MINDPLAYER_* overrides.
        let home = tmp.join("home");
        let codex_dir = tmp.join("codex"); // MINDPLAYER_CODEX_DIR
        let claude_dir = tmp.join("claude"); // empty
        let kiro_dir = tmp.join("kiro"); // empty
        let state_dir = tmp.join("state");
        let audit_dir = tmp.join("audit");
        let prompts_dir = tmp.join("prompts");
        let scope = tmp.join("scope"); // session cwd + launch dir
        let bindir = tmp.join("bin");
        for d in [
            &home,
            &codex_dir,
            &claude_dir,
            &kiro_dir,
            &state_dir,
            &audit_dir,
            &prompts_dir,
            &scope,
            &bindir,
        ] {
            std::fs::create_dir_all(d).expect("create dir");
        }

        // Fake `codex`: print a readiness banner, then become `cat` so the PTY
        // stays open and the child tty echoes typed input back to its screen.
        // It ignores `resume <id>` args entirely — we only exercise MindPlayer's
        // own key handling, never a real agent.
        write_script(
            &bindir.join("codex"),
            "#!/bin/sh\nprintf 'PANE-READY\\n'\nexec cat\n",
        );

        // Fake `open` (the macOS launcher MindPlayer shells out to): append its
        // argv to a marker file so a test can assert exactly what path it was
        // invoked with, instead of actually opening a real browser. Exits at
        // once — it's a fire-and-forget one-shot, exactly like the real thing.
        // Only meaningful on macOS (see the field-level cfg note above).
        #[cfg(target_os = "macos")]
        let open_marker = tmp.join("open-marker.txt");
        #[cfg(target_os = "macos")]
        write_script(
            &bindir.join("open"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
                open_marker.display()
            ),
        );

        // A real .html file to open (contents don't matter — only its existence
        // must pass `is_file()` validation before the launch). Only meaningful
        // on macOS (see the field-level cfg note above).
        #[cfg(target_os = "macos")]
        let html_fixture = scope.join("preview.html");
        #[cfg(target_os = "macos")]
        std::fs::write(&html_fixture, "<html><body>hi</body></html>\n")
            .expect("write html fixture");

        seed_codex_session(&codex_dir, &scope, "codex-alfa-0001", TITLE_A);
        seed_codex_session(&codex_dir, &scope, "codex-brvo-0002", TITLE_B);

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mindplayer"));
        cmd.cwd(&scope);
        cmd.env("TERM", "xterm-256color");
        cmd.env("HOME", &home);
        cmd.env("MINDPLAYER_CODEX_DIR", &codex_dir);
        cmd.env("MINDPLAYER_CLAUDE_DIR", &claude_dir);
        cmd.env("MINDPLAYER_KIRO_DIR", &kiro_dir);
        cmd.env("MINDPLAYER_STATE", &state_dir);
        cmd.env("MINDPLAYER_AUDIT", &audit_dir);
        cmd.env("MINDPLAYER_PROMPTS_DIR", &prompts_dir);
        // Prepend the fake-bin dir so `codex` resolves to our stub, keeping the
        // rest of PATH so the inner `sh` wrapper still works.
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{}", bindir.display(), path));

        let child = pair.slave.spawn_command(cmd).expect("spawn mindplayer");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));

        {
            let parser = parser.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                        }
                    }
                }
            });
        }

        Mp {
            parser,
            writer,
            child,
            _master: pair.master,
            _tmp: tmp,
            #[cfg(target_os = "macos")]
            html_fixture,
            #[cfg(target_os = "macos")]
            open_marker,
        }
    }

    /// Read the fake-`open` marker file (empty string if it hasn't been written
    /// yet), so a test can assert what path the browser launch received.
    #[cfg(target_os = "macos")]
    fn open_marker_contents(&self) -> String {
        std::fs::read_to_string(&self.open_marker).unwrap_or_default()
    }

    /// Poll until the fake `open` marker contains `needle` or `within` elapses.
    #[cfg(target_os = "macos")]
    fn wait_for_open(&self, needle: &str, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if self.open_marker_contents().contains(needle) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Current rendered screen as plain text.
    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    /// Poll the rendered screen until `pred` holds or `within` elapses.
    fn wait_until<F: Fn(&str) -> bool>(&self, within: Duration, pred: F) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if pred(&self.screen()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for(&self, needle: &str, within: Duration) -> bool {
        self.wait_until(within, |s| s.contains(needle))
    }

    /// Assert a substring shows up, dumping the live screen on failure.
    fn expect(&self, needle: &str, within: Duration, ctx: &str) {
        assert!(
            self.wait_for(needle, within),
            "{ctx}: expected {needle:?} on screen within {within:?}.\n\
             ---- screen ----\n{}\n----------------",
            self.screen()
        );
    }

    /// Advance from the initial scope-select screen into the main list, choosing
    /// the Global scope so cwd-matching can't affect which synthetic sessions
    /// show. Both seeded sessions must be listed before returning.
    fn start_into_main_list(&mut self) {
        self.expect("collect sessions", STARTUP_TIMEOUT, "scope-select screen");
        // `j` selects Global, Enter starts the scan → summary → main list.
        self.send(b"j");
        self.send(b"\r");
        self.expect(TITLE_A, STARTUP_TIMEOUT, "main list (session A)");
        self.expect(TITLE_B, STARTUP_TIMEOUT, "main list (session B)");
    }
}

impl Drop for Mp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Scenario 1 — search-then-freeze regression (commit 70acfda).
///
/// After `/` + a filter matching one session + Enter (resume) + Tab + a typed
/// character, the typed character must reach the live pane (visible via the
/// child tty echo). If `search_query` survived the resume — the bug — every key
/// after Enter would be swallowed by the search-modal branch instead, so the
/// token would never reach the pane and never appear on screen.
#[test]
fn search_then_typed_input_reaches_the_live_pane() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();

    // `/` opens search; the footer switches to the filter hint.
    mp.send(b"/");
    mp.expect("type to filter", STEP_TIMEOUT, "search opened");

    // Filter down to just session A ("alfa" matches MPTESTALFA, not MPTESTBRVO).
    mp.send(b"alfa");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |s| s.contains(TITLE_A)
            && !s.contains(TITLE_B)),
        "search should narrow the list to only session A.\n\
         ---- screen ----\n{}\n----------------",
        mp.screen()
    );

    // Enter resumes the matched session into a live pane.
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "resumed pane came up");

    // The exact reported sequence: Tab, then a printable character. With the fix
    // these reach the live pane; the typed token is echoed back by the child.
    mp.send(b"\t");
    mp.send(TYPED_TOKEN.as_bytes());
    mp.expect(
        TYPED_TOKEN,
        STEP_TIMEOUT,
        "typed input must reach the live pane (search must not swallow keys)",
    );
}

/// Scenario 2 — zoom-hides-multi-launch regression (commit e6aa682).
///
/// With a leftover zoom on (Ctrl-Z against a single open pane), marking two
/// sessions and launching them must show BOTH panes side by side. If the stale
/// zoom weren't cleared — the bug — only the focused pane would render full
/// screen and the other launched session's title would be hidden.
#[test]
fn zoom_left_on_does_not_hide_a_multi_launch() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();

    // Open a single pane (resume the selected session) and confirm it is live.
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "first pane came up");

    // Leftover zoom from earlier in the session.
    mp.send(b"\x1a"); // Ctrl-Z
    mp.expect("zoomed", STEP_TIMEOUT, "zoom toggled on");

    // Back to the list (Ctrl-X), the pane keeps running in the background.
    mp.send(b"\x18"); // Ctrl-X
    mp.expect("multi-select", STEP_TIMEOUT, "back on the list");

    // Enter multi-select, mark both sessions.
    mp.send(b"v");
    mp.expect("space marks", STEP_TIMEOUT, "multi-select on");
    mp.send(b" ");
    mp.expect("1 marked", STEP_TIMEOUT, "first session marked");
    mp.send(b"\x1b[B"); // Down
    mp.send(b" ");
    mp.expect("2 marked", STEP_TIMEOUT, "second session marked");

    // Launch all marked. The fix clears the leftover zoom so the split shows
    // every pane at once.
    //
    // We assert on the per-pane position markers (" 1/2 " and " 2/2 ") rather
    // than the session titles: the titles also appear as *list rows* just before
    // the launch, and — exactly like a real terminal — the vt100 grid keeps a
    // cell until something overwrites it, so a stale "MPTESTALFA" list row could
    // survive under a zoomed single-pane view and give a false pass. The " N/2 "
    // markers are rendered ONLY by a live pane's title, and " 1/2 " in
    // particular is only ever produced by a two-pane split — never by the
    // zoomed single-pane view (which shows just the focused " 2/2 "). Seeing
    // both therefore proves both panes render side by side.
    mp.send(b"\r");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |s| s.contains(" 1/2 ") && s.contains(" 2/2 ")),
        "a multi-launch must show BOTH live panes side by side (stale zoom must \
         be cleared) — expected both \" 1/2 \" and \" 2/2 \" pane markers.\n\
         ---- screen ----\n{}\n----------------",
        mp.screen()
    );
    // And both sessions' titles are present in that split.
    let screen = mp.screen();
    assert!(
        screen.contains(TITLE_A) && screen.contains(TITLE_B),
        "both session titles must show in the split view.\n\
         ---- screen ----\n{screen}\n----------------",
    );
}

/// Scenario 3 — the Ctrl-P candidate picker opens the selected file in the
/// browser (no typing).
///
/// The seeded session's cwd contains a real `.html` fixture. Once the periodic
/// candidate poll runs, the live pane shows a "N new" badge; Ctrl-P then opens
/// the ranked PICKER of detected files (NOT the blank path popup). Selecting the
/// candidate with Enter shells out to `open <resolved-path>` — asserted via the
/// fake `open` marker — the whole point of the feature: open a file the agent
/// just wrote without typing a path. The pane keeps showing the agent the whole
/// time (the browser opens in a separate window, never in the pane).
// `open_in_browser` is `#[cfg(target_os = "macos")]`-only (see modals.rs) — on
// any other OS it returns an error without ever invoking a command, so a fake
// `open` on PATH would never be called and this test would hang/fail waiting
// for a marker that's never written. CI's fmt/clippy/test gate runs on
// ubuntu-latest, so this genuinely never executes there — that's correct for
// a macOS-only feature, not a gap.
#[test]
#[cfg(target_os = "macos")]
fn ctrl_p_picker_opens_selected_file_in_the_browser() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();

    // Resume a session into a live pane, then type a token the agent echoes so
    // we can prove the pane still shows the agent after Ctrl-P.
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");
    mp.send(AGENT_TOKEN.as_bytes());
    mp.expect(AGENT_TOKEN, STEP_TIMEOUT, "agent echoed its token");

    // The candidate poll detects scope/preview.html and the pane grows a badge.
    mp.expect(
        "1 new",
        STEP_TIMEOUT,
        "pane shows the '1 new' detected-html badge",
    );

    // Ctrl-P opens the detected-file PICKER, not the blank path popup.
    mp.send(b"\x10");
    mp.expect(
        "Open a detected",
        STEP_TIMEOUT,
        "Ctrl-P opened the candidate picker",
    );
    assert!(
        !mp.screen().contains("Open HTML in browser"),
        "with candidates present Ctrl-P must open the picker, not the blank path popup.\n\
         ---- screen ----\n{}\n----------------",
        mp.screen()
    );
    // The fixture's filename is listed as a selectable candidate.
    mp.expect(
        "preview.html",
        STEP_TIMEOUT,
        "the detected fixture is listed in the picker",
    );

    // Enter opens the selected candidate: `open` is invoked with the resolved
    // (canonicalized) fixture path.
    mp.send(b"\r");
    let want = mp
        .html_fixture
        .canonicalize()
        .expect("canonicalize fixture");
    let want = want.display().to_string();
    assert!(
        mp.wait_for_open(&want, STEP_TIMEOUT),
        "selecting the candidate must invoke `open` with the resolved path {want:?}.\n\
         ---- open marker ----\n{}\n----------------",
        mp.open_marker_contents()
    );

    // The pane was never disturbed: the agent's own token is still on screen and
    // no preview ever rendered inside the pane.
    assert!(
        mp.screen().contains(AGENT_TOKEN),
        "the agent pane must stay put — opening the browser never touches it.\n\
         ---- screen ----\n{}\n----------------",
        mp.screen()
    );
}

/// Scenario 4 — the free-text path popup opens a typed path in the browser.
///
/// Reaches the free-text popup via the picker's Tab escape hatch (the seeded
/// cwd always holds the fixture, so Ctrl-P opens the picker first), types the
/// fixture's absolute path, and confirms with Enter. `open` is then invoked with
/// the resolved path — asserted via the fake `open` marker. Exercises the same
/// confirm path a no-candidates blank popup would take.
// See the cfg comment on `ctrl_p_picker_opens_selected_file_in_the_browser`
// above — `open_in_browser` is macOS-only, so this is too.
#[test]
#[cfg(target_os = "macos")]
fn ctrl_p_typed_path_opens_in_the_browser() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();

    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");

    // Wait for the detected-html badge so the next Ctrl-P deterministically opens
    // the picker, then Tab into the free-text path popup.
    mp.expect("1 new", STEP_TIMEOUT, "pane shows the detected-html badge");
    mp.send(b"\x10");
    mp.expect(
        "Open a detected",
        STEP_TIMEOUT,
        "Ctrl-P opened the candidate picker",
    );
    mp.send(b"\t"); // Tab: escape hatch to the free-text path popup.
    mp.expect(
        "Open HTML in browser",
        STEP_TIMEOUT,
        "Tab switched to the free-text path popup",
    );

    // Type the fixture path and confirm — `open` fires with the resolved path.
    let path = mp.html_fixture.display().to_string();
    mp.send(path.as_bytes());
    mp.send(b"\r");
    let want = mp
        .html_fixture
        .canonicalize()
        .expect("canonicalize fixture");
    let want = want.display().to_string();
    assert!(
        mp.wait_for_open(&want, STEP_TIMEOUT),
        "confirming a typed path must invoke `open` with the resolved path {want:?}.\n\
         ---- open marker ----\n{}\n----------------",
        mp.open_marker_contents()
    );

    // The popup closes on success and the pane shows the agent again.
    assert!(
        mp.wait_until(STEP_TIMEOUT, |s| !s.contains("Open HTML in browser")),
        "the popup must close after a successful open.\n\
         ---- screen ----\n{}\n----------------",
        mp.screen()
    );
}
