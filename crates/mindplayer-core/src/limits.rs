//! Subscription rate-limit windows — "how much of my 5h / weekly allowance is
//! gone", the numbers Claude Code's `/usage` and Codex's TUI show.
//!
//! A different quantity from [`crate::tokens`]: that counts tokens this session
//! spent, this reports utilization of the plan's windows.
//!
//! Four sources, deliberately different in cost:
//!
//! * **Codex** — no auth, no network. Every turn writes a `rate_limits` snapshot
//!   into `~/.codex/sessions/**/rollout-*.jsonl`; the newest one is read from the
//!   file's TAIL (rollouts here reach 600 MB, so the whole file is never read).
//! * **Claude** — a live `GET /api/oauth/usage` with the subscription OAuth
//!   token, shelled out through `curl` so no HTTP/TLS dependency enters the tree
//!   and a corporate MITM CA is trusted exactly as the user's other tools trust it.
//! * **Kiro** — its own bounded `kiro-cli chat --no-interactive /usage` report.
//!   That is account plan-credit usage; per-session context occupancy remains in
//!   [`crate::session::Session::context_pct`] and is not mislabeled as quota.
//! * **Cursor** — account quota from Cursor's first-party `/api/usage-summary`,
//!   authenticated with the Cursor Agent's macOS Keychain access token. This is
//!   separate from chat metadata, context occupancy, and per-session metering.
//!
//! Every failure carries its reason so the UI can say WHY a number is absent.
//! A missing window is never rendered as `0%` — that would read as "plenty left".

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// Bytes read from the end of a rollout when looking for its last snapshot.
/// `rate_limits` is written every turn, so the newest is always near the end;
/// not finding one within this window means giving up rather than reading a
/// multi-hundred-megabyte file to be thorough.
const ROLLOUT_TAIL_BYTES: u64 = 1 << 20;

/// Claude's OAuth usage endpoint and the beta header it requires.
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_BETA_HEADER: &str = "anthropic-beta: oauth-2025-04-20";
const CURSOR_USAGE_URL: &str = "https://cursor.com/api/usage-summary";
/// Where Cursor Agent stores its token. Only the macOS lookup reads these, so
/// they carry its gate too — otherwise a Linux build has them as dead code.
#[cfg(target_os = "macos")]
const CURSOR_KEYCHAIN_SERVICE: &str = "cursor-access-token";
#[cfg(target_os = "macos")]
const CURSOR_KEYCHAIN_ACCOUNT: &str = "cursor-user";

/// Seconds before the `curl` call is abandoned. The readout is decoration; it
/// must never hold anything up.
const CURL_TIMEOUT_SECS: u32 = 8;
/// Kiro's hidden `/usage` command sometimes keeps its harness alive after
/// printing a complete report. The readout is optional, so bound the whole
/// child and let the next refresh try again.
const KIRO_USAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// Same reason as the keychain constants above: only macOS shells out to
/// `security(1)`, so elsewhere this bound has nothing to bound.
#[cfg(target_os = "macos")]
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Kiro's account-level plan usage as reported by its own `/usage` command.
/// This is distinct from both context-window occupancy and the per-session
/// metering entries in sidecars: it has the plan ceiling and reset date needed
/// for an honest percentage gauge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KiroLimits {
    pub plan_name: Option<String>,
    pub credits_used: Option<f64>,
    pub credits_total: Option<f64>,
    pub used_percent: Option<f64>,
    pub reset_date: Option<String>,
}

impl KiroLimits {
    pub fn has_any(&self) -> bool {
        self.credits_used.is_some()
            && self.credits_total.is_some_and(|total| total > 0.0)
            && self.used_percent.is_some()
    }
}

/// Which first-party usage-summary block supplied the Cursor account quota.
/// `OnDemand` is deliberately absent: spend billing must not silently replace
/// the included-plan allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorQuotaSource {
    Plan,
    IndividualOverall,
    TeamPooled,
}

/// Cursor account-level quota from `/api/usage-summary`.
///
/// Monetary values remain in the endpoint's cents unit. They are not context
/// tokens, per-session tokens, or the on-demand spend meter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CursorLimits {
    pub used_percent: Option<f64>,
    pub used_cents: Option<i64>,
    pub limit_cents: Option<i64>,
    pub remaining_cents: Option<i64>,
    pub billing_cycle_start: Option<String>,
    pub billing_cycle_end: Option<String>,
    pub membership_type: Option<String>,
    pub limit_type: Option<String>,
    pub is_unlimited: bool,
    pub source: Option<CursorQuotaSource>,
}

impl CursorLimits {
    pub fn has_any(&self) -> bool {
        self.used_percent.is_some() || self.is_unlimited
    }
}

/// One account window for the footer, as values rather than a rendered line.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuotaRow {
    /// `claude 5h`, `kiro`, `cursor` — what the row is about.
    pub label: String,
    /// Percent USED. `None` means no window was reported; draw no gauge.
    pub used_percent: Option<f64>,
    /// The figure behind the percentage, or the reason there isn't one.
    pub detail: String,
    /// When the window rolls over, already shortened for display.
    pub resets: Option<String>,
}

impl QuotaRow {
    /// A row that explains an absent reading instead of implying a full one.
    fn reason(label: &str, why: &str) -> Self {
        Self {
            label: label.to_string(),
            used_percent: None,
            detail: why.to_string(),
            resets: None,
        }
    }

    /// True when this row carries a number the footer can gauge.
    pub fn has_gauge(&self) -> bool {
        self.used_percent.is_some()
    }
}

/// The last reading, kept on disk so a fresh start has something to show.
///
/// Reading the accounts takes seconds — Kiro alone runs its whole CLI — and a
/// footer that is blank until then teaches people to ignore it. The cache is
/// shown immediately and replaced the moment a live reading lands; the caller
/// is handed the time it was written so a stale number can say so rather than
/// pass for current.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct QuotaCache {
    written_at: i64,
    rows: Vec<QuotaRow>,
}

fn quota_cache_path(home: &Path) -> PathBuf {
    home.join(".mindplayer").join("limits-cache.json")
}

/// Store a reading. Failure is silent on purpose: a cache that cannot be
/// written must never interfere with the reading it was meant to speed up.
pub fn save_quota_cache(home: &Path, rows: &[QuotaRow]) {
    // An empty reading is not a reading. Writing it would replace the last
    // good one with nothing, so the next start would be blank — exactly what
    // the cache exists to prevent.
    if rows.is_empty() {
        return;
    }
    let cache = QuotaCache {
        written_at: chrono::Utc::now().timestamp(),
        rows: rows.to_vec(),
    };
    let Ok(body) = serde_json::to_vec(&cache) else {
        return;
    };
    let path = quota_cache_path(home);
    if let Ok(mut f) = crate::private::open_private(&path, false) {
        use std::io::Write;
        let _ = f.write_all(&body);
    }
}

/// The stored reading and when it was taken, or `None` when there isn't one.
pub fn load_quota_cache(home: &Path) -> Option<(Vec<QuotaRow>, chrono::DateTime<chrono::Utc>)> {
    let body = std::fs::read(quota_cache_path(home)).ok()?;
    let cache: QuotaCache = serde_json::from_slice(&body).ok()?;
    if cache.rows.is_empty() {
        return None;
    }
    let at = chrono::DateTime::from_timestamp(cache.written_at, 0)?;
    Some((cache.rows, at))
}

/// Shorten a reset epoch: a clock for windows that roll over within a day, a
/// date for the ones that don't.
fn epoch_label(epoch: i64, clock: bool) -> Option<String> {
    use chrono::TimeZone;
    let when = chrono::Local.timestamp_opt(epoch, 0).single()?;
    Some(
        when.format(if clock { "%H:%M" } else { "%m-%d" })
            .to_string(),
    )
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

/// All providers, each either a reading or the reason there isn't one.
#[derive(Debug, Clone)]
pub struct Limits {
    pub claude: Result<ClaudeLimits, String>,
    pub codex: Result<CodexLimits, String>,
    pub kiro: Result<KiroLimits, String>,
    pub cursor: Result<CursorLimits, String>,
}

impl Limits {
    /// True when at least one provider produced a displayable number.
    pub fn has_any(&self) -> bool {
        self.claude.as_ref().is_ok_and(ClaudeLimits::has_any)
            || self.codex.as_ref().is_ok_and(CodexLimits::has_any)
            || self.kiro.as_ref().is_ok_and(KiroLimits::has_any)
            || self.cursor.as_ref().is_ok_and(CursorLimits::has_any)
    }

    /// One row per account window, as values rather than a formatted line, so
    /// the footer can draw a gauge instead of printing a number to be read.
    ///
    /// Claude contributes two rows when it reports both windows; every other
    /// provider contributes one. `used_percent` is `None` whenever the provider
    /// reports no window at all — the caller must draw no gauge then, because
    /// an empty gauge reads as "plenty left" when the truth is "not reported".
    pub fn quota_rows(&self) -> Vec<QuotaRow> {
        let mut out = Vec::new();
        match &self.claude {
            Ok(c) if c.has_any() => {
                for (label, used, reset, clock) in [
                    ("claude 5h", c.five_hour, c.five_hour_reset, true),
                    ("claude wk", c.seven_day, c.seven_day_reset, false),
                ] {
                    if let Some(p) = used {
                        out.push(QuotaRow {
                            label: label.into(),
                            used_percent: Some(p),
                            detail: String::new(),
                            resets: reset.and_then(|e| epoch_label(e, clock)),
                        });
                    }
                }
            }
            Ok(_) => out.push(QuotaRow::reason("claude", "no windows reported")),
            Err(e) => out.push(QuotaRow::reason("claude", e)),
        }
        // Codex reports `primary`/`secondary` only on plans metered by windows.
        // On a business plan both are null and the monthly figure its own
        // `/status` shows is never written to the rollout, so there is no
        // percentage to gauge here — only the balance, which is not the same
        // quantity and must not be drawn as one.
        match &self.codex {
            Ok(c) if c.has_any() => {
                let windowed = [
                    (c.primary, c.primary_window_minutes),
                    (c.secondary, c.secondary_window_minutes),
                ];
                let mut any_window = false;
                for (used, window) in windowed {
                    if let Some(p) = used {
                        any_window = true;
                        out.push(QuotaRow {
                            label: format!("codex {}", window_label(window)),
                            used_percent: Some(p),
                            detail: String::new(),
                            resets: None,
                        });
                    }
                }
                if !any_window {
                    let detail = if c.credits_unlimited {
                        "credits unlimited".to_string()
                    } else if let Some(b) = c.credit_balance {
                        format!("credits {b:.0} · no window reported")
                    } else {
                        "no window reported".to_string()
                    };
                    out.push(QuotaRow {
                        label: "codex".into(),
                        used_percent: None,
                        detail,
                        resets: None,
                    });
                }
            }
            Ok(c) => out.push(QuotaRow::reason(
                "codex",
                &format!(
                    "no windows on {} plan",
                    c.plan_type.as_deref().unwrap_or("this")
                ),
            )),
            Err(e) => out.push(QuotaRow::reason("codex", e)),
        }
        match &self.kiro {
            Ok(k) if k.has_any() => out.push(QuotaRow {
                label: "kiro".into(),
                used_percent: k.used_percent,
                detail: match (k.credits_used, k.credits_total) {
                    (Some(u), Some(t)) => {
                        format!("{}/{} cr", compact_decimal(u), compact_decimal(t))
                    }
                    _ => String::new(),
                },
                resets: k.reset_date.clone(),
            }),
            Ok(_) => out.push(QuotaRow::reason("kiro", "no usage reported")),
            Err(e) => out.push(QuotaRow::reason("kiro", e)),
        }
        match &self.cursor {
            Ok(c) if c.has_any() => out.push(QuotaRow {
                label: "cursor".into(),
                used_percent: c.used_percent,
                detail: match (c.used_cents, c.limit_cents) {
                    (Some(u), Some(l)) => {
                        format!("${:.2}/${}", u as f64 / 100.0, l / 100)
                    }
                    _ => String::new(),
                },
                resets: c
                    .billing_cycle_end
                    .as_deref()
                    .and_then(cursor_date_label)
                    .map(str::to_string),
            }),
            Ok(_) => out.push(QuotaRow::reason("cursor", "no usage reported")),
            Err(e) => out.push(QuotaRow::reason("cursor", e)),
        }
        out
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
                self.kiro.as_ref().is_ok_and(KiroLimits::has_any),
                self.cursor.as_ref().is_ok_and(CursorLimits::has_any),
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
        match &self.kiro {
            Ok(k) if k.has_any() => {
                let mut parts = Vec::new();
                if let Some(percent) = k.used_percent {
                    parts.push(format!("{percent:.1}%"));
                }
                if let (Some(used), Some(total)) = (k.credits_used, k.credits_total) {
                    parts.push(format!(
                        "{}/{} cr",
                        compact_decimal(used),
                        compact_decimal(total)
                    ));
                }
                if let Some(reset) = k.reset_date.as_deref() {
                    parts.push(format!("reset {reset}"));
                }
                out.push(format!("kiro  {}", parts.join("  ")));
            }
            Ok(k) => out.push(format!(
                "kiro  no usage metrics on {}",
                k.plan_name.as_deref().unwrap_or("this plan")
            )),
            Err(e) => out.push(format!("kiro  — {e}")),
        }
        match &self.cursor {
            Ok(c) if c.has_any() => {
                let mut parts = Vec::new();
                if c.source == Some(CursorQuotaSource::TeamPooled) {
                    parts.push("team".to_string());
                }
                if let Some(percent) = c.used_percent {
                    parts.push(format!("{percent:.1}%"));
                } else if c.is_unlimited {
                    parts.push("unlimited".to_string());
                }
                if let (Some(used), Some(limit)) = (c.used_cents, c.limit_cents) {
                    parts.push(format!(
                        "${}/${}",
                        compact_decimal(used as f64 / 100.0),
                        compact_decimal(limit as f64 / 100.0)
                    ));
                }
                if let Some(reset) = c.billing_cycle_end.as_deref().and_then(cursor_date_label) {
                    parts.push(format!("reset {reset}"));
                }
                out.push(format!("cursor  {}", parts.join("  ")));
            }
            Ok(c) => out.push(format!(
                "cursor  no numeric quota on {} plan",
                c.membership_type.as_deref().unwrap_or("this")
            )),
            Err(e) => out.push(format!("cursor  — {e}")),
        }
        out
    }
}

fn cursor_date_label(raw: &str) -> Option<&str> {
    chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    raw.get(..10)
}

fn compact_decimal(value: f64) -> String {
    let precision = if value >= 1000.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    let mut text = format!("{value:.precision$}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
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

/// Read all providers concurrently. Each source has independent latency and
/// failure modes; serializing them let a slow Claude/Codex probe consume the
/// TUI's entire fetch deadline before Kiro even started.
pub fn fetch(home: &Path) -> Limits {
    fetch_parallel(
        || claude_limits(home),
        || codex_limits(home),
        || kiro_limits(home),
        || cursor_limits(home),
    )
}

fn fetch_parallel<C, D, K, U>(claude: C, codex: D, kiro: K, cursor: U) -> Limits
where
    C: FnOnce() -> Result<ClaudeLimits, String> + Send,
    D: FnOnce() -> Result<CodexLimits, String> + Send,
    K: FnOnce() -> Result<KiroLimits, String> + Send,
    U: FnOnce() -> Result<CursorLimits, String> + Send,
{
    std::thread::scope(|scope| {
        let claude = scope.spawn(claude);
        let codex = scope.spawn(codex);
        let kiro = scope.spawn(kiro);
        let cursor = scope.spawn(cursor);
        Limits {
            claude: claude
                .join()
                .unwrap_or_else(|_| Err("Claude limits probe panicked".into())),
            codex: codex
                .join()
                .unwrap_or_else(|_| Err("Codex limits probe panicked".into())),
            kiro: kiro
                .join()
                .unwrap_or_else(|_| Err("Kiro limits probe panicked".into())),
            cursor: cursor
                .join()
                .unwrap_or_else(|_| Err("Cursor limits probe panicked".into())),
        }
    })
}

// ── Kiro ──────────────────────────────────────────────────────────────────

/// Ask Kiro's own CLI for the account-level credit window. The command is the
/// same local `/usage` surface Kiro renders interactively; no transcript or
/// project content is sent. A missing local Kiro store skips the child entirely
/// (important for fixture homes and machines that do not use Kiro).
pub fn kiro_limits(home: &Path) -> Result<KiroLimits, String> {
    if !home.join(".kiro").exists() {
        return Err("no local Kiro profile".into());
    }
    let mut command = Command::new("kiro-cli");
    command
        .args(["chat", "--no-interactive", "/usage"])
        .current_dir(home)
        .env("HOME", home);
    let (status, stdout, stderr) = run_bounded(command, KIRO_USAGE_TIMEOUT, "kiro-cli")?;
    let output = [stdout, stderr]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !status.success() {
        return Err(format!("kiro-cli /usage exited with {status}"));
    }
    parse_kiro_usage(&output)
}

/// Parse the stable, human-readable report emitted by
/// `kiro-cli chat --no-interactive /usage`.
pub fn parse_kiro_usage(output: &str) -> Result<KiroLimits, String> {
    let clean = strip_ansi(output);
    let output = clean.as_str();
    let usage_line = output.lines().find(|line| line.contains("Estimated Usage"));
    let mut result = KiroLimits::default();
    if let Some(line) = usage_line {
        for part in line.split('|').map(str::trim) {
            if let Some(raw) = part.strip_prefix("resets on ") {
                let date = raw.trim();
                if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok() {
                    result.reset_date = Some(date.to_string());
                }
            } else if !part.is_empty()
                && !part.eq_ignore_ascii_case("Estimated Usage")
                && !part.to_ascii_lowercase().contains("managed by")
            {
                result.plan_name = Some(part.to_string());
            }
        }
    }

    let credit_line = output.lines().find(|line| line.contains("Credits ("));
    if let Some(line) = credit_line {
        let inside = line
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inside, _)| inside)
            .ok_or_else(|| "malformed Kiro credits line".to_string())?;
        let words: Vec<&str> = inside.split_whitespace().collect();
        let of = words
            .iter()
            .position(|word| word.eq_ignore_ascii_case("of"))
            .ok_or_else(|| "malformed Kiro credits line".to_string())?;
        let used = words
            .get(of.wrapping_sub(1))
            .and_then(|raw| parse_finite_nonnegative(raw))
            .ok_or_else(|| "invalid Kiro credits used".to_string())?;
        let total = words
            .get(of + 1)
            .and_then(|raw| parse_finite_nonnegative(raw))
            .filter(|value| *value > 0.0)
            .ok_or_else(|| "invalid Kiro credits total".to_string())?;
        if used > total {
            return Err("Kiro plan usage exceeds its credit total".into());
        }
        result.credits_used = Some(used);
        result.credits_total = Some(total);
        result.used_percent = Some((used / total) * 100.0);
    }

    if result.plan_name.is_none() && !result.has_any() {
        return Err("no recognizable Kiro usage report".into());
    }
    Ok(result)
}

fn parse_finite_nonnegative(raw: &str) -> Option<f64> {
    let value = raw.replace(',', "").parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                // CSI ends at its final byte in the ASCII 0x40..=0x7e range.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC ends at BEL or ST (ESC + backslash).
                let mut saw_escape = false;
                for c in chars.by_ref() {
                    if c == '\u{7}' || (saw_escape && c == '\\') {
                        break;
                    }
                    saw_escape = c == '\u{1b}';
                }
            }
            Some(_) => {
                // Two-byte escape sequence.
                chars.next();
            }
            None => {}
        }
    }
    out
}

fn run_bounded(
    mut command: Command,
    timeout: Duration,
    label: &str,
) -> Result<(std::process::ExitStatus, String, String), String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start {label}: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(format!("cannot capture {label} stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(format!("cannot capture {label} stderr"))?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("cannot wait for {label}: {e}"))?
        {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(format!("{label} timed out"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| format!("{label} stdout reader panicked"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| format!("{label} stderr reader panicked"))?;
    Ok((
        status,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

// ── Cursor ─────────────────────────────────────────────────────────────────

/// Fetch Cursor account quota from the same first-party endpoint used by
/// CodexBar. Only the Cursor Agent's macOS Keychain credential is used: browser
/// cookies and project/session data are never inspected or transmitted.
pub fn cursor_limits(home: &Path) -> Result<CursorLimits, String> {
    let token = cursor_access_token(home)?;
    let cookie = cursor_cookie_from_access_token(&token)?;
    let body = cursor_curl_json(&cookie)?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|e| format!("unparsable response from /api/usage-summary: {e}"))?;
    let parsed = parse_cursor_usage(&value)?;
    if parsed.has_any() {
        Ok(parsed)
    } else {
        Err("response carried no account quota".into())
    }
}

/// Parse the verified Cursor `/api/usage-summary` schema.
///
/// Source precedence follows CodexBar: regular plans use `individualUsage.plan`;
/// Enterprise/Team accounts then fall back to the personal `overall` cap, and
/// only finally to a shared team `pooled` cap. On-demand spend is not a quota
/// fallback because it is a different billing concept.
pub fn parse_cursor_usage(body: &Value) -> Result<CursorLimits, String> {
    if !body.is_object() {
        return Err("Cursor usage summary is not a JSON object".into());
    }

    fn cents(value: Option<&Value>) -> Option<i64> {
        value.and_then(Value::as_i64).filter(|v| *v >= 0)
    }
    fn percent(value: Option<&Value>) -> Option<f64> {
        value
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 100.0))
    }
    fn ratio(used: Option<i64>, limit: Option<i64>) -> Option<f64> {
        match (used, limit) {
            (Some(used), Some(limit)) if limit > 0 => {
                Some(((used as f64 / limit as f64) * 100.0).clamp(0.0, 100.0))
            }
            _ => None,
        }
    }
    fn string(body: &Value, key: &str) -> Option<String> {
        body.get(key).and_then(Value::as_str).map(str::to_string)
    }

    let is_unlimited = body
        .get("isUnlimited")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let plan = body
        .pointer("/individualUsage/plan")
        .filter(|block| block.get("enabled").and_then(Value::as_bool) != Some(false));
    let plan_used = cents(plan.and_then(|v| v.get("used")));
    let plan_limit = cents(plan.and_then(|v| v.get("limit")));
    let plan_remaining = cents(plan.and_then(|v| v.get("remaining")));
    let auto = percent(plan.and_then(|v| v.get("autoPercentUsed")));
    let api = percent(plan.and_then(|v| v.get("apiPercentUsed")));
    let plan_percent =
        percent(plan.and_then(|v| v.get("totalPercentUsed"))).or_else(|| match (auto, api) {
            (Some(auto), Some(api)) => Some((auto + api) / 2.0),
            (Some(auto), None) => Some(auto),
            (None, Some(api)) => Some(api),
            (None, None) => ratio(plan_used, plan_limit),
        });

    let overall = body
        .pointer("/individualUsage/overall")
        .filter(|block| block.get("enabled").and_then(Value::as_bool) != Some(false));
    let overall_used = cents(overall.and_then(|v| v.get("used")));
    let overall_limit = cents(overall.and_then(|v| v.get("limit")));
    let overall_remaining = cents(overall.and_then(|v| v.get("remaining")));
    let overall_percent = ratio(overall_used, overall_limit);

    let pooled = body
        .pointer("/teamUsage/pooled")
        .filter(|block| block.get("enabled").and_then(Value::as_bool) != Some(false));
    let pooled_used = cents(pooled.and_then(|v| v.get("used")));
    let pooled_limit = cents(pooled.and_then(|v| v.get("limit")));
    let pooled_remaining = cents(pooled.and_then(|v| v.get("remaining")));
    let pooled_percent = ratio(pooled_used, pooled_limit);

    let (used_percent, used_cents, limit_cents, remaining_cents, source) = if is_unlimited {
        (None, None, None, None, None)
    } else if plan_percent.is_some() {
        (
            plan_percent,
            plan_used,
            plan_limit,
            plan_remaining,
            Some(CursorQuotaSource::Plan),
        )
    } else if overall_percent.is_some() {
        (
            overall_percent,
            overall_used,
            overall_limit,
            overall_remaining,
            Some(CursorQuotaSource::IndividualOverall),
        )
    } else if pooled_percent.is_some() {
        (
            pooled_percent,
            pooled_used,
            pooled_limit,
            pooled_remaining,
            Some(CursorQuotaSource::TeamPooled),
        )
    } else {
        (None, None, None, None, None)
    };

    Ok(CursorLimits {
        used_percent,
        used_cents,
        limit_cents,
        remaining_cents,
        billing_cycle_start: string(body, "billingCycleStart"),
        billing_cycle_end: string(body, "billingCycleEnd"),
        membership_type: string(body, "membershipType"),
        limit_type: string(body, "limitType"),
        is_unlimited,
        source,
    })
}

fn cursor_access_token(home: &Path) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        if !is_real_home(home) {
            return Err("Cursor Keychain lookup is disabled for a non-user home".into());
        }
        let mut command = Command::new("/usr/bin/security");
        command.args([
            "find-generic-password",
            "-s",
            CURSOR_KEYCHAIN_SERVICE,
            "-a",
            CURSOR_KEYCHAIN_ACCOUNT,
            "-w",
        ]);
        let (status, stdout, _) = run_bounded(command, KEYCHAIN_TIMEOUT, "security(1)")?;
        if !status.success() {
            return Err("no Cursor Agent access token in macOS Keychain".into());
        }
        let token = stdout.trim_end_matches(['\r', '\n']);
        if token.is_empty() || token.chars().any(char::is_control) {
            return Err("Cursor Agent access token is empty or malformed".into());
        }
        Ok(token.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        Err("Cursor account quota authentication is currently supported on macOS".into())
    }
}

/// Build the WorkOS dashboard cookie used by Cursor and CodexBar. The JWT is
/// accepted only with three base64url segments, a safe subject, and at least 60
/// seconds of remaining validity.
fn cursor_cookie_from_access_token(token: &str) -> Result<String, String> {
    let segments: Vec<&str> = token.split('.').collect();
    if segments.len() != 3
        || segments.iter().any(|s| {
            s.is_empty()
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
    {
        return Err("Cursor Agent access token is not a valid JWT".into());
    }
    let payload = base64url_decode(segments[1])
        .ok_or_else(|| "Cursor Agent JWT payload is malformed".to_string())?;
    let payload: Value = serde_json::from_slice(&payload)
        .map_err(|_| "Cursor Agent JWT payload is malformed".to_string())?;
    let subject = payload
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 256
                && s.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(b, b'_' | b'-' | b'.' | b'|' | b'@' | b'+')
                })
        })
        .ok_or_else(|| "Cursor Agent JWT has no safe subject".to_string())?;
    let expires = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or_else(|| "Cursor Agent JWT has no expiration".to_string())?;
    if expires <= chrono::Utc::now().timestamp() + 60 {
        return Err("Cursor Agent access token is expired or near expiry".into());
    }
    Ok(format!("WorkosCursorSessionToken={subject}%3A%3A{token}"))
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }

    if input.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in input.bytes() {
        accumulator = (accumulator << 6) | u32::from(value(byte)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1_u32 << bits).saturating_sub(1);
        }
    }
    (accumulator == 0).then_some(out)
}

/// Call only the fixed Cursor endpoint. The cookie is in a mode-0600 config,
/// never argv; redirects are disabled and curl's default rc is disabled first.
fn cursor_curl_json(cookie: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!(
        "mindplayer-cursor-usage-{}-{}.curlrc",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    write_private(&path, &cursor_curl_config(cookie))
        .map_err(|e| format!("cannot stage Cursor curl config: {e}"))?;
    let output = Command::new("curl").args(curl_args()).arg(&path).output();
    let _ = std::fs::remove_file(&path);
    let output = output.map_err(|e| format!("cannot run curl for Cursor usage: {e}"))?;
    if !output.status.success() {
        return Err(format!("Cursor usage request failed ({})", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn cursor_curl_config(cookie: &str) -> String {
    let escaped = cookie
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "silent\nshow-error\nfail\nproto = \"=https\"\nmax-time = {CURL_TIMEOUT_SECS}\nurl = \"{CURSOR_USAGE_URL}\"\nheader = \"Cookie: {escaped}\"\n"
    )
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
        let user = std::env::var("USER").ok();
        let mut blobs = Vec::new();
        for args in keychain_lookups(user.as_deref()) {
            let out = std::process::Command::new("security")
                .args(&args)
                .output()
                .map_err(|e| format!("cannot run security(1): {e}"))?;
            if out.status.success() {
                blobs.push(String::from_utf8_lossy(&out.stdout).into_owned());
            }
        }
        if let Some(t) = first_claude_token(blobs.iter().map(String::as_str)) {
            return Ok(t);
        }
        if !blobs.is_empty() {
            return Err("keychain item has no claudeAiOauth token".into());
        }
    }
    Err("no subscription OAuth token found".into())
}

/// The keychain service Claude Code stores its credential JSON under.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// How to ask `security(1)` for the credential, most specific first.
///
/// The login keychain can hold several items under this one service name, and
/// asking for the service alone returns whichever comes first — on this machine
/// a stale item whose account is `unknown` and which carries only `mcpOAuth`.
/// The item Claude Code maintains has the OS user as its account, so that
/// lookup leads; the service-only one stays as a fallback for a login that
/// predates the convention.
#[cfg(any(target_os = "macos", test))]
fn keychain_lookups(user: Option<&str>) -> Vec<Vec<String>> {
    let to_args = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    if let Some(u) = user.filter(|u| !u.is_empty()) {
        out.push(to_args(&[
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            u,
            "-w",
        ]));
    }
    out.push(to_args(&[
        "find-generic-password",
        "-s",
        KEYCHAIN_SERVICE,
        "-w",
    ]));
    out
}

/// The first credential blob that carries a usable subscription token.
#[cfg(any(target_os = "macos", test))]
fn first_claude_token<'a>(blobs: impl IntoIterator<Item = &'a str>) -> Option<String> {
    blobs.into_iter().find_map(token_from_credentials)
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
    fn cursor_plan_usage_uses_the_dashboard_total_percent() {
        let body = json!({
            "billingCycleStart": "2026-09-01T00:00:00.000Z",
            "billingCycleEnd": "2026-10-01T00:00:00.000Z",
            "membershipType": "pro",
            "limitType": "user",
            "isUnlimited": false,
            "individualUsage": {
                "plan": {
                    "enabled": true,
                    "used": 1500,
                    "limit": 5000,
                    "remaining": 3500,
                    "autoPercentUsed": 20.0,
                    "apiPercentUsed": 40.0,
                    "totalPercentUsed": 30.0
                }
            }
        });
        let got = parse_cursor_usage(&body).expect("verified usage-summary shape parses");
        assert_eq!(got.used_percent, Some(30.0));
        assert_eq!(got.used_cents, Some(1500));
        assert_eq!(got.limit_cents, Some(5000));
        assert_eq!(got.source, Some(CursorQuotaSource::Plan));
        assert_eq!(
            got.billing_cycle_end.as_deref(),
            Some("2026-10-01T00:00:00.000Z")
        );
        assert!(got.has_any());
    }

    #[test]
    fn cursor_plan_ratio_is_used_when_percent_fields_are_absent() {
        let got = parse_cursor_usage(&json!({
            "individualUsage": { "plan": { "used": 4900, "limit": 50000 } }
        }))
        .unwrap();
        assert_eq!(got.used_percent, Some(9.8));
        assert_eq!(got.source, Some(CursorQuotaSource::Plan));
    }

    #[test]
    fn cursor_enterprise_prefers_the_personal_overall_cap() {
        let got = parse_cursor_usage(&json!({
            "membershipType": "enterprise",
            "individualUsage": {
                "overall": { "enabled": true, "used": 7384, "limit": 10000, "remaining": 2616 }
            },
            "teamUsage": {
                "pooled": { "enabled": true, "used": 12725135, "limit": 28122000, "remaining": 15396865 }
            }
        }))
        .unwrap();
        assert!((got.used_percent.unwrap() - 73.84).abs() < 0.0001);
        assert_eq!(got.used_cents, Some(7384));
        assert_eq!(got.limit_cents, Some(10000));
        assert_eq!(got.source, Some(CursorQuotaSource::IndividualOverall));
    }

    #[test]
    fn cursor_team_pool_is_only_a_last_resort() {
        let got = parse_cursor_usage(&json!({
            "membershipType": "enterprise",
            "teamUsage": {
                "pooled": { "enabled": true, "used": 12725135, "limit": 28122000 }
            }
        }))
        .unwrap();
        assert!(got.used_percent.unwrap() > 45.0);
        assert!(got.used_percent.unwrap() < 45.5);
        assert_eq!(got.source, Some(CursorQuotaSource::TeamPooled));
    }

    #[test]
    fn cursor_missing_numeric_quota_never_becomes_zero() {
        let got = parse_cursor_usage(&json!({
            "membershipType": "enterprise",
            "isUnlimited": false,
            "individualUsage": {
                "plan": { "enabled": true, "used": 0, "limit": null, "remaining": null }
            }
        }))
        .unwrap();
        assert_eq!(got.used_percent, None);
        assert_eq!(got.limit_cents, None);
        assert!(!got.has_any(), "missing cap must not read as 0% used");
    }

    #[test]
    fn cursor_disabled_quota_block_never_becomes_zero_percent() {
        let got = parse_cursor_usage(&json!({
            "membershipType": "enterprise",
            "isUnlimited": false,
            "individualUsage": {
                "plan": { "enabled": false, "used": 0, "limit": 10000, "totalPercentUsed": 0 }
            }
        }))
        .unwrap();
        assert_eq!(got.used_percent, None);
        assert!(!got.has_any(), "a disabled block is not an unused quota");
    }

    #[test]
    fn cursor_unlimited_is_visible_without_a_fake_percentage() {
        let got = parse_cursor_usage(&json!({
            "membershipType": "enterprise",
            "isUnlimited": true,
            "individualUsage": {
                "plan": {
                    "enabled": true,
                    "used": 2000,
                    "limit": 10000,
                    "totalPercentUsed": 20
                }
            }
        }))
        .unwrap();
        assert_eq!(got.used_percent, None);
        assert!(got.has_any());
        let lines = Limits {
            claude: Err("not under test".into()),
            codex: Err("not under test".into()),
            cursor: Ok(got),
            kiro: Err("not under test".into()),
        }
        .summary_lines();
        assert!(lines[3].contains("unlimited"), "{lines:?}");
        assert!(!lines[3].contains("0.0%"), "{lines:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_fixture_home_never_reaches_the_cursor_keychain() {
        let fixture =
            std::env::temp_dir().join(format!("mp-cursor-fixture-home-{}", std::process::id()));
        let err = cursor_access_token(&fixture).unwrap_err();
        assert!(err.contains("non-user home"), "{err}");
    }

    #[test]
    fn hostile_cursor_cookie_text_cannot_inject_a_curl_directive() {
        let cfg = cursor_curl_config("safe\"\nurl = \"http://evil");
        let urls: Vec<_> = cfg
            .lines()
            .filter(|line| line.trim_start().starts_with("url ="))
            .collect();
        assert_eq!(urls, [format!("url = \"{CURSOR_USAGE_URL}\"")]);
        let cookie_lines: Vec<_> = cfg
            .lines()
            .filter(|line| line.contains("http://evil"))
            .collect();
        assert_eq!(cookie_lines.len(), 1, "{cfg}");
        assert!(cookie_lines[0].starts_with("header ="), "{cfg}");
    }

    #[test]
    fn cursor_cookie_is_derived_from_a_live_jwt_without_reaching_argv() {
        let payload = base64url_encode_for_test(br#"{"sub":"auth0|user-123","exp":4102444800}"#);
        let token = format!("header.{payload}.signature");
        let cookie = cursor_cookie_from_access_token(&token).expect("valid token derives a cookie");
        assert_eq!(
            cookie,
            format!("WorkosCursorSessionToken=auth0|user-123%3A%3A{token}")
        );
        let cfg = cursor_curl_config(&cookie);
        assert!(cfg.contains(CURSOR_USAGE_URL));
        assert!(
            cfg.contains(&cookie),
            "cookie is staged in the private config"
        );
        assert!(!cfg.lines().any(|line| line.trim() == "location"));
        assert_eq!(curl_args(), ["-q", "--config"]);
    }

    fn base64url_encode_for_test(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            out.push(TABLE[(a >> 2) as usize] as char);
            out.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(TABLE[(c & 0x3f) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn kiro_usage_reads_plan_credits_from_the_cli_report() {
        let report = r#"
Error: user defined default infra not found. Falling back to in-memory default
Estimated Usage | resets on 2026-10-01 | KIRO POWER
Credits (185.50 of 10000 covered in plan)
████████████████████████████████████████████████████████ 1.9%
Since your account is through your organization, contact your administrator.
"#;
        let got = parse_kiro_usage(report).expect("current kiro-cli report parses");
        assert_eq!(got.plan_name.as_deref(), Some("KIRO POWER"));
        assert_eq!(got.credits_used, Some(185.5));
        assert_eq!(got.credits_total, Some(10_000.0));
        assert_eq!(got.used_percent, Some(1.855));
        assert_eq!(got.reset_date.as_deref(), Some("2026-10-01"));
        assert!(got.has_any());
    }

    #[test]
    fn kiro_usage_strips_ansi_before_parsing_credit_metrics() {
        let report = "\x1b[1mEstimated Usage\x1b[0m | resets on 2026-10-01 | \x1b[38;2;123;200;255mKIRO POWER\x1b[0m\n\x1b[1mCredits\x1b[0m (185.50 of 10000 covered in plan)\n";
        let got = parse_kiro_usage(report).expect("colored kiro-cli report parses");
        assert_eq!(got.plan_name.as_deref(), Some("KIRO POWER"));
        assert_eq!(got.credits_used, Some(185.5));
        assert_eq!(got.credits_total, Some(10_000.0));
        assert_eq!(got.used_percent, Some(1.855));
        assert_eq!(got.reset_date.as_deref(), Some("2026-10-01"));
    }
    #[test]
    fn kiro_usage_rejects_a_false_zero_when_no_credit_metrics_exist() {
        let report = "Estimated Usage | KIRO ENTERPRISE | managed by organization";
        let got = parse_kiro_usage(report).expect("plan-only report is still valid");
        assert_eq!(got.plan_name.as_deref(), Some("KIRO ENTERPRISE"));
        assert_eq!(got.credits_used, None);
        assert_eq!(got.credits_total, None);
        assert_eq!(got.used_percent, None);
        assert!(!got.has_any(), "missing metrics must not become zero usage");
    }

    #[test]
    fn kiro_usage_is_a_third_limits_row() {
        let limits = Limits {
            claude: Ok(ClaudeLimits::default()),
            codex: Ok(CodexLimits::default()),
            cursor: Err("not under test".into()),
            kiro: Ok(parse_kiro_usage(
                "Estimated Usage | resets on 2026-10-01 | KIRO POWER\nCredits (185.50 of 10000 covered in plan)",
            )
            .unwrap()),
        };
        let rows = limits.summary_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[2],
            ("kiro  1.9%  185.5/10000 cr  reset 2026-10-01".into(), true)
        );
    }

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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
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
            cursor: Err("not under test".into()),
            kiro: Err("no local Kiro profile".into()),
        };
        assert!(mixed.has_any(), "aggregate is true — the old trap");
        let rows = mixed.summary_rows();
        assert_eq!(rows.len(), 4);
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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
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
            cursor: Err("not under test".into()),
            kiro: Err("no local Kiro profile".into()),
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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
        };
        let line = &limits.summary_lines()[1];
        assert!(line.contains("some future limit reached"), "{line}");
    }

    /// The cache exists so a fresh start is not blank for the seconds a real
    /// reading takes. It must survive the round trip and keep its timestamp.
    #[test]
    fn a_saved_reading_comes_back_with_the_time_it_was_taken() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        assert!(load_quota_cache(home).is_none(), "nothing saved yet");

        let rows = vec![
            QuotaRow {
                label: "kiro".into(),
                used_percent: Some(6.4),
                detail: "639.7/10000 cr".into(),
                resets: Some("2026-10-01".into()),
            },
            QuotaRow::reason("codex", "no window reported"),
        ];
        let before = chrono::Utc::now().timestamp();
        save_quota_cache(home, &rows);

        let (back, at) = load_quota_cache(home).expect("the reading comes back");
        assert_eq!(back, rows);
        assert!(at.timestamp() >= before, "the time it was taken is kept");

        // The file records account usage, so it is owner-only like the rest of
        // what mindplayer writes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(quota_cache_path(home))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "group/other can read it: {mode:o}");
        }

        // An empty reading is not worth showing on the next start, and a
        // corrupt file must not stop the app from running. Checked here rather
        // than in a test of its own: both need `HOME`, and two tests mutating
        // it race under the default parallel runner.
        save_quota_cache(home, &[]);
        assert_eq!(
            load_quota_cache(home).map(|(rows, _)| rows),
            Some(rows),
            "an empty save must not erase the last real reading"
        );

        std::fs::write(quota_cache_path(home), b"{ not json").unwrap();
        assert!(
            load_quota_cache(home).is_none(),
            "a corrupt cache is just absent"
        );
    }

    #[test]
    fn the_keychain_is_asked_for_the_users_account_before_any_account() {
        let lookups = keychain_lookups(Some("eden"));
        assert_eq!(lookups.len(), 2, "{lookups:?}");
        assert_eq!(
            lookups[0],
            [
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                "eden",
                "-w"
            ],
            "the item Claude Code writes (account = OS user) comes first"
        );
        assert_eq!(
            lookups[1],
            ["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"],
            "then whatever the service name alone resolves to"
        );
        assert_eq!(keychain_lookups(None).len(), 1, "no user → service-only");
        assert_eq!(
            keychain_lookups(Some("")).len(),
            1,
            "empty user → service-only"
        );
    }

    #[test]
    fn an_mcp_only_item_is_skipped_in_favour_of_one_holding_the_token() {
        let mcp_only = json!({ "mcpOAuth": { "srv": { "accessToken": "x" } } }).to_string();
        let real = json!({
            "mcpOAuth": {},
            "claudeAiOauth": { "accessToken": "tok-real", "expiresAt": 4_102_444_800_000i64 }
        })
        .to_string();
        assert_eq!(
            first_claude_token([mcp_only.as_str(), real.as_str()]),
            Some("tok-real".to_string())
        );
        assert_eq!(first_claude_token([mcp_only.as_str()]), None);
        assert_eq!(first_claude_token([]), None);
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
            cursor: Err("not under test".into()),
            kiro: Err("not under test".into()),
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

#[test]
fn provider_fetches_overlap_so_kiro_is_not_starved_by_slow_predecessors() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn observe(active: &AtomicUsize, peak: &AtomicUsize) {
        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        active.fetch_sub(1, Ordering::SeqCst);
    }

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let claude_probe = (active.clone(), peak.clone());
    let codex_probe = (active.clone(), peak.clone());
    let kiro_probe = (active.clone(), peak.clone());
    let cursor_probe = (active, peak.clone());

    let _ = fetch_parallel(
        move || {
            observe(&claude_probe.0, &claude_probe.1);
            Err::<ClaudeLimits, _>("synthetic".to_string())
        },
        move || {
            observe(&codex_probe.0, &codex_probe.1);
            Err::<CodexLimits, _>("synthetic".to_string())
        },
        move || {
            observe(&kiro_probe.0, &kiro_probe.1);
            Err::<KiroLimits, _>("synthetic".to_string())
        },
        move || {
            observe(&cursor_probe.0, &cursor_probe.1);
            Err::<CursorLimits, _>("synthetic".to_string())
        },
    );

    assert_eq!(
        peak.load(Ordering::SeqCst),
        4,
        "all provider probes must be in flight together"
    );
}
