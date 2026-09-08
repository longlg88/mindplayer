//! Subscription rate-limit windows — "how much of my 5h / weekly allowance is
//! gone", the numbers Claude Code's `/usage` and Codex's TUI show.
//!
//! A different quantity from [`crate::tokens`]: that counts tokens this session
//! spent, this reports utilization of the plan's windows.
//!
//! Two sources, deliberately different in cost:
//!
//! * **Codex** — no auth, no network. Every turn writes a `rate_limits` snapshot
//!   into `~/.codex/sessions/**/rollout-*.jsonl`; the newest one is read from the
//!   file's TAIL (rollouts here reach 600 MB, so the whole file is never read).
//! * **Claude** — a live `GET /api/oauth/usage` with the subscription OAuth
//!   token, shelled out through `curl` so no HTTP/TLS dependency enters the tree
//!   and a corporate MITM CA is trusted exactly as the user's other tools trust it.
//!
//! Every failure carries its reason so the UI can say WHY a number is absent.
//! A missing window is never rendered as `0%` — that would read as "plenty left".

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// Bytes read from the end of a rollout when looking for its last snapshot.
/// `rate_limits` is written every turn, so the newest is always near the end;
/// not finding one within this window means giving up rather than reading a
/// multi-hundred-megabyte file to be thorough.
const ROLLOUT_TAIL_BYTES: u64 = 1 << 20;

/// Claude's OAuth usage endpoint and the beta header it requires.
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_BETA_HEADER: &str = "anthropic-beta: oauth-2025-04-20";

/// Seconds before the `curl` call is abandoned. The readout is decoration; it
/// must never hold anything up.
const CURL_TIMEOUT_SECS: u32 = 8;

/// Claude's two subscription windows, as percentages already (not fractions).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClaudeLimits {
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
    /// UNIX epoch seconds at which each window resets, when reported.
    pub five_hour_reset: Option<i64>,
    pub seven_day_reset: Option<i64>,
}

impl ClaudeLimits {
    pub fn has_any(&self) -> bool {
        self.five_hour.is_some() || self.seven_day.is_some()
    }
}

/// Codex's snapshot. Which fields are populated depends on the plan: percentage
/// windows on consumer plans, a credit balance on business plans, and neither
/// when the account has no metered windows at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexLimits {
    pub primary: Option<f64>,
    pub secondary: Option<f64>,
    pub primary_reset: Option<i64>,
    pub secondary_reset: Option<i64>,
    /// Each slot's window length. This — not slot order — is what says whether a
    /// slot is the weekly window (`>= 1440` minutes).
    pub primary_window_minutes: Option<f64>,
    pub secondary_window_minutes: Option<f64>,
    pub credit_balance: Option<f64>,
    pub credits_unlimited: bool,
    pub plan_type: Option<String>,
    /// Which pool the balance belongs to. Snapshots carry more than one — a
    /// `codex` pool and a `premium` pool with separate balances — so a bare
    /// number says nothing about what ran out.
    pub limit_id: Option<String>,
    /// Set when the account has actually hit a limit. This is the one field
    /// here that calls for action, so it outranks the balance on screen.
    pub rate_limit_reached: Option<String>,
}

impl CodexLimits {
    /// Is there anything worth putting on screen?
    pub fn has_any(&self) -> bool {
        self.primary.is_some()
            || self.secondary.is_some()
            || self.credit_balance.is_some()
            || self.credits_unlimited
            || self.rate_limit_reached.is_some()
    }
}

/// Turn a `rate_limit_reached_type` into something a person reads.
///
/// Unknown values are passed through with their underscores opened up rather
/// than dropped: a limit we cannot name is still a limit that was hit.
fn reached_label(raw: &str) -> String {
    match raw {
        "workspace_member_usage_limit_reached" => "workspace limit reached".to_string(),
        "usage_limit_reached" => "usage limit reached".to_string(),
        other => other.replace('_', " "),
    }
}

/// Both providers, each either a reading or the reason there isn't one.
#[derive(Debug, Clone)]
pub struct Limits {
    pub claude: Result<ClaudeLimits, String>,
    pub codex: Result<CodexLimits, String>,
}

impl Limits {
    /// True when at least one provider produced a displayable number.
    pub fn has_any(&self) -> bool {
        self.claude.as_ref().is_ok_and(ClaudeLimits::has_any)
            || self.codex.as_ref().is_ok_and(CodexLimits::has_any)
    }

    /// One row per provider: the line to show, and whether THAT provider
    /// produced a number.
    ///
    /// The flag is per-row on purpose. The UI dims a row that carries a reason
    /// instead of a value; deriving that from [`Self::has_any`] made one
    /// provider's reading un-dim the other provider's error text, because
    /// `has_any` is true when either side succeeded.
    pub fn summary_rows(&self) -> Vec<(String, bool)> {
        self.summary_lines()
            .into_iter()
            .zip([
                self.claude.as_ref().is_ok_and(ClaudeLimits::has_any),
                self.codex.as_ref().is_ok_and(CodexLimits::has_any),
            ])
            .collect()
    }

    /// One short line per provider for the UI, value or reason.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.claude {
            Ok(c) if c.has_any() => {
                let mut parts = Vec::new();
                if let Some(p) = c.five_hour {
                    parts.push(format!("5h {p:.0}%"));
                }
                if let Some(p) = c.seven_day {
                    parts.push(format!("week {p:.0}%"));
                }
                out.push(format!("claude  {}", parts.join("  ")));
            }
            Ok(_) => out.push("claude  no windows reported".into()),
            Err(e) => out.push(format!("claude  — {e}")),
        }
        match &self.codex {
            Ok(c) if c.has_any() => {
                let mut parts = Vec::new();
                for (used, window) in [
                    (c.primary, c.primary_window_minutes),
                    (c.secondary, c.secondary_window_minutes),
                ] {
                    if let Some(p) = used {
                        parts.push(format!("{} {p:.0}%", window_label(window)));
                    }
                }
                // A limit that was actually hit leads: it is the only thing here
                // the user can act on, and a balance beside it is background.
                if let Some(reached) = c.rate_limit_reached.as_deref() {
                    parts.push(reached_label(reached));
                }
                // The pool is named unless it is the plain `codex` one, which
                // the line already starts with — snapshots also carry a
                // `premium` pool whose balance is a different number entirely.
                let pool = match c.limit_id.as_deref() {
                    Some("codex") | None => String::new(),
                    Some(other) => format!("{other} "),
                };
                if c.credits_unlimited {
                    parts.push(format!("{pool}credits unlimited"));
                } else if let Some(b) = c.credit_balance {
                    parts.push(format!("{pool}credits {b:.0}"));
                }
                out.push(format!("codex   {}", parts.join("  ")));
            }
            Ok(c) => out.push(format!(
                "codex   no windows on {} plan",
                c.plan_type.as_deref().unwrap_or("this")
            )),
            Err(e) => out.push(format!("codex   — {e}")),
        }
        out
    }
}

/// Name a codex window by its length rather than by which slot it arrived in.
/// A day or longer reads as weekly; shorter windows are shown in hours.
fn window_label(window_minutes: Option<f64>) -> String {
    const DAY: f64 = 1440.0;
    const HOUR: f64 = 60.0;
    match window_minutes {
        Some(m) if m >= DAY => "weekly".to_string(),
        Some(m) if m >= HOUR => format!("{:.0}h", m / HOUR),
        // Sub-hour windows stay in minutes; dividing them by 60 and rounding
        // rendered everything under 45 minutes as a meaningless "0h".
        Some(m) if m > 0.0 => format!("{m:.0}m"),
        _ => "window".to_string(),
    }
}

/// Read both providers. Does disk and network I/O — call it off the UI thread.
pub fn fetch(home: &Path) -> Limits {
    Limits {
        claude: claude_limits(home),
        codex: codex_limits(home),
    }
}

// ── Codex ──────────────────────────────────────────────────────────────────

/// Newest rollout's last `rate_limits` snapshot.
pub fn codex_limits(home: &Path) -> Result<CodexLimits, String> {
    let root = home.join(".codex").join("sessions");
    let newest = newest_rollout(&root).ok_or_else(|| "no codex rollouts found".to_string())?;
    let tail = read_tail(&newest, ROLLOUT_TAIL_BYTES)
        .map_err(|e| format!("cannot read newest rollout: {e}"))?;
    for line in tail.lines().rev() {
        if !line.contains("\"rate_limits\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(rl) = find_key(&v, "rate_limits") {
            return Ok(parse_codex_rate_limits(rl));
        }
    }
    Err("no rate_limits snapshot in the newest rollout's tail".into())
}

/// Last `n` bytes of a file as lossy UTF-8, with the first (possibly partial)
/// line dropped so a mid-line cut never yields a half JSON object.
fn read_tail(path: &Path, n: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let from = len.saturating_sub(n);
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.take(n).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if from == 0 {
        return Ok(text);
    }
    Ok(match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => String::new(),
    })
}

/// Most recently modified `rollout-*.jsonl` under `root`, searched recursively.
/// Symlinks are not followed, so a self-referential directory cannot loop.
fn newest_rollout(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    walk_newest(root, &mut best, 0);
    best.map(|(_, p)| p)
}

fn walk_newest(dir: &Path, best: &mut Option<(SystemTime, PathBuf)>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk_newest(&p, best, depth + 1);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let is_rollout = p.extension().and_then(|s| s.to_str()) == Some("jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"));
        if !is_rollout {
            continue;
        }
        if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
            if best.as_ref().is_none_or(|(bt, _)| mt > *bt) {
                *best = Some((mt, p));
            }
        }
    }
}

/// Depth-first search for the first value under `key`, at any nesting.
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if let Value::Object(map) = v {
        if let Some(found) = map.get(key) {
            return Some(found);
        }
        for child in map.values() {
            if let Some(found) = find_key(child, key) {
                return Some(found);
            }
        }
    }
    None
}

/// Pure so it can be tested against captured payloads of every plan shape.
pub fn parse_codex_rate_limits(rl: &Value) -> CodexLimits {
    let pct = |k: &str| {
        rl.get(k)
            .and_then(|w| w.get("used_percent").or_else(|| w.get("utilization")))
            .and_then(Value::as_f64)
    };
    // Only `resets_at` — an absolute epoch. An earlier version also accepted
    // `resets_in_seconds`, which is a DURATION and would have been rendered as a
    // 1970 timestamp; no real rollout carries that key (0 of 30 sampled).
    let reset = |k: &str| {
        rl.get(k)
            .and_then(|w| w.get("resets_at"))
            .and_then(parse_epoch)
    };
    // Which slot is the weekly window is reported by `window_minutes`, not by
    // slot order. Labelling `secondary` "weekly" was an assumption about an
    // ordering the payload never promises.
    let window_minutes = |k: &str| {
        rl.get(k)
            .and_then(|w| w.get("window_minutes"))
            .and_then(Value::as_f64)
    };
    let credits = rl.get("credits");
    CodexLimits {
        primary: pct("primary"),
        secondary: pct("secondary"),
        primary_reset: reset("primary"),
        secondary_reset: reset("secondary"),
        primary_window_minutes: window_minutes("primary"),
        secondary_window_minutes: window_minutes("secondary"),
        credit_balance: credits
            .and_then(|c| c.get("balance"))
            .and_then(parse_number),
        credits_unlimited: credits
            .and_then(|c| c.get("unlimited"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        limit_id: rl
            .get("limit_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        rate_limit_reached: rl
            .get("rate_limit_reached_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        plan_type: rl
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Codex writes `balance` as a JSON string; accept either that or a number.
fn parse_number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

// ── Claude ─────────────────────────────────────────────────────────────────

/// Live utilization for the subscription windows.
pub fn claude_limits(home: &Path) -> Result<ClaudeLimits, String> {
    let token = claude_token(home)?;
    let body = curl_json(CLAUDE_USAGE_URL, &token)?;
    let v: Value = serde_json::from_str(&body)
        .map_err(|e| format!("unparsable response from /api/oauth/usage: {e}"))?;
    let parsed = parse_claude_usage(&v);
    if parsed.has_any() {
        Ok(parsed)
    } else {
        Err("response carried no window utilization".into())
    }
}

/// Subscription OAuth access token: the credentials file first, then the macOS
/// login Keychain for a keychain-only login.
///
/// Both are searched for `claudeAiOauth.accessToken` specifically. The same
/// Keychain item also holds unrelated `mcpOAuth` entries for MCP servers; those
/// are not subscription tokens and must not be sent to `/api/oauth/usage`.
fn claude_token(home: &Path) -> Result<String, String> {
    let file = home.join(".claude").join(".credentials.json");
    if let Ok(raw) = std::fs::read_to_string(&file) {
        if let Some(t) = token_from_credentials(&raw) {
            return Ok(t);
        }
    }
    // The login Keychain is machine-global, so it is the right fallback only
    // when `home` really is the user's home. A caller pointing at a fixture home
    // — tests do — must never reach into the developer's Keychain, which could
    // also raise an interactive approval dialog in the middle of a test run.
    #[cfg(target_os = "macos")]
    if is_real_home(home) {
        let out = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
            .map_err(|e| format!("cannot run security(1): {e}"))?;
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout);
            if let Some(t) = token_from_credentials(&raw) {
                return Ok(t);
            }
            return Err("keychain item has no claudeAiOauth token".into());
        }
    }
    Err("no subscription OAuth token found".into())
}

/// Is `home` the real `$HOME`? Gates the machine-global Keychain lookup.
#[cfg(target_os = "macos")]
fn is_real_home(home: &Path) -> bool {
    std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == home)
}

/// Pull `claudeAiOauth.accessToken` out of a credentials blob, rejecting it when
/// `expiresAt` is already past — refreshing is Claude Code's job, not ours.
pub fn token_from_credentials(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    if let Some(exp) = oauth.get("expiresAt").and_then(Value::as_i64) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if exp <= now_ms {
            return None;
        }
    }
    let token = oauth.get("accessToken").and_then(Value::as_str)?;
    // A bearer token is opaque ASCII. A control character in one means the blob
    // is corrupt or crafted; refuse it rather than sanitize and send it anyway,
    // because a newline would otherwise become a new curl config directive.
    if token.trim().is_empty() || token.chars().any(char::is_control) {
        return None;
    }
    Some(token.to_string())
}

/// The fixed leading arguments, in order; the config path follows them.
///
/// `-q` must come FIRST: `--config` ADDS a file, it does not replace the default
/// one, so without `-q` curl also reads `~/.curlrc` and merges it. An rc carrying
/// `insecure`, `proxy`, `location` or `output` would undo the hardening in
/// [`curl_config`] — and `insecure` in particular hands the bearer token to
/// anyone on the path. curl(1) only honours `-q` as the first argument.
///
/// Split out so a test can assert on the real argv rather than on a string we
/// happen to build.
fn curl_args() -> Vec<String> {
    vec!["-q".to_string(), "--config".to_string()]
}

/// GET `url` with the bearer token, via `curl`.
///
/// The token is passed in a 0600 config file, never on the command line, so it
/// cannot be read out of `ps` by any other process on the machine. The file is
/// removed as soon as curl returns.
fn curl_json(url: &str, token: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!(
        "mindplayer-usage-{}-{}.curlrc",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    write_private(&path, &curl_config(url, token))
        .map_err(|e| format!("cannot stage curl config: {e}"))?;
    let out = std::process::Command::new("curl")
        .args(curl_args())
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);
    let out = out.map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A curl config.
///
/// Values are double-quoted, so `"`/`\` are escaped rather than allowed to end
/// the quoted string early. Control characters are DROPPED, not escaped: curl
/// parses this file line by line, so a raw newline inside a value would start a
/// new directive — a token carrying one could otherwise redirect the request to
/// an attacker's `url =`. [`token_from_credentials`] already refuses such a
/// token; this is the second line of defence for any other caller.
fn curl_config(url: &str, token: &str) -> String {
    let esc = |s: &str| {
        s.chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    };
    format!(
        // No `location`: a fixed API endpoint has no reason to redirect, and
        // following one while an Authorization header is set risks a same-host
        // https→http downgrade putting the bearer token on the wire in clear.
        // `proto = "=https"` refuses a plaintext request outright.
        "silent\nshow-error\nfail\nproto = \"=https\"\nmax-time = {CURL_TIMEOUT_SECS}\nurl = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"{CLAUDE_BETA_HEADER}\"\n",
        esc(url),
        esc(token)
    )
}

/// Create a file readable only by this user, before writing secrets into it.
///
/// [`crate::private::create_new_private`] refuses an occupied path rather than
/// opening it: `create` would follow a symlink and adopt a pre-created file,
/// whose own mode would then survive and expose the token.
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = crate::private::create_new_private(path)?;
    f.write_all(body.as_bytes())
}

/// Pure so it can be tested against a captured `/api/oauth/usage` body.
pub fn parse_claude_usage(body: &Value) -> ClaudeLimits {
    let pct = |k: &str| {
        body.get(k)
            .and_then(|w| w.get("utilization"))
            .and_then(Value::as_f64)
    };
    let reset = |k: &str| {
        body.get(k)
            .and_then(|w| w.get("resets_at"))
            .and_then(parse_epoch)
    };
    ClaudeLimits {
        five_hour: pct("five_hour"),
        seven_day: pct("seven_day"),
        five_hour_reset: reset("five_hour"),
        seven_day_reset: reset("seven_day"),
    }
}

/// A reset time as UNIX epoch seconds. Accepts an integer/float epoch or an
/// RFC3339 string, since the two backends differ. Anything else → `None`, which
/// only hides the countdown.
fn parse_epoch(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    let s = v.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_windows_are_read_as_percentages_with_resets() {
        let body = json!({
            "five_hour": { "utilization": 42.5, "resets_at": 1785766495 },
            "seven_day": { "utilization": 7.0, "resets_at": "2026-08-10T00:00:00Z" }
        });
        let got = parse_claude_usage(&body);
        assert_eq!(got.five_hour, Some(42.5));
        assert_eq!(got.seven_day, Some(7.0));
        assert_eq!(got.five_hour_reset, Some(1785766495));
        assert!(got.seven_day_reset.is_some(), "rfc3339 reset parsed");
        assert!(got.has_any());
    }

    #[test]
    fn a_response_without_windows_has_nothing_rather_than_zero() {
        let got = parse_claude_usage(&json!({ "something_else": 1 }));
        assert_eq!(got.five_hour, None, "absent must not become 0.0");
        assert_eq!(got.seven_day, None);
        assert!(!got.has_any());
    }

    #[test]
    fn a_placeholder_reset_value_is_ignored() {
        let body = json!({ "five_hour": { "utilization": 5.0, "resets_at": "..." } });
        let got = parse_claude_usage(&body);
        assert_eq!(got.five_hour, Some(5.0));
        assert_eq!(got.five_hour_reset, None);
    }

    /// The exact shape observed on this machine: business plan, no windows.
    #[test]
    fn a_business_plan_snapshot_reports_no_windows() {
        let rl = json!({
            "limit_id": "codex", "limit_name": null,
            "primary": null, "secondary": null,
            "credits": { "has_credits": true, "unlimited": false, "balance": "13587.356966495514" },
            "individual_limit": null, "plan_type": "business",
            "rate_limit_reached_type": null
        });
        let got = parse_codex_rate_limits(&rl);
        assert_eq!(got.primary, None);
        assert_eq!(got.secondary, None);
        assert_eq!(got.plan_type.as_deref(), Some("business"));
        assert_eq!(
            got.credit_balance,
            Some(13587.356966495514),
            "balance arrives as a JSON string and must still parse"
        );
        assert!(got.has_any(), "a credit balance is worth showing");
    }

    /// NOTE: this payload is hand-written from the field names copad reads
    /// (`used_percent` / `window_minutes` / `resets_at`), NOT captured from a
    /// real consumer-plan account — none is available on this machine, which is
    /// business-plan and reports `primary`/`secondary` as null. Treat a pass
    /// here as "the parser matches our understanding", not as verified.
    #[test]
    fn a_consumer_plan_snapshot_reports_percentage_windows() {
        let rl = json!({
            "primary": { "used_percent": 63.2, "window_minutes": 300, "resets_at": 1785800000 },
            "secondary": { "used_percent": 12.0, "window_minutes": 10080 },
            "plan_type": "plus"
        });
        let got = parse_codex_rate_limits(&rl);
        assert_eq!(got.primary, Some(63.2));
        assert_eq!(got.secondary, Some(12.0));
        assert_eq!(got.primary_reset, Some(1785800000));
        assert_eq!(got.secondary_reset, None);
        assert_eq!(got.primary_window_minutes, Some(300.0));
        assert_eq!(got.secondary_window_minutes, Some(10080.0));
    }

    /// The label used to be hard-coded to slot order ("secondary" = weekly).
    /// The payload never promises that ordering; `window_minutes` states it.
    #[test]
    fn window_labels_come_from_the_window_length_not_the_slot_order() {
        // Weekly in the FIRST slot — the opposite of the old assumption.
        let rl = json!({
            "primary": { "used_percent": 40.0, "window_minutes": 10080 },
            "secondary": { "used_percent": 5.0, "window_minutes": 300 },
        });
        let line = &Limits {
            claude: Ok(ClaudeLimits::default()),
            codex: Ok(parse_codex_rate_limits(&rl)),
        }
        .summary_lines()[1];
        assert!(line.contains("weekly 40%"), "{line}");
        assert!(line.contains("5h 5%"), "{line}");
    }

    /// Dividing minutes by 60 and rounding rendered every window under 45
    /// minutes as "0h", which reads as a broken UI rather than a short window.
    #[test]
    fn sub_hour_windows_are_labelled_in_minutes() {
        assert_eq!(window_label(Some(1.0)), "1m");
        assert_eq!(window_label(Some(30.0)), "30m");
        assert_eq!(window_label(Some(59.0)), "59m");
        assert_eq!(window_label(Some(60.0)), "1h");
        assert_eq!(window_label(Some(300.0)), "5h");
        assert_eq!(window_label(Some(1439.0)), "24h");
        assert_eq!(window_label(Some(1440.0)), "weekly");
        assert_eq!(window_label(Some(10080.0)), "weekly");
        assert_eq!(window_label(None), "window");
        assert_eq!(window_label(Some(0.0)), "window", "zero is not a window");
    }

    /// The UI dims a row that carries a reason rather than a value. Deriving
    /// that from `has_any()` — true when EITHER provider succeeded — made one
    /// provider's number un-dim the other's error text.
    #[test]
    fn each_row_reports_its_own_provider_availability() {
        // claude has a number, codex errors.
        let mixed = Limits {
            claude: Ok(ClaudeLimits {
                five_hour: Some(42.0),
                ..ClaudeLimits::default()
            }),
            codex: Err("no codex rollouts found".into()),
        };
        assert!(mixed.has_any(), "aggregate is true — the old trap");
        let rows = mixed.summary_rows();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0.contains("5h 42%"), "{:?}", rows[0]);
        assert!(rows[0].1, "claude produced a number");
        assert!(rows[1].0.contains("no codex rollouts"), "{:?}", rows[1]);
        assert!(
            !rows[1].1,
            "codex produced none, so its row must not claim otherwise"
        );

        // And the mirror image, so the pairing is not accidentally positional.
        let flipped = Limits {
            claude: Err("no subscription OAuth token found".into()),
            codex: Ok(parse_codex_rate_limits(
                &json!({ "credits": { "unlimited": true } }),
            )),
        };
        let rows = flipped.summary_rows();
        assert!(!rows[0].1, "claude errored");
        assert!(rows[1].1, "codex produced a value");
    }

    #[test]
    fn a_window_without_a_stated_length_is_not_mislabelled() {
        let rl = json!({ "primary": { "used_percent": 7.0 } });
        let line = &Limits {
            claude: Ok(ClaudeLimits::default()),
            codex: Ok(parse_codex_rate_limits(&rl)),
        }
        .summary_lines()[1];
        assert!(line.contains("window 7%"), "{line}");
        assert!(!line.contains("weekly"), "must not guess weekly: {line}");
    }

    /// `resets_in_seconds` is a DURATION. Accepting it as an epoch produced a
    /// 1970 timestamp; no real rollout carries the key (0 of 30 sampled).
    #[test]
    fn a_duration_style_reset_key_is_not_read_as_an_epoch() {
        let rl = json!({ "primary": { "used_percent": 1.0, "resets_in_seconds": 3600 } });
        let got = parse_codex_rate_limits(&rl);
        assert_eq!(got.primary, Some(1.0));
        assert_eq!(
            got.primary_reset, None,
            "a duration must not become an absolute time"
        );
    }

    /// `--config` ADDS a file; it does not replace `~/.curlrc`. Only `-q` as the
    /// first argument suppresses that, so an rc with `insecure` could otherwise
    /// undo the hardening the config file sets up.
    #[test]
    fn the_default_curlrc_is_disabled_before_our_config_is_added() {
        let args = curl_args();
        assert_eq!(
            args.first().map(String::as_str),
            Some("-q"),
            "curl only honours -q as the FIRST argument: {args:?}"
        );
        let cfg_at = args.iter().position(|a| a == "--config");
        assert!(cfg_at.is_some_and(|i| i > 0), "{args:?}");
    }

    #[test]
    fn the_config_refuses_a_plaintext_request() {
        let cfg = curl_config(CLAUDE_USAGE_URL, "tok");
        assert!(cfg.contains("proto = \"=https\""), "{cfg}");
        assert!(
            !cfg.lines().any(|l| l.trim() == "location"),
            "redirects must not be followed while an Authorization header is set:\n{cfg}"
        );
    }

    /// `create` would have opened a pre-created path (or followed a symlink) and
    /// kept its mode, writing the bearer token into a file someone else can read.
    #[test]
    fn staging_the_token_refuses_an_occupied_path() {
        let dir = std::env::temp_dir().join(format!("mp-limits-occupied-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("curlrc");
        std::fs::write(&path, "pre-existing").unwrap();

        let err = write_private(&path, "secret").expect_err("must not overwrite");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "pre-existing",
            "and the occupant is left untouched"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_fixture_home_never_reaches_the_machine_keychain() {
        // A home with no credentials file must fail with the "not found" reason
        // rather than consulting the login Keychain (which `is_real_home` gates).
        let empty = std::env::temp_dir().join(format!("mp-limits-home-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let err = claude_token(&empty).unwrap_err();
        assert_eq!(err, "no subscription OAuth token found", "{err}");
    }

    #[test]
    fn an_empty_snapshot_has_nothing_to_show() {
        let got = parse_codex_rate_limits(&json!({ "plan_type": "business" }));
        assert!(!got.has_any());
        // The UI must be able to explain WHY, not just show a blank.
        let limits = Limits {
            claude: Err("no subscription OAuth token found".into()),
            codex: Ok(got),
        };
        assert!(!limits.has_any());
        let lines = limits.summary_lines();
        assert!(
            lines[0].contains("no subscription OAuth token"),
            "{lines:?}"
        );
        assert!(lines[1].contains("business"), "{lines:?}");
    }

    #[test]
    fn a_limit_that_was_actually_hit_leads_the_line() {
        // Real snapshot shape: the premium pool, drained, with the workspace
        // limit reached. The old line said only "credits 0", which reads as a
        // balance nobody set rather than a limit that stopped the account.
        let rl = json!({
            "limit_id": "premium",
            "primary": null,
            "secondary": null,
            "credits": { "has_credits": true, "unlimited": false, "balance": "0" },
            "plan_type": "business",
            "rate_limit_reached_type": "workspace_member_usage_limit_reached"
        });
        let got = parse_codex_rate_limits(&rl);
        assert_eq!(
            got.rate_limit_reached.as_deref(),
            Some("workspace_member_usage_limit_reached")
        );
        assert_eq!(got.limit_id.as_deref(), Some("premium"));

        let limits = Limits {
            claude: Err("not under test".into()),
            codex: Ok(got),
        };
        let line = &limits.summary_lines()[1];
        assert!(line.contains("workspace limit reached"), "{line}");
        assert!(line.contains("premium credits 0"), "{line}");
        assert!(
            line.find("workspace limit reached") < line.find("premium credits"),
            "the actionable half comes first: {line}"
        );
    }

    #[test]
    fn the_plain_codex_pool_is_not_labelled_twice() {
        // limit_id "codex" on a line that already starts with "codex".
        let rl = json!({
            "limit_id": "codex",
            "credits": { "has_credits": true, "unlimited": false, "balance": "25571.1" },
            "plan_type": "business"
        });
        let limits = Limits {
            claude: Err("not under test".into()),
            codex: Ok(parse_codex_rate_limits(&rl)),
        };
        let line = &limits.summary_lines()[1];
        assert!(line.contains("credits 25571"), "{line}");
        assert!(!line.contains("codex codex"), "{line}");
    }

    #[test]
    fn an_unnamed_limit_type_is_still_reported() {
        let rl = json!({
            "limit_id": "codex",
            "credits": { "has_credits": true, "unlimited": false, "balance": "0" },
            "rate_limit_reached_type": "some_future_limit_reached"
        });
        let limits = Limits {
            claude: Err("not under test".into()),
            codex: Ok(parse_codex_rate_limits(&rl)),
        };
        let line = &limits.summary_lines()[1];
        assert!(line.contains("some future limit reached"), "{line}");
    }

    #[test]
    fn unlimited_credits_are_shown_without_a_balance() {
        let rl = json!({ "credits": { "unlimited": true }, "plan_type": "enterprise" });
        let got = parse_codex_rate_limits(&rl);
        assert!(got.credits_unlimited);
        assert!(got.has_any());
        let lines = Limits {
            claude: Ok(ClaudeLimits::default()),
            codex: Ok(got),
        }
        .summary_lines();
        assert!(lines[1].contains("unlimited"), "{lines:?}");
    }

    #[test]
    fn mcp_oauth_entries_are_not_mistaken_for_a_subscription_token() {
        // The real Keychain item on this machine: mcpOAuth only, no claudeAiOauth.
        let raw =
            r#"{"mcpOAuth":{"reports|abc":{"serverName":"reports","accessToken":"mcp-token"}}}"#;
        assert_eq!(
            token_from_credentials(raw),
            None,
            "an MCP server token must never be sent to /api/oauth/usage"
        );
    }

    #[test]
    fn an_expired_subscription_token_is_refused() {
        let past = chrono::Utc::now().timestamp_millis() - 60_000;
        let raw = format!(r#"{{"claudeAiOauth":{{"accessToken":"t","expiresAt":{past}}}}}"#);
        assert_eq!(token_from_credentials(&raw), None);
    }

    #[test]
    fn a_live_subscription_token_is_accepted() {
        let future = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let raw =
            format!(r#"{{"claudeAiOauth":{{"accessToken":"tok-123","expiresAt":{future}}}}}"#);
        assert_eq!(token_from_credentials(&raw).as_deref(), Some("tok-123"));
    }

    #[test]
    fn the_token_never_reaches_the_command_line() {
        let cfg = curl_config(CLAUDE_USAGE_URL, "secret-token");
        assert!(
            cfg.contains("Bearer secret-token"),
            "token is in the config"
        );
        assert!(cfg.contains("max-time = 8"), "call is bounded");
        assert!(cfg.contains("silent"), "no progress meter on stdout");
    }

    #[test]
    fn a_hostile_token_cannot_inject_a_second_curl_directive() {
        let cfg = curl_config(CLAUDE_USAGE_URL, "a\"\nurl = \"http://evil");
        // curl parses line by line, so the invariant is per-line: exactly one
        // line may be a `url` directive, and it must be the endpoint we chose.
        let url_lines: Vec<&str> = cfg
            .lines()
            .filter(|l| l.trim_start().starts_with("url ="))
            .collect();
        assert_eq!(url_lines.len(), 1, "exactly one url directive:\n{cfg}");
        assert!(
            url_lines[0].contains(CLAUDE_USAGE_URL),
            "the surviving url is ours: {:?}",
            url_lines[0]
        );
        assert!(cfg.contains("\\\""), "the quote was escaped");
        // The hostile text is confined to the header value, on one line.
        let header_lines: Vec<&str> = cfg.lines().filter(|l| l.contains("http://evil")).collect();
        assert_eq!(header_lines.len(), 1, "injected text stayed on one line");
        assert!(
            header_lines[0].starts_with("header ="),
            "and it stayed inside the header directive: {:?}",
            header_lines[0]
        );
    }

    #[test]
    fn a_token_carrying_a_newline_is_refused_outright() {
        let future = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let raw = format!(
            "{{\"claudeAiOauth\":{{\"accessToken\":\"tok\\nurl = x\",\"expiresAt\":{future}}}}}"
        );
        assert_eq!(
            token_from_credentials(&raw),
            None,
            "a control character means the blob is corrupt or crafted"
        );
    }

    #[test]
    fn a_rollout_tail_is_read_without_touching_the_head() {
        let dir = std::env::temp_dir().join(format!("mp-limits-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rollout-x.jsonl");
        let mut body = String::new();
        // A head line that would win if the whole file were scanned in order.
        body.push_str(&format!(
            "{}\n",
            json!({"payload":{"rate_limits":{"plan_type":"STALE"}}})
        ));
        body.push_str(&"x".repeat(2 * ROLLOUT_TAIL_BYTES as usize));
        body.push('\n');
        body.push_str(&format!(
            "{}\n",
            json!({"payload":{"rate_limits":{"plan_type":"FRESH","primary":{"used_percent":9.0}}}})
        ));
        std::fs::write(&f, body).unwrap();

        let tail = read_tail(&f, ROLLOUT_TAIL_BYTES).unwrap();
        assert!(
            !tail.contains("STALE"),
            "head must be outside the tail window"
        );
        assert!(tail.contains("FRESH"));
    }

    #[test]
    fn a_partial_first_line_is_dropped_from_the_tail() {
        let dir = std::env::temp_dir().join(format!("mp-limits-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rollout-y.jsonl");
        std::fs::write(&f, "{\"broken\": tru\n{\"ok\":1}\n").unwrap();
        // Ask for fewer bytes than the file, forcing a mid-line start.
        let tail = read_tail(&f, 12).unwrap();
        assert!(!tail.contains("broken"), "partial line dropped: {tail:?}");
    }

    #[test]
    fn missing_codex_rollouts_report_the_reason() {
        let empty = std::env::temp_dir().join(format!("mp-limits-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let err = codex_limits(&empty).unwrap_err();
        assert!(err.contains("no codex rollouts"), "{err}");
    }
}
