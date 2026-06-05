//! PTY bridge for the Tauri app: spawn codex/claude in a pseudo-terminal and
//! stream raw bytes to the webview (xterm.js renders them). Mirrors the TUI's
//! critical fix — the child's stderr is redirected to a log file so it never
//! shares the TUI PTY (codex aborts otherwise).

use base64::Engine;
use mindplayer_core::Command as MpCommand;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Set once the child has been signalled/reaped, so a second `terminate()`
    /// never `killpg()`s a PID that may have been recycled by the OS.
    exited: bool,
}

/// Output event payload: which session, and base64-encoded bytes.
#[derive(Clone, serde::Serialize)]
struct Output {
    id: String,
    /// Base64 of the raw PTY bytes; the frontend decodes to a Uint8Array.
    b64: String,
}

/// Lifecycle event payload (e.g. the child exited).
#[derive(Clone, serde::Serialize)]
struct Lifecycle {
    id: String,
    event: String,
}

#[derive(Default)]
pub struct PtyManager {
    handles: Mutex<HashMap<String, PtyHandle>>,
}

impl PtyManager {
    /// Spawn (replacing any existing PTY for `id`) and stream output.
    pub fn start(
        &self,
        app: &AppHandle,
        id: &str,
        cmd: &MpCommand,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        // Replace an existing session with the same id.
        self.kill(id);

        let cols = cols.max(1);
        let rows = rows.max(1);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        // Redirect stderr to a per-session log so it never shares the TUI PTY.
        let log = stderr_log_path(id);
        let mut line = String::from("exec ");
        line.push_str(&shell_quote(&cmd.program));
        for arg in &cmd.args {
            line.push(' ');
            line.push_str(&shell_quote(arg));
        }
        line.push_str(" 2>>");
        line.push_str(&shell_quote(&log));

        let mut builder = CommandBuilder::new("sh");
        builder.arg("-c");
        builder.arg(&line);
        if !cmd.cwd.as_os_str().is_empty() {
            builder.cwd(&cmd.cwd);
        }
        builder.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // Reader thread: forward bytes to the webview as base64.
        {
            let app = app.clone();
            let id = id.to_string();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                            let _ = app.emit(
                                "pty://output",
                                Output {
                                    id: id.clone(),
                                    b64,
                                },
                            );
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = app.emit(
                    "pty://exit",
                    Lifecycle {
                        id: id.clone(),
                        event: "exit".into(),
                    },
                );
            });
        }

        self.handles.lock().unwrap().insert(
            id.to_string(),
            PtyHandle {
                master: pair.master,
                writer,
                child,
                exited: false,
            },
        );
        Ok(())
    }

    pub fn write(&self, id: &str, bytes: &[u8]) {
        if let Some(h) = self.handles.lock().unwrap().get_mut(id) {
            let _ = h.writer.write_all(bytes);
            let _ = h.writer.flush();
        }
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) {
        if let Some(h) = self.handles.lock().unwrap().get(id) {
            let _ = h.master.resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn kill(&self, id: &str) {
        if let Some(mut h) = self.handles.lock().unwrap().remove(id) {
            terminate(&mut h);
        }
    }

    pub fn kill_all(&self) {
        let mut map = self.handles.lock().unwrap();
        for (_, mut h) in map.drain() {
            terminate(&mut h);
        }
    }
}

/// Signal the child's process group, then SIGKILL the leader and reap it.
/// The child ran `setsid` (portable-pty), so signalling the group also stops
/// codex/claude helper subprocesses rather than orphaning them.
fn terminate(h: &mut PtyHandle) {
    // Already signalled/reaped: the PID may have been recycled, so signalling it
    // again could hit an unrelated process group. No-op.
    if h.exited {
        return;
    }
    if let Some(pid) = h.child.process_id() {
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
    }
    let _ = h.child.kill();
    let _ = h.child.wait();
    h.exited = true;
}

/// POSIX single-quote a token so it is safe inside `sh -c`.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn stderr_log_path(session_id: &str) -> String {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = std::path::Path::new(&home).join(".mindplayer").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.stderr.log"));
    // stderr may contain sensitive output; keep the log owner-only (0600).
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path);
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    path.to_string_lossy().into_owned()
}
