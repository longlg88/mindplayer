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
use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::Rect,
    prelude::{Position, Size},
    text::Line,
    widgets::Paragraph,
    Terminal, TerminalOptions, Viewport,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::stdout;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
const OLDER_TRACE_PROMPT: &str = "older trace prompt";
const FIXTURE_CONFIG_OLD: &str = "model = \"fixture-old\"\n";
const FIXTURE_CONFIG_NEW: &str = "model = \"fixture-new\"\n";
const FIXTURE_AUTH_SENTINEL: &str = "fixture-auth-sentinel\n";
const INLINE_FIXTURE_TEST: &str = "ratatui_inline_painter_fixture";

#[derive(Clone, Copy)]
enum CodexFixture {
    Echo,
    InlinePainter,
    TaskStatus,
    AccountQuota,
}

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
fn seed_codex_session(codex_dir: &Path, scope_cwd: &Path, id: &str, title: &str) -> PathBuf {
    let cwd = scope_cwd.display();
    // Path shape mirrors the real store, but discovery walks recursively so only
    // the `.jsonl` extension actually matters.
    let dir = codex_dir.join("2026").join("07").join("13");
    std::fs::create_dir_all(&dir).expect("create codex dir");
    let file = dir.join(format!("rollout-2026-07-13T10-00-00-{id}.jsonl"));
    let contents = format!(
        "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{cwd}\",\"timestamp\":\"2026-07-13T10:00:00Z\",\"rollout_path\":\"{rollout_path}\"}}}}\n\
         {{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\"content\":[{{\"text\":\"{title}\"}}]}}}}\n"
        , rollout_path = file.display()
    );
    std::fs::write(&file, contents).expect("write codex session");
    file
}

fn append_codex_task_event(path: &Path, event: &str, turn_id: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open codex transcript for task event");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:00Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"{event}\",\"turn_id\":\"{turn_id}\"}}}}"
    )
    .expect("append codex task event");
}

/// Append one synthetic Codex turn plus the native cumulative token-count
/// record observers should pick up from the already-selected session, without a
/// manual rescan.
fn append_codex_observer_event(path: &Path, command_marker: &str, total_tokens: u64) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open codex transcript for append");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:00Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{OLDER_TRACE_PROMPT}\"}}]}}}}"
    )
    .expect("append older user prompt");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:00Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"older answer\"}}]}}}}"
    )
    .expect("append older assistant answer");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"trace this command\"}}]}}}}"
    )
    .expect("append user prompt");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:02Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",\"name\":\"{command_marker}\",\"call_id\":\"call-observe-smoke\",\"arguments\":\"{{}}\"}}}}"
    )
    .expect("append function_call");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:03Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":18000,\"cached_input_tokens\":14000,\"output_tokens\":2000,\"reasoning_output_tokens\":100,\"total_tokens\":20000}},\"last_token_usage\":{{\"input_tokens\":18000,\"cached_input_tokens\":14000,\"output_tokens\":2000,\"reasoning_output_tokens\":100,\"total_tokens\":20000}}}}}}}}"
    )
    .expect("append first token_count");
    // Real Codex logs can repeat a token_count without advancing the cumulative
    // total. This duplicate must not be charged to the prompt twice.
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:04Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":18000,\"cached_input_tokens\":14000,\"output_tokens\":2000,\"reasoning_output_tokens\":100,\"total_tokens\":20000}},\"last_token_usage\":{{\"input_tokens\":5000,\"cached_input_tokens\":4000,\"output_tokens\":500,\"reasoning_output_tokens\":20,\"total_tokens\":5500}}}}}}}}"
    )
    .expect("append duplicate token_count");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:00:05Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":40000,\"cached_input_tokens\":30000,\"output_tokens\":3210,\"reasoning_output_tokens\":150,\"total_tokens\":{total_tokens}}},\"last_token_usage\":{{\"input_tokens\":22000,\"cached_input_tokens\":16000,\"output_tokens\":1210,\"reasoning_output_tokens\":50,\"total_tokens\":23210}}}}}}}}"
    )
    .expect("append final token_count");
}

fn append_handoff_turn(path: &Path) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open codex transcript for handoff append");
    writeln!(
        file,
        "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"handoff smoke request\"}}]}}}}"
    )
    .expect("append handoff user turn");
    writeln!(
        file,
        "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"handoff smoke answer\"}}]}}}}"
    )
    .expect("append handoff assistant turn");
}

fn append_long_codex_execution(path: &Path, final_marker: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open codex transcript for long execution append");
    for index in 0..12 {
        let body = format!("wrapped-result-{index}-{}", "x".repeat(300));
        writeln!(
            file,
            "{{\"timestamp\":\"2026-07-13T10:01:{index:02}Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"call-{index}\",\"output\":\"{body}\"}}}}"
        )
        .expect("append wrapped function result");
    }
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:01:20Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{final_marker}\"}}]}}}}"
    )
    .expect("append final scroll marker");
    writeln!(
        file,
        "{{\"timestamp\":\"2026-07-13T10:01:21Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":40800,\"cached_input_tokens\":30700,\"output_tokens\":3410,\"reasoning_output_tokens\":170,\"total_tokens\":44210}},\"last_token_usage\":{{\"input_tokens\":800,\"cached_input_tokens\":700,\"output_tokens\":200,\"reasoning_output_tokens\":20,\"total_tokens\":1000}}}}}}}}"
    )
    .expect("append updated token_count");
}

fn observer_panel_text(screen: &str) -> String {
    let mut panel = String::new();
    let mut in_panel = false;
    for line in screen.lines() {
        if !in_panel && line.contains("TRACE") {
            in_panel = true;
        }
        if in_panel {
            panel.push_str(line);
            panel.push('\n');
            if line.contains('└') || line.contains('╚') {
                break;
            }
        }
    }
    panel
}

fn contains_observer_total(screen: &str) -> bool {
    let panel = observer_panel_text(screen);
    ["43,210", "43210", "43.2K", "43.2k"]
        .iter()
        .any(|needle| panel.contains(needle))
}

fn contains_observer_native_usage(screen: &str) -> bool {
    let panel = observer_panel_text(screen);
    contains_observer_total(screen)
        && panel.contains("40000")
        && panel.contains("30000")
        && panel.contains("3210")
}

/// A live mindplayer process running in a PTY, with a background reader feeding
/// a shared vt100 parser we can snapshot at any time.
struct Mp {
    parser: Arc<Mutex<vt100::Parser>>,
    raw_output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    _tmp: PathBuf,
    codex_marker_dir: PathBuf,
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
    codex_alfa_file: PathBuf,
    codex_brvo_file: PathBuf,
}

impl Mp {
    /// Spawn the compiled binary against a temp home seeded with two synthetic
    /// Codex sessions and a fake `codex` on PATH.
    fn launch() -> Mp {
        Self::launch_with_options(ROWS, COLS, false, false, false, true)
    }

    /// Spawn with an explicit PTY size so smoke coverage can exercise narrow
    /// observer layout without depending on the developer's terminal.
    fn launch_with_size(rows: u16, cols: u16) -> Mp {
        Self::launch_with_options(rows, cols, false, false, false, false)
    }

    /// Spawn with optional ambient Codex selection and account-role fixtures.
    /// The fixture is written before App starts so startup preference resolution
    /// is exercised through the real binary, not by mutating App internals.
    fn launch_with_options(
        rows: u16,
        cols: u16,
        ambient_isolated: bool,
        isolated_primary: bool,
        with_accounts: bool,
        enable_cursor: bool,
    ) -> Mp {
        Self::launch_with_options_and_fixture(
            rows,
            cols,
            ambient_isolated,
            isolated_primary,
            with_accounts,
            enable_cursor,
            CodexFixture::Echo,
        )
    }

    fn launch_inline_fixture() -> Mp {
        Self::launch_with_options_and_fixture(
            ROWS,
            COLS,
            false,
            false,
            false,
            false,
            CodexFixture::InlinePainter,
        )
    }

    fn launch_task_status_fixture() -> Mp {
        Self::launch_with_options_and_fixture(
            ROWS,
            COLS,
            false,
            false,
            false,
            false,
            CodexFixture::TaskStatus,
        )
    }

    fn launch_account_quota_fixture() -> Mp {
        Self::launch_with_options_and_fixture(
            ROWS,
            COLS,
            false,
            false,
            true,
            false,
            CodexFixture::AccountQuota,
        )
    }

    fn launch_with_options_and_fixture(
        rows: u16,
        cols: u16,
        ambient_isolated: bool,
        isolated_primary: bool,
        with_accounts: bool,
        enable_cursor: bool,
        fixture: CodexFixture,
    ) -> Mp {
        let inline_fixture = matches!(fixture, CodexFixture::InlinePainter);
        let stale_completion = matches!(fixture, CodexFixture::TaskStatus);
        let tmp = unique_tmp();

        // Fully isolate every store the binary touches from the developer's real
        // home, via both a temp HOME and the explicit MINDPLAYER_* overrides.
        let home = tmp.join("home");
        let codex_dir = tmp.join("codex"); // MINDPLAYER_CODEX_DIR
        let claude_dir = tmp.join("claude"); // empty
        let kiro_dir = tmp.join("kiro"); // empty
        let cursor_dir = tmp.join("cursor"); // empty
        let state_dir = tmp.join("state");
        let audit_dir = tmp.join("audit");
        let prompts_dir = tmp.join("prompts");
        let codex_marker_dir = tmp.join("codex-markers");
        let scope = tmp.join("scope"); // session cwd + launch dir
        let bindir = tmp.join("bin");
        for d in [
            &home,
            &codex_dir,
            &claude_dir,
            &kiro_dir,
            &cursor_dir,
            &state_dir,
            &audit_dir,
            &prompts_dir,
            &codex_marker_dir,
            &scope,
            &bindir,
        ] {
            std::fs::create_dir_all(d).expect("create dir");
        }

        let isolated_home = home
            .join(".mindplayer")
            .join("accounts")
            .join("codex")
            .join("isolated");
        {
            let (default_role, isolated_role) = if isolated_primary {
                ("fallback", "primary")
            } else {
                ("primary", "fallback")
            };
            let mut accounts = serde_json::json!([
                {
                    "provider": "codex",
                    "name": "default",
                    "slot": { "kind": "inherited" },
                    "role": default_role,
                    "disabled": false
                },
                {
                    "provider": "claude",
                    "name": "default",
                    "slot": { "kind": "inherited" },
                    "role": "primary",
                    "disabled": true
                },
                {
                    "provider": "kiro",
                    "name": "default",
                    "slot": { "kind": "inherited" },
                    "role": "primary",
                    "disabled": true
                },
                {
                    "provider": "cursor",
                    "name": "default",
                    "slot": { "kind": "inherited" },
                    "role": "primary",
                    "disabled": !enable_cursor
                }
            ]);
            if with_accounts {
                accounts.as_array_mut().unwrap().push(serde_json::json!({
                    "provider": "codex",
                    "name": "isolated",
                    "slot": { "kind": "isolated", "path": isolated_home },
                    "role": isolated_role,
                    "disabled": false
                }));
            }
            let accounts_path = home.join(".mindplayer").join("accounts.json");
            std::fs::create_dir_all(accounts_path.parent().unwrap()).expect("create accounts dir");
            std::fs::write(
                accounts_path,
                serde_json::to_vec_pretty(&accounts).expect("serialize account fixture"),
            )
            .expect("write account fixture");
            if with_accounts {
                let canonical_codex_home = home.join(".codex");
                std::fs::create_dir_all(&canonical_codex_home)
                    .expect("create canonical Codex home");
                std::fs::write(canonical_codex_home.join("config.toml"), FIXTURE_CONFIG_NEW)
                    .expect("write canonical Codex config");
                std::fs::create_dir_all(&isolated_home).expect("create isolated Codex home");
                std::fs::write(isolated_home.join("config.toml"), FIXTURE_CONFIG_OLD)
                    .expect("write isolated Codex config");
                std::fs::write(isolated_home.join("auth.json"), FIXTURE_AUTH_SENTINEL)
                    .expect("write isolated auth sentinel");
            }
        }

        // Fake `codex`: answer non-interactive probes immediately, record only
        // account selection and argv for pane children, then become `cat` so
        // the PTY stays open and the child tty echoes typed input.
        let app_server = if matches!(fixture, CodexFixture::AccountQuota) {
            r#"case "$CODEX_HOME" in
  */.codex) email='one@example.com'; percent=10; cap=0 ;;
  *) email='two@example.com'; percent=65; cap=500 ;;
esac
if [ -f "$CODEX_HOME/usage-percent" ]; then
  IFS= read -r percent < "$CODEX_HOME/usage-percent"
fi
if [ -f "$CODEX_HOME/usage-email" ]; then
  IFS= read -r email < "$CODEX_HOME/usage-email"
fi
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"account/read"'*)
      printf '{"id":3,"result":{"account":{"type":"chatgpt","email":"%s","planType":"business"}}}\n' "$email" ;;
    *'"method":"account/rateLimits/read"'*)
      printf '{"id":2,"result":{"accountId":"workspace-shared","ordinaryUsageAllowed":true,"rateLimits":{"primary":{"usedPercent":%s,"windowDurationMins":10080},"individualLimit":{"limit":"%s","used":"%s","remainingPercent":0},"spendControlReached":true}}}\n' "$percent" "$cap" "$cap" ;;
  esac
done
exit 0"#
        } else {
            "printf 'app-server ready\\n'\nexit 0"
        };
        write_script(
            &bindir.join("codex"),
            &format!(
                "#!/bin/sh\n\nif [ \"$1 $2\" = \"login status\" ]; then\n  printf 'logged in\\n'\n  exit 0\nfi\nif [ \"$1\" = \"app-server\" ]; then\n  {app_server}\nfi\nif [ \"$MINDPLAYER_RATATUI_FIXTURE\" = \"1\" ]; then\n  exec \"$MINDPLAYER_SMOKE_TEST_BINARY\" --exact {} --ignored --nocapture\nfi\nif [ -n \"$MINDPLAYER_PANE_ID\" ]; then\n  marker='{}'\n  printf 'CODEX_HOME=%s\\n' \"$CODEX_HOME\" > \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  for arg in \"$@\"; do printf 'ARG=%s\\n' \"$arg\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\"; done\n  printf 'CONFIG_CONTENT_BEGIN\\n' >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  cat \"$CODEX_HOME/config.toml\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\" 2>/dev/null\n  printf 'CONFIG_CONTENT_END\\n' >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\nfi\nprintf 'PANE-READY\\n'\nexec cat\n",
                INLINE_FIXTURE_TEST,
                codex_marker_dir.display()
            ),
        );
        if enable_cursor {
            write_script(
                &bindir.join("agent"),
                "#!/bin/sh\nif [ \"$1\" = \"status\" ]; then printf 'logged in\\n'; exit 0; fi\nprintf 'PANE-READY\\n'\nexec cat\n",
            );
            write_script(&bindir.join("security"), "#!/bin/sh\nexit 1\n");
        }

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

        let alfa_root = if with_accounts {
            isolated_home.join("sessions")
        } else {
            codex_dir.clone()
        };
        let codex_alfa_file = seed_codex_session(&alfa_root, &scope, "codex-alfa-0001", TITLE_A);
        let codex_brvo_file = seed_codex_session(&codex_dir, &scope, "codex-brvo-0002", TITLE_B);
        if stale_completion {
            append_codex_task_event(&codex_alfa_file, "task_complete", "stale");
        }

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let binary = std::env::var_os("MINDPLAYER_TEST_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mindplayer")));
        let mut cmd = CommandBuilder::new(binary);
        cmd.cwd(&scope);
        cmd.env("TERM", "xterm-256color");
        // Crossterm 0.28.1 suppresses ANSI colors when NO_COLOR is set. The
        // parent test runner may intentionally export it, but this real-PTY
        // fixture must receive the production RGB escape sequences.
        cmd.env_remove("NO_COLOR");
        cmd.env_remove("CLICOLOR");
        cmd.env_remove("CLICOLOR_FORCE");
        cmd.env("HOME", &home);
        cmd.env("MINDPLAYER_CODEX_DIR", &codex_dir);
        cmd.env("MINDPLAYER_CLAUDE_DIR", &claude_dir);
        cmd.env("MINDPLAYER_KIRO_DIR", &kiro_dir);
        cmd.env("MINDPLAYER_CURSOR_DIR", &cursor_dir);
        cmd.env("MINDPLAYER_STATE", &state_dir);
        cmd.env("MINDPLAYER_AUDIT", &audit_dir);
        cmd.env("MINDPLAYER_PROMPTS_DIR", &prompts_dir);
        cmd.env("MINDPLAYER_TEST_MARKER_DIR", &codex_marker_dir);
        cmd.env(
            "MINDPLAYER_SMOKE_TEST_BINARY",
            std::env::current_exe().expect("locate smoke test binary"),
        );
        if inline_fixture {
            cmd.env("MINDPLAYER_RATATUI_FIXTURE", "1");
        }
        if ambient_isolated {
            cmd.env("CODEX_HOME", &isolated_home);
        }
        // Prepend the fake-bin dir so `codex` resolves to our stub, keeping the
        // rest of PATH so the inner `sh` wrapper still works.
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{}", bindir.display(), path));

        let child = pair.slave.spawn_command(cmd).expect("spawn mindplayer");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let raw_output = Arc::new(Mutex::new(Vec::new()));

        {
            let parser = parser.clone();
            let raw_output = Arc::clone(&raw_output);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            raw_output.lock().unwrap().extend_from_slice(&buf[..n]);
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
            raw_output,
            writer,
            child,
            _master: pair.master,
            _tmp: tmp,
            codex_marker_dir,
            #[cfg(target_os = "macos")]
            html_fixture,
            #[cfg(target_os = "macos")]
            open_marker,
            codex_alfa_file,
            codex_brvo_file,
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

    fn raw_output_contains(&self, needle: &str) -> bool {
        String::from_utf8_lossy(&self.raw_output.lock().unwrap()).contains(needle)
    }

    fn has_border_foreground(&self, expected: vt100::Color) -> bool {
        const BORDER_GLYPHS: &[&str] = &[
            "│", "┃", "╎", "╏", "┌", "┐", "└", "┘", "╭", "╮", "╰", "╯", "▗", "▖", "▝", "▘", "▐",
            "▌",
        ];
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let matches = (0..rows)
            .flat_map(|row| (0..cols).filter_map(move |col| screen.cell(row, col)))
            .filter(|cell| {
                cell.fgcolor() == expected
                    && BORDER_GLYPHS.iter().any(|glyph| cell.contents() == *glyph)
            })
            .count();
        matches >= 2
    }

    fn border_cells(&self) -> Vec<(u16, u16, String, vt100::Color)> {
        const BORDER_GLYPHS: &[&str] = &[
            "│", "┃", "╎", "╏", "┌", "┐", "└", "┘", "╭", "╮", "╰", "╯", "▗", "▖", "▝", "▘", "▐",
            "▌",
        ];
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        (0..rows)
            .flat_map(|row| {
                (0..cols).filter_map(move |col| {
                    let cell = screen.cell(row, col)?;
                    BORDER_GLYPHS
                        .iter()
                        .any(|glyph| cell.contents() == *glyph)
                        .then(|| (row, col, cell.contents(), cell.fgcolor()))
                })
            })
            .collect()
    }

    fn expect_foreground(&self, expected: vt100::Color, within: Duration, ctx: &str) {
        assert!(
            self.wait_until(within, |_| self.has_border_foreground(expected)),
            "{ctx}: expected foreground {expected:?} on screen within {within:?}.\n\
             ---- border cells ----\n{:?}\n---- screen ----\n{}\n----------------",
            self.border_cells(),
            self.screen(),
        );
    }

    fn expect_foreground_transition_absent(
        &self,
        expected: vt100::Color,
        within: Duration,
        ctx: &str,
    ) {
        assert!(
            self.wait_until(within, |_| !self.has_border_foreground(expected)),
            "{ctx}: foreground {expected:?} did not clear within {within:?}.\n\
             ---- screen ----\n{}\n----------------",
            self.screen()
        );
    }

    fn expect_foreground_absent(&self, expected: vt100::Color, within: Duration, ctx: &str) {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            assert!(
                !self.has_border_foreground(expected),
                "{ctx}: foreground {expected:?} reappeared during the continuous absence window.\n\
                 ---- screen ----\n{}\n----------------",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn expect_size(&self, rows: u16, cols: u16, within: Duration, ctx: &str) {
        assert!(
            self.wait_until(within, |_| self.parser.lock().unwrap().screen().size()
                == (rows, cols)),
            "{ctx}: expected PTY frame size {rows}x{cols}.\n---- screen ----\n{}\n----------------",
            self.screen()
        );
    }

    fn write_hook_status(&self, event: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        let pane_id = loop {
            if let Ok(entries) = std::fs::read_dir(&self.codex_marker_dir) {
                if let Some(id) = entries
                    .flatten()
                    .filter_map(|entry| entry.path().file_stem().map(|s| s.to_os_string()))
                    .next()
                {
                    break id;
                }
            }
            if Instant::now() >= deadline {
                panic!("pane marker did not appear before writing hook status");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let hooks = self._tmp.join("home/.mindplayer/hooks");
        std::fs::create_dir_all(&hooks).expect("create isolated hook directory");
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_secs();
        std::fs::write(
            hooks.join(pane_id).with_extension("json"),
            format!("{{\"event\":\"{event}\",\"ts\":{ts}}}\n"),
        )
        .expect("write isolated hook status");
    }

    fn task_status_file(&self) -> &Path {
        &self.codex_alfa_file
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    /// Resize the outer PTY exactly like a real terminal window resize. Update
    /// the parser first so bytes emitted during SIGWINCH handling are decoded
    /// against the new grid before the master ioctl can wake the child.
    fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        {
            let mut parser = self.parser.lock().unwrap();
            parser.set_size(rows, cols);
        }
        self._master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize outer PTY");
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

    fn wait_for_codex_home(&self, expected: &Path, within: Duration) {
        let expected = expected.display().to_string();
        let deadline = Instant::now() + within;
        loop {
            if let Ok(entries) = std::fs::read_dir(&self.codex_marker_dir) {
                for entry in entries.flatten() {
                    if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                        if contents
                            .lines()
                            .any(|line| line == format!("CODEX_HOME={expected}").as_str())
                        {
                            return;
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                let markers = std::fs::read_dir(&self.codex_marker_dir)
                    .ok()
                    .into_iter()
                    .flat_map(|entries| entries.flatten())
                    .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                    .collect::<Vec<_>>()
                    .join("\n---\n");
                panic!(
                    "expected Codex child CODEX_HOME={expected:?} within {within:?}; markers:\n{markers}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_codex_marker(&self, expected_home: &Path, expected_config: &str) {
        let expected_home = expected_home.display().to_string();
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Ok(entries) = std::fs::read_dir(&self.codex_marker_dir) {
                for entry in entries.flatten() {
                    if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                        if contents.contains(&format!("CODEX_HOME={expected_home}"))
                            && contents.contains(expected_config)
                        {
                            return;
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                panic!(
                    "expected Codex marker for home {expected_home:?} and config {expected_config:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn canonical_config(&self) -> PathBuf {
        self._tmp.join("home/.codex/config.toml")
    }

    fn isolated_auth(&self) -> PathBuf {
        isolated_codex_home(self).join("auth.json")
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

    /// Assert a substring is absent after the UI has had a chance to redraw.
    fn expect_absent(&self, needle: &str, within: Duration, ctx: &str) {
        assert!(
            self.wait_until(within, |s| !s.contains(needle)),
            "{ctx}: expected {needle:?} to disappear within {within:?}.\n\
             ---- screen ----\n{}\n----------------",
            self.screen()
        );
    }

    /// Advance from the initial scope-select screen into the main list, choosing
    /// the Global scope so cwd-matching can't affect which synthetic sessions
    /// show. Both seeded sessions must be listed before returning.
    fn start_into_main_list(&mut self) {
        self.expect("collect sessions", STARTUP_TIMEOUT, "scope-select screen");
        // Down moves the pick to Global; `▶ ` marks the selected option. Waiting
        // for the marker before pressing enter is what makes this reliable —
        // sending Down and Enter back to back raced, and the scan then ran under
        // Project scope. (`j` did this until vim-style navigation was removed.)
        self.send(b"\x1b[B");
        self.expect("▶ global", STARTUP_TIMEOUT, "Global option selected");
        self.send(b"\r");
        self.expect(TITLE_A, STARTUP_TIMEOUT, "main list (session A)");
        self.expect(TITLE_B, STARTUP_TIMEOUT, "main list (session B)");
        // Prove the scan actually ran under Global rather than Project, so
        // cwd-matching cannot decide which sessions show. The footer renders
        // `· global` vs `· working dir (…)` (see `App::scope_label`); the `· `
        // prefix is what keeps this from matching the scope-select option text,
        // which lingers on stale screen rows and made a bare "global" match pass
        // even while the footer said "working dir".
        self.expect("· global", STARTUP_TIMEOUT, "Global scope in the footer");
    }
}

const IDLE_BORDER: vt100::Color = vt100::Color::Rgb(140, 148, 162);
const WORKING_BORDER: vt100::Color = vt100::Color::Rgb(240, 212, 120);
const COMPLETED_BORDER: vt100::Color = vt100::Color::Rgb(145, 217, 174);
const ENDED_BORDER: vt100::Color = vt100::Color::Rgb(174, 183, 198);

#[test]
fn codex_task_events_drive_correlated_status_colors() {
    let mut mp = Mp::launch_task_status_fixture();
    mp.start_into_main_list();
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");

    // The stale completion was present before startup and must not mark the
    // newly opened live pane done.
    mp.expect_foreground(IDLE_BORDER, STEP_TIMEOUT, "startup status is idle");
    mp.expect_foreground_absent(
        COMPLETED_BORDER,
        Duration::from_millis(250),
        "stale completion is ignored",
    );

    append_codex_task_event(mp.task_status_file(), "task_started", "fresh");
    mp.expect_foreground(WORKING_BORDER, STEP_TIMEOUT, "fresh task starts working");

    append_codex_task_event(mp.task_status_file(), "task_complete", "other");
    mp.expect_foreground_absent(
        COMPLETED_BORDER,
        Duration::from_millis(650),
        "mismatched completion does not finish the fresh turn",
    );
    mp.expect_foreground(
        WORKING_BORDER,
        STEP_TIMEOUT,
        "mismatched completion leaves task working",
    );

    append_codex_task_event(mp.task_status_file(), "task_complete", "fresh");
    mp.expect_foreground(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "matching completion marks the fresh turn done",
    );
}

#[test]
fn completed_status_survives_focus_zoom_and_narrow_resize() {
    let mut mp = Mp::launch_task_status_fixture();
    mp.start_into_main_list();
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");
    append_codex_task_event(mp.task_status_file(), "task_started", "fresh");
    mp.expect_foreground(WORKING_BORDER, STEP_TIMEOUT, "task is working");
    append_codex_task_event(mp.task_status_file(), "task_complete", "fresh");
    mp.expect_foreground(COMPLETED_BORDER, STEP_TIMEOUT, "task is complete");

    mp.send(b"\t");
    mp.send(b"\x1a"); // zoom
    mp.resize(24, 12);
    mp.expect_size(24, 12, STEP_TIMEOUT, "narrow resize settles");
    mp.expect_foreground(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "focus, zoom, and narrow resize preserve the completed frame",
    );
}

#[test]
fn local_input_and_interrupt_clear_completion() {
    let mut mp = Mp::launch_task_status_fixture();
    mp.start_into_main_list();
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");
    append_codex_task_event(mp.task_status_file(), "task_started", "fresh");
    append_codex_task_event(mp.task_status_file(), "task_complete", "fresh");
    mp.expect_foreground(COMPLETED_BORDER, STEP_TIMEOUT, "task is complete");

    mp.send(b"\r");
    mp.expect_foreground_transition_absent(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "local Enter clears completion before the next rollout append",
    );
    append_codex_task_event(mp.task_status_file(), "task_complete", "fresh");
    mp.expect_foreground_absent(
        COMPLETED_BORDER,
        Duration::from_millis(650),
        "duplicate old completion after Enter is ignored",
    );

    append_codex_task_event(mp.task_status_file(), "task_started", "fresh2");
    mp.expect_foreground(WORKING_BORDER, STEP_TIMEOUT, "new turn starts after Enter");
    append_codex_task_event(mp.task_status_file(), "task_complete", "fresh2");
    mp.expect_foreground(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "new turn completion is observed",
    );
    mp.send(b"\x1b");
    mp.expect_foreground_transition_absent(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "Esc clears completion before delayed rollout data",
    );
    mp.expect_foreground_absent(
        COMPLETED_BORDER,
        Duration::from_millis(650),
        "interrupted input does not stay done",
    );
    append_codex_task_event(mp.task_status_file(), "task_complete", "fresh2");
    mp.expect_foreground_absent(
        COMPLETED_BORDER,
        Duration::from_millis(650),
        "delayed completion from interrupted turn stays non-green",
    );
}

#[test]
fn isolated_permission_hook_renders_blocked_border() {
    let mut mp = Mp::launch_task_status_fixture();
    mp.start_into_main_list();
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");
    mp.write_hook_status("PermissionRequest");
    mp.expect_foreground(
        vt100::Color::Rgb(255, 183, 131),
        STEP_TIMEOUT,
        "isolated permission hook marks pane blocked",
    );
}

#[test]
fn process_exit_uses_ended_border_color() {
    let mut mp = Mp::launch_task_status_fixture();
    mp.start_into_main_list();
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");
    mp.send(b"\x04"); // EOF for the fake codex's `cat` child.
    mp.expect_foreground(ENDED_BORDER, STEP_TIMEOUT, "exited pane is ended");
}

impl Drop for Mp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// The parent MindPlayer owns the pane PTY and deliberately does not answer a
/// child cursor-position query. Keep the real Crossterm backend for all ANSI
/// painting, but provide the inline viewport's initial cursor position without
/// adding a fake terminal emulator to the test.
struct FixtureBackend(CrosstermBackend<std::io::Stdout>);

impl Backend for FixtureBackend {
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.draw(content)
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        Ok(Position { x: 0, y: 0 })
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> std::io::Result<Size> {
        self.0.size()
    }

    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        self.0.window_size()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Backend::flush(&mut self.0)
    }
}

/// Child process used only by the resize regression. It is launched through
/// the fake `codex` wrapper as a real process in the pane PTY, so ratatui's
/// Crossterm backend sees the same inline viewport and terminal size changes
/// as an installed Codex TUI would. The loop intentionally repaints after
/// every observed `terminal::size()` change and keeps a small transcript in
/// the PTY's scrollback via ratatui's inline insertion API.
#[test]
#[ignore = "launched as a bounded child process by the resize regression"]
fn ratatui_inline_painter_fixture() {
    if std::env::var_os("MINDPLAYER_RATATUI_FIXTURE") != Some("1".into()) {
        return;
    }
    let pane = std::env::var("MINDPLAYER_PANE_ID")
        .unwrap_or_else(|_| "fixture".to_string())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .rev()
        .take(6)
        .collect::<String>();
    let pane = pane.chars().rev().collect::<String>();
    let mut terminal = Terminal::with_options(
        FixtureBackend(CrosstermBackend::new(stdout())),
        TerminalOptions {
            viewport: Viewport::Inline(8),
        },
    )
    .expect("create inline ratatui fixture terminal");

    let (mut last_cols, mut last_rows) =
        crossterm::terminal::size().expect("fixture terminal size");
    for index in 0..24 {
        let line = Line::from(format!("MPFIX-{pane}-SCROLL-{index:02}"));
        terminal
            .insert_before(1, |buffer| {
                buffer.set_line(0, 0, &line, buffer.area.width);
            })
            .expect("insert fixture scrollback line");
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut tick = 0u64;
    while Instant::now() < deadline {
        let (cols, rows) = crossterm::terminal::size().expect("read fixture terminal size");
        if (cols, rows) != (last_cols, last_rows) {
            let mut output = stdout();
            output
                .write_all(b"\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H\x1b[?2026h")
                .expect("write Codex resize sequence");
            output.flush().expect("flush Codex resize sequence");
            terminal
                .resize(Rect::new(0, 0, cols, rows))
                .expect("resize inline fixture terminal");
            output
                .write_all(b"\x1b[?2026l")
                .expect("write synchronized-update end");
            output.flush().expect("flush synchronized-update end");
            last_cols = cols;
            last_rows = rows;
        }
        terminal
            .draw(|frame| {
                let lines = vec![
                    Line::from(format!("MPFIX-{pane}-TICK-{tick}-SIZE-{cols}x{rows}")),
                    Line::from(format!("MPFIX-{pane}-ROW-01 stable semantic row")),
                    Line::from(format!("MPFIX-{pane}-ROW-02 stable semantic row")),
                    Line::from(format!("MPFIX-{pane}-PROMPT> ready")),
                ];
                frame.render_widget(Paragraph::new(lines), frame.area());
            })
            .expect("paint inline fixture");
        tick = tick.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(35));
    }
}

fn fixture_size_tokens(screen: &str) -> Vec<String> {
    screen
        .lines()
        .flat_map(|line| line.split("MPFIX-").skip(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|word| word.split_once("-SIZE-").map(|(_, size)| size.to_string()))
        .map(|size| format!("SIZE-{size}"))
        .collect()
}

fn fixture_semantic_positions(screen: &str) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let mut row_one = Vec::new();
    let mut row_two = Vec::new();
    let mut prompts = Vec::new();
    for (index, line) in screen.lines().enumerate() {
        if line.contains("ROW-01") {
            row_one.push(index);
        }
        if line.contains("ROW-02") {
            row_two.push(index);
        }
        if line.contains("PROMPT>") {
            prompts.push(index);
        }
    }
    (row_one, row_two, prompts)
}

fn fixture_tick_tokens(screen: &str) -> BTreeSet<String> {
    screen
        .lines()
        .flat_map(|line| line.split("MPFIX-").skip(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|word| word.contains("-TICK-") && word.contains("-SIZE-"))
        .map(|word| format!("MPFIX-{word}"))
        .collect()
}

/// Return the latest complete repaint epoch for each fixture pane. A geometry
/// token can be present in a stale snapshot while the child is still inside
/// its synchronized update; the per-pane tick is the evidence that a newly
/// sized frame has actually been painted.
fn fixture_tick_epochs(screen: &str) -> BTreeMap<String, String> {
    screen
        .lines()
        .flat_map(|line| line.split("MPFIX-").skip(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|word| {
            let (pane, _) = word.split_once("-TICK-")?;
            if !word.contains("-SIZE-") {
                return None;
            }
            Some((pane.to_string(), format!("MPFIX-{word}")))
        })
        .collect()
}

fn wait_for_stable_fixture_geometry(
    mp: &Mp,
    expected_panes: usize,
    different_from: Option<&[String]>,
    ctx: &str,
) -> Vec<String> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut previous = None;
    let mut consecutive = 0;
    loop {
        let screen = mp.screen();
        let sizes = fixture_size_tokens(&screen);
        let ready = screen.matches("ROW-01").count() == expected_panes
            && screen.matches("ROW-02").count() == expected_panes
            && screen.matches("PROMPT>").count() == expected_panes
            && sizes.len() == expected_panes
            && sizes.windows(2).all(|pair| pair[0] == pair[1]);
        if ready {
            if previous.as_ref() == Some(&sizes) {
                consecutive += 1;
            } else {
                previous = Some(sizes.clone());
                consecutive = 1;
            }
            if consecutive >= 3 && different_from.is_none_or(|baseline| sizes != baseline) {
                return sizes;
            }
        } else {
            previous = None;
            consecutive = 0;
        }
        if Instant::now() >= deadline {
            panic!("{ctx}: fixture geometry did not settle\n{screen}");
        }
        std::thread::sleep(Duration::from_millis(45));
    }
}

/// Wait for two successive, complete repaint epochs at the requested
/// geometry, returning only after the semantic rows are stable across those
/// epochs. This synchronizes on the fixture's actual tick marker rather than
/// treating matching size/count metadata as proof that the resize repaint is
/// finished.
fn wait_for_confirmed_fixture_repaint(
    mp: &Mp,
    expected_sizes: &[String],
    expected_panes: usize,
    ctx: &str,
) {
    assert_eq!(
        expected_sizes.len(),
        expected_panes,
        "{ctx}: expected pane count"
    );
    assert!(
        expected_sizes.windows(2).all(|pair| pair[0] == pair[1]),
        "{ctx}: expected one uniform geometry per pane, got {expected_sizes:?}"
    );

    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut previous_epochs: Option<BTreeMap<String, String>> = None;
    let mut stable_positions = None;
    let mut consecutive = 0;
    loop {
        let screen = mp.screen();
        let sizes = fixture_size_tokens(&screen);
        let epochs = fixture_tick_epochs(&screen);
        let counts_match = screen.matches("ROW-01").count() == expected_panes
            && screen.matches("ROW-02").count() == expected_panes
            && screen.matches("PROMPT>").count() == expected_panes;
        let geometry_match = sizes == expected_sizes;
        let complete_frame = epochs.len() == expected_panes;

        if counts_match && geometry_match && complete_frame {
            if let Some(previous) = &previous_epochs {
                let all_panes_repainted = epochs
                    .iter()
                    .all(|(pane, epoch)| previous.get(pane).is_some_and(|old| old != epoch));
                if all_panes_repainted {
                    let positions = fixture_semantic_positions(&screen);
                    if stable_positions.as_ref() == Some(&positions) {
                        consecutive += 1;
                    } else {
                        stable_positions = Some(positions);
                        consecutive = 1;
                    }
                    if consecutive >= 2 {
                        return;
                    }
                }
            }
            previous_epochs = Some(epochs);
        } else {
            previous_epochs = None;
            stable_positions = None;
            consecutive = 0;
        }

        if Instant::now() >= deadline {
            panic!("{ctx}: no confirmed repaint at {expected_sizes:?}\n{screen}");
        }
        std::thread::sleep(Duration::from_millis(35));
    }
}

fn assert_steady_fixture_repaints(
    mp: &Mp,
    expected_sizes: &[String],
    expected_panes: usize,
    ctx: &str,
) {
    wait_for_confirmed_fixture_repaint(mp, expected_sizes, expected_panes, ctx);
    let first = mp.screen();
    let positions = fixture_semantic_positions(&first);
    assert_eq!(
        first.matches("ROW-01").count(),
        expected_panes,
        "{ctx}: ROW-01 count\n{first}"
    );
    assert_eq!(
        first.matches("ROW-02").count(),
        expected_panes,
        "{ctx}: ROW-02 count\n{first}"
    );
    assert_eq!(
        first.matches("PROMPT>").count(),
        expected_panes,
        "{ctx}: prompt count\n{first}"
    );
    assert!(
        !positions.0.is_empty(),
        "{ctx}: ROW-01 has no physical position\n{first}"
    );
    assert_eq!(
        fixture_size_tokens(&first),
        expected_sizes,
        "{ctx}: geometry\n{first}"
    );

    let deadline = Instant::now() + Duration::from_millis(450);
    let mut ticks = BTreeSet::new();
    while Instant::now() < deadline {
        let screen = mp.screen();
        assert_eq!(
            screen.matches("ROW-01").count(),
            expected_panes,
            "{ctx}: ROW-01 drifted\n{screen}"
        );
        assert_eq!(
            screen.matches("ROW-02").count(),
            expected_panes,
            "{ctx}: ROW-02 drifted\n{screen}"
        );
        assert_eq!(
            screen.matches("PROMPT>").count(),
            expected_panes,
            "{ctx}: prompt drifted\n{screen}"
        );
        assert_eq!(
            fixture_semantic_positions(&screen),
            positions,
            "{ctx}: semantic rows moved\n{screen}"
        );
        assert_eq!(
            fixture_size_tokens(&screen),
            expected_sizes,
            "{ctx}: geometry drifted\n{screen}"
        );
        ticks.extend(fixture_tick_tokens(&screen));
        std::thread::sleep(Duration::from_millis(45));
    }
    assert!(
        ticks.len() >= 3,
        "{ctx}: fixture did not produce three repaint ticks\n{}",
        mp.screen()
    );
}

/// Outer resize regression for Codex's inline (non-alternate-screen) painter.
///
/// Two real pane children render ratatui inline viewports with scrollback and
/// a prompt. The parent PTY is resized wide -> narrow -> wide, then the live
/// view is zoomed and restored three times. Each settled frame must retain the
/// same physical semantic rows and prompts across changing repaint ticks; the
/// fixture's visible geometry must follow the child PTY's actual
/// `terminal::size()` and restore exactly.
#[test]
fn inline_codex_painter_preserves_rows_and_prompts_across_resize_and_zoom() {
    let mut mp = Mp::launch_inline_fixture();
    mp.start_into_main_list();

    mp.send(b"v");
    mp.expect("space marks", STEP_TIMEOUT, "multi-select enabled");
    mp.send(b" ");
    mp.expect("1 marked", STEP_TIMEOUT, "first fixture session marked");
    mp.send(b"\x1b[B");
    mp.send(b" ");
    mp.expect("2 marked", STEP_TIMEOUT, "second fixture session marked");
    mp.send(b"\r");
    mp.expect("PROMPT>", STEP_TIMEOUT, "inline fixture prompt painted");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| {
            screen.matches("ROW-01").count() == 2
                && screen.matches("ROW-02").count() == 2
                && screen.matches("PROMPT>").count() == 2
                && fixture_size_tokens(screen).len() == 2
        }),
        "both inline fixtures must paint their rows and prompts.\n{}",
        mp.screen()
    );

    let wide_sizes = wait_for_stable_fixture_geometry(&mp, 2, None, "initial wide frame geometry");
    assert_steady_fixture_repaints(&mp, &wide_sizes, 2, "initial wide frame");

    let mut narrow_sizes = None;
    for cycle in 1..=3 {
        mp.resize(32, 88);
        assert!(
            mp.wait_until(STEP_TIMEOUT, |screen| {
                screen.matches("ROW-01").count() == 2
                    && screen.matches("ROW-02").count() == 2
                    && screen.matches("PROMPT>").count() == 2
                    && fixture_size_tokens(screen).len() == 2
                    && fixture_size_tokens(screen) != wide_sizes
            }),
            "narrow resize cycle {cycle} did not settle.\n{}",
            mp.screen()
        );
        let sizes = wait_for_stable_fixture_geometry(
            &mp,
            2,
            Some(&wide_sizes),
            &format!("narrow cycle {cycle}"),
        );
        if let Some(expected) = &narrow_sizes {
            assert_eq!(&sizes, expected, "narrow geometry changed on cycle {cycle}");
        } else {
            narrow_sizes = Some(sizes.clone());
        }
        assert_steady_fixture_repaints(&mp, &sizes, 2, &format!("narrow cycle {cycle}"));
        assert!(
            !mp.screen().contains("SCROLL-"),
            "visible fixture history must stay cleared after the resize sequence in cycle {cycle}.\n{}",
            mp.screen()
        );

        mp.resize(ROWS, COLS);
        assert!(
            mp.wait_until(STEP_TIMEOUT, |screen| fixture_size_tokens(screen)
                == wide_sizes),
            "wide restore in cycle {cycle} did not reach exact original geometry.\n{}",
            mp.screen()
        );
        assert_steady_fixture_repaints(&mp, &wide_sizes, 2, &format!("wide cycle {cycle}"));

        mp.send(b"\x1a");
        mp.expect("ZOOMED", STEP_TIMEOUT, "zoom enabled");
        let zoom_sizes =
            wait_for_stable_fixture_geometry(&mp, 1, None, &format!("zoom cycle {cycle}"));
        assert_steady_fixture_repaints(&mp, &zoom_sizes, 1, &format!("zoom cycle {cycle}"));

        mp.send(b"\x1a");
        mp.expect_absent("ZOOMED", STEP_TIMEOUT, "zoom restored");
        assert!(
            mp.wait_until(STEP_TIMEOUT, |screen| fixture_size_tokens(screen)
                == wide_sizes),
            "zoom restore in cycle {cycle} did not reach exact wide geometry.\n{}",
            mp.screen()
        );
        assert_steady_fixture_repaints(&mp, &wide_sizes, 2, &format!("zoom restore cycle {cycle}"));
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

/// Scenario 3 — full-screen Trace smoke for the session observer feature.
///
/// This stays deliberately black-box: it drives a real `mindplayer` binary in a
/// real PTY, appends Codex JSONL to the selected session, and waits for the
/// rendered screen to update without triggering a manual rescan. Ctrl-G opens
/// Trace directly on macOS without relying on a function key; Esc returns to
/// the live PTY, where ordinary prompt text must still reach the fake `codex`
/// (`cat`) session.
#[test]
fn observer_tracks_codex_jsonl_appends_without_stealing_prompt_input() {
    const ORDINARY_PROMPT: &str = "ordinary-observe-smoke";
    const POST_TOGGLE_PROMPT: &str = "after-toggle-observe-smoke";
    const FUNCTION_MARKER: &str = "mp_observe_smoke_function_call";
    const SCROLL_MARKER: &str = "mp_observe_scroll_reached_end";
    const TOKEN_TOTAL: u64 = 43_210;

    for (label, rows, cols) in [("narrow", 28, 88), ("wide", 40, 160)] {
        let mut mp = Mp::launch_with_size(rows, cols);
        mp.start_into_main_list();

        mp.send(b"\r");
        mp.expect(PANE_READY, STEP_TIMEOUT, "agent pane came up");
        assert!(
            !mp.raw_output_contains("starting..."),
            "session entry must not publish a placeholder frame before the PTY"
        );

        mp.send(ORDINARY_PROMPT.as_bytes());
        mp.expect(
            ORDINARY_PROMPT,
            STEP_TIMEOUT,
            "ordinary prompt input still reaches the fake codex child",
        );

        mp.send(b"\x07"); // Ctrl-G
        mp.expect("TRACE", STEP_TIMEOUT, "Ctrl-G opens full-screen Trace");
        mp.expect(
            "READ ONLY",
            STEP_TIMEOUT,
            "Trace is visibly labelled as read-only",
        );
        mp.expect(
            "focused session",
            STEP_TIMEOUT,
            "Trace identifies its source session",
        );
        mp.expect(
            "Ctrl-G toggle",
            STEP_TIMEOUT,
            "Trace footer advertises the macOS-safe shortcut",
        );

        let selected_codex_file = if mp.screen().contains(TITLE_A) {
            &mp.codex_alfa_file
        } else {
            &mp.codex_brvo_file
        };
        append_codex_observer_event(selected_codex_file, FUNCTION_MARKER, TOKEN_TOTAL);
        assert!(
            mp.wait_until(STEP_TIMEOUT, |s| {
                let panel = observer_panel_text(s);
                panel.contains("PROMPTS")
                    && panel.contains("THIS TURN")
                    && panel.contains("43.2K TOKENS")
                    && (panel.contains("2 MODEL CALLS") || panel.contains("2 calls"))
                    && (panel.contains("latest call 23.2K") || panel.contains("latest 23.2K"))
                    && panel.contains("YOUR TEXT")
                    && panel.contains("~5 tokens")
                    && panel.contains(FUNCTION_MARKER)
                    && contains_observer_native_usage(s)
            }),
            "{label} Trace must notice the selected Codex JSONL append and \
             render provider-recorded turn usage as primary, the prompt-text \
             estimate as secondary, and the function_call name without \
             a rescan.\n\
             ---- observer panel ----\n{}\n---- screen ----\n{}\n----------------",
            observer_panel_text(&mp.screen()),
            mp.screen()
        );
        assert!(
            !observer_panel_text(&mp.screen()).contains("session cumulative"),
            "Trace must not foreground the all-session total"
        );

        // Long JSONL events wrap to many terminal rows. The real mouse wheel
        // over the execution region must use that rendered height, not the
        // number of logical events, or the final marker is unreachable.
        append_long_codex_execution(selected_codex_file, SCROLL_MARKER);
        mp.expect(
            "44.2K TOKENS",
            STEP_TIMEOUT,
            "Trace ingested the long execution batch",
        );
        assert!(
            !observer_panel_text(&mp.screen()).contains(SCROLL_MARKER),
            "the end marker should begin below the execution viewport"
        );
        let wheel_down = if label == "narrow" {
            b"\x1b[<65;40;18M".as_slice()
        } else {
            b"\x1b[<65;100;18M".as_slice()
        };
        for _ in 0..30 {
            mp.send(wheel_down);
        }
        mp.expect(
            SCROLL_MARKER,
            STEP_TIMEOUT,
            "mouse wheel scrolls wrapped execution history to the end",
        );
        println!(
            "---- {label} observer screen after append ----\n{}\n---- end {label} observer screen ----",
            mp.screen()
        );

        // Exercise the real crossterm SGR mouse path, not just the App method:
        // wheel down over the rendered prompt rail must select the older turn.
        mp.send(b"\x1b[<65;5;6M");
        assert!(
            mp.wait_until(STEP_TIMEOUT, |s| s.matches(OLDER_TRACE_PROMPT).count() >= 2),
            "{label} mouse wheel over the prompt rail must select the older prompt\n{}",
            mp.screen()
        );
        // Return to the newest turn so the rest of this scenario keeps testing
        // the appended provider event.
        mp.send(b"\x1b[<64;5;6M");
        assert!(
            mp.wait_until(STEP_TIMEOUT, |s| s.matches("trace this command").count()
                >= 2),
            "{label} reverse wheel must reselect the newest prompt\n{}",
            mp.screen()
        );

        // Retired Ctrl-O must not close Trace; Esc is the universal return key.
        mp.send(b"\x0f");
        mp.expect("TRACE", STEP_TIMEOUT, "Ctrl-O no longer controls Trace");
        mp.send(b"\x1b");
        mp.expect_absent("TRACE", STEP_TIMEOUT, "Esc returns to the live session");

        mp.send(POST_TOGGLE_PROMPT.as_bytes());
        mp.expect(
            POST_TOGGLE_PROMPT,
            STEP_TIMEOUT,
            "ordinary prompt input still reaches the fake codex child after observer toggles",
        );

        mp.send(b"\x07"); // Ctrl-G opens again.
        mp.expect("TRACE", STEP_TIMEOUT, "Ctrl-G reopens Trace");
        mp.send(b"\x07"); // Ctrl-G toggles it closed.
        mp.expect_absent("TRACE", STEP_TIMEOUT, "Ctrl-G closes Trace");
    }
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

#[test]
fn cursor_is_available_in_new_and_handoff_pickers() {
    {
        let mut mp = Mp::launch();
        mp.start_into_main_list();
        mp.send(b"n");
        mp.expect("New session", STEP_TIMEOUT, "new-session picker opened");
        mp.expect(
            "cursor",
            STEP_TIMEOUT,
            "Cursor appears in new-session picker",
        );
    }

    let mut mp = Mp::launch();
    mp.start_into_main_list();
    mp.send(b"h");
    mp.expect("Handoff", STEP_TIMEOUT, "handoff picker opened");
    mp.expect("cursor", STEP_TIMEOUT, "Cursor appears in handoff picker");
}

fn isolated_codex_home(mp: &Mp) -> PathBuf {
    mp._tmp
        .join("home")
        .join(".mindplayer")
        .join("accounts")
        .join("codex")
        .join("isolated")
}

#[test]
fn codex_usage_separates_workspace_members_and_refreshes_after_u_enter() {
    let mut mp = Mp::launch_account_quota_fixture();
    mp.start_into_main_list();
    mp.expect(
        "65.0%",
        STEP_TIMEOUT,
        "second workspace member keeps its own usage",
    );
    mp.expect(
        "10.0%",
        STEP_TIMEOUT,
        "first workspace member keeps its own usage",
    );
    assert!(!mp.screen().contains("same login"), "{}", mp.screen());
    assert!(
        !mp.screen().contains("default credits"),
        "zero cap must not draw 100%: {}",
        mp.screen()
    );
    mp.expect(
        "credit cap reached",
        STEP_TIMEOUT,
        "positive cap is separate from weekly usage",
    );

    mp.send(b"u");
    mp.expect("Accounts", STEP_TIMEOUT, "accounts opened");
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "first account entered");
    mp.wait_for_codex_home(&mp._tmp.join("home/.codex"), STEP_TIMEOUT);
    mp.send(b"\x18");
    mp.expect("multi-select", STEP_TIMEOUT, "return to list");
    mp.send(b"u");
    mp.expect("Accounts", STEP_TIMEOUT, "accounts reopened");
    mp.send(b"\x1b[B\r");
    mp.wait_for_codex_home(&isolated_codex_home(&mp), STEP_TIMEOUT);
    std::fs::write(isolated_codex_home(&mp).join("usage-percent"), "73\n").unwrap();
    mp.send(b"\x18");
    mp.expect("multi-select", STEP_TIMEOUT, "return from second account");
    mp.send(b"\x12");
    mp.expect(
        "73.0%",
        STEP_TIMEOUT,
        "Ctrl-R refresh reads the second account's latest usage",
    );
    assert!(!mp.screen().contains("same login"), "{}", mp.screen());

    // A real duplicate still gets an explanation on ALL rows, including credits.
    std::fs::write(
        isolated_codex_home(&mp).join("usage-email"),
        "one@example.com\n",
    )
    .unwrap();
    mp.send(b"\x12");
    mp.expect(
        "same login as default",
        STEP_TIMEOUT,
        "duplicate login is identified",
    );
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| screen.lines().any(|line| line
            .contains("isolated credits")
            && line.contains("same login as default"))),
        "{}",
        mp.screen()
    );
}

#[test]
fn account_rename_from_korean_ime_preserves_login_and_refreshes_usage() {
    let mut mp = Mp::launch_account_quota_fixture();
    mp.start_into_main_list();
    let original_home = isolated_codex_home(&mp);
    let original_rollout = mp.codex_alfa_file.clone();

    // Resume a rollout that physically belongs to the isolated account before
    // relabelling it. The marker proves the pre-rename session uses this home.
    mp.send(b"/");
    mp.expect("type to filter", STEP_TIMEOUT, "session search opened");
    mp.send(b"alfa\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "isolated rollout resumed");
    mp.wait_for_codex_home(&original_home, STEP_TIMEOUT);
    mp.send(b"\x11"); // Ctrl-Q closes the focused pane so rename is allowed.
    mp.expect("multi-select", STEP_TIMEOUT, "returned to the session list");

    mp.expect("10.0%", STEP_TIMEOUT, "usage fixture ready");
    mp.send(b"u");
    mp.expect("Accounts", STEP_TIMEOUT, "accounts opened");
    mp.send(b"\x1b[B");
    mp.send("ㄱ".as_bytes());
    mp.expect(
        "Rename account",
        STEP_TIMEOUT,
        "Korean physical r opens rename",
    );
    mp.send(b"\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7frenamed\r");
    mp.expect_absent("Rename account", STEP_TIMEOUT, "rename was accepted");
    let accounts = mindplayer_core::accounts::load_accounts(&mp._tmp.join("home"));
    let renamed = accounts
        .iter()
        .find(|account| {
            account.provider == mindplayer_core::session::Agent::Codex && account.name == "renamed"
        })
        .expect("renamed account persisted");
    assert_eq!(renamed.slot_path(), original_home);
    assert!(original_home.is_dir(), "the isolated home must not move");
    assert!(!original_home.with_file_name("renamed").exists());
    assert!(
        original_rollout.is_file(),
        "the original rollout must remain"
    );
    assert_eq!(
        std::fs::read_to_string(original_home.join("auth.json")).unwrap(),
        FIXTURE_AUTH_SENTINEL
    );
    mp.send(b"q");
    mp.expect(
        "renamed weekly",
        STEP_TIMEOUT,
        "renamed quota label reaches footer",
    );
    mp.expect_absent(
        "isolated weekly",
        STEP_TIMEOUT,
        "old label cannot return from stale worker",
    );
    mp.send(b"u");
    mp.expect("Accounts", STEP_TIMEOUT, "accounts reopened after rename");
    mp.send(b"\x1b[B");
    mp.send(b"q");
    mp.expect_absent("Accounts", STEP_TIMEOUT, "accounts closed after reopen");

    // The same discovered rollout must still resume from the unchanged home
    // after the account has the new display label.
    mp.send(b"/");
    mp.expect("type to filter", STEP_TIMEOUT, "session search reopened");
    mp.send(b"alfa\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "renamed account rollout resumed");
    mp.wait_for_codex_home(&original_home, STEP_TIMEOUT);
    assert!(
        original_rollout.is_file(),
        "the original rollout must remain"
    );
    assert_eq!(
        std::fs::read_to_string(original_home.join("auth.json")).unwrap(),
        FIXTURE_AUTH_SENTINEL
    );
}

#[test]
fn new_session_explicit_isolated_choice_launches_on_isolated_codex_home() {
    let mut mp = Mp::launch_with_options(ROWS, COLS, false, false, true, false);
    mp.start_into_main_list();
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "new-session picker opened");
    mp.expect(
        "codex · isolated",
        STEP_TIMEOUT,
        "isolated Codex choice is visible",
    );
    mp.send(b"\x1b[B");
    mp.expect(
        "▶ codex · isolated",
        STEP_TIMEOUT,
        "isolated choice selected",
    );
    mp.send(b"\r\r");
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "isolated new-session pane came up",
    );
    mp.wait_for_codex_marker(&isolated_codex_home(&mp), FIXTURE_CONFIG_NEW);
    assert_eq!(
        std::fs::read_to_string(mp.isolated_auth()).unwrap(),
        FIXTURE_AUTH_SENTINEL
    );
}

#[test]
fn new_session_explicit_inherited_choice_pins_primary_home_over_ambient_isolated() {
    let mut mp = Mp::launch_with_options(ROWS, COLS, true, false, true, false);
    mp.start_into_main_list();
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "new-session picker opened");
    mp.send(b"\x1b[B");
    mp.expect(
        "▶ codex · default",
        STEP_TIMEOUT,
        "inherited choice selected",
    );
    mp.send(b"\r\r");
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "inherited new-session pane came up",
    );
    mp.wait_for_codex_home(&mp._tmp.join("home").join(".codex"), STEP_TIMEOUT);
}

#[test]
fn handoff_to_isolated_account_launches_on_isolated_codex_home() {
    let mut mp = Mp::launch_with_options(ROWS, COLS, false, false, true, false);
    mp.start_into_main_list();
    append_handoff_turn(&mp.codex_brvo_file);
    mp.send(b"h");
    mp.expect("Handoff", STEP_TIMEOUT, "handoff picker opened");
    mp.send(b"\x1b[B");
    mp.expect(
        "▶ isolated",
        STEP_TIMEOUT,
        "isolated handoff target selected",
    );
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "isolated handoff pane came up");
    mp.wait_for_codex_marker(&isolated_codex_home(&mp), FIXTURE_CONFIG_NEW);
    assert_eq!(
        std::fs::read_to_string(mp.isolated_auth()).unwrap(),
        FIXTURE_AUTH_SENTINEL
    );
}

#[test]
fn later_new_session_reads_updated_shared_canonical_codex_config() {
    let mut mp = Mp::launch_with_options(ROWS, COLS, false, false, true, false);
    mp.start_into_main_list();

    mp.send(b"n");
    mp.expect(
        "New session",
        STEP_TIMEOUT,
        "first new-session picker opened",
    );
    mp.send(b"\x1b[B\r\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "first isolated pane came up");
    mp.wait_for_codex_marker(&isolated_codex_home(&mp), FIXTURE_CONFIG_NEW);

    std::fs::write(mp.canonical_config(), "model = \"fixture-next\"\n")
        .expect("update canonical Codex config");
    mp.send(b"\x18");
    mp.expect("multi-select", STEP_TIMEOUT, "returned to session list");
    mp.send(b"n");
    mp.expect(
        "New session",
        STEP_TIMEOUT,
        "second new-session picker opened",
    );
    mp.send(b"\x1b[B\r\r");
    mp.wait_for_codex_marker(&isolated_codex_home(&mp), "model = \"fixture-next\"");
    assert_eq!(
        std::fs::read_to_string(mp.isolated_auth()).unwrap(),
        FIXTURE_AUTH_SENTINEL
    );
}

#[test]
fn default_new_session_prefers_ambient_isolated_codex_home() {
    let mut mp = Mp::launch_with_options(ROWS, COLS, true, false, true, false);
    mp.start_into_main_list();
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "new-session picker opened");
    mp.expect(
        "▶ codex · isolated",
        STEP_TIMEOUT,
        "ambient isolated choice is preferred",
    );
    mp.send(b"\r\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "default new-session pane came up");
    mp.wait_for_codex_home(&isolated_codex_home(&mp), STEP_TIMEOUT);
}

#[test]
fn accounts_primary_switch_wins_for_later_new_session_over_ambient_isolated() {
    let mut mp = Mp::launch_with_options(ROWS, COLS, true, true, true, false);
    mp.start_into_main_list();

    // The fixture starts with isolated primary and inherited fallback. Accounts
    // opens on the first Codex entry, inherited, so `w` changes the persisted
    // provider primary without relying on list ordering inside the test.
    mp.send(b"u");
    mp.expect("Accounts", STEP_TIMEOUT, "accounts screen opened");
    mp.expect(
        "default",
        STEP_TIMEOUT,
        "inherited Codex account is selected",
    );
    mp.send(b"w");
    mp.expect(
        "new codex sessions will use default",
        STEP_TIMEOUT,
        "primary switch confirmed",
    );
    mp.send(b"q");
    mp.expect_absent("Accounts", STEP_TIMEOUT, "accounts screen closed");

    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "new-session picker opened");
    mp.expect(
        "▶ codex · default",
        STEP_TIMEOUT,
        "switched primary leads new-session choices",
    );
    mp.send(b"\r\r");
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "post-switch new-session pane came up",
    );
    mp.wait_for_codex_home(&mp._tmp.join("home").join(".codex"), STEP_TIMEOUT);
}
