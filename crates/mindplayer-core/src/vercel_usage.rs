//! Read-only budgets for OMP's configured Vercel AI Gateway key and attributed user.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(60);
const QUOTAS_URL: &str = "https://ai-gateway.vercel.sh/v1/quotas";
const MAX_PAGES: usize = 100;
// Keep CLI JSON pages below the shared subprocess reader's 64 KiB cap.
const PAGE_SIZE: &str = "20";

/// Where a spending limit is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    ApiKey,
    User,
    Default,
}

/// A scope's observed spending and budget limit, in dollars.
#[derive(Debug, Clone, PartialEq)]
pub struct VercelBudget {
    /// Budget-period spend; a default policy alone does not supply a meter.
    pub used: Option<f64>,
    pub source: BudgetSource,
    pub limit: f64,
    pub refresh_period: String,
}

/// Independent key and attributed-user budgets reported by Vercel.
#[derive(Debug, Clone, PartialEq)]
pub struct VercelUsage {
    pub key: Result<VercelBudget, VercelUsageError>,
    /// `Ok(None)` means the key is explicitly team-attributed, not an unknown user.
    pub user: Result<Option<VercelBudget>, VercelUsageError>,
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
    /// The selected key did not identify its budget attribution.
    UserNotIdentified,
    /// The identified scope has no active budget.
    NoBudget,
}

impl fmt::Display for VercelUsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConfigured => "vercel gateway credential not configured",
            Self::Unavailable => "vercel budget unavailable",
            Self::InvalidResponse => "invalid vercel budget response",
            Self::KeyNotIdentified => "vercel gateway key not uniquely identified",
            Self::UserNotIdentified => "vercel key user attribution unknown",
            Self::NoBudget => "vercel scope has no active budget",
        })
    }
}

impl std::error::Error for VercelUsageError {}

/// Fetch the configured key's budget and its attributed user's budget.
///
/// # Errors
/// Returns a sanitized error if the credential or key identity is unusable.
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
        // The high-level list command projects away spend-attribution metadata.
        let endpoint = paged_endpoint(
            format!("/v1/api-keys?purpose=ai-gateway&limit={PAGE_SIZE}"),
            "until",
            cursor.as_deref(),
        );
        let mut cli = Command::new("vercel");
        cli.current_dir(cwd).args([
            "api",
            &endpoint,
            "--method",
            "GET",
            "--raw",
            "--scope",
            &team,
            "--non-interactive",
        ]);
        cli.env_remove("AI_GATEWAY_API_KEY")
            .env_remove("VERCEL_OIDC_TOKEN");
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
    let selected = selected_key(&matches, cursor.as_deref())?;
    let key_id = &selected.id;
    if key_id.contains(suffix) {
        return Err(VercelUsageError::KeyNotIdentified);
    }
    let key = inspect_budget(cwd, &team, "api-key", key_id, deadline);
    let user = match &selected.user {
        Ok(Some(user_id)) => inspect_budget(cwd, &team, "user", user_id, deadline).map(Some),
        Ok(None) => Ok(None),
        Err(error) => Err(error.clone()),
    };
    Ok(VercelUsage { key, user })
}

fn budget_endpoint(scope_type: &str, cursor: Option<&str>) -> String {
    paged_endpoint(
        format!(
            "/ai-gateway/budgets/list?scopeType={scope_type}&includeDefaultCovered=true&limit={PAGE_SIZE}"
        ),
        "cursor",
        cursor,
    )
}

fn paged_endpoint(mut endpoint: String, parameter: &str, cursor: Option<&str>) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    if let Some(value) = cursor {
        endpoint.reserve(parameter.len() + 2 + value.len() * 3);
        endpoint.push('&');
        endpoint.push_str(parameter);
        endpoint.push('=');
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                endpoint.push(char::from(byte));
            } else {
                endpoint.extend([
                    '%',
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 15)]),
                ]);
            }
        }
    }
    endpoint
}

fn inspect_budget(
    cwd: &Path,
    team: &str,
    scope_type: &str,
    scope_id: &str,
    deadline: Instant,
) -> Result<VercelBudget, VercelUsageError> {
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut budget = None;
    for _ in 0..MAX_PAGES {
        // The high-level CLI omits default-covered meters. Read the complete API
        // through its existing login, keeping credentials out of arguments.
        let endpoint = budget_endpoint(scope_type, cursor.as_deref());
        let mut cli = Command::new("vercel");
        cli.current_dir(cwd).args([
            "api",
            &endpoint,
            "--method",
            "GET",
            "--raw",
            "--scope",
            team,
            "--non-interactive",
        ]);
        cli.env_remove("AI_GATEWAY_API_KEY")
            .env_remove("VERCEL_OIDC_TOKEN");
        let output = run(&mut cli, &[], deadline)?;
        if !output.status.success() {
            return Err(VercelUsageError::Unavailable);
        }
        cursor = parse_budget_page(&output.stdout, scope_type, scope_id, &mut budget)?;
        match &cursor {
            None => {
                return match budget {
                    Some(result) => result,
                    None => inspect_default_budget(cwd, team, scope_type, deadline),
                };
            }
            Some(next) if cursors.insert(next.clone()) => {}
            Some(_) => return Err(VercelUsageError::InvalidResponse),
        }
    }
    Err(VercelUsageError::Unavailable)
}

fn inspect_default_budget(
    cwd: &Path,
    team: &str,
    scope_type: &str,
    deadline: Instant,
) -> Result<VercelBudget, VercelUsageError> {
    let mut cli = Command::new("vercel");
    cli.current_dir(cwd).args([
        "api",
        "/ai-gateway/budgets/defaults/list",
        "--method",
        "GET",
        "--raw",
        "--scope",
        team,
        "--non-interactive",
    ]);
    cli.env_remove("AI_GATEWAY_API_KEY")
        .env_remove("VERCEL_OIDC_TOKEN");
    let output = run(&mut cli, &[], deadline)?;
    if !output.status.success() {
        return Err(VercelUsageError::Unavailable);
    }
    parse_default_budget(&output.stdout, scope_type)
}

fn parse_default_budget(output: &[u8], scope_type: &str) -> Result<VercelBudget, VercelUsageError> {
    let value: serde_json::Value =
        serde_json::from_slice(output).map_err(|_| VercelUsageError::InvalidResponse)?;
    let defaults = value
        .get("defaults")
        .and_then(|v| v.as_array())
        .ok_or(VercelUsageError::InvalidResponse)?;
    let mut selected = None;
    for row in defaults {
        let scope = row
            .get("scopeType")
            .and_then(|v| v.as_str())
            .filter(|scope| matches!(*scope, "team" | "project" | "user" | "api-key"))
            .ok_or(VercelUsageError::InvalidResponse)?;
        if scope == scope_type {
            if selected.is_some() {
                return Err(VercelUsageError::InvalidResponse);
            }
            selected = Some(row);
        }
    }
    let row = selected.ok_or(VercelUsageError::NoBudget)?;
    match row.get("active").and_then(|v| v.as_bool()) {
        Some(true) => {}
        Some(false) => return Err(VercelUsageError::NoBudget),
        None => return Err(VercelUsageError::InvalidResponse),
    }
    let limit = decimal(row.get("limitAmount"))?;
    if limit <= 0.0 {
        return Err(VercelUsageError::InvalidResponse);
    }
    Ok(VercelBudget {
        used: None,
        source: BudgetSource::Default,
        limit,
        refresh_period: budget_period(row)?.to_owned(),
    })
}

fn parse_budget_page(
    output: &[u8],
    scope_type: &str,
    scope_id: &str,
    budget: &mut Option<Result<VercelBudget, VercelUsageError>>,
) -> Result<Option<String>, VercelUsageError> {
    let value: serde_json::Value =
        serde_json::from_slice(output).map_err(|_| VercelUsageError::InvalidResponse)?;
    let budgets = value
        .get("budgets")
        .and_then(|v| v.as_array())
        .filter(|rows| rows.len() <= 100)
        .ok_or(VercelUsageError::InvalidResponse)?;
    for row in budgets {
        if row.get("scopeType").and_then(|v| v.as_str()) != Some(scope_type) {
            return Err(VercelUsageError::InvalidResponse);
        }
        let id = row
            .get("scopeId")
            .and_then(|v| v.as_str())
            .filter(|id| safe_id(id))
            .ok_or(VercelUsageError::InvalidResponse)?;
        if id == scope_id {
            if row.get("archived").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            if budget.is_some() {
                return Err(VercelUsageError::InvalidResponse);
            }
            *budget = Some(parse_budget_value(row, scope_type, scope_id));
        }
    }
    let next = value
        .get("nextCursor")
        .ok_or(VercelUsageError::InvalidResponse)?;
    if next.is_null() {
        return Ok(None);
    }
    next.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .map(|s| Some(s.to_owned()))
        .ok_or(VercelUsageError::InvalidResponse)
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

#[derive(Debug, PartialEq)]
struct SelectedKey {
    id: String,
    user: Result<Option<String>, VercelUsageError>,
}

fn attributed_user(key: &serde_json::Value) -> Result<Option<String>, VercelUsageError> {
    match key
        .get("metadata")
        .and_then(|metadata| metadata.get("spendAttribution"))
        .and_then(|value| value.as_str())
    {
        Some("team") => Ok(None),
        Some("user") => {
            // User-attributed keys count against their creator, never the CLI login.
            let id = key
                .get("createdBy")
                .and_then(|value| value.as_str())
                .map(|id| id.strip_prefix("usr_").unwrap_or(id))
                .filter(|id| safe_id(id))
                .ok_or(VercelUsageError::UserNotIdentified)?;
            Ok(Some(format!("usr_{id}")))
        }
        _ => Err(VercelUsageError::UserNotIdentified),
    }
}

fn parse_keys(
    output: &[u8],
    team: &str,
    suffix: &str,
    matches: &mut Vec<SelectedKey>,
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
            matches.push(SelectedKey {
                id: id.to_owned(),
                user: attributed_user(key),
            });
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
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or(VercelUsageError::InvalidResponse)?;
        Ok(Some(next.to_owned()))
    }
}

fn selected_key<'a>(
    matches: &'a [SelectedKey],
    cursor: Option<&str>,
) -> Result<&'a SelectedKey, VercelUsageError> {
    match (matches, cursor) {
        ([key], None) => Ok(key),
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

#[cfg(test)]
fn parse_budget(
    output: &[u8],
    scope_type: &str,
    scope_id: &str,
) -> Result<VercelBudget, VercelUsageError> {
    let value: serde_json::Value =
        serde_json::from_slice(output).map_err(|_| VercelUsageError::InvalidResponse)?;
    parse_budget_value(&value, scope_type, scope_id)
}

fn parse_budget_value(
    value: &serde_json::Value,
    scope_type: &str,
    scope_id: &str,
) -> Result<VercelBudget, VercelUsageError> {
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
    let period = budget_period(value)?;
    let source = match value.get("source") {
        None if scope_type == "user" => BudgetSource::User,
        None => BudgetSource::ApiKey,
        Some(serde_json::Value::String(source)) if source == "default" => BudgetSource::Default,
        Some(_) => return Err(VercelUsageError::InvalidResponse),
    };
    Ok(VercelBudget {
        used: Some(used),
        source,
        limit,
        refresh_period: period.to_owned(),
    })
}

fn budget_period(value: &serde_json::Value) -> Result<&str, VercelUsageError> {
    value
        .get("refreshPeriod")
        .and_then(|v| v.as_str())
        .filter(|s| matches!(*s, "daily" | "weekly" | "monthly" | "none"))
        .ok_or(VercelUsageError::InvalidResponse)
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
        assert_eq!(selected_key(&matches[..1], None).unwrap().id, "key_one");
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
                used: Some(12.25),
                source: BudgetSource::ApiKey,
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
    fn budget_listing_distinguishes_absence_from_failed_or_incomplete_reads() {
        let mut budget = None;
        assert_eq!(
            parse_budget_page(
                br#"{"budgets":[],"nextCursor":"page2"}"#,
                "api-key",
                "key_new",
                &mut budget
            ),
            Ok(Some("page2".into()))
        );
        assert!(budget.is_none());
        let second = br#"{"budgets":[{"scopeType":"api-key","scopeId":"key_new","source":"default","limitAmount":"200","currentSpend":"12.50","refreshPeriod":"monthly","active":true,"archived":false}],"nextCursor":null}"#;
        assert_eq!(
            parse_budget_page(second, "api-key", "key_new", &mut budget),
            Ok(None)
        );
        assert_eq!(budget.as_ref().unwrap().as_ref().unwrap().used, Some(12.5));
        assert_eq!(
            budget.as_ref().unwrap().as_ref().unwrap().source,
            BudgetSource::Default
        );
        assert_eq!(
            parse_budget_page(second, "api-key", "key_new", &mut budget),
            Err(VercelUsageError::InvalidResponse)
        );
        let mut absent = None;
        assert_eq!(
            parse_budget_page(second, "api-key", "key_other", &mut absent),
            Ok(None)
        );
        assert_eq!(
            absent.unwrap_or(Err(VercelUsageError::NoBudget)),
            Err(VercelUsageError::NoBudget)
        );
        for malformed in [
            br#"{"budgets":[]}"#.as_slice(),
            br#"{"budgets":[],"nextCursor":""}"#,
            br#"{"budgets":[{"scopeType":"team","scopeId":"key_new"}],"nextCursor":null}"#,
            b"Unauthorized",
        ] {
            assert_eq!(
                parse_budget_page(malformed, "api-key", "key_new", &mut None),
                Err(VercelUsageError::InvalidResponse)
            );
        }
    }

    #[test]
    fn archived_overrides_allow_default_lookup_but_inactive_overrides_do_not() {
        let archived = br#"{"budgets":[{"scopeType":"api-key","scopeId":"key_one","archived":true}],"nextCursor":null}"#;
        let mut budget = None;
        assert_eq!(
            parse_budget_page(archived, "api-key", "key_one", &mut budget),
            Ok(None)
        );
        assert!(
            budget.is_none(),
            "a removed override leaves room for a default"
        );
        let inactive = br#"{"budgets":[{"scopeType":"api-key","scopeId":"key_one","archived":false,"active":false}],"nextCursor":null}"#;
        assert_eq!(
            parse_budget_page(inactive, "api-key", "key_one", &mut budget),
            Ok(None)
        );
        assert_eq!(
            budget,
            Some(Err(VercelUsageError::NoBudget)),
            "do not reinterpret a disabled override as absence"
        );
    }

    #[test]
    fn default_budget_keeps_unknown_spending_distinct_from_zero() {
        let output = br#"{"defaults":[{"scopeType":"team","active":true,"limitAmount":99,"refreshPeriod":"daily"},{"scopeType":"api-key","active":true,"limitAmount":"1000","refreshPeriod":"monthly"}]}"#;
        assert_eq!(
            parse_default_budget(output, "api-key"),
            Ok(VercelBudget {
                used: None,
                source: BudgetSource::Default,
                limit: 1000.0,
                refresh_period: "monthly".into(),
            })
        );
        assert_eq!(
            parse_default_budget(br#"{"defaults":[]}"#, "api-key"),
            Err(VercelUsageError::NoBudget)
        );
        assert_eq!(
            parse_default_budget(
                br#"{"defaults":[{"scopeType":"api-key","active":false}]}"#,
                "api-key"
            ),
            Err(VercelUsageError::NoBudget)
        );
        for body in [
            r#"{}"#,
            r#"{"defaults":null}"#,
            r#"{"defaults":[{}]}"#,
            r#"{"defaults":[{"scopeType":"api-key","active":true}]}"#,
            r#"{"defaults":[{"scopeType":"api-key","limitAmount":1,"refreshPeriod":"monthly"}]}"#,
            r#"{"defaults":[{"scopeType":"api-key","active":true,"limitAmount":-1,"refreshPeriod":"monthly"}]}"#,
            r#"{"defaults":[{"scopeType":"api-key","active":true,"limitAmount":"NaN","refreshPeriod":"monthly"}]}"#,
            r#"{"defaults":[{"scopeType":"api-key","active":true,"limitAmount":1000,"refreshPeriod":"yearly"}]}"#,
            r#"{"defaults":[{"scopeType":"api-key","active":false},{"scopeType":"api-key","active":true,"limitAmount":1,"refreshPeriod":"monthly"}]}"#,
            "Unauthorized",
        ] {
            assert_eq!(
                parse_default_budget(body.as_bytes(), "api-key"),
                Err(VercelUsageError::InvalidResponse),
                "{body}"
            );
        }
    }

    #[test]
    fn user_attribution_uses_key_creator_not_cli_identity() {
        for creator in ["other_member", "usr_other_member"] {
            let page = serde_json::json!({
                "apiKeys": [{
                    "id": "key_one", "teamId": "team_a", "partialKey": "abcdef",
                    "createdBy": creator, "metadata": {"spendAttribution": "user"}
                }],
                "pagination": {"next": null},
                "user": {"id": "cli_member"}
            });
            let mut matches = Vec::new();
            parse_keys(
                &serde_json::to_vec(&page).unwrap(),
                "team_a",
                "abcdef",
                &mut matches,
            )
            .unwrap();
            assert_eq!(
                selected_key(&matches, None).unwrap().user,
                Ok(Some("usr_other_member".into()))
            );
        }
    }

    #[test]
    fn team_attribution_and_missing_metadata_never_inherit_creator_budget() {
        assert_eq!(
            attributed_user(&serde_json::json!({
                "createdBy": "cli_member", "metadata": {"spendAttribution": "team"}
            })),
            Ok(None)
        );
        for key in [
            serde_json::json!({"createdBy": "cli_member"}),
            serde_json::json!({"metadata": null}),
            serde_json::json!({"metadata": {}}),
            serde_json::json!({"metadata": {"spendAttribution": "unknown"}}),
            serde_json::json!({"metadata": {"spendAttribution": "user"}}),
            serde_json::json!({"createdBy": "usr_", "metadata": {"spendAttribution": "user"}}),
            serde_json::json!({"createdBy": "\u{1b}[31m", "metadata": {"spendAttribution": "user"}}),
        ] {
            assert_eq!(
                attributed_user(&key),
                Err(VercelUsageError::UserNotIdentified)
            );
        }
    }

    #[test]
    fn user_default_policy_requires_a_meter_for_spend_and_blocked_state() {
        let defaults = br#"{"defaults":[{"scopeType":"api-key","active":true,"limitAmount":1000,"refreshPeriod":"monthly"},{"scopeType":"user","active":true,"limitAmount":50,"refreshPeriod":"weekly"}]}"#;
        let policy = parse_default_budget(defaults, "user").unwrap();
        assert_eq!(policy.used, None);
        assert_eq!(policy.limit, 50.0);
        assert_eq!(policy.refresh_period, "weekly");
        assert_eq!(policy.source, BudgetSource::Default);
        let mut meter = None;
        let page = br#"{"budgets":[{"scopeType":"user","scopeId":"usr_other_member","source":"default","limitAmount":50,"currentSpend":51.007682468,"refreshPeriod":"weekly","active":true,"archived":false}],"nextCursor":null}"#;
        assert_eq!(
            parse_budget_page(page, "user", "usr_cli_member", &mut meter),
            Ok(None)
        );
        assert_eq!(meter, None);
        assert_eq!(
            parse_budget_page(page, "user", "usr_other_member", &mut meter),
            Ok(None)
        );
        let observed = meter.unwrap().unwrap();
        assert_eq!(observed.used, Some(51.007682468));
        assert_eq!(observed.source, BudgetSource::Default);
        assert!(observed.used.unwrap() >= observed.limit);
        for malformed in [
            br#"{"budgets":[]}"#.as_slice(),
            br#"{"budgets":[],"nextCursor":""}"#,
            b"Unauthorized",
        ] {
            assert_eq!(
                parse_budget_page(malformed, "user", "usr_other_member", &mut None),
                Err(VercelUsageError::InvalidResponse)
            );
        }
    }

    #[test]
    fn opaque_key_and_budget_cursors_cannot_inject_query_parameters() {
        let cursor = "page2=+/&scopeType=team";
        assert_eq!(
            budget_endpoint("user", Some(cursor)),
            "/ai-gateway/budgets/list?scopeType=user&includeDefaultCovered=true&limit=20&cursor=page2%3D%2B%2F%26scopeType%3Dteam"
        );
        assert_eq!(
            paged_endpoint(
                "/v1/api-keys?purpose=ai-gateway".into(),
                "until",
                Some(cursor)
            ),
            "/v1/api-keys?purpose=ai-gateway&until=page2%3D%2B%2F%26scopeType%3Dteam"
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
