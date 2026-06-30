//! Render a `vt100::Screen` into a ratatui buffer, cell by cell.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// Background for drag-to-copy selected cells.
const SELECTION_BG: Color = Color::Rgb(58, 78, 120);

pub struct TerminalView<'a> {
    screen: &'a vt100::Screen,
    /// Pane-relative, 0-based, inclusive row-major selection
    /// `(start_row, start_col, end_row, end_col)` to highlight, if any.
    selection: Option<(u16, u16, u16, u16)>,
}

impl<'a> TerminalView<'a> {
    pub fn new(screen: &'a vt100::Screen) -> Self {
        Self {
            screen,
            selection: None,
        }
    }

    /// Highlight a drag-copy selection region (pane-relative cells).
    pub fn with_selection(mut self, selection: Option<(u16, u16, u16, u16)>) -> Self {
        self.selection = selection;
        self
    }
}

/// Whether cell `(y, x)` falls inside the inclusive row-major selection.
fn cell_selected(sel: (u16, u16, u16, u16), y: u16, x: u16) -> bool {
    let (sr, sc, er, ec) = sel;
    if y < sr || y > er {
        return false;
    }
    if sr == er {
        x >= sc && x <= ec
    } else if y == sr {
        x >= sc
    } else if y == er {
        x <= ec
    } else {
        true
    }
}

impl Widget for TerminalView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for y in 0..area.height {
            if y >= rows {
                break;
            }
            for x in 0..area.width {
                if x >= cols {
                    break;
                }
                let Some(cell) = self.screen.cell(y, x) else {
                    continue;
                };
                let target = &mut buf[(area.x + x, area.y + y)];
                let contents = cell.contents();
                target.set_symbol(if contents.is_empty() { " " } else { &contents });

                let mut style = Style::default()
                    .fg(to_color(cell.fgcolor()))
                    .bg(to_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Selection wins over the cell's own background so the copied
                // region is unmistakable.
                if self.selection.is_some_and(|sel| cell_selected(sel, y, x)) {
                    style = style.bg(SELECTION_BG).remove_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    }
}

fn to_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::cell_selected;

    #[test]
    fn selection_is_row_major_and_inclusive() {
        // Multi-row selection: rows 1..=3, from col 3 on row 1 to col 5 on row 3.
        let sel = (1, 3, 3, 5);
        assert!(!cell_selected(sel, 0, 4), "row above the selection");
        assert!(!cell_selected(sel, 1, 2), "start row, before the start col");
        assert!(cell_selected(sel, 1, 3), "the start cell itself");
        assert!(cell_selected(sel, 1, 99), "start row, after the start col");
        assert!(cell_selected(sel, 2, 0), "a full middle row");
        assert!(cell_selected(sel, 3, 5), "the end cell itself");
        assert!(!cell_selected(sel, 3, 6), "end row, past the end col");
        assert!(!cell_selected(sel, 4, 0), "row below the selection");
    }

    #[test]
    fn single_row_selection() {
        let sel = (2, 4, 2, 6);
        assert!(!cell_selected(sel, 2, 3));
        assert!(cell_selected(sel, 2, 4));
        assert!(cell_selected(sel, 2, 6));
        assert!(!cell_selected(sel, 2, 7));
        assert!(!cell_selected(sel, 1, 5));
    }
}
