//! Decouples on-screen rendering from the event loop.
//!
//! `Terminal::draw` normally writes straight to stdout, so a slow reader on
//! the other end (a terminal emulator that has fallen behind under system
//! load, or a scrolled-back / suspended window) makes that `write(2)` block —
//! and since the event loop calls `draw` before polling input, the whole app
//! stops responding to keys until the write unblocks. That's indistinguishable
//! from a hang or crash from the user's side, even though the process is
//! alive and idle.
//!
//! [`FrameSink`] plugs into `CrosstermBackend` in place of `Stdout`: writes
//! only touch an in-memory buffer, and `flush()` (which `Terminal::draw`
//! calls once per frame) hands that buffer to a mailbox. A dedicated thread
//! owns the real stdout and does the actual blocking write. If it falls
//! behind, only that thread stalls: the event loop keeps polling and
//! handling input, and pending frames queue up in the mailbox.
//!
//! Frames are *coalesced*, never dropped: each `flush()` appends its bytes
//! to whatever is already pending, rather than replacing it. `Terminal::draw`
//! only ever writes the diff between its last-rendered buffer and the
//! current one, on the assumption that every prior diff actually reached the
//! terminal — dropping a frame's bytes (keeping only the "latest") would
//! permanently desync the real terminal from ratatui's model of it, and
//! every later diff would compound that corruption. Concatenating instead
//! just delays delivery; once the writer thread catches up, the terminal
//! ends up in the correct final state.

use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long `drain()` waits for the writer thread to catch up before giving
/// up. Bounded so a wedged terminal (the exact case this module exists for)
/// can never hang process exit forever — best-effort, matching the rest of
/// teardown.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(300);

struct MailboxState {
    pending: Vec<u8>,
    /// True from the moment the writer thread takes a frame out of `pending`
    /// until its OS write finishes (or fails). `pending.is_empty()` alone is
    /// NOT enough to mean "fully written" — the thread clears `pending` before
    /// the write happens, so there's a window where nothing is queued but the
    /// previous frame hasn't actually reached the terminal yet.
    writing: bool,
}

struct Mailbox {
    state: Mutex<MailboxState>,
    cvar: Condvar,
}

#[derive(Clone)]
pub(crate) struct RenderWriter {
    mailbox: Arc<Mailbox>,
}

impl RenderWriter {
    /// Takes ownership of the real stdout handle — from here on only the
    /// background thread writes to it.
    fn spawn(mut stdout: impl Write + Send + 'static) -> Self {
        let mailbox = Arc::new(Mailbox {
            state: Mutex::new(MailboxState {
                pending: Vec::new(),
                writing: false,
            }),
            cvar: Condvar::new(),
        });
        let worker = Arc::clone(&mailbox);
        // Detached by design: the process exits right after the event loop
        // returns, so there's nothing to join — `drain()` is how teardown
        // waits for it instead, without risking a join that never returns.
        std::thread::Builder::new()
            .name("mindplayer-render-writer".into())
            .spawn(move || loop {
                let frame = {
                    let mut guard = worker.state.lock().unwrap();
                    while guard.pending.is_empty() {
                        guard = worker.cvar.wait(guard).unwrap();
                    }
                    guard.writing = true;
                    std::mem::take(&mut guard.pending)
                };
                let ok = stdout.write_all(&frame).is_ok() && stdout.flush().is_ok();
                {
                    // Cleared (and drain() woken) on the error path too —
                    // otherwise a dead writer thread would leave `writing`
                    // stuck true forever and drain() would hang until its
                    // timeout on every single call for the rest of the run.
                    let mut guard = worker.state.lock().unwrap();
                    guard.writing = false;
                    worker.cvar.notify_all();
                }
                if !ok {
                    return;
                }
            })
            .expect("failed to spawn render-writer thread");
        RenderWriter { mailbox }
    }

    /// Hands off a frame. Never blocks: this only ever appends under a brief
    /// lock. If the writer thread is still busy with a previous write, the
    /// new bytes queue up behind it rather than replacing it — see the
    /// module docs for why replacing would corrupt the terminal.
    fn send(&self, mut frame: Vec<u8>) {
        if frame.is_empty() {
            return;
        }
        let mut guard = self.mailbox.state.lock().unwrap();
        if guard.pending.is_empty() {
            guard.pending = frame;
        } else {
            guard.pending.append(&mut frame);
        }
        self.mailbox.cvar.notify_one();
    }

    /// Blocks until every frame handed to `send()` so far has actually
    /// reached the terminal (or the writer thread has died trying), up to
    /// [`DRAIN_TIMEOUT`]. Call this *before* writing anything to stdout
    /// through a separate handle (as `restore_terminal()` does) — otherwise
    /// the cleanup escape codes can race a still-in-flight frame and leave
    /// the terminal in a corrupted state right as the user quits, exactly
    /// the scenario this module exists to protect against mid-session.
    pub(crate) fn drain(&self) -> bool {
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        let mut guard = self.mailbox.state.lock().unwrap();
        while !guard.pending.is_empty() || guard.writing {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next_guard, _timeout) = self.mailbox.cvar.wait_timeout(guard, remaining).unwrap();
            guard = next_guard;
        }
        // Re-checked directly rather than trusted from `wait_timeout`'s own
        // timed-out flag: a spurious wakeup right at the deadline could
        // otherwise report "timed out" even though the state had actually
        // caught up in the same instant.
        guard.pending.is_empty() && !guard.writing
    }
}

/// `CrosstermBackend<FrameSink>` replaces `CrosstermBackend<Stdout>`: same
/// escape-code output, but routed through the mailbox above instead of
/// straight to the terminal.
pub struct FrameSink {
    buf: Vec<u8>,
    writer: RenderWriter,
}

impl FrameSink {
    pub fn spawn(stdout: impl Write + Send + 'static) -> Self {
        let writer = RenderWriter::spawn(stdout);
        // Stashed globally (rather than returned as a handle for the caller
        // to thread through) so `drain_global` below works from *both*
        // `teardown()` and the panic hook without changing either's
        // signature — a panic can unwind through arbitrary call frames, so
        // there's no parameter path that would reach it otherwise. Only ever
        // set once, from here, so a stale/second handle is not a concern.
        let _ = GLOBAL_WRITER.set(writer.clone());
        FrameSink {
            buf: Vec::new(),
            writer,
        }
    }
}

static GLOBAL_WRITER: std::sync::OnceLock<RenderWriter> = std::sync::OnceLock::new();

/// Best-effort: if a render-writer thread is active, block (up to
/// [`DRAIN_TIMEOUT`]) until it has flushed everything already handed to it.
/// Call this before writing to stdout through any other handle — see
/// `RenderWriter::drain` for why. A safe no-op if `FrameSink::spawn` never
/// ran (e.g. a panic during `--help`/`--version` handling, before the
/// terminal is even set up).
pub(crate) fn drain_global() {
    if let Some(writer) = GLOBAL_WRITER.get() {
        writer.drain();
    }
}

impl Write for FrameSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// `Terminal::draw` calls this once per frame after writing all of the
    /// frame's escape codes — the natural point to hand the whole frame off.
    fn flush(&mut self) -> io::Result<()> {
        self.writer.send(std::mem::take(&mut self.buf));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone, Default)]
    struct RecordingSink(Arc<Mutex<Vec<u8>>>);

    impl Write for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn wait_for(recorded: &Arc<Mutex<Vec<u8>>>, expected: &[u8]) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if recorded.lock().unwrap().as_slice() == expected {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn write_only_buffers_flush_hands_the_frame_to_the_writer_thread() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut sink = FrameSink::spawn(RecordingSink(Arc::clone(&recorded)));

        sink.write_all(b"hello frame").unwrap();
        // Nothing reaches the "terminal" until flush — draw()'s blocking
        // write on the real fd is exactly what this module exists to avoid.
        assert!(recorded.lock().unwrap().is_empty());

        sink.flush().unwrap();
        assert!(wait_for(&recorded, b"hello frame"));
    }

    #[derive(Clone, Default)]
    struct GatedSink {
        recorded: Arc<Mutex<Vec<u8>>>,
        started: Arc<(Mutex<bool>, Condvar)>,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Write for GatedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            {
                let (lock, cvar) = &*self.started;
                *lock.lock().unwrap() = true;
                cvar.notify_all();
            }
            {
                let (lock, cvar) = &*self.gate;
                let mut opened = lock.lock().unwrap();
                while !*opened {
                    opened = cvar.wait(opened).unwrap();
                }
            }
            self.recorded.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Regression test for the corruption this design previously caused:
    /// `Terminal::draw` writes are diffs against ratatui's belief of what's
    /// on screen, so dropping a queued frame permanently desyncs the real
    /// terminal from that belief — every later diff then compounds the
    /// corruption. Two frames sent while the writer thread is stuck on a
    /// prior write must both still reach the terminal, in order, once it
    /// unblocks — never just the latest one.
    #[test]
    fn pending_frames_are_coalesced_not_dropped_while_writer_is_busy() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let sink = GatedSink {
            recorded: Arc::clone(&recorded),
            started: Arc::clone(&started),
            gate: Arc::clone(&gate),
        };
        let mut frame_sink = FrameSink::spawn(sink);

        frame_sink.write_all(b"frame1").unwrap();
        frame_sink.flush().unwrap();

        // Wait for the writer thread to actually be blocked inside the
        // "slow terminal" write of frame1 before queuing more frames.
        {
            let (lock, cvar) = &*started;
            let mut s = lock.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            while !*s && Instant::now() < deadline {
                let (guard, _) = cvar.wait_timeout(s, Duration::from_millis(50)).unwrap();
                s = guard;
            }
            assert!(*s, "writer thread never entered the blocking write");
        }

        // Both of these must queue up rather than the second overwriting
        // the first — that's the bug this test guards against.
        frame_sink.write_all(b"frame2").unwrap();
        frame_sink.flush().unwrap();
        frame_sink.write_all(b"frame3").unwrap();
        frame_sink.flush().unwrap();

        {
            let (lock, cvar) = &*gate;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }

        assert!(wait_for(&recorded, b"frame1frame2frame3"));
    }

    #[test]
    fn drain_waits_for_an_in_flight_write_before_returning() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let sink = GatedSink {
            recorded: Arc::clone(&recorded),
            started: Arc::clone(&started),
            gate: Arc::clone(&gate),
        };
        let mut frame_sink = FrameSink::spawn(sink);
        frame_sink.write_all(b"frame1").unwrap();
        frame_sink.flush().unwrap();

        {
            let (lock, cvar) = &*started;
            let mut s = lock.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            while !*s && Instant::now() < deadline {
                let (guard, _) = cvar.wait_timeout(s, Duration::from_millis(50)).unwrap();
                s = guard;
            }
            assert!(*s, "writer thread never entered the blocking write");
        }

        // drain() is called from another thread since it must actually
        // block here — the gate isn't open yet.
        let writer = frame_sink.writer.clone();
        let drained = std::thread::spawn(move || writer.drain());

        // Give drain() a moment to actually start waiting, then confirm it
        // hasn't returned early while the write is still gated shut.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !drained.is_finished(),
            "drain() returned before the write finished"
        );

        {
            let (lock, cvar) = &*gate;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }

        assert!(
            drained.join().unwrap(),
            "drain() should report success once the writer catches up"
        );
        assert_eq!(*recorded.lock().unwrap(), b"frame1");
    }

    #[test]
    fn drain_gives_up_after_its_timeout_instead_of_hanging_forever() {
        // A sink that blocks forever (never ungated) — the exact "wedged
        // terminal" case this module exists for. drain() must still return
        // (with `false`, meaning it gave up) rather than hang teardown.
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let never_opens = Arc::new((Mutex::new(false), Condvar::new()));
        let sink = GatedSink {
            recorded,
            started,
            gate: never_opens,
        };
        let mut frame_sink = FrameSink::spawn(sink);
        frame_sink.write_all(b"stuck").unwrap();
        frame_sink.flush().unwrap();

        let before = Instant::now();
        let drained = frame_sink.writer.drain();
        assert!(!drained);
        assert!(
            before.elapsed() < Duration::from_secs(2),
            "drain() should give up around DRAIN_TIMEOUT, not hang"
        );
    }
}
