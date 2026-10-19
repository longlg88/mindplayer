//! Bundle the local experiment with the executable, not a checkout-relative path.

use crate::startup_routing::RoutingMode;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use mindplayer_core::{accounts::Account, limits::QuotaRow, Agent, Command};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ASSETS: &[(&str, &str)] = &[
    (
        "run.mjs",
        include_str!("../../../tools/local-model-router/run.mjs"),
    ),
    (
        "startup-check.mjs",
        include_str!("../../../tools/local-model-router/startup-check.mjs"),
    ),
    (
        "private-ui.mjs",
        include_str!("../../../tools/local-model-router/private-ui.mjs"),
    ),
    (
        "launch.mjs",
        include_str!("../../../tools/local-model-router/launch.mjs"),
    ),
    (
        "router.mjs",
        include_str!("../../../tools/local-model-router/router.mjs"),
    ),
    (
        "proxy.mjs",
        include_str!("../../../tools/local-model-router/proxy.mjs"),
    ),
    (
        "app-server.mjs",
        include_str!("../../../tools/local-model-router/app-server.mjs"),
    ),
    (
        "websocket.mjs",
        include_str!("../../../tools/local-model-router/websocket.mjs"),
    ),
    (
        "jev.mjs",
        include_str!("../../../tools/local-model-router/jev.mjs"),
    ),
    (
        "rubric.mjs",
        include_str!("../../../tools/local-model-router/rubric.mjs"),
    ),
    (
        "system-fetch.mjs",
        include_str!("../../../tools/local-model-router/system-fetch.mjs"),
    ),
    (
        "policy.mjs",
        include_str!("../../../tools/local-model-router/policy.mjs"),
    ),
    (
        "service.mjs",
        include_str!("../../../tools/local-model-router/service.mjs"),
    ),
    (
        "current-family.mjs",
        include_str!("../../../tools/local-model-router/current-family.mjs"),
    ),
    (
        "status.mjs",
        include_str!("../../../tools/local-model-router/status.mjs"),
    ),
];
static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
const AVAILABILITY_ENV: &str = "MINDPLAYER_ROUTER_AVAILABILITY";
const AVAILABILITY_MAX_AGE: chrono::Duration = chrono::Duration::seconds(60);
const STATUS_ENV: &str = "MINDPLAYER_ROUTER_STATUS";
const PER_PROMPT_ENV: &str = "MINDPLAYER_PER_PROMPT_CHOICE";
const STATUS_MAX_BYTES: u64 = 4096;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const ROUTING_ENV: &[&str] = &[
    "AI_GATEWAY_API_KEY",
    "TYPESAFE_API_KEY",
    "VERCEL_OIDC_TOKEN",
    "MINDPLAYER_LOCAL_ROUTER_TOKEN",
    "MINDPLAYER_CLAUDE_JEV_URL",
    "MINDPLAYER_CLAUDE_JEV_TOKEN",
    AVAILABILITY_ENV,
    STATUS_ENV,
    PER_PROMPT_ENV,
];
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_MAX_BYTES: usize = 256;
const STARTUP_FAILED: &str = "Jev 시작 준비 상태를 확인할 수 없습니다.";
const STARTUP_NODE_UNAVAILABLE: &str = "Node.js를 실행할 수 없습니다.";
const STARTUP_TIMED_OUT: &str = "Jev 시작 준비 확인 시간이 초과되었습니다.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RoutingStage {
    Ready,
    Judging,
    Requested,
    Accepted,
    Running,
    Completed,
    Failed,
    Interrupted,
    Closed,
}

/// Metadata only: terminal text and arbitrary upstream error bodies never
/// enter this channel. Node replaces the file; its inode is not its identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RoutingStatus {
    pub schema_version: u8,
    pub generation: String,
    pub revision: u64,
    pub stage: RoutingStage,
    pub thread_id: Option<String>,
    pub request_id: serde_json::Value,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub observed_model: Option<String>,
    pub reason: Option<String>,
    pub server_model_reported: bool,
}

impl RoutingStatus {
    fn ready(generation: String) -> Self {
        Self {
            schema_version: 2,
            generation,
            revision: 0,
            stage: RoutingStage::Ready,
            thread_id: None,
            request_id: serde_json::Value::Null,
            turn_id: None,
            model: None,
            effort: None,
            observed_model: None,
            reason: None,
            server_model_reported: false,
        }
    }

    fn valid(&self) -> bool {
        let optional_identifier =
            |value: &Option<String>| value.as_deref().is_none_or(status_identifier);
        self.schema_version == 2
            && status_identifier(&self.generation)
            && self.revision <= MAX_SAFE_INTEGER
            && [
                &self.thread_id,
                &self.turn_id,
                &self.model,
                &self.effort,
                &self.observed_model,
                &self.reason,
            ]
            .into_iter()
            .all(optional_identifier)
            && (self.request_id.is_null()
                || self.request_id.as_str().is_some_and(status_identifier)
                || self
                    .request_id
                    .as_i64()
                    .is_some_and(|id| id.unsigned_abs() <= MAX_SAFE_INTEGER))
            && (!self.server_model_reported
                || (self.observed_model.is_some() && self.turn_id.is_some()))
    }
}

fn status_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
}

pub(crate) struct RoutingStatusChannel {
    path: PathBuf,
    directory: fs::File,
    snapshot: Mutex<RoutingStatus>,
}

impl RoutingStatusChannel {
    pub(crate) fn create_at(data_root: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

        // Validate existing parents too: recursive creation alone does not
        // make an existing directory private or reject a symbolic link.
        mindplayer_core::private::create_dir_private(data_root)?;
        check_status_directory(&fs::symlink_metadata(data_root)?)?;
        let root = data_root.join("routing-status");
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        check_status_directory(&fs::symlink_metadata(&root)?)?;
        let generation = format!(
            "pane-{}-{:x}-{:x}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        );
        let owned_dir = root.join(&generation);
        fs::DirBuilder::new().mode(0o700).create(&owned_dir)?;
        let result = (|| -> Result<Self> {
            let directory = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(&owned_dir)?;
            check_status_directory(&directory.metadata()?)?;
            let snapshot = RoutingStatus::ready(generation);
            let path = owned_dir.join("status.json");
            let channel = Self {
                path,
                directory,
                snapshot: Mutex::new(snapshot.clone()),
            };
            // O_EXCL refuses a planted file; the directory was created by this
            // invocation and is never reused by another pane.
            mindplayer_core::private::create_new_private(channel.path())?
                .write_all(&serde_json::to_vec(&snapshot)?)?;
            Ok(channel)
        })();
        if result.is_err() {
            let _ = fs::remove_dir(&owned_dir);
        }
        result
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn snapshot(&self) -> Option<RoutingStatus> {
        self.snapshot.lock().ok().map(|snapshot| snapshot.clone())
    }

    /// Called on idle ticks as well as PTY output. A regular file opened
    /// without following links and read through a byte cap cannot block on a
    /// FIFO or grow an unbounded allocation on the UI thread.
    pub(crate) fn poll(&self) -> bool {
        let Ok(next) = self.read() else { return false };
        let Ok(mut cached) = self.snapshot.lock() else {
            return false;
        };
        if next.generation != cached.generation || next.revision <= cached.revision {
            return false;
        }
        *cached = next;
        true
    }

    fn read(&self) -> Result<RoutingStatus> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::MetadataExt;

        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                c"status.json".as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.len() > STATUS_MAX_BYTES
        {
            bail!("라우팅 상태 파일이 안전하지 않습니다");
        }
        let mut bytes = Vec::new();
        file.take(STATUS_MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > STATUS_MAX_BYTES {
            bail!("라우팅 상태 파일이 너무 큽니다");
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        // Option fields must be explicitly present as null, not silently
        // defaulted by serde when a writer emits an incomplete contract.
        if value.as_object().is_none_or(|object| object.len() != 12) {
            bail!("라우팅 상태 필드가 잘못되었습니다");
        }
        let next: RoutingStatus = serde_json::from_value(value)?;
        if !next.valid() {
            bail!("라우팅 상태 값이 잘못되었습니다");
        }
        Ok(next)
    }
}

fn check_status_directory(metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        bail!("라우팅 상태 디렉터리가 비공개 디렉터리가 아닙니다");
    }
    Ok(())
}

impl Drop for RoutingStatusChannel {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        // Unlink just our entry relative to the directory we opened. Never
        // recurse, follow a replacement directory, or sweep other panes.
        unsafe {
            libc::unlinkat(self.directory.as_raw_fd(), c"status.json".as_ptr(), 0);
        }
        if let Some(parent) = self.path.parent() {
            if let (Ok(current), Ok(owned)) =
                (fs::symlink_metadata(parent), self.directory.metadata())
            {
                if current.file_type().is_dir()
                    && current.dev() == owned.dev()
                    && current.ino() == owned.ino()
                {
                    let _ = fs::remove_dir(parent);
                }
            }
        }
    }
}

/// Only this process's recent, successful account reading can authorize a
/// footer snapshot. Cached rows and an inherited snapshot are never live data.
pub(crate) fn forward_availability(
    command: &mut Command,
    account: Option<&Account>,
    rows: Option<&[QuotaRow]>,
    observed_at: Option<DateTime<Utc>>,
) {
    // This snapshot describes Codex quota/identity, never Claude availability.
    if command.program != "codex" || !is_session(command) {
        return;
    }
    command.env.set.retain(|(key, _)| key != AVAILABILITY_ENV);
    if !command.env.unset.iter().any(|key| key == AVAILABILITY_ENV) {
        command.env.unset.push(AVAILABILITY_ENV.into());
    }
    let Some(account) = account.filter(|account| account.provider == Agent::Codex) else {
        return;
    };
    let Some(observed_at) = observed_at else {
        return;
    };
    let age = Utc::now().signed_duration_since(observed_at);
    if age < chrono::Duration::zero() || age > AVAILABILITY_MAX_AGE {
        return;
    }
    let Some((_, codex_home)) = command
        .env
        .set
        .iter()
        .rev()
        .find(|(key, _)| key == "CODEX_HOME")
    else {
        return;
    };
    // A display label alone cannot bind a reading to a command using a
    // different account home. The service verifies the actual login separately.
    if !account
        .launch_env()
        .set
        .iter()
        .any(|(key, value)| key == "CODEX_HOME" && value == codex_home)
    {
        return;
    }
    let matching: Vec<_> = rows
        .into_iter()
        .flatten()
        .filter(|row| row.agent == Agent::Codex && row.account == account.name)
        .collect();
    if matching.is_empty() || matching.iter().any(|row| row.failure.is_some()) {
        return;
    }
    let blocked = matching.iter().any(|row| row.blocked);
    let identity = matching.iter().find_map(|row| row.identity.as_deref());
    let snapshot = serde_json::json!({
        "observedAt": observed_at.timestamp_millis(),
        "account": account.name,
        "blocked": blocked,
        "identity": identity,
        "source": "footer-live",
        "pool": null,
        "codexHome": codex_home,
    });
    command
        .env
        .set
        .push((AVAILABILITY_ENV.into(), snapshot.to_string()));
}

/// Called at the actual PTY spawn boundary: new, resume and handoff all pass
/// here. Commands for login/account probes and other providers remain native.
pub(crate) fn prepare(command: &Command) -> Result<Command> {
    prepare_at(
        command,
        &mindplayer_core::private::data_root().join("runtime"),
    )
}

fn prepare_at(command: &Command, runtime: &Path) -> Result<Command> {
    reject_legacy_claude_launcher(command)?;
    if is_native_claude(command) {
        return Ok(native_command(command));
    }
    if !is_session(command) {
        return Ok(command.clone());
    }
    let script = install_assets(runtime)?;
    Ok(wrap(command, &script))
}

/// Prepare before login_then_start_with builds its shell. Claude and explicit
/// Native commands keep their provider, arguments and auth without router IO.
pub(crate) fn prepare_for_mode(command: &Command, mode: RoutingMode) -> Result<Command> {
    reject_legacy_claude_launcher(command)?;
    if mode == RoutingMode::Native || is_native_claude(command) {
        return Ok(native_command(command));
    }
    prepare(command)
}

fn native_command(command: &Command) -> Command {
    let mut native = command.clone();
    native
        .env
        .set
        .retain(|(key, _)| !ROUTING_ENV.contains(&key.as_str()));
    for key in ROUTING_ENV {
        if !native.env.unset.iter().any(|unset| unset.as_str() == *key) {
            native.env.unset.push((*key).into());
        }
    }
    native
}

// The core's generated login shell has one prepared exec after native login.
// Inspect that command only; prompt arguments after it remain opaque.
fn login_exec(command: &Command) -> Option<&str> {
    (command.program == "sh" && command.args.len() == 2 && command.args[0] == "-c")
        .then(|| {
            command.args[1]
                .split_once(" && exec ")
                .map(|(_, exec)| exec)
        })
        .flatten()
}

fn is_native_claude(command: &Command) -> bool {
    command.program == "claude"
        || login_exec(command).is_some_and(|exec| exec == "claude" || exec.starts_with("claude "))
}

// Decode only words emitted by shell_word; never evaluate the shell or inspect
// a prompt argument as syntax. Paths may contain spaces, quotes and flag text.
fn first_shell_word(input: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in input.char_indices() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\'' {
            quoted = !quoted;
        } else if ch == '\\' && !quoted {
            escaped = true;
        } else if ch == ' ' && !quoted {
            return Some((word, &input[index + 1..]));
        } else {
            word.push(ch);
        }
    }
    (!quoted && !escaped && !word.is_empty()).then_some((word, ""))
}

fn reject_legacy_claude_launcher(command: &Command) -> Result<()> {
    let direct = command.program == "node"
        && command.args.first().is_some_and(|script| {
            Path::new(script)
                .file_name()
                .is_some_and(|name| name == "claude-run.mjs")
        });
    let login = login_exec(command)
        .and_then(|exec| {
            let (mut program, mut rest) = first_shell_word(exec)?;
            if program == "env" {
                loop {
                    (program, rest) = first_shell_word(rest)?;
                    if !program.contains('=') {
                        break;
                    }
                }
            }
            (program == "node")
                .then(|| first_shell_word(rest))
                .flatten()
        })
        .is_some_and(|(script, _)| {
            Path::new(&script)
                .file_name()
                .is_some_and(|name| name == "claude-run.mjs")
        });
    if direct || login {
        bail!("Claude Jev 실행기는 더 이상 지원하지 않습니다. Claude 세션을 다시 여세요.");
    }
    Ok(())
}

/// Allocate only at the actual spawn boundary. The fresh-login shell may
/// contain an already prepared launcher: inject into that exec alone so the
/// native login never inherits the writable channel.
pub(crate) fn prepare_spawn(command: &Command) -> Result<(Command, Option<RoutingStatusChannel>)> {
    attach_spawn_status(prepare(command)?, &mindplayer_core::private::data_root())
}

pub(crate) fn prepare_spawn_for_mode(
    command: &Command,
    mode: RoutingMode,
) -> Result<(Command, Option<RoutingStatusChannel>)> {
    match mode {
        RoutingMode::Jev | RoutingMode::PerPrompt => prepare_spawn(command),
        RoutingMode::Native => Ok((prepare_for_mode(command, mode)?, None)),
    }
}

/// Local readiness only, called by the startup UI's background worker for
/// Jev. No authentication request, model probe, app-server or status channel.
pub(crate) fn check_startup() -> std::result::Result<(), &'static str> {
    let script = install_assets(&mindplayer_core::private::data_root().join("runtime"))
        .map_err(|_| STARTUP_FAILED)?
        .with_file_name("startup-check.mjs");
    let mut process = std::process::Command::new("node");
    process.arg(script);
    // Readiness must not execute inherited Node preload hooks.
    process.env_remove("NODE_OPTIONS").env_remove("NODE_PATH");
    run_startup_check(process, STARTUP_TIMEOUT)
}

fn run_startup_check(
    mut process: std::process::Command,
    timeout: Duration,
) -> std::result::Result<(), &'static str> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = process.spawn().map_err(|_| STARTUP_NODE_UNAVAILABLE)?;
    let started = Instant::now();
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or(STARTUP_FAILED)?;
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(STARTUP_FAILED);
        }
        let mut output = Vec::new();
        loop {
            read_startup_output(&mut stdout, &mut output)?;
            if let Some(status) = child.try_wait().map_err(|_| STARTUP_FAILED)? {
                // Drain bytes written between the read and observing exit,
                // without waiting for EOF on a pipe inherited by a descendant.
                read_startup_output(&mut stdout, &mut output)?;
                return startup_result(status.success(), &output);
            }
            if started.elapsed() >= timeout {
                return Err(STARTUP_TIMED_OUT);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        // Include the bounded private UI contract subprocess in cancellation.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn read_startup_output(
    stdout: &mut impl Read,
    output: &mut Vec<u8>,
) -> std::result::Result<(), &'static str> {
    let mut buffer = [0; STARTUP_MAX_BYTES + 1];
    loop {
        match stdout.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if output.len() + count > STARTUP_MAX_BYTES {
                    return Err(STARTUP_FAILED);
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(STARTUP_FAILED),
        }
    }
}

fn startup_result(success: bool, output: &[u8]) -> std::result::Result<(), &'static str> {
    // Accept only this module's complete fixed JSON, never interpolate Node's
    // stdout/stderr or parse an upstream body into a user-visible error.
    match (success, output) {
        (true, b"{\"ok\":true,\"reason\":\"ready\"}\n") => Ok(()),
        (false, b"{\"ok\":false,\"reason\":\"jev-key-unavailable\"}\n") => {
            Err("Jev 키가 없습니다. 전용 키를 설정한 뒤 다시 시도하세요.")
        }
        (false, b"{\"ok\":false,\"reason\":\"external-transfer-policy-unset\"}\n") => {
            Err("정책에서 외부 프롬프트 전송을 허용하지 않습니다.")
        }
        (false, b"{\"ok\":false,\"reason\":\"policy-invalid\"}\n") => {
            Err("Jev 정책 파일이 올바르지 않습니다.")
        }
        (false, b"{\"ok\":false,\"reason\":\"private-ui-missing\"}\n") => {
            Err("MindPlayer 전용 Codex 설치 파일이 없습니다.")
        }
        (false, b"{\"ok\":false,\"reason\":\"private-ui-invalid\"}\n") => {
            Err("MindPlayer 전용 Codex 설치·무결성·UI 계약을 확인할 수 없습니다.")
        }
        _ => Err(STARTUP_FAILED),
    }
}

fn attach_spawn_status(
    mut command: Command,
    data_root: &Path,
) -> Result<(Command, Option<RoutingStatusChannel>)> {
    reject_legacy_claude_launcher(&command)?;
    command
        .env
        .set
        .retain(|(key, _)| key != STATUS_ENV && key != PER_PROMPT_ENV);
    for key in [STATUS_ENV, PER_PROMPT_ENV] {
        if !command.env.unset.iter().any(|unset| unset == key) {
            command.env.unset.push(key.into());
        }
    }
    let script = bundle_directory(&data_root.join("runtime")).join("run.mjs");
    let direct = command.program == "node"
        && command
            .args
            .first()
            .is_some_and(|arg| Path::new(arg) == script)
        && command.args.get(1).is_some_and(|arg| arg == "--mindplayer")
        && command.args.get(2).is_some_and(|arg| arg == "--cwd")
        && command
            .args
            .get(3)
            .is_some_and(|arg| Path::new(arg) == command.cwd)
        && command.args.get(4).is_some_and(|arg| arg == "--");
    let shell_exec = format!(
        " && exec node {} --mindplayer --cwd {} --",
        shell_word(&script.to_string_lossy()),
        shell_word(&command.cwd.to_string_lossy())
    );
    let fresh_login = command.program == "sh"
        && command.args.len() == 2
        && command.args[0] == "-c"
        && command.args[1].contains(&shell_exec);
    if !direct && !fresh_login {
        return Ok((command, None));
    }
    verify_assets(script.parent().expect("bundled script has a parent"))?;
    let channel = RoutingStatusChannel::create_at(data_root)?;
    if direct {
        command.env.set.push((PER_PROMPT_ENV.into(), "1".into()));
        command.env.set.push((
            STATUS_ENV.into(),
            channel.path().to_string_lossy().into_owned(),
        ));
    } else {
        let replacement = format!(
            " && exec env {} {} node {} --mindplayer --cwd {} --",
            shell_word(&format!("{STATUS_ENV}={}", channel.path().display())),
            shell_word(&format!("{PER_PROMPT_ENV}=1")),
            shell_word(&script.to_string_lossy()),
            shell_word(&command.cwd.to_string_lossy())
        );
        command.args[1] = command.args[1].replacen(&shell_exec, &replacement, 1);
    }
    Ok((command, Some(channel)))
}

// Match the core's login_then_start_with quoting without parsing or executing
// opaque prompt arguments as shell syntax.
fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-/_=.".contains(&byte))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn is_session(command: &Command) -> bool {
    match command.program.as_str() {
        "codex" => !matches!(
            command.args.first().map(String::as_str),
            Some(
                "login"
                    | "logout"
                    | "app-server"
                    | "exec"
                    | "review"
                    | "mcp"
                    | "mcp-server"
                    | "completion"
                    | "debug"
                    | "sandbox"
                    | "apply"
                    | "cloud"
                    | "features"
                    | "--version"
                    | "-V"
                    | "-v"
                    | "--help"
                    | "-h"
            )
        ),
        _ => false,
    }
}

fn wrap(command: &Command, script: &Path) -> Command {
    let mut routed = command.clone();
    routed.program = "node".into();
    routed.args = vec![
        script.to_string_lossy().into_owned(),
        "--mindplayer".into(),
        "--cwd".into(),
        command.cwd.to_string_lossy().into_owned(),
        "--".into(),
    ];
    routed.args.extend(command.args.clone());
    if command.program == "codex" {
        routed.env.set.retain(|(key, _)| key != PER_PROMPT_ENV);
        routed.env.unset.retain(|key| key != PER_PROMPT_ENV);
        routed.env.set.push((PER_PROMPT_ENV.into(), "1".into()));
    }
    routed
}

fn verify_assets(dir: &Path) -> Result<()> {
    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
        bail!("로컬 라우터 경로가 디렉터리가 아닙니다");
    }
    for (name, source) in ASSETS {
        let file = dir.join(name);
        if !fs::symlink_metadata(&file)?.file_type().is_file()
            || fs::read(&file)? != source.as_bytes()
        {
            bail!("로컬 라우터 파일이 설치본과 다릅니다: {name}");
        }
    }
    Ok(())
}

fn bundle_directory(root: &Path) -> PathBuf {
    let mut hash = DefaultHasher::new();
    ASSETS.hash(&mut hash);
    root.join(format!("codex-router-{:016x}", hash.finish()))
}

fn install_assets(root: &Path) -> Result<PathBuf> {
    let destination = bundle_directory(root);
    if destination.exists() {
        verify_assets(&destination)?;
        return Ok(destination.join("run.mjs"));
    }
    mindplayer_core::private::create_dir_private(root)?;
    let stage = root.join(format!(
        ".router-stage-{}-{}",
        std::process::id(),
        NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
    ));
    // Unique, private staging directory; publish only after every asset is
    // complete, so simultaneous session opens cannot execute half a module.
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&stage)?;
    let result = (|| -> Result<()> {
        for (name, source) in ASSETS {
            mindplayer_core::private::create_new_private(&stage.join(name))?
                .write_all(source.as_bytes())?;
        }
        if let Err(error) = fs::rename(&stage, &destination) {
            // Another opener may have published exactly this bundle first.
            verify_assets(&destination)
                .with_context(|| format!("로컬 라우터 설치 실패: {error}"))?;
        }
        Ok(())
    })();
    // Only the exact staging directory created by this invocation is removed.
    if stage.exists() {
        let _ = fs::remove_dir_all(&stage);
    }
    result?;
    Ok(destination.join("run.mjs"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mindplayer_core::accounts::LaunchEnv;

    fn assert_native_commands() {
        let cwd: PathBuf = "/tmp/native project".into();
        let mut commands = Vec::new();
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let account = Account::inherited(agent);
            let start = mindplayer_core::new_session(agent, cwd.clone(), &account);
            commands.push(start.clone());
            commands.push(mindplayer_core::login(agent, cwd.clone(), &account));
            let prepared = prepare_for_mode(&start, RoutingMode::Native).unwrap();
            let login = mindplayer_core::resume::login_then_start_with(&account, prepared);
            assert!(!login.args[1].contains("exec node "));
            commands.push(login);
        }
        for args in [
            vec!["resume", "thread-id", "--sandbox", "danger-full-access"],
            vec!["--ask-for-approval", "never", "handoff ' $ opaque prompt"],
            vec!["logout"],
            vec!["app-server"],
        ] {
            commands.push(Command {
                program: "codex".into(),
                args: args.into_iter().map(String::from).collect(),
                cwd: cwd.clone(),
                env: LaunchEnv::default(),
            });
        }
        for mut command in commands {
            command.env.set.extend([
                ("CODEX_HOME".into(), "/account/unchanged".into()),
                ("CLAUDE_CONFIG_DIR".into(), "/claude/unchanged".into()),
                ("OPENAI_API_KEY".into(), "native-account-key".into()),
                ("ANTHROPIC_API_KEY".into(), "native-provider-key".into()),
                ("MINDPLAYER_UNRELATED".into(), "keep".into()),
            ]);
            let clean_set = command.env.set.clone();
            // Duplicates must also disappear; these are dedicated to routing.
            for key in ROUTING_ENV.iter().chain(ROUTING_ENV) {
                command
                    .env
                    .set
                    .push(((*key).into(), "routing-canary".into()));
            }
            command.env.unset = vec!["UNRELATED".into(), STATUS_ENV.into()];
            let expected_unset: Vec<String> = ["UNRELATED", STATUS_ENV]
                .into_iter()
                .chain(ROUTING_ENV.iter().copied().filter(|key| *key != STATUS_ENV))
                .map(String::from)
                .collect();
            let original = command.clone();
            let prepared = prepare_for_mode(&command, RoutingMode::Native).unwrap();
            let (spawn, channel) = prepare_spawn_for_mode(&command, RoutingMode::Native).unwrap();
            assert!(channel.is_none());
            assert_eq!(spawn, prepared);
            assert_eq!(prepared.program, original.program);
            assert_eq!(prepared.args, original.args);
            assert_eq!(prepared.cwd, original.cwd);
            assert_eq!(prepared.env.set, clean_set);
            assert_eq!(prepared.env.unset, expected_unset);
            assert_eq!(
                prepare_for_mode(&prepared, RoutingMode::Native).unwrap(),
                prepared,
                "native preparation must be idempotent"
            );
            assert_eq!(command, original, "the input command is never mutated");
        }
    }

    #[test]
    fn native_mode_needs_no_node_assets_private_ui_or_status_channel() {
        const CHILD_HOME: &str = "MP_NATIVE_BOUNDARY_TEST_HOME";
        if let Some(home) = std::env::var_os(CHILD_HOME) {
            let data_root = PathBuf::from(home).join(".mindplayer");
            assert_eq!(mindplayer_core::private::data_root(), data_root);
            assert!(!data_root.exists());
            assert_native_commands();
            assert!(
                !data_root.exists(),
                "Native must not stage or allocate anything"
            );
            return;
        }
        // Isolate HOME/PATH in a fresh test process, without racing getenv in
        // parallel tests or touching the real private UI/credentials/runtime.
        let root = std::env::temp_dir().join(format!(
            "mp-native-boundary-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_router::tests::native_mode_needs_no_node_assets_private_ui_or_status_channel",
                "--nocapture",
            ])
            .env("HOME", &root)
            .env("PATH", "")
            .env(CHILD_HOME, &root)
            .env("AI_GATEWAY_API_KEY", "ambient-routing-canary")
            .env("TYPESAFE_API_KEY", "ambient-routing-canary")
            .output();
        fs::remove_dir_all(root).unwrap();
        let output = result.unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn jev_mode_preserves_legacy_preparation_for_non_session_commands() {
        for program in ["claude", "codex", "sh"] {
            let command = Command {
                program: program.into(),
                args: vec!["login".into()],
                cwd: "/tmp".into(),
                env: LaunchEnv::default(),
            };
            assert_eq!(
                prepare_for_mode(&command, RoutingMode::Jev).unwrap(),
                prepare(&command).unwrap()
            );
            let (prepared, channel) = prepare_spawn_for_mode(&command, RoutingMode::Jev).unwrap();
            let (legacy, legacy_channel) = prepare_spawn(&command).unwrap();
            assert_eq!(prepared, legacy);
            assert!(channel.is_none() && legacy_channel.is_none());
        }
    }

    #[test]
    fn native_claude_strips_stale_routing_env_without_changing_auth_or_arguments() {
        let mut command = Command {
            program: "claude".into(),
            args: vec!["--resume".into(), "unchanged-session".into()],
            cwd: "/tmp/a project".into(),
            env: LaunchEnv {
                set: vec![
                    ("CLAUDE_CONFIG_DIR".into(), "/account/unchanged".into()),
                    ("ANTHROPIC_API_KEY".into(), "native-auth-canary".into()),
                ],
                unset: vec!["UNRELATED".into()],
            },
        };
        let auth = command.env.set.clone();
        let routing_keys = [
            "AI_GATEWAY_API_KEY",
            "TYPESAFE_API_KEY",
            "VERCEL_OIDC_TOKEN",
            "MINDPLAYER_LOCAL_ROUTER_TOKEN",
            "MINDPLAYER_ROUTER_STATUS",
            "MINDPLAYER_ROUTER_AVAILABILITY",
            "MINDPLAYER_CLAUDE_JEV_URL",
            "MINDPLAYER_CLAUDE_JEV_TOKEN",
            "MINDPLAYER_PER_PROMPT_CHOICE",
        ];
        for key in routing_keys.into_iter().chain(routing_keys) {
            command
                .env
                .set
                .push((key.into(), "stale-routing-canary".into()));
        }
        let original = command.clone();
        for mode in [
            RoutingMode::Native,
            RoutingMode::Jev,
            RoutingMode::PerPrompt,
        ] {
            let prepared = prepare_for_mode(&command, mode).unwrap();
            let (spawn, channel) = prepare_spawn_for_mode(&command, mode).unwrap();
            assert!(channel.is_none());
            assert_eq!(spawn, prepared);
            assert_eq!(prepared.program, command.program);
            assert_eq!(prepared.args, command.args);
            assert_eq!(prepared.cwd, command.cwd);
            assert_eq!(prepared.env.set, auth);
            assert!(prepared.env.unset.iter().any(|key| key == "UNRELATED"));
            for key in routing_keys {
                assert_eq!(
                    prepared
                        .env
                        .unset
                        .iter()
                        .filter(|unset| unset.as_str() == key)
                        .count(),
                    1
                );
            }
        }
        assert_eq!(command, original);
    }

    #[test]
    fn native_claude_login_shell_is_sanitized_at_the_actual_spawn_boundary() {
        let account = Account::inherited(Agent::Claude);
        let start = mindplayer_core::new_session(Agent::Claude, "/tmp/a folder".into(), &account);
        for mode in [RoutingMode::Jev, RoutingMode::PerPrompt] {
            let start = prepare_for_mode(&start, mode).unwrap();
            let mut login = mindplayer_core::resume::login_then_start_with(&account, start);
            assert!(login.args[1].contains("claude auth login && exec claude"));
            let auth = login.env.set.clone();
            for key in ROUTING_ENV
                .iter()
                .copied()
                .chain(["MINDPLAYER_PER_PROMPT_CHOICE"])
            {
                login
                    .env
                    .set
                    .push((key.into(), "stale-routing-canary".into()));
            }
            let (spawn, channel) = prepare_spawn_for_mode(&login, mode).unwrap();
            assert!(channel.is_none());
            assert_eq!(spawn.program, login.program);
            assert_eq!(spawn.args, login.args);
            assert_eq!(spawn.cwd, login.cwd);
            assert_eq!(spawn.env.set, auth);
            assert!(spawn
                .env
                .unset
                .iter()
                .any(|key| key == "MINDPLAYER_PER_PROMPT_CHOICE"));
            for key in ROUTING_ENV {
                assert!(spawn.env.unset.iter().any(|unset| unset.as_str() == *key));
            }
        }
    }

    #[test]
    fn codex_non_session_commands_never_stage_router_assets() {
        let root = std::env::temp_dir().join(format!(
            "mp-non-session-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        for arg in [
            "login",
            "logout",
            "app-server",
            "--help",
            "-h",
            "--version",
            "-V",
            "exec",
            "review",
            "mcp",
            "mcp-server",
            "completion",
            "debug",
            "sandbox",
            "apply",
            "cloud",
            "features",
        ] {
            let command = Command {
                program: "codex".into(),
                args: vec![arg.into()],
                cwd: "/tmp".into(),
                env: LaunchEnv::default(),
            };
            assert!(!is_session(&command), "{arg}");
            assert_eq!(prepare_at(&command, &root).unwrap(), command);
            for mode in [RoutingMode::Jev, RoutingMode::PerPrompt] {
                assert_eq!(prepare_for_mode(&command, mode).unwrap(), command);
                let (spawn, channel) = prepare_spawn_for_mode(&command, mode).unwrap();
                assert!(channel.is_none());
                assert_eq!(spawn.program, command.program);
                assert_eq!(spawn.args, command.args);
            }
        }
        assert!(!root.exists());
    }

    #[test]
    fn startup_output_requires_the_exact_fixed_contract_and_matching_exit() {
        let ready = b"{\"ok\":true,\"reason\":\"ready\"}\n";
        assert_eq!(startup_result(true, ready), Ok(()));
        assert_eq!(startup_result(false, ready), Err(STARTUP_FAILED));
        for output in [
            b"".as_slice(),
            b"{\"ok\":true,\"reason\":\"ready\",\"secret\":\"canary\"}\n",
            b"{\"ok\":false,\"reason\":\"credential-upstream-body\"}\n",
            b"{\"ok\":true,\"reason\":\"ready\"}\nextra",
        ] {
            assert_eq!(startup_result(true, output), Err(STARTUP_FAILED));
            assert_eq!(startup_result(false, output), Err(STARTUP_FAILED));
        }
        let mut bounded = Vec::new();
        assert_eq!(
            read_startup_output(&mut std::io::repeat(b'x'), &mut bounded),
            Err(STARTUP_FAILED)
        );
        assert!(bounded.len() <= STARTUP_MAX_BYTES);
    }

    #[test]
    fn startup_process_is_bounded_when_silent_or_flooding_stdout_or_stderr() {
        for script in [
            "while :; do :; done",
            "while :; do printf '%s' credential-canary; done",
            "while :; do printf '%s' credential-canary >&2; done",
        ] {
            let mut process = std::process::Command::new("/bin/sh");
            process.args(["-c", script]);
            let start = Instant::now();
            let result = run_startup_check(process, Duration::from_millis(100));
            assert!(matches!(result, Err(STARTUP_FAILED | STARTUP_TIMED_OUT)));
            assert!(start.elapsed() < Duration::from_secs(3));
        }
        let missing = std::process::Command::new("/no-such-mindplayer-startup-node");
        assert_eq!(
            run_startup_check(missing, Duration::from_millis(100)),
            Err(STARTUP_NODE_UNAVAILABLE)
        );
        let mut ready = std::process::Command::new("/bin/sh");
        ready.args(["-c", "printf '%s\\n' '{\"ok\":true,\"reason\":\"ready\"}'"]);
        assert_eq!(run_startup_check(ready, Duration::from_secs(1)), Ok(()));
    }

    #[test]
    fn codex_sessions_route_and_unrelated_providers_stay_native() {
        let account = Account::inherited(Agent::Codex);
        let new = mindplayer_core::new_session(Agent::Codex, "/tmp".into(), &account);
        assert!(is_session(&new));
        assert!(!is_session(&mindplayer_core::login(
            Agent::Codex,
            "/tmp".into(),
            &account
        )));
        for agent in [Agent::Kiro, Agent::Cursor] {
            let command =
                mindplayer_core::new_session(agent, "/tmp".into(), &Account::inherited(agent));
            assert!(!is_session(&command));
            assert_eq!(prepare(&command).unwrap(), command);
        }
        let mut resume = new.clone();
        resume
            .args
            .splice(0..0, ["resume".into(), "thread-id".into()]);
        assert!(is_session(&resume));
    }

    #[test]
    fn claude_new_resume_handoff_and_administration_stay_native_without_assets() {
        let root = std::env::temp_dir().join(format!(
            "mp-claude-router-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        let cwd = PathBuf::from("/tmp/a project");
        for args in [
            vec![],
            vec!["--resume", "session-id"],
            vec!["handoff 한글 ' $ opaque prompt"],
            vec!["auth", "login"],
            vec!["auth", "status"],
            vec!["plugin", "list"],
            vec!["login"],
            vec!["--version"],
            vec!["--help"],
        ] {
            let command = Command {
                program: "claude".into(),
                args: args.into_iter().map(String::from).collect(),
                cwd: cwd.clone(),
                env: LaunchEnv {
                    set: vec![("CLAUDE_CONFIG_DIR".into(), "/account/unchanged".into())],
                    unset: vec!["ANTHROPIC_API_KEY".into()],
                },
            };
            assert!(!is_session(&command));
            let direct = prepare_at(&command, &root.join("runtime")).unwrap();
            assert_eq!(direct.program, command.program);
            assert_eq!(direct.args, command.args);
            assert_eq!(direct.cwd, command.cwd);
            assert_eq!(direct.env.set, command.env.set);
            for mode in [
                RoutingMode::Native,
                RoutingMode::Jev,
                RoutingMode::PerPrompt,
            ] {
                let prepared = prepare_for_mode(&command, mode).unwrap();
                let (spawn, channel) = prepare_spawn_for_mode(&command, mode).unwrap();
                assert!(channel.is_none());
                assert_eq!(prepared.program, command.program);
                assert_eq!(prepared.args, command.args);
                assert_eq!(prepared.cwd, command.cwd);
                assert_eq!(spawn, prepared);
                assert_eq!(prepared.env.set, command.env.set);
                for key in ROUTING_ENV {
                    assert!(prepared
                        .env
                        .unset
                        .iter()
                        .any(|unset| unset.as_str() == *key));
                }
            }
        }
        assert!(!root.exists(), "Claude must not stage any router assets");
    }

    #[test]
    fn actual_preparation_consumers_reject_old_claude_launchers() {
        let native = Command {
            program: "claude".into(),
            args: vec!["--resume".into(), "session-id".into()],
            cwd: "/tmp/a folder".into(),
            env: LaunchEnv::default(),
        };
        for script in [
            "/old/runtime/claude-run.mjs",
            "/old/a folder/claude-run.mjs",
            "/old/ --mindplayer --cwd /claude-run.mjs",
            "/old/'quoted/claude-run.mjs",
        ] {
            let old = wrap(&native, Path::new(script));
            let login = mindplayer_core::resume::login_then_start_with(
                &Account::inherited(Agent::Claude),
                old.clone(),
            );
            let mut status_login = login.clone();
            status_login.args[1] = status_login.args[1].replacen(
                " && exec node ",
                " && exec env 'MINDPLAYER_ROUTER_STATUS=/old/ --mindplayer --cwd /status.json' node ",
                1,
            );
            for command in [old, login, status_login] {
                assert!(prepare(&command).is_err());
                assert!(prepare_spawn(&command).is_err());
                for mode in [
                    RoutingMode::Native,
                    RoutingMode::Jev,
                    RoutingMode::PerPrompt,
                ] {
                    assert!(prepare_for_mode(&command, mode).is_err());
                    assert!(prepare_spawn_for_mode(&command, mode).is_err());
                }
            }
        }
        // A filename mentioned inside an opaque Codex prompt is not a launcher.
        let codex = Command {
            program: "codex".into(),
            args: vec!["explain claude-run.mjs --mindplayer --cwd /tmp --".into()],
            ..native
        };
        let login = mindplayer_core::resume::login_then_start_with(
            &Account::inherited(Agent::Codex),
            wrap(&codex, Path::new("/runtime/run.mjs")),
        );
        assert!(prepare_for_mode(&login, RoutingMode::PerPrompt).is_ok());
    }

    #[test]
    fn wrapping_preserves_account_cwd_and_native_arguments() {
        let original = Command {
            program: "codex".into(),
            args: vec![
                "resume".into(),
                "thread-id".into(),
                "한글 ' $ prompt".into(),
            ],
            cwd: "/tmp/a project".into(),
            env: LaunchEnv {
                set: vec![("CODEX_HOME".into(), "/account/home".into())],
                unset: vec!["OPENAI_API_KEY".into()],
            },
        };
        let routed = wrap(&original, Path::new("/runtime/a folder/run.mjs"));
        assert_eq!(routed.program, "node");
        assert_eq!(
            &routed.args[..5],
            [
                "/runtime/a folder/run.mjs",
                "--mindplayer",
                "--cwd",
                "/tmp/a project",
                "--"
            ]
        );
        assert_eq!(routed.args[5..], original.args);
        let mut expected_env = original.env.clone();
        expected_env
            .set
            .push(("MINDPLAYER_PER_PROMPT_CHOICE".into(), "1".into()));
        assert_eq!(routed.env, expected_env);
        assert_eq!(routed.cwd, original.cwd);
    }

    #[test]
    fn bundle_is_complete_private_reusable_and_rejects_modified_modules() {
        let root = std::env::temp_dir().join(format!("mp-router-assets-{}", std::process::id()));
        let script = install_assets(&root).unwrap();
        assert_eq!(install_assets(&root).unwrap(), script);
        verify_assets(script.parent().unwrap()).unwrap();
        assert_eq!(
            ASSETS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            [
                "run.mjs",
                "startup-check.mjs",
                "private-ui.mjs",
                "launch.mjs",
                "router.mjs",
                "proxy.mjs",
                "app-server.mjs",
                "websocket.mjs",
                "jev.mjs",
                "rubric.mjs",
                "system-fetch.mjs",
                "policy.mjs",
                "service.mjs",
                "current-family.mjs",
                "status.mjs"
            ]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&script).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(script.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        for (name, source) in ASSETS {
            let path = script.parent().unwrap().join(name);
            fs::write(&path, "modified").unwrap();
            assert!(install_assets(&root).is_err(), "accepted modified {name}");
            fs::write(path, source).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn availability_fixture() -> (Account, Command, Vec<QuotaRow>) {
        let account =
            Account::isolated(Path::new("/tmp/mp-router-footer"), Agent::Codex, "chosen").unwrap();
        let command = mindplayer_core::new_session(Agent::Codex, "/tmp".into(), &account);
        let rows = vec![QuotaRow {
            agent: Agent::Codex,
            account: account.name.clone(),
            used_percent: Some(7.0),
            identity: Some("opaque-existing-identity".into()),
            ..Default::default()
        }];
        (account, command, rows)
    }

    fn availability(command: &Command) -> Option<serde_json::Value> {
        command
            .env
            .set
            .iter()
            .find(|(key, _)| key == AVAILABILITY_ENV)
            .map(|(_, value)| serde_json::from_str(value).unwrap())
    }

    #[test]
    fn availability_is_scoped_to_live_account_home_and_keeps_fetch_time() {
        let (account, mut command, mut rows) = availability_fixture();
        let observed_at = Utc::now() - chrono::Duration::seconds(15);
        rows.extend([
            QuotaRow {
                agent: Agent::Claude,
                account: account.name.clone(),
                blocked: true,
                ..Default::default()
            },
            QuotaRow {
                agent: Agent::Codex,
                account: "another".into(),
                blocked: true,
                ..Default::default()
            },
        ]);
        forward_availability(&mut command, Some(&account), Some(&rows), Some(observed_at));
        assert_eq!(
            availability(&command).unwrap(),
            serde_json::json!({
                "observedAt": observed_at.timestamp_millis(),
                "account": account.name,
                "blocked": false,
                "identity": "opaque-existing-identity",
                "source": "footer-live",
                "pool": null,
                "codexHome": account.slot_path().to_string_lossy(),
            })
        );
        // Reusing the command must not refresh the reading's age at spawn.
        forward_availability(&mut command, Some(&account), Some(&rows), Some(observed_at));
        assert_eq!(
            availability(&command).unwrap()["observedAt"],
            observed_at.timestamp_millis()
        );
    }

    #[test]
    fn unavailable_or_stale_footer_data_clears_inherited_snapshot() {
        let (account, command, rows) = availability_fixture();
        let now = Utc::now();
        let failed = vec![QuotaRow {
            failure: Some(mindplayer_core::limits::FailureKind::RateLimited),
            ..rows[0].clone()
        }];
        let other = vec![QuotaRow {
            account: "another".into(),
            ..rows[0].clone()
        }];
        for (account, rows, observed_at) in [
            (Some(&account), Some(rows.as_slice()), None),
            (Some(&account), None, Some(now)),
            (None, Some(rows.as_slice()), Some(now)),
            (
                Some(&account),
                Some(rows.as_slice()),
                Some(now - chrono::Duration::seconds(61)),
            ),
            (
                Some(&account),
                Some(rows.as_slice()),
                Some(now + chrono::Duration::seconds(30)),
            ),
            (Some(&account), Some(failed.as_slice()), Some(now)),
            (Some(&account), Some(other.as_slice()), Some(now)),
        ] {
            let mut command = command.clone();
            command
                .env
                .set
                .push((AVAILABILITY_ENV.into(), "old-snapshot".into()));
            forward_availability(&mut command, account, rows, observed_at);
            assert!(availability(&command).is_none());
            assert!(command.env.unset.iter().any(|key| key == AVAILABILITY_ENV));
        }
        let mut different_home = command.clone();
        different_home.env.set = vec![("CODEX_HOME".into(), "/another/home".into())];
        forward_availability(&mut different_home, Some(&account), Some(&rows), Some(now));
        assert!(availability(&different_home).is_none());
    }

    #[test]
    fn availability_distinguishes_spend_caps_from_blocked_usage() {
        let (account, command, mut rows) = availability_fixture();
        rows[0].identity = None;
        rows[0].used_percent = Some(100.0);
        rows[0].spend_cap = true;
        for (spend_cap, refused, expected) in [
            (true, false, false),
            (true, true, true),
            (false, false, false),
            (false, true, true),
        ] {
            let mut command = command.clone();
            rows[0].spend_cap = spend_cap;
            rows[0].blocked = refused;
            forward_availability(&mut command, Some(&account), Some(&rows), Some(Utc::now()));
            let snapshot = availability(&command).unwrap();
            assert_eq!(snapshot["blocked"], expected);
            assert!(snapshot["identity"].is_null());
        }
    }

    #[test]
    fn availability_leaves_logins_and_other_providers_native() {
        for agent in [Agent::Codex, Agent::Claude, Agent::Kiro, Agent::Cursor] {
            let account = Account::inherited(agent);
            let mut commands = vec![mindplayer_core::login(agent, "/tmp".into(), &account)];
            if agent != Agent::Codex {
                commands.push(mindplayer_core::new_session(agent, "/tmp".into(), &account));
            }
            for mut command in commands {
                let original = command.clone();
                forward_availability(&mut command, Some(&account), Some(&[]), Some(Utc::now()));
                assert_eq!(command, original);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod status_tests {
    use super::*;
    use mindplayer_core::accounts::LaunchEnv;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mp-pane-status-{}-{}",
                std::process::id(),
                NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
            ));
            mindplayer_core::private::create_dir_private(&root).unwrap();
            Self(root)
        }

        fn channel(&self) -> RoutingStatusChannel {
            RoutingStatusChannel::create_at(&self.0).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn publish(channel: &RoutingStatusChannel, status: &RoutingStatus) {
        let temporary = channel.path().with_extension("next");
        mindplayer_core::private::create_new_private(&temporary)
            .unwrap()
            .write_all(&serde_json::to_vec(status).unwrap())
            .unwrap();
        fs::rename(temporary, channel.path()).unwrap();
    }

    #[test]
    fn routing_status_v2_bootstrap_preserves_the_twelve_field_contract() {
        let fixture = Fixture::new();
        let channel = fixture.channel();
        let snapshot = channel.snapshot().unwrap();
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            serde_json::json!({
                "schemaVersion": 2, "generation": snapshot.generation, "revision": 0,
                "stage": "ready", "threadId": null, "requestId": null, "turnId": null,
                "model": null, "effort": null, "observedModel": null, "reason": null,
                "serverModelReported": false,
            })
        );
    }

    #[test]
    fn routing_status_ack_is_accepted_instead_of_running() {
        let accepted: RoutingStage = serde_json::from_str("\"accepted\"")
            .expect("a start ACK needs its own accepted lifecycle stage");
        assert_ne!(accepted, RoutingStage::Running);
        assert_eq!(serde_json::to_value(accepted).unwrap(), "accepted");
    }

    #[test]
    fn routing_status_v2_polls_lifecycle_reports_and_fresh_followup_metadata() {
        let fixture = Fixture::new();
        let channel = fixture.channel();
        let mut value = serde_json::json!({
            "schemaVersion": 2, "generation": channel.snapshot().unwrap().generation,
            "revision": 0, "stage": "requested", "threadId": "main-thread",
            "requestId": 1, "turnId": null, "model": "gpt-5.5", "effort": "high",
            "observedModel": null, "reason": "catalog-guided-jev", "serverModelReported": false,
        });
        for (revision, stage) in [
            (1, "requested"),
            (2, "accepted"),
            (3, "running"),
            (4, "completed"),
        ] {
            value["revision"] = serde_json::json!(revision);
            value["stage"] = serde_json::json!(stage);
            if stage == "running" {
                value["turnId"] = serde_json::json!("turn-1");
            }
            fs::write(channel.path(), serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(channel.poll(), "lifecycle snapshot rejected: {value}");
            assert_eq!(
                serde_json::to_value(channel.snapshot().unwrap()).unwrap(),
                value
            );
        }
        value["revision"] = serde_json::json!(5);
        value["observedModel"] = serde_json::json!("gpt-5.6");
        value["serverModelReported"] = serde_json::json!(true);
        fs::write(channel.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            channel.poll(),
            "a correlated server model report is optional metadata"
        );
        assert_eq!(
            channel.snapshot().unwrap().model.as_deref(),
            Some("gpt-5.5")
        );

        value["revision"] = serde_json::json!(6);
        value["stage"] = serde_json::json!("judging");
        value["requestId"] = serde_json::json!(2);
        for field in ["turnId", "model", "effort", "observedModel", "reason"] {
            value[field] = serde_json::Value::Null;
        }
        value["serverModelReported"] = serde_json::json!(false);
        fs::write(channel.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(channel.poll());
        assert_eq!(
            serde_json::to_value(channel.snapshot().unwrap()).unwrap(),
            value
        );
    }

    #[test]
    fn routing_status_bootstrap_is_exclusive_private_and_unique_per_pane() {
        let fixture = Fixture::new();
        let first = fixture.channel();
        let second = fixture.channel();
        let snapshot = first.snapshot().unwrap();
        assert_ne!(first.path(), second.path());
        assert_ne!(snapshot.generation, second.snapshot().unwrap().generation);
        assert!(status_identifier(&snapshot.generation));
        assert_eq!(snapshot, first.read().unwrap());
        assert_eq!(snapshot.stage, RoutingStage::Ready);
        assert_eq!(snapshot.revision, 0);
        assert!(snapshot.thread_id.is_none() && snapshot.model.is_none());
        assert!(snapshot.request_id.is_null() && !snapshot.server_model_reported);
        for directory in [
            fixture.0.clone(),
            fixture.0.join("routing-status"),
            first.path().parent().unwrap().to_path_buf(),
        ] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(first.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(mindplayer_core::private::create_new_private(first.path()).is_err());
        let owned = first.path().parent().unwrap().to_path_buf();
        drop(first);
        assert!(!owned.exists());
        assert!(
            second.path().exists(),
            "closing one pane must not sweep another"
        );
    }

    #[test]
    fn routing_status_accepts_atomic_replacements_but_rejects_stale_generations_and_revisions() {
        use std::os::unix::fs::MetadataExt;
        let fixture = Fixture::new();
        let channel = fixture.channel();
        let initial_inode = fs::metadata(channel.path()).unwrap().ino();
        let mut status = channel.snapshot().unwrap();
        status.revision = 2;
        status.stage = RoutingStage::Judging;
        status.thread_id = Some("main-thread".into());
        publish(&channel, &status);
        assert_ne!(fs::metadata(channel.path()).unwrap().ino(), initial_inode);
        assert!(channel.poll());
        assert_eq!(channel.snapshot(), Some(status.clone()));
        assert!(!channel.poll(), "same revision is not a redraw");
        for revision in [0, 1, 2] {
            let mut stale = status.clone();
            stale.revision = revision;
            stale.stage = RoutingStage::Completed;
            publish(&channel, &stale);
            assert!(!channel.poll());
            assert_eq!(channel.snapshot(), Some(status.clone()));
        }
        let mut other_generation = status.clone();
        other_generation.generation.push_str("-stale");
        other_generation.revision = 99;
        publish(&channel, &other_generation);
        assert!(!channel.poll());
        assert_eq!(channel.snapshot(), Some(status));
    }

    #[test]
    fn routing_status_rejects_symlinks_nonregular_files_and_public_modes() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = Fixture::new();
        let channel = fixture.channel();
        let target = fixture.0.join("foreign.json");
        let mut next = channel.snapshot().unwrap();
        next.revision = 1;
        fs::write(&target, serde_json::to_vec(&next).unwrap()).unwrap();
        fs::remove_file(channel.path()).unwrap();
        symlink(&target, channel.path()).unwrap();
        assert!(!channel.poll());
        fs::remove_file(channel.path()).unwrap();
        let name = std::ffi::CString::new(channel.path().as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(
            !channel.poll(),
            "FIFO must be opened nonblocking, then rejected"
        );
        fs::remove_file(channel.path()).unwrap();
        fs::create_dir(channel.path()).unwrap();
        assert!(!channel.poll());
        fs::remove_dir(channel.path()).unwrap();
        fs::write(channel.path(), serde_json::to_vec(&next).unwrap()).unwrap();
        fs::set_permissions(channel.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!channel.poll());
        fs::set_permissions(channel.path(), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(channel.poll());
    }

    #[test]
    fn routing_status_is_bounded_and_requires_the_complete_sanitized_contract() {
        let fixture = Fixture::new();
        let channel = fixture.channel();
        let mut next = channel.snapshot().unwrap();
        next.revision = 1;
        let mut exact = serde_json::to_vec(&next).unwrap();
        exact.resize(STATUS_MAX_BYTES as usize, b' ');
        fs::write(channel.path(), &exact).unwrap();
        assert!(
            channel.poll(),
            "4096 bytes including JSON whitespace are allowed"
        );
        next.revision = 2;
        let valid = serde_json::to_value(&next).unwrap();
        let mut invalid = Vec::new();
        for (field, replacement) in [
            ("schemaVersion", serde_json::json!(1)),
            ("schemaVersion", serde_json::json!(3)),
            ("stage", serde_json::json!("unknown")),
            ("model", serde_json::json!("gpt-5.5\u{1b}[31m")),
            ("reason", serde_json::json!("upstream error body\n")),
            ("generation", serde_json::json!("한글")),
            ("threadId", serde_json::json!("x".repeat(129))),
            ("requestId", serde_json::json!({"prompt":"not metadata"})),
            ("requestId", serde_json::json!(MAX_SAFE_INTEGER + 1)),
            ("revision", serde_json::json!(MAX_SAFE_INTEGER + 1)),
            ("serverModelReported", serde_json::json!(true)),
        ] {
            let mut value = valid.clone();
            value[field] = replacement;
            invalid.push(value);
        }
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("model");
        invalid.push(missing);
        let mut extra = valid;
        extra["prompt"] = serde_json::json!("not allowed");
        invalid.push(extra);
        for value in invalid {
            fs::write(channel.path(), serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(!channel.poll(), "invalid value accepted: {value}");
        }
        exact.push(b' ');
        fs::write(channel.path(), exact).unwrap();
        assert!(!channel.poll());
        fs::write(channel.path(), b"{").unwrap();
        assert!(!channel.poll());
        assert_eq!(channel.snapshot().unwrap().revision, 1);
    }

    #[test]
    fn routing_status_rejects_unsafe_roots_and_cleanup_never_follows_a_replaced_directory() {
        let fixture = Fixture::new();
        let root = fixture.0.join("routing-status");
        let foreign = fixture.0.join("foreign");
        mindplayer_core::private::create_dir_private(&foreign).unwrap();
        symlink(&foreign, &root).unwrap();
        assert!(RoutingStatusChannel::create_at(&fixture.0).is_err());
        fs::remove_file(&root).unwrap();
        let channel = fixture.channel();
        let original = channel.path().parent().unwrap().to_path_buf();
        let moved = root.join("moved-owned-directory");
        fs::rename(&original, &moved).unwrap();
        fs::write(foreign.join("status.json"), b"foreign sentinel").unwrap();
        symlink(&foreign, &original).unwrap();
        drop(channel);
        assert_eq!(
            fs::read(foreign.join("status.json")).unwrap(),
            b"foreign sentinel"
        );
        assert!(!moved.join("status.json").exists());
        assert!(fs::symlink_metadata(&original)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(original).unwrap();
        fs::remove_dir(moved).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RoutingStatusChannel::create_at(&fixture.0).is_err());
    }

    #[test]
    fn routing_status_environment_is_only_injected_into_the_owned_node_launcher() {
        let fixture = Fixture::new();
        let codex_script = install_assets(&fixture.0.join("runtime")).unwrap();
        for (agent, name, login_text) in [(Agent::Codex, "run.mjs", "codex login && exec env ")] {
            let script = codex_script.with_file_name(name);
            let native = Command {
                program: agent.program().into(),
                args: vec![],
                cwd: "/tmp/a folder".into(),
                env: LaunchEnv {
                    set: vec![(STATUS_ENV.into(), "/inherited/status.json".into())],
                    unset: vec![],
                },
            };
            let (routed, channel) =
                attach_spawn_status(wrap(&native, &script), &fixture.0).unwrap();
            assert_eq!(
                routed
                    .env
                    .set
                    .iter()
                    .filter(|(key, _)| key == STATUS_ENV)
                    .count(),
                1
            );
            assert_eq!(
                routed
                    .env
                    .set
                    .iter()
                    .find(|(key, _)| key == STATUS_ENV)
                    .unwrap()
                    .1,
                channel.as_ref().unwrap().path().to_string_lossy()
            );
            let account = Account::inherited(agent);
            let login =
                mindplayer_core::resume::login_then_start_with(&account, wrap(&native, &script));
            let (login, channel) = attach_spawn_status(login, &fixture.0).unwrap();
            assert!(channel.is_some());
            assert!(login.args[1].contains(login_text));
            assert!(!login.env.set.iter().any(|(key, _)| key == STATUS_ENV));
            for program in ["node", "codex", "claude", "sh"] {
                let mut command = native.clone();
                command.program = program.into();
                let (prepared, channel) = attach_spawn_status(command, &fixture.0).unwrap();
                assert!(channel.is_none());
                assert!(!prepared.env.set.iter().any(|(key, _)| key == STATUS_ENV));
                assert!(prepared.env.unset.iter().any(|key| key == STATUS_ENV));
            }
        }
    }

    #[test]
    fn status_consumer_rejects_an_old_prepared_claude_launcher_without_allocating() {
        let fixture = Fixture::new();
        let script = install_assets(&fixture.0.join("runtime"))
            .unwrap()
            .with_file_name("claude-run.mjs");
        let native = Command {
            program: "claude".into(),
            args: vec![],
            cwd: "/tmp".into(),
            env: LaunchEnv::default(),
        };
        let prepared = wrap(&native, &script);
        let login = mindplayer_core::resume::login_then_start_with(
            &Account::inherited(Agent::Claude),
            prepared.clone(),
        );
        for command in [prepared, login] {
            assert!(attach_spawn_status(command, &fixture.0).is_err());
        }
        assert!(!fixture.0.join("routing-status").exists());
    }

    #[test]
    fn per_prompt_codex_new_resume_handoff_and_login_reach_the_owned_launcher() {
        const CHILD_HOME: &str = "MP_PER_PROMPT_BOUNDARY_TEST_HOME";
        const SERVICE_KEYS: [&str; 9] = [
            "AI_GATEWAY_API_KEY",
            "TYPESAFE_API_KEY",
            "VERCEL_OIDC_TOKEN",
            "MINDPLAYER_LOCAL_ROUTER_TOKEN",
            "MINDPLAYER_ROUTER_AVAILABILITY",
            "MINDPLAYER_ROUTER_STATUS",
            "MINDPLAYER_CLAUDE_JEV_URL",
            "MINDPLAYER_CLAUDE_JEV_TOKEN",
            "MINDPLAYER_PER_PROMPT_CHOICE",
        ];
        if let Some(home) = std::env::var_os(CHILD_HOME) {
            let home = PathBuf::from(home);
            let data_root = home.join(".mindplayer");
            assert_eq!(mindplayer_core::private::data_root(), data_root);
            assert!(!data_root.exists());
            let bin = home.join("bin");
            let cwd = home.join("a folder");
            fs::create_dir(&bin).unwrap();
            fs::create_dir(&cwd).unwrap();
            for (name, body) in [
                ("codex", "[ \"$#\" -eq 1 ] && [ \"$1\" = login ] || exit 91\n/usr/bin/env > \"$FIXTURE_ROOT/native.env\""),
                ("node", "printf '%s\\0' \"$@\" > \"$FIXTURE_ROOT/node.args\"\n/usr/bin/env > \"$FIXTURE_ROOT/node.env\""),
                ("open", "exit 0"),
            ] {
                let path = bin.join(name);
                fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let account = Account::inherited(Agent::Codex);
            for args in [
                vec![],
                vec!["resume", "thread-id"],
                vec![
                    "--ask-for-approval",
                    "never",
                    "handoff 한글 ' $ opaque prompt",
                ],
            ] {
                let mut command = Command {
                    program: "codex".into(),
                    args: args.into_iter().map(String::from).collect(),
                    cwd: cwd.clone(),
                    env: account.launch_env(),
                };
                command
                    .env
                    .set
                    .push(("MINDPLAYER_PER_PROMPT_CHOICE".into(), "stale".into()));
                let original = command.clone();
                let prepared = prepare_for_mode(&command, RoutingMode::PerPrompt).unwrap();
                assert_eq!(prepared.program, "node");
                assert_eq!(Path::new(&prepared.args[0]).file_name().unwrap(), "run.mjs");
                assert_eq!(prepared.args[5..], command.args);
                assert_eq!(prepared.cwd, command.cwd);
                assert_eq!(
                    prepared
                        .env
                        .set
                        .iter()
                        .filter(|(key, _)| key == "MINDPLAYER_PER_PROMPT_CHOICE")
                        .count(),
                    1
                );
                assert!(prepared
                    .env
                    .set
                    .iter()
                    .any(|(key, value)| key == "MINDPLAYER_PER_PROMPT_CHOICE" && value == "1"));
                assert!(
                    !data_root.join("routing-status").exists()
                        || fs::read_dir(data_root.join("routing-status"))
                            .unwrap()
                            .next()
                            .is_none()
                );
                let (spawn, channel) =
                    prepare_spawn_for_mode(&prepared, RoutingMode::PerPrompt).unwrap();
                let channel = channel.unwrap();
                assert_eq!(spawn.args, prepared.args);
                assert_eq!(
                    spawn
                        .env
                        .set
                        .iter()
                        .filter(|(key, _)| key == STATUS_ENV)
                        .count(),
                    1
                );
                assert!(spawn
                    .env
                    .set
                    .iter()
                    .any(|(key, value)| key == "MINDPLAYER_PER_PROMPT_CHOICE" && value == "1"));
                let owned = channel.path().parent().unwrap().to_path_buf();
                drop(channel);
                assert!(!owned.exists());
                let expected_node_args = format!("{}\0", prepared.args.join("\0")).into_bytes();
                let login = mindplayer_core::resume::login_then_start_with(&account, prepared);
                let (spawn, channel) =
                    prepare_spawn_for_mode(&login, RoutingMode::PerPrompt).unwrap();
                assert!(channel.is_some());
                assert!(spawn.args[1].contains("codex login && exec env "));
                let (login_prefix, successor) = spawn.args[1].split_once(" && exec ").unwrap();
                let login_unsets = SERVICE_KEYS.map(|key| format!("-u {key}")).join(" ");
                assert!(login_prefix.ends_with(&format!("env {login_unsets} codex login")));
                for key in SERVICE_KEYS {
                    assert!(!login_prefix.contains(&format!("{key}=")));
                }
                assert!(successor.contains("MINDPLAYER_PER_PROMPT_CHOICE=1"));
                assert!(!spawn
                    .env
                    .set
                    .iter()
                    .any(|(key, _)| key == "MINDPLAYER_PER_PROMPT_CHOICE" || key == STATUS_ENV));
                let channel = channel.unwrap();
                assert_eq!(spawn.cwd, command.cwd);
                let mut child = std::process::Command::new(&spawn.program);
                child
                    .args(&spawn.args)
                    .current_dir(&spawn.cwd)
                    .env_clear()
                    .env("HOME", &home)
                    .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                    .env("FIXTURE_ROOT", &home);
                for key in SERVICE_KEYS {
                    child.env(key, "synthetic-service-canary");
                }
                for key in &spawn.env.unset {
                    child.env_remove(key);
                }
                child.envs(spawn.env.set.iter().cloned());
                let output = child.output().unwrap();
                assert!(output.status.success(), "{output:?}");
                let native_env = fs::read_to_string(home.join("native.env")).unwrap();
                let node_env = fs::read_to_string(home.join("node.env")).unwrap();
                for key in SERVICE_KEYS {
                    assert!(
                        !native_env
                            .lines()
                            .any(|line| line.starts_with(&format!("{key}="))),
                        "native login inherited {key}"
                    );
                    let value = match key {
                        "MINDPLAYER_PER_PROMPT_CHOICE" => "1".into(),
                        "MINDPLAYER_ROUTER_STATUS" => channel.path().to_string_lossy().into_owned(),
                        _ => "synthetic-service-canary".into(),
                    };
                    assert_eq!(
                        node_env
                            .lines()
                            .filter(|line| line.starts_with(&format!("{key}=")))
                            .collect::<Vec<_>>(),
                        [format!("{key}={value}")],
                    );
                }
                for env in [&native_env, &node_env] {
                    for (key, value) in account.launch_env().set.into_iter().chain([
                        ("HOME".into(), home.to_string_lossy().into_owned()),
                        (
                            "PWD".into(),
                            fs::canonicalize(&spawn.cwd)
                                .unwrap()
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    ]) {
                        assert!(env.lines().any(|line| line == format!("{key}={value}")));
                    }
                }
                assert_eq!(
                    fs::read(home.join("node.args")).unwrap(),
                    expected_node_args
                );
                let owned = channel.path().parent().unwrap().to_path_buf();
                drop(channel);
                assert!(!owned.exists());
                assert_eq!(command, original);
            }
            return;
        }
        let fixture = Fixture::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "local_router::status_tests::per_prompt_codex_new_resume_handoff_and_login_reach_the_owned_launcher", "--nocapture"])
            .env("HOME", &fixture.0).env("PATH", "").env(CHILD_HOME, &fixture.0)
            .env_remove("AI_GATEWAY_API_KEY").env_remove("TYPESAFE_API_KEY").env_remove("VERCEL_OIDC_TOKEN")
            .output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
