// Prevent a console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! MindPlayer Tauri backend.
//!
//! Reuses `mindplayer-core` for discovery / token aggregation / archive state,
//! and bridges an embedded PTY (codex/claude) to the webview, where xterm.js
//! renders the live session. PTY output is streamed via the `pty://output`
//! event; the frontend sends keystrokes back through `pty_write`.

mod pty;

use chrono::Utc;
use mindplayer_core::{resume, scan, Agent, Aggregate, ScanConfig, Scope, Session, State};
use pty::PtyManager;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State as TauriState};

/// Shared app state.
struct AppState {
    config: ScanConfig,
    archive: Mutex<State>,
    ptys: PtyManager,
    /// Monotonic counter for unique new-session ids.
    new_counter: AtomicU64,
}

/// What the frontend receives for a scan.
#[derive(Serialize)]
struct ScanResult {
    sessions: Vec<Session>,
    aggregate: Aggregate,
}

/// Reject a cwd from the webview that isn't an existing directory before we
/// spawn a process in it.
fn validate_cwd(cwd: &str) -> Result<(), String> {
    if cwd.is_empty() || std::path::Path::new(cwd).is_dir() {
        Ok(())
    } else {
        Err(format!("cwd is not a directory: {cwd}"))
    }
}

fn parse_scope(scope: &str, cwd: Option<String>) -> Scope {
    match scope {
        "global" => Scope::Global,
        _ => {
            let dir = cwd
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            Scope::WorkingDir(dir)
        }
    }
}

/// The default working directory shown on the scope screen.
#[tauri::command]
fn default_cwd() -> String {
    std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Expand `~`, canonicalize, and validate a user-entered working dir, returning
/// the resolved absolute path. Errors (with a message) if it isn't a directory,
/// so the UI can keep the picker open and show why.
#[tauri::command]
fn resolve_cwd(cwd: String) -> Result<String, String> {
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        return Err("path is empty".to_string());
    }
    let expanded = if trimmed == "~" {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(trimmed))
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(trimmed))
    } else {
        PathBuf::from(trimmed)
    };
    let resolved = expanded.canonicalize().unwrap_or(expanded);
    if resolved.is_dir() {
        Ok(resolved.to_string_lossy().into_owned())
    } else {
        Err(format!("not a directory: {}", resolved.display()))
    }
}

/// Scan sessions in `scope` and return the list plus aggregate totals.
/// The list reflects the full in-scope set (the UI applies its archived /
/// sub-agent view filters); aggregate is always over everything collected.
#[tauri::command]
fn scan_sessions(scope: String, cwd: Option<String>, state: TauriState<AppState>) -> ScanResult {
    let scope = parse_scope(&scope, cwd);
    let mut sessions = scan(&scope, &state.config);
    {
        let mut archive = state.archive.lock().unwrap();
        // Match any queued new-session labels to freshly scanned sessions.
        if archive.resolve_pending(&sessions) {
            let _ = archive.save();
        }
        archive.apply(&mut sessions);
    }
    let aggregate = Aggregate::of(&sessions);
    ScanResult {
        sessions,
        aggregate,
    }
}

/// Mark a session archived (or not) in the sidecar state.
#[tauri::command]
fn set_archived(id: String, archived: bool, state: TauriState<AppState>) -> Result<(), String> {
    let mut archive = state.archive.lock().unwrap();
    archive.set_archived(&id, archived);
    archive.save().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_label(id: String, label: String, state: TauriState<AppState>) -> Result<(), String> {
    let mut archive = state.archive.lock().unwrap();
    archive.set_label(&id, &label);
    archive.save().map_err(|e| e.to_string())
}

/// Start (or restart) a PTY resuming `session_id` of `agent`, in `cwd`.
#[tauri::command]
fn pty_start(
    app: AppHandle,
    session_id: String,
    agent: String,
    cwd: String,
    cols: u16,
    rows: u16,
    state: TauriState<AppState>,
) -> Result<(), String> {
    let agent = match agent.as_str() {
        "claude" => Agent::Claude,
        "kiro" => Agent::Kiro,
        _ => Agent::Codex,
    };
    validate_cwd(&cwd)?;
    let session = Session {
        id: session_id.clone(),
        agent,
        cwd: PathBuf::from(&cwd),
        file: PathBuf::new(),
        started_at: None,
        last_active: None,
        last_prompt_at: None,
        tokens: Default::default(),
        title: String::new(),
        archived: false,
        is_subagent: false,
        context_pct: None,
    };
    let command = resume(&session);
    state
        .ptys
        .start(&app, &session_id, &command, cols, rows)
        .map_err(|e| e.to_string())
}

/// Start a brand new session (codex/claude) in `cwd`.
#[tauri::command]
fn pty_new(
    app: AppHandle,
    agent: String,
    cwd: String,
    label: String,
    cols: u16,
    rows: u16,
    state: TauriState<AppState>,
) -> Result<String, String> {
    let agent = match agent.as_str() {
        "claude" => Agent::Claude,
        "kiro" => Agent::Kiro,
        _ => Agent::Codex,
    };
    validate_cwd(&cwd)?;
    // A unique synthetic id per new session (avoids collisions in the PTY map).
    let n = state.new_counter.fetch_add(1, Ordering::Relaxed);
    let id = format!("new:{}:{}", agent.as_str(), n);
    let dir = PathBuf::from(&cwd);

    // Queue the label; codex/claude only write the rollout file after the first
    // interaction, so it's matched on a later scan (and persists across runs).
    let label = label.trim();
    if !label.is_empty() {
        let mut archive = state.archive.lock().unwrap();
        archive.add_pending_label(
            agent.as_str(),
            dir.clone(),
            Utc::now() - chrono::Duration::seconds(5),
            label,
        );
        let _ = archive.save();
    }

    let command = mindplayer_core::new_session(agent, dir);
    state
        .ptys
        .start(&app, &id, &command, cols, rows)
        .map(|_| id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn pty_write(id: String, data: String, state: TauriState<AppState>) {
    state.ptys.write(&id, data.as_bytes());
}

#[tauri::command]
fn pty_resize(id: String, cols: u16, rows: u16, state: TauriState<AppState>) {
    state.ptys.resize(&id, cols, rows);
}

#[tauri::command]
fn pty_kill(id: String, state: TauriState<AppState>) {
    state.ptys.kill(&id);
}

fn main() {
    tauri::Builder::default()
        .manage(AppState {
            config: ScanConfig::from_env(),
            archive: Mutex::new(State::load()),
            ptys: PtyManager::default(),
            new_counter: AtomicU64::new(0),
        })
        .invoke_handler(tauri::generate_handler![
            default_cwd,
            resolve_cwd,
            scan_sessions,
            set_archived,
            set_label,
            pty_start,
            pty_new,
            pty_write,
            pty_resize,
            pty_kill,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Tear down any live PTYs when the window closes.
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.ptys.kill_all();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running MindPlayer");
}
