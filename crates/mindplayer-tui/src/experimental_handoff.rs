//! Experimental cross-agent handoff helpers.
//!
//! This module is intentionally isolated behind `MINDPLAYER_EXPERIMENTAL_HANDOFF`
//! so the experiment can be removed without touching the normal session flows.

use mindplayer_core::{new_session, Agent, Command, Session};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ENV_FLAG: &str = "MINDPLAYER_EXPERIMENTAL_HANDOFF";
pub(crate) const HANDOFF_DIR_ENV: &str = "MINDPLAYER_HANDOFF_DIR";
const INLINE_CHAR_BUDGET: usize = 60_000;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn enabled() -> bool {
    std::env::var(ENV_FLAG)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

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
The full extracted transcript is included below and also saved in the handoff artifact.

```text
{transcript_inline}
```
"
        )
    } else {
        "\
The source session is large, so the transcript is not pasted into this prompt.
Do not answer from a tail preview. Read the full handoff artifact before continuing.
"
        .to_string()
    };
    let size_note = if inline_truncated {
        "large source session; artifact-only inline prompt"
    } else {
        "full inline transcript included"
    };
    format!(
        "\
MindPlayer experimental handoff from {source_agent} to {target_agent}.

Source session:
- agent: {source_agent}
- session id: {source_id}
- working directory: {cwd}
- title: {title}
- full extracted transcript: {artifact}
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

fn extract_transcript(source: &Session) -> Result<String, String> {
    if source.file.as_os_str().is_empty() {
        return Err("source session has no transcript file yet".to_string());
    }
    match source.agent {
        Agent::Claude => extract_jsonl_transcript(source, parse_claude_turn),
        Agent::Codex => extract_jsonl_transcript(source, parse_codex_turn),
        Agent::Kiro => extract_kiro_transcript(source),
    }
}

fn extract_jsonl_transcript(
    source: &Session,
    parse_turn: fn(&Value) -> Option<(String, String)>,
) -> Result<String, String> {
    let file = File::open(&source.file)
        .map_err(|e| format!("failed to open {}: {e}", source.file.display()))?;
    let reader = BufReader::new(file.take(MAX_SOURCE_BYTES));
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
    if count == 0 {
        return Err(format!(
            "no readable transcript turns found in {}",
            source.file.display()
        ));
    }
    Ok(out.trim().to_string())
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
        return extract_jsonl_transcript(&clone, parse_generic_turn);
    }
    let text = std::fs::read_to_string(&source.file)
        .map_err(|e| format!("failed to read {}: {e}", source.file.display()))?;
    Ok(format!(
        "Kiro metadata sidecar only; no adjacent jsonl transcript was found.\n\n{}",
        neutralize_controls(text.trim())
    ))
}

fn parse_generic_turn(v: &Value) -> Option<(String, String)> {
    let role = v
        .get("role")
        .or_else(|| v.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("message")
        .to_string();
    let text = v
        .get("message")
        .and_then(extract_content_text)
        .or_else(|| v.get("content").and_then(extract_content_text))
        .or_else(|| v.get("text").and_then(Value::as_str).map(str::to_string))?;
    Some((role, text))
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
        "# MindPlayer experimental handoff\n\n- source agent: {}\n- target agent: {}\n- source session id: {}\n- cwd: {}\n- title: {}\n- transcript file: {}\n\n---\n\n{}",
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
    fn enabled_tracks_env_flag() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_FLAG);
        assert!(!enabled());

        std::env::set_var(ENV_FLAG, "1");
        assert!(enabled());

        std::env::set_var(ENV_FLAG, "true");
        assert!(enabled());

        std::env::set_var(ENV_FLAG, "0");
        assert!(!enabled());

        std::env::remove_var(ENV_FLAG);
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
    fn title_marks_handoff_direction() {
        let dir = temp_dir("title");
        assert_eq!(
            title_for(&session(dir.join("x.jsonl")), Agent::Kiro),
            "handoff claude -> kiro claude-s"
        );
    }

    #[test]
    fn large_transcript_is_saved_full_but_prompt_uses_artifact_only() {
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
        assert!(prompt.contains("artifact-only inline prompt"));
        assert!(prompt.contains("Do not answer from a tail preview"));
        assert!(prompt.contains(&prepared.artifact.display().to_string()));
        assert!(!prompt.contains(recent));
        let artifact = fs::read_to_string(&prepared.artifact).unwrap();
        assert!(artifact.contains("old context old context old context"));
        assert!(artifact.contains(recent));

        std::env::remove_var(HANDOFF_DIR_ENV);
    }
}
