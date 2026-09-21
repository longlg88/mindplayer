//! Experimental selected-session observer.
//!
//! This module is intentionally standalone: UI code owns when it is enabled,
//! while this file owns local JSONL parsing and bounded background reads. It
//! only reads the selected session's local transcript file.

use mindplayer_core::{Agent, Session};
use serde_json::Value;
use std::collections::VecDeque;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const MAX_EVENTS: usize = 100;
const MAX_USAGE_LINES: usize = 5;
const READ_BUDGET_BYTES: u64 = 64 * 1024;
const INITIAL_BACKFILL_BYTES: u64 = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_TEXT_CHARS: usize = 360;
const PROMPT_USAGE_PREFIX: &str = "__mindplayer_prompt_usage ";

/// Live local observer for the currently selected session.
///
/// The public fields are deliberately plain so the leader UI can render them
/// without coupling to the worker internals.
#[derive(Default)]
pub struct Observer {
    pub enabled: bool,
    pub lines: Vec<String>,
    pub usage_lines: Vec<String>,
    pub notice: String,

    current: Option<SourceKey>,
    reader: Option<JsonlFollower>,
    in_flight: Option<Receiver<WorkerResult>>,
    last_poll: Option<Instant>,
    events: VecDeque<String>,
}

impl Observer {
    /// Number of prompt groups available to the trace UI. Events before the
    /// first retained user prompt form one explicit partial group so bounded
    /// tail reads never make observed work disappear.
    pub fn turn_count(&self) -> usize {
        let prompts = self
            .lines
            .iter()
            .filter(|line| line.starts_with("user: "))
            .count();
        if prompts == 0 {
            usize::from(!self.lines.is_empty())
        } else if self
            .lines
            .first()
            .is_some_and(|line| !line.starts_with("user: "))
        {
            prompts + 1
        } else {
            prompts
        }
    }

    /// Poll the selected session without blocking the UI thread.
    ///
    /// Returns `true` when one of the public fields changed. The observer is
    /// isolated by provider, session id, and source path. Background work is
    /// one-shot: each worker performs one bounded read and exits, so inactive
    /// panes cannot accumulate sleeping readers.
    pub fn poll(&mut self, session: &Session) -> bool {
        if !self.enabled {
            return false;
        }

        let key = SourceKey::from_session(session);
        if self.current.as_ref() != Some(&key) {
            self.current = Some(key.clone());
            self.in_flight = None;
            self.reader = None;
            self.last_poll = None;
            self.events.clear();
            self.lines.clear();
            self.usage_lines.clear();
            self.notice.clear();

            match session.agent {
                Agent::Codex | Agent::Claude => {
                    let notice = coverage_notice(session);
                    self.reader = Some(JsonlFollower::new(
                        key.agent,
                        key.file.clone(),
                        INITIAL_BACKFILL_BYTES,
                    ));
                    self.notice = notice;
                    self.spawn_read();
                }
                Agent::Kiro => {
                    self.notice =
                        "Kiro transcript observer unsupported; local schema is not verified"
                            .to_string();
                    self.lines
                        .push("unsupported: Kiro transcript events unavailable".to_string());
                    if let Some(pct) = session.context_pct {
                        self.usage_lines
                            .push(format!("Kiro context: {:.1}%", pct.clamp(0.0, 100.0)));
                    }
                }
                Agent::Cursor => {
                    self.notice =
                        "Cursor observer unsupported: verified local store exposes metadata only"
                            .to_string();
                    self.lines.push(
                        "unsupported: Cursor transcript/usage unavailable locally".to_string(),
                    );
                }
            }
            return true;
        }

        let mut changed = false;
        if let Some(rx) = self.in_flight.take() {
            match rx.try_recv() {
                Ok(result) => {
                    changed |= self.apply_outcome(result.outcome);
                    self.reader = Some(result.reader);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.in_flight = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }

        if changed {
            self.lines = self.events.iter().cloned().collect();
        }
        self.spawn_read();
        changed
    }

    fn apply_outcome(&mut self, outcome: ReadOutcome) -> bool {
        match outcome {
            ReadOutcome::Changed {
                events,
                usage,
                notice,
            } => {
                for line in events {
                    push_observed_event(&mut self.events, line);
                }
                if !usage.is_empty() {
                    self.usage_lines = usage.into_iter().take(MAX_USAGE_LINES).collect();
                }
                if let Some(notice) = notice {
                    self.notice = merge_notice(&self.notice, &notice);
                }
                true
            }
            ReadOutcome::Unchanged => false,
            ReadOutcome::Error(e) => {
                self.notice = merge_notice(&self.notice, &e);
                true
            }
        }
    }

    fn spawn_read(&mut self) {
        if self.in_flight.is_some() {
            return;
        }
        if self
            .last_poll
            .is_some_and(|last| last.elapsed() < POLL_INTERVAL)
        {
            return;
        }
        let Some(mut reader) = self.reader.take() else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(1);
        self.last_poll = Some(Instant::now());
        thread::spawn(move || {
            let outcome = reader.read_once();
            let _ = tx.try_send(WorkerResult { reader, outcome });
        });
        self.in_flight = Some(rx);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceKey {
    agent: Agent,
    session_id: String,
    file: PathBuf,
}

impl SourceKey {
    fn from_session(session: &Session) -> Self {
        Self {
            agent: session.agent,
            session_id: session.id.clone(),
            file: session.file.clone(),
        }
    }
}

struct WorkerResult {
    reader: JsonlFollower,
    outcome: ReadOutcome,
}

fn push_bounded(items: &mut VecDeque<String>, item: String, max: usize) {
    items.push_back(item);
    while items.len() > max {
        items.pop_front();
    }
}

fn push_observed_event(items: &mut VecDeque<String>, item: String) {
    let Some(delta) = prompt_usage_from_line(&item) else {
        push_bounded(items, item, MAX_EVENTS);
        return;
    };
    for index in (0..items.len()).rev() {
        if items[index].starts_with("user: ") {
            break;
        }
        if let Some(current) = prompt_usage_from_line(&items[index]) {
            items[index] = prompt_usage_line(current.saturating_add(delta));
            return;
        }
    }
    push_bounded(items, item, MAX_EVENTS);
}

fn prompt_usage_line(usage: Usage) -> String {
    format!(
        "{PROMPT_USAGE_PREFIX}total={} input={} cached={} output={} reasoning={}",
        usage.total, usage.input, usage.cached, usage.output, usage.reasoning
    )
}

pub(crate) fn prompt_usage_from_line(line: &str) -> Option<Usage> {
    let fields = line.strip_prefix(PROMPT_USAGE_PREFIX)?;
    let mut usage = Usage::default();
    for field in fields.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        let value = value.parse::<u64>().ok()?;
        match key {
            "total" => usage.total = value,
            "input" => usage.input = value,
            "cached" => usage.cached = value,
            "output" => usage.output = value,
            "reasoning" => usage.reasoning = value,
            _ => return None,
        }
    }
    Some(usage)
}

fn coverage_notice(session: &Session) -> String {
    let scope = if session.is_subagent {
        "subagent selected; parent and sibling subagents excluded"
    } else {
        "selected file only; unrecorded startup and subagents excluded"
    };
    format!(
        "observing {} locally ({scope}); bounded tail + append follow",
        session.agent.as_str()
    )
}

fn merge_notice(base: &str, extra: &str) -> String {
    if base.is_empty() || base == extra {
        extra.to_string()
    } else if base.contains(extra) {
        base.to_string()
    } else {
        format!("{base}; {extra}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceIdentity {
    len: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl SourceIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                len: metadata.len(),
                dev: metadata.dev(),
                ino: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                len: metadata.len(),
            }
        }
    }

    fn same_source(self, other: Self) -> bool {
        #[cfg(unix)]
        {
            self.dev == other.dev && self.ino == other.ino
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

struct JsonlFollower {
    agent: Agent,
    file: PathBuf,
    offset: u64,
    skipped_head: bool,
    partial_scope: bool,
    drop_first_fragment: bool,
    claude_observed: Usage,
    codex_cumulative: Option<Usage>,
    identity: Option<SourceIdentity>,
}

impl JsonlFollower {
    fn new(agent: Agent, file: PathBuf, initial_backfill: u64) -> Self {
        let identity = std::fs::metadata(&file)
            .ok()
            .map(|m| SourceIdentity::from_metadata(&m));
        let len = identity.map(|i| i.len).unwrap_or(0);
        let offset = len.saturating_sub(initial_backfill);
        Self {
            agent,
            file,
            offset,
            skipped_head: offset > 0,
            partial_scope: offset > 0,
            drop_first_fragment: offset > 0,
            claude_observed: Usage::default(),
            codex_cumulative: None,
            identity,
        }
    }

    #[cfg(test)]
    fn from_start(agent: Agent, file: PathBuf) -> Self {
        Self {
            agent,
            file,
            offset: 0,
            skipped_head: false,
            partial_scope: false,
            drop_first_fragment: false,
            claude_observed: Usage::default(),
            codex_cumulative: None,
            identity: None,
        }
    }

    fn read_once(&mut self) -> ReadOutcome {
        let mut file = match File::open(&self.file) {
            Ok(file) => file,
            Err(e) => {
                return ReadOutcome::Error(format!(
                    "observer: cannot read {}: {e}",
                    self.file.display()
                ))
            }
        };
        let identity = match file.metadata() {
            Ok(m) => SourceIdentity::from_metadata(&m),
            Err(e) => {
                return ReadOutcome::Error(format!(
                    "observer: cannot stat {}: {e}",
                    self.file.display()
                ))
            }
        };
        let len = identity.len;

        let mut notice = None;
        let source_changed = self
            .identity
            .is_some_and(|previous| !previous.same_source(identity));
        if source_changed || len < self.offset {
            self.offset = 0;
            self.claude_observed = Usage::default();
            self.codex_cumulative = None;
            self.drop_first_fragment = false;
            self.partial_scope = false;
            notice = Some(if source_changed {
                "observer: source file replaced; restarted at beginning".to_string()
            } else {
                "observer: source truncated; restarted at beginning".to_string()
            });
        }
        self.identity = Some(identity);
        if len == self.offset {
            return match notice {
                Some(notice) => ReadOutcome::Changed {
                    events: Vec::new(),
                    usage: Vec::new(),
                    notice: Some(notice),
                },
                None => ReadOutcome::Unchanged,
            };
        }

        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return ReadOutcome::Error(format!("observer: cannot seek {}", self.file.display()));
        }
        let to_read = (len - self.offset).min(READ_BUDGET_BYTES);
        let mut bytes = Vec::with_capacity(to_read as usize);
        if let Err(e) = file.by_ref().take(to_read).read_to_end(&mut bytes) {
            return ReadOutcome::Error(format!(
                "observer: cannot read {}: {e}",
                self.file.display()
            ));
        }

        if bytes.is_empty() {
            return ReadOutcome::Unchanged;
        }

        let Some(last_newline) = bytes.iter().rposition(|b| *b == b'\n') else {
            if to_read == READ_BUDGET_BYTES && self.offset.saturating_add(to_read) < len {
                self.offset = self.offset.saturating_add(to_read);
                self.drop_first_fragment = true;
                self.partial_scope = true;
                return ReadOutcome::Changed {
                    events: Vec::new(),
                    usage: Vec::new(),
                    notice: Some("observer: skipped oversized JSONL record".to_string()),
                };
            }
            return ReadOutcome::Unchanged;
        };
        let complete = if self.drop_first_fragment {
            self.drop_first_fragment = false;
            match bytes[..last_newline].iter().position(|b| *b == b'\n') {
                Some(first_newline) => &bytes[first_newline + 1..last_newline],
                None => &[],
            }
        } else {
            &bytes[..last_newline]
        };
        self.offset = self.offset.saturating_add(last_newline as u64 + 1);

        let mut events = Vec::new();
        let mut usage = Vec::new();
        let text = String::from_utf8_lossy(complete);
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let parsed = match self.agent {
                Agent::Codex => parse_codex(&v),
                Agent::Claude => parse_claude(&v, &mut self.claude_observed, self.partial_scope),
                Agent::Kiro | Agent::Cursor => Parsed::default(),
            };
            events.extend(parsed.events);
            if let Some(delta) = self.prompt_usage_delta(parsed.prompt_usage) {
                events.push(prompt_usage_line(delta));
            }
            if !parsed.usage.is_empty() {
                usage = parsed.usage;
            }
        }
        if self.skipped_head {
            let skipped = format!(
                "observer: bounded tail of {}; older events omitted",
                self.file.display()
            );
            notice = Some(notice.map_or(skipped.clone(), |n| format!("{n}; {skipped}")));
            self.skipped_head = false;
        }

        if events.is_empty() && usage.is_empty() && notice.is_none() {
            ReadOutcome::Unchanged
        } else {
            ReadOutcome::Changed {
                events,
                usage,
                notice,
            }
        }
    }

    fn prompt_usage_delta(&mut self, usage: Option<UsageRecord>) -> Option<Usage> {
        match usage? {
            UsageRecord::Delta(delta) => (delta.total > 0).then_some(delta),
            UsageRecord::Codex { cumulative, last } => {
                let delta = match self.codex_cumulative {
                    Some(previous) if previous == cumulative => None,
                    Some(previous) => cumulative.checked_delta(previous).or(last),
                    None => last,
                };
                self.codex_cumulative = Some(cumulative);
                delta.filter(|delta| delta.total > 0)
            }
        }
    }
}

enum ReadOutcome {
    Changed {
        events: Vec<String>,
        usage: Vec<String>,
        notice: Option<String>,
    },
    Unchanged,
    Error(String),
}

#[derive(Default)]
struct Parsed {
    events: Vec<String>,
    usage: Vec<String>,
    prompt_usage: Option<UsageRecord>,
}

#[derive(Debug, Clone, Copy)]
enum UsageRecord {
    Codex {
        cumulative: Usage,
        last: Option<Usage>,
    },
    Delta(Usage),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) input: u64,
    cache_create: u64,
    cache_read: u64,
    pub(crate) cached: u64,
    pub(crate) output: u64,
    pub(crate) reasoning: u64,
    pub(crate) total: u64,
}

impl Usage {
    fn claude_record(usage: &Value) -> Self {
        let input = u64_field(usage, "input_tokens");
        let cache_create = u64_field(usage, "cache_creation_input_tokens");
        let cache_read = u64_field(usage, "cache_read_input_tokens");
        let output = u64_field(usage, "output_tokens");
        let cached = cache_create.saturating_add(cache_read);
        let total = input
            .saturating_add(cache_create)
            .saturating_add(cache_read)
            .saturating_add(output);
        Self {
            input,
            cache_create,
            cache_read,
            cached,
            output,
            reasoning: 0,
            total,
        }
    }

    fn codex_record(usage: &Value) -> Self {
        Self {
            input: u64_field(usage, "input_tokens"),
            cached: u64_field(usage, "cached_input_tokens"),
            output: u64_field(usage, "output_tokens"),
            reasoning: u64_field(usage, "reasoning_output_tokens"),
            total: u64_field(usage, "total_tokens"),
            ..Self::default()
        }
    }

    fn checked_delta(self, previous: Self) -> Option<Self> {
        Some(Self {
            input: self.input.checked_sub(previous.input)?,
            cache_create: self.cache_create.checked_sub(previous.cache_create)?,
            cache_read: self.cache_read.checked_sub(previous.cache_read)?,
            cached: self.cached.checked_sub(previous.cached)?,
            output: self.output.checked_sub(previous.output)?,
            reasoning: self.reasoning.checked_sub(previous.reasoning)?,
            total: self.total.checked_sub(previous.total)?,
        })
    }

    fn saturating_add(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            cache_create: self.cache_create.saturating_add(other.cache_create),
            cache_read: self.cache_read.saturating_add(other.cache_read),
            cached: self.cached.saturating_add(other.cached),
            output: self.output.saturating_add(other.output),
            reasoning: self.reasoning.saturating_add(other.reasoning),
            total: self.total.saturating_add(other.total),
        }
    }
}

fn parse_codex(v: &Value) -> Parsed {
    let mut parsed = Parsed::default();
    let payload = v.get("payload").unwrap_or(v);
    match v.get("type").and_then(Value::as_str) {
        Some("response_item") => parse_codex_response_item(payload, &mut parsed),
        Some("event_msg") => parse_codex_event_msg(payload, &mut parsed),
        _ => {}
    }
    parsed
}

fn parse_codex_response_item(payload: &Value, parsed: &mut Parsed) {
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let role = payload
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("message");
            if !matches!(role, "user" | "assistant") {
                return;
            }
            if let Some(text) = content_text(payload.get("content")) {
                parsed
                    .events
                    .push(format!("{role}: {}", sanitize_text(&text)));
            }
        }
        "function_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("function");
            let args = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .map(compact_value)
                .unwrap_or_default();
            parsed
                .events
                .push(format!("function_call {name}: {}", sanitize_text(&args)));
        }
        "function_call_output" => {
            let out = payload
                .get("output")
                .or_else(|| payload.get("content"))
                .map(compact_value)
                .unwrap_or_default();
            parsed
                .events
                .push(format!("function_call_output: {}", sanitize_text(&out)));
        }
        "custom_tool_call" => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("custom_tool");
            let input = payload
                .get("input")
                .or_else(|| payload.get("arguments"))
                .map(compact_value)
                .unwrap_or_default();
            parsed.events.push(format!(
                "custom_tool_call {name}: {}",
                sanitize_text(&input)
            ));
        }
        "custom_tool_call_output" | "custom_tool_output" => {
            let out = payload
                .get("output")
                .or_else(|| payload.get("content"))
                .map(compact_value)
                .unwrap_or_default();
            parsed
                .events
                .push(format!("custom_tool_output: {}", sanitize_text(&out)));
        }
        // Do not surface hidden reasoning/scratchpad records.
        "reasoning" | "reasoning_item" | "thinking" => {}
        _ => {}
    }
}

fn parse_codex_event_msg(payload: &Value, parsed: &mut Parsed) {
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return;
    }
    let Some(info) = payload.get("info") else {
        return;
    };
    let Some(total_usage) = info.get("total_token_usage") else {
        return;
    };
    let cumulative = Usage::codex_record(total_usage);
    parsed.usage = vec![
        "Codex usage: session cumulative".to_string(),
        format!("total {}", cumulative.total),
        format!("input {}", cumulative.input),
        format!("cache {}", cumulative.cached),
        format!("output {}", cumulative.output),
    ];
    let last = info.get("last_token_usage").map(Usage::codex_record);
    if let Some(last) = last {
        let last_total = last.total;
        if last_total > 0 {
            parsed.usage[4] = format!("output {} | last {last_total}", cumulative.output);
        }
    }
    parsed.prompt_usage = Some(UsageRecord::Codex { cumulative, last });
}

fn parse_claude(v: &Value, observed: &mut Usage, partial_scope: bool) -> Parsed {
    let mut parsed = Parsed::default();
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "assistant" => {
            let message = v.get("message").unwrap_or(v);
            parse_claude_content(message.get("content"), "assistant", &mut parsed);
            if let Some(usage) = message.get("usage") {
                *observed = Usage::claude_record(usage);
                parsed.prompt_usage = Some(UsageRecord::Delta(*observed));
                let scope = if partial_scope {
                    "latest observed message"
                } else {
                    "latest message"
                };
                parsed.usage = vec![
                    format!("Claude usage: {scope}"),
                    format!("total {}", observed.total),
                    format!("input {}", observed.input),
                    format!("cache c{} r{}", observed.cache_create, observed.cache_read),
                    format!("output {}", observed.output),
                ];
            }
        }
        "user" => {
            let message = v.get("message").unwrap_or(v);
            parse_claude_content(message.get("content"), "user", &mut parsed);
        }
        _ => {}
    }
    parsed
}

fn parse_claude_content(content: Option<&Value>, role: &str, parsed: &mut Parsed) {
    let Some(content) = content else {
        return;
    };
    match content {
        Value::String(s) => {
            parsed.events.push(format!("{role}: {}", sanitize_text(s)));
        }
        Value::Array(items) => {
            for item in items {
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            parsed
                                .events
                                .push(format!("{role}: {}", sanitize_text(text)));
                        }
                    }
                    "tool_use" => {
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = item.get("input").map(compact_value).unwrap_or_default();
                        parsed
                            .events
                            .push(format!("tool_use {name}: {}", sanitize_text(&input)));
                    }
                    "tool_result" => {
                        let text = item
                            .get("content")
                            .map(compact_value)
                            .unwrap_or_else(|| compact_value(item));
                        parsed
                            .events
                            .push(format!("tool_result: {}", sanitize_text(&text)));
                    }
                    // Do not show hidden reasoning/thinking payloads.
                    "thinking" | "redacted_thinking" => {}
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if hidden_content(item) {
                    continue;
                }
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    append_text(&mut out, text);
                } else if let Some(text) = item.get("content").and_then(|v| content_text(Some(v))) {
                    append_text(&mut out, &text);
                }
            }
            (!out.is_empty()).then_some(out)
        }
        Value::Object(_) => content?
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| content?.get("content").and_then(|v| content_text(Some(v)))),
        _ => None,
    }
}

fn hidden_content(v: &Value) -> bool {
    matches!(
        v.get("type").and_then(Value::as_str).unwrap_or(""),
        "reasoning" | "reasoning_text" | "thinking" | "redacted_thinking"
    )
}

fn append_text(out: &mut String, text: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
}

fn compact_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn sanitize_text(raw: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    let mut skip_ansi = false;
    for ch in raw.chars() {
        if skip_ansi {
            if ch.is_ascii_alphabetic() {
                skip_ansi = false;
            }
            continue;
        }
        if ch == '\u{1b}' {
            skip_ansi = true;
            continue;
        }
        let next = match ch {
            '\n' | '\r' | '\t' => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        if next.is_whitespace() {
            if last_space {
                continue;
            }
            last_space = true;
            out.push(' ');
        } else {
            last_space = false;
            out.push(next);
        }
        if out.chars().count() >= MAX_TEXT_CHARS {
            out.push_str("...");
            break;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mindplayer_core::TokenUsage;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_codex_public_events_and_native_usage_without_reasoning() {
        let records = [
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "visible answer"},
                    {"type": "reasoning_text", "text": "hidden chain"}
                ]}
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "function_call", "name": "shell", "arguments": "{\"cmd\":\"date\"}"}
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {"type": "function_call_output", "output": "Mon\u{1b}[31m"}
            }),
            serde_json::json!({
                "type": "event_msg",
                "payload": {"type": "token_count", "info": {"total_token_usage": {
                    "input_tokens": 10,
                    "cached_input_tokens": 3,
                    "output_tokens": 5,
                    "total_tokens": 18
                }}}
            }),
        ];

        let mut events = Vec::new();
        let mut usage = Vec::new();
        for record in records {
            let parsed = parse_codex(&record);
            events.extend(parsed.events);
            usage.extend(parsed.usage);
        }

        assert_eq!(events[0], "assistant: visible answer");
        assert!(events.iter().any(|l| l.contains("function_call shell")));
        assert!(events.iter().any(|l| l == "function_call_output: Mon"));
        assert!(!events.iter().any(|l| l.contains("hidden chain")));
        assert_eq!(
            usage,
            vec![
                "Codex usage: session cumulative",
                "total 18",
                "input 10",
                "cache 3",
                "output 5"
            ]
        );
    }

    #[test]
    fn parses_claude_messages_tools_and_usage_components() {
        let mut observed = Usage::default();
        let record = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "hello\nworld"},
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "a"}},
                    {"type": "thinking", "thinking": "do not show"}
                ],
                "usage": {
                    "input_tokens": 11,
                    "cache_creation_input_tokens": 2,
                    "cache_read_input_tokens": 7,
                    "output_tokens": 13
                }
            }
        });

        let parsed = parse_claude(&record, &mut observed, false);

        assert_eq!(parsed.events[0], "assistant: hello world");
        assert_eq!(parsed.events[1], "tool_use Read: {\"file_path\":\"a\"}");
        assert!(!parsed.events.iter().any(|l| l.contains("do not show")));
        assert_eq!(observed.total, 33);
        assert_eq!(
            parsed.usage,
            vec![
                "Claude usage: latest message",
                "total 33",
                "input 11",
                "cache c2 r7",
                "output 13"
            ]
        );
    }

    #[test]
    fn incremental_reader_does_not_consume_partial_lines() {
        let dir = temp_dir("observe-partial");
        let path = dir.join("codex.jsonl");
        write_all(
            &path,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"first\"}]}}\n{\"type\":\"response_item\"",
        );
        let mut reader = JsonlFollower::from_start(Agent::Codex, path.clone());

        let first = reader.read_once();
        let events = outcome_events(first);
        assert_eq!(events, vec!["user: first"]);
        assert!(reader.offset < fs::metadata(&path).unwrap().len());

        append_all(
            &path,
            ",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"text\":\"second\"}]}}\n",
        );
        let second = reader.read_once();
        assert_eq!(outcome_events(second), vec!["assistant: second"]);
    }

    #[test]
    fn rotation_restarts_and_observer_bounds_events() {
        let dir = temp_dir("observe-rotation");
        let path = dir.join("codex.jsonl");
        write_all(
            &path,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"old\"}]}}\n",
        );
        let mut reader = JsonlFollower::from_start(Agent::Codex, path.clone());
        assert_eq!(outcome_events(reader.read_once()), vec!["user: old"]);

        let replacement = dir.join("replacement.jsonl");
        write_all(
            &replacement,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"text\":\"new\"}]}}\n",
        );
        fs::rename(&replacement, &path).unwrap();
        let rotated = reader.read_once();
        match rotated {
            ReadOutcome::Changed { events, notice, .. } => {
                assert_eq!(events, vec!["assistant: new"]);
                assert!(notice.unwrap().contains("source file replaced"));
            }
            _ => panic!("expected rotation change"),
        }

        let mut observer = Observer {
            enabled: true,
            ..Observer::default()
        };
        for i in 0..(MAX_EVENTS + 7) {
            push_bounded(&mut observer.events, format!("event {i}"), MAX_EVENTS);
        }
        observer.lines = observer.events.iter().cloned().collect();
        assert_eq!(observer.lines.len(), MAX_EVENTS);
        assert_eq!(observer.lines[0], "event 7");
    }

    #[test]
    fn unsupported_agents_are_explicit() {
        let mut observer = Observer {
            enabled: true,
            ..Observer::default()
        };
        let mut session = session(Agent::Cursor, PathBuf::from("meta.json"));
        assert!(observer.poll(&session));
        assert!(observer.notice.contains("Cursor observer unsupported"));
        assert!(observer.lines[0].contains("unsupported"));

        session.agent = Agent::Kiro;
        session.id = "k".to_string();
        assert!(observer.poll(&session));
        assert!(observer
            .notice
            .contains("Kiro transcript observer unsupported"));
    }

    #[test]
    fn reader_recovers_from_oversized_records_and_first_read_truncation() {
        let dir = temp_dir("observe-oversize");
        let path = dir.join("codex.jsonl");
        write_all(
            &path,
            &format!(
                "{}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"old tail\"}]}}".repeat(900)
            ),
        );
        let mut reader = JsonlFollower::new(Agent::Codex, path.clone(), INITIAL_BACKFILL_BYTES);
        write_all(
            &path,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"after truncate\"}]}}\n",
        );
        assert_eq!(
            outcome_events(reader.read_once()),
            vec!["user: after truncate"]
        );

        let path = dir.join("oversize.jsonl");
        let long = "x".repeat(READ_BUDGET_BYTES as usize + 32);
        write_all(&path, &long);
        let mut reader = JsonlFollower::from_start(Agent::Codex, path.clone());
        match reader.read_once() {
            ReadOutcome::Changed { notice, .. } => {
                assert!(notice.unwrap().contains("skipped oversized"));
            }
            _ => panic!("expected oversize notice"),
        }
        append_all(
            &path,
            "\n{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"text\":\"recovered\"}]}}\n",
        );
        assert_eq!(
            outcome_events(reader.read_once()),
            vec!["assistant: recovered"]
        );
    }

    #[test]
    fn latest_usage_snapshot_wins_within_one_read() {
        let dir = temp_dir("observe-latest-usage");
        let path = dir.join("codex.jsonl");
        write_all(
            &path,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"total_tokens\":2}}}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":10,\"cached_input_tokens\":3,\"output_tokens\":5,\"total_tokens\":18}}}}\n",
        );
        let mut reader = JsonlFollower::from_start(Agent::Codex, path);
        match reader.read_once() {
            ReadOutcome::Changed { usage, .. } => {
                assert_eq!(
                    usage,
                    vec![
                        "Codex usage: session cumulative",
                        "total 18",
                        "input 10",
                        "cache 3",
                        "output 5"
                    ]
                );
            }
            _ => panic!("expected usage"),
        }
    }

    #[test]
    fn prompt_usage_sums_cumulative_growth_and_ignores_duplicate_snapshots() {
        let dir = temp_dir("observe-prompt-usage");
        let path = dir.join("codex.jsonl");
        write_all(
            &path,
            concat!(
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"measure this prompt\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":18000,\"cached_input_tokens\":14000,\"output_tokens\":2000,\"reasoning_output_tokens\":100,\"total_tokens\":20000},\"last_token_usage\":{\"input_tokens\":18000,\"cached_input_tokens\":14000,\"output_tokens\":2000,\"reasoning_output_tokens\":100,\"total_tokens\":20000}}}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":18000,\"cached_input_tokens\":14000,\"output_tokens\":2000,\"reasoning_output_tokens\":100,\"total_tokens\":20000},\"last_token_usage\":{\"input_tokens\":5000,\"cached_input_tokens\":4000,\"output_tokens\":500,\"reasoning_output_tokens\":20,\"total_tokens\":5500}}}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":40000,\"cached_input_tokens\":30000,\"output_tokens\":3210,\"reasoning_output_tokens\":150,\"total_tokens\":43210},\"last_token_usage\":{\"input_tokens\":22000,\"cached_input_tokens\":16000,\"output_tokens\":1210,\"reasoning_output_tokens\":50,\"total_tokens\":23210}}}}\n"
            ),
        );
        let mut reader = JsonlFollower::from_start(Agent::Codex, path);
        let mut observer = Observer {
            enabled: true,
            ..Observer::default()
        };

        observer.apply_outcome(reader.read_once());
        observer.lines = observer.events.iter().cloned().collect();

        let prompt_usage = observer
            .lines
            .iter()
            .filter_map(|line| prompt_usage_from_line(line))
            .collect::<Vec<_>>();
        assert_eq!(prompt_usage.len(), 1, "one aggregate belongs to the prompt");
        assert_eq!(prompt_usage[0].total, 43_210);
        assert_eq!(prompt_usage[0].input, 40_000);
        assert_eq!(prompt_usage[0].cached, 30_000);
        assert_eq!(prompt_usage[0].output, 3_210);
        assert_eq!(prompt_usage[0].reasoning, 150);
    }

    fn outcome_events(outcome: ReadOutcome) -> Vec<String> {
        match outcome {
            ReadOutcome::Changed { events, .. } => events,
            ReadOutcome::Unchanged => Vec::new(),
            ReadOutcome::Error(e) => panic!("{e}"),
        }
    }

    fn write_all(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
    }

    fn append_all(path: &Path, text: &str) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("mindplayer-{name}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn session(agent: Agent, file: PathBuf) -> Session {
        Session {
            id: agent.as_str().to_string(),
            agent,
            cwd: PathBuf::from("/tmp"),
            file,
            started_at: Some(Utc::now()),
            last_active: Some(Utc::now()),
            last_prompt_at: None,
            tokens: TokenUsage::default(),
            title: String::new(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }
}
