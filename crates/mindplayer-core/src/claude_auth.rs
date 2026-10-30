//! Bounded Claude subscription identity checks, independent of quota.
//!
//! Native `auth status` identifies the selected provider/configuration, but its
//! email/org are cached metadata. Success requires a separate authenticated GET
//! of the profile. That endpoint/shape is implementation-observed in Claude
//! Code 2.1.302, not a documented public API; unknown contracts fail closed.

use crate::accounts::{claude_launch_env, Account, Slot};
use crate::limits::{AccountIdentity, FailureKind, ProbeError};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTPUT: usize = 64 * 1024;
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

/// Verify this config home's subscription identity in that directory.
/// For launch checks use [`claude_account_in_at`] with the actual launch cwd.
pub fn claude_account_in(config_dir: &Path) -> Result<AccountIdentity, ProbeError> {
    claude_account_in_at(config_dir, config_dir)
}

/// Read native provider/config status in `cwd`, then REQUIRE a live profile.
/// Never uses quota, cached account metadata as proof, or a model/inference call.
/// This is always an EXPLICIT config override, even for `HOME/.claude`.
/// Use [`claude_account_for`] to preserve an inherited account's native vault.
/// Arbitrary settings/CLI overrides are not supported: callers must use the
/// account launch environment, and auth overrides in settings are refused.
pub fn claude_account_in_at(config_dir: &Path, cwd: &Path) -> Result<AccountIdentity, ProbeError> {
    probe(
        config_dir,
        false,
        cwd,
        &mut Command::new("claude"),
        &mut Command::new("curl"),
        TIMEOUT,
        subscription_token,
    )
}

/// Verify this exact account slot in the eventual launch's working directory.
/// Native default versus explicit-path vault selection comes from `Slot`, not
/// path equality: explicitly setting `HOME/.claude` selects a different vault.
pub fn claude_account_for(account: &Account, cwd: &Path) -> Result<AccountIdentity, ProbeError> {
    if account.provider != crate::session::Agent::Claude {
        return Err(unreadable());
    }
    let (config_dir, inherited) = match &account.slot {
        Slot::Inherited => (default_config_dir()?, true),
        Slot::Isolated { path } => (path.clone(), false),
    };
    probe(
        &config_dir,
        inherited,
        cwd,
        &mut Command::new("claude"),
        &mut Command::new("curl"),
        TIMEOUT,
        subscription_token,
    )
}

fn unreadable() -> ProbeError {
    ProbeError::new(
        FailureKind::Unreadable,
        "Claude account verification unavailable; check CLI version and account settings",
    )
}

fn temporary() -> ProbeError {
    ProbeError::new(
        FailureKind::Temporary,
        "Claude account check timed out or could not connect; retry",
    )
}

fn sign_in() -> ProbeError {
    ProbeError::new(FailureKind::SignIn, "Claude subscription sign-in required")
}

fn default_config_dir() -> Result<PathBuf, ProbeError> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude"))
        .ok_or_else(unreadable)
}

fn probe(
    config_dir: &Path,
    inherited: bool,
    cwd: &Path,
    native: &mut Command,
    http: &mut Command,
    timeout: Duration,
    token_lookup: impl FnOnce(&Path, bool, Instant) -> Result<String, ProbeError>,
) -> Result<AccountIdentity, ProbeError> {
    let deadline = Instant::now() + timeout;
    if !config_dir.is_absolute() || !cwd.is_absolute() {
        return Err(unreadable());
    }
    reject_settings_overrides(config_dir, cwd)?;
    let env = claude_launch_env((!inherited).then_some(config_dir));
    for command in [&mut *native, &mut *http] {
        command.current_dir(cwd);
        for (key, value) in &env.set {
            command.env(key, value);
        }
        for key in &env.unset {
            command.env_remove(key);
        }
    }
    native.args(["auth", "status", "--json"]);
    let status = run(native, &[], deadline)?;
    let configured = status_identity(&status, config_dir)?;
    let token = token_lookup(config_dir, inherited, deadline)?;
    let config = profile_request(&token)?;
    http.args(["-q", "--config", "-"]);
    // Never send credentials through an inherited proxy or curlrc.
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "CURL_CA_BUNDLE",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        http.env_remove(key);
    }
    let response = run(http, config.as_bytes(), deadline)?;
    let identity = profile_identity(&response)?;
    // Cached metadata can only veto a conflicting live identity, never prove it.
    if configured != (identity.email.clone(), identity.workspace.clone()) {
        return Err(unreadable());
    }
    Ok(identity)
}

fn safe_text(value: Option<&Value>) -> Result<&str, ProbeError> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && !s.chars().any(char::is_control))
        .ok_or_else(unreadable)
}

fn status_identity(output: &Output, config_dir: &Path) -> Result<(String, String), ProbeError> {
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(temporary());
    }
    let status: Value = serde_json::from_slice(&output.stdout).map_err(|_| unreadable())?;
    if status.get("loggedIn").and_then(Value::as_bool) == Some(false) {
        return Err(sign_in());
    }
    if !output.status.success()
        || status.get("loggedIn").and_then(Value::as_bool) != Some(true)
        || status.get("authMethod").and_then(Value::as_str) != Some("claude.ai")
        || status.get("apiProvider").and_then(Value::as_str) != Some("firstParty")
        || Path::new(safe_text(status.get("configDirectory"))?) != config_dir
    {
        return Err(unreadable());
    }
    Ok((
        safe_text(status.get("email"))?.trim().to_lowercase(),
        safe_text(status.get("orgId"))?.trim().to_string(),
    ))
}

fn profile_identity(output: &Output) -> Result<AccountIdentity, ProbeError> {
    if !output.status.success() {
        return Err(temporary());
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| unreadable())?;
    let (body, code) = text.rsplit_once('\n').ok_or_else(unreadable)?;
    match code.trim().parse::<u16>().map_err(|_| unreadable())? {
        200 => {}
        401 | 403 => return Err(sign_in()),
        429 => {
            return Err(ProbeError::new(
                FailureKind::RateLimited,
                "Claude profile rate limited",
            ))
        }
        500..=599 => return Err(temporary()),
        _ => return Err(unreadable()),
    }
    let profile: Value = serde_json::from_str(body).map_err(|_| unreadable())?;
    let account = safe_text(profile.pointer("/account/uuid"))?.trim();
    let email = safe_text(profile.pointer("/account/email"))?
        .trim()
        .to_lowercase();
    let workspace = safe_text(profile.pointer("/organization/uuid"))?
        .trim()
        .to_string();
    let mut hasher = DefaultHasher::new();
    ("claude", account, &workspace, &email).hash(&mut hasher);
    Ok(AccountIdentity {
        fingerprint: format!("claude-user-{:016x}", hasher.finish()),
        email,
        workspace,
    })
}

fn profile_request(token: &str) -> Result<String, ProbeError> {
    if token.is_empty() || !token.is_ascii() || token.chars().any(char::is_control) {
        return Err(unreadable());
    }
    let escaped = token.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!(
        "silent\nshow-error\nproto = \"=https\"\nmax-redirs = 0\nproxy = \"\"\nnoproxy = \"*\"\nconnect-timeout = 3\nmax-time = 8\nurl = \"{PROFILE_URL}\"\nheader = \"Authorization: Bearer {escaped}\"\nheader = \"Cache-Control: no-cache\"\nheader = \"Content-Type: application/json\"\nwrite-out = \"\\n%{{http_code}}\"\n"
    ))
}

fn reject_settings_overrides(config_dir: &Path, cwd: &Path) -> Result<(), ProbeError> {
    let mut paths = vec![config_dir.join("settings.json")];
    for parent in cwd.ancestors() {
        paths.push(parent.join(".claude/settings.json"));
        paths.push(parent.join(".claude/settings.local.json"));
    }
    #[cfg(target_os = "macos")]
    paths.push(PathBuf::from(
        "/Library/Application Support/ClaudeCode/managed-settings.json",
    ));
    #[cfg(not(target_os = "macos"))]
    paths.push(PathBuf::from("/etc/claude-code/managed-settings.json"));
    for path in paths {
        let Some(raw) = read_small(&path)? else {
            continue;
        };
        let settings: Value = serde_json::from_slice(&raw).map_err(|_| unreadable())?;
        if settings
            .get("apiKeyHelper")
            .is_some_and(|value| !value.is_null())
        {
            return Err(settings_override());
        }
        if let Some(env) = settings.get("env") {
            let env = env.as_object().ok_or_else(unreadable)?;
            if env.keys().any(|key| {
                key.starts_with("ANTHROPIC_")
                    || key.starts_with("CLAUDE_CODE_OAUTH_")
                    || key.starts_with("CLAUDE_CODE_USE_")
                    || matches!(
                        key.as_str(),
                        "CLAUDE_CONFIG_DIR" | "CLAUDE_SECURESTORAGE_CONFIG_DIR"
                    )
            }) {
                return Err(settings_override());
            }
        }
    }
    Ok(())
}

fn settings_override() -> ProbeError {
    ProbeError::new(FailureKind::Unreadable, "Claude auth overrides in settings prevent account verification; remove them for this subscription slot")
}

fn read_small(path: &Path) -> Result<Option<Vec<u8>>, ProbeError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unreadable()),
    };
    if !file.metadata().map_err(|_| unreadable())?.is_file() {
        return Err(unreadable());
    }
    let mut bytes = Vec::new();
    file.take(MAX_OUTPUT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() > MAX_OUTPUT {
        return Err(unreadable());
    }
    Ok(Some(bytes))
}

fn subscription_token(
    config_dir: &Path,
    inherited: bool,
    deadline: Instant,
) -> Result<String, ProbeError> {
    #[cfg(target_os = "macos")]
    {
        let service = keychain_service((!inherited).then_some(config_dir), deadline)?;
        let account = keychain_account();
        let output = run(
            Command::new("security").args([
                "find-generic-password",
                "-a",
                &account,
                "-w",
                "-s",
                &service,
            ]),
            &[],
            deadline.min(Instant::now() + Duration::from_secs(2)),
        )?;
        if output.status.success() {
            return token_from_blob(&output.stdout);
        }
        // Item-not-found is the only safe reason to try this same home's file.
        // Never search another service, account selector, or the global vault.
        if output.status.code() != Some(44) {
            return Err(temporary());
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (inherited, deadline);
    let raw = read_small(&config_dir.join(".credentials.json"))?.ok_or_else(sign_in)?;
    token_from_blob(&raw)
}

fn token_from_blob(raw: &[u8]) -> Result<String, ProbeError> {
    let raw = std::str::from_utf8(raw).map_err(|_| unreadable())?;
    crate::limits::token_from_credentials(raw).ok_or_else(sign_in)
}

#[cfg(target_os = "macos")]
fn keychain_account() -> String {
    let user = std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty())
        .or_else(|| {
            // Native os.userInfo().username fallback, without changing HOME.
            unsafe {
                let entry = libc::getpwuid(libc::geteuid());
                if entry.is_null() || (*entry).pw_name.is_null() {
                    return None;
                }
                Some(
                    std::ffi::CStr::from_ptr((*entry).pw_name)
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        });
    user.filter(|user| {
        user.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    })
    .unwrap_or_else(|| "claude-code-user".to_string())
}

#[cfg(target_os = "macos")]
fn keychain_service(
    config_override: Option<&Path>,
    deadline: Instant,
) -> Result<String, ProbeError> {
    let Some(path) = config_override else {
        return Ok("Claude Code-credentials".into());
    };
    let raw = path.to_str().ok_or_else(unreadable)?;
    // Unsupported Unicode normalization is refused rather than hashing a
    // different vault. Native uses NFC, without resolve/realpath/trim.
    if !raw.is_ascii() {
        return Err(unreadable());
    }
    let output = run(
        Command::new("/usr/bin/shasum").args(["-a", "256"]),
        raw.as_bytes(),
        deadline,
    )?;
    if !output.status.success() {
        return Err(temporary());
    }
    let digest = std::str::from_utf8(&output.stdout)
        .map_err(|_| unreadable())?
        .split_whitespace()
        .next()
        .ok_or_else(unreadable)?;
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(unreadable());
    }
    Ok(format!("Claude Code-credentials-{}", &digest[..8]))
}

pub(crate) fn run(
    command: &mut Command,
    input: &[u8],
    deadline: Instant,
) -> Result<Output, ProbeError> {
    run_inner(command, input, deadline, false)
}

/// Preserve the gateway key only for the OMP credential lookup.
pub(crate) fn run_with_gateway_key(
    command: &mut Command,
    input: &[u8],
    deadline: Instant,
) -> Result<Output, ProbeError> {
    run_inner(command, input, deadline, true)
}

#[cfg(unix)]
fn run_inner(
    command: &mut Command,
    input: &[u8],
    deadline: Instant,
    preserve_gateway_key: bool,
) -> Result<Output, ProbeError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            unsafe {
                libc::killpg(self.0.id() as libc::pid_t, libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
    fn nonblocking(fd: libc::c_int) -> Result<(), ProbeError> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(temporary());
        }
        Ok(())
    }
    fn drain(reader: &mut impl Read, capture: Option<&mut Vec<u8>>) -> Result<bool, ProbeError> {
        let mut capture = capture;
        let mut chunk = [0; 8192];
        for _ in 0..8 {
            match reader.read(&mut chunk) {
                Ok(0) => return Ok(true),
                Ok(n) => {
                    if let Some(bytes) = &mut capture {
                        if bytes.len() + n > MAX_OUTPUT {
                            return Err(unreadable());
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(temporary()),
            }
        }
        Ok(false)
    }
    if Instant::now() >= deadline {
        return Err(temporary());
    }
    for key in crate::accounts::ROUTER_SERVICE_ENV {
        if !preserve_gateway_key || *key != "AI_GATEWAY_API_KEY" {
            command.env_remove(key);
        }
    }
    let mut child = Child(
        command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| temporary())?,
    );
    let mut stdin = Some(child.0.stdin.take().ok_or_else(temporary)?);
    let mut stdout = child.0.stdout.take().ok_or_else(temporary)?;
    let mut stderr = child.0.stderr.take().ok_or_else(temporary)?;
    nonblocking(stdin.as_ref().unwrap().as_raw_fd())?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let mut sent = 0;
    let mut bytes = Vec::new();
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            return Err(temporary());
        }
        if let Some(writer) = stdin.as_mut() {
            if sent == input.len() {
                stdin.take();
            } else {
                match writer.write(&input[sent..]) {
                    Ok(n) => sent += n,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err(temporary()),
                }
            }
        }
        let stdout_done = drain(&mut stdout, Some(&mut bytes))?;
        let stderr_done = drain(&mut stderr, None)?;
        if status.is_none() {
            status = child.0.try_wait().map_err(|_| temporary())?;
        }
        if let Some(status) = status {
            if stdout_done && stderr_done {
                return Ok(Output {
                    status,
                    stdout: bytes,
                    stderr: Vec::new(),
                });
            }
            // A wrapper may exit while descendants still hold the pipes open.
            unsafe {
                libc::killpg(child.0.id() as libc::pid_t, libc::SIGKILL);
            }
        }
        std::thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(not(unix))]
fn run_inner(_: &mut Command, _: &[u8], _: Instant, _: bool) -> Result<Output, ProbeError> {
    Err(unreadable())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};

    fn output(value: Value, code: i32) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: Vec::new(),
        }
    }

    fn status(config: &Path) -> Value {
        serde_json::json!({"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
            "configDirectory":config,"email":" Person@Example.test ","orgId":"org-fixture"})
    }

    fn profile() -> Value {
        serde_json::json!({"account":{"uuid":"account-fixture","email":"person@example.test"},
            "organization":{"uuid":"org-fixture"}})
    }

    fn response(value: Value, code: u16) -> Output {
        let mut out = output(value, 0);
        out.stdout.extend_from_slice(format!("\n{code}").as_bytes());
        out
    }

    struct Fixture {
        root: tempfile::TempDir,
        config: PathBuf,
        cwd: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join(".claude");
            let cwd = root.path().join("project");
            for path in [&config, &cwd, &root.path().join("bin")] {
                std::fs::create_dir_all(path).unwrap();
            }
            // /var is a symlink on macOS: shells expose the physical cwd in
            // PWD. Keep config spelling raw (it selects the vault), but compare
            // the actual working directory rather than that symlink spelling.
            let cwd = cwd.canonicalize().unwrap();
            std::fs::write(
                root.path().join("status.json"),
                serde_json::to_vec(&status(&config)).unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.path().join("profile.json"),
                serde_json::to_vec(&profile()).unwrap(),
            )
            .unwrap();
            Self { root, config, cwd }
        }
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.path().join("bin").join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
        fn command(&self, name: &str, body: &str) -> Command {
            let mut command = Command::new(self.script(name, body));
            command
                .env_clear()
                .env("HOME", self.root.path())
                .env("PATH", "/usr/bin:/bin")
                .env("FIXTURE_ROOT", self.root.path())
                .env("EXPECTED_CONFIG", &self.config)
                .env("EXPECTED_CWD", &self.cwd)
                .env("CLAUDE_CONFIG_DIR", "/wrong/ambient")
                .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", "/wrong/vault")
                .env("ANTHROPIC_API_KEY", "synthetic-ambient-key")
                .env("ANTHROPIC_BASE_URL", "https://invalid.example");
            command
        }
        fn native(&self) -> Command {
            self.command("claude", "cat \"$FIXTURE_ROOT/status.json\"")
        }
        fn http(&self) -> Command {
            self.command("curl", "cat > \"$FIXTURE_ROOT/request\"\ncat \"$FIXTURE_ROOT/profile.json\"\nprintf '\\n200'")
        }
        fn probe(
            &self,
            native: &mut Command,
            http: &mut Command,
        ) -> Result<AccountIdentity, ProbeError> {
            probe(
                &self.config,
                false,
                &self.cwd,
                native,
                http,
                Duration::from_secs(2),
                |_, _, _| Ok("synthetic-token".into()),
            )
        }
    }

    #[test]
    fn native_auth_probes_remove_service_environment_from_native_and_http_children() {
        let f = Fixture::new();
        let mut native = f.command(
            "claude",
            "/usr/bin/env > \"$FIXTURE_ROOT/native.env\"\ncat \"$FIXTURE_ROOT/status.json\"",
        );
        let mut http = f.command("curl", "/usr/bin/env > \"$FIXTURE_ROOT/http.env\"\ncat > \"$FIXTURE_ROOT/request\"\ncat \"$FIXTURE_ROOT/profile.json\"\nprintf '\\n200'");
        for command in [&mut native, &mut http] {
            for key in [
                "AI_GATEWAY_API_KEY",
                "TYPESAFE_API_KEY",
                "VERCEL_OIDC_TOKEN",
                "MINDPLAYER_LOCAL_ROUTER_TOKEN",
                "MINDPLAYER_ROUTER_AVAILABILITY",
                "MINDPLAYER_ROUTER_STATUS",
                "MINDPLAYER_CLAUDE_JEV_URL",
                "MINDPLAYER_CLAUDE_JEV_TOKEN",
                "MINDPLAYER_PER_PROMPT_CHOICE",
                "MINDPLAYER_JEV_DEFAULT",
            ] {
                command.env(key, "synthetic-service-canary");
            }
        }
        assert_eq!(
            f.probe(&mut native, &mut http).unwrap().email,
            "person@example.test"
        );
        let leaked: Vec<_> = ["native.env", "http.env"]
            .into_iter()
            .filter(|file| {
                std::fs::read_to_string(f.root.path().join(file))
                    .unwrap()
                    .contains("synthetic-service-canary")
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "service environment leaked to {leaked:?}"
        );
    }

    #[test]
    fn native_auth_credential_tools_remove_service_environment() {
        let f = Fixture::new();
        let mut command = f.command("security", "/usr/bin/env");
        for key in [
            "AI_GATEWAY_API_KEY",
            "TYPESAFE_API_KEY",
            "VERCEL_OIDC_TOKEN",
            "MINDPLAYER_LOCAL_ROUTER_TOKEN",
            "MINDPLAYER_ROUTER_AVAILABILITY",
            "MINDPLAYER_ROUTER_STATUS",
            "MINDPLAYER_CLAUDE_JEV_URL",
            "MINDPLAYER_CLAUDE_JEV_TOKEN",
            "MINDPLAYER_PER_PROMPT_CHOICE",
            "MINDPLAYER_JEV_DEFAULT",
        ] {
            command.env(key, "synthetic-service-canary");
        }
        let output = run(&mut command, &[], Instant::now() + Duration::from_secs(2)).unwrap();
        assert!(output.status.success());
        assert!(!String::from_utf8(output.stdout)
            .unwrap()
            .contains("synthetic-service-canary"));
    }

    #[test]
    fn http_receives_the_selected_launch_environment_and_cwd() {
        let f = Fixture::new();
        let mut http = f.command("curl", "printf '%s\\n' \"$PWD\" \"$EXPECTED_CWD\" \"$CLAUDE_CONFIG_DIR\" \"$EXPECTED_CONFIG\" \"${CLAUDE_SECURESTORAGE_CONFIG_DIR+x}${ANTHROPIC_API_KEY+x}${ANTHROPIC_BASE_URL+x}\" > \"$FIXTURE_ROOT/http.context\"\n[ \"$PWD\" = \"$EXPECTED_CWD\" ] || exit 71\n[ \"$CLAUDE_CONFIG_DIR\" = \"$EXPECTED_CONFIG\" ] || exit 72\n[ -z \"${CLAUDE_SECURESTORAGE_CONFIG_DIR+x}${ANTHROPIC_API_KEY+x}${ANTHROPIC_BASE_URL+x}\" ] || exit 73\ncat > \"$FIXTURE_ROOT/request\"\ncat \"$FIXTURE_ROOT/profile.json\"\nprintf '\\n200'");
        let actual = f.probe(&mut f.native(), &mut http).unwrap_or_else(|error| {
            panic!(
                "{error}: {:?}",
                std::fs::read_to_string(f.root.path().join("http.context"))
            )
        });
        assert_eq!(actual.email, "person@example.test");
        assert_eq!(actual.workspace, "org-fixture");
    }

    #[test]
    fn metadata_success_still_requires_the_live_profile() {
        let f = Fixture::new();
        let mut http = f.command(
            "curl",
            "cat >/dev/null\nprintf 'provider-secret-error\\n401'",
        );
        let error = f.probe(&mut f.native(), &mut http).unwrap_err();
        assert_eq!(error.kind, FailureKind::SignIn);
        assert!(!error.detail.contains("provider-secret"));
        let mut offline = f.command("offline", "printf 'token-secret' >&2\nexit 7");
        assert_eq!(
            f.probe(&mut f.native(), &mut offline).unwrap_err().kind,
            FailureKind::Temporary
        );
    }

    #[test]
    fn signed_out_rejects_before_reading_credentials_or_http() {
        let f = Fixture::new();
        let mut native = f.command(
            "claude",
            "printf '{\"loggedIn\":false,\"authMethod\":\"none\"}'\nexit 1",
        );
        let error = probe(
            &f.config,
            false,
            &f.cwd,
            &mut native,
            &mut f.http(),
            Duration::from_secs(2),
            |_, _, _| panic!("signed-out status must not access credentials"),
        )
        .unwrap_err();
        assert_eq!(error.kind, FailureKind::SignIn);
        assert!(!f.root.path().join("request").exists());
    }

    #[test]
    fn status_rejects_other_auth_sources_routing_and_missing_identity() {
        let config = Path::new("/synthetic/profile");
        for (key, replacement) in [
            ("authMethod", serde_json::json!("api_key")),
            ("authMethod", serde_json::json!("oauth_token")),
            ("authMethod", serde_json::json!("api_key_helper")),
            ("apiProvider", serde_json::json!("gateway")),
            ("configDirectory", serde_json::json!("/wrong/profile")),
            ("email", Value::Null),
            ("orgId", Value::Null),
            ("email", serde_json::json!("person\u{1b}@example.test")),
            ("orgId", serde_json::json!("org\nunsafe")),
        ] {
            let mut value = status(config);
            value[key] = replacement;
            assert_eq!(
                status_identity(&output(value, 0), config).unwrap_err().kind,
                FailureKind::Unreadable,
                "{key}"
            );
        }
    }

    #[test]
    fn live_identity_requires_all_source_traced_fields_and_safe_display() {
        for pointer in ["/account/uuid", "/account/email", "/organization/uuid"] {
            for invalid in [
                Value::Null,
                serde_json::json!(" "),
                serde_json::json!("unsafe\nfield"),
                serde_json::json!(false),
            ] {
                let mut body = profile();
                *body.pointer_mut(pointer).unwrap() = invalid;
                assert_eq!(
                    profile_identity(&response(body, 200)).unwrap_err().kind,
                    FailureKind::Unreadable
                );
            }
        }
        let identity = profile_identity(&response(profile(), 200)).unwrap();
        let mut normalized = profile();
        normalized["account"]["email"] = serde_json::json!(" PERSON@EXAMPLE.TEST ");
        assert_eq!(
            identity,
            profile_identity(&response(normalized, 200)).unwrap()
        );
        let mut different = profile();
        different["account"]["uuid"] = serde_json::json!("another-account");
        assert_ne!(
            identity.fingerprint,
            profile_identity(&response(different, 200))
                .unwrap()
                .fingerprint
        );
    }

    #[test]
    fn live_profile_error_kinds_are_typed_and_do_not_expose_provider_errors() {
        for (code, kind) in [
            (401, FailureKind::SignIn),
            (403, FailureKind::SignIn),
            (429, FailureKind::RateLimited),
            (503, FailureKind::Temporary),
            (302, FailureKind::Unreadable),
        ] {
            let error = profile_identity(&response(
                serde_json::json!({"error":"raw-secret-provider-message"}),
                code,
            ))
            .unwrap_err();
            assert_eq!(error.kind, kind);
            assert!(!error.detail.contains("raw-secret"));
        }
    }

    #[test]
    fn native_metadata_cannot_override_a_conflicting_live_profile() {
        let f = Fixture::new();
        let mut body = profile();
        body["organization"]["uuid"] = serde_json::json!("other-org");
        std::fs::write(
            f.root.path().join("profile.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert_eq!(
            f.probe(&mut f.native(), &mut f.http()).unwrap_err().kind,
            FailureKind::Unreadable
        );
    }

    #[test]
    fn settings_cannot_restore_an_ambient_auth_source_or_execute_a_helper() {
        let f = Fixture::new();
        for settings in [
            serde_json::json!({"env":{"ANTHROPIC_API_KEY":"synthetic"}}),
            serde_json::json!({"apiKeyHelper":"must-not-execute"}),
        ] {
            std::fs::write(
                f.config.join("settings.json"),
                serde_json::to_vec(&settings).unwrap(),
            )
            .unwrap();
            let error = probe(
                &f.config,
                false,
                &f.cwd,
                &mut f.native(),
                &mut f.http(),
                Duration::from_secs(1),
                |_, _, _| panic!("settings rejected before credentials"),
            )
            .unwrap_err();
            assert_eq!(error.kind, FailureKind::Unreadable);
            assert!(!f.root.path().join("request").exists());
        }
    }

    #[test]
    fn expired_or_unsafe_credentials_never_become_a_profile_request() {
        for blob in [
            r#"{"claudeAiOauth":{"accessToken":"synthetic","expiresAt":1}}"#,
            r#"{"claudeAiOauth":{"accessToken":"bad\nconfig"}}"#,
            r#"{}"#,
        ] {
            assert!(token_from_blob(blob.as_bytes()).is_err());
        }
        for token in ["", "bad\nurl", "bad\rtoken", "non-ascii-é"] {
            assert!(profile_request(token).is_err());
        }
        let config = profile_request("synthetic-token").unwrap();
        assert!(config.contains(PROFILE_URL));
        assert!(config.contains("Cache-Control: no-cache"));
        assert!(!config.contains("insecure"));
        assert!(!config.contains("location"));
    }

    #[test]
    fn runner_drains_stderr_and_keeps_tokens_off_argv_and_errors() {
        let f = Fixture::new();
        let mut http = f.command("curl", "printf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/argv\"\ni=0; while [ \"$i\" -lt 2000 ]; do printf 'sensitive-stderr-contents-long-line\\n' >&2; i=$((i+1)); done\ncat > \"$FIXTURE_ROOT/request\"\ncat \"$FIXTURE_ROOT/profile.json\"\nprintf '\\n200'");
        f.probe(&mut f.native(), &mut http).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.root.path().join("argv")).unwrap(),
            "-q\n--config\n-\n"
        );
        assert!(std::fs::read_to_string(f.root.path().join("request"))
            .unwrap()
            .contains("Bearer synthetic-token"));
    }

    #[test]
    fn runner_bounds_stdout_and_blocked_stdin() {
        let f = Fixture::new();
        let mut flood = f.command("flood", "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done");
        assert_eq!(
            run(&mut flood, &[], Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .kind,
            FailureKind::Unreadable
        );
        let mut blocked = f.command("blocked", "sleep 10");
        let start = Instant::now();
        assert_eq!(
            run(
                &mut blocked,
                &vec![0; 1024 * 1024],
                start + Duration::from_millis(120)
            )
            .unwrap_err()
            .kind,
            FailureKind::Temporary
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn runner_timeout_kills_and_reaps_the_process_group() {
        let f = Fixture::new();
        let mut command = f.command(
            "timeout",
            "sleep 10 &\nprintf '%s %s' \"$$\" \"$!\" > \"$FIXTURE_ROOT/pids\"\nwait",
        );
        assert_eq!(
            run(&mut command, &[], Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .kind,
            FailureKind::Temporary
        );
        let pids = std::fs::read_to_string(f.root.path().join("pids")).unwrap();
        for pid in pids
            .split_whitespace()
            .map(|pid| pid.parse::<i32>().unwrap())
        {
            let start = Instant::now();
            while unsafe { libc::kill(pid, 0) } == 0 && start.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_ne!(
                unsafe { libc::kill(pid, 0) },
                0,
                "fixture process {pid} survived"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_vault_service_uses_raw_path_spelling_not_path_equality() {
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(
            keychain_service(None, deadline).unwrap(),
            "Claude Code-credentials"
        );
        assert_eq!(
            keychain_service(Some(Path::new("/synthetic/home/.claude")), deadline).unwrap(),
            "Claude Code-credentials-2eb62ada"
        );
        assert_eq!(
            keychain_service(Some(Path::new("/synthetic/profile-B")), deadline).unwrap(),
            "Claude Code-credentials-e8280f5e"
        );
        assert_ne!(
            keychain_service(Some(Path::new("/synthetic/./profile-B")), deadline).unwrap(),
            keychain_service(Some(Path::new("/synthetic/profile-B")), deadline).unwrap()
        );
    }

    // Only a subprocess gets fake HOME/PATH. Its security executable is an
    // explicit fixture: fake HOME alone would NOT isolate the macOS Keychain.
    #[test]
    fn native_profile_fixture_child() {
        let Some(mode) = std::env::var_os("MINDPLAYER_CLAUDE_FIXTURE") else {
            return;
        };
        let cwd = PathBuf::from(std::env::var_os("EXPECTED_CWD").unwrap());
        let config = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".claude");
        let mut account = Account::inherited(crate::session::Agent::Claude);
        if mode == "explicit" {
            account.slot = Slot::Isolated {
                path: config.clone(),
            };
        }
        let identity = claude_account_for(&account, &cwd).unwrap();
        assert_eq!(identity.email, "person@example.test");
        assert_eq!(identity.workspace, "org-fixture");
    }

    #[test]
    fn production_probe_keeps_default_and_explicit_same_path_vaults_distinct() {
        for mode in ["inherited", "explicit"] {
            let f = Fixture::new();
            std::fs::write(
                f.config.join(".credentials.json"),
                br#"{"claudeAiOauth":{"accessToken":"synthetic-token","expiresAt":4102444800000}}"#,
            )
            .unwrap();
            f.script(
                "security",
                "printf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/security.args\"\nexit 44",
            );
            let context = "[ \"$PWD\" = \"$EXPECTED_CWD\" ] || exit 71\nif [ \"$MINDPLAYER_CLAUDE_FIXTURE\" = inherited ]; then [ -z \"${CLAUDE_CONFIG_DIR+x}\" ] || exit 72; else [ \"$CLAUDE_CONFIG_DIR\" = \"$EXPECTED_CONFIG\" ] || exit 73; fi\n[ -z \"${CLAUDE_SECURESTORAGE_CONFIG_DIR+x}${ANTHROPIC_API_KEY+x}${ANTHROPIC_CUSTOM_HEADERS+x}${CLAUDE_CODE_OAUTH_REFRESH_TOKEN+x}${CLAUDE_CODE_OAUTH_SCOPES+x}\" ] || exit 74\n";
            f.script(
                "claude",
                &format!("{context}cat \"$FIXTURE_ROOT/status.json\""),
            );
            f.script("curl", &format!("{context}printf '%s\\n' \"$@\" > \"$FIXTURE_ROOT/curl.args\"\ncat > \"$FIXTURE_ROOT/request\"\ncat \"$FIXTURE_ROOT/profile.json\"\nprintf '\\n200'"));
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "claude_auth::tests::native_profile_fixture_child",
                    "--test-threads=1",
                ])
                .env_clear()
                .env("HOME", f.root.path())
                .env("USER", "synthetic-fixture-user")
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", f.root.path().join("bin").display()),
                )
                .env("FIXTURE_ROOT", f.root.path())
                .env("EXPECTED_CWD", &f.cwd)
                .env("EXPECTED_CONFIG", &f.config)
                .env("MINDPLAYER_CLAUDE_FIXTURE", mode);
            for key in [
                "CLAUDE_CONFIG_DIR",
                "CLAUDE_SECURESTORAGE_CONFIG_DIR",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_CUSTOM_HEADERS",
                "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
                "CLAUDE_CODE_OAUTH_SCOPES",
            ] {
                child.env(key, "synthetic-wrong-ambient");
            }
            let output = run(&mut child, &[], Instant::now() + Duration::from_secs(5)).unwrap();
            assert!(
                output.status.success(),
                "fixture {mode}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            #[cfg(target_os = "macos")]
            {
                let args = std::fs::read_to_string(f.root.path().join("security.args")).unwrap();
                let service = args.lines().last().unwrap();
                assert!(args.contains("synthetic-fixture-user"));
                if mode == "inherited" {
                    assert_eq!(service, "Claude Code-credentials");
                } else {
                    assert_eq!(
                        service,
                        keychain_service(Some(&f.config), Instant::now() + Duration::from_secs(1))
                            .unwrap()
                    );
                    assert_ne!(service, "Claude Code-credentials");
                }
            }
            assert_eq!(
                std::fs::read_to_string(f.root.path().join("curl.args")).unwrap(),
                "-q\n--config\n-\n"
            );
            let request = std::fs::read_to_string(f.root.path().join("request")).unwrap();
            assert!(request.contains("Bearer synthetic-token"));
            assert!(!request.contains("synthetic-wrong-ambient"));
        }
    }
}
