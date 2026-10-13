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
