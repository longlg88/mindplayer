/// A parser for terminal output which produces an in-memory representation of
/// the terminal contents.
pub struct Parser {
    parser: vte::Parser,
    screen: crate::screen::Screen,
    synchronized_frame: Option<crate::screen::Screen>,
}

// Recovery policy, not a protocol deadline: a missing end marker must not
// freeze an embedded pane indefinitely. Repeated begin markers extend it.
const SYNCHRONIZED_UPDATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

impl Parser {
    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback.
    #[must_use]
    pub fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        Self {
            parser: vte::Parser::new(),
            screen: crate::screen::Screen::new(crate::grid::Size { rows, cols }, scrollback_len),
            synchronized_frame: None,
        }
    }

    /// Processes the contents of the given byte string, and updates the
    /// in-memory terminal state.
    pub fn process(&mut self, bytes: &[u8]) {
        self.expire_synchronized_update();
        for byte in bytes {
            self.parser.advance(&mut self.screen, *byte);
            if self.screen.synchronized_update_start().is_some() {
                if self.synchronized_frame.is_none() {
                    self.synchronized_frame = Some(self.screen.presentation_snapshot());
                }
            } else {
                self.synchronized_frame = None;
            }
        }
    }

    /// Drains terminal replies generated while processing child output.
    ///
    /// Replies capture the live child cursor at query time, independently of
    /// presentation snapshots. Write these bytes to the child PTY after
    /// releasing the parser lock. Pending replies are capped at 32 KiB;
    /// overflow drops whole later replies until the queue is drained.
    pub fn take_terminal_replies(&mut self) -> Vec<u8> {
        self.screen.take_terminal_replies()
    }

    /// Resizes the terminal.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        // Resizing may terminate synchronization early; never present a
        // snapshot whose cell addresses belong to the previous geometry.
        self.end_synchronized_update();
        self.screen.set_size(rows, cols);
    }

    /// Scrolls to the given position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and
    /// should be `0` to put the normal screen in view.
    ///
    /// This affects the return values of methods called on `parser.screen()`:
    /// for instance, `parser.screen().cell(0, 0)` will return the top left
    /// corner of the screen after taking the scrollback offset into account.
    /// It does not affect `parser.process()` at all.
    ///
    /// The value given will be clamped to the actual size of the scrollback.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.end_synchronized_update();
        self.screen.set_scrollback(rows);
    }

    /// Returns a reference to a `Screen` object containing the terminal
    /// state.
    #[must_use]
    pub fn screen(&self) -> &crate::screen::Screen {
        &self.screen
    }

    /// Returns a coherent presentation while a synchronized update is in
    /// progress. Parsing and input-mode queries still use the live `screen()`.
    /// A missing end marker releases presentation after a bounded timeout.
    #[must_use]
    pub fn render_screen(&self) -> &crate::screen::Screen {
        self.render_screen_at(std::time::Instant::now())
    }

    fn render_screen_at(&self, now: std::time::Instant) -> &crate::screen::Screen {
        if self
            .screen
            .synchronized_update_start()
            .is_some_and(|start| now.saturating_duration_since(start) < SYNCHRONIZED_UPDATE_TIMEOUT)
        {
            self.synchronized_frame.as_ref().unwrap_or(&self.screen)
        } else {
            &self.screen
        }
    }

    /// Releases a held presentation, for example when the output stream ends.
    pub fn end_synchronized_update(&mut self) {
        self.screen.end_synchronized_update();
        self.synchronized_frame = None;
    }

    /// Whether presentation is held, including a frame awaiting timeout recovery.
    #[must_use]
    pub fn synchronized_update_pending(&self) -> bool {
        self.synchronized_frame.is_some()
    }

    /// Releases an expired frame and reports whether a redraw is required.
    pub fn expire_synchronized_update(&mut self) -> bool {
        if self
            .screen
            .synchronized_update_start()
            .is_some_and(|start| start.elapsed() >= SYNCHRONIZED_UPDATE_TIMEOUT)
        {
            self.end_synchronized_update();
            true
        } else {
            false
        }
    }
}

impl Default for Parser {
    /// Returns a parser with dimensions 80x24 and no scrollback.
    fn default() -> Self {
        Self::new(24, 80, 0)
    }
}

impl std::io::Write for Parser {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod terminal_reply_tests {
    use super::Parser;

    #[test]
    fn cpr_captures_each_query_position_and_drains_once() {
        let mut parser = Parser::new(4, 12, 0);
        assert!(parser.take_terminal_replies().is_empty());
        parser.process(b"\x1b[2;3H\x1b[6n\x1b[4;11H\x1b[6n\x1b[H");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[2;3R\x1b[4;11R");
        assert_eq!(parser.screen().cursor_position(), (0, 0));
        assert!(parser.take_terminal_replies().is_empty());
    }

    #[test]
    fn cpr_waits_for_the_final_byte_across_every_split() {
        let query = b"\x1b[6n";
        for split in 0..query.len() {
            let mut parser = Parser::new(4, 12, 0);
            parser.process(b"\x1b[3;7H");
            parser.process(&query[..split]);
            assert!(parser.take_terminal_replies().is_empty());
            parser.process(&query[split..]);
            assert_eq!(parser.take_terminal_replies(), b"\x1b[3;7R");
        }
        let mut parser = Parser::new(4, 12, 0);
        for byte in b"\x1b[6" {
            parser.process(&[*byte]);
            assert!(parser.take_terminal_replies().is_empty());
        }
        parser.process(b"n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[1;1R");
    }

    #[test]
    fn cpr_ignores_other_queries_parameters_and_string_payloads() {
        let mut parser = Parser::new(4, 12, 0);
        for input in [
            b"\x1b[5n".as_slice(),
            b"\x1b[?6n",
            b"\x1b[n",
            b"\x1b[0n",
            b"\x1b[6;1n",
            b"\x1b[6;n",
            b"\x1b[;6n",
            b"\x1b[6:1n",
            b"\x1b[6:0n",
            b"\x1b[6$n",
            b"\x1b[6\x18n",
            b"\x1b]2;payload [6n\x07",
            b"\x1bPqpayload [6n\x1b\\",
        ] {
            parser.process(input);
            assert!(parser.take_terminal_replies().is_empty(), "{input:?}");
        }
        // Overflowed parameter storage must not turn a malformed CSI into DSR.
        let mut malformed = b"\x1b[6".to_vec();
        malformed.extend_from_slice(&b";0".repeat(64));
        malformed.push(b'n');
        parser.process(&malformed);
        assert!(parser.take_terminal_replies().is_empty());
        parser.process(b"\x1b[H\x1b[6n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[1;1R");
    }

    #[test]
    fn cpr_uses_live_state_not_the_synchronized_snapshot() {
        let mut parser = Parser::new(4, 12, 0);
        parser.process(b"\x1b[2;3H\x1b[6n\x1b[?2026h\x1b[4;9H\x1b[6n");
        assert_eq!(parser.render_screen().cursor_position(), (1, 2));
        assert_eq!(parser.screen().cursor_position(), (3, 8));
        assert!(parser
            .synchronized_frame
            .as_mut()
            .unwrap()
            .take_terminal_replies()
            .is_empty());
        assert_eq!(parser.take_terminal_replies(), b"\x1b[2;3R\x1b[4;9R");
        parser.process(b"\x1b[?2026l");
        assert!(parser.take_terminal_replies().is_empty());
    }

    #[test]
    fn cpr_uses_active_grid_and_ignores_user_scrollback() {
        let mut parser = Parser::new(3, 12, 10);
        parser.process(b"history\r\nfirst\r\nsecond\r\nthird\x1b[2;4H");
        parser.set_scrollback(1);
        assert_eq!(parser.screen().scrollback(), 1);
        parser.process(b"\x1b[6n\x1b[?1049h\x1b[3;8H\x1b[6n\x1b[?1049l\x1b[6n");
        assert_eq!(
            parser.take_terminal_replies(),
            b"\x1b[2;4R\x1b[3;8R\x1b[2;4R"
        );
    }

    #[test]
    fn cpr_survives_resize_and_reports_the_new_geometry() {
        let mut parser = Parser::new(4, 12, 0);
        parser.process(b"\x1b[4;11H\x1b[6n\x1b[6");
        parser.set_size(2, 6);
        let cursor = parser.screen().cursor_position();
        parser.process(b"n");
        let expected = format!("\x1b[4;11R\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1);
        assert_eq!(parser.take_terminal_replies(), expected.as_bytes());
    }

    #[test]
    fn cpr_clamps_pending_wrap_and_wide_character_columns() {
        let mut parser = Parser::new(2, 3, 0);
        parser.process(b"abc\x1b[6n");
        assert_eq!(parser.screen().cursor_position(), (0, 3));
        parser.process(b"d\x1b[6n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[1;3R\x1b[2;2R");
        let mut wide = Parser::new(2, 2, 0);
        wide.process("한\x1b[6n".as_bytes());
        assert_eq!(wide.take_terminal_replies(), b"\x1b[1;2R");
    }

    #[test]
    fn cpr_preserves_queued_replies_across_terminal_reset() {
        let mut parser = Parser::new(4, 12, 0);
        parser.process(b"\x1b[3;7H\x1b[6n\x1bc\x1b[6n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[3;7R\x1b[1;1R");
    }

    #[test]
    fn cpr_bounds_whole_replies_across_chunks_and_recovers_after_drain() {
        let mut parser = Parser::new(4, 12, 0);
        parser.process(&b"\x1b[6n".repeat(4_000));
        parser.process(&b"\x1b[6n".repeat(4_000));
        let replies = parser.take_terminal_replies();
        let reply = b"\x1b[1;1R";
        assert_eq!(replies.len(), (32 * 1024 / reply.len()) * reply.len());
        assert!(replies
            .chunks_exact(reply.len())
            .all(|chunk| chunk == reply));
        assert!(parser.take_terminal_replies().is_empty());
        parser.process(b"\x1b[2;3H\x1b[6n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[2;3R");
    }

    #[test]
    fn cpr_fits_every_query_from_an_eight_kibibyte_pty_read() {
        let mut parser = Parser::new(1, u16::MAX, 0);
        parser.process(b"\x1b[1;65535H");
        parser.process(&b"\x1b[6n".repeat(8_192 / 4));
        let replies = parser.take_terminal_replies();
        let reply = b"\x1b[1;65535R";
        assert_eq!(replies.len(), reply.len() * (8_192 / 4));
        assert!(replies
            .chunks_exact(reply.len())
            .all(|chunk| chunk == reply));
    }

    #[test]
    fn cpr_uses_page_coordinates_unless_origin_mode_is_enabled() {
        let mut parser = Parser::new(8, 12, 0);
        parser.process(b"\x1b[3;6r\x1b[4;7H\x1b[6n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[4;7R");
        parser.process(b"\x1b[?6h\x1b[2;8H\x1b[06n");
        assert_eq!(parser.screen().cursor_position(), (3, 7));
        assert_eq!(parser.take_terminal_replies(), b"\x1b[2;8R");
        parser.process(b"\x1b7\x1b[?6l\x1b[5;9H\x1b[6n\x1b8\x1b[6n");
        assert_eq!(parser.take_terminal_replies(), b"\x1b[5;9R\x1b[2;8R");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronized_presentation_holds_cells_attributes_and_cursor_until_end() {
        let mut parser = Parser::new(4, 24, 100);
        parser.process(b"old\x1b[3;4H");
        let before = parser.screen().contents();
        let cursor = parser.screen().cursor_position();
        // Markers and paint can arrive across arbitrary PTY read boundaries.
        for chunk in [
            b"\x1b[?20".as_slice(),
            b"26h",
            b"\x1b[2J\x1b[H",
            b"\x1b[31mnew",
        ] {
            parser.process(chunk);
            assert_eq!(parser.render_screen().contents(), before);
            assert_eq!(parser.render_screen().cursor_position(), cursor);
        }
        assert_eq!(parser.screen().contents(), "new");
        assert_eq!(
            parser.render_screen().cell(0, 0).unwrap().fgcolor(),
            crate::Color::Default
        );
        parser.process(b"\x1b[?2026l");
        assert_eq!(parser.render_screen().contents(), "new");
        assert_eq!(
            parser.render_screen().cell(0, 0).unwrap().fgcolor(),
            crate::Color::Idx(1)
        );
        assert_eq!(parser.render_screen().cursor_position(), (0, 3));
        parser.process(b"er");
        assert_eq!(parser.render_screen().contents(), "newer");
    }

    #[test]
    fn repeated_begin_extends_one_frame_without_nesting() {
        let mut parser = Parser::new(4, 24, 0);
        parser.process(b"old\x1b[?2026h\rnew");
        let first = parser.screen.synchronized_update_start().unwrap();
        parser.process(b"\x1b[?2026h");
        let repeated = parser.screen.synchronized_update_start().unwrap();
        assert!(repeated >= first);
        assert_eq!(parser.render_screen().contents(), "old");
        parser.process(b"\x1b[?2026l");
        assert_eq!(parser.render_screen().contents(), "new");
        parser.process(b"\x1b[?2026l");
        assert_eq!(parser.render_screen().contents(), "new");
    }

    #[test]
    fn missing_end_recovers_without_requiring_another_output_byte() {
        let mut parser = Parser::new(4, 24, 0);
        parser.process(b"old\x1b[?2026h\rnew");
        let start = parser.screen.synchronized_update_start().unwrap();
        assert_eq!(parser.render_screen_at(start).contents(), "old");
        assert_eq!(
            parser
                .render_screen_at(start + SYNCHRONIZED_UPDATE_TIMEOUT)
                .contents(),
            "new"
        );
    }

    #[test]
    fn resize_and_user_scrolling_release_stale_presentation() {
        let mut parser = Parser::new(4, 24, 100);
        parser.process(b"old\x1b[?2026h\rnew");
        parser.set_size(3, 12);
        assert_eq!(parser.render_screen().size(), (3, 12));
        assert_eq!(parser.render_screen().contents(), "new");
        assert!(parser.synchronized_frame.is_none());
        parser.process(b"\x1b[?2026h\rscr");
        parser.set_scrollback(0);
        assert_eq!(parser.render_screen().contents(), "scr");
    }

    #[test]
    fn history_purge_is_not_a_viewport_or_cursor_reset() {
        let mut parser = Parser::new(3, 24, 100);
        parser.process(b"history\r\nfirst\r\nsecond\r\nthird\x1b[2;4H");
        let viewport = parser.screen().contents();
        let cursor = parser.screen().cursor_position();
        parser.process(b"\x1b[3J");
        assert_eq!(parser.screen().contents(), viewport);
        assert_eq!(parser.screen().cursor_position(), cursor);
        parser.set_scrollback(100);
        assert_eq!(parser.screen().scrollback(), 0);
        assert_eq!(parser.screen().contents(), viewport);
    }
}
