//! The MindPlayer mascot, drawn as half-block pixel art for the TUI, with a
//! gentle blink + bob animation driven by the app tick.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

/// Pixel resolution a user image is pixelated to. Bigger than the built-in
/// 16×16 sprite so photos/detailed art stay recognizable. Must be even (each
/// terminal row is two stacked pixels via half-blocks).
const CUSTOM_N: u32 = 32;

/// A user-supplied mascot, pixelated into an `n`×`n` grid and rendered with the
/// same half-block pipeline. A static image gets the gentle bob; an animated
/// GIF plays its own frames. A near-uniform background is auto-dropped so the
/// subject sits on the terminal background instead of in a colored box.
pub struct Sprite {
    /// Each frame is `n` rows × `n` cols of pixels; `None` = transparent.
    frames: Vec<Vec<Vec<Option<Color>>>>,
    animated: bool,
    n: u16,
}

impl Sprite {
    /// Load + pixelate an image (png/jpg/gif/webp/bmp). `None` if it can't be
    /// read/decoded.
    pub fn load(path: &Path) -> Option<Self> {
        let frames_rgba = decode_frames(path)?;
        if frames_rgba.is_empty() {
            return None;
        }
        let animated = frames_rgba.len() > 1;
        let frames = frames_rgba.iter().map(|f| pixelate(f, CUSTOM_N)).collect();
        Some(Self {
            frames,
            animated,
            n: CUSTOM_N as u16,
        })
    }

    /// Rendered width in terminal cells (one cell per pixel column).
    pub fn cell_width(&self) -> u16 {
        self.n
    }

    /// Rendered height in cells: two pixels per row, plus one for the bob.
    pub fn cell_height(&self) -> u16 {
        self.n / 2 + 1
    }

    /// Render as half-block lines for this tick.
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
        with_bob(sprite, bob_up, self.n)
    }
}

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

/// Aspect-fit an RGBA frame into an `n`×`n` grid of `Option<Color>` (transparent
/// padding around it). Already-transparent pixels stay `None`, and a near-uniform
/// background (sampled from the corners) is dropped so the subject pops on the
/// terminal background instead of sitting in a colored box.
fn pixelate(img: &image::RgbaImage, n: u32) -> Vec<Vec<Option<Color>>> {
    use image::imageops::FilterType;
    let resized = image::DynamicImage::ImageRgba8(img.clone())
        // Nearest keeps hard pixel edges (a crisp "pixel-art" look) instead of
        // the soft blend an averaging filter produces at this small size.
        .resize(n, n, FilterType::Nearest)
        .to_rgba8();
    let (w, h) = resized.dimensions();
    let (w, h) = (w.min(n), h.min(n));
    let ox = (n - w) / 2;
    let oy = (n - h) / 2;
    let bg = detect_background(&resized, w, h);
    let mut grid = vec![vec![None; n as usize]; n as usize];
    for y in 0..h {
        for x in 0..w {
            let px = resized.get_pixel(x, y).0;
            let opaque = px[3] >= 96;
            let is_bg = bg.is_some_and(|b| color_dist(px, b) < 52);
            if opaque && !is_bg {
                grid[(y + oy) as usize][(x + ox) as usize] = Some(Color::Rgb(px[0], px[1], px[2]));
            }
        }
    }
    grid
}

/// Squared-ish RGB distance between two pixels (alpha ignored).
fn color_dist(a: [u8; 4], b: [u8; 4]) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).unsigned_abs();
    d(a[0], b[0]) + d(a[1], b[1]) + d(a[2], b[2])
}

/// If the four corners agree on a color, return it as the background to drop;
/// otherwise `None` (don't strip anything — there's no clear flat background).
fn detect_background(img: &image::RgbaImage, w: u32, h: u32) -> Option<[u8; 4]> {
    if w < 4 || h < 4 {
        return None;
    }
    let corners = [
        img.get_pixel(0, 0).0,
        img.get_pixel(w - 1, 0).0,
        img.get_pixel(0, h - 1).0,
        img.get_pixel(w - 1, h - 1).0,
    ];
    // Opaque + mutually similar → it's a flat background.
    if corners.iter().any(|c| c[3] < 96) {
        return None;
    }
    let agree = corners.iter().all(|c| color_dist(*c, corners[0]) < 40);
    agree.then_some(corners[0])
}

/// Collapse an N-row pixel grid into N/2 half-block lines (top=fg, bottom=bg).
fn halfblock_lines(grid: &[Vec<Option<Color>>]) -> Vec<Line<'static>> {
    let w = grid.first().map_or(0, |r| r.len());
    (0..grid.len())
        .step_by(2)
        .map(|y| {
            let spans: Vec<Span<'static>> = (0..w)
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

/// Wrap sprite lines into a block one row taller, bobbing up or down.
fn with_bob(sprite: Vec<Line<'static>>, bob_up: bool, width: u16) -> Vec<Line<'static>> {
    let blank = Line::from(" ".repeat(width as usize));
    let mut out = Vec::with_capacity(sprite.len() + 1);
    if bob_up {
        out.push(blank);
        out.extend(sprite);
    } else {
        out.extend(sprite);
        out.push(blank);
    }
    out
}

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
