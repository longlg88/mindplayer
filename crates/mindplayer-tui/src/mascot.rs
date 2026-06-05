//! The MindPlayer mascot, drawn as half-block pixel art for the TUI, with a
//! gentle blink + bob animation driven by the app tick.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

const OUT: Color = Color::Rgb(26, 31, 44);
const BODY: Color = Color::Rgb(126, 162, 247);
const LT: Color = Color::Rgb(173, 194, 251);
const DK: Color = Color::Rgb(92, 116, 200);
const WHITE: Color = Color::Rgb(245, 246, 249);
const PLAY: Color = Color::Rgb(255, 255, 255);
const CHEEK: Color = Color::Rgb(245, 150, 170);

/// 16×16 sprite. Chars: o=outline B=body L=highlight D=belly W=eye-white
/// k=pupil P=play-badge c=cheek s=smile, space=transparent.
const ART: [&str; 16] = [
    "      oLLo      ",
    "      oLLo      ",
    "    oooooooo    ",
    "   oBBBBBBBBo   ",
    "  oBLLBBBBBBBo  ",
    "  oBWWBBBBWWBo  ",
    "  oBWkBBBBWkBo  ",
    "  oBBBBBBBBBBo  ",
    "  ocBBBssBBBco  ",
    "  oBDDDPDDDDo   ",
    "  oBDDDPPDDDo   ",
    "  oBDDDPDDDDo   ",
    "   oDDDDDDDo    ",
    "  oBBo  oBBo    ",
    "  oo      oo    ",
    "                ",
];

/// Pixel width / height of the sprite and its rendered size in terminal cells.
pub const WIDTH: u16 = 16;
/// Rendered height in rows (8 sprite-row-pairs + 1 for the bob).
pub const HEIGHT: u16 = 9;

fn color(c: char) -> Option<Color> {
    match c {
        'o' | 'k' | 's' => Some(OUT),
        'B' => Some(BODY),
        'L' => Some(LT),
        'D' => Some(DK),
        'W' => Some(WHITE),
        'P' => Some(PLAY),
        'c' => Some(CHEEK),
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
        // Close the eyes: flatten the white/pupil blocks into a dark line.
        for &x in &[4usize, 5, 10, 11] {
            grid[5][x] = 'B';
            grid[6][x] = 'o';
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
