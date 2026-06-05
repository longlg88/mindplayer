//! Render a `vt100::Screen` into a ratatui buffer, cell by cell.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

pub struct TerminalView<'a> {
    screen: &'a vt100::Screen,
}

impl<'a> TerminalView<'a> {
    pub fn new(screen: &'a vt100::Screen) -> Self {
        Self { screen }
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
