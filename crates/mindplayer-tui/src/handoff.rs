//! Cross-agent handoff helpers.

use mindplayer_core::{new_session, Agent, Command, Session};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const HANDOFF_DIR_ENV: &str = "MINDPLAYER_HANDOFF_DIR";
const INLINE_CHAR_BUDGET: usize = 60_000;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn target_for_choice(choice: usize) -> Agent {
    match choice {
        0 => Agent::Codex,
        1 => Agent::Claude,
        _ => Agent::Kiro,
    }
}

pub fn command_for(source: &Session, target: Agent) -> Command {
    new_session(target, source.cwd.clone())
}

pub fn title_for(source: &Session, target: Agent) -> String {
    format!(
        "handoff {} -> {} {}",
        source.agent.as_str(),
        target.as_str(),
        short(&source.id)
    )
}

pub struct PreparedHandoff {
    pub input: Vec<u8>,
    pub artifact: PathBuf,
    pub transcript_chars: usize,
    pub inline_truncated: bool,
}

pub fn prepare_initial_input(source: &Session, target: Agent) -> Result<PreparedHandoff, String> {
    let transcript = extract_transcript(source)?;
    let artifact = write_artifact(source, target, &transcript)?;
    let transcript_chars = transcript.chars().count();
    let inline_truncated = transcript_chars > INLINE_CHAR_BUDGET;
    let inline = (!inline_truncated).then_some(transcript.as_str());
    let mut prompt = prompt_for(source, target, &artifact, &inline, inline_truncated);
    prompt.push('\r');
    Ok(PreparedHandoff {
        input: prompt.into_bytes(),
        artifact,
        transcript_chars,
        inline_truncated,
    })
}

pub fn prepare_thread_sync_input(
    target: &Session,
    peers: &[Session],
) -> Result<PreparedHandoff, String> {
    let mut sections = Vec::new();
    for peer in peers {
        if peer.id == target.id {
            continue;
        }
        let Ok(transcript) = extract_transcript(peer) else {
            continue;
        };
        sections.push(format!(
            "\
## {} lane

- session id: {}
- title: {}
- transcript file: {}

{}
",
            peer.agent.as_str(),
            peer.id,
            peer.title,
            peer.file.display(),
            transcript
        ));
    }
    if sections.is_empty() {
        return Err("no readable peer lane transcripts found".to_string());
    }
    let transcript = sections.join("\n\n---\n\n");
    let artifact = write_thread_sync_artifact(target, peers, &transcript)?;
    let transcript_chars = transcript.chars().count();
    let inline_truncated = transcript_chars > INLINE_CHAR_BUDGET;
    let inline = (!inline_truncated).then_some(transcript.as_str());
    let mut prompt = thread_sync_prompt_for(target, peers, &artifact, &inline, inline_truncated);
    prompt.push('\r');
    Ok(PreparedHandoff {
        input: prompt.into_bytes(),
        artifact,
        transcript_chars,
        inline_truncated,
    })
}

#[cfg(test)]
pub fn extract_session_transcript(source: &Session) -> Result<String, String> {
    extract_transcript(source)
}

fn prompt_for(
    source: &Session,
    target: Agent,
    artifact: &Path,
    transcript_inline: &Option<&str>,
    inline_truncated: bool,
) -> String {
    let transcript_block = if let Some(transcript_inline) = transcript_inline {
        format!(
            "\
The extracted transcript is included below and also saved in the handoff artifact.

```text
{transcript_inline}
```
"
        )
    } else {
        "\
The source session is large, so the transcript is not pasted into this prompt.
Do not answer from a preview. Read the handoff artifact before continuing.
"
        .to_string()
    };
    let size_note = if inline_truncated {
        "large source session; artifact-only prompt"
    } else {
        "full inline transcript included"
    };
    format!(
        "\
MindPlayer handoff from {source_agent} to {target_agent}.

Source session:
- agent: {source_agent}
- session id: {source_id}
- working directory: {cwd}
- title: {title}
- extracted transcript artifact: {artifact}
- transcript mode: {size_note}

Before answering, first read the handoff artifact above. Then identify the latest user request, summarize the relevant prior context in a few bullets, ignore unrelated runtime/setup/hook noise unless it changes the task, and continue the same task in this working directory.
Treat the artifact as the previous session context. If the artifact is inaccessible or insufficient, ask me before making irreversible changes.

{transcript_block}
",
        source_agent = source.agent.as_str(),
        target_agent = target.as_str(),
        source_id = source.id,
        cwd = source.cwd.display(),
        title = source.title,
        artifact = artifact.display(),
        transcript_block = transcript_block,
        size_note = size_note
    )
}

fn thread_sync_prompt_for(
    target: &Session,
    peers: &[Session],
    artifact: &Path,
    transcript_inline: &Option<&str>,
    inline_truncated: bool,
) -> String {
    let peer_list = peers
        .iter()
        .filter(|p| p.id != target.id)
        .map(|p| format!("- {}: {} ({})", p.agent.as_str(), p.id, p.title))
        .collect::<Vec<_>>()
        .join("\n");
    let transcript_block = if let Some(transcript_inline) = transcript_inline {
        format!(
            "\
The peer lane transcripts are included below and also saved in the sync artifact.

```text
{transcript_inline}
```
"
        )
    } else {
        "\
The peer lane transcripts are large, so they are not pasted into this prompt.
Do not answer from a preview. Read the sync artifact before continuing.
"
        .to_string()
    };
    let size_note = if inline_truncated {
        "large peer lanes; artifact-only prompt"
    } else {
        "full inline peer transcripts included"
    };
    format!(
        "\
MindPlayer thread sync for this {target_agent} session.

This session is one lane in a multi-agent MindPlayer thread. Other lanes have context that may not exist in this native {target_agent} session yet.

Current lane:
- agent: {target_agent}
- session id: {target_id}
- working directory: {cwd}
- title: {title}

Peer lanes:
{peer_list}

Full peer context artifact: {artifact}
Transcript mode: {size_note}

Before answering the user's next request, first read the sync artifact above if the inline context is truncated. Incorporate the peer lane context as prior conversation state for this same task. Do not redo completed work unless the peer context indicates it failed or is stale.

{transcript_block}
",
        target_agent = target.agent.as_str(),
        target_id = target.id,
        cwd = target.cwd.display(),
        title = target.title,
        peer_list = peer_list,
        artifact = artifact.display(),
        size_note = size_note,
        transcript_block = transcript_block,
    )
}

fn extract_transcript(source: &Session) -> Result<String, String> {
    if source.file.as_os_str().is_empty() {
        return Ok(metadata_only_transcript(
            source,
            "source session has no transcript file yet",
        ));
    }
    match source.agent {
        Agent::Claude => extract_jsonl_transcript(source, parse_claude_turn),
        Agent::Codex => extract_jsonl_transcript(source, parse_codex_turn),
        Agent::Kiro => extract_kiro_transcript(source),
    }
}

fn metadata_only_transcript(source: &Session, reason: &str) -> String {
    format!(
        "\
No readable transcript is available for this source session.

Reason: {reason}

Source metadata:
- agent: {}
- session id: {}
- working directory: {}
- title: {}

Continue from this task metadata and ask the user for missing context before making irreversible changes.
",
        source.agent.as_str(),
        source.id,
        source.cwd.display(),
        source.title
    )
}

fn extract_jsonl_transcript(
    source: &Session,
    parse_turn: fn(&Value) -> Option<(String, String)>,
) -> Result<String, String> {
    extract_jsonl_transcript_with_limit(source, parse_turn, MAX_SOURCE_BYTES)
}

fn extract_jsonl_transcript_with_limit(
    source: &Session,
    parse_turn: fn(&Value) -> Option<(String, String)>,
    max_source_bytes: u64,
) -> Result<String, String> {
    let file = File::open(&source.file)
        .map_err(|e| format!("failed to open {}: {e}", source.file.display()))?;
    let file_len = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    if file_len <= max_source_bytes {
        let (out, count) = parse_jsonl_reader(BufReader::new(file), parse_turn);
        if count == 0 {
            return Err(format!(
                "no readable transcript turns found in {}",
                source.file.display()
            ));
        }
        return Ok(out.trim().to_string());
    }

    let (head, tail, tail_record_dropped) =
        read_head_and_tail_windows(&source.file, file_len, max_source_bytes)?;
    let (head_out, head_count) = parse_jsonl_reader(BufReader::new(head.as_slice()), parse_turn);
    let (tail_out, tail_count) = parse_jsonl_reader(BufReader::new(tail.as_slice()), parse_turn);

    if head_count + tail_count == 0 {
        return Err(format!(
            "no readable transcript turns found in {}",
            source.file.display()
        ));
    }

    let mut out = String::new();
    if !head_out.trim().is_empty() {
        out.push_str(head_out.trim());
    }
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("## transcript truncation note\n");
    out.push_str(&format!(
        "The source JSONL was {} bytes, so MindPlayer extracted the beginning and latest tail within a {} byte read budget. The middle is omitted; open the transcript file for the complete raw record.",
        file_len, max_source_bytes
    ));
    if tail_record_dropped {
        out.push_str(" The latest JSONL record was larger than the tail window and could not be parsed from the bounded extract.");
    }
    if !tail_out.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(tail_out.trim());
    }

    Ok(out.trim().to_string())
}

fn parse_jsonl_reader<R: BufRead>(
    reader: R,
    parse_turn: fn(&Value) -> Option<(String, String)>,
) -> (String, usize) {
    let mut out = String::new();
    let mut count = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some((role, text)) = parse_turn(&v) {
            if text.trim().is_empty() {
                continue;
            }
            count += 1;
            out.push_str("\n\n## ");
            out.push_str(&role);
            out.push('\n');
            out.push_str(&neutralize_controls(text.trim()));
        }
    }
    (out, count)
}

fn read_head_and_tail_windows(
    path: &Path,
    file_len: u64,
    max_source_bytes: u64,
) -> Result<(Vec<u8>, Vec<u8>, bool), String> {
    let head_bytes = (max_source_bytes / 2).max(1);
    let tail_bytes = max_source_bytes.saturating_sub(head_bytes).max(1);

    let mut head_file =
        File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let mut head = Vec::new();
    Read::by_ref(&mut head_file)
        .take(head_bytes)
        .read_to_end(&mut head)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

    let tail_start = file_len.saturating_sub(tail_bytes);
    let tail_starts_on_line_boundary = if tail_start == 0 {
        true
    } else {
        let mut probe =
            File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        probe
            .seek(SeekFrom::Start(tail_start - 1))
            .map_err(|e| format!("failed to seek {}: {e}", path.display()))?;
        let mut prev = [0u8; 1];
        probe
            .read_exact(&mut prev)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        prev[0] == b'\n'
    };

    let mut tail_file =
        File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    tail_file
        .seek(SeekFrom::Start(tail_start))
        .map_err(|e| format!("failed to seek {}: {e}", path.display()))?;
    let mut tail = Vec::new();
    tail_file
        .read_to_end(&mut tail)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut tail_record_dropped = false;
    if !tail_starts_on_line_boundary {
        if let Some(pos) = tail.iter().position(|b| *b == b'\n') {
            tail.drain(..=pos);
        } else {
            tail.clear();
            tail_record_dropped = true;
        }
    }

    Ok((head, tail, tail_record_dropped))
}

fn parse_claude_turn(v: &Value) -> Option<(String, String)> {
    let kind = v.get("type").and_then(Value::as_str)?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let message = v.get("message")?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_string();
    let text = message
        .get("content")
        .and_then(extract_content_text)
        .unwrap_or_default();
    Some((role, text))
}

fn parse_codex_turn(v: &Value) -> Option<(String, String)> {
    if v.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = v.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("message")
        .to_string();
    let text = payload
        .get("content")
        .and_then(extract_content_text)
        .unwrap_or_default();
    Some((role, text))
}

fn extract_kiro_transcript(source: &Session) -> Result<String, String> {
    let jsonl = source.file.with_extension("jsonl");
    if jsonl.exists() {
        let mut clone = source.clone();
        clone.file = jsonl;
        return extract_jsonl_transcript(&clone, parse_kiro_turn);
    }
    let text = std::fs::read_to_string(&source.file)
        .map_err(|e| format!("failed to read {}: {e}", source.file.display()))?;
    Ok(format!(
        "Kiro metadata sidecar only; no adjacent jsonl transcript was found.\n\n{}",
        neutralize_controls(text.trim())
    ))
}

fn parse_kiro_turn(v: &Value) -> Option<(String, String)> {
    let kind = v.get("kind").and_then(Value::as_str)?;
    let data = v.get("data")?;
    let role = match kind {
        "Prompt" => "user",
        "AssistantMessage" => "assistant",
        "ToolResults" => "tool",
        _ => return None,
    };
    let text = data
        .get("content")
        .and_then(extract_kiro_content_text)
        .unwrap_or_default();
    (!text.trim().is_empty()).then(|| (role.to_string(), text))
}

fn extract_kiro_content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
                let data = item.get("data").unwrap_or(item);
                match kind {
                    "text" => {
                        if let Some(text) = data.as_str() {
                            append_block(&mut out, text);
                        }
                    }
                    // Kiro stores large signed thinking blobs. They are not
                    // user-visible task context, and including them can drown
                    // out the actual handoff.
                    "thinking" => {}
                    "toolUse" => {
                        let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = data
                            .get("input")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "{}".to_string());
                        append_block(&mut out, &format!("[tool_use {name}] {input}"));
                    }
                    "toolResult" => {
                        let status = data.get("status").and_then(Value::as_str).unwrap_or("");
                        let result = data
                            .get("content")
                            .and_then(extract_kiro_content_text)
                            .unwrap_or_else(|| data.to_string());
                        append_block(&mut out, &format!("[tool_result {status}] {result}"));
                    }
                    "json" => {
                        append_block(&mut out, &data.get("data").unwrap_or(data).to_string());
                    }
                    _ => {
                        if let Some(text) = data.as_str() {
                            append_block(&mut out, text);
                        } else if let Some(text) = data
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| data.get("content").and_then(extract_kiro_content_text))
                        {
                            append_block(&mut out, &text);
                        }
                    }
                }
            }
            (!out.is_empty()).then_some(out)
        }
        Value::Object(_) => content
            .get("content")
            .and_then(extract_kiro_content_text)
            .or_else(|| {
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        _ => None,
    }
}

fn extract_content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    append_block(&mut out, text);
                } else if let Some(text) = item.get("content").and_then(extract_content_text) {
                    append_block(&mut out, &text);
                } else if item.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = item
                        .get("input")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".to_string());
                    append_block(&mut out, &format!("[tool_use {name}] {input}"));
                } else if item.get("type").and_then(Value::as_str) == Some("tool_result") {
                    append_block(&mut out, &format!("[tool_result] {}", item));
                }
            }
            (!out.is_empty()).then_some(out)
        }
        Value::Object(_) => content
            .get("content")
            .and_then(extract_content_text)
            .or_else(|| {
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        _ => None,
    }
}

fn append_block(out: &mut String, text: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(text);
}

fn write_artifact(source: &Session, target: Agent, transcript: &str) -> Result<PathBuf, String> {
    let dir = handoff_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before unix epoch: {e}"))?
        .as_secs();
    let path = dir.join(format!(
        "{}-to-{}-{}-{}.md",
        source.agent.as_str(),
        target.as_str(),
        safe_id(&source.id),
        ts
    ));
    let mut f = create_private_file(&path)?;
    writeln!(
        f,
        "# MindPlayer handoff\n\n- source agent: {}\n- target agent: {}\n- source session id: {}\n- cwd: {}\n- title: {}\n- transcript file: {}\n\n---\n\n{}",
        source.agent.as_str(),
        target.as_str(),
        source.id,
        source.cwd.display(),
        source.title,
        source.file.display(),
        transcript
    )
    .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(path)
}

fn write_thread_sync_artifact(
    target: &Session,
    peers: &[Session],
    transcript: &str,
) -> Result<PathBuf, String> {
    let dir = handoff_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before unix epoch: {e}"))?
        .as_secs();
    let path = dir.join(format!(
        "sync-to-{}-{}-{}.md",
        target.agent.as_str(),
        safe_id(&target.id),
        ts
    ));
    let mut f = create_private_file(&path)?;
    let peer_list = peers
        .iter()
        .filter(|p| p.id != target.id)
        .map(|p| format!("- {} {} ({})", p.agent.as_str(), p.id, p.file.display()))
        .collect::<Vec<_>>()
        .join("\n");
    writeln!(
        f,
        "# MindPlayer thread sync\n\n- target agent: {}\n- target session id: {}\n- cwd: {}\n- title: {}\n\nPeer lanes:\n{}\n\n---\n\n{}",
        target.agent.as_str(),
        target.id,
        target.cwd.display(),
        target.title,
        peer_list,
        transcript
    )
    .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("failed to create {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<File, String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("failed to create {}: {e}", path.display()))
}

fn handoff_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(HANDOFF_DIR_ENV) {
        return PathBuf::from(dir);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
        .join(".mindplayer")
        .join("handoffs")
}

fn safe_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect()
}

fn neutralize_controls(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' || c == '\t' || !c.is_control() {
                c
            } else {
                ' '
            }
        })
        .collect()
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mindplayer_core::TokenUsage;
    use std::fs;
    use std::path::PathBuf;

    fn session(file: PathBuf) -> Session {
        Session {
            id: "claude-session-123456".into(),
            agent: Agent::Claude,
            cwd: PathBuf::from("/work/project"),
            file,
            started_at: None,
            last_active: None,
            tokens: TokenUsage::default(),
            title: "fix failing deployment".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mindplayer-handoff-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prompt_carries_source_transcript_and_submits() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("prompt");
        let transcript = dir.join("claude.jsonl");
        fs::write(
            &transcript,
            r#"{"type":"user","message":{"role":"user","content":"please fix deploy"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I changed deploy.yaml and tests pass."}]}}"#,
        )
        .unwrap();
        std::env::set_var(HANDOFF_DIR_ENV, dir.join("handoffs"));

        let prepared = prepare_initial_input(&session(transcript), Agent::Codex).unwrap();
        assert_eq!(prepared.input.last(), Some(&b'\r'));
        let text = String::from_utf8(prepared.input).unwrap();
        assert!(text.contains("from claude to codex"));
        assert!(text.contains("session id: claude-session-123456"));
        assert!(text.contains("working directory: /work/project"));
        assert!(text.contains("title: fix failing deployment"));
        assert!(text.contains("please fix deploy"));
        assert!(text.contains("I changed deploy.yaml"));
        assert!(prepared.artifact.exists());
        assert!(fs::read_to_string(&prepared.artifact)
            .unwrap()
            .contains("## assistant"));

        std::env::remove_var(HANDOFF_DIR_ENV);
    }

    #[test]
    fn prompt_falls_back_to_metadata_when_source_has_no_file() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("metadata");
        std::env::set_var(HANDOFF_DIR_ENV, dir.join("handoffs"));
        let mut source = session(PathBuf::new());
        source.id = "new:claude:1".into();
        source.title = "(new claude session)".into();

        let prepared = prepare_initial_input(&source, Agent::Codex).unwrap();
        let prompt = String::from_utf8(prepared.input).unwrap();

        assert!(prompt.contains("source session has no transcript file yet"));
        assert!(prompt.contains("new:claude:1"));
        assert!(prepared.artifact.exists());

        std::env::remove_var(HANDOFF_DIR_ENV);
    }

    #[test]
    fn kiro_jsonl_transcript_uses_v1_content_schema() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("kiro-jsonl");
        let sidecar = dir.join("kiro.json");
        let jsonl = dir.join("kiro.jsonl");
        fs::write(&sidecar, "{}").unwrap();
        fs::write(
            &jsonl,
            r#"{"version":"v1","kind":"Prompt","data":{"content":[{"kind":"text","data":"find the ten second timer"}]}}
{"version":"v1","kind":"AssistantMessage","data":{"content":[{"kind":"thinking","data":{"text":"hidden reasoning","signature":"large-signature"}},{"kind":"text","data":"It is in blueprint/testmindplayer."},{"kind":"toolUse","data":{"name":"grep","input":{"pattern":"sleep 10","path":"blueprint"}}}]}}
{"version":"v1","kind":"ToolResults","data":{"content":[{"kind":"toolResult","data":{"content":[{"kind":"text","data":"count_time.sh"}],"status":"success"}}]}}"#,
        )
        .unwrap();
        std::env::set_var(HANDOFF_DIR_ENV, dir.join("handoffs"));
        let mut source = session(sidecar);
        source.agent = Agent::Kiro;
        source.id = "kiro-session".into();

        let prepared = prepare_initial_input(&source, Agent::Codex).unwrap();
        let prompt = String::from_utf8(prepared.input).unwrap();

        assert!(prompt.contains("from kiro to codex"));
        assert!(prompt.contains("find the ten second timer"));
        assert!(prompt.contains("It is in blueprint/testmindplayer."));
        assert!(prompt.contains("[tool_use grep]"));
        assert!(prompt.contains("[tool_result success] count_time.sh"));
        assert!(!prompt.contains("large-signature"));

        std::env::remove_var(HANDOFF_DIR_ENV);
    }

    #[test]
    fn title_marks_handoff_direction() {
        let dir = temp_dir("title");
        assert_eq!(
            title_for(&session(dir.join("x.jsonl")), Agent::Kiro),
            "handoff claude -> kiro claude-s"
        );
    }

    #[test]
    fn large_inline_transcript_uses_artifact_only_prompt() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("large");
        let transcript = dir.join("claude.jsonl");
        let large = "old context ".repeat(8_000);
        let recent = "recent limit handoff details";
        fs::write(
            &transcript,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"{large}"}}}}
{{"type":"assistant","message":{{"role":"assistant","content":"{recent}"}}}}"#
            ),
        )
        .unwrap();
        std::env::set_var(HANDOFF_DIR_ENV, dir.join("handoffs"));

        let prepared = prepare_initial_input(&session(transcript), Agent::Codex).unwrap();
        assert!(prepared.inline_truncated);
        assert!(prepared.transcript_chars > INLINE_CHAR_BUDGET);
        let prompt = String::from_utf8(prepared.input).unwrap();
        assert!(prompt.contains("artifact-only prompt"));
        assert!(prompt.contains("Do not answer from a preview"));
        assert!(prompt.contains(&prepared.artifact.display().to_string()));
        assert!(!prompt.contains(recent));
        let artifact = fs::read_to_string(&prepared.artifact).unwrap();
        assert!(artifact.contains("old context old context old context"));
        assert!(artifact.contains(recent));

        std::env::remove_var(HANDOFF_DIR_ENV);
    }

    #[test]
    fn oversized_jsonl_extraction_keeps_latest_tail_turns() {
        let dir = temp_dir("oversized-tail");
        let transcript = dir.join("claude.jsonl");
        let middle = "middle context that should be omitted ".repeat(128);
        fs::write(
            &transcript,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"initial pulse task"}}}}
{{"type":"assistant","message":{{"role":"assistant","content":"{middle}"}}}}
{{"type":"user","message":{{"role":"user","content":"latest ClaudeCode work: write the todo html and report completion"}}}}"#
            ),
        )
        .unwrap();

        let extracted =
            extract_jsonl_transcript_with_limit(&session(transcript), parse_claude_turn, 512)
                .unwrap();

        assert!(extracted.contains("initial pulse task"));
        assert!(extracted.contains("transcript truncation note"));
        assert!(extracted.contains("latest ClaudeCode work: write the todo html"));
    }

    #[test]
    fn oversized_jsonl_extraction_notes_unparseable_huge_final_record() {
        let dir = temp_dir("oversized-final-record");
        let transcript = dir.join("claude.jsonl");
        let huge_final = "latest record too large ".repeat(256);
        fs::write(
            &transcript,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"initial pulse task"}}}}
{{"type":"assistant","message":{{"role":"assistant","content":"{huge_final}"}}}}"#
            ),
        )
        .unwrap();

        let extracted =
            extract_jsonl_transcript_with_limit(&session(transcript), parse_claude_turn, 512)
                .unwrap();

        assert!(extracted.contains("initial pulse task"));
        assert!(extracted.contains("latest JSONL record was larger than the tail window"));
    }
}
