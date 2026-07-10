//! Session discovery: scan the Codex and Claude session stores and parse each
//! `.jsonl` file into a [`Session`].
//!
//! Parsing is line-by-line best-effort: a malformed or unknown line is skipped
//! rather than discarding the whole session. Data shapes were verified against
//! real files:
//! - Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-<iso>-<uuid>.jsonl`, first
//!   line `session_meta` (carries `cwd`), cumulative tokens in
//!   `event_msg`/`token_count` near EOF.
//! - Claude: `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`, per-message
//!   `usage` summed across `assistant` messages. The directory name encodes the
//!   cwd (`/` and `.` replaced by `-`).
//!
//! Performance: the real store is multi-GB with individual Codex files up to
//! hundreds of MB. We never read a whole file just to discover it. Codex cwd is
//! read from the first line (cheap scope filter); tokens come from a bounded
//! tail read; titles from a bounded head read. Claude is filtered by directory
//! name before any file is opened.

use crate::session::{Agent, Session, TokenUsage};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Tail window growth is capped here so a pathological file without any
/// `token_count` line still costs at most this much I/O.
const MAX_TAIL_BYTES: u64 = 16 * 1024 * 1024;
const INITIAL_TAIL_BYTES: u64 = 256 * 1024;
/// How many leading lines to scan for metadata / the first user prompt.
const HEAD_LINES: usize = 300;
/// Upper bound on bytes read from a Claude transcript. Claude is parsed in full
/// (for token totals), but we cap the read so a crafted/corrupt file — e.g. one
/// enormous line with no newline — can't exhaust memory.
const MAX_CLAUDE_BYTES: u64 = 64 * 1024 * 1024;

/// Which sessions to include based on working directory.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Only sessions whose `cwd` equals this directory.
    WorkingDir(PathBuf),
    /// Every session, regardless of `cwd`.
    Global,
}

impl Scope {
    fn matches(&self, cwd: &Path) -> bool {
        match self {
            Scope::Global => true,
            Scope::WorkingDir(p) => cwd == p.as_path(),
        }
    }

    /// Stable token for persisting the last-used scope.
    pub fn label(&self) -> String {
        match self {
            Scope::Global => "global".to_string(),
            Scope::WorkingDir(_) => "working_dir".to_string(),
        }
    }
}

/// Where the session stores live. Injected so tests can point at fixtures
/// without mutating process-global environment (which races under parallel test
/// runners). Binaries use [`ScanConfig::from_env`].
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub codex_dir: PathBuf,
    pub claude_dir: PathBuf,
    /// Kiro CLI session store: `~/.kiro/sessions/cli/` holding `<uuid>.json`
    /// metadata sidecars (plus `<uuid>.jsonl` conversation logs).
    pub kiro_dir: PathBuf,
}

impl ScanConfig {
    /// Default locations, overridable via `MINDPLAYER_CODEX_DIR` /
    /// `MINDPLAYER_CLAUDE_DIR` / `MINDPLAYER_KIRO_DIR` (used for manual testing).
    pub fn from_env() -> Self {
        ScanConfig {
            codex_dir: env_dir("MINDPLAYER_CODEX_DIR", &[".codex", "sessions"]),
            claude_dir: env_dir("MINDPLAYER_CLAUDE_DIR", &[".claude", "projects"]),
            kiro_dir: env_dir("MINDPLAYER_KIRO_DIR", &[".kiro", "sessions", "cli"]),
        }
    }
}

fn env_dir(var: &str, under_home: &[&str]) -> PathBuf {
    if let Ok(p) = std::env::var(var) {
        return PathBuf::from(p);
    }
    let mut base = home();
    for part in under_home {
        base.push(part);
    }
    base
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// Scan both stores and return every session in `scope`.
///
/// The returned list is the full in-scope set; aggregation (the scan-screen
/// numbers) is computed over this. The UI applies its own archived / sub-agent
/// view filters on top.
pub fn scan(scope: &Scope, cfg: &ScanConfig) -> Vec<Session> {
    // Discovery is I/O- and parse-bound and embarrassingly parallel. We first
    // gather the candidate paths (cheap), then parse them across threads.
    let codex_paths = gather_jsonl(&cfg.codex_dir);
    let claude_items = gather_claude_items(&cfg.claude_dir, scope);

    let kiro_paths = gather_kiro(&cfg.kiro_dir);

    let mut sessions = parallel_filter_map(&codex_paths, |path| parse_codex_file(path, scope));
    let claude = parallel_filter_map(&claude_items, |(path, cwd_override)| {
        parse_claude_file(path, cwd_override.as_deref())
    });
    sessions.extend(claude);
    let kiro = parallel_filter_map(&kiro_paths, |path| parse_kiro_file(path, scope));
    sessions.extend(kiro);
    // Normalize the recency key to file mtime here (off the UI thread) — the
    // SAME source the periodic refresh uses. Parsers derive last_active from the
    // in-file transcript timestamp, which can disagree with mtime for a live
    // session; mixing the two made rows flap. refresh_activity also sorts.
    refresh_activity(&mut sessions);
    sessions
}

/// True if the session was last touched (active or started) within the last
/// 24 hours. Drives the list's "recent" category — a rolling window, not a
/// calendar-day boundary, so a session from late last night still counts as
/// recent at 1am rather than instantly aging out at midnight.
pub fn touched_recently(session: &Session, now: DateTime<Utc>) -> bool {
    let Some(touched) = session.last_active.or(session.started_at) else {
        return false;
    };
    now - touched < chrono::Duration::hours(24)
}

/// Order the last 24h of work first, then newest-active, then newest-created.
pub fn sort_by_recency(sessions: &mut [Session]) {
    let now = Utc::now();
    sessions.sort_by(|a, b| {
        touched_recently(b, now)
            .cmp(&touched_recently(a, now))
            .then(b.last_active.cmp(&a.last_active))
            .then(b.started_at.cmp(&a.started_at))
    });
}

/// Cheaply refresh `last_active` from each session file's mtime and re-sort.
/// No JSON parsing — a few thousand `stat`s, used for periodic live re-ordering
/// so a session you're actively working in bubbles to the top. (File mtime
/// tracks the last appended event, which matches the in-transcript timestamp.)
pub fn refresh_activity(sessions: &mut [Session]) {
    for s in sessions.iter_mut() {
        if let Ok(modified) = std::fs::metadata(&s.file).and_then(|m| m.modified()) {
            s.last_active = Some(DateTime::<Utc>::from(modified));
        }
    }
    sort_by_recency(sessions);
}

/// Refresh mtime-based activity and token/context usage for already-discovered
/// sessions. This is meant for the live TUI: initial scans can catch a newly
/// spawned session before its first token record is written, so mtime-only
/// refreshes would otherwise leave that row stuck at `0` until a manual rescan.
pub fn refresh_activity_and_usage(sessions: &mut [Session]) {
    for s in sessions.iter_mut() {
        let old_active = s.last_active;
        let Ok(modified) = std::fs::metadata(&s.file).and_then(|m| m.modified()) else {
            continue;
        };
        let active = DateTime::<Utc>::from(modified);
        s.last_active = Some(active);

        let changed = old_active.is_none_or(|prev| active > prev);
        let missing_usage = s.tokens.total == 0 && s.agent != Agent::Kiro;
        let missing_context = s.agent == Agent::Kiro && s.context_pct.is_none();
        if !changed && !missing_usage && !missing_context {
            continue;
        }

        match s.agent {
            Agent::Codex => {
                let (tokens, _) = codex_tail_scan(&s.file);
                if tokens.total > 0 || s.tokens.total == 0 {
                    s.tokens = tokens;
                }
            }
            Agent::Claude => {
                let tokens = claude_usage_scan(&s.file);
                if tokens.total > 0 || s.tokens.total == 0 {
                    s.tokens = tokens;
                }
            }
            Agent::Kiro => {
                if let Some(context_pct) = kiro_context_pct(&s.file) {
                    s.context_pct = Some(context_pct);
                }
            }
        }
    }
    sort_by_recency(sessions);
}

/// Parse `items` into results across a thread pool sized to the machine.
/// Scoped threads borrow `f` and the slices directly — no allocation per item
/// beyond the work itself, and panics in one chunk don't abort the others.
fn parallel_filter_map<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Option<R> + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(items.len());
    let chunk = items.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().filter_map(f).collect::<Vec<R>>()))
            .collect();
        let mut out = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(part) => out.extend(part),
                // A worker chunk panicked: don't abort the whole scan, but make
                // the (otherwise invisible) data loss observable.
                Err(_) => eprintln!("mindplayer: a scan worker thread panicked; some sessions in that batch were skipped"),
            }
        }
        out
    })
}

fn gather_jsonl(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(walkdir::DirEntry::into_path)
        .filter(|p| is_jsonl(p))
        .collect()
}

/// Claude scope membership is decided by directory name (it groups sessions by
/// launch cwd), so we resolve the per-file cwd override while gathering paths.
fn gather_claude_items(root: &Path, scope: &Scope) -> Vec<(PathBuf, Option<PathBuf>)> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let want_dir = match scope {
        Scope::WorkingDir(p) => Some(encode_cwd(p)),
        Scope::Global => None,
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if let Some(want) = &want_dir {
            if dir.file_name().and_then(|n| n.to_str()) != Some(want.as_str()) {
                continue;
            }
        }
        // For a matched working-dir scope the cwd is known exactly; for global
        // we fall back to the per-message cwd, then a lossy dir-name decode.
        let cwd_override = match scope {
            Scope::WorkingDir(p) => Some(p.clone()),
            Scope::Global => None,
        };
        for file in WalkDir::new(&dir).into_iter().filter_map(Result::ok) {
            let path = file.path();
            if is_jsonl(path) {
                out.push((path.to_path_buf(), cwd_override.clone()));
            }
        }
    }
    out
}

fn is_jsonl(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

/// Kiro CLI writes one `<uuid>.json` metadata sidecar per session in a flat
/// `cli/` dir (alongside `.jsonl`/`.history`/`.lock`); the `.json` is all we
/// need to list it.
fn gather_kiro(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(walkdir::DirEntry::into_path)
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect()
}

// --- Codex ----------------------------------------------------------------

struct CodexMeta {
    id: Option<String>,
    cwd: PathBuf,
    started: Option<DateTime<Utc>>,
    /// `session_meta.thread_source`: "user" for top-level, "subagent" for spawned.
    is_subagent: bool,
}

fn parse_codex_file(path: &Path, scope: &Scope) -> Option<Session> {
    let meta = codex_meta(path);
    // Scope filter happens before we read the (potentially huge) body.
    if !scope.matches(&meta.cwd) {
        return None;
    }
    let id = meta.id.or_else(|| codex_uuid_from_filename(path))?;
    let (tokens, last_active) = codex_tail_scan(path);
    let title = codex_head_title(path).unwrap_or_else(|| "(empty)".to_string());
    let is_subagent = meta.is_subagent || looks_like_subagent_title(&title);
    Some(Session {
        id,
        agent: Agent::Codex,
        cwd: meta.cwd,
        file: path.to_path_buf(),
        started_at: meta.started,
        last_active,
        tokens,
        title,
        archived: false,
        is_subagent,
        context_pct: None,
    })
}

/// Title patterns that mark a spawned helper/agent rather than a user session:
/// agent role prompts ("You are …") and codex review-subagent passes.
fn looks_like_subagent_title(t: &str) -> bool {
    let t = t.trim_start();
    t.starts_with("You are ")
        || t.starts_with("Code review pass for")
        || t.starts_with("Security review pass for")
        || t.starts_with("Devil's advocate review")
        || t.starts_with("Documentation architecture review")
        || t.starts_with("Read-only")
        // Fan-out prompt templates from the user's own multi-agent tooling
        // (a batch of per-region/per-stack worker sessions, each titled with
        // its own target rather than the shared task) — verified against
        // real kiro sessions: a genuine top-level kiro request never opens
        // with either literal label.
        || t.starts_with("대상 스택:")
        || t.starts_with("작업 디렉토리:")
}

/// Read just the leading lines to find the `session_meta` record (id, cwd,
/// start time). `cwd` is empty if not found (so it only matches `Global`).
fn codex_meta(path: &Path) -> CodexMeta {
    let mut meta = CodexMeta {
        id: None,
        cwd: PathBuf::new(),
        started: None,
        is_subagent: false,
    };
    let Ok(file) = File::open(path) else {
        return meta;
    };
    for line in BufReader::new(file)
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
    {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            let payload = v.get("payload");
            meta.id = payload
                .and_then(|p| p.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            meta.cwd = payload
                .and_then(|p| p.get("cwd"))
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_default();
            meta.started = payload
                .and_then(|p| p.get("timestamp"))
                .and_then(Value::as_str)
                .and_then(parse_ts);
            meta.is_subagent = payload
                .and_then(|p| p.get("thread_source"))
                .and_then(Value::as_str)
                == Some("subagent");
            break;
        }
    }
    meta
}

/// Read a bounded window from EOF and take the last `token_count` (cumulative)
/// plus the last timestamp. Grows the window if no `token_count` is in range.
fn codex_tail_scan(path: &Path) -> (TokenUsage, Option<DateTime<Utc>>) {
    let mut tokens = TokenUsage::default();
    let mut last_active = None;
    let Ok(mut file) = File::open(path) else {
        return (tokens, last_active);
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return (tokens, last_active),
    };
    if len == 0 {
        return (tokens, last_active);
    }

    let mut window = INITIAL_TAIL_BYTES;
    loop {
        let start = len.saturating_sub(window);
        if file.seek(SeekFrom::Start(start)).is_err() {
            break;
        }
        let mut bytes = Vec::new();
        if file
            .by_ref()
            .take(len - start)
            .read_to_end(&mut bytes)
            .is_err()
        {
            break;
        }
        // Drop only a genuine partial fragment: when starting mid-file, skip
        // exactly up to and including the first newline (never a whole record).
        let slice: &[u8] = if start > 0 {
            match bytes.iter().position(|&b| b == b'\n') {
                Some(nl) => &bytes[nl + 1..],
                None => &[], // no newline in window => all one partial line
            }
        } else {
            &bytes
        };
        let text = String::from_utf8_lossy(slice);
        let mut found_token = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(ts) = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts)
            {
                // Records are append-ordered, but a trailing partial/garbage
                // write must never pull the time backwards.
                last_active = Some(last_active.map_or(ts, |prev: DateTime<Utc>| prev.max(ts)));
            }
            let payload = v.get("payload");
            if payload.and_then(|p| p.get("type")).and_then(Value::as_str) == Some("token_count") {
                if let Some(info) = payload
                    .and_then(|p| p.get("info"))
                    .and_then(|i| i.get("total_token_usage"))
                {
                    tokens = codex_usage(info);
                    found_token = true;
                }
            }
        }
        if found_token || start == 0 || window >= MAX_TAIL_BYTES {
            break;
        }
        window = (window * 4).min(MAX_TAIL_BYTES);
    }
    (tokens, last_active)
}

/// Read the leading lines to find the first *real* user prompt for the title.
/// Codex injects boilerplate (AGENTS.md instructions, environment context) as
/// the first user message; we skip those and use the first genuine request,
/// falling back to the boilerplate only if nothing better is found.
fn codex_head_title(path: &Path) -> Option<String> {
    let Ok(file) = File::open(path) else {
        return None;
    };
    let reader = BufReader::new(file);
    let mut fallback: Option<String> = None;
    for line in reader.lines().take(HEAD_LINES).map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = v.get("payload") else {
            continue;
        };
        if payload.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(text) = extract_codex_text(payload) else {
            continue;
        };
        let cleaned = extract_intent(&text);
        if is_boilerplate_title(&cleaned) {
            fallback.get_or_insert(cleaned);
        } else {
            return Some(cleaned);
        }
    }
    fallback
}

/// Turn a raw user message into the best one-line intent for the list.
///
/// codex's goal loop wraps the real task in `<objective>…</objective>` behind a
/// generic "Continue working toward the active thread goal" preamble; when
/// present we surface the objective itself. Otherwise we just clean the text.
pub(crate) fn extract_intent(raw: &str) -> String {
    if let Some(start) = raw.find("<objective>") {
        let after = &raw[start + "<objective>".len()..];
        let inner = after.split("</objective>").next().unwrap_or(after);
        let cleaned = clean_title(inner);
        if !cleaned.is_empty() && cleaned != "(empty)" {
            return cleaned;
        }
    }
    clean_title(raw)
}

/// Injected context / system preamble that shouldn't be used as a session title.
fn is_boilerplate_title(t: &str) -> bool {
    let t = t.trim_start();
    t.is_empty()
        || t == "(empty)"
        || t.starts_with("# AGENTS.md instructions")
        || t.starts_with("# CLAUDE.md instructions")
        || t.contains("AUTONOMY DIRECTIVE")
        || t.starts_with("Continue working toward the active")
        || t.starts_with("Caveat: The messages below")
        || t.starts_with("This session is being continued")
        || t.starts_with("<system-reminder>")
}

fn codex_usage(info: &Value) -> TokenUsage {
    let g = |k: &str| info.get(k).and_then(Value::as_u64).unwrap_or(0);
    TokenUsage {
        input: g("input_tokens"),
        output: g("output_tokens"),
        cached: g("cached_input_tokens"),
        total: g("total_tokens"),
    }
}

fn extract_codex_text(payload: &Value) -> Option<String> {
    let content = payload.get("content")?.as_array()?;
    let mut out = String::new();
    for item in content {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Reconstruct the UUID from `rollout-<iso>-<8-4-4-4-12>.jsonl` when the
/// `session_meta` line is missing/corrupt: the UUID is the last five
/// dash-separated groups of the file stem.
fn codex_uuid_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    Some(parts[parts.len() - 5..].join("-"))
}

// --- Claude ---------------------------------------------------------------

fn parse_claude_file(path: &Path, cwd_override: Option<&Path>) -> Option<Session> {
    let reader = BufReader::new(File::open(path).ok()?.take(MAX_CLAUDE_BYTES));
    let mut id: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut last_active: Option<DateTime<Utc>> = None;
    let mut tokens = TokenUsage::default();
    let mut title: Option<String> = None;
    let mut title_fallback: Option<String> = None;
    let mut is_sidechain = false;

    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(ts) = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
        {
            if started_at.is_none() {
                started_at = Some(ts);
            }
            last_active = Some(ts);
        }
        if id.is_none() {
            id = v
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        }
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            is_sidechain = true;
        }
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "user" => {
                if title.is_none() {
                    if let Some(text) = v.get("message").and_then(extract_claude_text) {
                        let cleaned = extract_intent(&text);
                        // Skip injected preamble (system reminders, AGENTS/CLAUDE
                        // boilerplate); keep the first genuine request.
                        if is_boilerplate_title(&cleaned) {
                            title_fallback.get_or_insert(cleaned);
                        } else {
                            title = Some(cleaned);
                        }
                    }
                }
            }
            "assistant" => {
                if let Some(usage) = v.get("message").and_then(|m| m.get("usage")) {
                    add_claude_usage(&mut tokens, usage);
                }
            }
            _ => {}
        }
    }

    // Prefer the session's OWN recorded cwd. `encode_cwd` maps both '/' and '.'
    // to '-', so the Claude project directory name can collide across different
    // real cwds; trusting the dir-name-derived scope here could relabel a
    // session and make `resume` launch in the wrong directory. The scope dir
    // and the lossy dir-name decode are fallbacks only when the transcript has
    // no cwd of its own.
    let cwd = cwd
        .or_else(|| cwd_override.map(Path::to_path_buf))
        .unwrap_or_else(|| decode_claude_cwd(path.parent()));
    let title = title
        .or(title_fallback)
        .unwrap_or_else(|| "(empty)".to_string());
    let is_subagent = is_sidechain || looks_like_subagent_title(&title);
    let stem_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string);
    // A spawned subagent's transcript (under `<session>/subagents/*.jsonl`)
    // records its PARENT conversation's `sessionId` on every line, not one of
    // its own — trusting that field here would collide every subagent with its
    // parent (and with each other) under the same id. That corrupts anything
    // keyed by id, e.g. the periodic mtime refresh: merging per-id updates
    // could silently stamp the parent's fresh row with a subagent's stale one.
    // The file stem (its own unique filename) is what's actually unique.
    let id = if is_subagent {
        stem_id.or(id)
    } else {
        // Claude file stem IS the sessionId; trust it over a possibly-missing
        // field for the main transcript.
        id.or(stem_id)
    }?;
    Some(Session {
        id,
        agent: Agent::Claude,
        cwd,
        file: path.to_path_buf(),
        started_at,
        last_active,
        tokens,
        title,
        archived: false,
        is_subagent,
        context_pct: None,
    })
}

fn extract_claude_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut out = String::new();
        for item in arr {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
        return (!out.is_empty()).then_some(out);
    }
    None
}

fn add_claude_usage(tokens: &mut TokenUsage, usage: &Value) {
    let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    let input = g("input_tokens");
    let output = g("output_tokens");
    let cache_creation = g("cache_creation_input_tokens");
    let cache_read = g("cache_read_input_tokens");
    // saturating: untrusted files could declare absurd counts; never panic/wrap.
    tokens.input = tokens.input.saturating_add(input);
    tokens.output = tokens.output.saturating_add(output);
    tokens.cached = tokens
        .cached
        .saturating_add(cache_creation)
        .saturating_add(cache_read);
    tokens.total = tokens
        .total
        .saturating_add(input)
        .saturating_add(output)
        .saturating_add(cache_creation)
        .saturating_add(cache_read);
}

fn claude_usage_scan(path: &Path) -> TokenUsage {
    let Ok(file) = File::open(path) else {
        return TokenUsage::default();
    };
    let reader = BufReader::new(file.take(MAX_CLAUDE_BYTES));
    let mut tokens = TokenUsage::default();
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(usage) = v.get("message").and_then(|m| m.get("usage")) {
            add_claude_usage(&mut tokens, usage);
        }
    }
    tokens
}

fn kiro_context_pct(path: &Path) -> Option<f64> {
    let mut buf = String::new();
    File::open(path)
        .ok()?
        .take(256 * 1024)
        .read_to_string(&mut buf)
        .ok()?;
    let v: Value = serde_json::from_str(&buf).ok()?;
    v.get("session_state")
        .and_then(|s| s.get("rts_model_state"))
        .and_then(|r| r.get("context_usage_percentage"))
        .and_then(Value::as_f64)
}

/// Claude's directory-name encoding of a cwd: `/` and `.` become `-`
/// (e.g. `/Users/jane.doe/proj` -> `-Users-jane-doe-proj`).
fn encode_cwd(p: &Path) -> String {
    p.to_string_lossy().replace(['/', '.'], "-")
}

/// Best-effort inverse of [`encode_cwd`] for when no `cwd` field is present.
/// Lossy (every `-` becomes `/`), used only as a display fallback.
fn decode_claude_cwd(dir: Option<&Path>) -> PathBuf {
    let name = dir
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("");
    PathBuf::from(name.replace('-', "/"))
}

// --- Kiro -----------------------------------------------------------------

/// Parse a Kiro CLI session from its `<uuid>.json` metadata sidecar. The sidecar
/// carries everything the list needs (id, cwd, timestamps, title, and whether it
/// was a spawned sub-agent). Token usage is not read: the `.jsonl` conversation
/// log's schema isn't verified here, so tokens stay zero rather than guessed.
fn parse_kiro_file(path: &Path, scope: &Scope) -> Option<Session> {
    // The sidecar is tiny (~1 KB); cap the read so a pathological/hostile file
    // can't exhaust memory. A truncated read just fails to parse and is skipped.
    let mut buf = String::new();
    File::open(path)
        .ok()?
        .take(256 * 1024)
        .read_to_string(&mut buf)
        .ok()?;
    let v: Value = serde_json::from_str(&buf).ok()?;

    let cwd = v
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    if !scope.matches(&cwd) {
        return None;
    }
    // id: prefer the recorded session_id, fall back to the filename stem.
    let id = v
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })?;

    let started_at = v
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_ts);
    let last_active = v
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .or(started_at);

    let title = v
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .map(clean_title)
        .unwrap_or_else(|| "(kiro session)".to_string());

    // NOTE: kiro-cli writes `session_created_reason: "subagent"` even for normal
    // interactive `kiro-cli chat` sessions (verified against real session
    // files: every sampled session — including plainly top-level, casually
    // typed ones — carries this same value), so that field is NOT a reliable
    // sub-agent signal here. Fall back to the same title heuristic used for
    // codex/claude instead — verified against a real fan-out (one kiro
    // session per AWS region from an eks-module-upgrade task) whose titles
    // all matched `looks_like_subagent_title`, with zero false positives
    // across every other sampled kiro session title.
    let is_subagent = looks_like_subagent_title(&title);

    // Kiro records no cumulative token counts (verified: the .jsonl has no token
    // fields and input/output_token_count stay 0), but it does report the
    // current context-window occupancy. Surface that instead of a token total.
    let context_pct = v
        .get("session_state")
        .and_then(|s| s.get("rts_model_state"))
        .and_then(|r| r.get("context_usage_percentage"))
        .and_then(Value::as_f64);

    Some(Session {
        id,
        agent: Agent::Kiro,
        cwd,
        // Point at the sidecar: it is rewritten each turn, so its mtime tracks
        // activity for the periodic mtime-based re-sort.
        file: path.to_path_buf(),
        started_at,
        last_active,
        tokens: TokenUsage::default(),
        title,
        archived: false,
        is_subagent,
        context_pct,
    })
}

// --- shared ---------------------------------------------------------------

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Strip `<...>` tags, neutralize control/escape chars, collapse whitespace,
/// truncate to 60 chars. Titles come from untrusted transcript content, so any
/// control byte (ESC, CR, …) is replaced with a space before it can reach a
/// renderer — defense-in-depth against terminal escape-sequence injection,
/// independent of the ratatui / webview layers that also filter.
fn clean_title(raw: &str) -> String {
    let mut s = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            // Replace (don't drop) control chars with a space so escape
            // sequences can't form and adjacent words don't silently merge.
            _ if !in_tag && ch.is_control() => s.push(' '),
            _ if !in_tag => s.push(ch),
            _ => {}
        }
    }
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = collapsed.trim().chars().take(60).collect();
    if title.is_empty() {
        "(empty)".to_string()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_usage_reads_all_fields() {
        let info = serde_json::json!({
            "input_tokens": 1044194,
            "cached_input_tokens": 965120,
            "output_tokens": 10017,
            "total_tokens": 1054211
        });
        let u = codex_usage(&info);
        assert_eq!(u.input, 1044194);
        assert_eq!(u.cached, 965120);
        assert_eq!(u.output, 10017);
        assert_eq!(u.total, 1054211);
    }

    #[test]
    fn claude_usage_sums_all_components() {
        let mut t = TokenUsage::default();
        let usage = serde_json::json!({
            "input_tokens": 2,
            "output_tokens": 1080,
            "cache_creation_input_tokens": 2209,
            "cache_read_input_tokens": 42463
        });
        add_claude_usage(&mut t, &usage);
        add_claude_usage(&mut t, &usage);
        assert_eq!(t.input, 4);
        assert_eq!(t.output, 2160);
        assert_eq!(t.cached, (2209 + 42463) * 2);
        assert_eq!(t.total, (2 + 1080 + 2209 + 42463) * 2);
    }

    #[test]
    fn uuid_recovered_from_codex_filename() {
        let p = Path::new("rollout-2026-01-02T03-04-05-11111111-2222-7333-8444-555566667777.jsonl");
        assert_eq!(
            codex_uuid_from_filename(p).as_deref(),
            Some("11111111-2222-7333-8444-555566667777")
        );
    }

    #[test]
    fn clean_title_strips_command_tags_and_truncates() {
        let raw = "<command-name>/x</command-name>\n  hello   world  ";
        assert_eq!(clean_title(raw), "/x hello world");
        let long = "a".repeat(100);
        assert_eq!(clean_title(&long).chars().count(), 60);
        assert_eq!(clean_title("   "), "(empty)");
    }

    #[test]
    fn clean_title_neutralizes_escape_sequences() {
        // A malicious transcript title with a raw ANSI escape must not survive
        // into the rendered title; control bytes become spaces (and collapse).
        let raw = "ev\x1b[31mil\x07 \x1b]0;pwn\x07title";
        let out = clean_title(raw);
        assert!(!out.contains('\x1b'), "ESC stripped: {out:?}");
        assert!(!out.contains('\x07'), "BEL stripped: {out:?}");
        assert!(out.chars().all(|c| !c.is_control()));
        // Control chars become spaces, so adjacent words don't merge.
        assert_eq!(out, "ev [31mil ]0;pwn title");
    }

    #[test]
    fn claude_text_handles_string_and_array() {
        let s = serde_json::json!({"content": "hi there"});
        assert_eq!(extract_claude_text(&s).as_deref(), Some("hi there"));
        let a = serde_json::json!({"content": [
            {"type": "text", "text": "foo "},
            {"type": "tool_use", "name": "x"},
            {"type": "text", "text": "bar"}
        ]});
        assert_eq!(extract_claude_text(&a).as_deref(), Some("foo bar"));
    }

    #[test]
    fn encode_cwd_matches_claude_dirname() {
        assert_eq!(
            encode_cwd(Path::new("/Users/jane.doe/Work/my-project")),
            "-Users-jane-doe-Work-my-project"
        );
    }

    #[test]
    fn objective_is_extracted_from_goal_wrapper() {
        let raw = r#"<codex_internal_context source="goal"> Continue working toward the active thread goal. <objective> add a dark mode toggle to settings </objective> </codex_internal_context>"#;
        assert_eq!(extract_intent(raw), "add a dark mode toggle to settings");
        // No objective => just cleaned text.
        assert_eq!(extract_intent("just do X"), "just do X");
    }

    #[test]
    fn subagent_titles_detected() {
        assert!(looks_like_subagent_title(
            "You are TEAM WORKER \"worker-04\""
        ));
        assert!(looks_like_subagent_title("Code review pass for /repo/x"));
        assert!(looks_like_subagent_title("Security review pass for /x"));
        assert!(!looks_like_subagent_title("fix the failing test"));
        assert!(!looks_like_subagent_title("You said hello")); // not "You are "
                                                               // Real per-region/per-stack fan-out worker titles from the user's
                                                               // own multi-agent tooling (verified against an actual eks-module
                                                               // upgrade fan-out, one kiro session per AWS region).
        assert!(looks_like_subagent_title(
            "대상 스택: /repo/data-nest/aws/data-prod/cac1/regional/airflow"
        ));
        assert!(looks_like_subagent_title("작업 디렉토리: /repo/soda-k8s"));
        assert!(looks_like_subagent_title(
            "Read-only investigation. In /repo, search for X"
        ));
        assert!(looks_like_subagent_title(
            "Read-only. Read the file /repo/index.html"
        ));
        // Genuine casual top-level requests (Korean included) must not match.
        assert!(!looks_like_subagent_title("kiro mcp없나?"));
        assert!(!looks_like_subagent_title(
            "local disk full 나고 있대 삭제할 만한거 찾아봐줘"
        ));
    }

    #[test]
    fn scope_matches_working_dir_exactly() {
        let scope = Scope::WorkingDir(PathBuf::from("/a/b"));
        assert!(scope.matches(Path::new("/a/b")));
        assert!(!scope.matches(Path::new("/a/b/c")));
        assert!(Scope::Global.matches(Path::new("/anything")));
    }

    #[test]
    fn sort_by_recency_promotes_sessions_touched_within_24h() {
        let now = Utc::now();
        let old = Session {
            id: "old".into(),
            agent: Agent::Codex,
            cwd: PathBuf::new(),
            file: PathBuf::new(),
            started_at: Some(now - chrono::Duration::days(2)),
            // Just past the 24h window — must sort below every recent session
            // regardless of calendar day (no midnight cliff).
            last_active: Some(now - chrono::Duration::hours(25)),
            tokens: TokenUsage::default(),
            title: "old".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        let mut recent_older = old.clone();
        recent_older.id = "recent-older".into();
        // Just inside the 24h window (e.g. late last night) — still "recent".
        recent_older.last_active = Some(now - chrono::Duration::hours(23));
        let mut recent_newer = recent_older.clone();
        recent_newer.id = "recent-newer".into();
        recent_newer.last_active = Some(now - chrono::Duration::hours(1));

        let mut sessions = vec![old, recent_older, recent_newer];
        sort_by_recency(&mut sessions);

        let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["recent-newer", "recent-older", "old"]);
    }

    #[test]
    fn touched_recently_uses_a_rolling_window_not_a_calendar_day() {
        let now = Utc::now();
        let mut s = Session {
            id: "s".into(),
            agent: Agent::Codex,
            cwd: PathBuf::new(),
            file: PathBuf::new(),
            started_at: None,
            last_active: Some(now - chrono::Duration::hours(23)),
            tokens: TokenUsage::default(),
            title: String::new(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        };
        assert!(
            touched_recently(&s, now),
            "23h ago is within the rolling 24h window, even if it crossed midnight"
        );
        s.last_active = Some(now - chrono::Duration::hours(25));
        assert!(
            !touched_recently(&s, now),
            "25h ago is outside the window, even if it's still 'today' by calendar date"
        );
    }
}
