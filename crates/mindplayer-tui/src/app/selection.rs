use super::*;

impl App {
    /// Convert an absolute terminal cell to a pane-relative 0-based (row, col)
    /// inside `pane_id`, or None if the pane has no live area or the point is
    /// above/left of it. Points past that pane clamp to its edge.
    pub(crate) fn pane_cell(&self, pane_id: &str, col: u16, row: u16) -> Option<(u16, u16)> {
        let (x, y, rows, cols) = self.pane_bounds.get(pane_id).copied().unwrap_or((
            self.pty_x,
            self.pty_y,
            self.pty_rows,
            self.pty_cols,
        ));
        if cols == 0 || rows == 0 || col < x || row < y {
            return None;
        }
        let pcol = (col - x).min(cols - 1);
        let prow = (row - y).min(rows - 1);
        Some((prow, pcol))
    }

    pub(crate) fn pane_at_cell(&self, col: u16, row: u16) -> Option<String> {
        self.panes.iter().find_map(|id| {
            let (x, y, rows, cols) = self.pane_bounds.get(id).copied()?;
            (col >= x && col < x.saturating_add(cols) && row >= y && row < y.saturating_add(rows))
                .then(|| id.clone())
        })
    }

    /// Begin a drag selection in the pane under the mouse.
    pub fn selection_start(&mut self, col: u16, row: u16) {
        let Some(pane_id) = self
            .pane_at_cell(col, row)
            .or_else(|| self.focused_pane().map(str::to_string))
        else {
            self.selection = None;
            return;
        };
        if let Some(pos) = self.panes.iter().position(|id| id == &pane_id) {
            self.focused = pos;
            self.sync_active();
        }
        match self.pane_cell(&pane_id, col, row) {
            Some(cell) => {
                self.selection = Some(PaneSelection {
                    pane_id,
                    anchor: cell,
                    cursor: cell,
                })
            }
            None => self.selection = None,
        }
    }

    /// Extend the active selection to the given absolute cell.
    pub fn selection_update(&mut self, col: u16, row: u16) {
        let Some(pane_id) = self.selection.as_ref().map(|sel| sel.pane_id.clone()) else {
            return;
        };
        if let Some(cell) = self.pane_cell(&pane_id, col, row) {
            if let Some(sel) = self.selection.as_mut() {
                sel.cursor = cell;
            }
        }
    }

    /// Finish the selection: copy the pane's selected text to the clipboard and
    /// clear the highlight. Returns true only if it was an actual drag that
    /// copied something — a plain click (anchor == cursor) copies nothing and
    /// returns false so the caller can forward the click to a mouse-aware child.
    pub fn selection_finish(&mut self) -> bool {
        let Some(sel) = self.selection.take() else {
            return false;
        };
        // A click without dragging is not a selection — don't copy a stray cell.
        if sel.anchor == sel.cursor {
            return false;
        }
        let (sr, sc, er, ec) = sel.bounds();
        let Some(pty) = self.ptys.get(&sel.pane_id) else {
            return false;
        };
        let Ok(parser) = pty.parser().lock() else {
            return false;
        };
        let screen = parser.screen();
        let (_, cols) = screen.size();
        // contents_between's end column is exclusive; +1 includes the cell under
        // the cursor so the highlight and the copied text cover the same cells.
        let end_col = ec.saturating_add(1).min(cols);
        let text = screen.contents_between(sr, sc, er, end_col);
        let text = text.trim_end().to_string();
        if !text.is_empty() {
            let chars = text.chars().count();
            self.pending_clipboard = Some(text);
            self.status = format!("copied {chars} chars from pane {}", short(&sel.pane_id));
        }
        // It was a drag (anchor != cursor): consume it as a selection regardless
        // of whether the cells held text, so the caller never re-fires it as a
        // click into the child.
        true
    }

    /// Selection bounds for `sid` if the active selection is in that pane — used
    /// by the renderer to highlight the selected cells.
    pub fn selection_for_pane(&self, sid: &str) -> Option<(u16, u16, u16, u16)> {
        self.selection
            .as_ref()
            .filter(|s| s.pane_id == sid)
            .map(PaneSelection::bounds)
    }

    /// Take any text queued for the system clipboard (event loop writes OSC 52).
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// Scroll the displayed session's scrollback (positive = older). Returns
    /// true if the view moved.
    pub fn scroll_active(&self, delta: isize) -> bool {
        self.active_pty().is_some_and(|p| p.scroll_by(delta))
    }

    /// Whether the displayed child has xterm mouse reporting on — if so, mouse
    /// events are forwarded to it instead of scrolling MindPlayer's scrollback.
    pub fn active_wants_mouse(&self) -> bool {
        self.active_pty().is_some_and(|p| p.mouse_wanted())
    }

    /// Translate an absolute terminal cell (from a mouse event) into 1-based
    /// coordinates relative to the live pane's inner area, clamped to it.
    pub fn pane_relative(&self, col: u16, row: u16) -> (u16, u16) {
        let c = col
            .saturating_sub(self.pty_x)
            .min(self.pty_cols.saturating_sub(1))
            + 1;
        let r = row
            .saturating_sub(self.pty_y)
            .min(self.pty_rows.saturating_sub(1))
            + 1;
        (c, r)
    }

    /// Forward a (pane-relative) mouse event to the displayed child. Returns
    /// true if a sequence was sent (caller redraws).
    pub fn forward_mouse_to_pty(
        &mut self,
        cb: u16,
        release: bool,
        motion: bool,
        col: u16,
        row: u16,
    ) -> bool {
        if let Some(id) = self.focused_pane().map(str::to_string) {
            if let Some(pty) = self.ptys.get_mut(&id) {
                return pty.forward_mouse(cb, release, motion, col, row);
            }
        }
        false
    }
}
