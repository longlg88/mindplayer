//! Embedded PTY: spawn `codex`/`claude` in a pseudo-terminal, feed its output
//! into a `vt100` parser on a reader thread, and forward keystrokes to it.

use anyhow::Result;
use mindplayer_core::Command as MpCommand;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// A live child process attached to a PTY, rendered in the right pane.
pub struct PtySession {
    /// vt100 screen state, updated by the reader thread.
    parser: Arc<Mutex<vt100::Parser>>,
    /// Set by the reader thread when new output arrives, so the UI only redraws
    /// when the screen actually changed (instead of every frame).
    dirty: Arc<AtomicBool>,
    /// Monotonic count of output reads, bumped by the reader thread. The app
    /// polls this (without consuming `dirty`) to tell "working" sessions
    /// (producing output now) from idle/ended ones in the list.
    seq: Arc<AtomicU64>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Set once we observe the child has exited. `try_wait()` reaps the child on
    /// unix, freeing its PID for OS reuse; `portable_pty::Child::process_id()`
    /// keeps returning that stale PID unconditionally. Without this guard a
    /// later `terminate()` would `killpg()` a PID/PGID we no longer own — i.e.
    /// signal an unrelated process group. Once exited, terminate() is a no-op.
    exited: bool,
    pub rows: u16,
    pub cols: u16,
}

impl PtySession {
    pub fn spawn(cmd: &MpCommand, session_id: &str, rows: u16, cols: u16) -> Result<Self> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        // We must NOT hand the child's stderr to the same PTY as stdout.
        // codex (and other tracing-based CLIs) write logs to stderr; when that
        // shares the TUI PTY, codex aborts on its first write
        // (`assertion failed: output.write(&bytes).is_ok()`). portable-pty
        // always dups the slave onto all three std streams, so we wrap the
        // command in `sh -c 'exec PROG ARGS 2>>logfile'` to send stderr to a
        // per-session log instead (which also captures errors for diagnostics).
        let stderr_log = stderr_log_path(session_id);
        let mut shell_line = String::from("exec ");
        shell_line.push_str(&shell_quote(&cmd.program));
        for arg in &cmd.args {
            shell_line.push(' ');
            shell_line.push_str(&shell_quote(arg));
        }
        shell_line.push_str(" 2>>");
        shell_line.push_str(&shell_quote(&stderr_log));

        let mut builder = CommandBuilder::new("sh");
        builder.arg("-c");
        builder.arg(&shell_line);
        if !cmd.cwd.as_os_str().is_empty() {
            builder.cwd(&cmd.cwd);
        }
        builder.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(builder)?;
        // Slave handle no longer needed in the parent; closing it lets the
        // child own the terminal cleanly.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 5_000)));
        let dirty = Arc::new(AtomicBool::new(true)); // draw the first frame
        let seq = Arc::new(AtomicU64::new(0));

        {
            let parser = parser.clone();
            let dirty = dirty.clone();
            let seq = seq.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: child closed the PTY
                        Ok(n) => {
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                            dirty.store(true, Ordering::Relaxed);
                            seq.fetch_add(1, Ordering::Relaxed);
                        }
                        // EINTR is transient (e.g. a signal); keep reading.
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            });
        }

        Ok(Self {
            parser,
            dirty,
            seq,
            master: pair.master,
            writer,
            child,
            exited: false,
            rows,
            cols,
        })
    }

    /// Returns true (and resets) if the screen changed since the last check.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    /// Monotonic output-read count (never reset). The app diffs this against a
    /// remembered value to detect a session that is actively producing output.
    pub fn output_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    pub fn parser(&self) -> &Arc<Mutex<vt100::Parser>> {
        &self.parser
    }

    /// Scroll the rendered viewport through this session's scrollback. Positive
    /// `delta` shows older lines (wheel up); clamped to available history.
    /// Returns true if the visible offset actually changed.
    pub fn scroll_by(&self, delta: isize) -> bool {
        if let Ok(mut p) = self.parser.lock() {
            let cur = p.screen().scrollback() as isize;
            let next = (cur + delta).max(0) as usize;
            p.set_scrollback(next);
            return p.screen().scrollback() != cur as usize;
        }
        false
    }

    /// Jump back to the live bottom of the scrollback.
    pub fn scroll_reset(&self) {
        if let Ok(mut p) = self.parser.lock() {
            p.set_scrollback(0);
        }
    }

    /// Forward raw bytes (encoded keystrokes) to the child. Typing jumps the
    /// view back to the live bottom (like a normal terminal).
    pub fn send(&mut self, bytes: &[u8]) {
        self.scroll_reset();
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Forward pasted text to the child. If the child has enabled bracketed
    /// paste (DECSET 2004 — codex/claude do), wrap it in `ESC[200~`/`ESC[201~`
    /// so multi-line content is inserted literally instead of each newline
    /// executing as Enter. Otherwise send it raw (markers would leak as text).
    pub fn paste(&mut self, text: &str) {
        self.scroll_reset();
        let bracketed = self
            .parser
            .lock()
            .map(|p| p.screen().bracketed_paste())
            .unwrap_or(false);
        if bracketed {
            let _ = self.writer.write_all(b"\x1b[200~");
            let _ = self.writer.write_all(text.as_bytes());
            let _ = self.writer.write_all(b"\x1b[201~");
        } else {
            let _ = self.writer.write_all(text.as_bytes());
        }
        let _ = self.writer.flush();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    /// True while the child process is still running. Records the exit so a
    /// later `terminate()` never signals the (now reaped, possibly recycled) PID.
    pub fn is_alive(&mut self) -> bool {
        if self.exited {
            return false;
        }
        match self.child.try_wait() {
            Ok(None) => true,
            _ => {
                // Ok(Some(status)) reaps the child; Err means we can't tell, but
                // either way we must not killpg() its PID afterwards.
                self.exited = true;
                false
            }
        }
    }

    /// Terminate the child (used on close / app exit).
    pub fn kill(&mut self) {
        self.terminate();
    }

    /// Signal the whole process group, then SIGKILL the leader and reap it.
    /// The child ran `setsid` (portable-pty pre_exec), so its pgid == its pid;
    /// signalling the group also stops codex/claude helper subprocesses
    /// (MCP / language servers) instead of orphaning them.
    fn terminate(&mut self) {
        // If the child was already observed dead (and reaped by try_wait), its
        // PID may have been recycled by the OS — signalling it now could hit an
        // unrelated process group. Skip all signalling in that case.
        if self.exited {
            return;
        }
        if let Some(pid) = self.child.process_id() {
            unsafe {
                libc::killpg(pid as i32, libc::SIGTERM);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait(); // reap so we never leave a zombie
        self.exited = true;
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Never leave orphaned children or zombies behind.
        self.terminate();
    }
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

/// Per-session stderr log under `~/.mindplayer/logs/`. The log can capture
/// whatever codex/claude write to stderr (potentially sensitive), so the dir is
/// created `0700` and the file pre-created `0600` before the shell appends to it
/// — never world-/group-readable on a shared machine.
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
    // Create with 0600 if absent; tighten perms on a pre-existing log too.
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

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("codex"), "'codex'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn stderr_log_path_sanitizes_id() {
        let p = stderr_log_path("new:codex");
        assert!(p.ends_with("new_codex.stderr.log"), "got {p}");
        let p2 = stderr_log_path("11111111-2222-7333");
        assert!(p2.ends_with("11111111-2222-7333.stderr.log"), "got {p2}");
    }
}
