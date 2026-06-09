//! The MindPlayer mascot, drawn as half-block pixel art for the TUI, with a
//! gentle blink + bob animation driven by the app tick.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

/// A user-supplied mascot, pixelated into the same 16×16 footprint as the
/// built-in sprite so it drops into the existing layout unchanged. A static
/// image gets the gentle bob; an animated GIF plays its own frames.
pub struct Sprite {
    /// Each frame is [`WIDTH`]×16 pixels; `None` = transparent.
    frames: Vec<Vec<Vec<Option<Color>>>>,
    animated: bool,
}

impl Sprite {
    /// Load + pixelate an image (png/jpg/gif/webp/bmp) into a 16×16 sprite.
    /// Returns `None` if the file can't be read/decoded.
    pub fn load(path: &Path) -> Option<Self> {
        let frames_rgba = decode_frames(path)?;
        if frames_rgba.is_empty() {
            return None;
        }
        let animated = frames_rgba.len() > 1;
        let frames = frames_rgba.iter().map(pixelate).collect();
        Some(Self { frames, animated })
    }

    /// Render as half-block lines for this tick — same shape as [`lines`].
    pub fn lines(&self, tick: usize) -> Vec<Line<'static>> {
        let idx = if self.animated {
            // ~240ms per frame at the ~80ms app tick; loop.
            (tick / 3) % self.frames.len()
        } else {
            0
        };
        let grid = &self.frames[idx];
        let sprite = halfblock_lines(grid);
        // Static images bob; an animated GIF carries its own motion.
        let bob_up = !self.animated && (tick / 7).is_multiple_of(2);
        with_bob(sprite, bob_up)
    }
}

const N: u32 = WIDTH as u32; // pixelate target (square, matches the built-in)

/// Decode an image to one or more RGBA frames (multiple only for animated GIF).
fn decode_frames(path: &Path) -> Option<Vec<image::RgbaImage>> {
    let is_gif = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    if is_gif {
        use image::AnimationDecoder;
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(dec) = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)) {
                if let Ok(frames) = dec.into_frames().collect_frames() {
                    if !frames.is_empty() {
                        return Some(frames.into_iter().map(|f| f.into_buffer()).collect());
                    }
                }
            }
        }
    }
    let img = image::open(path).ok()?.to_rgba8();
    Some(vec![img])
}

/// Aspect-fit an RGBA frame into an N×N grid of `Option<Color>` (transparent
/// padding around it; near-transparent pixels stay `None`).
fn pixelate(img: &image::RgbaImage) -> Vec<Vec<Option<Color>>> {
    use image::imageops::FilterType;
    let resized = image::DynamicImage::ImageRgba8(img.clone())
        .resize(N, N, FilterType::Triangle)
        .to_rgba8();
    let (w, h) = resized.dimensions();
    let ox = (N - w.min(N)) / 2;
    let oy = (N - h.min(N)) / 2;
    let mut grid = vec![vec![None; N as usize]; N as usize];
    for y in 0..h.min(N) {
        for x in 0..w.min(N) {
            let px = resized.get_pixel(x, y).0;
            if px[3] >= 96 {
                grid[(y + oy) as usize][(x + ox) as usize] = Some(Color::Rgb(px[0], px[1], px[2]));
            }
        }
    }
    grid
}

/// Collapse a 16-row pixel grid into 8 half-block lines (top=fg, bottom=bg).
fn halfblock_lines(grid: &[Vec<Option<Color>>]) -> Vec<Line<'static>> {
    (0..16)
        .step_by(2)
        .map(|y| {
            let spans: Vec<Span<'static>> = (0..WIDTH as usize)
                .map(|x| {
                    let top = grid.get(y).and_then(|r| r.get(x).copied()).flatten();
                    let bot = grid.get(y + 1).and_then(|r| r.get(x).copied()).flatten();
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
        .collect()
}

/// Wrap 8 sprite lines into the [`HEIGHT`]-row block, bobbing up or down.
fn with_bob(sprite: Vec<Line<'static>>, bob_up: bool) -> Vec<Line<'static>> {
    let blank = Line::from(" ".repeat(WIDTH as usize));
    let mut out = Vec::with_capacity(HEIGHT as usize);
    if bob_up {
        out.push(blank);
        out.extend(sprite);
    } else {
        out.extend(sprite);
        out.push(blank);
    }
    out
}

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
