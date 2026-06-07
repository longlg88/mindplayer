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
    /// Process-group id (== leader pid; the child ran `setsid`) captured at
    /// spawn, so the helper children (MCP / language servers) can still be
    /// signalled even after the leader has been reaped.
    pgid: Option<i32>,
    /// True once the group has been sent SIGTERM, so we never signal it twice
    /// (and never re-signal a possibly-recycled pgid late).
    group_signalled: bool,
    /// Memoized "looks like a waiting prompt" flag, recomputed by the reader
    /// thread on each output batch (it already holds the parser lock). The
    /// render/sort hot path reads this atomic instead of locking + allocating
    /// the whole screen on every frame.
    blocked: Arc<AtomicBool>,
    /// Memoized "the agent is busy working" flag (its TUI shows an interrupt
    /// hint / spinner), so a session waiting on a long subprocess with no new
    /// output is still classified Working rather than Idle.
    busy: Arc<AtomicBool>,
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
        // Capture the process-group id now, while the leader is definitely alive.
        let pgid = child.process_id().map(|p| p as i32);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 5_000)));
        let dirty = Arc::new(AtomicBool::new(true)); // draw the first frame
        let seq = Arc::new(AtomicU64::new(0));
        let blocked = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));

        {
            let parser = parser.clone();
            let dirty = dirty.clone();
            let seq = seq.clone();
            let blocked = blocked.clone();
            let busy = busy.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: child closed the PTY
                        Ok(n) => {
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                                // Recompute the screen heuristics while we hold
                                // the lock, so the UI hot path never has to.
                                let screen = p.screen().contents();
                                blocked.store(text_looks_blocked(&screen), Ordering::Relaxed);
                                busy.store(text_looks_busy(&screen), Ordering::Relaxed);
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
            pgid,
            group_signalled: false,
            blocked,
            busy,
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

    /// Heuristic: does the visible screen look like the child is waiting for the
    /// user at a confirm/approval prompt? Used to surface a "blocked" status
    /// (herdr-style rollup). Best-effort + tunable — see [`text_looks_blocked`].
    pub fn looks_blocked(&self) -> bool {
        self.blocked.load(Ordering::Relaxed)
    }

    /// Memoized: the agent's TUI shows it's actively working (an interrupt hint
    /// / spinner), so a session waiting on a long subprocess with no new output
    /// is still "working" rather than "idle". See [`text_looks_busy`].
    pub fn looks_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
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

    /// Does the child have xterm mouse reporting enabled? If so, the wheel/click
    /// should be forwarded to it (it scrolls its own full-screen view, e.g.
    /// codex) instead of moving MindPlayer's vt100 scrollback — which is empty
    /// for an alternate-screen app anyway.
    pub fn mouse_wanted(&self) -> bool {
        self.parser
            .lock()
            .map(|p| p.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None)
            .unwrap_or(false)
    }

    /// Forward a mouse event to the child, encoded per its negotiated protocol.
    /// `cb` = base button code (0/1/2 = left/middle/right, 64/65 = wheel up/down,
    /// 3 = none), `release` = button-up, `motion` = drag/move. `col`/`row` are
    /// 1-based and pane-relative. Honors the child's reporting mode (so we don't
    /// flood a press-only app with motion) and returns true if a sequence was
    /// written.
    pub fn forward_mouse(
        &mut self,
        cb: u16,
        release: bool,
        motion: bool,
        col: u16,
        row: u16,
    ) -> bool {
        let (mode, encoding) = match self.parser.lock() {
            Ok(p) => (
                p.screen().mouse_protocol_mode(),
                p.screen().mouse_protocol_encoding(),
            ),
            Err(_) => return false,
        };
        if !mouse_allowed(mode, release, motion, cb) {
            return false;
        }
        let bytes = mouse_sequence(encoding, cb, release, motion, col, row);
        let _ = self.writer.write_all(&bytes);
        let _ = self.writer.flush();
        true
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

    /// SIGTERM the child's process group exactly once. The child ran `setsid`
    /// (portable-pty pre_exec) so its pgid == its leader pid; signalling the
    /// group stops helper subprocesses (MCP / language servers) instead of
    /// orphaning them. We use the pgid captured at spawn (not `child.process_id`
    /// after a reap), and `group_signalled` ensures we never re-signal a
    /// pgid that may have been recycled once the whole group is gone. Safe to
    /// call right when the leader exits — survivors keep the pgid alive.
    pub fn signal_group(&mut self) {
        if self.group_signalled {
            return;
        }
        self.group_signalled = true;
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::killpg(pgid, libc::SIGTERM);
            }
        }
    }

    /// Terminate the child (used on close / app exit).
    fn terminate(&mut self) {
        // Always clean up the group once, even if the leader was already reaped
        // (otherwise its MCP/LSP children would be orphaned).
        self.signal_group();
        // Only touch the leader if we haven't already reaped it — std guards a
        // reaped child, but skipping avoids any chance of hitting a recycled PID.
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait(); // reap so we never leave a zombie
            self.exited = true;
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Never leave orphaned children or zombies behind.
        self.terminate();
    }
}

/// Whether a mouse event should be forwarded under the child's reporting mode.
/// `cb == 3` with `motion` means a plain move (no button held).
fn mouse_allowed(mode: vt100::MouseProtocolMode, release: bool, motion: bool, cb: u16) -> bool {
    use vt100::MouseProtocolMode as M;
    let plain_motion = motion && cb == 3;
    match mode {
        M::None => false,
        M::Press => !release && !motion,
        M::PressRelease => !motion,
        M::ButtonMotion => !plain_motion,
        M::AnyMotion => true,
    }
}

/// Encode a mouse event as the wire bytes for the child's negotiated encoding.
/// `cb` is the base button code; `col`/`row` are 1-based, pane-relative.
fn mouse_sequence(
    encoding: vt100::MouseProtocolEncoding,
    cb: u16,
    release: bool,
    motion: bool,
    col: u16,
    row: u16,
) -> Vec<u8> {
    match encoding {
        // SGR (1006): `ESC [ < Cb ; Cx ; Cy (M|m)`, M=press, m=release.
        vt100::MouseProtocolEncoding::Sgr => {
            let final_cb = cb + if motion { 32 } else { 0 };
            let end = if release { 'm' } else { 'M' };
            format!("\x1b[<{final_cb};{col};{row}{end}").into_bytes()
        }
        // X10 / UTF-8 default: `ESC [ M Cb Cx Cy`, each value offset by 32.
        _ => {
            let base = if cb >= 64 {
                cb // wheel codes already carry their own bit — no offsets
            } else if release {
                3 // X10 reports any button release as button 3
            } else {
                cb + if motion { 32 } else { 0 } // press / drag
            };
            let enc = |v: u16| -> u8 { v.saturating_add(32).min(255) as u8 };
            vec![0x1b, b'[', b'M', enc(base), enc(col), enc(row)]
        }
    }
}

/// Structural markers (lowercased) that strongly indicate a waiting prompt —
/// matched anywhere on the last few lines.
const BLOCKED_STRUCTURAL: &[&str] = &[
    "(y/n)",
    "[y/n]",
    "(y/n",
    "(yes/no)",
    "[yes/no]",
    "y/n)",
    "y/n]",
    "❯ 1.",
    "1. yes",
    "press enter to continue",
];

/// Confirm/approval asks — only count when the line is an actual question
/// (ends with `?`), to avoid flagging the same words in narration.
const BLOCKED_ASKS: &[&str] = &[
    "do you want",
    "do you wish",
    "would you like",
    "proceed",
    "continue",
    "confirm",
    "overwrite",
    "apply change",
    "allow",
];

/// True if the visible terminal text looks like the child is waiting at a
/// confirm/approval prompt. Conservative: matches structural prompt markers, or
/// a question line (`…?`) containing an approval verb — not bare words mid-output.
fn text_looks_blocked(screen: &str) -> bool {
    let tail: Vec<String> = screen
        .lines()
        .rev()
        .take(6)
        .map(|l| l.trim().to_lowercase())
        .collect();
    if tail
        .iter()
        .any(|l| BLOCKED_STRUCTURAL.iter().any(|m| l.contains(m)))
    {
        return true;
    }
    tail.iter()
        .any(|l| l.ends_with('?') && BLOCKED_ASKS.iter().any(|m| l.contains(m)))
}

/// Markers (lowercased) shown by codex/claude *while a turn is running* — most
/// reliably the interrupt hint, which disappears once the agent is idle at the
/// prompt. Used so a session waiting on a subprocess (no new output) still reads
/// as Working. Heuristic — tunable.
const BUSY_MARKERS: &[&str] = &[
    "esc to interrupt",
    "to interrupt",
    "ctrl-c to",
    "cogitat", // claude's "Cogitating…"
    "thinking…",
    "working…",
    "compacting",
    "still running",
];

/// True if the visible terminal text shows the agent is actively working.
fn text_looks_busy(screen: &str) -> bool {
    let tail = screen
        .lines()
        .rev()
        .take(6)
        .map(|l| l.trim().to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    BUSY_MARKERS.iter().any(|m| tail.contains(m))
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

    #[test]
    fn mouse_sequence_sgr_wheel_and_drag() {
        use vt100::MouseProtocolEncoding::Sgr;
        // wheel up at col 5, row 3 → press, no motion.
        assert_eq!(
            mouse_sequence(Sgr, 64, false, false, 5, 3),
            b"\x1b[<64;5;3M"
        );
        // wheel down.
        assert_eq!(
            mouse_sequence(Sgr, 65, false, false, 1, 1),
            b"\x1b[<65;1;1M"
        );
        // left release → lowercase m.
        assert_eq!(mouse_sequence(Sgr, 0, true, false, 2, 4), b"\x1b[<0;2;4m");
        // left drag → motion bit (+32) set.
        assert_eq!(mouse_sequence(Sgr, 0, false, true, 2, 4), b"\x1b[<32;2;4M");
    }

    #[test]
    fn mouse_sequence_x10_offsets_by_32() {
        use vt100::MouseProtocolEncoding::Default;
        // wheel up at col 1, row 1: ESC [ M, (64+32), (1+32), (1+32).
        assert_eq!(
            mouse_sequence(Default, 64, false, false, 1, 1),
            vec![0x1b, b'[', b'M', 96, 33, 33]
        );
        // release reported as button 3 (3+32 = 35).
        assert_eq!(
            mouse_sequence(Default, 0, true, false, 1, 1),
            vec![0x1b, b'[', b'M', 35, 33, 33]
        );
    }

    #[test]
    fn blocked_detection_matches_prompts() {
        assert!(text_looks_blocked(
            "running…\nDo you want to proceed? (y/n)"
        ));
        assert!(text_looks_blocked("Apply changes?\n  1. Yes\n  2. No"));
        assert!(text_looks_blocked("Continue?"));
        // Working / non-prompt screens must NOT be flagged.
        assert!(!text_looks_blocked("Thinking…  esc to interrupt"));
        assert!(!text_looks_blocked("wrote foo.rs\nall done"));
        assert!(!text_looks_blocked(""));
        // Narration containing approval words but no actual prompt: not blocked.
        assert!(!text_looks_blocked("I'll proceed to write the file now."));
        assert!(!text_looks_blocked("normalizing\n2. normalize the data"));
    }

    #[test]
    fn busy_detection_matches_working_indicators() {
        // A turn running a subprocess (no fresh output) still reads as busy.
        assert!(text_looks_busy(
            "✻ Cogitated for 7m 28s · 1 shell still running\n  esc to interrupt"
        ));
        assert!(text_looks_busy("Thinking…"));
        assert!(text_looks_busy("running build\n… (esc to interrupt)"));
        // Idle prompt / finished output is not busy.
        assert!(!text_looks_busy("› \ntype your message"));
        assert!(!text_looks_busy("wrote foo.rs\nall done"));
        assert!(!text_looks_busy(""));
    }

    #[test]
    fn mouse_allowed_respects_mode() {
        use vt100::MouseProtocolMode::*;
        // None forwards nothing.
        assert!(!mouse_allowed(None, false, false, 64));
        // Press: wheel/press yes, release/motion no.
        assert!(mouse_allowed(Press, false, false, 64));
        assert!(!mouse_allowed(Press, true, false, 0));
        assert!(!mouse_allowed(Press, false, true, 0));
        // PressRelease: release yes, motion no.
        assert!(mouse_allowed(PressRelease, true, false, 0));
        assert!(!mouse_allowed(PressRelease, false, true, 0));
        // ButtonMotion: drag (button held) yes, plain move (cb==3) no.
        assert!(mouse_allowed(ButtonMotion, false, true, 0));
        assert!(!mouse_allowed(ButtonMotion, false, true, 3));
        // AnyMotion: everything, incl. plain move.
        assert!(mouse_allowed(AnyMotion, false, true, 3));
    }
}
