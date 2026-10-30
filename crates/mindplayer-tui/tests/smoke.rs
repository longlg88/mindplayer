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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::stdout;
use std::io::{Read, Write};
use std::net::TcpStream;
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
const STARTUP_CHOICE: &str = "MindPlayer / 실행 설정표";
const PROMPT_JEV: &str = "[F6] Jev ON · 외부 전송 동의";
const PROMPT_NATIVE: &str = "[F6] Jev OFF · 기본 모델 유지";
const F6: &[u8] = b"\x1b[17~";
const F7: &[u8] = b"\x1b[18~";

fn fixture_codex_identity(workspace: &str, email: &str) -> String {
    let email = email.trim().to_lowercase();
    let mut hash = DefaultHasher::new();
    (workspace, email).hash(&mut hash);
    format!("codex-user-{:016x}", hash.finish())
}

fn fixture_claude_identity(workspace: &str, email: &str) -> String {
    let email = email.trim().to_lowercase();
    let user = format!("fixture-user-{email}");
    let mut hash = DefaultHasher::new();
    ("claude", user.as_str(), workspace, &email).hash(&mut hash);
    format!("claude-user-{:016x}", hash.finish())
}

#[derive(Clone, Copy)]
enum CodexFixture {
    Echo,
    InlinePainter,
    TaskStatus,
    AccountQuota,
    AuthRecovery,
    ClaudeAuthRecovery,
    KeylessRouter,
    ActualRouter,
    LifecycleRouter,
    RealJevRouter,
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

struct FixtureDir(PathBuf);

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for FixtureDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

/// A fresh, unique temp directory, optionally isolated from the source tree.
fn unique_tmp() -> FixtureDir {
    let root = std::env::var_os("MINDPLAYER_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
    std::fs::create_dir_all(&root).expect("create temp root");
    loop {
        let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = root.join(format!("mp-smoke-{}-{}", std::process::id(), n));
        match std::fs::create_dir(&dir) {
            Ok(()) => return FixtureDir(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => panic!("create temp dir: {err}"),
        }
    }
}

struct FixtureChild(Box<dyn portable_pty::Child + Send + Sync>);

impl std::ops::Deref for FixtureChild {
    type Target = dyn portable_pty::Child + Send + Sync;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl std::ops::DerefMut for FixtureChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.0
    }
}

impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
#[test]
fn fixture_directory_is_removed_after_normal_and_unwinding_teardown() {
    fn assert_reaped(pid: u32) {
        let output = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .expect("check fixture child");
        assert!(
            !output.status.success(),
            "fixture child {pid} was not reaped"
        );
    }

    let mp = Mp::launch_with_options(ROWS, COLS, false, false, false, false);
    let path = mp._tmp.clone();
    let pid = mp.child.process_id().expect("fixture child PID");
    assert!(path.is_dir());
    drop(mp);
    assert_reaped(pid);
    assert!(!path.exists(), "normal teardown removed the fixture");

    let mut spawned = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Mp::launch_with_options_and_fixture_after_spawn(
            (ROWS, COLS),
            false,
            false,
            false,
            false,
            CodexFixture::Echo,
            |path, pid| {
                spawned = Some((path.to_path_buf(), pid.expect("fixture child PID")));
                panic!("exercise constructor unwind");
            },
        );
    }));
    assert!(result.is_err());
    let (path, pid) = spawned.expect("constructor reached child spawn");
    assert_reaped(pid);
    assert!(!path.exists(), "constructor unwind removed the fixture");
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

fn write_omp_script(path: &Path, body: &str) {
    // The real OMP token lookup can outlive MindPlayer and recreate HOME after
    // fixture cleanup. Never let a smoke fixture run it or read real credentials.
    // Replace atomically so an in-flight probe never sees a truncated script.
    let staged = path.with_extension("new");
    write_script(
        &staged,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = token ]; then\n  : > \"$HOME/.mindplayer/omp-token-probe\" 2>/dev/null\n  exit 1\nfi\n{body}"
        ),
    );
    std::fs::rename(staged, path).expect("install isolated OMP launcher");
}

/// Offline auth protocol fixture: only a native `codex login` changes the
/// selected home's sign-in flag. Quota remains available while signed out.
fn seed_codex_auth_fixture(path: &Path) {
    std::fs::write(
        path,
        r#"import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

const home = process.env.CODEX_HOME;
if (!home) throw new Error('auth fixture requires explicit CODEX_HOME');
const args = process.argv.slice(2);
const events = `${process.env.MINDPLAYER_TEST_MARKER_DIR}/../auth-events.jsonl`;
function record(event, fields = {}) {
  appendFileSync(events, `${JSON.stringify({ event, home, cwd: process.cwd(), pid: process.pid, at: Date.now(), ...fields })}\n`);
}
function setting(name, fallback) {
  try { return readFileSync(`${home}/${name}`, 'utf8').trim(); } catch { return fallback; }
}
function signedOut() {
  return existsSync(`${home}/signed-out`) && !existsSync(`${home}/fixture-signed-in`);
}
if (args[0] === '--fixture-launch') {
  // Probe subprocesses may inherit a pane ID; they are not conversations.
  if (!args.includes('app-server')) record('conversation', { args: args.slice(1), pane: process.env.MINDPLAYER_PANE_ID });
  process.exit(0);
}
if (args[0] === 'logout') {
  record('logout', { args });
  process.exit(91);
}
if (args[0] === 'login') {
  if (args[1] === 'status') {
    process.stdout.write(signedOut() ? 'Not logged in\n' : 'Logged in using ChatGPT\n');
    process.exit(signedOut() ? 1 : 0);
  }
  record('login', { args });
  mkdirSync(home, { recursive: true });
  writeFileSync(`${home}/fixture-signed-in`, 'native fixture login completed\n');
  process.exit(0);
}
if (!args.includes('app-server')) throw new Error('unexpected auth fixture invocation');
const email = () => setting('account-email', home.endsWith('/.codex') ? 'one@example.com' : 'two@example.com');
const input = createInterface({ input: process.stdin });
input.on('line', line => {
  const request = JSON.parse(line);
  const reply = result => process.stdout.write(`${JSON.stringify({ id: request.id, result })}\n`);
  if (request.method === 'initialize') reply({ serverInfo: { name: 'smoke-auth' } });
  else if (request.method === 'model/list') reply({ data: [{ model: 'fixture-simple', id: 'fixture-simple', isDefault: true, inputModalities: ['text'] }], nextCursor: null });
  else if (request.method === 'account/read') {
    const refresh = request.params?.refreshToken === true;
    record('account-request', { id: request.id, refresh });
    const respond = () => {
      if (existsSync(`${home}/hold-account-response`)) {
        setTimeout(respond, 20);
        return;
      }
      const result = { account: signedOut() ? null : { type: 'chatgpt', email: email(), planType: 'business' } };
      if (!existsSync(`${home}/legacy-workspace`)) result.workspaceRouting = { chatgptAccountId: setting('account-workspace', 'fixture-account') };
      record('account-response', { id: request.id, refresh, result });
      reply(result);
    };
    setTimeout(respond, Number(setting('account-delay-ms', '0')));
  } else if (request.method === 'account/rateLimits/read') {
    record('quota-response', { id: request.id });
    reply({ accountId: setting('quota-workspace', 'fixture-account'), ordinaryUsageAllowed: true, rateLimits: { primary: { usedPercent: 10, windowDurationMins: 10080 } } });
  }
});
"#,
    )
    .expect("write isolated Codex auth fixture");
}

/// Saved native status and the required profile response are independent
/// fixtures. Neither contacts Claude, Keychain, or a real credential store.
fn seed_claude_auth_fixture(path: &Path) {
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/local-model-router/claude-fixture.mjs"),
        path,
    )
    .expect("copy isolated native Claude and profile fixture");
}

fn seed_claude_status(home: &Path, email: &str, org: &str) {
    std::fs::create_dir_all(home).expect("create fixture Claude home");
    // These are the conditional saved claude.ai fields exported by 2.1.302.
    // A true loggedIn value deliberately says nothing about live validity.
    let status = serde_json::json!({
        "loggedIn": true,
        "authMethod": "claude.ai",
        "apiProvider": "firstParty",
        "analyticsDisabled": false,
        "projectsDirectory": home.join("projects"),
        "configDirectory": home,
        "email": email,
        "orgId": org,
        "orgName": "Fixture organization",
        "subscriptionType": null
    });
    std::fs::write(
        home.join("fixture-auth-status.json"),
        serde_json::to_vec(&status).unwrap(),
    )
    .expect("write saved native status fixture");
    std::fs::write(
        home.join("fixture-profile.json"),
        serde_json::to_vec(&serde_json::json!({
            "account": { "uuid": format!("fixture-user-{email}"), "email": email },
            "organization": { "uuid": org }
        }))
        .unwrap(),
    )
    .expect("write independent synthetic profile fixture");
}

fn seed_claude_session(root: &Path, cwd: &Path, id: &str, title: &str) -> PathBuf {
    let project = root.join(cwd.to_string_lossy().replace(['/', '.'], "-"));
    std::fs::create_dir_all(&project).expect("create fixture Claude project");
    let file = project.join(format!("{id}.jsonl"));
    let rows = [
        serde_json::json!({
            "type": "user", "sessionId": id, "cwd": cwd,
            "timestamp": "2026-07-13T10:00:00Z",
            "message": { "role": "user", "content": title }
        }),
        serde_json::json!({
            "type": "assistant", "sessionId": id, "cwd": cwd,
            "timestamp": "2026-07-13T10:00:01Z",
            "message": { "role": "assistant", "content": "synthetic saved reply",
                "usage": { "input_tokens": 10, "output_tokens": 5 } }
        }),
    ];
    let mut transcript = std::fs::File::create(&file).expect("create fixture Claude transcript");
    for row in rows {
        writeln!(transcript, "{row}").expect("write fixture Claude transcript");
    }
    file
}

/// Seed a synthetic private UI, or an explicitly supplied native candidate,
/// without changing the global Codex or the developer's HOME.
fn seed_private_codex(home: &Path, fake_codex: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let staged = home.join("private-codex-fixture");
    let staged_host = home.join("private-code-mode-host-fixture");
    if let Some(real) = std::env::var_os("MINDPLAYER_TEST_PRIVATE_CODEX") {
        let real = PathBuf::from(real);
        assert!(
            real.is_absolute() && real.is_file(),
            "explicit private Codex"
        );
        let host = real.parent().unwrap().join("codex-code-mode-host");
        std::fs::copy(&real, &staged).expect("copy the explicit native Codex");
        std::fs::copy(host, &staged_host).expect("copy the matching native host");
        for path in [&staged, &staged_host] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("private native fixture permissions");
        }
    } else {
        let source = std::fs::read_to_string(fake_codex).expect("read fake Codex");
        write_script(
            &staged,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = \"--mindplayer-ui-contract\" ]; then printf '%s\\n' 'mindplayer-native-routing-v4'; exit 0; fi\nif [ -n \"$MINDPLAYER_ROUTER_MARKER_DIR\" ]; then\n  case \" $* \" in\n    *\" app-server \"*) marker=private-app-server ;;\n    *) marker=private-ui ;;\n  esac\n  printf '%s\\n' \"$0\" > \"$MINDPLAYER_ROUTER_MARKER_DIR/$marker\"\nfi\n{source}"
            ),
        );
        // Explicit no-op host: this synthetic fixture does not prove native tools.
        write_script(&staged_host, "#!/bin/sh\nexit 0\n");
    }
    let digest = std::process::Command::new("node")
        .args([
            "--input-type=module",
            "--eval",
            "import { createHash } from 'node:crypto'; import { readFileSync } from 'node:fs'; process.stdout.write(JSON.stringify(process.argv.slice(1).map(path => createHash('sha256').update(readFileSync(path)).digest('hex'))));",
        ])
        .arg(&staged)
        .arg(&staged_host)
        .env_remove("NODE_OPTIONS")
        .output()
        .expect("hash isolated private Codex fixture");
    assert!(digest.status.success(), "hash private Codex fixture");
    let [digest, host_digest]: [String; 2] =
        serde_json::from_slice(&digest.stdout).expect("fixture SHA-256 hashes");
    assert_eq!(digest.len(), 64);
    assert_eq!(host_digest.len(), 64);
    let root = home.join(".mindplayer/native-codex");
    let build = root.join("builds").join(&digest);
    std::fs::create_dir_all(&build).expect("create private Codex fixture directories");
    for directory in [&root, &root.join("builds"), &build] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .expect("private fixture directory permissions");
    }
    std::fs::rename(staged, build.join("codex")).expect("publish private Codex fixture");
    std::fs::rename(staged_host, build.join("codex-code-mode-host"))
        .expect("publish private code-mode host fixture");
    let manifest = root.join("active.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1,
            "sourceRevision": "092d3acd6bec3e3a14bdc7e7a2810ab628ab759d",
            "contract": "mindplayer-native-routing-v4",
            "version": "0.162.1",
            "binarySha256": digest,
            "binary": format!("builds/{digest}/codex"),
            "codeModeHostSha256": host_digest,
            "codeModeHost": format!("builds/{digest}/codex-code-mode-host"),
        }))
        .expect("serialize private Codex fixture manifest"),
    )
    .expect("write private Codex fixture manifest");
    std::fs::set_permissions(manifest, std::fs::Permissions::from_mode(0o600))
        .expect("private fixture manifest permissions");
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
struct FixtureJevCredentials(PathBuf);

impl Drop for FixtureJevCredentials {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Mp {
    parser: Arc<Mutex<vt100::Parser>>,
    raw_output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    child: FixtureChild,
    _master: Box<dyn MasterPty + Send>,
    _tmp: PathBuf,
    codex_marker_dir: PathBuf,
    router_marker_dir: PathBuf,
    jev_provider: String,
    /// The router fixture was given a Jev key, so its sessions start with Jev ON.
    jev_key: bool,
    _jev_credentials: Option<FixtureJevCredentials>,
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
    // Fields are dropped in declaration order, after the child and PTY handles.
    _fixture_dir: FixtureDir,
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

    fn launch_auth_recovery_fixture() -> Mp {
        Self::launch_with_options_and_fixture(
            ROWS,
            COLS,
            false,
            false,
            true,
            false,
            CodexFixture::AuthRecovery,
        )
    }

    fn launch_claude_auth_fixture() -> Mp {
        Self::launch_with_options_and_fixture(
            ROWS,
            COLS,
            false,
            false,
            true,
            false,
            CodexFixture::ClaudeAuthRecovery,
        )
    }

    fn launch_actual_router_fixture() -> Mp {
        Self::launch_router_fixture(CodexFixture::ActualRouter)
    }

    fn launch_router_fixture(fixture: CodexFixture) -> Mp {
        Self::launch_with_options_and_fixture(ROWS, COLS, false, false, true, false, fixture)
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
        Self::launch_with_options_and_fixture_after_spawn(
            (rows, cols),
            ambient_isolated,
            isolated_primary,
            with_accounts,
            enable_cursor,
            fixture,
            |_, _| {},
        )
    }

    fn launch_with_options_and_fixture_after_spawn(
        dimensions: (u16, u16),
        ambient_isolated: bool,
        isolated_primary: bool,
        with_accounts: bool,
        enable_cursor: bool,
        fixture: CodexFixture,
        after_spawn: impl FnOnce(&Path, Option<u32>),
    ) -> Mp {
        let (rows, cols) = dimensions;
        let inline_fixture = matches!(fixture, CodexFixture::InlinePainter);
        let stale_completion = matches!(fixture, CodexFixture::TaskStatus);
        let actual_router = matches!(
            fixture,
            CodexFixture::ActualRouter
                | CodexFixture::LifecycleRouter
                | CodexFixture::RealJevRouter
                | CodexFixture::KeylessRouter
        );
        let real_jev = matches!(fixture, CodexFixture::RealJevRouter);
        let claude_auth = matches!(fixture, CodexFixture::ClaudeAuthRecovery);
        let fixture_dir = unique_tmp();
        let tmp = fixture_dir.0.clone();
        let (identity_workspace, default_email, isolated_email) = if actual_router {
            (
                "fixture-account",
                "fixture@example.com",
                "fixture@example.com",
            )
        } else if matches!(
            fixture,
            CodexFixture::AccountQuota | CodexFixture::AuthRecovery
        ) {
            ("fixture-account", "one@example.com", "two@example.com")
        } else {
            (
                "fixture-account",
                "fixture@example.com",
                "fixture@example.com",
            )
        };
        let default_identity = fixture_codex_identity(identity_workspace, default_email);
        let isolated_identity = fixture_codex_identity(identity_workspace, isolated_email);

        // Fully isolate every store the binary touches from the developer's real
        // home, via both a temp HOME and the explicit MINDPLAYER_* overrides.
        let home = tmp.join("home");
        let codex_dir = tmp.join("codex"); // MINDPLAYER_CODEX_DIR
        let claude_dir = tmp.join("claude"); // empty except in Claude auth scenarios
        let kiro_dir = tmp.join("kiro"); // empty
        let cursor_dir = tmp.join("cursor"); // empty
        let omp_dir = tmp.join("omp").join("sessions");
        let state_dir = tmp.join("state");
        let audit_dir = tmp.join("audit");
        let prompts_dir = tmp.join("prompts");
        let codex_marker_dir = tmp.join("codex-markers");
        let router_marker_dir = tmp.join("router-markers");
        let router_audit_dir = tmp.join("router-audits");
        let scope = tmp.join("scope"); // session cwd + launch dir
        let bindir = tmp.join("bin");
        for d in [
            &home,
            &codex_dir,
            &claude_dir,
            &kiro_dir,
            &cursor_dir,
            &omp_dir,
            &state_dir,
            &audit_dir,
            &prompts_dir,
            &codex_marker_dir,
            &router_marker_dir,
            &router_audit_dir,
            &scope,
            &bindir,
        ] {
            std::fs::create_dir_all(d).expect("create dir");
        }
        write_omp_script(&bindir.join("omp"), "exit 1\n");

        let isolated_home = home
            .join(".mindplayer")
            .join("accounts")
            .join("codex")
            .join("isolated");
        let default_claude_home = home.join(".claude");
        let isolated_claude_home = home.join(".mindplayer/accounts/claude/slot-seven");
        seed_claude_status(&default_claude_home, "one@example.test", "fixture-org-one");
        seed_claude_status(&isolated_claude_home, "two@example.test", "fixture-org-two");
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
                    "disabled": claude_auth,
                    "codex_identity": default_identity
                },
                {
                    "provider": "claude",
                    "name": if claude_auth { "cobalt" } else { "default" },
                    "slot": { "kind": "inherited" },
                    "role": "primary",
                    "disabled": !claude_auth,
                    "claude_identity": fixture_claude_identity("fixture-org-one", "one@example.test")
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
                    "disabled": claude_auth,
                    "codex_identity": isolated_identity
                }));
            }
            if claude_auth {
                accounts.as_array_mut().unwrap().push(serde_json::json!({
                    "provider": "claude",
                    "name": "spruce7",
                    "slot": { "kind": "isolated", "path": isolated_claude_home },
                    "role": "fallback",
                    "disabled": false,
                    "claude_identity": fixture_claude_identity("fixture-org-two", "two@example.test")
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

        // Never resolve a real Claude executable, including background status
        // probes in the older Codex-only smoke scenarios.
        let claude_fixture = tmp.join("claude-auth-fixture.mjs");
        if claude_auth {
            seed_claude_auth_fixture(&claude_fixture);
            for (name, mode) in [("claude", "native"), ("curl", "curl")] {
                write_script(
                    &bindir.join(name),
                    &format!(
                        "#!/bin/sh\nexec node '{}' '{mode}' \"$0\" \"$@\"\n",
                        claude_fixture.display()
                    ),
                );
            }
            // No real macOS vault lookup is allowed in these fixtures.
            write_script(&bindir.join("security"), "#!/bin/sh\nexit 44\n");
            for selected in [&default_claude_home, &isolated_claude_home] {
                std::fs::write(
                    selected.join(".credentials.json"),
                    serde_json::to_vec(&serde_json::json!({
                        "claudeAiOauth": {
                            "accessToken": format!("synthetic-offline-token:{}", selected.display()),
                            "expiresAt": 4_102_444_800_000_u64
                        }
                    }))
                    .unwrap(),
                )
                .expect("write synthetic profile transport credential");
            }
        } else {
            write_script(
                &bindir.join("claude"),
                "#!/bin/sh\nif [ \"$1 $2\" = \"auth status\" ]; then\n  selected=${CLAUDE_CONFIG_DIR:-\"$HOME/.claude\"}\n  cat \"$selected/fixture-auth-status.json\"\n  exit $?\nfi\nprintf 'unexpected offline Claude invocation\\n' >&2\nexit 92\n",
            );
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
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"serverInfo":{"name":"smoke"}}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"fixture-simple","id":"fixture-simple","isDefault":true,"inputModalities":["text"]},{"model":"fixture-complex","id":"fixture-complex","inputModalities":["text"]}],"nextCursor":null}}' ;;
    *'"method":"account/read"'*)
      printf '{"id":3,"result":{"account":{"type":"chatgpt","email":"%s","planType":"business"}}}\n' "$email" ;;
    *'"method":"account/rateLimits/read"'*)
      printf '{"id":2,"result":{"accountId":"workspace-shared","ordinaryUsageAllowed":true,"rateLimits":{"primary":{"usedPercent":%s,"windowDurationMins":10080},"individualLimit":{"limit":"%s","used":"%s","remainingPercent":0},"spendControlReached":true}}}\n' "$percent" "$cap" "$cap" ;;
  esac
done
exit 0"#
        } else {
            r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"serverInfo":{"name":"smoke"}}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"fixture-simple","id":"fixture-simple","isDefault":true,"inputModalities":["text"]},{"model":"fixture-complex","id":"fixture-complex","inputModalities":["text"]}],"nextCursor":null}}' ;;
    *'"method":"account/read"'*) printf '%s\n' '{"id":3,"result":{"account":{"type":"chatgpt","email":"fixture@example.com","planType":"business"}}}' ;;
    *'"method":"account/rateLimits/read"'*) printf '%s\n' '{"id":2,"result":{"accountId":"workspace-shared","ordinaryUsageAllowed":true,"rateLimits":{"primary":{"usedPercent":5,"windowDurationMins":10080}}}}' ;;
  esac
done
exit 0"#
        };
        // Native's execution sentinel must not confuse fixture quota probes
        // with the application's router. This fixture answers probes in sh.
        let auth_fixture = tmp.join("codex-auth-fixture.mjs");
        let app_server = if matches!(fixture, CodexFixture::AuthRecovery) {
            seed_codex_auth_fixture(&auth_fixture);
            format!("exec node '{}' \"$@\"", auth_fixture.display())
        } else {
            format!(
                "if [ -n \"$MINDPLAYER_ROUTER_FIXTURE\" ]; then exec node \"$MINDPLAYER_ROUTER_FIXTURE\" \"$@\"; fi\n{app_server}"
            )
        };
        write_script(
            &bindir.join("codex"),
            &format!(
                "#!/bin/sh\n\nif [ \"$1 $2\" = \"login status\" ]; then\n  printf 'logged in\\n'\n  exit 0\nfi\nif [ \"$1\" = \"app-server\" ]; then\n  {app_server}\nfi\nif [ \"$MINDPLAYER_RATATUI_FIXTURE\" = \"1\" ]; then\n  exec \"$MINDPLAYER_SMOKE_TEST_BINARY\" --exact {} --ignored --nocapture\nfi\nif [ -n \"$MINDPLAYER_PANE_ID\" ]; then\n  marker='{}'\n  printf 'CODEX_HOME=%s\\n' \"$CODEX_HOME\" > \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  for arg in \"$@\"; do printf 'ARG=%s\\n' \"$arg\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\"; done\n  printf 'CONFIG_CONTENT_BEGIN\\n' >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  cat \"$CODEX_HOME/config.toml\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\" 2>/dev/null\n  printf 'CONFIG_CONTENT_END\\n' >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\nfi\nprintf 'PANE-READY\\n'\nexec cat\n",
                INLINE_FIXTURE_TEST,
                codex_marker_dir.display()
            ),
        );
        if actual_router {
            write_script(
                &bindir.join("codex"),
                "#!/bin/sh\nexec node \"$MINDPLAYER_ROUTER_FIXTURE\" \"$@\"\n",
            );
        } else {
            write_script(
                &bindir.join("codex"),
                &format!(
                    "#!/bin/sh\n\nif [ \"$1 $2\" = \"login status\" ]; then\n  printf 'logged in\\n'\n  exit 0\nfi\ncase \" $* \" in\n  *app-server*)\n    {app_server}\n    exit $?\n    ;;\nesac\nif [ \"$MINDPLAYER_RATATUI_FIXTURE\" = \"1\" ]; then\n  exec \"$MINDPLAYER_SMOKE_TEST_BINARY\" --exact {INLINE_FIXTURE_TEST} --ignored --nocapture\nfi\nif [ -n \"$MINDPLAYER_PANE_ID\" ]; then\n  marker='{marker}'\n  printf 'CODEX_HOME=%s\\nPID=%s\\n' \"$CODEX_HOME\" \"$$\" > \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  printf '%s\\n' \"$$\" >> \"$marker/../native-launches.log\"\n  for key in TYPESAFE_API_KEY AI_GATEWAY_API_KEY VERCEL_OIDC_TOKEN; do\n    case \"$key\" in\n      TYPESAFE_API_KEY) present=${{TYPESAFE_API_KEY:+1}} ;;\n      AI_GATEWAY_API_KEY) present=${{AI_GATEWAY_API_KEY:+1}} ;;\n      VERCEL_OIDC_TOKEN) present=${{VERCEL_OIDC_TOKEN:+1}} ;;\n    esac\n    printf '%s_PRESENT=%s\\n' \"$key\" \"${{present:-0}}\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  done\n  for arg in \"$@\"; do printf 'ARG=%s\\n' \"$arg\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\"; done\n  printf 'CONFIG_CONTENT_BEGIN\\n' >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\n  cat \"$CODEX_HOME/config.toml\" >> \"$marker/$MINDPLAYER_PANE_ID.txt\" 2>/dev/null\n  printf 'CONFIG_CONTENT_END\\n' >> \"$marker/$MINDPLAYER_PANE_ID.txt\"\nfi\nprintf 'PANE-READY\\n'\nexec cat\n",
                    app_server = app_server,
                    INLINE_FIXTURE_TEST = INLINE_FIXTURE_TEST,
                    marker = codex_marker_dir.display(),
                ),
            );
        }
        if matches!(fixture, CodexFixture::AuthRecovery) {
            let codex = bindir.join("codex");
            let original = std::fs::read_to_string(&codex).expect("read echo Codex fixture");
            write_script(
                &codex,
                &format!(
                    "#!/bin/sh\ncase \"$1\" in login|logout) exec node '{auth}' \"$@\" ;; esac\nif [ -n \"$MINDPLAYER_PANE_ID\" ]; then node '{auth}' --fixture-launch \"$@\"; fi\n{original}",
                    auth = auth_fixture.display()
                ),
            );
        }
        seed_private_codex(&home, &bindir.join("codex"));
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
        let (codex_alfa_file, codex_brvo_file) = if claude_auth {
            seed_claude_session(
                &isolated_claude_home.join("projects"),
                &{
                    let session_cwd = scope.join("session-cwd");
                    std::fs::create_dir_all(&session_cwd).expect("create different resume cwd");
                    session_cwd
                },
                "claude-alfa-0001",
                TITLE_A,
            );
            seed_claude_session(&claude_dir, &scope, "claude-brvo-0002", TITLE_B);
            // Codex observer helpers are unused in the Claude-only fixture.
            (tmp.join("unused-codex-alfa"), tmp.join("unused-codex-brvo"))
        } else {
            (
                seed_codex_session(&alfa_root, &scope, "codex-alfa-0001", TITLE_A),
                seed_codex_session(&codex_dir, &scope, "codex-brvo-0002", TITLE_B),
            )
        };
        if stale_completion {
            append_codex_task_event(&codex_alfa_file, "task_complete", "stale");
            // Task-status scenarios must select their transcript explicitly,
            // not depend on filesystem timestamp resolution or scan order.
            // Keep BRVO newer so blindly resuming the first row is always wrong.
            let now = SystemTime::now();
            for (file, age) in [(&codex_alfa_file, 2), (&codex_brvo_file, 1)] {
                std::fs::File::open(file)
                    .unwrap()
                    .set_times(
                        std::fs::FileTimes::new().set_modified(now - Duration::from_secs(age)),
                    )
                    .unwrap();
            }
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
        // Offline smoke must never consume a real inherited inference key.
        cmd.env_remove("TYPESAFE_API_KEY");
        cmd.env_remove("AI_GATEWAY_API_KEY");
        cmd.env_remove("VERCEL_OIDC_TOKEN");
        cmd.env_remove("MINDPLAYER_CLAUDE_JEV_URL");
        cmd.env_remove("MINDPLAYER_CLAUDE_JEV_TOKEN");
        cmd.env_remove("NODE_OPTIONS");
        for key in [
            "OMP_PROFILE",
            "PI_PROFILE",
            "PI_CONFIG_DIR",
            "PI_CONFIG_FILES",
            "XDG_DATA_HOME",
        ] {
            cmd.env_remove(key);
        }
        cmd.env("PI_CODING_AGENT_DIR", tmp.join("omp"));
        cmd.env("HOME", &home);
        if claude_auth {
            cmd.env("MINDPLAYER_TEST_CLAUDE_ROOT", &tmp);
            // Synthetic canaries prove native Claude receives no Jev credentials.
            cmd.env("TYPESAFE_API_KEY", "mindplayer-smoke-typesafe-canary");
            cmd.env("AI_GATEWAY_API_KEY", "mindplayer-smoke-gateway-canary");
            cmd.env("VERCEL_OIDC_TOKEN", "mindplayer-smoke-oidc-canary");
            // Test native account selection against synthetic ambient auth;
            // the event log records presence only, never token values.
            cmd.env("CLAUDE_CONFIG_DIR", tmp.join("ambient-claude"));
            cmd.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", tmp.join("ambient-vault"));
            cmd.env("ANTHROPIC_API_KEY", "synthetic-ambient-key");
            cmd.env("ANTHROPIC_AUTH_TOKEN", "synthetic-ambient-auth");
            cmd.env("CLAUDE_CODE_OAUTH_TOKEN", "synthetic-ambient-oauth");
            cmd.env(
                "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
                "synthetic-ambient-refresh",
            );
            cmd.env("CLAUDE_CODE_OAUTH_SCOPES", "synthetic-ambient-scope");
            cmd.env(
                "ANTHROPIC_CUSTOM_HEADERS",
                "x-fixture-auth: synthetic-ambient",
            );
            cmd.env("ANTHROPIC_BASE_URL", "https://fixture.invalid");
            cmd.env("CLAUDE_CODE_USE_BEDROCK", "1");
            cmd.env("CLAUDE_CODE_USE_VERTEX", "1");
            cmd.env("CLAUDE_CODE_USE_FOUNDRY", "1");
        }
        cmd.env("MINDPLAYER_CODEX_DIR", &codex_dir);
        cmd.env("MINDPLAYER_CLAUDE_DIR", &claude_dir);
        cmd.env("MINDPLAYER_KIRO_DIR", &kiro_dir);
        cmd.env("MINDPLAYER_CURSOR_DIR", &cursor_dir);
        cmd.env("MINDPLAYER_OMP_DIR", &omp_dir);
        cmd.env("MINDPLAYER_STATE", &state_dir);
        cmd.env("MINDPLAYER_AUDIT", &audit_dir);
        cmd.env("MINDPLAYER_PROMPTS_DIR", &prompts_dir);
        cmd.env("MINDPLAYER_HANDOFF_DIR", tmp.join("handoffs"));
        cmd.env("MINDPLAYER_TEST_MARKER_DIR", &codex_marker_dir);
        let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/local-model-router/mindplayer-fixture.mjs");
        cmd.env("MINDPLAYER_ROUTER_FIXTURE", fixture_path);
        cmd.env("MINDPLAYER_ROUTER_MARKER_DIR", &router_marker_dir);
        let mut jev_provider = "typesafe".to_string();
        let mut jev_credentials = None;
        if actual_router {
            cmd.env(
                "MINDPLAYER_ROUTER_LIFECYCLE_GATES",
                if matches!(fixture, CodexFixture::LifecycleRouter) {
                    "1"
                } else {
                    "0"
                },
            );
            let preload = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tools/local-model-router/mindplayer-jev-fixture.mjs")
                .canonicalize()
                .expect("locate Jev fixture preload");
            cmd.env(
                "NODE_OPTIONS",
                format!("--import=\"{}\"", preload.display()),
            );
            // Production's own private audit files remain observable even when
            // Gateway uses OS trust and never calls the fetch preload.
            cmd.env("TMPDIR", &router_audit_dir);
            cmd.env("TMP", &router_audit_dir);
            cmd.env("TEMP", &router_audit_dir);
            if real_jev {
                assert_eq!(
                    std::env::var("MINDPLAYER_TEST_REAL_JEV").as_deref(),
                    Ok("1"),
                    "authenticated Jev requires explicit opt-in"
                );
                let credential =
                    FixtureJevCredentials(home.join(".mindplayer/jev-credentials.json"));
                let output = std::process::Command::new("node")
                    .arg(&preload)
                    .arg("--prepare-credentials")
                    .arg(&credential.0)
                    .env_remove("NODE_OPTIONS")
                    .output()
                    .expect("select the current Jev credential without inference");
                assert!(
                    output.status.success(),
                    "current Jev credential is unavailable"
                );
                let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
                    .expect("credential fixture returns provider metadata only");
                jev_provider = metadata["provider"]
                    .as_str()
                    .filter(|provider| ["typesafe", "gateway"].contains(provider))
                    .expect("supported current Jev provider")
                    .to_string();
                jev_credentials = Some(credential);
                cmd.env("MINDPLAYER_TEST_REAL_JEV", "1");
            } else {
                cmd.env("MINDPLAYER_TEST_REAL_JEV", "0");
                if !matches!(fixture, CodexFixture::KeylessRouter) {
                    cmd.env("TYPESAFE_API_KEY", "mindplayer-smoke-typesafe-canary");
                }
            }
            cmd.env("MINDPLAYER_TEST_JEV_PROVIDER", &jev_provider);
        }
        if matches!(fixture, CodexFixture::AccountQuota) {
            cmd.env("MINDPLAYER_ROUTER_QUOTA", "1");
        }
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

        let child = FixtureChild(pair.slave.spawn_command(cmd).expect("spawn mindplayer"));
        drop(pair.slave);
        after_spawn(&tmp, child.process_id());

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
            router_marker_dir,
            jev_provider,
            jev_key: actual_router && !matches!(fixture, CodexFixture::KeylessRouter),
            _jev_credentials: jev_credentials,
            #[cfg(target_os = "macos")]
            html_fixture,
            #[cfg(target_os = "macos")]
            open_marker,
            codex_alfa_file,
            codex_brvo_file,
            _fixture_dir: fixture_dir,
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

    fn capture_stable_ui_evidence(&self, label: &str) {
        self.capture_ui_evidence_at_size(label, 40, 120);
    }

    fn capture_ui_evidence_at_size(&self, label: &str, rows: u16, columns: u16) {
        let Some(directory) = std::env::var_os("MINDPLAYER_TEST_UI_EVIDENCE_DIR") else {
            return;
        };
        let directory = PathBuf::from(directory);
        assert!(
            directory.is_absolute() && directory.is_dir(),
            "explicit existing evidence directory"
        );
        assert!(label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-'));
        let deadline = Instant::now() + STEP_TIMEOUT;
        let mut frame = self.screen();
        let mut unchanged_since = Instant::now();
        loop {
            let current = self.screen();
            if frame != current {
                frame = current;
                unchanged_since = Instant::now();
            } else if unchanged_since.elapsed() >= Duration::from_millis(150) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{rows}x{columns} screenshot buffer did not stabilize"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let size = self
            ._master
            .get_size()
            .expect("actual screenshot PTY geometry");
        assert_eq!((size.rows, size.cols), (rows, columns));
        assert_eq!(self.parser.lock().unwrap().screen().size(), (rows, columns));
        let write_new = |suffix: &str, bytes: &[u8]| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let path = directory.join(format!("{label}.{suffix}"));
            let mut file = options
                .open(&path)
                .expect("exclusively create task-owned UI evidence");
            file.write_all(bytes).expect("write actual PTY evidence");
            println!("UI_EVIDENCE={}", path.display());
        };
        write_new("screen.txt", frame.as_bytes());
        write_new("ansi", &self.raw_output.lock().unwrap());
        let cells: Vec<Vec<_>> = {
            let parser = self.parser.lock().unwrap();
            let screen = parser.screen();
            (0..rows)
                .map(|row| {
                    (0..columns)
                        .map(|column| {
                            let cell = screen.cell(row, column).expect("screen cell");
                            serde_json::json!({
                                "text": cell.contents(), "wide": cell.is_wide(),
                                "continuation": cell.is_wide_continuation(),
                                "fg": format!("{:?}", cell.fgcolor()),
                                "bg": format!("{:?}", cell.bgcolor()), "bold": cell.bold()
                            })
                        })
                        .collect()
                })
                .collect()
        };
        let metadata = serde_json::json!({
            "rows": size.rows, "columns": size.cols, "label": label,
            "binary": std::env::var_os("MINDPLAYER_TEST_BINARY").map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mindplayer"))),
            "sourceVersion": env!("CARGO_PKG_VERSION"),
            "nativeUiContract": "mindplayer-native-routing-v4", "buffer": frame, "cells": cells,
            "scope": "actual MindPlayer PTY and vt100 cell attributes; synthetic native UI/backend/judge; no auth/inference or OS styling/IME proof"
        });
        write_new("json", &serde_json::to_vec_pretty(&metadata).unwrap());
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

    fn resume_task_status_fixture(&mut self) {
        self.start_into_main_list();
        self.send(b"/");
        self.expect(
            "type to filter",
            STEP_TIMEOUT,
            "select task-status transcript",
        );
        self.send(b"alfa\r");
        assert_single_codex_resume(self, &self._tmp.join("home/.codex"), "codex-alfa-0001");
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

    fn codex_auth_events(&self, home: &Path, event: &str) -> Vec<serde_json::Value> {
        let home = home.display().to_string();
        std::fs::read_to_string(self._tmp.join("auth-events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|row| row["home"] == home && row["event"] == event)
            .collect()
    }

    fn wait_for_codex_auth_events(&self, home: &Path, event: &str, count: usize) {
        assert!(
            self.wait_until(STEP_TIMEOUT, |_| self.codex_auth_events(home, event).len()
                >= count),
            "expected {count} {event} events for {}: {:?}\n{}",
            home.display(),
            self.codex_auth_events(home, event),
            self.screen()
        );
    }

    fn claude_auth_events(&self, home: Option<&Path>, event: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self._tmp.join("claude-auth-events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|row| {
                row["event"] == event
                    && home.is_none_or(|home| row["home"] == home.to_string_lossy().as_ref())
            })
            .collect()
    }

    fn wait_for_claude_auth_events(&self, home: &Path, event: &str, count: usize) {
        assert!(
            self.wait_until(STEP_TIMEOUT, |_| self
                .claude_auth_events(Some(home), event)
                .len()
                >= count),
            "expected {count} {event} events for {}: {:?}\n{}",
            home.display(),
            self.claude_auth_events(Some(home), event),
            self.screen()
        );
    }

    fn wait_for_router_route(&self, label: &str, model: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        let needle = format!("\"label\":\"{label}\",\"model\":\"{model}\"");
        loop {
            let routes = std::fs::read_to_string(self.router_marker_dir.join("routes.jsonl"))
                .unwrap_or_default();
            if routes.contains(&needle) {
                return;
            }
            if Instant::now() >= deadline {
                panic!("router did not record {label} -> {model}; routes: {routes}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn router_rows(&self, name: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.router_marker(name))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid metadata-only router marker"))
            .collect()
    }

    fn wait_for_router_turn(&self, label: &str) -> serde_json::Value {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let rows = self.router_rows("routes.jsonl");
            let routes: Vec<_> = rows.iter().filter(|row| row["label"] == label).collect();
            if !routes.is_empty() {
                assert_eq!(
                    routes.len(),
                    1,
                    "exactly one synthetic turn/start for {label}"
                );
                return routes[0].clone();
            }
            assert!(
                Instant::now() < deadline,
                "missing synthetic turn/start for {label}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn router_audit_rows(&self) -> Vec<serde_json::Value> {
        std::fs::read_dir(self._tmp.join("router-audits"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("routing.jsonl")).ok())
            .flat_map(|contents| {
                contents
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("valid service audit metadata"))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn assert_router_audit_turn_once(&self, label: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        let turn_id = format!("fixture-turn-{label}");
        loop {
            let audit = self.router_audit_rows();
            let acknowledgments: Vec<_> = audit
                .iter()
                .filter(|row| row["event"] == "start-acknowledged" && row["turnId"] == turn_id)
                .collect();
            let completions: Vec<_> = audit
                .iter()
                .filter(|row| row["event"] == "turn-completed" && row["turnId"] == turn_id)
                .collect();
            if !acknowledgments.is_empty() && !completions.is_empty() {
                assert_eq!(acknowledgments.len(), 1);
                assert_eq!(completions.len(), 1);
                assert_eq!(completions[0]["status"], "completed");
                let ack = acknowledgments[0];
                let one = |event: &str| {
                    let rows: Vec<_> = audit
                        .iter()
                        .filter(|row| {
                            row["event"] == event
                                && row["requestId"] == ack["requestId"]
                                && row["threadId"] == ack["threadId"]
                        })
                        .collect();
                    assert_eq!(rows.len(), 1, "exactly one {event} for {label}");
                    rows[0]
                };
                let started = one("jev-started");
                let judged = one("jev-judged");
                let selected = one("model-selected");
                let requested = one("route-requested");
                for row in [started, judged, selected] {
                    assert_eq!(row["provider"], self.jev_provider);
                }
                assert_eq!(started["rubricVersion"], "mindplayer-capacity-v2");
                assert_eq!(judged["rubricVersion"], started["rubricVersion"]);
                for field in ["inputTokens", "outputTokens"] {
                    assert!(judged["usage"][field]
                        .as_u64()
                        .is_some_and(|tokens| tokens > 0));
                }
                assert!([
                    "bounded_routine",
                    "multistep_coordination",
                    "open_problem",
                    "unknown"
                ]
                .contains(&judged["capability"].as_str().expect("valid Jev capability")));
                for field in ["contextSufficient", "highConsequence"] {
                    assert!(judged[field]
                        .as_f64()
                        .is_some_and(|value| (0.0..=1.0).contains(&value)));
                }
                let route = self.wait_for_router_turn(label);
                for field in ["model", "effort"] {
                    assert_eq!(
                        selected[field], route[field],
                        "selected configuration was accepted"
                    );
                    assert_eq!(requested[field], route[field]);
                }
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing Jev audit and accepted turn for {label}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_router_jev_turn_once(&self, label: &str) {
        self.assert_router_audit_turn_once(label);
        // macOS Gateway uses system-fetch.mjs; the service audit observes the
        // validated result without intercepting or replacing its OS transport.
        if self.jev_provider == "gateway" && cfg!(target_os = "macos") {
            return;
        }
        let read_rows = |name: &str| -> Vec<serde_json::Value> {
            std::fs::read_to_string(self.router_marker(name))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid router marker JSON"))
                .filter(|row: &serde_json::Value| row["label"] == label)
                .collect()
        };
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let events = read_rows("jev.jsonl");
            let responses: Vec<_> = events
                .iter()
                .filter(|row| row["event"] == "response")
                .collect();
            let routes = read_rows("routes.jsonl");
            if !responses.is_empty() && !routes.is_empty() {
                assert_eq!(routes.len(), 1, "exactly one turn/start for {label}");
                assert_eq!(
                    events.iter().filter(|row| row["event"] == "start").count(),
                    1,
                    "exactly one Jev start for {label}"
                );
                assert_eq!(responses.len(), 1, "exactly one Jev response for {label}");
                let response = responses[0];
                assert_eq!(response["provider"], self.jev_provider);
                assert_eq!(
                    response["status"], 200,
                    "successful Jev response for {label}"
                );
                if self.jev_provider == "typesafe" {
                    let version: Vec<_> = response["model"]
                        .as_str()
                        .expect("physical Jev model")
                        .strip_prefix("jev-")
                        .expect("resolved physical Jev version")
                        .split('.')
                        .collect();
                    assert_eq!(version.len(), 3);
                    assert!(version.iter().all(|part| !part.is_empty()
                        && (part.len() == 1 || !part.starts_with('0'))
                        && part.bytes().all(|byte| byte.is_ascii_digit())));
                } else {
                    assert_eq!(response["model"], "typesafe-ai/jev");
                }
                let start = events.iter().find(|row| row["event"] == "start").unwrap();
                assert_eq!(start["rubricVersion"], "mindplayer-capacity-v2");
                assert_eq!(start["configurationRubricMatches"], true);
                assert_eq!(start["questionsHaveRubric"], true);
                assert_eq!(
                    start["recentCount"], 0,
                    "prompt-first state excludes transcript history"
                );
                assert!(start["stateBytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes <= 8192));
                assert_eq!(
                    start["stateKeys"],
                    serde_json::json!([
                        "attachments",
                        "baseline",
                        "contextKnown",
                        "currentFamily",
                        "currentInput",
                        "previousDecision",
                        "recent",
                        "rubricVersion",
                        "schemaVersion",
                        "truncated"
                    ])
                );
                let all = self.router_rows("jev.jsonl");
                let starts: Vec<_> = all.iter().filter(|row| row["event"] == "start").collect();
                let position = starts.iter().position(|row| row["label"] == label).unwrap();
                if position == 0 {
                    assert_eq!(start["previousDecision"], serde_json::Value::Null);
                    assert_eq!(start["previousDecisionKeys"], serde_json::json!([]));
                } else {
                    assert_eq!(
                        start["previousDecisionKeys"],
                        serde_json::json!([
                            "capability",
                            "contextSufficient",
                            "effort",
                            "highConsequence",
                            "model",
                            "turnOutcome"
                        ])
                    );
                    let previous_label = starts[position - 1]["label"].as_str().unwrap();
                    let route = self.wait_for_router_turn(previous_label);
                    let prior_response = all
                        .iter()
                        .find(|row| row["event"] == "response" && row["label"] == previous_label)
                        .unwrap();
                    for field in ["model", "effort"] {
                        assert_eq!(start["previousDecision"][field], route[field]);
                    }
                    for field in ["capability", "contextSufficient", "highConsequence"] {
                        assert_eq!(start["previousDecision"][field], prior_response[field]);
                    }
                    assert_eq!(start["previousDecision"]["turnOutcome"], "completed");
                }
                assert!([
                    "bounded_routine",
                    "multistep_coordination",
                    "open_problem",
                    "unknown"
                ]
                .contains(&response["capability"].as_str().expect("Jev capability")));
                for field in ["inputTokens", "outputTokens"] {
                    assert!(response["usage"][field]
                        .as_u64()
                        .is_some_and(|tokens| tokens > 0));
                }
                for field in ["contextSufficient", "highConsequence"] {
                    assert!(response[field]
                        .as_f64()
                        .is_some_and(|value| (0.0..=1.0).contains(&value)));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    for path in [&self.router_marker_dir, &self.router_marker("jev.jsonl")] {
                        assert_eq!(
                            std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                            0
                        );
                    }
                }
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing Jev response and turn/start for {label}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn router_marker(&self, name: &str) -> PathBuf {
        self.router_marker_dir.join(name)
    }

    fn assert_router_consecutive_totals(&self, labels: &[&str]) {
        self.assert_router_mixed_totals(labels, labels.len());
    }

    fn assert_router_mixed_totals(&self, labels: &[&str], judgments: usize) {
        let audit = self.router_audit_rows();
        for event in [
            "jev-started",
            "jev-judged",
            "model-selected",
            "route-requested",
            "start-acknowledged",
            "turn-completed",
        ] {
            assert_eq!(
                audit.iter().filter(|row| row["event"] == event).count(),
                if ["start-acknowledged", "turn-completed"].contains(&event) {
                    labels.len()
                } else {
                    judgments
                },
                "exact consecutive {event} count; no retries or extra turns"
            );
        }
        assert_eq!(
            audit
                .iter()
                .filter(|row| row["event"] == "route-skipped")
                .count(),
            labels.len() - judgments,
            "only explicit native prompts bypass judgment"
        );
        assert!(!audit
            .iter()
            .any(|row| ["jev-failed", "route-failed", "start-rejected"]
                .contains(&row["event"].as_str().unwrap_or_default())));
        let starts: Vec<_> = audit
            .iter()
            .filter(|row| row["event"] == "jev-started")
            .collect();
        for (index, started) in starts.iter().enumerate() {
            assert_eq!(
                started["previousDecisionKnown"],
                index > 0,
                "same-thread accepted prior judgment is bound to the next turn"
            );
            assert_eq!(started["catalogRevision"], starts[0]["catalogRevision"]);
            assert_eq!(started["threadId"], starts[0]["threadId"]);
        }
        let routes = self.router_rows("routes.jsonl");
        assert_eq!(routes.len(), labels.len());
        for (route, label) in routes.iter().zip(labels) {
            assert_eq!(route["label"], *label);
            assert_eq!(route["threadId"], starts[0]["threadId"]);
        }
        if self.jev_provider == "typesafe" || !cfg!(target_os = "macos") {
            let responses = self.router_rows("jev.jsonl");
            for event in ["start", "response"] {
                assert_eq!(
                    responses.iter().filter(|row| row["event"] == event).count(),
                    judgments
                );
            }
        }
    }

    fn assert_router_status(&self, model: &str) {
        self.wait_for_router_status(model, "completed", None);
    }

    fn wait_for_router_status(
        &self,
        model: &str,
        stage: &str,
        turn_id: Option<&str>,
    ) -> serde_json::Value {
        let root = self._tmp.join("home/.mindplayer/routing-status");
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let states: Vec<serde_json::Value> = std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| std::fs::read(entry.path().join("status.json")).ok())
                .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
                .collect();
            if let Some(status) = states.iter().find(|status| {
                status["stage"] == stage
                    && status["model"] == model
                    && turn_id.is_none_or(|turn_id| status["turnId"] == turn_id)
            }) {
                assert_eq!(status["schemaVersion"], 2);
                assert!(
                    status.get("executionVerified").is_none(),
                    "schema v1 field must be absent"
                );
                assert_eq!(
                    status["serverModelReported"], false,
                    "normal lifecycle requires no server model reroute"
                );
                assert_eq!(status["observedModel"], serde_json::Value::Null);
                return status.clone();
            }
            assert!(
                Instant::now() < deadline,
                "pane status did not reflect {stage} {model} {turn_id:?}: {states:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn release_router_lifecycle(&self, label: &str, event: &str) {
        assert!(["A", "B", "C"].contains(&label));
        assert!(["ack", "started", "completed"].contains(&event));
        std::fs::write(
            self.router_marker(&format!("{label}.{event}")),
            b"release\n",
        )
        .expect("release one synthetic backend lifecycle event");
    }

    fn wait_for_router_audit_event(&self, event: &str, turn_id: &str) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if self
                .router_audit_rows()
                .iter()
                .any(|row| row["event"] == event && row["turnId"] == turn_id)
            {
                return;
            }
            assert!(Instant::now() < deadline, "missing {event} for {turn_id}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_router_rendered_lifecycle(&self, model: &str, effort: &str, label: &str) {
        let configuration = format!("{model} {effort}");
        // This synthetic Codex has no native model footer: routing is painted
        // on the pane's bottom border, never the launcher's lifecycle legend.
        let is_border = |line: &str| {
            matches!(
                line.trim_start().chars().next(),
                Some('▝' | '╰' | '└' | '╚')
            )
        };
        assert!(
            self.wait_until(STEP_TIMEOUT, |screen| screen.lines().any(|line| {
                is_border(line) && line.contains(&configuration) && line.contains(label)
            })),
            "expected {label:?} beside {configuration:?} on the rendered model line:\n{}",
            self.screen()
        );
        let screen = self.screen();
        let borders: Vec<_> = screen.lines().filter(|line| is_border(line)).collect();
        assert_eq!(
            borders.len(),
            1,
            "exactly one synthetic pane border: {screen}"
        );
        let line = borders[0];
        assert!(
            line.contains(&configuration) && line.contains(label),
            "{line}"
        );
        assert_eq!(
            screen.matches("Jev 선택 · 요청 ").count(),
            1,
            "the pane must not show duplicate routing labels: {screen}"
        );
        for other in ["요청 수락", "실행 중", "완료"] {
            if other != label {
                assert!(
                    !line.contains(other),
                    "conflicting lifecycle labels: {line}"
                );
            }
        }
        for obsolete in ["실행 모델 미확인", "실행 미확인", "완료·미확인"] {
            assert!(
                !line.contains(obsolete),
                "normal lifecycle is not unverified: {line}"
            );
        }
    }

    fn resume_router_fixture(&mut self) {
        self.start_into_main_list();
        self.send(b"/");
        self.expect("type to filter", STEP_TIMEOUT, "session search opened");
        self.send(b"alfa\r");
        self.expect(PANE_READY, STARTUP_TIMEOUT, "embedded router pane came up");
        let keyed = self.jev_key;
        self.expect(
            if keyed { PROMPT_JEV } else { PROMPT_NATIVE },
            STEP_TIMEOUT,
            "a session starts ON only when a Jev key loaded",
        );
        assert!(self.router_rows("ui-requests.jsonl").is_empty());
        // These scenarios begin from OFF, which one F6 reaches before any prompt is sent.
        if keyed {
            self.send(F6);
            self.expect(
                PROMPT_NATIVE,
                STEP_TIMEOUT,
                "F6 turns the default ON session OFF",
            );
        }
        assert!(!self.raw_output_contains("Jev 미선택"));
        assert!(!self.raw_output_contains("[F7]"));
        assert!(self.router_rows("ui-requests.jsonl").is_empty());
        self.embedded_router_bundle();
    }

    fn router_json(&self, name: &str) -> serde_json::Value {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Ok(contents) = std::fs::read_to_string(self.router_marker(name)) {
                if let Ok(value) = serde_json::from_str(&contents) {
                    return value;
                }
            }
            assert!(
                Instant::now() < deadline,
                "missing router JSON marker: {name}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_router_gateway_key_is_private(&self) {
        for name in ["ui.json", "backend-env.json"] {
            assert_eq!(
                self.router_json(name)["gatewayKeyPresent"],
                false,
                "gateway canary reached {name}"
            );
            assert_eq!(
                self.router_json(name)["typesafeKeyPresent"],
                false,
                "typesafe canary reached {name}"
            );
        }
    }

    fn embedded_router_bundle(&self) -> PathBuf {
        let runtime = self._tmp.join("home/.mindplayer/runtime");
        let bundles: Vec<_> = std::fs::read_dir(runtime)
            .expect("embedded router runtime directory")
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("codex-router-")
            })
            .collect();
        assert_eq!(bundles.len(), 1, "one complete runtime bundle per fixture");
        let names: BTreeSet<_> = std::fs::read_dir(&bundles[0])
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let checkout =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/local-model-router");
        for name in names {
            let installed =
                std::fs::read(bundles[0].join(&name)).expect("read extracted binary router asset");
            let source = std::fs::read(checkout.join(&name)).expect("read checkout router asset");
            assert!(
                installed == source,
                "binary router asset differs from checkout: {name}"
            );
        }
        bundles.into_iter().next().unwrap()
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

    fn assert_no_startup_choice(&self) {
        assert!(
            !self.raw_output_contains(STARTUP_CHOICE),
            "startup routing must never render"
        );
        for label in ["Jev 사용 (Codex)", "CLI 그대로"] {
            assert!(
                !self
                    .screen()
                    .lines()
                    .take(3)
                    .any(|line| line.contains(label)),
                "run-local mode must not remain in the header: {label}"
            );
        }
    }

    fn choose_prompt(&mut self, jev: bool) {
        let mode = if jev { "jev" } else { "native" };
        let label = if jev { PROMPT_JEV } else { PROMPT_NATIVE };
        if self.router_json("composer-state.json")["mode"] != mode {
            self.send(F6);
        }
        self.expect(
            label,
            STEP_TIMEOUT,
            "session toggle matches the requested mode",
        );
        assert_eq!(self.router_json("composer-state.json")["mode"], mode);
    }

    fn submit_jev(&mut self, input: &[u8]) {
        self.choose_prompt(true);
        self.send(input);
        self.expect(PROMPT_JEV, STEP_TIMEOUT, "submission keeps the session ON");
    }

    fn quit_from_list(&mut self) {
        self.send(b"q");
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll natural MindPlayer quit") {
                assert!(status.success(), "list-focused q exits cleanly");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "list-focused q did not exit MindPlayer (pid {:?}, PTY {:?}, root {}).\n\
                 ---- screen ----\n{}\n----------------",
                self.child.process_id(),
                self._master.get_size(),
                self._tmp.display(),
                self.screen(),
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_no_session_child(&self) {
        assert_eq!(
            std::fs::read_dir(&self.codex_marker_dir).unwrap().count(),
            0,
            "startup must not launch a native session"
        );
        for marker in ["ui.pid", "private-ui", "routes.jsonl", "jev.jsonl"] {
            assert!(
                !self.router_marker(marker).exists(),
                "startup must not launch a routed UI or submit a turn: {marker}"
            );
        }
        assert!(
            self.claude_auth_events(None, "conversation").is_empty(),
            "a blocked launch must not start a Claude conversation"
        );
        assert!(!self.screen().contains(PANE_READY));
    }

    fn assert_native_without_router(&self) {
        for path in [
            "node-invocations.log",
            "home/.mindplayer/runtime",
            "home/.mindplayer/routing-status",
        ] {
            assert!(
                !self._tmp.join(path).exists(),
                "Native must not execute Node or create router artifacts: {path}"
            );
        }
        assert!(!self.router_marker("jev.jsonl").exists());
        assert!(!self.router_marker("routes.jsonl").exists());
    }

    fn native_launches(&self) -> Vec<u32> {
        std::fs::read_to_string(self._tmp.join("native-launches.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.parse().expect("fixture records child PIDs only"))
            .collect()
    }

    /// Complete only the existing scope/scan flow. The first real list is
    /// immediately usable; each Codex session starts with its own Jev OFF mode.
    fn start_into_main_list(&mut self) {
        self.expect("collect sessions", STARTUP_TIMEOUT, "scope-select screen");
        self.send(b"\x1b[B");
        self.expect("▶ global", STARTUP_TIMEOUT, "Global option selected");
        self.send(b"\r");
        self.expect("2 sessions", STARTUP_TIMEOUT, "scanned session count");
        self.expect("· global", STARTUP_TIMEOUT, "Global scope in footer");
        self.expect(TITLE_A, STARTUP_TIMEOUT, "main list (session A)");
        self.expect(TITLE_B, STARTUP_TIMEOUT, "main list (session B)");
        self.assert_no_startup_choice();
        self.assert_no_session_child();
    }
}

const IDLE_BORDER: vt100::Color = vt100::Color::Rgb(140, 148, 162);
const WORKING_BORDER: vt100::Color = vt100::Color::Rgb(240, 212, 120);
const COMPLETED_BORDER: vt100::Color = vt100::Color::Rgb(145, 217, 174);
const ENDED_BORDER: vt100::Color = vt100::Color::Rgb(174, 183, 198);

#[test]
fn startup_scope_scan_opens_the_list_without_a_routing_choice() {
    let mut mp = Mp::launch_with_size(29, 88);
    mp.start_into_main_list();
    for (rows, cols) in [(29, 88), (24, 40), (29, 52), (40, 140)] {
        mp.resize(rows, cols);
        mp.expect("· global", STEP_TIMEOUT, "scope footer survives resize");
        mp.assert_no_startup_choice();
        mp.assert_no_session_child();
    }
    mp.quit_from_list();
    mp.assert_native_without_router();
}

#[test]
fn completed_vercel_probe_does_not_recreate_fixture_after_cleanup() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();
    // The row is column-aligned, so compare it with its padding collapsed.
    let vercel_unavailable = |screen: &str| {
        screen.lines().any(|line| {
            line.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .starts_with("vercel — unavailable")
        })
    };
    assert!(
        mp.wait_until(STARTUP_TIMEOUT, vercel_unavailable),
        "the asynchronous Vercel probe completed: expected the vercel row to read unavailable.\n\
         ---- screen ----\n{}\n----------------",
        mp.screen()
    );
    assert!(
        mp._tmp.join("home/.mindplayer/omp-token-probe").exists(),
        "the actual probe invoked the isolated OMP token guard"
    );
    let path = mp._tmp.clone();
    drop(mp);
    assert!(!path.exists(), "fixture directory was removed");
    std::thread::sleep(Duration::from_secs(1));
    assert!(!path.exists(), "the completed probe recreated the fixture");
}

fn choose_omp(mp: &mut Mp) {
    mp.start_into_main_list();
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "open new-session picker");
    for _ in 0..8 {
        if mp.screen().contains("▶ omp") {
            return;
        }
        mp.send(b"\x1b[B");
        let _ = mp.wait_until(Duration::from_millis(200), |screen| {
            screen.contains("▶ omp")
        });
    }
    panic!(
        "OMP was not offered in the new-session picker: {}",
        mp.screen()
    );
}

#[test]
fn new_session_starts_omp_in_the_selected_directory() {
    let mut mp = Mp::launch_with_options(40, 120, false, false, false, false);
    let marker = mp._tmp.join("omp-arguments");
    write_omp_script(
        &mp._tmp.join("bin/omp"),
        &format!(
            "printf '%s\\n' \"$PWD\" > '{}'\nprintf '%s\\n' \"$@\" >> '{}'\nprintf 'ready\\n' > \"$MINDPLAYER_OMP_PICKER_READY\"\nprintf 'OMP-FIXTURE-READY\\n'\nexec cat\n",
            marker.display(), marker.display()
        ),
    );
    choose_omp(&mut mp);
    mp.capture_stable_ui_evidence("omp-picker");
    mp.resize(29, 52);
    mp.expect(
        "▶ omp",
        STEP_TIMEOUT,
        "OMP choice survives a narrow terminal",
    );
    mp.capture_ui_evidence_at_size("omp-picker-narrow", 29, 52);
    mp.resize(40, 120);
    mp.send(b"\r");
    mp.expect("Label / subject", STEP_TIMEOUT, "OMP label entry");
    mp.send(b"OMP fixture\r");
    mp.expect(
        "OMP-FIXTURE-READY",
        STARTUP_TIMEOUT,
        "actual OMP program launched",
    );
    let args = std::fs::read_to_string(marker).unwrap();
    let mut lines = args.lines();
    let cwd = lines.next().expect("OMP working directory");
    assert_eq!(
        std::fs::canonicalize(cwd).unwrap(),
        std::fs::canonicalize(mp._tmp.join("scope")).unwrap()
    );
    assert_eq!(lines.next(), Some("-e"));
    lines.next().expect("new-session model picker");
    assert_eq!(lines.next(), Some("--config"));
    let config = lines.next().expect("new-session config");
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "autoResume: false\n"
    );
    assert_eq!(lines.next(), None, "do not route OMP through Codex or Jev");
    mp.send(b"\x11");
    mp.quit_from_list();
}

// The offline child models the extension's two distinct notifications: the
// ready file means the chooser is installed, while selected means the chooser
// has closed successfully. It records bytes at the PTY boundary, not a
// reconstructed screen, so a prompt accidentally sent to the chooser is
// observable even when the chooser would have consumed it silently.
fn omp_handoff_fixture(cancel_first: bool) {
    let mut mp = Mp::launch_with_options(40, 120, false, false, false, false);
    let script = mp._tmp.join("omp-handoff-fixture.js");
    std::fs::write(
        &script,
        r#"const fs = require("fs");
const root = process.env.HOME + "/..";
const record = (name, text) => fs.appendFileSync(root + "/" + name, text);
fs.writeFileSync(root + "/omp-launch.json", JSON.stringify({
  cwd: process.cwd(), args: process.argv.slice(2),
  ready: process.env.MINDPLAYER_OMP_PICKER_READY,
  selected: process.env.MINDPLAYER_OMP_PICKER_SELECTED
}));
process.stdin.setRawMode(true);
process.stdin.resume();
let state = "picker";
let line = "";
fs.writeFileSync(process.env.MINDPLAYER_OMP_PICKER_READY, "ready\n");
process.stdout.write("OMP CHOOSE MODEL\r\n");
process.stdin.on("data", (data) => {
  for (const byte of data) {
    const ch = String.fromCharCode(byte);
    if (state === "selected") {
      record("omp-delivered", Buffer.from([byte]));
    } else {
      record("omp-before-selection", Buffer.from([byte]));
      if (state === "picker") {
        if (byte === 27) {
          state = "cancelled";
          line = "";
          process.stdout.write("OMP MODEL REQUIRED\r\n");
        } else if (byte === 13) {
          state = "selected";
          process.stdout.write("\x1b[?2004hOMP MODEL SELECTED\r\n> ");
          fs.writeFileSync(process.env.MINDPLAYER_OMP_PICKER_SELECTED, "selected\n");
        }
      } else if (byte === 13) {
        if (line === "/mindplayer-model") {
          state = "picker";
          process.stdout.write("OMP CHOOSE MODEL AGAIN\r\n");
        } else {
          process.stdout.write("OMP CHOOSE MODEL FIRST\r\n");
        }
        line = "";
      } else {
        line += ch;
      }
    }
  }
});
"#,
    )
    .unwrap();
    write_omp_script(
        &mp._tmp.join("bin/omp"),
        &format!("exec node '{}' \"$@\"\n", script.display()),
    );
    append_handoff_turn(&mp.codex_brvo_file);
    mp.start_into_main_list();
    mp.send(b"/brvo");
    mp.expect("type to filter", STEP_TIMEOUT, "source search");
    mp.send(b"\x1b");
    mp.expect_absent("type to filter", STEP_TIMEOUT, "source selected");
    mp.send(b"h");
    mp.expect("Handoff", STEP_TIMEOUT, "handoff target picker");
    mp.expect("omp", STEP_TIMEOUT, "OMP is a handoff target");
    for _ in 0..8 {
        if mp.screen().contains("▶ omp") {
            break;
        }
        mp.send(b"\x1b[B");
        let _ = mp.wait_until(Duration::from_millis(200), |s| s.contains("▶ omp"));
    }
    mp.expect("▶ omp", STEP_TIMEOUT, "select OMP handoff target");
    mp.send(b"\r");
    mp.expect(
        "OMP CHOOSE MODEL",
        STARTUP_TIMEOUT,
        "OMP chooser is interactive",
    );

    let launch: serde_json::Value =
        serde_json::from_slice(&std::fs::read(mp._tmp.join("omp-launch.json")).unwrap()).unwrap();
    assert_eq!(
        std::fs::canonicalize(launch["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(mp._tmp.join("scope")).unwrap()
    );
    let args = launch["args"].as_array().unwrap();
    assert_eq!(
        args.len(),
        4,
        "OMP must not use another provider's launcher"
    );
    assert_eq!(args[0], "-e");
    let extension = args[1].as_str().unwrap();
    assert!(
        std::fs::read_to_string(extension)
            .unwrap()
            .contains("mindplayer-model"),
        "the embedded model-picker extension is loaded"
    );
    assert_eq!(args[2], "--config");
    assert_eq!(
        std::fs::read_to_string(args[3].as_str().unwrap()).unwrap(),
        "autoResume: false\n"
    );
    assert!(launch["ready"].as_str().is_some());
    assert!(launch["selected"].as_str().is_some());

    // Search keys must reach the chooser rather than being buffered with the
    // initial handoff. Cancellation must keep the handoff pending.
    mp.send(b"fixture-search");
    assert!(mp.wait_until(STEP_TIMEOUT, |_| {
        std::fs::read_to_string(mp._tmp.join("omp-before-selection"))
            .unwrap_or_default()
            .contains("fixture-search")
    }));
    if cancel_first {
        mp.send(b"\x1b");
        mp.expect("OMP MODEL REQUIRED", STEP_TIMEOUT, "cancelled chooser");
        mp.send(b"unrelated input\r");
        mp.expect(
            "OMP CHOOSE MODEL FIRST",
            STEP_TIMEOUT,
            "pending input rejected",
        );
        mp.send(b"/mindplayer-model\r");
        mp.expect("OMP CHOOSE MODEL AGAIN", STEP_TIMEOUT, "retry chooser");
    }
    let before = std::fs::read_to_string(mp._tmp.join("omp-before-selection")).unwrap();
    assert!(!before.contains("handoff smoke request"));
    assert!(!before.contains("handoff smoke answer"));
    assert!(!mp._tmp.join("omp-delivered").exists());

    mp.send(b"\r");
    mp.expect(
        "OMP MODEL SELECTED",
        STEP_TIMEOUT,
        "chooser closes after selection",
    );
    mp.expect(
        "submitted initial context to",
        STEP_TIMEOUT,
        "handoff delivered after model selection",
    );
    let artifacts: Vec<_> = std::fs::read_dir(mp._tmp.join("handoffs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(artifacts.len(), 1);
    let artifact = std::fs::read_to_string(&artifacts[0]).unwrap();
    let context = artifact.split_once("\n\n---\n\n").unwrap().1.trim_end();
    assert!(context.contains("handoff smoke request"));
    assert!(context.contains("handoff smoke answer"));
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            std::fs::read_to_string(mp._tmp.join("omp-delivered"))
                .unwrap_or_default()
                .replace("\r\n", "\n")
                .contains(context)
        }),
        "complete original context reaches OMP"
    );
    // A subsequent key is a PTY ordering fence for the captured submission.
    mp.send(b"after-handoff");
    assert!(mp.wait_until(STEP_TIMEOUT, |_| {
        std::fs::read_to_string(mp._tmp.join("omp-delivered"))
            .unwrap_or_default()
            .contains("after-handoff")
    }));
    let delivered = std::fs::read_to_string(mp._tmp.join("omp-delivered"))
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(delivered.matches(context).count(), 1);
    assert_eq!(delivered.matches("\x1b[200~").count(), 1);
    assert_eq!(delivered.matches("\x1b[201~").count(), 1);
    assert!(
        delivered.contains("\x1b[201~\r") || delivered.contains("\x1b[201~\n"),
        "the bracketed paste is followed by its submit key"
    );
    assert!(delivered.contains("codex-brvo-0002"));
    assert!(delivered.contains(artifacts[0].to_string_lossy().as_ref()));
    assert!(!delivered.contains("unrelated input"));
    assert!(mp.native_launches().is_empty(), "no Codex pane launched");
    assert!(mp.router_rows("jev.jsonl").is_empty(), "no Jev inference");
    assert!(
        mp.router_rows("routes.jsonl").is_empty(),
        "no provider routing"
    );
    mp.send(b"\x11");
    mp.quit_from_list();
}

#[test]
fn omp_handoff_delivers_original_context_once_after_model_selection() {
    omp_handoff_fixture(false);
}

#[test]
fn omp_handoff_cancel_and_retry_preserves_original_context() {
    omp_handoff_fixture(true);
}

#[cfg(unix)]
#[test]
#[ignore = "requires MINDPLAYER_TEST_OMP_BINARY; never selects a model or sends inference"]
fn actual_omp_handoff_picker_can_cancel_and_retry_without_inference() {
    let executable = std::env::var_os("MINDPLAYER_TEST_OMP_BINARY")
        .map(PathBuf::from)
        .expect("set the absolute installed OMP executable");
    assert!(executable.is_absolute() && executable.is_file());
    let mut mp = Mp::launch_with_options(40, 120, false, false, false, false);
    // A synthetic key makes the model catalog available, but no inherited
    // credentials are passed and this test never selects a model.
    write_omp_script(
        &mp._tmp.join("bin/omp"),
        &format!(
            "exec env -i HOME=\"$HOME\" PATH=\"$PATH\" TERM=\"$TERM\" PI_CODING_AGENT_DIR=\"$PI_CODING_AGENT_DIR\" OPENAI_API_KEY=offline-fixture-key MINDPLAYER_OMP_PICKER_READY=\"$MINDPLAYER_OMP_PICKER_READY\" MINDPLAYER_OMP_PICKER_SELECTED=\"$MINDPLAYER_OMP_PICKER_SELECTED\" '{}' \"$@\"\n",
            executable.display()
        ),
    );
    std::fs::write(
        mp._tmp.join("omp/config.yml"),
        "startup:\n  quiet: true\n  setupWizard: false\n  showSplash: false\nspelling:\n  autocomplete: 'off'\n",
    )
    .unwrap();
    append_handoff_turn(&mp.codex_brvo_file);
    mp.start_into_main_list();
    mp.send(b"/brvo");
    mp.expect("type to filter", STEP_TIMEOUT, "source search");
    mp.send(b"\x1b");
    mp.expect_absent("type to filter", STEP_TIMEOUT, "source selected");
    mp.send(b"h");
    mp.expect("Handoff", STEP_TIMEOUT, "handoff target picker");
    mp.expect("omp", STEP_TIMEOUT, "OMP is a handoff target");
    for _ in 0..8 {
        if mp.screen().contains("▶ omp") {
            break;
        }
        mp.send(b"\x1b[B");
        let _ = mp.wait_until(Duration::from_millis(200), |s| s.contains("▶ omp"));
    }
    mp.expect("▶ omp", STEP_TIMEOUT, "select OMP handoff target");
    mp.send(b"\r");
    mp.expect(
        "Choose a model and provider",
        Duration::from_secs(45),
        "real OMP chooser",
    );
    mp.send(b"codex-mini");
    mp.expect(
        "openai/codex-mini-latest",
        STEP_TIMEOUT,
        "chooser receives search",
    );
    assert!(!mp.screen().contains("handoff smoke request"));
    assert!(!mp.screen().contains("handoff smoke answer"));
    mp.send(b"\x1b");
    mp.expect(
        "Model required",
        STEP_TIMEOUT,
        "cancelled choice remains required",
    );
    mp.send(b"blocked prompt\r");
    mp.expect(
        "Choose a model first",
        STEP_TIMEOUT,
        "ordinary prompt is blocked",
    );
    mp.send(b"/mindplayer-model\r");
    mp.expect(
        "Choose a model and provider",
        STEP_TIMEOUT,
        "retry real chooser",
    );
    assert!(!mp.screen().contains("handoff smoke request"));
    assert!(!mp.screen().contains("handoff smoke answer"));
    assert!(mp.native_launches().is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
    assert!(mp.router_rows("routes.jsonl").is_empty());
    // No model is selected, so no handoff can be sent to a provider.
    mp.send(b"\x11");
    mp.quit_from_list();
}

#[cfg(unix)]
#[test]
#[ignore = "requires MINDPLAYER_TEST_OMP_BINARY; provider transport is isolated to localhost"]
fn actual_omp_handoff_delivers_context_after_local_only_model_selection() {
    fn matching_texts<'a>(value: &'a serde_json::Value, needle: &str, found: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) if text.contains(needle) => found.push(text),
            serde_json::Value::Array(items) => {
                for item in items {
                    matching_texts(item, needle, found);
                }
            }
            serde_json::Value::Object(items) => {
                for item in items.values() {
                    matching_texts(item, needle, found);
                }
            }
            _ => {}
        }
    }

    let executable = std::env::var_os("MINDPLAYER_TEST_OMP_BINARY")
        .map(PathBuf::from)
        .expect("set the absolute installed OMP executable");
    assert!(executable.is_absolute() && executable.is_file());
    // A loopback-only provider endpoint receives the real request but cannot
    // send it to an inference service.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let mut mp = Mp::launch_with_options(40, 120, false, false, false, false);
    write_omp_script(
        &mp._tmp.join("bin/omp"),
        &format!(
            "exec env -i HOME=\"$HOME\" PATH=\"$PATH\" TERM=\"$TERM\" PI_CODING_AGENT_DIR=\"$PI_CODING_AGENT_DIR\" ANTHROPIC_API_KEY=fixture-key ANTHROPIC_BASE_URL='{}' MINDPLAYER_OMP_PICKER_READY=\"$MINDPLAYER_OMP_PICKER_READY\" MINDPLAYER_OMP_PICKER_SELECTED=\"$MINDPLAYER_OMP_PICKER_SELECTED\" '{}' \"$@\"\n",
            base_url,
            executable.display()
        ),
    );
    std::fs::write(
        mp._tmp.join("omp/config.yml"),
        "startup:\n  quiet: true\n  setupWizard: false\n  showSplash: false\nspelling:\n  autocomplete: 'off'\n",
    )
    .unwrap();
    append_handoff_turn(&mp.codex_brvo_file);
    mp.start_into_main_list();
    mp.send(b"/brvo");
    mp.expect("type to filter", STEP_TIMEOUT, "source search");
    mp.send(b"\x1b");
    mp.expect_absent("type to filter", STEP_TIMEOUT, "source selected");
    mp.send(b"h");
    mp.expect("Handoff", STEP_TIMEOUT, "handoff target picker");
    mp.expect("omp", STEP_TIMEOUT, "OMP is a handoff target");
    for _ in 0..8 {
        if mp.screen().contains("▶ omp") {
            break;
        }
        mp.send(b"\x1b[B");
        let _ = mp.wait_until(Duration::from_millis(200), |s| s.contains("▶ omp"));
    }
    mp.expect("▶ omp", STEP_TIMEOUT, "select OMP handoff target");
    mp.send(b"\r");
    mp.expect(
        "Choose a model and provider",
        Duration::from_secs(45),
        "real OMP chooser",
    );
    mp.send(b"claude-sonnet-4-5");
    mp.expect(
        "anthropic/claude-sonnet-4-5",
        STEP_TIMEOUT,
        "local-only Anthropic model is offered",
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "no provider request before model selection"
    );
    mp.send(b"\r");
    mp.expect(
        "submitted initial context to",
        STEP_TIMEOUT,
        "handoff submitted after selection",
    );
    let artifacts: Vec<_> = std::fs::read_dir(mp._tmp.join("handoffs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(artifacts.len(), 1);
    let artifact = std::fs::read_to_string(&artifacts[0]).unwrap();
    let context = artifact.split_once("\n\n---\n\n").unwrap().1.trim_end();
    assert!(context.contains("handoff smoke request"));
    assert!(context.contains("handoff smoke answer"));
    let connection = std::sync::Mutex::new(None);
    assert!(
        mp.wait_until(Duration::from_secs(20), |_| {
            if let Ok((stream, _)) = listener.accept() {
                *connection.lock().unwrap() = Some(stream);
                true
            } else {
                false
            }
        }),
        "the selected provider did not connect to the isolated loopback endpoint"
    );
    let mut connection = connection.into_inner().unwrap().unwrap();
    connection.set_nonblocking(false).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = Vec::new();
    let mut chunk = [0u8; 8192];
    let (body_start, body_len) = loop {
        let n = connection
            .read(&mut chunk)
            .expect("read localhost request headers");
        assert!(n > 0, "provider closed before request headers");
        request.extend_from_slice(&chunk[..n]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&request[..end]).unwrap();
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .expect("provider request has a content length");
            break (end + 4, length);
        }
    };
    while request.len() - body_start < body_len {
        let n = connection
            .read(&mut chunk)
            .expect("read localhost request body");
        assert!(n > 0, "provider closed before request body");
        request.extend_from_slice(&chunk[..n]);
    }
    let payload: serde_json::Value =
        serde_json::from_slice(&request[body_start..body_start + body_len]).unwrap();
    assert_eq!(payload["model"], "claude-sonnet-4-5");
    let mut matching = Vec::new();
    matching_texts(&payload["messages"], context, &mut matching);
    assert_eq!(matching.len(), 1, "handoff appears in exactly one message");
    assert_eq!(matching[0].matches(context).count(), 1);
    assert!(matching[0].contains("codex-brvo-0002"));
    assert!(matching[0].contains(artifacts[0].to_string_lossy().as_ref()));
    let body = r#"{"error":{"message":"offline fixture"}}"#;
    write!(
        connection,
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    drop(connection);
    assert!(mp.native_launches().is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
    assert!(mp.router_rows("routes.jsonl").is_empty());
    mp.send(b"\x11");
    mp.quit_from_list();
    drop(mp);
    drop(listener);
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicit installed OMP executable"]
fn actual_omp_opens_from_new_session_picker() {
    use std::os::unix::fs::symlink;

    let executable = std::env::var_os("MINDPLAYER_TEST_OMP_BINARY")
        .map(PathBuf::from)
        .expect("set the absolute installed OMP executable");
    assert!(executable.is_absolute() && executable.is_file());
    let mut mp = Mp::launch_with_options(40, 120, false, false, false, false);
    symlink(executable, mp._tmp.join("bin/omp-real")).unwrap();
    write_omp_script(
        &mp._tmp.join("bin/omp"),
        "exec \"$(dirname \"$0\")/omp-real\" \"$@\"\n",
    );
    std::fs::write(
        mp._tmp.join("omp/config.yml"),
        "startup:\n  quiet: true\n  setupWizard: false\n  showSplash: false\nspelling:\n  autocomplete: 'off'\n",
    )
    .unwrap();
    choose_omp(&mut mp);
    mp.send(b"\r");
    mp.expect("Label / subject", STEP_TIMEOUT, "OMP label entry");
    mp.send(b"OMP native smoke\r");
    mp.expect(
        "Choose a model and provider",
        Duration::from_secs(45),
        "real OMP provider picker",
    );
    mp.send(b"codex-mini");
    mp.expect(
        "openai/codex-mini-latest",
        STEP_TIMEOUT,
        "search actual model catalog",
    );
    mp.send(b"\x1b");
    mp.expect(
        "Model required",
        STEP_TIMEOUT,
        "cancel keeps selection required",
    );
    mp.send(b"blocked prompt\r");
    mp.expect(
        "Choose a model first",
        STEP_TIMEOUT,
        "ordinary input blocked after cancel",
    );
    mp.send(b"/mindplayer-model\r");
    mp.expect("Choose a model and provider", STEP_TIMEOUT, "retry picker");
    mp.send(b"codex-mini\r");
    mp.expect(
        "Selected openai/codex-mini-latest",
        STEP_TIMEOUT,
        "model selected",
    );
    mp.capture_stable_ui_evidence("omp-native-session");
    mp.send(b"\x11");
    mp.quit_from_list();
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicit installed OMP executable"]
fn actual_omp_resumes_discovered_session() {
    use std::os::unix::fs::symlink;

    let executable = std::env::var_os("MINDPLAYER_TEST_OMP_BINARY")
        .map(PathBuf::from)
        .expect("set the absolute installed OMP executable");
    assert!(executable.is_absolute() && executable.is_file());
    let mut mp = Mp::launch_with_options(40, 120, false, false, false, false);
    symlink(executable, mp._tmp.join("bin/omp-real")).unwrap();
    write_omp_script(
        &mp._tmp.join("bin/omp"),
        "exec \"$(dirname \"$0\")/omp-real\" \"$@\"\n",
    );
    std::fs::write(
        mp._tmp.join("omp/config.yml"),
        "startup:\n  quiet: true\n  setupWizard: false\n  showSplash: false\nspelling:\n  autocomplete: 'off'\n",
    )
    .unwrap();
    let bucket = mp._tmp.join("omp/sessions/fixture");
    std::fs::create_dir_all(&bucket).unwrap();
    let session = bucket.join("2026-01-01_0199ef00-0000-7000-8000-000000000001.jsonl");
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": "0199ef00-0000-7000-8000-000000000001",
        "timestamp": "2026-01-01T00:00:00Z", "cwd": mp._tmp.join("scope")
    });
    let message = serde_json::json!({
        "type": "message", "id": "a1b2c3d4", "parentId": null,
        "timestamp": "2026-01-01T00:00:01Z",
        "message": { "role": "user", "content": [{ "type": "text", "text": "OMP_RESUME_SENTINEL" }], "timestamp": 1767225601000i64 }
    });
    std::fs::write(&session, format!("{header}\n{message}\n")).unwrap();
    mp.expect("collect sessions", STARTUP_TIMEOUT, "scope selection");
    mp.send(b"\x1b[B\r");
    mp.expect("3 sessions", STARTUP_TIMEOUT, "OMP session is discovered");
    mp.expect("multi-select", STARTUP_TIMEOUT, "session list is ready");
    mp.send(b"/OMP_RESUME_SENTINEL");
    mp.expect("/OMP_RESUME_SENTINEL", STEP_TIMEOUT, "OMP search is active");
    mp.send(b"\r");
    mp.expect("no-model", Duration::from_secs(45), "native OMP composer");
    mp.expect(
        "OMP_RESUME_SENTINEL",
        Duration::from_secs(45),
        "native OMP resumes the recorded conversation",
    );
    mp.capture_stable_ui_evidence("omp-native-resume");
    mp.send(b"\x1bq");
    mp.quit_from_list();
}

/// Real Rust PTY + embedded router, with a synthetic native composer/backend.
/// The native lane separately proves private Codex composer/queue behavior.
#[test]
fn session_toggle_one_f6_judges_two_followups_until_f6_turns_off() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    let pid = std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap();
    mp.send(F6);
    mp.expect(PROMPT_JEV, STEP_TIMEOUT, "one F6 enables this session");
    for (text, jev, label, model, effort) in [
        ("analyze B", true, "B", "gpt-6-astra", "high"),
        ("A", true, "A", "gpt-6-luna", "low"),
        ("C", false, "C", "gpt-6-luna", "low"),
    ] {
        if !jev {
            mp.send(F6);
            mp.expect(PROMPT_NATIVE, STEP_TIMEOUT, "next F6 turns Jev OFF");
        }
        mp.send(text.as_bytes());
        mp.expect(&format!("> {text}"), STEP_TIMEOUT, "directly typed draft");
        mp.send(b"\r");
        let route = mp.wait_for_router_turn(label);
        assert_eq!(route["model"], model);
        assert_eq!(route["effort"], effort);
        assert_eq!(
            route["routingChoicePresent"], false,
            "local metadata stays private"
        );
        mp.wait_for_router_audit_event("turn-completed", &format!("fixture-turn-{label}"));
        if jev {
            mp.assert_router_jev_turn_once(label);
        }
        mp.expect(
            if jev { PROMPT_JEV } else { PROMPT_NATIVE },
            STEP_TIMEOUT,
            "submitted inputs retain the session toggle",
        );
    }
    assert_eq!(
        std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap(),
        pid
    );
    let requests: Vec<_> = mp
        .router_rows("ui-requests.jsonl")
        .into_iter()
        .filter(|row| row["method"] == "turn/start")
        .collect();
    let routes = mp.router_rows("routes.jsonl");
    assert_eq!(requests.len(), 3);
    let mut identities = BTreeSet::new();
    for ((request, route), mode) in requests.iter().zip(&routes).zip(["jev", "jev", "native"]) {
        identities.insert(
            request["params"]["clientUserMessageId"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        assert_eq!(
            request["params"]["mindplayerRoutingChoice"],
            serde_json::json!({ "mode": mode, "includeRecent": false })
        );
        assert_eq!(request["params"]["model"], "gpt-6-luna");
        assert_eq!(
            request["params"]["effort"], "low",
            "a prior Jev selection cannot rewrite native settings"
        );
        assert_eq!(route["requestId"], request["id"]);
        assert_eq!(
            route["clientUserMessageId"],
            request["params"]["clientUserMessageId"]
        );
        assert_eq!(route["params"]["input"], request["params"]["input"]);
        if mode == "native" {
            let mut expected = request["params"].clone();
            expected
                .as_object_mut()
                .unwrap()
                .remove("mindplayerRoutingChoice");
            expected["mindplayerRequireNewTurn"] = serde_json::json!(true);
            assert_eq!(
                route["params"], expected,
                "OFF preserves every incoming native parameter"
            );
        }
    }
    assert_eq!(
        identities.len(),
        3,
        "each input snapshots its own native identity"
    );
    mp.assert_router_mixed_totals(&["B", "A", "C"], 2);
    let skipped: Vec<_> = mp
        .router_audit_rows()
        .into_iter()
        .filter(|row| row["event"] == "route-skipped")
        .collect();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["requestId"], requests[2]["id"]);
    assert_eq!(skipped[0]["reason"], "prompt-native");
    assert_eq!(skipped[0]["model"], "gpt-6-luna");
    assert_eq!(skipped[0]["effort"], "low");
}

#[test]
fn session_toggle_off_enter_submits_native_without_f6_or_judgment() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    mp.send(b"A\r");
    let route = mp.wait_for_router_turn("A");
    assert_eq!(route["model"], "gpt-6-luna");
    assert_eq!(route["effort"], "low");
    assert_eq!(route["routingChoicePresent"], false);
    let sent = mp.router_rows("ui-requests.jsonl");
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["params"]["mindplayerRoutingChoice"],
        serde_json::json!({"mode": "native", "includeRecent": false})
    );
    assert_eq!(
        route["clientUserMessageId"],
        sent[0]["params"]["clientUserMessageId"]
    );
    mp.wait_for_router_audit_event("turn-completed", "fixture-turn-A");
    assert!(mp.router_rows("jev.jsonl").is_empty());
    assert!(!mp
        .router_audit_rows()
        .iter()
        .any(|row| row["event"] == "jev-started"));
    mp.expect(
        PROMPT_NATIVE,
        STEP_TIMEOUT,
        "OFF remains the session default",
    );
}

#[test]
fn session_toggle_f6_changes_mode_without_submitting_the_draft() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    mp.send(b"A");
    mp.choose_prompt(true);
    assert_eq!(mp.router_json("composer-state.json")["draft"], "A");
    mp.choose_prompt(false);
    assert_eq!(mp.router_json("composer-state.json")["draft"], "A");
    assert!(!mp.screen().contains("JEV-CONTEXT"));
    assert!(mp.router_rows("ui-requests.jsonl").is_empty());
    assert!(mp.router_rows("routes.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
}

fn session_toggle_f7_is_inert(jev: bool) {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    mp.choose_prompt(jev);
    mp.send(b"analyze B");
    mp.expect("> analyze B", STEP_TIMEOUT, "typed input before F7");
    let before = mp.router_json("composer-state.json");
    mp.send(F7);
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            mp.router_json("composer-state.json")["revision"]
                .as_u64()
                .unwrap()
                > before["revision"].as_u64().unwrap()
        }),
        "the synthetic child must actually consume F7"
    );
    let after = mp.router_json("composer-state.json");
    assert_eq!(after["draft"], before["draft"]);
    assert_eq!(after["mode"], before["mode"]);
    assert_eq!(
        after.as_object().unwrap().len(),
        3,
        "only revision, draft and mode remain"
    );
    assert!(!mp.raw_output_contains("JEV-CONTEXT"));
    assert!(!mp.raw_output_contains("[F7]"));
    assert!(!mp.router_marker("previews.jsonl").exists());
    assert!(mp.router_rows("ui-requests.jsonl").is_empty());
    assert!(mp.router_rows("routes.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
    mp.send(b"\r");
    let route = mp.wait_for_router_turn("B");
    assert_eq!(
        route["model"],
        if jev { "gpt-6-astra" } else { "gpt-6-luna" }
    );
    assert_eq!(route["effort"], if jev { "high" } else { "low" });
    if jev {
        mp.assert_router_jev_turn_once("B");
        let starts: Vec<_> = mp
            .router_rows("jev.jsonl")
            .into_iter()
            .filter(|row| row["event"] == "start")
            .collect();
        assert_eq!(starts.len(), 1);
        let snapshot = &starts[0]["state"];
        assert_eq!(snapshot["currentInput"], "analyze B");
        assert_eq!(
            snapshot["baseline"],
            serde_json::json!({"model": "gpt-6-luna", "effort": "low"})
        );
        assert_eq!(snapshot["recent"], serde_json::json!([]));
        assert_eq!(snapshot["contextKnown"], false);
        assert_eq!(snapshot["attachments"], false);
        assert_eq!(snapshot["truncated"], false);
        assert!(starts[0]["stateBytes"].as_u64().unwrap() <= 8192);
    } else {
        mp.wait_for_router_audit_event("turn-completed", "fixture-turn-B");
        assert!(mp.router_rows("jev.jsonl").is_empty());
    }
    let sent = mp.router_rows("ui-requests.jsonl");
    assert_eq!(sent.len(), 1, "F7 adds no local RPC or hidden request");
    assert_eq!(sent[0]["method"], "turn/start");
    assert_eq!(
        sent[0]["params"]["mindplayerRoutingChoice"],
        serde_json::json!({"mode": if jev { "jev" } else { "native" }, "includeRecent": false})
    );
    assert_eq!(
        route["clientUserMessageId"],
        sent[0]["params"]["clientUserMessageId"]
    );
    assert_eq!(route["routingChoicePresent"], false);
}

#[test]
fn session_toggle_f7_has_no_ui_or_rpc_effect_while_off() {
    session_toggle_f7_is_inert(false);
}

#[test]
fn session_toggle_f7_has_no_ui_or_rpc_effect_while_on() {
    session_toggle_f7_is_inert(true);
}

#[test]
fn session_toggle_draft_edits_preserve_on_until_submission() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    mp.choose_prompt(true);
    mp.send(b"discard this draft");
    mp.expect("> discard this draft", STEP_TIMEOUT, "draft before editing");
    mp.send(b"\x15analyze Bx\x7f");
    mp.expect(
        "> analyze B",
        STEP_TIMEOUT,
        "replacement after clear and backspace",
    );
    assert_eq!(mp.router_json("composer-state.json")["mode"], "jev");
    assert!(mp.router_rows("jev.jsonl").is_empty());
    mp.send(b"\r");
    let route = mp.wait_for_router_turn("B");
    assert_eq!(route["model"], "gpt-6-astra");
    assert_eq!(route["effort"], "high");
    mp.assert_router_jev_turn_once("B");
    mp.expect(
        PROMPT_JEV,
        STEP_TIMEOUT,
        "edited input leaves the session ON",
    );
    let sent = mp.router_rows("ui-requests.jsonl");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["params"]["input"][0]["text"], "analyze B");
    assert_eq!(
        sent[0]["params"]["mindplayerRoutingChoice"],
        serde_json::json!({"mode": "jev", "includeRecent": false})
    );
}

#[test]
fn session_toggle_followup_uses_the_actual_preceding_terminal_outcome() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::LifecycleRouter);
    mp.resume_router_fixture();
    mp.submit_jev(b"A\r");
    mp.wait_for_router_route("A", "gpt-6-luna");
    mp.release_router_lifecycle("A", "ack");
    mp.wait_for_router_audit_event("start-acknowledged", "fixture-turn-A");
    mp.release_router_lifecycle("A", "started");
    mp.wait_for_router_status("gpt-6-luna", "running", Some("fixture-turn-A"));

    // This synthetic child only holds a draft. The actual native suite proves
    // queue ownership; do not manufacture its queue or reviewed-token behavior.
    mp.send(b"analyze B");
    mp.expect("> analyze B", STEP_TIMEOUT, "followup draft remains local");
    assert_eq!(mp.router_json("composer-state.json")["mode"], "jev");
    assert_eq!(
        mp.router_rows("jev.jsonl")
            .iter()
            .filter(|row| row["event"] == "start")
            .count(),
        1
    );
    mp.release_router_lifecycle("A", "completed");
    mp.assert_router_jev_turn_once("A");
    mp.send(b"\r");
    mp.wait_for_router_route("B", "gpt-6-astra");
    for event in ["ack", "started", "completed"] {
        mp.release_router_lifecycle("B", event);
    }
    mp.assert_router_audit_turn_once("B");
    let requests = mp.router_rows("jev.jsonl");
    let start = requests
        .iter()
        .find(|row| row["event"] == "start" && row["label"] == "B")
        .unwrap();
    assert_eq!(start["state"]["currentInput"], "analyze B");
    assert_eq!(
        start["state"]["previousDecision"],
        serde_json::json!({
            "model": "gpt-6-luna", "effort": "low", "capability": "bounded_routine",
            "contextSufficient": 1, "highConsequence": 0, "turnOutcome": "completed"
        })
    );
    assert!(!mp.router_marker("previews.jsonl").exists());
    mp.assert_router_consecutive_totals(&["A", "B"]);
    mp.expect(
        PROMPT_JEV,
        STEP_TIMEOUT,
        "native terminal notifications keep ON",
    );
}

#[test]
fn session_toggle_separate_panes_keep_independent_modes() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    let first_pid = std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap();
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "list after hiding the first pane",
    );
    mp.send(b"/brvo\r");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| {
            screen.matches(PROMPT_JEV).count() == 1 && screen.matches(PROMPT_NATIVE).count() == 1
        }),
        "second pane must start at the default ON without taking the first pane's OFF: {}",
        mp.screen()
    );
    let second_pid = std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap();
    assert_ne!(first_pid, second_pid, "independent native child processes");
    mp.send(F6);
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| screen.matches(PROMPT_NATIVE).count()
            == 2),
        "disabling the focused pane must preserve the first pane: {}",
        mp.screen()
    );
    mp.send(F6);
    assert!(
        mp.wait_until(STEP_TIMEOUT, |screen| {
            screen.matches(PROMPT_JEV).count() == 1 && screen.matches(PROMPT_NATIVE).count() == 1
        }),
        "enabling the focused pane must leave the first pane OFF: {}",
        mp.screen()
    );
    assert!(mp.router_rows("ui-requests.jsonl").is_empty());
    assert!(mp.router_rows("routes.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
}

#[test]
fn session_toggle_bound_account_survives_split_and_zoom_at_40x120() {
    let mut mp = Mp::launch_with_options_and_fixture(
        40,
        120,
        false,
        false,
        true,
        false,
        CodexFixture::ActualRouter,
    );
    mp.resume_router_fixture();
    let bound_account = "codex · isolated";
    mp.expect(
        bound_account,
        STEP_TIMEOUT,
        "split pane identifies its bound account",
    );
    assert_eq!(
        mp.screen().matches(bound_account).count(),
        1,
        "account label is pane-bound"
    );
    mp.capture_stable_ui_evidence("split-native-off");
    mp.choose_prompt(true);
    mp.expect(
        bound_account,
        STEP_TIMEOUT,
        "split ON retains the bound account",
    );
    mp.capture_stable_ui_evidence("split-jev-on");
    mp.send(b"\x1a"); // Native outer-app Ctrl-Z zooms the same running pane.
    mp.expect("ZOOMED", STEP_TIMEOUT, "zoom the same running pane");
    mp.expect(
        bound_account,
        STEP_TIMEOUT,
        "zoom title identifies the same account",
    );
    mp.expect(PROMPT_JEV, STEP_TIMEOUT, "zoom keeps the native session ON");
    assert_eq!(mp.screen().matches(bound_account).count(), 1);
    mp.capture_stable_ui_evidence("zoom-jev-on");
    mp.choose_prompt(false);
    mp.expect(
        bound_account,
        STEP_TIMEOUT,
        "OFF retains the zoomed account label",
    );
    mp.capture_stable_ui_evidence("zoom-native-off");
    assert!(mp.router_rows("ui-requests.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
}

#[test]
fn session_toggle_reattach_preserves_the_same_child_draft_and_on_mode() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    let original = std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap();
    mp.send(b"A");
    mp.choose_prompt(true);
    mp.send(b"\x18");
    mp.expect("multi-select", STEP_TIMEOUT, "Ctrl-X returns to the list");
    mp.assert_no_startup_choice();
    assert!(mp.router_rows("routes.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
    mp.send(b"\r");
    mp.expect(
        "> A",
        STEP_TIMEOUT,
        "reattach retains the same unsubmitted draft",
    );
    mp.expect(
        PROMPT_JEV,
        STEP_TIMEOUT,
        "reattaching the same child preserves its session ON mode",
    );
    assert_eq!(
        std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap(),
        original
    );
    mp.send(b"\r");
    mp.wait_for_router_route("A", "gpt-6-luna");
    mp.assert_router_jev_turn_once("A");
    mp.expect(
        PROMPT_JEV,
        STEP_TIMEOUT,
        "follow-up keeps ON without another F6",
    );
}

#[test]
fn session_toggle_keyless_native_preserves_model_and_effort_without_judgment() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::KeylessRouter);
    mp.resume_router_fixture();
    mp.send(b"A");
    mp.choose_prompt(false);
    mp.send(b"\r");
    let route = mp.wait_for_router_turn("A");
    assert_eq!(route["model"], "gpt-6-luna");
    assert_eq!(route["effort"], "low");
    mp.wait_for_router_audit_event("turn-completed", "fixture-turn-A");
    assert!(mp.router_rows("jev.jsonl").is_empty());
    assert!(!mp
        .router_audit_rows()
        .iter()
        .any(|row| row["event"] == "jev-started"));
}

#[test]
fn session_toggle_missing_key_blocks_jev_without_retry_or_native_fallback() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::KeylessRouter);
    mp.resume_router_fixture();
    mp.submit_jev(b"analyze B\r");
    mp.expect(
        "PROMPT-ERROR",
        STEP_TIMEOUT,
        "missing key is visible in the actual pane",
    );
    let errors = mp.router_rows("ui-errors.jsonl");
    assert_eq!(errors.len(), 1);
    assert!(mp.router_rows("routes.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
    assert_eq!(
        mp.router_audit_rows()
            .iter()
            .filter(|row| row["event"] == "route-failed" && row["reason"] == "jev-key-unavailable")
            .count(),
        1
    );
}

#[test]
fn session_toggle_policy_denial_blocks_jev_without_transfer_or_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let mut mp = Mp::launch_actual_router_fixture();
    let policy = mp._tmp.join("home/.mindplayer/jev-policy.json");
    std::fs::write(
        &policy,
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "version": "smoke-denied", "profiles": [],
            "consent": { "externalPromptTransfer": false }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
    mp.resume_router_fixture();
    mp.submit_jev(b"analyze B\r");
    mp.expect(
        "PROMPT-ERROR",
        STEP_TIMEOUT,
        "policy denial is visible in the pane",
    );
    assert_eq!(mp.router_rows("ui-errors.jsonl").len(), 1);
    assert!(mp.router_rows("routes.jsonl").is_empty());
    assert!(mp.router_rows("jev.jsonl").is_empty());
    assert_eq!(
        mp.router_audit_rows()
            .iter()
            .filter(|row| row["event"] == "route-failed"
                && row["reason"] == "external-transfer-policy-unset")
            .count(),
        1
    );
}

#[test]
fn session_toggle_judge_failure_is_visible_without_retry_or_native_fallback() {
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    std::fs::write(mp.router_marker("jev-rate-limited"), "synthetic 429\n").unwrap();
    mp.submit_jev(b"analyze B\r");
    mp.expect(
        "Jev rate limit reached",
        STEP_TIMEOUT,
        "bounded judge failure is visible",
    );
    assert_eq!(mp.router_rows("ui-errors.jsonl").len(), 1);
    assert!(mp.router_rows("routes.jsonl").is_empty());
    let events = mp.router_rows("jev.jsonl");
    assert_eq!(
        events.iter().filter(|row| row["event"] == "start").count(),
        1
    );
    let responses: Vec<_> = events
        .iter()
        .filter(|row| row["event"] == "response")
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["status"], 429);
    let audit = mp.router_audit_rows();
    assert_eq!(
        audit
            .iter()
            .filter(|row| row["event"] == "route-failed" && row["reason"] == "jev-rate-limited")
            .count(),
        1
    );
    assert!(!audit.iter().any(|row| [
        "jev-judged",
        "route-requested",
        "route-skipped",
        "start-acknowledged"
    ]
    .contains(&row["event"].as_str().unwrap_or_default())));
    assert!(!mp.screen().contains("raw-upstream-secret-canary"));
}

#[test]
fn session_toggle_missing_private_ui_fails_visibly_without_native_fallback() {
    let mut mp = Mp::launch_actual_router_fixture();
    std::fs::remove_file(mp._tmp.join("home/.mindplayer/native-codex/active.json")).unwrap();
    mp.start_into_main_list();
    mp.send(b"/alfa\r");
    mp.expect(
        "MindPlayer 전용 Codex 설치 파일이 없습니다",
        STARTUP_TIMEOUT,
        "missing private UI is a bounded launch failure",
    );
    for marker in ["ui.pid", "private-ui", "routes.jsonl", "jev.jsonl"] {
        assert!(
            !mp.router_marker(marker).exists(),
            "no fallback child or transfer: {marker}"
        );
    }
    assert!(mp.native_launches().is_empty());
    assert!(!mp.screen().contains(PANE_READY));
}

#[test]
fn startup_list_q_exits_without_launching_a_session() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();
    mp.quit_from_list();
    mp.assert_no_session_child();
    mp.assert_native_without_router();
}

#[test]
fn codex_task_events_drive_correlated_status_colors() {
    let mut mp = Mp::launch_task_status_fixture();
    mp.resume_task_status_fixture();

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
    mp.resume_task_status_fixture();
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
    mp.resume_task_status_fixture();
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

/// SIGWINCH must not strand terminal input until another key produces readiness.
#[test]
fn resize_bursts_do_not_leave_typed_input_waiting_for_another_key() {
    let mut mp = Mp::launch();
    mp.start_into_main_list();
    mp.send(b"\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "echo pane is ready");

    for cycle in 0..16 {
        let token = format!("k{cycle:02}");
        for step in 0..8 {
            mp.resize(32 + step % 2, 88 + step % 3);
            if step == 3 {
                mp.send(token.as_bytes());
            }
        }
        // No later input or resize may rescue a missed readiness notification.
        mp.expect(
            &token,
            STEP_TIMEOUT,
            "input during resize must arrive unaided",
        );
    }

    // All of these ordinary key events share one read. A zero-timeout drain
    // on the Unix TTY backend must not strand parser-buffered events.
    mp.send(b"BATCH_1234567890");
    mp.expect(
        "BATCH_1234567890",
        STEP_TIMEOUT,
        "drain a multi-key input batch",
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

fn saved_codex_identity(mp: &Mp, name: &str) -> Option<String> {
    mindplayer_core::accounts::load_accounts(&mp._tmp.join("home"))
        .into_iter()
        .find(|account| {
            account.provider == mindplayer_core::session::Agent::Codex && account.name == name
        })
        .expect("selected Codex account remains registered")
        .codex_identity
}

fn assert_single_codex_resume(mp: &Mp, home: &Path, session: &str) {
    mp.wait_for_codex_home(home, STEP_TIMEOUT);
    mp.expect(PANE_READY, STEP_TIMEOUT, "preserved conversation started");
    mp.expect_absent("Accounts", STEP_TIMEOUT, "recovery closes Accounts");
    let markers: Vec<_> = std::fs::read_dir(&mp.codex_marker_dir)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(
        markers.len(),
        1,
        "only the requested conversation may start"
    );
    assert_eq!(mp.native_launches().len(), 1, "launch is one-shot");
    assert!(markers[0]
        .lines()
        .any(|line| line == format!("CODEX_HOME={}", home.display())));
    let args: Vec<_> = markers[0]
        .lines()
        .filter_map(|line| line.strip_prefix("ARG="))
        .collect();
    assert!(
        args.contains(&"resume"),
        "original resume command: {args:?}"
    );
    assert!(args.contains(&session), "original session id: {args:?}");
}

fn signed_out_login_resumes_exact_home(isolated: bool) {
    let mut mp = Mp::launch_auth_recovery_fixture();
    mp.start_into_main_list();
    let home = if isolated {
        isolated_codex_home(&mp)
    } else {
        mp._tmp.join("home/.codex")
    };
    let other_home = if isolated {
        mp._tmp.join("home/.codex")
    } else {
        isolated_codex_home(&mp)
    };
    let (name, search, session, email) = if isolated {
        (
            "isolated",
            b"alfa\r".as_slice(),
            "codex-alfa-0001",
            "two@example.com",
        )
    } else {
        (
            "default",
            b"brvo\r".as_slice(),
            "codex-brvo-0002",
            "one@example.com",
        )
    };
    let original_binding = saved_codex_identity(&mp, name);
    std::fs::write(home.join("signed-out"), "requires native login\n").unwrap();
    mp.send(b"/");
    mp.expect(
        "type to filter",
        STEP_TIMEOUT,
        "search selected account's rollout",
    );
    mp.send(search);
    mp.expect("Accounts", STEP_TIMEOUT, "signed-out resume opens Accounts");
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "native login recovery is offered",
    );
    mp.expect(
        &format!("▶ {name}"),
        STEP_TIMEOUT,
        "blocked account stays selected",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_codex_identity(&mp, name), original_binding);

    // This is the sole recovery input: success must freshly verify the exact
    // preserved launch without requiring Enter or a second resume.
    mp.send(b"l");
    mp.wait_for_codex_auth_events(&home, "login", 1);
    assert_single_codex_resume(&mp, &home, session);
    let logins = mp.codex_auth_events(&home, "login");
    assert_eq!(logins.len(), 1);
    assert_eq!(logins[0]["args"], serde_json::json!(["login"]));
    assert_eq!(logins[0]["cwd"], serde_json::json!(mp._tmp.join("scope")));
    let conversations = mp.codex_auth_events(&home, "conversation");
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0]["cwd"], logins[0]["cwd"]);
    assert_eq!(conversations[0]["pane"], session);
    // The native UI now connects through the local router before receiving
    // the preserved resume command. Check the complete consumer argv, not an
    // obsolete assumption that `resume` is the first process argument.
    let args = conversations[0]["args"].as_array().unwrap();
    let remote = args[1].as_str().expect("native loopback URL");
    assert!(remote
        .strip_prefix("ws://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0));
    let mut expected = vec![
        serde_json::json!("--remote"),
        serde_json::json!(remote),
        serde_json::json!("--remote-auth-token-env"),
        serde_json::json!("MINDPLAYER_LOCAL_ROUTER_TOKEN"),
        serde_json::json!("--cd"),
        serde_json::json!(mp._tmp.join("scope")),
        serde_json::json!("resume"),
        serde_json::json!(session),
    ];
    if isolated {
        expected.extend([
            serde_json::json!("-c"),
            serde_json::json!(format!(
                "projects={{\"{}\"={{trust_level=\"trusted\"}}}}",
                mp._tmp.join("scope").display()
            )),
        ]);
    }
    assert_eq!(args, &expected, "preserve the exact routed resume command");
    assert!(home.join("fixture-signed-in").is_file());
    assert!(!other_home.join("fixture-signed-in").exists());
    assert!(mp.codex_auth_events(&home, "logout").is_empty());
    assert!(mp.codex_auth_events(&other_home, "login").is_empty());
    assert!(mp.codex_auth_events(&other_home, "logout").is_empty());
    assert_eq!(
        saved_codex_identity(&mp, name),
        Some(fixture_codex_identity("fixture-account", email)),
        "the selected account retains its verified binding"
    );
    let required: Vec<_> = mp
        .codex_auth_events(&home, "account-request")
        .into_iter()
        .filter(|row| row["refresh"] == true)
        .collect();
    assert_eq!(
        required.len(),
        2,
        "pre-login failure and post-login exact-launch check; no redundant AfterLogin inspection"
    );
    assert!(required.iter().all(|row| row["id"] == 3));
    let responses: Vec<_> = mp
        .codex_auth_events(&home, "account-response")
        .into_iter()
        .filter(|row| row["refresh"] == true)
        .collect();
    assert_eq!(
        responses.len(),
        2,
        "both required reads must actually reply"
    );
    for (request, response) in required.iter().zip(&responses) {
        assert_eq!(response["pid"], request["pid"]);
        assert_eq!(response["id"], 3);
        assert!(request["at"].as_u64().unwrap() <= response["at"].as_u64().unwrap());
    }
    assert_ne!(
        required[0]["pid"], required[1]["pid"],
        "post-login identity must come from a fresh app-server"
    );
    assert_eq!(responses[0]["result"]["account"], serde_json::Value::Null);
    assert!(responses[0]["at"].as_u64().unwrap() <= logins[0]["at"].as_u64().unwrap());
    assert!(required[1]["at"].as_u64().unwrap() >= logins[0]["at"].as_u64().unwrap());
    assert_eq!(responses[1]["result"]["account"]["email"], email);
    assert_eq!(
        responses[1]["result"]["workspaceRouting"]["chatgptAccountId"],
        "fixture-account"
    );
    assert!(responses[1]["at"].as_u64().unwrap() <= conversations[0]["at"].as_u64().unwrap());
    mp.send(TYPED_TOKEN.as_bytes());
    mp.expect(
        TYPED_TOKEN,
        STEP_TIMEOUT,
        "recovered conversation owns keyboard focus",
    );
    println!(
        "{name}: exact home resumed once; native login used no logout\n{}",
        mp.screen()
    );
}

#[test]
fn codex_auth_signed_out_inherited_login_resumes_the_requested_session() {
    signed_out_login_resumes_exact_home(false);
}

#[test]
fn codex_auth_signed_out_isolated_login_resumes_the_requested_session() {
    signed_out_login_resumes_exact_home(true);
}

#[test]
fn codex_auth_mismatch_waits_for_readable_identity_confirmation_then_resumes() {
    let mut mp = Mp::launch_auth_recovery_fixture();
    mp.start_into_main_list();
    let home = isolated_codex_home(&mp);
    let original_binding = saved_codex_identity(&mp, "isolated");
    std::fs::write(home.join("account-email"), "actual@example.test\n").unwrap();
    std::fs::write(home.join("account-workspace"), "actual-workspace\n").unwrap();
    std::fs::write(home.join("quota-workspace"), "actual-workspace\n").unwrap();
    mp.send(b"/");
    mp.expect("type to filter", STEP_TIMEOUT, "search isolated rollout");
    mp.send(b"alfa\r");
    mp.expect("Accounts", STEP_TIMEOUT, "mismatched launch opens Accounts");
    mp.expect(
        "저장된 로그인과 다름",
        STEP_TIMEOUT,
        "saved binding mismatch explained",
    );
    mp.expect(
        "actual@example.test",
        STEP_TIMEOUT,
        "actual email is readable",
    );
    mp.expect(
        "actual-workspace",
        STEP_TIMEOUT,
        "actual workspace is readable",
    );
    mp.expect("v로 연결", STEP_TIMEOUT, "explicit confirmation offered");
    mp.assert_no_session_child();
    assert_eq!(saved_codex_identity(&mp, "isolated"), original_binding);
    let required_before = mp
        .codex_auth_events(&home, "account-request")
        .iter()
        .filter(|row| row["refresh"] == true)
        .count();

    mp.send(b"v");
    assert_single_codex_resume(&mp, &home, "codex-alfa-0001");
    assert_eq!(
        saved_codex_identity(&mp, "isolated"),
        Some(fixture_codex_identity(
            "actual-workspace",
            "actual@example.test"
        )),
        "bind the actual account and workspace rather than the old display label"
    );
    assert_eq!(
        mp.codex_auth_events(&home, "account-request")
            .iter()
            .filter(|row| row["refresh"] == true)
            .count(),
        required_before + 2,
        "confirmation refreshes the account and rechecks the launch"
    );
    assert!(mp.codex_auth_events(&home, "login").is_empty());
    assert!(mp.codex_auth_events(&home, "logout").is_empty());
    mp.send(TYPED_TOKEN.as_bytes());
    mp.expect(
        TYPED_TOKEN,
        STEP_TIMEOUT,
        "confirmed resume receives keyboard input",
    );
}

#[test]
fn codex_auth_confirmation_rechecks_an_identity_changed_since_inspection() {
    let mut mp = Mp::launch_auth_recovery_fixture();
    mp.start_into_main_list();
    let home = isolated_codex_home(&mp);
    let original_binding = saved_codex_identity(&mp, "isolated");
    std::fs::write(home.join("account-email"), "first@example.test\n").unwrap();
    mp.send(b"/");
    mp.expect("type to filter", STEP_TIMEOUT, "search isolated rollout");
    mp.send(b"alfa\r");
    mp.expect("Accounts", STEP_TIMEOUT, "mismatch requires confirmation");
    mp.expect(
        "first@example.test",
        STEP_TIMEOUT,
        "first actual identity shown",
    );

    std::fs::write(home.join("account-email"), "latest@example.test\n").unwrap();
    mp.send(b"v");
    mp.expect(
        "latest@example.test",
        STEP_TIMEOUT,
        "confirmation rereads the actual account",
    );
    mp.expect(
        "v로 연결",
        STEP_TIMEOUT,
        "changed identity needs new confirmation",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_codex_identity(&mp, "isolated"), original_binding);

    mp.send(b"v");
    assert_single_codex_resume(&mp, &home, "codex-alfa-0001");
    assert_eq!(
        saved_codex_identity(&mp, "isolated"),
        Some(fixture_codex_identity(
            "fixture-account",
            "latest@example.test"
        ))
    );
}

#[test]
fn codex_auth_signed_out_handoff_keeps_the_target_home_and_initial_context() {
    let mut mp = Mp::launch_auth_recovery_fixture();
    mp.start_into_main_list();
    let home = isolated_codex_home(&mp);
    std::fs::write(home.join("signed-out"), "requires native login\n").unwrap();
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
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "signed-out handoff opens Accounts",
    );
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "target home needs login",
    );
    mp.assert_no_session_child();
    mp.send(b"l");
    mp.wait_for_codex_auth_events(&home, "login", 1);
    mp.wait_for_codex_home(&home, STEP_TIMEOUT);
    mp.expect(PANE_READY, STEP_TIMEOUT, "original handoff target started");
    mp.expect_absent(
        "Accounts",
        STEP_TIMEOUT,
        "recovered handoff closes Accounts",
    );
    mp.expect(
        "handoff smoke request",
        STEP_TIMEOUT,
        "preserved initial user context delivered",
    );
    mp.expect(
        "handoff smoke answer",
        STEP_TIMEOUT,
        "preserved assistant context delivered",
    );
    assert_eq!(mp.native_launches().len(), 1);
    let markers: Vec<_> = std::fs::read_dir(&mp.codex_marker_dir)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(markers.len(), 1);
    assert!(markers[0].lines().all(|line| line != "ARG=resume"));
    assert!(mp.codex_auth_events(&home, "logout").is_empty());
}

#[test]
fn codex_auth_quota_first_waits_beyond_grace_for_the_required_account() {
    let mut mp = Mp::launch_auth_recovery_fixture();
    mp.start_into_main_list();
    let home = isolated_codex_home(&mp);
    // The fixture holds the required account reply while quota responds. A
    // release marker makes both the negative assertion and order deterministic.
    std::fs::write(home.join("hold-account-response"), "hold\n").unwrap();
    std::fs::write(home.join("legacy-workspace"), "quota fallback\n").unwrap();
    mp.send(b"/");
    mp.expect("type to filter", STEP_TIMEOUT, "search isolated rollout");
    mp.send(b"alfa\r");
    assert!(mp.wait_until(STEP_TIMEOUT, |_| mp
        .codex_auth_events(&home, "account-request")
        .iter()
        .any(|row| row["refresh"] == true)));
    let request = mp
        .codex_auth_events(&home, "account-request")
        .into_iter()
        .find(|row| row["refresh"] == true)
        .unwrap();
    let pid = request["pid"].clone();
    assert!(mp.wait_until(STEP_TIMEOUT, |_| mp
        .codex_auth_events(&home, "quota-response")
        .iter()
        .any(|row| row["pid"] == pid && row["id"] == 2)));
    let quota = mp
        .codex_auth_events(&home, "quota-response")
        .into_iter()
        .find(|row| row["pid"] == pid)
        .unwrap();
    let held = Instant::now();
    while held.elapsed() < Duration::from_millis(400) {
        mp.assert_no_session_child();
        assert!(
            !mp.screen().contains("Accounts"),
            "pending account is not a failed login"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // A real input/render round trip must work while the worker is waiting.
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "input stays responsive during auth check",
    );
    mp.assert_no_session_child();
    std::fs::remove_file(home.join("hold-account-response")).unwrap();
    mp.wait_for_codex_home(&home, STEP_TIMEOUT);
    mp.expect(
        "ctrl-x live (1)",
        STEP_TIMEOUT,
        "completed auth keeps the user's List focus",
    );
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "List remains visible after launch",
    );
    assert!(
        !mp.screen().contains(PANE_READY),
        "background launch must not replace List with its live pane\n{}",
        mp.screen()
    );
    mp.send(b"\x18");
    assert_single_codex_resume(&mp, &home, "codex-alfa-0001");
    let response = mp
        .codex_auth_events(&home, "account-response")
        .into_iter()
        .find(|row| row["pid"] == pid)
        .expect("required account eventually replied");
    assert!(response["at"].as_u64().unwrap() - quota["at"].as_u64().unwrap() > 250);
    assert_eq!(response["id"], 3);
    assert_eq!(
        saved_codex_identity(&mp, "isolated"),
        Some(fixture_codex_identity("fixture-account", "two@example.com")),
        "legacy account/read uses the quota workspace without losing required identity"
    );
}

fn claude_fixture_home(mp: &Mp, isolated: bool) -> PathBuf {
    if isolated {
        mp._tmp.join("home/.mindplayer/accounts/claude/slot-seven")
    } else {
        mp._tmp.join("home/.claude")
    }
}

fn saved_claude_identity(mp: &Mp, isolated: bool) -> Option<String> {
    let name = if isolated { "spruce7" } else { "cobalt" };
    mindplayer_core::accounts::load_accounts(&mp._tmp.join("home"))
        .into_iter()
        .find(|account| {
            account.provider == mindplayer_core::session::Agent::Claude && account.name == name
        })
        .expect("arbitrarily labeled Claude account remains registered")
        .claude_identity
}

fn request_claude_fixture_resume(mp: &mut Mp, isolated: bool) {
    mp.send(b"/");
    mp.expect(
        "type to filter",
        STEP_TIMEOUT,
        "search requested Claude session",
    );
    mp.send(if isolated { b"alfa\r" } else { b"brvo\r" });
}

fn replace_claude_profile(home: &Path, user: &str, email: &str, org: &str) {
    std::fs::write(
        home.join("fixture-profile.json"),
        serde_json::to_vec(&serde_json::json!({
            "account": { "uuid": user, "email": email },
            "organization": { "uuid": org }
        }))
        .unwrap(),
    )
    .expect("change only the required profile response, not saved native status");
}

fn assert_claude_launch_context(
    mp: &Mp,
    home: &Path,
    isolated: bool,
    cwd: &Path,
    conversation: &serde_json::Value,
) {
    let status = mp
        .claude_auth_events(Some(home), "status")
        .into_iter()
        .last()
        .expect("native status precedes conversation");
    let profile = mp
        .claude_auth_events(Some(home), "profile-response")
        .into_iter()
        .last()
        .expect("required live profile precedes conversation");
    assert_eq!(
        status["args"],
        serde_json::json!(["auth", "status", "--json"])
    );
    assert_eq!(status["executable"], conversation["executable"]);
    assert_eq!(
        conversation["executable"],
        mp._tmp.join("bin/claude").to_string_lossy().as_ref()
    );
    let mut events = mp.claude_auth_events(Some(home), "status");
    events.extend(mp.claude_auth_events(Some(home), "login"));
    events.extend([profile.clone(), conversation.clone()]);
    for event in &events {
        assert_eq!(event["home"], home.to_string_lossy().as_ref());
        assert_eq!(
            event["cwd"],
            cwd.to_string_lossy().as_ref(),
            "probe must use the pending command's actual cwd: {event}"
        );
        let env = &event["environment"];
        assert_eq!(env["HOME"], mp._tmp.join("home").to_string_lossy().as_ref());
        assert_eq!(
            env["CLAUDE_CONFIG_DIR"],
            if isolated {
                serde_json::json!(home)
            } else {
                serde_json::Value::Null
            },
            "native default's unset namespace is distinct from an explicit default path"
        );
        for key in [
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
            "CLAUDE_CODE_OAUTH_SCOPES",
            "ANTHROPIC_CUSTOM_HEADERS",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            assert!(
                env[key].is_null(),
                "ambient {key} leaked into selected account: {event}"
            );
        }
    }
    assert_eq!(status["environment"], conversation["environment"]);
    assert_eq!(profile["code"], 200);
    assert!(profile["at"].as_u64().unwrap() <= conversation["at"].as_u64().unwrap());
}

fn assert_single_claude_resume(mp: &Mp, isolated: bool) {
    let home = claude_fixture_home(mp, isolated);
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "preserved Claude conversation started",
    );
    mp.expect_absent(
        "Accounts",
        STEP_TIMEOUT,
        "confirmation returns to conversation",
    );
    let launches = mp.claude_auth_events(None, "conversation");
    assert_eq!(
        launches.len(),
        1,
        "only the requested conversation may start"
    );
    assert_eq!(
        launches[0]["args"],
        serde_json::json!([
            "--resume",
            if isolated {
                "claude-alfa-0001"
            } else {
                "claude-brvo-0002"
            }
        ]),
        "resume the exact original session, not a new conversation"
    );
    let cwd = mp._tmp.join(if isolated {
        "scope/session-cwd"
    } else {
        "scope"
    });
    assert_claude_launch_context(mp, &home, isolated, &cwd, &launches[0]);
}

fn claude_signed_out_login_resumes_exact_home(isolated: bool) {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let binding =
        saved_claude_identity(&mp, isolated).expect("fixture starts with a saved binding");
    let home = claude_fixture_home(&mp, isolated);
    let other_home = claude_fixture_home(&mp, !isolated);
    std::fs::write(home.join("signed-out"), "synthetic native login required\n").unwrap();
    request_claude_fixture_resume(&mut mp, isolated);
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "signed-out Claude resume opens Accounts",
    );
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "offer native selected-home login",
    );
    mp.expect(
        if isolated {
            "▶ spruce7"
        } else {
            "▶ cobalt"
        },
        STEP_TIMEOUT,
        "arbitrary label remains selected",
    );
    mp.assert_no_session_child();
    mp.send(b"l");
    mp.wait_for_claude_auth_events(&home, "login-exit", 1);
    assert_single_claude_resume(&mp, isolated);
    let logins = mp.claude_auth_events(Some(&home), "login");
    assert_eq!(logins.len(), 1);
    assert_eq!(logins[0]["args"], serde_json::json!(["auth", "login"]));
    assert_eq!(saved_claude_identity(&mp, isolated), Some(binding));
    assert!(home.join("fixture-signed-in").exists());
    assert!(!other_home.join("fixture-signed-in").exists());
    assert!(mp.claude_auth_events(Some(&other_home), "login").is_empty());
    assert!(mp.claude_auth_events(None, "logout").is_empty());
    let after_login_profiles = mp
        .claude_auth_events(Some(&home), "profile-response")
        .into_iter()
        .filter(|row| row["at"].as_u64().unwrap() >= logins[0]["at"].as_u64().unwrap())
        .count();
    assert_eq!(
        after_login_profiles, 1,
        "post-login continuation checks the exact held command, not a scope-wide inspection"
    );
    mp.send(format!("{TYPED_TOKEN}\r").as_bytes());
    mp.expect(
        TYPED_TOKEN,
        STEP_TIMEOUT,
        "recovered conversation receives keyboard focus",
    );
    println!(
        "synthetic native Claude login resumed the exact home once\n{}",
        mp.screen()
    );
}

#[test]
fn claude_auth_signed_out_inherited_login_resumes_the_requested_session() {
    claude_signed_out_login_resumes_exact_home(false);
}

#[test]
fn claude_auth_signed_out_isolated_login_resumes_the_requested_session() {
    claude_signed_out_login_resumes_exact_home(true);
}

#[test]
fn claude_auth_saved_logged_in_status_cannot_bypass_a_rejected_profile() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let binding = saved_claude_identity(&mp, true).unwrap();
    let home = claude_fixture_home(&mp, true);
    std::fs::write(home.join("profile-http-status"), "401\n").unwrap();
    request_claude_fixture_resume(&mut mp, true);
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "required profile failure blocks conversation",
    );
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "rejected credential requires login",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_claude_identity(&mp, true), Some(binding));
    assert_eq!(
        mp.claude_auth_events(Some(&home), "status").last().unwrap()["status"]["loggedIn"],
        true
    );
    assert_eq!(
        mp.claude_auth_events(Some(&home), "profile-response")
            .last()
            .unwrap()["code"],
        401
    );
    let reads = mp.claude_auth_events(Some(&home), "profile-response").len();
    mp.send(b"v");
    mp.wait_for_claude_auth_events(&home, "profile-response", reads + 1);
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "metadata inspection cannot authorize after 401",
    );
    mp.assert_no_session_child();
}

#[test]
fn claude_auth_mismatch_requires_confirmation_of_the_profile_identity() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let binding = saved_claude_identity(&mp, true).unwrap();
    let home = claude_fixture_home(&mp, true);
    seed_claude_status(&home, "actual@example.test", "actual-organization");
    replace_claude_profile(
        &home,
        "fixture-user-actual",
        "actual@example.test",
        "actual-organization",
    );
    request_claude_fixture_resume(&mut mp, true);
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "profile mismatch blocks original launch",
    );
    mp.expect(
        "actual@example.test",
        STEP_TIMEOUT,
        "show the actual profile email",
    );
    mp.expect(
        "actual-organization",
        STEP_TIMEOUT,
        "show required profile's actual organization",
    );
    mp.expect("v로 연결", STEP_TIMEOUT, "require explicit confirmation");
    mp.assert_no_session_child();
    assert_eq!(saved_claude_identity(&mp, true), Some(binding.clone()));
    mp.send(b"v");
    assert_single_claude_resume(&mp, true);
    assert_ne!(saved_claude_identity(&mp, true), Some(binding));
    assert_eq!(
        mp.claude_auth_events(Some(&home), "status").last().unwrap()["status"]["email"],
        "actual@example.test",
        "native metadata and required profile must agree"
    );
}

#[test]
fn claude_auth_confirmation_rechecks_a_profile_changed_since_inspection() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let binding = saved_claude_identity(&mp, true).unwrap();
    let home = claude_fixture_home(&mp, true);
    seed_claude_status(&home, "first@example.test", "first-organization");
    replace_claude_profile(
        &home,
        "fixture-user-first",
        "first@example.test",
        "first-organization",
    );
    request_claude_fixture_resume(&mut mp, true);
    mp.expect("Accounts", STEP_TIMEOUT, "profile mismatch opens Accounts");
    mp.expect(
        "first@example.test",
        STEP_TIMEOUT,
        "first profile identity shown",
    );
    seed_claude_status(&home, "latest@example.test", "latest-organization");
    replace_claude_profile(
        &home,
        "fixture-user-latest",
        "latest@example.test",
        "latest-organization",
    );
    mp.send(b"v");
    mp.expect(
        "latest@example.test",
        STEP_TIMEOUT,
        "confirmation reads the profile again",
    );
    mp.expect(
        "latest-organization",
        STEP_TIMEOUT,
        "latest organization is readable",
    );
    mp.expect(
        "v로 연결",
        STEP_TIMEOUT,
        "changed profile requires renewed confirmation",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_claude_identity(&mp, true), Some(binding.clone()));
    mp.send(b"v");
    assert_single_claude_resume(&mp, true);
    assert_ne!(saved_claude_identity(&mp, true), Some(binding));
}

#[test]
fn claude_auth_fresh_login_preserves_the_label_and_reserves_a_new_home() {
    let mut mp = Mp::launch_claude_auth_fixture();
    let account_root = mp._tmp.join("home/.mindplayer/accounts/claude");
    let occupied = account_root.join("claude-2");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(
        occupied.join(".credentials.json"),
        "occupied synthetic sentinel\n",
    )
    .unwrap();
    let originals: Vec<_> = [
        claude_fixture_home(&mp, false),
        claude_fixture_home(&mp, true),
        occupied,
    ]
    .into_iter()
    .map(|home| {
        let bytes = std::fs::read(home.join(".credentials.json")).unwrap();
        (home, bytes)
    })
    .collect();
    mp.start_into_main_list();
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "choose a fresh Claude login");
    mp.send(b"l");
    mp.expect(
        "Label / subject",
        STEP_TIMEOUT,
        "label the original request",
    );
    mp.send(b"MPCLAUDEFRESH\r");
    mp.expect(
        "미등록 로그인",
        STEP_TIMEOUT,
        "new login is not silently bound",
    );
    mp.expect(
        "fresh@example.test",
        STEP_TIMEOUT,
        "show the new login's actual email",
    );
    mp.expect(
        "fixture-org-fresh",
        STEP_TIMEOUT,
        "show the new login's organization",
    );
    mp.expect(
        "v로 연결",
        STEP_TIMEOUT,
        "require explicit fresh-home confirmation",
    );
    mp.assert_no_session_child();
    let accounts = mindplayer_core::accounts::load_accounts(&mp._tmp.join("home"));
    let fresh = accounts
        .iter()
        .find(|account| {
            account.provider == mindplayer_core::session::Agent::Claude
                && account.slot_path() == account_root.join("claude-3")
        })
        .expect("fresh login skips occupied and registered homes");
    let home = fresh.slot_path();
    assert_eq!(fresh.claude_identity, None);
    mp.wait_for_claude_auth_events(&home, "login-exit", 1);
    assert_eq!(
        mp.claude_auth_events(Some(&home), "login")[0]["args"],
        serde_json::json!(["auth", "login"])
    );
    for (original, bytes) in originals {
        assert_eq!(
            std::fs::read(original.join(".credentials.json")).unwrap(),
            bytes,
            "fresh native login must not copy or replace another home's credential"
        );
    }
    mp.send(b"v");
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "continue the original labeled new session",
    );
    mp.expect_absent("Accounts", STEP_TIMEOUT, "binding returns to conversation");
    let launches = mp.claude_auth_events(None, "conversation");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["pane"], "new:claude:1");
    assert_eq!(launches[0]["args"], serde_json::json!([]));
    assert_claude_launch_context(&mp, &home, true, &mp._tmp.join("scope"), &launches[0]);
    let saved = mindplayer_core::accounts::load_accounts(&mp._tmp.join("home"));
    assert!(saved
        .iter()
        .find(|account| account.slot_path() == home)
        .unwrap()
        .claude_identity
        .is_some());
    assert!(mp.claude_auth_events(None, "logout").is_empty());
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "return to List after fresh login",
    );
    mp.expect(
        "MPCLAUDEFRESH",
        STEP_TIMEOUT,
        "preserve the requested label through recovery",
    );
}

fn append_claude_turn(file: &Path, role: &str, text: &str) {
    let mut transcript = std::fs::OpenOptions::new().append(true).open(file).unwrap();
    writeln!(
        transcript,
        "{}",
        serde_json::json!({
            "type": role,
            "message": { "role": role, "content": text }
        })
    )
    .unwrap();
}

fn fixture_process_exists(pid: u32) -> bool {
    assert!(pid > 1, "fixture must record an exact child PID");
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .expect("check the exact fixture process")
        .status
        .success()
}

/// Real MindPlayer PTY and native launcher, with synthetic offline account
/// probes and a native child that records argv/environment and echoes stdin.
fn claude_launcher_preserves_context(entry: &str) {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let isolated = entry != "new";
    let home = claude_fixture_home(&mp, isolated);
    let cwd = mp._tmp.join(if entry == "resume" {
        "scope/session-cwd"
    } else {
        "scope"
    });
    let credentials = std::fs::read(home.join(".credentials.json")).unwrap();
    let binding = saved_claude_identity(&mp, isolated);
    match entry {
        "new" => {
            mp.send(b"n");
            mp.expect(
                "New session",
                STEP_TIMEOUT,
                "choose an existing Claude login",
            );
            mp.expect(
                "▶ claude · cobalt",
                STEP_TIMEOUT,
                "inherited Claude account selected",
            );
            mp.send(b"\r");
            mp.expect("Label / subject", STEP_TIMEOUT, "new Claude label prompt");
            mp.send(b"\r");
        }
        "resume" => request_claude_fixture_resume(&mut mp, true),
        "handoff" => {
            let source = mp
                ._tmp
                .join("claude")
                .join(cwd.to_string_lossy().replace(['/', '.'], "-"))
                .join("claude-brvo-0002.jsonl");
            append_claude_turn(&source, "user", "original Claude launch question");
            append_claude_turn(&source, "assistant", "original Claude launch answer");
            mp.send(b"/brvo");
            mp.expect("type to filter", STEP_TIMEOUT, "source search is open");
            mp.send(b"\x1b");
            mp.expect_absent("type to filter", STEP_TIMEOUT, "select the source session");
            mp.send(b"h");
            mp.expect("Handoff", STEP_TIMEOUT, "choose Claude handoff account");
            mp.send(b"\x1b[B");
            mp.expect("▶ spruce7", STEP_TIMEOUT, "isolated Claude handoff target");
            mp.send(b"\r");
        }
        _ => panic!("unknown Claude launcher scenario: {entry}"),
    }
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    mp.expect(PANE_READY, STEP_TIMEOUT, "original Claude child is live");
    let launches = mp.claude_auth_events(None, "conversation");
    assert_eq!(launches.len(), 1, "one original Claude child for {entry}");
    let conversation = &launches[0];
    assert_eq!(
        conversation["pane"],
        match entry {
            "new" => "new:claude:1",
            "resume" => "claude-alfa-0001",
            "handoff" => "handoff:claude:claude:1",
            _ => unreachable!(),
        }
    );
    assert_claude_launch_context(&mp, &home, isolated, &cwd, conversation);
    assert_eq!(saved_claude_identity(&mp, isolated), binding);
    assert_eq!(
        std::fs::read(home.join(".credentials.json")).unwrap(),
        credentials
    );
    for event in [
        "login",
        "logout",
        "unexpected-command",
        "unexpected-transport",
    ] {
        assert!(
            mp.claude_auth_events(None, event).is_empty(),
            "unexpected {event}"
        );
    }
    let launch = &conversation["launch"];
    for (key, present) in launch["privateEnvironment"].as_object().unwrap() {
        assert_eq!(present, false, "{key} leaked to original Claude in {entry}");
    }
    let native_args: Vec<serde_json::Value> = if entry == "resume" {
        vec!["--resume".into(), "claude-alfa-0001".into()]
    } else {
        Vec::new()
    };
    let args = conversation["args"].as_array().unwrap();
    let native_pid = u32::try_from(conversation["pid"].as_u64().unwrap()).unwrap();
    assert!(
        fixture_process_exists(native_pid),
        "recorded Claude child is live"
    );
    assert_eq!(launch["brokerEnvironmentKeys"], serde_json::json!([]));
    assert_eq!(launch["pluginArgs"], serde_json::json!([]));
    assert_eq!(
        conversation["parentPid"],
        mp.child.process_id().unwrap(),
        "native Claude has no intervening Node Jev launcher"
    );
    mp.assert_native_without_router();
    assert!(
        !mp.screen().contains("Jev"),
        "Claude must not show any Jev selector or status"
    );
    assert_eq!(
        args, &native_args,
        "preserve the exact native {entry} arguments"
    );
    if entry == "handoff" {
        let artifacts: Vec<_> = std::fs::read_dir(mp._tmp.join("handoffs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(artifacts.len(), 1);
        let artifact = std::fs::read_to_string(&artifacts[0]).unwrap();
        let context = artifact.split_once("\n\n---\n\n").unwrap().1.trim_end();
        assert!(context.contains("original Claude launch question"));
        assert!(context.contains("original Claude launch answer"));
        assert!(
            mp.wait_until(STEP_TIMEOUT, |_| {
                let input = mp
                    .claude_auth_events(Some(&home), "input")
                    .iter()
                    .filter_map(|row| row["text"].as_str())
                    .collect::<String>()
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                input.contains(context)
                    && input.contains("claude-brvo-0002")
                    && input.contains(artifacts[0].to_string_lossy().as_ref())
            }),
            "complete initial context must reach the original Claude child"
        );
    }
    assert!(
        mp.router_rows("jev.jsonl").is_empty(),
        "launcher fixture executes no judgments"
    );
    claude_hide_then_quit_cleans_up(&mut mp, entry, native_pid);
}

fn claude_hide_then_quit_cleans_up(mp: &mut Mp, entry: &str, native_pid: u32) {
    mp.send(b"\x11");
    mp.expect(
        "closed live pane",
        STEP_TIMEOUT,
        "hide the actual Claude pane",
    );
    // Ctrl-Q retains its PTY. Only genuine application exit must end it.
    assert!(
        fixture_process_exists(native_pid),
        "Claude {entry} stays live while hidden"
    );
    mp.assert_native_without_router();
    let mindplayer_pid = mp
        .child
        .process_id()
        .expect("record the owned MindPlayer PID");
    mp.quit_from_list();
    assert!(
        !fixture_process_exists(mindplayer_pid),
        "natural app exit is reaped"
    );
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| !fixture_process_exists(native_pid)),
        "Claude {entry} native PID {native_pid} survived genuine q exit"
    );
    mp.assert_native_without_router();
}

#[test]
fn claude_native_function_keys_never_open_jev_or_transfer_a_prompt() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    request_claude_fixture_resume(&mut mp, true);
    let home = claude_fixture_home(&mp, true);
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "native Claude receives input directly",
    );
    let conversation = mp.claude_auth_events(Some(&home), "conversation")[0].clone();
    let pid = u32::try_from(conversation["pid"].as_u64().unwrap()).unwrap();
    for input in [F6, F7, b"first Claude prompt\r", b"second Claude prompt\r"] {
        mp.send(input);
    }
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            let input = mp
                .claude_auth_events(Some(&home), "input")
                .iter()
                .filter_map(|row| row["text"].as_str())
                .collect::<String>();
            input.contains("\x1b[17~")
                && input.contains("\x1b[18~")
                && input.contains("first Claude prompt")
                && input.contains("second Claude prompt")
        }),
        "F6/F7 and repeated prompt bytes reach the original native child"
    );
    assert_eq!(conversation["parentPid"], mp.child.process_id().unwrap());
    assert_eq!(
        conversation["launch"]["brokerEnvironmentKeys"],
        serde_json::json!([])
    );
    assert_eq!(conversation["launch"]["pluginArgs"], serde_json::json!([]));
    assert!(
        !mp.screen().contains("Jev"),
        "Claude displays no Jev selector, preview, or model status"
    );
    mp.assert_native_without_router();
    claude_hide_then_quit_cleans_up(&mut mp, "repeated direct input", pid);
}

#[test]
fn claude_native_new_session_bypasses_router_and_preserves_inherited_account() {
    claude_launcher_preserves_context("new");
}

#[test]
fn claude_native_resume_bypasses_router_and_preserves_selected_account_cwd() {
    claude_launcher_preserves_context("resume");
}

#[test]
fn claude_native_handoff_bypasses_router_and_preserves_initial_context() {
    claude_launcher_preserves_context("handoff");
}

#[test]
fn claude_auth_signed_out_handoff_preserves_the_original_initial_context() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, true);
    let source_cwd = mp._tmp.join("scope");
    let source = mp
        ._tmp
        .join("claude")
        .join(source_cwd.to_string_lossy().replace(['/', '.'], "-"))
        .join("claude-brvo-0002.jsonl");
    append_claude_turn(&source, "user", "original handoff question");
    append_claude_turn(&source, "assistant", "original handoff answer");
    std::fs::write(home.join("signed-out"), "synthetic native login required\n").unwrap();
    mp.send(b"/brvo\x1b");
    mp.expect_absent(
        "type to filter",
        STEP_TIMEOUT,
        "select the inherited source without resuming it",
    );
    mp.send(b"h");
    mp.expect("Handoff", STEP_TIMEOUT, "open Claude handoff targets");
    mp.send(b"\x1b[B");
    mp.expect(
        "▶ spruce7",
        STEP_TIMEOUT,
        "select the exact isolated target",
    );
    mp.send(b"\r");
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "hold handoff until native target login",
    );
    mp.expect("l로 이 홈에 로그인", STEP_TIMEOUT, "target requires login");
    mp.assert_no_session_child();
    let artifacts: Vec<_> = std::fs::read_dir(mp._tmp.join("handoffs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(artifacts.len(), 1);
    let artifact = std::fs::read_to_string(&artifacts[0]).unwrap();
    let original_context = artifact.split_once("\n\n---\n\n").unwrap().1.trim_end();
    assert!(original_context.contains("original handoff question"));
    assert!(original_context.contains("original handoff answer"));
    // Recovery must replay PendingSpawn's original input, not regenerate it.
    append_claude_turn(&source, "user", "late turn after auth was blocked");
    mp.send(b"l");
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            mp.claude_auth_events(Some(&home), "input")
                .iter()
                .filter_map(|row| row["text"].as_str())
                .collect::<String>()
                .contains("original handoff answer")
        }),
        "preserved handoff input was not delivered\n{}",
        mp.screen()
    );
    let launches = mp.claude_auth_events(None, "conversation");
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["pane"], "handoff:claude:claude:1");
    assert_eq!(launches[0]["args"], serde_json::json!([]));
    assert_claude_launch_context(&mp, &home, true, &source_cwd, &launches[0]);
    let input = mp
        .claude_auth_events(Some(&home), "input")
        .iter()
        .filter_map(|row| row["text"].as_str())
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    assert!(
        input.contains(original_context),
        "handoff must deliver the complete original transcript: {input}"
    );
    assert!(input.contains("claude-brvo-0002"));
    assert!(input.contains(artifacts[0].to_string_lossy().as_ref()));
    assert!(!input.contains("late turn after auth was blocked"));
    assert_eq!(std::fs::read_to_string(&artifacts[0]).unwrap(), artifact);
    assert_eq!(
        std::fs::read_dir(mp._tmp.join("handoffs")).unwrap().count(),
        1
    );
    assert!(mp.claude_auth_events(None, "logout").is_empty());
}

fn claude_unsuccessful_login_never_starts_conversation(cancel: bool) {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, true);
    let binding = saved_claude_identity(&mp, true);
    std::fs::write(home.join("signed-out"), "synthetic native login required\n").unwrap();
    std::fs::write(
        home.join(if cancel {
            "hold-login"
        } else {
            "login-exit-code"
        }),
        if cancel { "hold\n" } else { "17\n" },
    )
    .unwrap();
    request_claude_fixture_resume(&mut mp, true);
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "hold requested conversation for login",
    );
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "offer selected-home login",
    );
    mp.send(b"l");
    mp.wait_for_claude_auth_events(&home, "login", 1);
    if cancel {
        mp.send(b"\x03");
    }
    mp.wait_for_claude_auth_events(&home, "login-exit", 1);
    assert_eq!(
        mp.claude_auth_events(Some(&home), "login-exit")[0]["code"],
        if cancel { 130 } else { 17 }
    );
    mp.expect(
        "로그인 취소/실패",
        STEP_TIMEOUT,
        "failed or cancelled native login cannot release held launch",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_claude_identity(&mp, true), binding);
    assert!(!home.join("fixture-signed-in").exists());
    assert!(mp
        .claude_auth_events(Some(&home), "profile-request")
        .is_empty());
    assert!(mp.claude_auth_events(None, "logout").is_empty());
}

#[test]
fn claude_auth_failed_native_login_does_not_start_the_held_conversation() {
    claude_unsuccessful_login_never_starts_conversation(false);
}

#[test]
fn claude_auth_cancelled_native_login_does_not_start_the_held_conversation() {
    claude_unsuccessful_login_never_starts_conversation(true);
}

fn hidden_claude_login_preserves_resume_and_focus(list_focus: bool) {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, true);
    let other_home = claude_fixture_home(&mp, false);
    std::fs::write(home.join("signed-out"), "synthetic native login required\n").unwrap();
    std::fs::write(home.join("hold-login"), "hold\n").unwrap();
    request_claude_fixture_resume(&mut mp, true);
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "hold the original resume",
    );
    mp.send(b"l");
    mp.wait_for_claude_auth_events(&home, "login", 1);
    mp.send(b"\x11"); // Ctrl-Q hides the login without ending its PTY.
    mp.expect("closed pane", STEP_TIMEOUT, "hide the running login pane");
    mp.send(b"\x18");
    mp.expect("multi-select", STEP_TIMEOUT, "return to the session list");
    request_claude_fixture_resume(&mut mp, false);
    mp.wait_for_claude_auth_events(&other_home, "conversation", 1);
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "another account owns the focused pane",
    );
    if list_focus {
        mp.send(b"\x18");
        mp.expect(
            "multi-select",
            STEP_TIMEOUT,
            "leave the other pane for the list",
        );
    }

    assert!(mp.claude_auth_events(Some(&home), "login-exit").is_empty());
    assert!(mp
        .claude_auth_events(Some(&home), "conversation")
        .is_empty());
    std::fs::remove_file(home.join("hold-login")).unwrap();
    mp.wait_for_claude_auth_events(&home, "login-exit", 1);
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    let login = &mp.claude_auth_events(Some(&home), "login")[0];
    let exit = &mp.claude_auth_events(Some(&home), "login-exit")[0];
    assert_eq!(
        login["pid"], exit["pid"],
        "the original hidden PTY must finish"
    );
    assert_eq!(exit["code"], 0);
    let launch = &mp.claude_auth_events(Some(&home), "conversation")[0];
    assert_eq!(launch["pane"], "claude-alfa-0001");
    assert_eq!(
        launch["args"],
        serde_json::json!(["--resume", "claude-alfa-0001"])
    );
    assert_claude_launch_context(&mp, &home, true, &mp._tmp.join("scope/session-cwd"), launch);
    assert!(exit["at"].as_u64().unwrap() <= launch["at"].as_u64().unwrap());
    if list_focus {
        mp.expect(
            "multi-select",
            STEP_TIMEOUT,
            "hidden login completion keeps list focus",
        );
        assert!(!mp.screen().contains(PANE_READY), "{}", mp.screen());
        mp.send(b"\x18");
    }
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "return to the previously focused pane",
    );
    mp.send(b"still-the-other-pane\r");
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            mp.claude_auth_events(Some(&other_home), "input")
                .iter()
                .filter_map(|event| event["text"].as_str())
                .collect::<String>()
                .contains("still-the-other-pane")
        }),
        "successful background recovery stole terminal focus\n{}",
        mp.screen()
    );
    assert!(mp.claude_auth_events(Some(&home), "input").is_empty());
    let settled = Instant::now();
    while settled.elapsed() < Duration::from_millis(350) {
        assert_eq!(mp.claude_auth_events(Some(&home), "login").len(), 1);
        assert_eq!(mp.claude_auth_events(Some(&home), "conversation").len(), 1);
        assert_eq!(mp.claude_auth_events(None, "conversation").len(), 2);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn claude_auth_hidden_login_resumes_once_without_stealing_another_pane() {
    hidden_claude_login_preserves_resume_and_focus(false);
}

#[test]
fn claude_auth_hidden_login_resumes_once_without_stealing_list_focus() {
    hidden_claude_login_preserves_resume_and_focus(true);
}

#[test]
fn claude_auth_hidden_login_keeps_same_home_guard_until_exit() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, false);
    std::fs::write(home.join("signed-out"), "synthetic native login required\n").unwrap();
    std::fs::write(home.join("hold-login"), "hold\n").unwrap();
    request_claude_fixture_resume(&mut mp, false);
    mp.expect(
        "l로 이 홈에 로그인",
        STEP_TIMEOUT,
        "hold the original resume",
    );
    mp.send(b"l");
    mp.wait_for_claude_auth_events(&home, "login", 1);
    mp.send(b"\x11");
    mp.expect("closed pane", STEP_TIMEOUT, "hide the running login");
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "list remains usable during login",
    );
    let checks = mp.claude_auth_events(Some(&home), "status").len();
    mp.send(b"u\x1b[B\x1b[B\x1b[B");
    mp.expect(
        "▶ cobalt",
        STEP_TIMEOUT,
        "select the home whose login is hidden",
    );
    mp.send(b"vv");
    mp.expect(
        "로그인 진행 중",
        STEP_TIMEOUT,
        "hidden login still owns this home",
    );
    let held = Instant::now();
    while held.elapsed() < Duration::from_millis(350) {
        assert_eq!(mp.claude_auth_events(Some(&home), "login").len(), 1);
        assert_eq!(mp.claude_auth_events(Some(&home), "status").len(), checks);
        assert!(mp.claude_auth_events(Some(&home), "login-exit").is_empty());
        mp.assert_no_session_child();
        std::thread::sleep(Duration::from_millis(20));
    }
    mp.send(b"\x1b");
    mp.expect_absent(
        "Accounts",
        STEP_TIMEOUT,
        "dismiss accounts before login finishes",
    );
    std::fs::remove_file(home.join("hold-login")).unwrap();
    mp.wait_for_claude_auth_events(&home, "login-exit", 1);
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "the delayed resume preserves list focus",
    );
    assert_eq!(mp.claude_auth_events(Some(&home), "login").len(), 1);
    assert_eq!(mp.claude_auth_events(None, "conversation").len(), 1);
    assert_eq!(
        mp.claude_auth_events(Some(&home), "conversation")[0]["args"],
        serde_json::json!(["--resume", "claude-brvo-0002"])
    );
}

#[test]
fn claude_auth_delayed_profile_keeps_list_focus_until_explicitly_opened() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, true);
    std::fs::write(home.join("hold-profile-response"), "hold\n").unwrap();
    request_claude_fixture_resume(&mut mp, true);
    mp.wait_for_claude_auth_events(&home, "profile-request", 1);
    let held = Instant::now();
    while held.elapsed() < Duration::from_millis(400) {
        mp.assert_no_session_child();
        std::thread::sleep(Duration::from_millis(20));
    }
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "input works while profile is pending",
    );
    mp.assert_no_session_child();
    std::fs::remove_file(home.join("hold-profile-response")).unwrap();
    mp.wait_for_claude_auth_events(&home, "conversation", 1);
    mp.expect(
        "ctrl-x live (1)",
        STEP_TIMEOUT,
        "native launch marker arrives without stealing focus",
    );
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "List stays visible after the profile completes",
    );
    assert!(
        !mp.screen().contains(PANE_READY),
        "background completion must preserve List focus\n{}",
        mp.screen()
    );
    mp.send(b"\x18");
    assert_single_claude_resume(&mp, true);
    let request = &mp.claude_auth_events(Some(&home), "profile-request")[0];
    let response = &mp.claude_auth_events(Some(&home), "profile-response")[0];
    assert!(response["at"].as_u64().unwrap() - request["at"].as_u64().unwrap() > 250);
}

#[test]
fn claude_auth_pending_cwd_settings_cannot_be_authorized_from_startup_scope() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, true);
    let binding = saved_claude_identity(&mp, true);
    let local = mp._tmp.join("scope/session-cwd/.claude");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(
        local.join("settings.local.json"),
        r#"{"env":{"ANTHROPIC_API_KEY":"synthetic-project-override"}}"#,
    )
    .unwrap();
    request_claude_fixture_resume(&mut mp, true);
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "pending cwd's settings override blocks launch",
    );
    mp.expect(
        "auth overrides in settings",
        STEP_TIMEOUT,
        "explain why verification is unavailable",
    );
    mp.assert_no_session_child();
    assert!(mp.claude_auth_events(Some(&home), "status").is_empty());
    assert!(mp
        .claude_auth_events(Some(&home), "profile-request")
        .is_empty());
    mp.send(b"v");
    mp.expect(
        "v로 다시 확인",
        STEP_TIMEOUT,
        "fresh inspection also uses pending cwd B, not scope A",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_claude_identity(&mp, true), binding);
}

#[test]
fn claude_auth_saved_metadata_disagreement_does_not_bind_the_live_profile() {
    let mut mp = Mp::launch_claude_auth_fixture();
    mp.start_into_main_list();
    let home = claude_fixture_home(&mp, true);
    let binding = saved_claude_identity(&mp, true);
    replace_claude_profile(
        &home,
        "different-user",
        "different@example.test",
        "different-org",
    );
    request_claude_fixture_resume(&mut mp, true);
    mp.expect(
        "Accounts",
        STEP_TIMEOUT,
        "status/profile disagreement fails closed",
    );
    mp.expect(
        "verification unavailable",
        STEP_TIMEOUT,
        "disagreement is not a valid account proof",
    );
    mp.assert_no_session_child();
    let reads = mp.claude_auth_events(Some(&home), "profile-response").len();
    mp.send(b"v");
    mp.wait_for_claude_auth_events(&home, "profile-response", reads + 1);
    mp.expect(
        "v로 다시 확인",
        STEP_TIMEOUT,
        "confirmation cannot bind contradictory evidence",
    );
    mp.assert_no_session_child();
    assert_eq!(saved_claude_identity(&mp, true), binding);
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
fn fresh_login_never_reuses_an_occupied_credential_home() {
    let mut mp = Mp::launch_with_options_and_fixture(
        ROWS,
        COLS,
        false,
        false,
        false,
        false,
        CodexFixture::AuthRecovery,
    );
    let home = mp._tmp.join("home");
    let occupied = home.join(".mindplayer/accounts/codex/codex-2");
    let inherited = home.join(".codex");
    for dir in [&occupied, &inherited] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("auth.json"), FIXTURE_AUTH_SENTINEL).unwrap();
    }
    // Only this synthetic CLI completes login without a browser or credentials.
    // The labelled new session stays pending until the actual login is bound.
    mp.start_into_main_list();
    mp.send(b"n");
    mp.expect("New session", STEP_TIMEOUT, "open the account picker");
    mp.send(b"l");
    mp.expect("Label / subject", STEP_TIMEOUT, "choose a fresh login");
    mp.send(b"MPAUTHFRESH\r");
    mp.expect(
        "미등록 로그인",
        STEP_TIMEOUT,
        "fresh login displays the unbound actual account",
    );
    mp.expect(
        "two@example.com",
        STEP_TIMEOUT,
        "fresh login email is readable",
    );
    mp.expect(
        "fixture-account",
        STEP_TIMEOUT,
        "fresh login workspace is readable",
    );
    mp.expect(
        "v로 연결",
        STEP_TIMEOUT,
        "fresh login offers explicit binding",
    );

    let accounts = mindplayer_core::accounts::load_accounts(&home);
    let fresh = accounts
        .iter()
        .find(|account| {
            account.provider == mindplayer_core::session::Agent::Codex && !account.is_inherited()
        })
        .expect("fresh login registered");
    assert_ne!(
        fresh.slot_path(),
        occupied,
        "a new login must never reuse an existing credential home"
    );
    let expected = home.join(".mindplayer/accounts/codex/codex-3");
    assert_eq!(fresh.slot_path(), expected);
    assert_eq!(fresh.codex_identity, None);
    assert_eq!(
        std::fs::read_dir(mp._tmp.join("codex-markers"))
            .unwrap()
            .count(),
        0,
        "fresh login must not launch an unverified Codex session"
    );
    assert!(
        !expected.join("auth.json").exists(),
        "no credentials copied"
    );
    for dir in [&occupied, &inherited] {
        assert_eq!(
            std::fs::read_to_string(dir.join("auth.json")).unwrap(),
            FIXTURE_AUTH_SENTINEL,
            "an existing login must remain unchanged"
        );
    }
    mp.wait_for_codex_auth_events(&expected, "login", 1);
    assert_eq!(
        mp.codex_auth_events(&expected, "login")[0]["args"],
        serde_json::json!(["login"])
    );
    assert!(expected.join("fixture-signed-in").is_file());
    assert!(mp.codex_auth_events(&expected, "logout").is_empty());
    mp.send(b"v");
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "original labelled new session continues",
    );
    mp.expect_absent("Accounts", STEP_TIMEOUT, "binding returns to conversation");
    mp.wait_for_codex_home(&expected, STEP_TIMEOUT);
    assert_eq!(mp.native_launches().len(), 1);
    let marker = std::fs::read_to_string(mp.codex_marker_dir.join("new:codex:1.txt"))
        .expect("the original synthetic session id is preserved");
    assert!(marker.lines().all(|line| line != "ARG=resume"));
    assert_eq!(
        saved_codex_identity(&mp, &fresh.name),
        Some(fixture_codex_identity("fixture-account", "two@example.com"))
    );
    mp.send(b"\x18");
    mp.expect(
        "multi-select",
        STEP_TIMEOUT,
        "return to list after fresh login",
    );
    mp.expect(
        "MPAUTHFRESH",
        STEP_TIMEOUT,
        "requested subject survives auth recovery",
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
    mp.send(b"\r\r\r");
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
    mp.send(b"\r\r\r");
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
    mp.send(b"\x1b[B\r\r\r");
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
    mp.send(b"\x1b[B\r\r\r");
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
    mp.send(b"\r\r\r");
    mp.expect(PANE_READY, STEP_TIMEOUT, "default new-session pane came up");
    mp.wait_for_codex_home(&isolated_codex_home(&mp), STEP_TIMEOUT);
}

fn prepare_jev_fixture_credentials(
    home: &Path,
    destination: &Path,
    keys: &[(&str, &str)],
) -> std::process::Output {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/local-model-router/mindplayer-jev-fixture.mjs");
    let mut command = std::process::Command::new("node");
    command
        .arg(fixture)
        .arg("--prepare-credentials")
        .arg(destination)
        .env("HOME", home)
        .env_remove("NODE_OPTIONS")
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("AI_GATEWAY_API_KEY")
        .env_remove("VERCEL_OIDC_TOKEN");
    for (name, value) in keys {
        command.env(name, value);
    }
    command.output().expect("run credential fixture")
}

#[test]
fn jev_fixture_preserves_gateway_precedence_without_printing_credentials() {
    let tmp = unique_tmp();
    let destination = tmp.join("isolated/.mindplayer/jev-credentials.json");
    let output = prepare_jev_fixture_credentials(
        &tmp,
        &destination,
        &[
            ("TYPESAFE_API_KEY", "typesafe-selection-canary"),
            ("AI_GATEWAY_API_KEY", "gateway-selection-canary"),
        ],
    );
    assert!(output.status.success(), "credential staging succeeded");
    assert_eq!(output.stdout, b"{\"provider\":\"gateway\"}\n");
    assert!(output.stderr.is_empty());
    let credential: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&destination).unwrap()).unwrap();
    assert_eq!(credential["provider"], "gateway");
    assert_eq!(credential["apiKey"], "gateway-selection-canary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&destination, destination.parent().unwrap()] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}

#[test]
fn jev_fixture_uses_the_current_saved_provider_without_environment_keys() {
    let tmp = unique_tmp();
    let source = tmp.join(".mindplayer/jev-credentials.json");
    let prepared = prepare_jev_fixture_credentials(
        &tmp,
        &source,
        &[("TYPESAFE_API_KEY", "saved-typesafe-selection-canary")],
    );
    assert!(prepared.status.success(), "seed private saved credential");
    let destination = tmp.join("isolated/.mindplayer/jev-credentials.json");
    let output = prepare_jev_fixture_credentials(&tmp, &destination, &[]);
    assert!(output.status.success(), "load saved credential");
    assert_eq!(output.stdout, b"{\"provider\":\"typesafe\"}\n");
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read(source).unwrap(),
        std::fs::read(destination).unwrap()
    );
}

#[test]
fn jev_fixture_refuses_missing_credentials_without_creating_a_file() {
    let tmp = unique_tmp();
    let destination = tmp.join("isolated/.mindplayer/jev-credentials.json");
    let output = prepare_jev_fixture_credentials(&tmp, &destination, &[]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!destination.exists());
}

#[test]
fn resume_uses_embedded_router_protocol_and_cleans_up_remote_children() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::LifecycleRouter);
    mp.resume_router_fixture();

    let metadata = mp.router_json("ui.json");
    let private_ui = std::fs::read_to_string(mp._tmp.join("router-markers/private-ui"))
        .expect("the integrated pane launched its private Codex UI");
    assert!(Path::new(private_ui.trim())
        .starts_with(mp._tmp.join("home/.mindplayer/native-codex/builds")));
    let private_backend =
        std::fs::read_to_string(mp._tmp.join("router-markers/private-app-server"))
            .expect("the integrated backend used private Codex");
    assert_eq!(private_backend, private_ui);
    assert_eq!(metadata["cwd"], mp._tmp.join("scope").display().to_string());
    assert_eq!(
        metadata["codexHome"],
        isolated_codex_home(&mp).display().to_string()
    );
    let args = metadata["args"].as_array().expect("router UI args");
    assert!(args.iter().any(|arg| arg == "resume"));
    assert!(args.iter().any(|arg| arg == "codex-alfa-0001"));
    assert!(!args.iter().any(|arg| arg == "--model"));
    mp.assert_router_gateway_key_is_private();
    mp.embedded_router_bundle();

    for (input, label, model, effort) in [
        (b"A\r".as_slice(), "A", "gpt-6-luna", "low"),
        (b"analyze B\r".as_slice(), "B", "gpt-6-astra", "high"),
        (b"C\r".as_slice(), "C", "gpt-6-luna", "low"),
    ] {
        let turn_id = format!("fixture-turn-{label}");
        mp.submit_jev(input);
        mp.wait_for_router_route(label, model);
        mp.release_router_lifecycle(label, "ack");
        let accepted = mp.wait_for_router_status(model, "accepted", Some(&turn_id));
        assert_eq!(accepted["effort"], effort);
        mp.assert_router_rendered_lifecycle(model, effort, "요청 수락");
        assert!(!mp
            .router_audit_rows()
            .iter()
            .any(|row| row["turnId"] == turn_id
                && ["turn-started", "turn-completed"]
                    .contains(&row["event"].as_str().unwrap_or_default())));

        mp.release_router_lifecycle(label, "started");
        let running = mp.wait_for_router_status(model, "running", Some(&turn_id));
        assert_eq!(running["effort"], effort);
        mp.assert_router_rendered_lifecycle(model, effort, "실행 중");

        mp.release_router_lifecycle(label, "completed");
        mp.assert_router_jev_turn_once(label);
        let completed = mp.wait_for_router_status(model, "completed", Some(&turn_id));
        assert_eq!(completed["effort"], effort);
        mp.assert_router_rendered_lifecycle(model, effort, "완료");
    }
    mp.assert_router_consecutive_totals(&["A", "B", "C"]);
    assert_eq!(
        mp.router_audit_rows()
            .iter()
            .filter(|row| row["event"] == "turn-started")
            .count(),
        3
    );
    assert!(!mp
        .router_audit_rows()
        .iter()
        .any(|row| row["event"] == "model/rerouted"));
    let routes = std::fs::read_to_string(mp.router_marker("routes.jsonl")).unwrap();
    assert!(routes.contains("\"threadId\":\"codex-alfa-0001\""));
    assert!(routes.contains(&format!("\"cwd\":\"{}\"", mp._tmp.join("scope").display())));

    let proxy = std::fs::read_to_string(mp.router_marker("proxy-url")).unwrap();
    let port = proxy
        .trim()
        .rsplit(':')
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let ui_pid = std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap();
    let app_pid = std::fs::read_to_string(mp.router_marker("app-server.pid")).unwrap();
    let status_root = mp._tmp.join("home/.mindplayer/routing-status");
    let statuses: Vec<_> = std::fs::read_dir(&status_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(statuses.len(), 1, "one owned routing-status directory");
    mp.send(b"\x11");
    mp.expect(
        "closed live pane",
        STEP_TIMEOUT,
        "hide routed pane without ending its PTY",
    );
    assert!(fixture_process_exists(ui_pid.trim().parse().unwrap()));
    assert!(fixture_process_exists(app_pid.trim().parse().unwrap()));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
    assert!(
        statuses[0].is_dir(),
        "hidden pane retains its routing channel"
    );
    mp.quit_from_list();

    let deadline = Instant::now() + STEP_TIMEOUT;
    while Instant::now() < deadline {
        let ui_gone = !fixture_process_exists(ui_pid.trim().parse().unwrap());
        let app_gone = !fixture_process_exists(app_pid.trim().parse().unwrap());
        let proxy_closed = TcpStream::connect(("127.0.0.1", port)).is_err();
        if ui_gone && app_gone && proxy_closed && !statuses[0].exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "genuine q exit left router resources: UI {}, backend {}, port {}, status {}",
        ui_pid.trim(),
        app_pid.trim(),
        port,
        statuses[0].display()
    );
}

#[test]
fn embedded_router_jev_menu_excludes_legacy_models_before_current_family() {
    // MINDPLAYER_TEST_BINARY selects the installed executable for this same
    // regression. Both servers remain synthetic: this proves menu/wire
    // compatibility, not genuine model inference or native effort application.
    let mut mp = Mp::launch_actual_router_fixture();
    mp.resume_router_fixture();
    let catalog = mp.router_json("model-catalog.json");
    let ids: Vec<_> = catalog
        .as_array()
        .expect("synthetic served catalog")
        .iter()
        .map(|model| model["model"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "gpt-5.4",
            "gpt-5.3-codex",
            "gpt-6-luna",
            "gpt-6.1-sol",
            "gpt-6-astra"
        ],
        "legacy models must precede current models in the actual served catalog"
    );
    let expected: BTreeSet<_> = [
        ("gpt-6-luna", "low"),
        ("gpt-6.1-sol", "medium"),
        ("gpt-6-astra", "high"),
    ]
    .into_iter()
    .collect();
    for (input, label, model, effort) in [
        (b"A\r".as_slice(), "A", "gpt-6-luna", "low"),
        (b"analyze B\r".as_slice(), "B", "gpt-6-astra", "high"),
        (b"C\r".as_slice(), "C", "gpt-6-luna", "low"),
    ] {
        mp.submit_jev(input);
        let route = mp.wait_for_router_turn(label);
        assert_eq!(route["model"], model);
        assert_eq!(route["effort"], effort);
        mp.assert_router_jev_turn_once(label);
        mp.assert_router_status(model);
        let rows = mp.router_rows("jev.jsonl");
        let start = rows
            .iter()
            .find(|row| row["event"] == "start" && row["label"] == label)
            .expect("actual outbound Jev request metadata");
        let menu = start["configurationMenu"]
            .as_array()
            .expect("configuration choices offered to Jev");
        let actual: BTreeSet<_> = menu
            .iter()
            .map(|choice| {
                (
                    choice["model"].as_str().unwrap(),
                    choice["effort"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(menu.len(), expected.len(), "no duplicate menu choices");
        assert_eq!(
            actual, expected,
            "Jev sees only current-family choices for {label}"
        );
    }
    mp.assert_router_consecutive_totals(&["A", "B", "C"]);
}

#[test]
fn embedded_router_accepts_the_selected_terra_baseline() {
    // Runs the selected installed executable when MINDPLAYER_TEST_BINARY is
    // set. The model catalog and Jev response are isolated synthetic fixtures.
    let mut mp = Mp::launch_actual_router_fixture();
    std::fs::write(mp.router_marker("synthetic-terra-baseline"), b"opt-in\n").unwrap();
    mp.resume_router_fixture();
    mp.submit_jev(b"A\r");
    let route = mp.wait_for_router_turn("A");
    assert_eq!(route["model"], "gpt-6.1-terra");
    assert_eq!(route["effort"], "xhigh");
    mp.assert_router_jev_turn_once("A");
    let rows = mp.router_rows("jev.jsonl");
    let start = rows
        .iter()
        .find(|row| row["event"] == "start" && row["label"] == "A")
        .expect("synthetic Jev request");
    assert_eq!(start["state"]["baseline"]["model"], "gpt-6.1-terra");
    assert_eq!(start["state"]["baseline"]["effort"], "xhigh");
}

#[test]
fn embedded_router_excludes_capacity_failures_until_explicit_followup() {
    // The installed MINDPLAYER_TEST_BINARY runs in a real PTY. Both provider
    // responses are SYNTHETIC; this does not prove real inference or capacity.
    let mut mp = Mp::launch_actual_router_fixture();
    std::fs::write(mp.router_marker("synthetic-capacity-failure"), b"opt-in\n").unwrap();
    let status_root = mp._tmp.join("home/.mindplayer/routing-status");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mp.resume_router_fixture();
        mp.assert_router_gateway_key_is_private();
        mp.submit_jev(b"A\r");
        let first = mp.wait_for_router_turn("A");
        assert_eq!(first["model"], "gpt-6-luna");
        assert_eq!(first["effort"], "low");
        mp.wait_for_router_audit_event("start-acknowledged", "fixture-turn-A");
        mp.wait_for_router_audit_event("turn-completed", "fixture-turn-A");
        mp.wait_for_router_status("gpt-6-luna", "failed", Some("fixture-turn-A"));
        assert!(mp.wait_until(STEP_TIMEOUT, |_| mp
            .router_rows("capacity-events.jsonl")
            .len()
            == 2));
        let events = mp.router_rows("capacity-events.jsonl");
        assert_eq!(events[0]["method"], "error");
        assert_eq!(events[0]["params"]["threadId"], first["threadId"]);
        assert_eq!(events[0]["params"]["turnId"], "fixture-turn-A");
        assert_eq!(
            events[0]["params"]["error"]["codexErrorInfo"],
            "serverOverloaded"
        );
        assert_eq!(events[0]["params"]["willRetry"], false);
        assert_eq!(events[1]["method"], "turn/completed");
        assert_eq!(events[1]["params"]["turn"]["id"], "fixture-turn-A");
        assert_eq!(events[1]["params"]["turn"]["status"], "failed");
        // Terminal status and delivered events synchronize the next explicit
        // input. A fixed sleep would not establish native queue ownership.
        assert_eq!(
            mp.router_rows("routes.jsonl").len(),
            1,
            "no automatic resubmit after failure"
        );
        assert_eq!(mp.router_rows("ui-requests.jsonl").len(), 1);
        assert_eq!(mp.router_rows("jev.jsonl").len(), 2);

        for (input, label, model, effort) in [
            (b"C\r".as_slice(), "C", "gpt-6.1-sol", "medium"),
            (b"analyze B\r".as_slice(), "B", "gpt-6-astra", "high"),
        ] {
            mp.send(input);
            let route = mp.wait_for_router_turn(label);
            assert_eq!(route["model"], model);
            assert_eq!(route["effort"], effort);
            mp.wait_for_router_status(model, "completed", Some(&format!("fixture-turn-{label}")));
            mp.assert_router_audit_turn_once(label);
        }
        let judgments = mp.router_rows("jev.jsonl");
        let starts: Vec<_> = judgments
            .iter()
            .filter(|row| row["event"] == "start")
            .collect();
        assert_eq!(
            starts.len(),
            3,
            "exactly one judgment per explicit ON prompt"
        );
        for (index, start) in starts.iter().enumerate() {
            assert_eq!(start["label"], ["A", "C", "B"][index]);
            assert_eq!(start["configurationRubricMatches"], true);
            assert_eq!(start["questionsHaveRubric"], true);
            assert_eq!(start["capacityRecovery"], index > 0);
            assert_eq!(
                start["configurationInstructions"]
                    .as_str()
                    .unwrap()
                    .contains("CAPACITY_RECOVERY:"),
                index > 0
            );
            assert_eq!(
                start["stateKeys"],
                serde_json::json!([
                    "attachments",
                    "baseline",
                    "contextKnown",
                    "currentFamily",
                    "currentInput",
                    "previousDecision",
                    "recent",
                    "rubricVersion",
                    "schemaVersion",
                    "truncated"
                ])
            );
            let criteria: serde_json::Value =
                serde_json::from_str(start["configurationCriteriaJson"].as_str().unwrap()).unwrap();
            let menu = criteria
                .as_object()
                .expect("actual serialized Jev criteria");
            assert_eq!(menu.contains_key("abstain"), index > 0);
            if index > 0 {
                assert!(
                    menu["abstain"].is_string(),
                    "abstain has no model or effort"
                );
            }
            let candidates: Vec<_> = menu
                .iter()
                .filter(|(key, _)| key.as_str() != "abstain")
                .map(|(_, value)| value)
                .collect();
            let models: BTreeSet<_> = candidates
                .iter()
                .map(|choice| choice["model"].as_str().unwrap())
                .collect();
            let mut expected = BTreeSet::from(["gpt-6.1-sol", "gpt-6-astra"]);
            if index == 0 {
                expected.insert("gpt-6-luna");
            }
            assert_eq!(
                models, expected,
                "the failed model must stay absent from every later Jev menu"
            );
            assert_eq!(
                candidates.len(),
                expected.len(),
                "no duplicate model/effort candidates"
            );
            assert_eq!(menu.len(), expected.len() + usize::from(index > 0));
            let response = judgments
                .iter()
                .find(|row| row["event"] == "response" && row["label"] == start["label"])
                .unwrap();
            assert!(
                candidates.iter().any(|choice| choice["model"]
                    == response["configuration"]["model"]
                    && choice["effort"] == response["configuration"]["effort"]),
                "synthetic Jev must choose a supplied candidate"
            );
            let probabilities = response["configuration"]["probabilities"]
                .as_object()
                .unwrap();
            assert_eq!(
                probabilities.keys().collect::<BTreeSet<_>>(),
                menu.keys().collect::<BTreeSet<_>>(),
                "probabilities include every actual criterion, including abstain"
            );
        }

        mp.choose_prompt(false);
        mp.send(b"D\r");
        let native = mp.wait_for_router_turn("bootstrap");
        assert_eq!(native["params"]["input"][0]["text"], "D");
        assert_eq!(
            native["model"], "gpt-6-luna",
            "OFF preserves the native configuration"
        );
        assert_eq!(native["effort"], "low");
        mp.wait_for_router_audit_event("turn-completed", "fixture-turn-bootstrap");
        assert_eq!(
            mp.router_rows("jev.jsonl").len(),
            judgments.len(),
            "OFF never judges"
        );
        let sent = mp.router_rows("ui-requests.jsonl");
        let routes = mp.router_rows("routes.jsonl");
        assert_eq!(sent.len(), 4);
        assert_eq!(
            routes.len(),
            sent.len(),
            "no silent retry or ping-pong submission"
        );
        let identities: BTreeSet<_> = sent
            .iter()
            .map(|request| request["params"]["clientUserMessageId"].as_str().unwrap())
            .collect();
        assert_eq!(
            identities.len(),
            4,
            "every explicit input has its own identity"
        );
        for ((request, route), text) in sent.iter().zip(&routes).zip(["A", "C", "analyze B", "D"]) {
            assert_eq!(request["params"]["input"][0]["text"], text);
            assert_eq!(route["params"]["input"][0]["text"], text);
            assert_eq!(
                route["clientUserMessageId"],
                request["params"]["clientUserMessageId"]
            );
            assert_eq!(route["threadId"], first["threadId"]);
            assert_eq!(route["routingChoicePresent"], false);
        }
        let audit = mp.router_audit_rows();
        for event in ["start-acknowledged", "turn-completed"] {
            assert_eq!(audit.iter().filter(|row| row["event"] == event).count(), 4);
        }
        assert!(!mp
            ._tmp
            .join("home/.mindplayer/jev-credentials.json")
            .exists());
    }));
    let children: Vec<u32> = ["ui.pid", "app-server.pid"]
        .into_iter()
        .filter_map(|name| {
            std::fs::read_to_string(mp.router_marker(name))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
        .collect();
    let port = std::fs::read_to_string(mp.router_marker("proxy-url"))
        .ok()
        .and_then(|url| url.trim().rsplit(':').next()?.parse::<u16>().ok());
    mp.send(b"\x11");
    mp.expect(
        "closed live pane",
        STEP_TIMEOUT,
        "hide capacity fixture before natural quit",
    );
    mp.quit_from_list();
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            children.iter().all(|pid| !fixture_process_exists(*pid))
                && port.is_none_or(|port| TcpStream::connect(("127.0.0.1", port)).is_err())
                && std::fs::read_dir(&status_root)
                    .map_or(true, |mut entries| entries.next().is_none())
        }),
        "natural quit must clean up synthetic children, proxy, and owned status channel"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn embedded_router_late_ack_preserves_running_without_reroute() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::LifecycleRouter);
    mp.resume_router_fixture();
    mp.submit_jev(b"A\r");
    mp.wait_for_router_route("A", "gpt-6-luna");
    mp.release_router_lifecycle("A", "started");
    mp.wait_for_router_status("gpt-6-luna", "running", Some("fixture-turn-A"));
    mp.release_router_lifecycle("A", "ack");
    mp.wait_for_router_audit_event("start-acknowledged", "fixture-turn-A");
    mp.wait_for_router_status("gpt-6-luna", "running", Some("fixture-turn-A"));
    mp.assert_router_rendered_lifecycle("gpt-6-luna", "low", "실행 중");
    mp.release_router_lifecycle("A", "completed");
    mp.assert_router_jev_turn_once("A");
    mp.assert_router_consecutive_totals(&["A"]);
}

#[test]
fn embedded_router_status_tracks_the_owned_thread_after_resume() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::LifecycleRouter);
    mp.resume_router_fixture();
    append_codex_task_event(&mp.codex_alfa_file, "task_started", "old-thread-turn");
    mp.expect_foreground(WORKING_BORDER, STEP_TIMEOUT, "original thread is working");

    // Inject only the already-validated protocol's ownership snapshot. The real
    // binary must stop consuming the original file, then tail the current file
    // without replaying its historical completion. No live login is involved.
    let paths: Vec<_> = std::fs::read_dir(mp._tmp.join("home/.mindplayer/routing-status"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join("status.json"))
        .collect();
    assert_eq!(paths.len(), 1);
    let mut status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
    status["revision"] = serde_json::json!(status["revision"].as_u64().unwrap() + 1);
    status["threadId"] = serde_json::json!("codex-brvo-0002");
    status["requestId"] = serde_json::json!(1);
    status["turnId"] = serde_json::json!("current-thread-turn");
    status["stage"] = serde_json::json!("completed");
    std::fs::write(&paths[0], serde_json::to_vec(&status).unwrap()).unwrap();
    mp.expect_foreground(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "current thread completion supersedes the original thread's working reader",
    );
    // Allow the interval-gated reader to bind at EOF before appending a fresh
    // automatic turn; its native lifecycle must beat the old router snapshot.
    std::thread::sleep(Duration::from_millis(650));
    append_codex_task_event(
        &mp.codex_brvo_file,
        "task_started",
        "automatic-current-turn",
    );
    mp.expect_foreground(
        WORKING_BORDER,
        STEP_TIMEOUT,
        "current rollout starts a new turn",
    );
    append_codex_task_event(
        &mp.codex_brvo_file,
        "task_complete",
        "automatic-current-turn",
    );
    mp.expect_foreground(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "current rollout completes its own turn",
    );
}

#[test]
fn embedded_router_clear_preserves_the_label_live_pty_and_resume_target() {
    // Offline app-consumer proof: the actual binary/router run in PTYs, but
    // ownership metadata and native UI/backend are synthetic. Select the exact
    // installed consumer with MINDPLAYER_TEST_BINARY; native /clear is separate.
    let mut mp = Mp::launch_router_fixture(CodexFixture::KeylessRouter);
    let state_path = mp._tmp.join("state");
    // The shared fixture creates an empty directory at MINDPLAYER_STATE.
    // This scenario needs that exact isolated path to become a durable file.
    std::fs::remove_dir(&state_path).expect("remove empty fixture state directory");
    mp.start_into_main_list();
    let status_root = mp._tmp.join("home/.mindplayer/routing-status");
    let mut child_pids = Vec::new();
    let mut proxy_ports = Vec::new();
    // Preserve natural app/router cleanup even when the old installed consumer
    // fails the continuity assertion. Mp::drop alone kills the outer process.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        const OLD_ID: &str = "codex-alfa-0001";
        const NEW_ID: &str = "codex-clear-0003";
        const LABEL: &str = "MPCLEARCONTINUITY";
        // Recency can put BRVO first; anchor label editing to the actual ALFA
        // pane rather than assuming the first visible row's identity.
        mp.send(b"/alfa\r");
        mp.expect(
            PANE_READY,
            STARTUP_TIMEOUT,
            "resume the original fixture row",
        );
        mp.send(b"\x18");
        mp.expect(
            "multi-select",
            STEP_TIMEOUT,
            "return to the resumed row in the list",
        );
        mp.send(b"e");
        mp.expect(
            "Set label",
            STEP_TIMEOUT,
            "label the first native fixture row",
        );
        mp.send(format!("{LABEL}\r").as_bytes());
        mp.expect_absent("Set label", STEP_TIMEOUT, "save the fixture label");
        let saved = mindplayer_core::State::load_from(&state_path);
        assert_eq!(saved.label_for(OLD_ID), Some(LABEL));
        mp.send(format!("/{LABEL}\r").as_bytes());
        mp.expect(
            PANE_READY,
            STARTUP_TIMEOUT,
            "resume the labeled native fixture",
        );
        mp.expect(PROMPT_NATIVE, STEP_TIMEOUT, "offline session has Jev OFF");
        let original_ui = std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap();
        let original_backend = std::fs::read_to_string(mp.router_marker("app-server.pid")).unwrap();
        child_pids.push(original_ui.trim().parse::<u32>().unwrap());
        child_pids.push(original_backend.trim().parse::<u32>().unwrap());
        let port = std::fs::read_to_string(mp.router_marker("proxy-url"))
            .unwrap()
            .trim()
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .unwrap();
        proxy_ports.push(port);
        let original_args = mp.router_json("ui.json")["args"].clone();
        assert!(original_args
            .as_array()
            .unwrap()
            .windows(2)
            .any(|args| args == ["resume", OLD_ID]));
        let original_transcript = std::fs::read(&mp.codex_alfa_file).unwrap();
        let paths: Vec<_> = std::fs::read_dir(&status_root)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("status.json"))
            .collect();
        assert_eq!(paths.len(), 1, "one pane-owned status channel");
        let mut status: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
        assert_eq!(status["schemaVersion"], 2);
        assert_eq!(status["threadId"], OLD_ID);
        assert_eq!(status["stage"], "ready");
        status["revision"] = serde_json::json!(status["revision"].as_u64().unwrap() + 1);
        status["threadId"] = serde_json::json!(NEW_ID);
        status["clearAncestors"] = serde_json::json!([OLD_ID]);
        assert_eq!(status.as_object().unwrap().len(), 13);
        // Preserve the real generation and private file permissions, as in the
        // adjacent /resume ownership test. No native transcript is rewritten.
        std::fs::write(&paths[0], serde_json::to_vec(&status).unwrap()).unwrap();
        mp.send(b"\x11");
        mp.expect(
            "closed live pane",
            STEP_TIMEOUT,
            "hide the original live PTY",
        );
        // Deliberately delay the producer's first rollout beyond one status
        // polling interval. Only automatic discovery can find this new row.
        std::thread::sleep(Duration::from_millis(650));
        assert_eq!(
            mindplayer_core::State::load_from(&state_path).label_for(OLD_ID),
            Some(LABEL),
            "retain the old label until the target transcript exists"
        );
        let current_file = seed_codex_session(
            &isolated_codex_home(&mp).join("sessions"),
            &mp._tmp.join("scope"),
            NEW_ID,
            "MPCLEAREDTRANSCRIPT",
        );
        let current_transcript = std::fs::read(&current_file).unwrap();
        assert!(
            mp.wait_until(STEP_TIMEOUT, |_| {
                let saved = mindplayer_core::State::load_from(&state_path);
                saved.label_for(NEW_ID) == Some(LABEL) && saved.label_for(OLD_ID).is_none()
            }),
            "/clear must persist the label on {NEW_ID}, remove it from {OLD_ID}, and discover the late rollout without manual refresh; saved labels: {:?}",
            mindplayer_core::State::load_from(&state_path).labels
        );
        assert!(fixture_process_exists(child_pids[0]));
        assert!(fixture_process_exists(child_pids[1]));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        assert!(
            paths[0].is_file(),
            "hidden PTY retains its owned status channel"
        );
        mp.send(format!("/{LABEL}\r").as_bytes());
        mp.expect(
            PANE_READY,
            STEP_TIMEOUT,
            "reattach the cleared label's live PTY",
        );
        mp.send(b"A");
        mp.expect(
            "> A",
            STEP_TIMEOUT,
            "input still reaches the original native composer after clear and reattach",
        );
        assert_eq!(
            std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap(),
            original_ui,
            "clear and label reattach must not respawn the UI"
        );
        assert_eq!(
            std::fs::read_to_string(mp.router_marker("app-server.pid")).unwrap(),
            original_backend,
            "clear and label reattach must not respawn the backend"
        );
        assert_eq!(std::fs::read_dir(&status_root).unwrap().count(), 1);
        mp.send(b"\r");
        let input = mp.wait_for_router_turn("A");
        assert_eq!(input["params"]["input"][0]["text"], "A");
        mp.wait_for_router_audit_event("turn-completed", "fixture-turn-A");

        // End only the recorded synthetic UI, then resume by label. This
        // exercises a fresh native command without requiring a real login.
        assert!(std::process::Command::new("kill")
            .args(["-TERM", original_ui.trim()])
            .status()
            .unwrap()
            .success());
        mp.expect(
            "session ended",
            STEP_TIMEOUT,
            "fixture child has genuinely exited",
        );
        mp.send(b"\x11");
        mp.expect("closed live pane", STEP_TIMEOUT, "close the ended pane");
        mp.send(format!("/{LABEL}\r").as_bytes());
        assert!(
            mp.wait_until(STARTUP_TIMEOUT, |_| {
                let metadata = std::fs::read(mp.router_marker("ui.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
                metadata.as_ref().is_some_and(|metadata| {
                    metadata["args"]
                        .as_array()
                        .is_some_and(|args| args.windows(2).any(|args| args == ["resume", NEW_ID]))
                })
            }),
            "resuming the migrated label must launch the exact new native thread ID"
        );
        let metadata = mp.router_json("ui.json");
        assert_eq!(metadata["cwd"], mp._tmp.join("scope").display().to_string());
        assert_eq!(
            metadata["codexHome"],
            isolated_codex_home(&mp).display().to_string()
        );
        assert_ne!(
            std::fs::read_to_string(mp.router_marker("ui.pid")).unwrap(),
            original_ui
        );
        mp.send(b"C\r");
        assert_eq!(mp.wait_for_router_turn("C")["threadId"], NEW_ID);
        mp.wait_for_router_audit_event("turn-completed", "fixture-turn-C");
        assert!(mp.router_rows("jev.jsonl").is_empty());
        let saved = mindplayer_core::State::load_from(&state_path);
        assert_eq!(saved.label_for(NEW_ID), Some(LABEL));
        assert_eq!(saved.label_for(OLD_ID), None);
        assert_eq!(
            std::fs::read(&mp.codex_alfa_file).unwrap(),
            original_transcript
        );
        assert_eq!(std::fs::read(&current_file).unwrap(), current_transcript);
    }));
    for name in ["ui.pid", "app-server.pid"] {
        if let Ok(pid) = std::fs::read_to_string(mp.router_marker(name)) {
            child_pids.push(pid.trim().parse().unwrap());
        }
    }
    if let Ok(proxy) = std::fs::read_to_string(mp.router_marker("proxy-url")) {
        proxy_ports.push(proxy.trim().rsplit(':').next().unwrap().parse().unwrap());
    }
    for modal in ["Set label", "type to filter"] {
        if mp.screen().contains(modal) {
            mp.send(b"\x1b");
            mp.expect_absent(modal, STEP_TIMEOUT, "close fixture modal before cleanup");
        }
    }
    mp.send(b"\x11");
    mp.quit_from_list();
    assert!(
        mp.wait_until(STEP_TIMEOUT, |_| {
            child_pids.iter().all(|pid| !fixture_process_exists(*pid))
                && proxy_ports
                    .iter()
                    .all(|port| TcpStream::connect(("127.0.0.1", *port)).is_err())
                && std::fs::read_dir(&status_root)
                    .map_or(true, |mut entries| entries.next().is_none())
        }),
        "natural app quit must clean up the synthetic children, proxies, and owned status channels"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn embedded_router_late_ack_preserves_completed_without_reroute() {
    let mut mp = Mp::launch_router_fixture(CodexFixture::LifecycleRouter);
    mp.resume_router_fixture();
    mp.submit_jev(b"A\r");
    mp.wait_for_router_route("A", "gpt-6-luna");
    mp.release_router_lifecycle("A", "started");
    mp.wait_for_router_status("gpt-6-luna", "running", Some("fixture-turn-A"));
    mp.release_router_lifecycle("A", "completed");
    mp.wait_for_router_status("gpt-6-luna", "completed", Some("fixture-turn-A"));
    mp.release_router_lifecycle("A", "ack");
    mp.wait_for_router_audit_event("start-acknowledged", "fixture-turn-A");
    mp.wait_for_router_status("gpt-6-luna", "completed", Some("fixture-turn-A"));
    mp.assert_router_rendered_lifecycle("gpt-6-luna", "low", "완료");
    mp.expect_foreground(
        COMPLETED_BORDER,
        STEP_TIMEOUT,
        "correlated router completion makes the pane green without rollout tail events",
    );
    mp.assert_router_jev_turn_once("A");
    mp.assert_router_consecutive_totals(&["A"]);
}

/// Main runs this once against the newly installed executable. The ordinary
/// embedded tests always mock Jev, even if the opt-in flag is inherited.
#[test]
#[ignore = "two authenticated Jev calls; requires explicit installed-binary opt-in"]
fn installed_binary_uses_current_jev_provider_for_consecutive_turns() {
    assert_eq!(
        std::env::var("MINDPLAYER_TEST_REAL_JEV").as_deref(),
        Ok("1")
    );
    let binary = PathBuf::from(
        std::env::var_os("MINDPLAYER_TEST_BINARY")
            .expect("set the exact installed MINDPLAYER_TEST_BINARY"),
    );
    assert!(
        binary.is_absolute() && binary.is_file(),
        "installed executable must exist"
    );
    let binary = binary.canonicalize().expect("resolve installed executable");
    assert_ne!(
        binary,
        PathBuf::from(env!("CARGO_BIN_EXE_mindplayer"))
            .canonicalize()
            .expect("resolve Cargo binary"),
        "select the installed executable"
    );
    let version = std::process::Command::new(&binary)
        .arg("--version")
        .output()
        .expect("read installed executable version");
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).expect("installed version text");
    assert!(
        version
            .split_whitespace()
            .any(|part| part == env!("CARGO_PKG_VERSION")),
        "installed version must match this checkout"
    );
    let mut proof_file = std::env::var_os("MINDPLAYER_TEST_JEV_PROOF").map(|path| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .expect("create new private metadata-only proof before calling Jev")
    });
    let mut mp = Mp::launch_router_fixture(CodexFixture::RealJevRouter);
    mp.start_into_main_list();
    mp.send(b"/");
    mp.expect(
        "type to filter",
        STEP_TIMEOUT,
        "synthetic session search opened",
    );
    mp.send(b"alfa\r");
    mp.expect(PANE_READY, STARTUP_TIMEOUT, "installed router pane came up");
    // Fail stale installations before the first paid judgment.
    mp.embedded_router_bundle();
    mp.assert_router_gateway_key_is_private();
    for (text, jev, label) in [
        ("analyze B", true, "B"),
        ("A", true, "A"),
        ("C", false, "C"),
    ] {
        mp.send(text.as_bytes());
        mp.expect(
            &format!("> {text}"),
            STEP_TIMEOUT,
            "direct installed-fixture input",
        );
        mp.choose_prompt(jev);
        mp.send(b"\r");
        let route = mp.wait_for_router_turn(label);
        if jev {
            mp.assert_router_jev_turn_once(label);
        } else {
            assert_eq!(route["model"], "gpt-6-luna");
            assert_eq!(route["effort"], "low");
        }
        assert_eq!(route["routingChoicePresent"], false);
        mp.wait_for_router_audit_event("turn-completed", &format!("fixture-turn-{label}"));
        mp.assert_router_status(route["model"].as_str().unwrap());
        mp.expect(
            if jev { PROMPT_JEV } else { PROMPT_NATIVE },
            STEP_TIMEOUT,
            "installed fixture retains the session toggle after submission",
        );
    }
    mp.assert_router_mixed_totals(&["B", "A", "C"], 2);
    let responses: Vec<_> = mp
        .router_rows("jev.jsonl")
        .into_iter()
        .filter(|row| row["event"] == "response")
        .collect();
    let audit = mp.router_audit_rows();
    let proof = serde_json::json!({
        "schemaVersion": 1, "installedBinary": binary, "version": version.trim(),
        "provider": mp.jev_provider, "authenticatedJev": true, "syntheticTurns": 3,
        "authenticatedJudgments": 2, "promptModes": ["jev", "jev", "native"],
        "sessionToggleSticky": true, "nativeUiContract": "mindplayer-native-routing-v4",
        "codexInference": false, "automaticRetries": 0,
        "rubricVersion": "mindplayer-capacity-v2",
        "promptFirstContract": if responses.is_empty() { "service-audit" } else { "request-metadata" },
        "previousDecisionBound": [false, true],
        "responses": responses,
        "judgments": audit.iter().filter(|row| row["event"] == "jev-judged").collect::<Vec<_>>(),
        "acceptedConfigurations": mp.router_rows("routes.jsonl"),
        "auditRoot": mp._tmp.join("router-audits"), "markerDirectory": mp.router_marker_dir,
        "gatewayPhysicalVersionAttested": false
    });
    if let Some(file) = &mut proof_file {
        serde_json::to_writer_pretty(&mut *file, &proof).expect("write metadata-only proof");
        writeln!(file).unwrap();
    }
    println!("JEV_PROOF={proof}");
}

#[test]
fn new_resume_and_handoff_use_the_same_embedded_router_bundle() {
    let mut bundle_name = None;
    for entry in ["new", "resume", "handoff"] {
        let mut mp = Mp::launch_actual_router_fixture();
        mp.start_into_main_list();
        match entry {
            "new" => {
                mp.send(b"n");
                mp.expect("New session", STEP_TIMEOUT, "new-session picker opened");
                mp.send(b"\x1b[B");
                mp.expect(
                    "▶ codex · isolated",
                    STEP_TIMEOUT,
                    "isolated target selected",
                );
                mp.send(b"\r\r\r");
            }
            "resume" => {
                mp.send(b"/");
                mp.expect("type to filter", STEP_TIMEOUT, "session search opened");
                mp.send(b"alfa\r");
            }
            "handoff" => {
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
            }
            _ => unreachable!(),
        }
        if entry == "handoff" {
            // With a key the handoff's automatic first turn is a new session turn, so Jev judges it.
            mp.expect(
                "submitted initial context to",
                STEP_TIMEOUT,
                "handoff context was pasted before the test turn",
            );
            mp.expect(
                PROMPT_JEV,
                STEP_TIMEOUT,
                "handoff starts ON when a Jev key loaded",
            );
            let route = mp.wait_for_router_turn("bootstrap");
            mp.assert_router_jev_turn_once("bootstrap");
            let sent = mp.router_rows("ui-requests.jsonl");
            assert_eq!(sent.len(), 1);
            let context = sent[0]["params"]["input"][0]["text"]
                .as_str()
                .expect("original handoff context");
            assert!(context.contains("handoff smoke request"));
            assert!(context.contains("handoff smoke answer"));
            assert_eq!(
                sent[0]["params"]["mindplayerRoutingChoice"],
                serde_json::json!({"mode": "jev", "includeRecent": false})
            );
            assert_eq!(route["requestId"], sent[0]["id"]);
            assert_eq!(
                route["clientUserMessageId"],
                sent[0]["params"]["clientUserMessageId"]
            );
            mp.wait_for_router_audit_event("turn-completed", "fixture-turn-bootstrap");
            let composer = mp.router_json("composer-state.json");
            assert_eq!(composer["draft"], "");
            assert_eq!(composer["mode"], "jev");
        } else {
            mp.expect(PANE_READY, STARTUP_TIMEOUT, "embedded router pane came up");
            mp.expect(
                PROMPT_JEV,
                STEP_TIMEOUT,
                "new and resumed sessions start ON when a Jev key loaded",
            );
        }
        let metadata = mp.router_json("ui.json");
        assert_eq!(
            metadata["codexHome"],
            isolated_codex_home(&mp).display().to_string()
        );
        let args = metadata["args"].as_array().unwrap();
        assert_eq!(args.iter().any(|arg| arg == "resume"), entry == "resume");
        assert!(!args.iter().any(|arg| arg == "--model"));
        mp.assert_router_gateway_key_is_private();
        mp.submit_jev(b"analyze B\r");
        mp.wait_for_router_route("B", "gpt-6-astra");
        mp.assert_router_jev_turn_once("B");
        let bundle = mp.embedded_router_bundle();
        let name = bundle.file_name().unwrap().to_owned();
        if let Some(expected) = &bundle_name {
            assert_eq!(
                &name, expected,
                "{entry} changed the embedded runtime bundle"
            );
        } else {
            bundle_name = Some(name);
        }
    }
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
    mp.send(b"\r\r\r");
    mp.expect(
        PANE_READY,
        STEP_TIMEOUT,
        "post-switch new-session pane came up",
    );
    mp.wait_for_codex_home(&mp._tmp.join("home").join(".codex"), STEP_TIMEOUT);
}
