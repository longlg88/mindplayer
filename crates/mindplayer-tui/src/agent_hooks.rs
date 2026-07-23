//! Optional hook-based agent status classification for Claude Code and Codex,
//! sourced from a small per-pane state file that an installed lifecycle hook
//! writes on every event. Falls back to the screen-text heuristic in `pty.rs`
//! when no hook data exists yet (hooks not installed, or this pane hasn't
//! seen its first event) — and always for Kiro, whose CLI has no
//! approval-wait hook event to distinguish "blocked" from "still computing"
//! (see docs/mindplayer-vs-herdr investigation: kiro-cli exposes
//! AgentSpawn/UserPromptSubmit/PreToolUse/PostToolUse/Stop but no
//! PermissionRequest equivalent).

use crate::app::SessionStatus;
use mindplayer_core::session::Agent;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// True for agents whose CLI exposes a hook mindplayer can wire into for live
/// status. Kiro is deliberately excluded — see module docs.
pub fn supports_hooks(agent: Agent) -> bool {
    matches!(agent, Agent::Claude | Agent::Codex)
}

/// A hook-derived reading for one pane: the status it implies, and when the
/// underlying event fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookReading {
    pub status: SessionStatus,
    pub event_epoch_secs: u64,
}

/// Only a `Working` reading goes stale — the agent may have crashed or been
/// killed mid-turn without ever firing `Stop`, and a screen-text recheck is a
/// safer read at that point than trusting a five-minute-old "still going".
/// `Blocked`/`Idle` are legitimate rest states that don't expire on their own
/// (a session waiting an hour for approval is still, correctly, waiting).
const WORKING_TRUST_WINDOW: Duration = Duration::from_secs(5 * 60);

fn hooks_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MINDPLAYER_HOOKS_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("hooks")
}

fn hook_state_path(pane_id: &str) -> PathBuf {
    hooks_dir().join(format!("{pane_id}.json"))
}

fn event_to_status(event: &str) -> Option<SessionStatus> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(SessionStatus::Working),
        "PermissionRequest" => Some(SessionStatus::Blocked),
        "Stop" => Some(SessionStatus::Idle),
        _ => None,
    }
}

/// Parse a hook state file's contents (`{"event": "...", "ts": <unix_secs>}`)
/// into a reading, applying the staleness rule above. Pulled out of
/// [`read_hook_status`] so the parsing/staleness logic is unit-testable
/// without touching the filesystem.
fn parse_hook_state(raw: &str, now_epoch_secs: u64) -> Option<HookReading> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let event = value.get("event")?.as_str()?;
    let ts = value.get("ts")?.as_u64()?;
    let status = event_to_status(event)?;
    if status == SessionStatus::Working
        && now_epoch_secs.saturating_sub(ts) > WORKING_TRUST_WINDOW.as_secs()
    {
        return None;
    }
    Some(HookReading {
        status,
        event_epoch_secs: ts,
    })
}

/// Read this pane's latest hook event and map it to a status. `None` if
/// there's no hook data yet, the file is unreadable/corrupt, or a `Working`
/// reading has gone stale (see [`WORKING_TRUST_WINDOW`]).
pub fn read_hook_status(pane_id: &str) -> Option<HookReading> {
    let raw = fs::read_to_string(hook_state_path(pane_id)).ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    parse_hook_state(&raw, now)
}

// --- hook script + installer -------------------------------------------

/// Installed at `~/.mindplayer/hooks/mindplayer-agent-hook.sh`. Reads the
/// hook JSON Claude Code/Codex send on stdin, and — only when running inside
/// a mindplayer-spawned pane (`$MINDPLAYER_PANE_ID` set by `pty.rs::spawn`) —
/// records the event so mindplayer can read it back. Always exits 0: this is
/// purely observational and must never block, deny, or alter agent behavior.
pub const HOOK_SCRIPT: &str = r#"#!/bin/sh
# Installed by `mindplayer --install-agent-hooks`. Do not edit by hand; rerun
# that command after a mindplayer upgrade if this script needs to change.
if [ -z "$MINDPLAYER_PANE_ID" ]; then
  exit 0
fi
input="$(cat)"
event="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("hook_event_name", ""))
except Exception:
    pass
' 2>/dev/null)"
if [ -z "$event" ]; then
  exit 0
fi
dir="${MINDPLAYER_HOOKS_DIR:-$HOME/.mindplayer/hooks}"
mkdir -p "$dir" 2>/dev/null
ts="$(date +%s)"
printf '{"event":"%s","ts":%s}\n' "$event" "$ts" > "$dir/$MINDPLAYER_PANE_ID.json" 2>/dev/null
exit 0
"#;

pub const HOOK_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
];

/// Where the installer writes the hook script itself (separate from the
/// per-pane state files it later writes into `hooks_dir()`).
pub fn hook_script_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".mindplayer")
        .join("mindplayer-agent-hook.sh")
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// Claude Code's user-level (all-projects) settings file — confirmed against
/// the official docs, not guessed: https://code.claude.com/docs/en/hooks.
pub fn claude_settings_path() -> PathBuf {
    home().join(".claude").join("settings.json")
}

/// Codex's user-level hooks file — confirmed against the official docs
/// (`CODEX_HOME` can relocate this; not handled here since mindplayer itself
/// doesn't otherwise honor that override for codex session discovery either).
pub fn codex_hooks_path() -> PathBuf {
    home().join(".codex").join("hooks.json")
}

/// One config file's before/after merge result.
pub struct MergeResult {
    /// Full JSON to write back if applying.
    pub value: serde_json::Value,
    /// Events that already had our command hook registered (no-op, idempotent).
    pub already_present: Vec<String>,
    /// Events newly added by this merge.
    pub added: Vec<String>,
}

/// Additively register `command` for every event in [`HOOK_EVENTS`] inside an
/// existing (possibly empty/absent) Claude-settings-shaped or
/// codex-hooks.json-shaped JSON value — both use the identical
/// `{"hooks": {"EventName": [{"matcher"?, "hooks": [{"type","command"}]}]}}`
/// shape. Never removes or reorders anything already there; only appends a
/// new matcher-group entry for events that don't already reference this exact
/// command, so re-running the installer (e.g. after a mindplayer upgrade
/// moves the script) is a no-op for events already wired up.
fn merge_hook_command(existing: serde_json::Value, command: &str) -> MergeResult {
    use serde_json::{json, Value};

    let mut root = match existing {
        Value::Object(_) => existing,
        _ => json!({}),
    };
    let root_obj = root.as_object_mut().expect("root is always an object");
    let hooks = root_obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().expect("hooks is always an object");

    let mut already_present = Vec::new();
    let mut added = Vec::new();

    for event in HOOK_EVENTS {
        let entries = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| json!([]));
        if !entries.is_array() {
            *entries = json!([]);
        }
        let entries_arr = entries.as_array_mut().expect("entries is always an array");

        let already_wired = entries_arr.iter().any(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|hs| {
                    hs.iter()
                        .any(|h| h.get("command").and_then(Value::as_str) == Some(command))
                })
        });
        if already_wired {
            already_present.push((*event).to_string());
            continue;
        }
        entries_arr.push(json!({
            "hooks": [
                { "type": "command", "command": command }
            ]
        }));
        added.push((*event).to_string());
    }

    MergeResult {
        value: root,
        already_present,
        added,
    }
}

fn read_json_or_empty(path: &std::path::Path) -> serde_json::Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Compute what installing would change, without writing anything — used to
/// show the user a diff-shaped report before touching their real global
/// Claude Code / Codex config.
pub fn plan_install() -> (MergeResult, MergeResult) {
    let script_path = hook_script_path().to_string_lossy().into_owned();
    let claude = merge_hook_command(read_json_or_empty(&claude_settings_path()), &script_path);
    let codex = merge_hook_command(read_json_or_empty(&codex_hooks_path()), &script_path);
    (claude, codex)
}

/// Actually write the hook script and both config files. Call only after the
/// caller has shown the user `plan_install`'s result and gotten a go-ahead —
/// this touches global config shared with every other Claude Code/Codex
/// session on the machine, not just mindplayer's own.
pub fn apply_install(claude: &MergeResult, codex: &MergeResult) -> std::io::Result<()> {
    let script_path = hook_script_path();
    if let Some(dir) = script_path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&script_path, HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)?;
    }

    for (path, merged) in [
        (claude_settings_path(), &claude.value),
        (codex_hooks_path(), &codex.value),
    ] {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let pretty = serde_json::to_string_pretty(merged)?;
        fs::write(&path, pretty + "\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_events_to_status() {
        assert_eq!(
            event_to_status("UserPromptSubmit"),
            Some(SessionStatus::Working)
        );
        assert_eq!(event_to_status("PreToolUse"), Some(SessionStatus::Working));
        assert_eq!(event_to_status("PostToolUse"), Some(SessionStatus::Working));
        assert_eq!(
            event_to_status("PermissionRequest"),
            Some(SessionStatus::Blocked)
        );
        assert_eq!(event_to_status("Stop"), Some(SessionStatus::Idle));
        assert_eq!(event_to_status("SessionStart"), None);
        assert_eq!(event_to_status(""), None);
    }

    #[test]
    fn working_reading_expires_after_the_trust_window() {
        let now = 10_000u64;
        let fresh = format!(r#"{{"event":"PreToolUse","ts":{}}}"#, now - 60);
        assert_eq!(
            parse_hook_state(&fresh, now).map(|r| r.status),
            Some(SessionStatus::Working)
        );
        let stale = format!(r#"{{"event":"PreToolUse","ts":{}}}"#, now - 600);
        assert_eq!(parse_hook_state(&stale, now), None);
    }

    #[test]
    fn blocked_and_idle_readings_never_expire() {
        let now = 100_000u64;
        let old_blocked = format!(r#"{{"event":"PermissionRequest","ts":{}}}"#, now - 3600);
        assert_eq!(
            parse_hook_state(&old_blocked, now).map(|r| r.status),
            Some(SessionStatus::Blocked)
        );
        let old_idle = format!(r#"{{"event":"Stop","ts":{}}}"#, now - 3600);
        assert_eq!(
            parse_hook_state(&old_idle, now).map(|r| r.status),
            Some(SessionStatus::Idle)
        );
    }

    #[test]
    fn malformed_or_unknown_state_is_ignored() {
        let now = 1_000u64;
        assert_eq!(parse_hook_state("not json", now), None);
        assert_eq!(parse_hook_state(r#"{"event":"Weird","ts":1}"#, now), None);
        assert_eq!(parse_hook_state(r#"{"ts":1}"#, now), None);
        assert_eq!(parse_hook_state(r#"{"event":"Stop"}"#, now), None);
    }

    #[test]
    fn only_claude_and_codex_support_hooks() {
        assert!(supports_hooks(Agent::Claude));
        assert!(supports_hooks(Agent::Codex));
        assert!(!supports_hooks(Agent::Kiro));
    }

    #[test]
    fn merge_preserves_every_existing_hook_and_unrelated_key() {
        // Regression guard: this merges into the user's real, already
        // heavily-hooked ~/.claude/settings.json — losing or reordering an
        // existing entry here would silently break their other tooling.
        let existing = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "~/.claude/hooks/omc-pretooluse.sh"}]},
                    {"hooks": [{"type": "command", "command": "~/.claude/hooks/another.sh"}]}
                ],
                "Stop": [
                    {"hooks": [{"type": "command", "command": "~/.claude/hooks/stop-reminder.sh"}]}
                ],
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "~/.claude/hooks/session-start.sh"}]}
                ]
            },
            "permissions": {"allow": ["Bash(git *)"]},
            "someOtherTopLevelKey": {"nested": true}
        });

        let result = merge_hook_command(
            existing.clone(),
            "/home/u/.mindplayer/mindplayer-agent-hook.sh",
        );

        // Every pre-existing hook entry is still there, untouched, in order.
        assert_eq!(
            result.value["hooks"]["PreToolUse"][0],
            existing["hooks"]["PreToolUse"][0]
        );
        assert_eq!(
            result.value["hooks"]["PreToolUse"][1],
            existing["hooks"]["PreToolUse"][1]
        );
        assert_eq!(
            result.value["hooks"]["Stop"][0],
            existing["hooks"]["Stop"][0]
        );
        // An event mindplayer doesn't touch (SessionStart) is left completely
        // alone — not even iterated.
        assert_eq!(
            result.value["hooks"]["SessionStart"],
            existing["hooks"]["SessionStart"]
        );
        // Unrelated top-level keys survive the round-trip.
        assert_eq!(result.value["permissions"], existing["permissions"]);
        assert_eq!(
            result.value["someOtherTopLevelKey"],
            existing["someOtherTopLevelKey"]
        );

        // Our own entry got appended (not prepended/replacing) for every
        // event we care about.
        assert_eq!(
            result.value["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            result.value["hooks"]["PreToolUse"][2]["hooks"][0]["command"],
            "/home/u/.mindplayer/mindplayer-agent-hook.sh"
        );
        assert_eq!(
            result.added,
            vec![
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "PermissionRequest",
                "Stop"
            ]
        );
        assert!(result.already_present.is_empty());
    }

    #[test]
    fn merge_is_idempotent() {
        let first = merge_hook_command(serde_json::json!({}), "/home/u/hook.sh");
        let second = merge_hook_command(first.value.clone(), "/home/u/hook.sh");
        assert!(second.added.is_empty());
        assert_eq!(second.already_present.len(), HOOK_EVENTS.len());
        // No duplicate entries accumulated on the second pass.
        assert_eq!(second.value["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }
}
