//! Read-only budget for OMP's configured Vercel AI Gateway API key.

use std::collections::HashSet;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(60);
const QUOTAS_URL: &str = "https://ai-gateway.vercel.sh/v1/quotas";
const MAX_PAGES: usize = 100;
const MAX_TARGET_CONFIG_BYTES: u64 = 16 * 1024;

/// The selected API key's spending and budget limit, in dollars.
#[derive(Debug, Clone, PartialEq)]
pub struct VercelBudget {
    pub used: f64,
    pub limit: f64,
    pub refresh_period: String,
}

/// The selected API key's budget and optional local spending target.
#[derive(Debug, Clone, PartialEq)]
pub struct VercelUsage {
    pub key: Result<VercelBudget, VercelUsageError>,
    /// A local target in dollars, not a Vercel-enforced limit.
    pub target: Option<f64>,
}

/// Sanitized failure to read or identify a Vercel budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VercelUsageError {
    /// OMP did not return a usable Gateway credential.
    NotConfigured,
    /// Credential lookup, network access, or Vercel CLI access failed.
    Unavailable,
    /// An upstream response was invalid or unsupported.
    InvalidResponse,
    /// The selected key could not be uniquely identified.
    KeyNotIdentified,
    /// The selected key has no active budget.
    NoBudget,
}

impl fmt::Display for VercelUsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConfigured => "vercel gateway credential not configured",
            Self::Unavailable => "vercel budget unavailable",
            Self::InvalidResponse => "invalid vercel budget response",
            Self::KeyNotIdentified => "vercel gateway key not uniquely identified",
            Self::NoBudget => "vercel scope has no active budget",
        })
    }
}

impl std::error::Error for VercelUsageError {}

/// Fetch the configured key's budget and its optional local target.
///
/// # Errors
/// Returns a sanitized error if the credential, key identity, or local target is unusable.
/// A budget lookup failure is retained in the returned usage.
pub fn fetch_vercel_usage(cwd: &Path) -> Result<VercelUsage, VercelUsageError> {
    let deadline = Instant::now() + TIMEOUT;
    let mut omp = Command::new("omp");
    omp.current_dir(cwd).args(["token", "vercel-ai-gateway"]);
    let output = crate::claude_auth::run_with_gateway_key(&mut omp, &[], deadline)
        .map_err(|_| VercelUsageError::Unavailable)?;
    if !output.status.success() {
        return Err(VercelUsageError::NotConfigured);
    }
    let token = credential(&output.stdout)?;
    if token.len() < 6 || !token.is_ascii() {
        return Err(VercelUsageError::KeyNotIdentified);
    }
    let suffix = &token[token.len() - 6..];
    // Validated bearer characters prevent injection into curl's stdin configuration.
    let config = format!(
        "silent\nproto = \"=https\"\nmax-redirs = 0\nproxy = \"\"\nnoproxy = \"*\"\nconnect-timeout = 3\nmax-time = 8\nurl = \"{QUOTAS_URL}\"\nheader = \"Authorization: Bearer {token}\"\nwrite-out = \"\\n%{{http_code}}\"\n"
    );
    let mut curl = Command::new("curl");
    curl.current_dir(cwd).args(["-q", "--config", "-"]);
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
        "SSLKEYLOGFILE",
        "AI_GATEWAY_API_KEY",
        "VERCEL_OIDC_TOKEN",
    ] {
        curl.env_remove(key);
    }
    let team = parse_quotas(&run(&mut curl, config.as_bytes(), deadline)?)?;
    if team.contains(suffix) {
        return Err(VercelUsageError::KeyNotIdentified);
    }
    let mut matches = Vec::new();
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut cli = Command::new("vercel");
        cli.current_dir(cwd).args([
            "ai-gateway",
            "api-keys",
            "list",
            "--json",
            "--state",
            "active",
            "--limit",
            "100",
            "--scope",
            &team,
            "--non-interactive",
        ]);
        cli.env_remove("AI_GATEWAY_API_KEY")
            .env_remove("VERCEL_OIDC_TOKEN");
        if let Some(next) = &cursor {
            cli.arg(format!("--next={next}"));
        }
        let output = run(&mut cli, &[], deadline)?;
        if !output.status.success() {
            return Err(VercelUsageError::Unavailable);
        }
        cursor = parse_keys(&output.stdout, &team, suffix, &mut matches)?;
        match &cursor {
            None => break,
            Some(next) if !next.contains(suffix) && cursors.insert(next.clone()) => {}
            Some(_) => return Err(VercelUsageError::KeyNotIdentified),
        }
    }
    let key_id = selected_key(&matches, cursor.as_deref())?;
    if key_id.contains(suffix) {
        return Err(VercelUsageError::KeyNotIdentified);
    }
    let key = inspect_budget(cwd, &team, key_id, deadline);
    let target = match std::env::var_os("HOME") {
        Some(home) => read_target(
            &Path::new(&home).join(".mindplayer/vercel-key-target.json"),
            key_id,
            key.as_ref().ok(),
        )?,
        None => None,
    };
    Ok(VercelUsage { key, target })
}

fn read_target(
    path: &Path,
    key_id: &str,
    budget: Option<&VercelBudget>,
) -> Result<Option<f64>, VercelUsageError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(VercelUsageError::Unavailable),
    };
    let mut bytes = Vec::new();
    file.take(MAX_TARGET_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| VercelUsageError::Unavailable)?;
    if bytes.len() as u64 > MAX_TARGET_CONFIG_BYTES {
        return Err(VercelUsageError::InvalidResponse);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| VercelUsageError::InvalidResponse)?;
    let configured_key = value
        .get("key_id")
        .and_then(|v| v.as_str())
        .ok_or(VercelUsageError::InvalidResponse)?;
    if configured_key != key_id {
        return Ok(None);
    }
    let target = value
        .get("target_usd")
        .and_then(|v| v.as_f64())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or(VercelUsageError::InvalidResponse)?;
    let period = value
        .get("refresh_period")
        .and_then(|v| v.as_str())
        .ok_or(VercelUsageError::InvalidResponse)?;
    let Some(budget) = budget else {
        return Ok(None);
    };
    if period != budget.refresh_period {
        return Err(VercelUsageError::InvalidResponse);
    }
    Ok(Some(target))
}

fn inspect_budget(
    cwd: &Path,
    team: &str,
    key_id: &str,
    deadline: Instant,
) -> Result<VercelBudget, VercelUsageError> {
    let mut cli = Command::new("vercel");
    cli.current_dir(cwd)
        .args(["ai-gateway", "budgets", "inspect", "api-key", key_id]);
    cli.args(["--json", "--scope", team, "--non-interactive"])
        .env_remove("AI_GATEWAY_API_KEY")
        .env_remove("VERCEL_OIDC_TOKEN");
    let output = run(&mut cli, &[], deadline)?;
    if !output.status.success() {
        return Err(VercelUsageError::Unavailable);
    }
    parse_budget(&output.stdout, "api-key", key_id)
}

fn run(command: &mut Command, input: &[u8], deadline: Instant) -> Result<Output, VercelUsageError> {
    crate::claude_auth::run(command, input, deadline).map_err(|_| VercelUsageError::Unavailable)
}

fn credential(output: &[u8]) -> Result<&str, VercelUsageError> {
    let output = output.strip_suffix(b"\n").unwrap_or(output);
    let output = output.strip_suffix(b"\r").unwrap_or(output);
    let token = std::str::from_utf8(output).map_err(|_| VercelUsageError::NotConfigured)?;
    let base = token.trim_end_matches('=');
    if base.is_empty()
        || !base
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
    {
        return Err(VercelUsageError::NotConfigured);
    }
    Ok(token)
}

fn safe_id(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn parse_quotas(output: &Output) -> Result<String, VercelUsageError> {
    if !output.status.success() {
        return Err(VercelUsageError::Unavailable);
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| VercelUsageError::InvalidResponse)?;
    let (body, status) = text
        .rsplit_once('\n')
        .ok_or(VercelUsageError::InvalidResponse)?;
    if status != "200" {
        return Err(VercelUsageError::Unavailable);
    }
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| VercelUsageError::InvalidResponse)?;
    let quotas = value
        .get("quotas")
        .and_then(|v| v.as_array())
        .ok_or(VercelUsageError::InvalidResponse)?;
    let mut team: Option<&str> = None;
    for quota in quotas {
        let id = quota
            .get("teamId")
            .and_then(|v| v.as_str())
            .filter(|id| safe_id(id))
            .ok_or(VercelUsageError::InvalidResponse)?;
        if team.is_some_and(|previous| previous != id) {
            return Err(VercelUsageError::KeyNotIdentified);
        }
        team = Some(id);
    }
    team.map(str::to_owned)
        .ok_or(VercelUsageError::KeyNotIdentified)
}

fn parse_keys(
    output: &[u8],
    team: &str,
    suffix: &str,
    matches: &mut Vec<String>,
) -> Result<Option<String>, VercelUsageError> {
    let value: serde_json::Value =
        serde_json::from_slice(output).map_err(|_| VercelUsageError::InvalidResponse)?;
    let keys = value
        .get("apiKeys")
        .and_then(|v| v.as_array())
        .ok_or(VercelUsageError::InvalidResponse)?;
    if keys.len() > 100 {
        return Err(VercelUsageError::InvalidResponse);
    }
    for key in keys {
        let id = key
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|id| safe_id(id))
            .ok_or(VercelUsageError::InvalidResponse)?;
        if key.get("teamId").and_then(|v| v.as_str()) != Some(team) {
            return Err(VercelUsageError::InvalidResponse);
        }
        let partial = key
            .get("partialKey")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() == 6 && s.is_ascii())
            .ok_or(VercelUsageError::InvalidResponse)?;
        if partial == suffix {
            matches.push(id.to_owned());
        }
    }
    let next = value
        .get("pagination")
        .and_then(|v| v.get("next"))
        .ok_or(VercelUsageError::InvalidResponse)?;
    if next.is_null() {
        Ok(None)
    } else {
        let next = next
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 4096
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .ok_or(VercelUsageError::InvalidResponse)?;
        Ok(Some(next.to_owned()))
    }
}

fn selected_key<'a>(
    matches: &'a [String],
    cursor: Option<&str>,
) -> Result<&'a str, VercelUsageError> {
    match (matches, cursor) {
        ([id], None) => Ok(id),
        _ => Err(VercelUsageError::KeyNotIdentified),
    }
}

fn decimal(value: Option<&serde_json::Value>) -> Result<f64, VercelUsageError> {
    let value = value.ok_or(VercelUsageError::InvalidResponse)?;
    let number = match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    };
    number
        .filter(|n| n.is_finite())
        .ok_or(VercelUsageError::InvalidResponse)
}

fn parse_budget(
    output: &[u8],
    scope_type: &str,
    scope_id: &str,
) -> Result<VercelBudget, VercelUsageError> {
    let value: serde_json::Value =
        serde_json::from_slice(output).map_err(|_| VercelUsageError::InvalidResponse)?;
    if value.is_null() {
        return Err(VercelUsageError::NoBudget);
    }
    if value.get("scopeType").and_then(|v| v.as_str()) != Some(scope_type)
        || value.get("scopeId").and_then(|v| v.as_str()) != Some(scope_id)
    {
        return Err(VercelUsageError::InvalidResponse);
    }
    match (
        value.get("active").and_then(|v| v.as_bool()),
        value.get("archived").and_then(|v| v.as_bool()),
    ) {
        (Some(true), Some(false)) => {}
        (Some(_), Some(_)) => return Err(VercelUsageError::NoBudget),
        _ => return Err(VercelUsageError::InvalidResponse),
    }
    let limit = decimal(value.get("limitAmount"))?;
    let used = decimal(value.get("currentSpend"))?;
    if limit <= 0.0 || used < 0.0 {
        return Err(VercelUsageError::InvalidResponse);
    }
    let period = value
        .get("refreshPeriod")
        .and_then(|v| v.as_str())
        .filter(|s| matches!(*s, "daily" | "weekly" | "monthly" | "none"))
        .ok_or(VercelUsageError::InvalidResponse)?;
    Ok(VercelBudget {
        used,
        limit,
        refresh_period: period.to_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn response(body: &str, status: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: format!("{body}\n{status}").into_bytes(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn omp_lookup_preserves_only_the_gateway_credential() {
        fn command() -> Command {
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "printf '%s|%s' \"$AI_GATEWAY_API_KEY\" \"$TYPESAFE_API_KEY\"",
                ])
                .env_clear()
                .env("AI_GATEWAY_API_KEY", "gateway-fixture")
                .env("TYPESAFE_API_KEY", "other-fixture");
            command
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let output = crate::claude_auth::run_with_gateway_key(&mut command(), &[], deadline)
            .expect("OMP lookup command");
        assert_eq!(output.stdout, b"gateway-fixture|");
        let output =
            crate::claude_auth::run(&mut command(), &[], deadline).expect("ordinary child command");
        assert_eq!(output.stdout, b"|");
    }

    #[test]
    fn quota_identity_requires_one_team() {
        assert_eq!(
            parse_quotas(&response(
                r#"{"quotas":[{"teamId":"team_a"},{"teamId":"team_a"}]}"#,
                "200"
            )),
            Ok("team_a".into())
        );
        for body in [
            r#"{"quotas":[]}"#,
            r#"{"quotas":[{"teamId":"team_a"},{"teamId":"team_b"}]}"#,
            r#"{"quotas":[{}]}"#,
            r#"{}"#,
        ] {
            assert!(parse_quotas(&response(body, "200")).is_err());
        }
        assert_eq!(
            parse_quotas(&response("{}", "401")),
            Err(VercelUsageError::Unavailable)
        );
    }

    #[test]
    fn keys_require_full_pages_and_detect_duplicate_matches() {
        let mut matches = Vec::new();
        assert_eq!(parse_keys(br#"{"apiKeys":[{"id":"key_one","teamId":"team_a","partialKey":"abcdef"}],"pagination":{"next":"cursor_2"}}"#, "team_a", "abcdef", &mut matches), Ok(Some("cursor_2".into())));
        assert_eq!(parse_keys(br#"{"apiKeys":[{"id":"key_two","teamId":"team_a","partialKey":"abcdef"}],"pagination":{"next":null}}"#, "team_a", "abcdef", &mut matches), Ok(None));
        assert_eq!(matches.len(), 2);
        assert_eq!(
            selected_key(&matches, None),
            Err(VercelUsageError::KeyNotIdentified)
        );
        assert_eq!(
            selected_key(&matches[..1], Some("cursor_2")),
            Err(VercelUsageError::KeyNotIdentified)
        );
        assert_eq!(selected_key(&matches[..1], None), Ok("key_one"));
        assert_eq!(
            selected_key(&[], None),
            Err(VercelUsageError::KeyNotIdentified)
        );
        for body in [br#"{"apiKeys":[]}"#.as_slice(), br#"{"apiKeys":[{"id":"key_one","teamId":"team_a"}],"pagination":{"next":null}}"#, br#"{"apiKeys":[{"id":"key_one","teamId":"team_b","partialKey":"abcdef"}],"pagination":{"next":null}}"#, br#"{"apiKeys":[],"pagination":{}}"#, br#"{"apiKeys":[],"pagination":{"next":""}}"#] {
            assert!(parse_keys(body, "team_a", "abcdef", &mut Vec::new()).is_err());
        }
    }

    #[test]
    fn budget_is_bound_to_key_and_requires_valid_values() {
        let valid = br#"{"scopeType":"api-key","scopeId":"key_one","limitAmount":"100.00","currentSpend":"12.25","refreshPeriod":"monthly","active":true,"archived":false}"#;
        assert_eq!(
            parse_budget(valid, "api-key", "key_one"),
            Ok(VercelBudget {
                used: 12.25,
                limit: 100.0,
                refresh_period: "monthly".into()
            })
        );
        assert_eq!(
            parse_budget(valid, "api-key", "key_two"),
            Err(VercelUsageError::InvalidResponse)
        );
        for body in [
            r#"{"scopeType":"team","scopeId":"key_one","limitAmount":1,"currentSpend":0,"refreshPeriod":"monthly","active":true,"archived":false}"#,
            r#"{"scopeType":"api-key","scopeId":"key_one","limitAmount":0,"currentSpend":0,"refreshPeriod":"monthly","active":true,"archived":false}"#,
            r#"{"scopeType":"api-key","scopeId":"key_one","limitAmount":1,"currentSpend":-1,"refreshPeriod":"monthly","active":true,"archived":false}"#,
            r#"{"scopeType":"api-key","scopeId":"key_one","limitAmount":"NaN","currentSpend":0,"refreshPeriod":"monthly","active":true,"archived":false}"#,
            r#"{"scopeType":"api-key","scopeId":"key_one","limitAmount":1,"currentSpend":0,"refreshPeriod":"yearly","active":true,"archived":false}"#,
        ] {
            assert_eq!(
                parse_budget(body.as_bytes(), "api-key", "key_one"),
                Err(VercelUsageError::InvalidResponse)
            );
        }
        assert_eq!(
            parse_budget(b"null", "api-key", "key_one"),
            Err(VercelUsageError::NoBudget)
        );
        let inactive =
            br#"{"scopeType":"api-key","scopeId":"key_one","active":false,"archived":false}"#;
        assert_eq!(
            parse_budget(inactive, "api-key", "key_one"),
            Err(VercelUsageError::NoBudget)
        );
    }

    #[test]
    fn local_target_is_scoped_to_key_and_preserves_actual_budget() {
        let dir = tempfile::tempdir().expect("temporary config directory");
        let path = dir.path().join("target.json");
        let budget = parse_budget(
            br#"{"scopeType":"api-key","scopeId":"key_one","limitAmount":1000,"currentSpend":12.25,"refreshPeriod":"monthly","active":true,"archived":false}"#,
            "api-key",
            "key_one",
        ).expect("actual key budget");
        assert_eq!(read_target(&path, "key_one", Some(&budget)), Ok(None));
        std::fs::write(
            &path,
            r#"{"key_id":"key_one","refresh_period":"monthly","target_usd":200}"#,
        )
        .expect("write config");
        let usage = VercelUsage {
            target: read_target(&path, "key_one", Some(&budget)).expect("matching target"),
            key: Ok(budget),
        };
        assert_eq!(usage.target, Some(200.0));
        assert_eq!(
            usage.key,
            Ok(VercelBudget {
                used: 12.25,
                limit: 1000.0,
                refresh_period: "monthly".into(),
            })
        );
        assert_eq!(
            read_target(&path, "key_other", usage.key.as_ref().ok()),
            Ok(None)
        );
        assert_eq!(read_target(&path, "key_one", None), Ok(None));
    }

    #[test]
    fn local_target_rejects_invalid_config_and_wrong_period() {
        let dir = tempfile::tempdir().expect("temporary config directory");
        let path = dir.path().join("target.json");
        let budget = VercelBudget {
            used: 12.25,
            limit: 1000.0,
            refresh_period: "monthly".into(),
        };
        for config in [
            r#"{"key_id":"key_one","refresh_period":"weekly","target_usd":200}"#,
            r#"{"key_id":"key_one","refresh_period":"monthly","target_usd":0}"#,
            r#"{"key_id":"key_one","refresh_period":"monthly","target_usd":-1}"#,
            r#"{"key_id":"key_one","refresh_period":"monthly","target_usd":"NaN"}"#,
            r#"{"key_id":"key_one","refresh_period":"monthly","target_usd":1e999}"#,
            r#"{"key_id":"key_one","target_usd":200}"#,
            r#"{"refresh_period":"monthly","target_usd":200}"#,
            "not json",
        ] {
            std::fs::write(&path, config).expect("write config");
            assert_eq!(
                read_target(&path, "key_one", Some(&budget)),
                Err(VercelUsageError::InvalidResponse),
                "{config}"
            );
        }
        std::fs::write(&path, r#"{"key_id":"key_other","target_usd":-1}"#)
            .expect("write other key config");
        assert_eq!(read_target(&path, "key_one", Some(&budget)), Ok(None));
        std::fs::write(&path, vec![b' '; MAX_TARGET_CONFIG_BYTES as usize + 1])
            .expect("write oversized config");
        assert_eq!(
            read_target(&path, "key_one", Some(&budget)),
            Err(VercelUsageError::InvalidResponse)
        );
        assert_eq!(
            read_target(dir.path(), "key_one", Some(&budget)),
            Err(VercelUsageError::Unavailable)
        );
    }

    #[test]
    fn credential_rejects_config_injection() {
        assert_eq!(credential(b"test_key-123.abc\n"), Ok("test_key-123.abc"));
        for token in [
            &b""[..],
            &b"a\nb"[..],
            &b"a\"b"[..],
            &b" a"[..],
            &b"x=y"[..],
            &b"\xff"[..],
        ] {
            assert_eq!(credential(token), Err(VercelUsageError::NotConfigured));
        }
    }
}
