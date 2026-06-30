//! The MindPlayer mascot, drawn as half-block pixel art for the TUI, with a
//! gentle blink + bob animation driven by the app tick.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

const MINT: Color = Color::Rgb(142, 224, 199);
const MINT_DK: Color = Color::Rgb(90, 183, 173);
const FACE: Color = Color::Rgb(237, 249, 227);
const INK: Color = Color::Rgb(34, 53, 82);

/// 16x16 Pane Bot sprite, matching `assets/mascot.png` / the README mascot.
/// Chars: T=mint, S=shadow, F=face, D=dark body/ink.
const ART: [&str; 16] = [
    "                ",
    "       TT       ",
    "       SS       ",
    "     SSSSSS     ",
    "    TTTTTTTT    ",
    "   TTTTTTTTTT   ",
    "   STTTTTTTTS   ",
    "   STFFFFFFTS   ",
    "   STFDFFDFTS   ",
    "   STFDFFDFTS   ",
    "   STFFFFFFTS   ",
    "   STTTTTTTTS   ",
    "     DDDDDD     ",
    "   DDDTDTDTDD   ",
    "     DDDDDD     ",
    "                ",
];

/// Pixel width / height of the sprite and its rendered size in terminal cells.
pub const WIDTH: u16 = 16;
/// Rendered height in rows (8 sprite-row-pairs + 1 for the bob).
pub const HEIGHT: u16 = 9;

fn color(c: char) -> Option<Color> {
    match c {
        'T' => Some(MINT),
        'S' => Some(MINT_DK),
        'F' => Some(FACE),
        'D' => Some(INK),
        _ => None,
    }
}

/// The mascot as ratatui lines for the given animation tick. Always [`HEIGHT`]
/// rows tall (the extra row lets it bob without clipping).
pub fn lines(tick: usize) -> Vec<Line<'static>> {
    let blink = tick % 28 < 2; // a quick blink roughly every ~2.2s
    let bob_up = (tick / 7).is_multiple_of(2); // gentle up/down bob

    let mut grid: Vec<Vec<char>> = ART.iter().map(|r| r.chars().collect()).collect();
    if blink {
        for &x in &[6usize, 9] {
            grid[8][x] = 'F';
        }
    }

    let sprite: Vec<Line<'static>> = (0..16)
        .step_by(2)
        .map(|y| {
            let spans: Vec<Span<'static>> = (0..16)
                .map(|x| {
                    let top = color(grid[y][x]);
                    let bot = color(grid[y + 1][x]);
                    match (top, bot) {
                        (Some(t), Some(b)) => Span::styled("▀", Style::default().fg(t).bg(b)),
                        (Some(t), None) => Span::styled("▀", Style::default().fg(t)),
                        (None, Some(b)) => Span::styled("▄", Style::default().fg(b)),
                        (None, None) => Span::raw(" "),
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect();

    let blank = Line::from(" ".repeat(WIDTH as usize));
    let mut out = Vec::with_capacity(HEIGHT as usize);
    if bob_up {
        out.push(blank.clone());
        out.extend(sprite);
    } else {
        out.extend(sprite);
        out.push(blank);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_rows_are_terminal_cell_width() {
        assert_eq!(ART.len(), 16);
        for row in ART {
            assert_eq!(row.chars().count(), WIDTH as usize, "{row:?}");
        }
    }

    #[test]
    fn rendered_lines_keep_stable_size_while_animating() {
        for tick in [0, 1, 2, 7, 14, 28] {
            let frame = lines(tick);
            assert_eq!(frame.len(), HEIGHT as usize);
            for line in frame {
                assert_eq!(line.width(), WIDTH as usize);
            }
        }
    }

    #[test]
    fn blink_changes_the_eye_row_without_changing_layout() {
        let blink = lines(0);
        let open = lines(2);
        assert_eq!(blink.len(), open.len());
        assert_eq!(
            blink.iter().map(Line::width).collect::<Vec<_>>(),
            open.iter().map(Line::width).collect::<Vec<_>>()
        );
        assert_ne!(format!("{blink:?}"), format!("{open:?}"));
    }
}
