//! The MindPlayer mascot, drawn as half-block pixel art for the TUI, with a
//! gentle blink + bob animation driven by the app tick.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

const OUT: Color = Color::Rgb(74, 44, 72); // dark plum outline
const PINK: Color = Color::Rgb(255, 176, 205); // body
const HI: Color = Color::Rgb(255, 209, 224); // glossy highlight
const CHEEK: Color = Color::Rgb(255, 120, 150); // blush
const MOUTH: Color = Color::Rgb(170, 52, 86); // little smile
const EYE: Color = Color::Rgb(40, 38, 86); // navy eyes
const WHITE: Color = Color::Rgb(255, 255, 255); // eye shine
const FOOT: Color = Color::Rgb(224, 58, 80); // red feet

/// 16×16 Kirby-style sprite. Chars: o=outline P=body h=highlight c=cheek
/// m=mouth e=eye w=eye-shine f=foot, space=transparent. Simple flat colors so
/// it stays crisp and high-contrast at terminal resolution.
const ART: [&str; 16] = [
    "     oooooo     ",
    "   ooPPPPPPoo   ",
    "  oPPhhPPPPPPo  ",
    " oPPhhPPPPPPPPo ",
    " oPPeePPPPeePPo ",
    " oPPeePPPPeePPo ",
    " oPPewPPPPwePPo ",
    " oPcPPPmmPPPcPo ",
    " oPPPPPPPPPPPPo ",
    " oPPPPPPPPPPPPo ",
    "  oPPPPPPPPPPo  ",
    "   oPPPPPPPPo   ",
    "  ffff   ffff   ",
    "   ff     ff    ",
    "                ",
    "                ",
];

/// Pixel width / height of the sprite and its rendered size in terminal cells.
pub const WIDTH: u16 = 16;
/// Rendered height in rows (8 sprite-row-pairs + 1 for the bob).
pub const HEIGHT: u16 = 9;

fn color(c: char) -> Option<Color> {
    match c {
        'o' => Some(OUT),
        'P' => Some(PINK),
        'h' => Some(HI),
        'c' => Some(CHEEK),
        'm' => Some(MOUTH),
        'e' => Some(EYE),
        'w' => Some(WHITE),
        'f' => Some(FOOT),
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
        // Close the eyes: fill them pink with a dark line through the middle.
        for &x in &[4usize, 5, 10, 11] {
            grid[4][x] = 'P';
            grid[5][x] = 'o';
            grid[6][x] = 'P';
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
