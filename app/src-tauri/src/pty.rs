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
        let child = pair.slave.spawn_command(command_builder(cmd, &log))?;
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

fn command_builder(cmd: &MpCommand, log: &str) -> CommandBuilder {
    let mut line = String::from("exec ");
    line.push_str(&shell_quote(&cmd.program));
    for arg in &cmd.args {
        line.push(' ');
        line.push_str(&shell_quote(arg));
    }
    line.push_str(" 2>>");
    line.push_str(&shell_quote(log));

    let mut builder = CommandBuilder::new("sh");
    builder.arg("-c");
    builder.arg(&line);
    if !cmd.cwd.as_os_str().is_empty() {
        builder.cwd(&cmd.cwd);
    }
    builder.env("TERM", "xterm-256color");
    // Clear ambient values first so explicit selections take precedence.
    for name in &cmd.env.unset {
        builder.env_remove(name);
    }
    for (name, value) in &cmd.env.set {
        builder.env(name, value);
    }
    builder
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

#[cfg(test)]
mod tests {
    use super::*;
    use mindplayer_core::accounts::LaunchEnv;
    use std::path::PathBuf;
    use std::process::Command;

    // Seed inheritance in a separate test process, avoiding global env changes
    // that could race other tests or affect the user's homes and credentials.
    fn inherited_env_test(name: &str) -> anyhow::Result<bool> {
        if std::env::var("PTY_ENV_TEST_CHILD").as_deref() == Ok(name) {
            return Ok(true);
        }
        let output = Command::new(std::env::current_exe()?)
            .args(["--exact", name, "--nocapture"])
            .env("PTY_ENV_TEST_CHILD", name)
            .env("PTY_TEST_REMOVE", "ambient")
            .env("PTY_TEST_OVERRIDE", "ambient")
            .env("PTY_TEST_BOTH", "ambient")
            .env("PTY_TEST_KEEP", "inherited")
            .env_remove("PTY_TEST_EMPTY")
            .output()?;
        assert!(
            output.status.success(),
            "PTY regression subprocess failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        Ok(false)
    }

    fn pty_output(cmd: &MpCommand) -> anyhow::Result<String> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut reader = pair.master.try_clone_reader()?;
        let mut child = pair
            .slave
            .spawn_command(command_builder(cmd, "/dev/null"))?;
        drop(pair.slave);
        let mut output = String::new();
        let read = reader.read_to_string(&mut output);
        if read.is_err() {
            let _ = child.kill();
        }
        // Drain before waiting: PTY output can hold up child exit on macOS.
        // Reap before assertions, including the RED run and read failures.
        let status = child.wait()?;
        read?;
        assert!(status.success(), "PTY child failed: {status:?}: {output}");
        Ok(output.replace("\r\n", "\n"))
    }

    #[test]
    fn launch_env_unsets_inherited_values() -> anyhow::Result<()> {
        if !inherited_env_test("pty::tests::launch_env_unsets_inherited_values")? {
            return Ok(());
        }
        let cmd = MpCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '<%s><%s><%s>' \"${PTY_TEST_REMOVE+present}\" \"$PTY_TEST_KEEP\" \"$TERM\""
                    .into(),
            ],
            cwd: PathBuf::new(),
            env: LaunchEnv {
                unset: vec!["PTY_TEST_REMOVE".into()],
                ..LaunchEnv::default()
            },
        };
        assert_eq!(pty_output(&cmd)?, "<><inherited><xterm-256color>");
        Ok(())
    }

    #[test]
    fn launch_env_sets_values_after_unset() -> anyhow::Result<()> {
        if !inherited_env_test("pty::tests::launch_env_sets_values_after_unset")? {
            return Ok(());
        }
        let cmd = MpCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '<%s><%s><%s><%s><%s>' \"$PTY_TEST_OVERRIDE\" \"$PTY_TEST_BOTH\" \"${PTY_TEST_EMPTY+present}\" \"$PTY_TEST_EMPTY\" \"$TERM\"".into(),
            ],
            cwd: PathBuf::new(),
            env: LaunchEnv {
                unset: vec!["PTY_TEST_BOTH".into()],
                set: vec![
                    ("PTY_TEST_OVERRIDE".into(), "selected ' $value".into()),
                    ("PTY_TEST_BOTH".into(), "selected".into()),
                    ("PTY_TEST_EMPTY".into(), String::new()),
                    ("TERM".into(), "screen-256color".into()),
                ],
            },
        };
        assert_eq!(
            pty_output(&cmd)?,
            "<selected ' $value><selected><present><><screen-256color>"
        );
        Ok(())
    }

    #[test]
    fn launch_env_preserves_cwd_args_and_base_term() -> anyhow::Result<()> {
        let cwd = std::env::temp_dir().canonicalize()?;
        let cmd = MpCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "test -t 0 && test -t 1 && ! test -t 2 || exit 1; test \"$(pwd -P)\" = \"$1\" || exit 2; printf '<%s><%s><%s>' \"$TERM\" \"$2\" \"$3\"; printf 'stderr-only' >&2".into(),
                "pty-test".into(),
                cwd.to_string_lossy().into_owned(),
                "spaces ' quotes $() ; *\nnext line".into(),
                String::new(),
            ],
            cwd,
            env: LaunchEnv::default(),
        };
        assert_eq!(
            pty_output(&cmd)?,
            "<xterm-256color><spaces ' quotes $() ; *\nnext line><>"
        );
        Ok(())
    }
}
