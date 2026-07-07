//! The MindPlayer mascot — a small whale, drawn side-on and swimming, as
//! half-block pixel art with a gentle blink + bob animation driven by the
//! app tick. It also cycles through its three expressions live while the
//! app is running (not per-launch): every [`CYCLE_TICKS`] ticks it advances
//! to the next one, wrapping back to the first after the last.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

const BODY: Color = Color::Rgb(111, 179, 217);
const TAIL: Color = Color::Rgb(74, 134, 173);
const INK: Color = Color::Rgb(28, 43, 51);
const LIGHT: Color = Color::Rgb(234, 246, 251);
const BLUSH: Color = Color::Rgb(246, 166, 192);
const HIGHLIGHT: Color = Color::Rgb(127, 178, 224);

/// One expression: a 24-wide sprite plus which cells are its eye (so blink
/// can close the right pixels regardless of how big/where that variant's
/// eye is drawn).
struct Variant {
    art: [&'static str; 13],
    eye: &'static [(usize, usize)],
}

// Rows 0-3 are the spout (a stem bursting into a horizontal arm, plus two
// stray droplets) and are identical across every variant — only the body
// below it (head, back highlight, belly, tail) changes per expression.
const SPOUT: [&str; 4] = [
    ".......D....D...........",
    ".........DDDDD..........",
    "...........DD...........",
    "...........DD...........",
];

const VARIANTS: [Variant; 3] = [
    // "큰눈 + 볼터치" — bigger two-pixel eye, blushing cheek.
    Variant {
        art: [
            SPOUT[0],
            SPOUT[1],
            SPOUT[2],
            SPOUT[3],
            "....AAAAAAAAAA..........",
            "..FFFAAAAAAAAAAAAA......",
            ".AAAACCAAAAAAAAAAABB....",
            ".AEAAAAAAAAAAAAAAAABBB..",
            ".DDDDDAAAAAAAAAAABBB....",
            ".DDDDDDDDAAAAAAAAA......",
            "..DDDDDDDDDAAAAA........",
            "...DDDDDDDDDAA..........",
            ".....DDDDDDD............",
        ],
        eye: &[(6, 5), (6, 6)],
    },
    // "차분한 표정" — plain single-pixel eye, no blush.
    Variant {
        art: [
            SPOUT[0],
            SPOUT[1],
            SPOUT[2],
            SPOUT[3],
            "....AAAAAAAAAA..........",
            "..FFFAAAAAAAAAAAAA......",
            ".AAAACAAAAAAAAAAAABB....",
            ".AAAAAAAAAAAAAAAAAABBB..",
            ".DDDDDAAAAAAAAAAABBB....",
            ".DDDDDDDDAAAAAAAAA......",
            "..DDDDDDDDDAAAAA........",
            "...DDDDDDDDDAA..........",
            ".....DDDDDDD............",
        ],
        eye: &[(6, 5)],
    },
    // "다이나믹, 큰 꼬리" — same eye as above, tail hook stretched up a row
    // higher so it reads mid-swing.
    Variant {
        art: [
            SPOUT[0],
            SPOUT[1],
            SPOUT[2],
            SPOUT[3],
            "....AAAAAAAAAA..........",
            "..FFFAAAAAAAAAAAAABB....",
            ".AAAACAAAAAAAAAAAABB....",
            ".AAAAAAAAAAAAAAAAAABBB..",
            ".DDDDDAAAAAAAAAAABBB....",
            ".DDDDDDDDAAAAAAAAA......",
            "..DDDDDDDDDAAAAA........",
            "...DDDDDDDDDAA..........",
            ".....DDDDDDD............",
        ],
        eye: &[(6, 5)],
    },
];

const ART_WIDTH: usize = 24;
const ART_HEIGHT: usize = 13;
/// How many terminal cells each pixel of the 24x10 art becomes. Upscaling
/// the validated pixel data (rather than hand-drawing a bigger grid) keeps
/// the exact same silhouette/eye coordinates and just makes it read bigger.
const SCALE: usize = 2;
/// How many ticks (~80ms each) each expression stays on screen before the
/// mascot advances to the next one — about 4.5s, long enough to register.
const CYCLE_TICKS: usize = 56;

/// Rendered size in terminal cells.
pub const WIDTH: u16 = (ART_WIDTH * SCALE) as u16;
/// Rendered height in rows (half the scaled art height, rounded down, + 1
/// for the bob).
pub const HEIGHT: u16 = (ART_HEIGHT * SCALE / 2 + 1) as u16;

/// Nearest-neighbor upscale: each source cell becomes a `scale`x`scale`
/// block of the same character.
fn upscale(grid: Vec<Vec<char>>, scale: usize) -> Vec<Vec<char>> {
    grid.into_iter()
        .flat_map(|row| {
            let wide: Vec<char> = row
                .into_iter()
                .flat_map(|c| std::iter::repeat_n(c, scale))
                .collect();
            std::iter::repeat_n(wide, scale)
        })
        .collect()
}

fn color(c: char) -> Option<Color> {
    match c {
        'A' => Some(BODY),
        'B' => Some(TAIL),
        'C' => Some(INK),
        'D' => Some(LIGHT),
        'E' => Some(BLUSH),
        'F' => Some(HIGHLIGHT),
        _ => None,
    }
}

/// The mascot as ratatui lines for the given animation tick. Always [`HEIGHT`]
/// rows tall (the extra row lets it bob without clipping). Which expression
/// is drawn advances on its own as `tick` grows — see [`CYCLE_TICKS`].
pub fn lines(tick: usize) -> Vec<Line<'static>> {
    let blink = tick % 28 < 2; // a quick blink roughly every ~2.2s
    let bob_up = (tick / 7).is_multiple_of(2); // gentle up/down bob
    let v = &VARIANTS[(tick / CYCLE_TICKS) % VARIANTS.len()];

    let mut grid: Vec<Vec<char>> = v.art.iter().map(|r| r.chars().collect()).collect();
    if blink {
        for &(row, col) in v.eye {
            grid[row][col] = 'A';
        }
    }
    let grid = upscale(grid, SCALE);

    let sprite: Vec<Line<'static>> = (0..ART_HEIGHT * SCALE)
        .step_by(2)
        .map(|y| {
            let spans: Vec<Span<'static>> = (0..WIDTH as usize)
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
    fn every_variant_art_row_is_art_width_before_upscaling() {
        for v in &VARIANTS {
            for row in v.art {
                assert_eq!(row.chars().count(), ART_WIDTH, "{row:?}");
            }
        }
    }

    #[test]
    fn every_variant_eye_cell_is_inside_the_grid_and_not_blank() {
        for v in &VARIANTS {
            for &(row, col) in v.eye {
                assert!(row < ART_HEIGHT && col < ART_WIDTH);
                let ch = v.art[row].chars().nth(col).unwrap();
                assert!(color(ch).is_some(), "eye cell {row},{col} is blank");
            }
        }
    }

    #[test]
    fn rendered_lines_keep_stable_size_across_the_whole_cycle() {
        for cycle in 0..VARIANTS.len() {
            for offset in [0, 1, 2, 7, 14, 28, CYCLE_TICKS - 1] {
                let frame = lines(cycle * CYCLE_TICKS + offset);
                assert_eq!(frame.len(), HEIGHT as usize);
                for line in frame {
                    assert_eq!(line.width(), WIDTH as usize);
                }
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

    #[test]
    fn expression_advances_every_cycle_ticks_and_wraps_around() {
        let first = lines(0);
        let second = lines(CYCLE_TICKS);
        let third = lines(CYCLE_TICKS * 2);
        let wrapped = lines(CYCLE_TICKS * 3);
        assert_ne!(format!("{first:?}"), format!("{second:?}"));
        assert_ne!(format!("{second:?}"), format!("{third:?}"));
        // Same tick-within-cycle, same blink/bob phase, three full cycles
        // later — should be pixel-identical to the very first frame.
        assert_eq!(format!("{first:?}"), format!("{wrapped:?}"));
    }

    #[test]
    fn upscale_repeats_each_pixel_into_a_scale_by_scale_block() {
        let grid = vec![vec!['A', 'B'], vec!['C', 'D']];
        let scaled = upscale(grid, 2);
        assert_eq!(
            scaled,
            vec![
                vec!['A', 'A', 'B', 'B'],
                vec!['A', 'A', 'B', 'B'],
                vec!['C', 'C', 'D', 'D'],
                vec!['C', 'C', 'D', 'D'],
            ]
        );
    }
}
