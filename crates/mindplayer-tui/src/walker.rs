//! The walking character strip that replaces the fixed whale mascot on the
//! browse screens (scope select, scanning, and the session list's hero block).
//!
//! Same half-block rendering technique as [`crate::mascot`] — a char grid, a
//! palette lookup, and two pixel rows folded into one terminal row with
//! `▀`/`▄` plus fg/bg. What's new is that the sprite is *blitted into a
//! full-width canvas* at an x position derived from the tick, so the character
//! walks the whole strip instead of sitting centered.
//!
//! Position, frame, and facing are all pure functions of `tick` — no new state
//! on `App`, so the existing `hero_visible` redraw gating and `app.spinner`
//! tick keep working untouched (same contract as `mascot::lines`).

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

pub const SPRITE_W: usize = 12;
/// Must stay even — a terminal row holds exactly two pixel rows.
pub const SPRITE_H: usize = 8;

/// Pixel rows of clearance above the standing pose, so a hop has somewhere to
/// go instead of clipping off the top of the strip. Even, like `SPRITE_H`.
const HEADROOM: usize = 2;
/// The pixel canvas is the sprite plus its hop clearance.
const CANVAS_H: usize = SPRITE_H + HEADROOM;

/// Canvas rows plus the ground line the character walks on.
pub const HEIGHT: u16 = (CANVAS_H / 2) as u16 + 1;

/// Ticks per cell of horizontal travel (larger = slower).
const TICKS_PER_CELL: usize = 2;
/// Ticks each walk frame is held.
const TICKS_PER_FRAME: usize = 6;
/// How high a hop lifts the sprite, in pixel rows. One terminal row.
const HOP_LIFT: usize = 2;
/// Ticks per hop (up then down), so a hop reads as a bounce, not a jitter.
const TICKS_PER_HOP: usize = 8;
/// Ticks per half breath while sleeping — slow enough to read as breathing.
const TICKS_PER_BREATH: usize = 20;

const GROUND: Color = Color::Rgb(60, 70, 88);

/// What the character is doing right now. All five come from one repeating
/// sweep (see [`state_at`]) so behavior stays a pure function of the tick and
/// the strip width — no new state on `App`, no RNG, and the same contract
/// `mascot::lines` had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Motion {
    /// Striding along the strip.
    Walk,
    /// Standing still, feet together.
    Pause,
    /// Moving, but bouncing as it goes.
    Hop,
    /// Sitting down; does not advance.
    Sit,
    /// Dozing, settled lower; does not advance.
    Sleep,
}

impl Motion {
    /// Whether this motion carries the character along the strip. `Pause`,
    /// `Sit` and `Sleep` hold position, which is what makes the walk look
    /// deliberate instead of a metronome. Only the tests need to ask — the
    /// render path gets the motion and the cell count together from
    /// [`state_at`], so it never has to classify one.
    #[cfg(test)]
    const fn advances(self) -> bool {
        matches!(self, Motion::Walk | Motion::Hop)
    }
}

/// A scheduled stop, placed at a cell offset within one out-and-back sweep.
/// Keyed to *position* rather than to time on purpose: a time-keyed schedule
/// could freeze the character at an off-screen spot, leaving the strip empty
/// for the whole beat (it did — see `a_stationary_pose_is_always_on_screen`).
struct Stop {
    /// Cell offset within the sweep at which the character stops.
    at: usize,
    motion: Motion,
    ticks: usize,
}

/// Stops for one sweep, at cell offsets that are always fully on-screen for
/// `width`. Must stay sorted by `at` — [`state_at`] walks them in order.
fn stops_for(width: usize) -> [Stop; 3] {
    let span = width + SPRITE_W;
    // Outbound, the sprite is fully in view for cells `SPRITE_W ..= width`.
    let lo = SPRITE_W;
    let visible = width.saturating_sub(SPRITE_W);
    [
        Stop {
            at: lo + visible / 4,
            motion: Motion::Pause,
            ticks: 45,
        },
        Stop {
            at: lo + visible * 3 / 4,
            motion: Motion::Sit,
            ticks: 80,
        },
        // Mirrored onto the return leg, which lands at x = visible/2.
        Stop {
            at: span * 2 - lo - visible / 2,
            motion: Motion::Sleep,
            ticks: 140,
        },
    ]
}

/// Whether the character bounces rather than strides at this point of the
/// sweep. A band just after the right-edge turnaround, so it is always in view.
fn hops_at(cells_in_sweep: usize, width: usize) -> bool {
    let span = width + SPRITE_W;
    let lo = span + SPRITE_W;
    let hi = lo + width.saturating_sub(SPRITE_W) / 3;
    (lo..hi).contains(&cells_in_sweep)
}

/// The motion while moving: hop inside the hop band, otherwise walk.
fn moving_motion(cells_in_sweep: usize, width: usize) -> Motion {
    if hops_at(cells_in_sweep, width) {
        Motion::Hop
    } else {
        Motion::Walk
    }
}

/// Current motion, cells advanced within this sweep, and ticks into the current
/// stop. O(number of stops) — constant, and independent of how large `tick` has
/// grown, so this stays cheap after hours of uptime.
fn state_at(tick: usize, width: usize) -> (Motion, usize, usize) {
    let total_cells = (width + SPRITE_W) * 2;
    let stops = stops_for(width);
    let stop_ticks: usize = stops.iter().map(|s| s.ticks).sum();
    let cycle = total_cells * TICKS_PER_CELL + stop_ticks;
    let mut t = tick % cycle;
    let mut cells = 0usize;
    for stop in &stops {
        let travel = stop.at.saturating_sub(cells) * TICKS_PER_CELL;
        if t < travel {
            let c = cells + t / TICKS_PER_CELL;
            return (moving_motion(c, width), c, 0);
        }
        t -= travel;
        cells = stop.at;
        if t < stop.ticks {
            return (stop.motion, cells, t);
        }
        t -= stop.ticks;
    }
    let c = cells + t / TICKS_PER_CELL;
    (moving_motion(c, width), c, 0)
}

/// One pickable character. `frames` is a 2-frame walk cycle drawn facing
/// right; walking left mirrors it at render time rather than needing its own
/// art. Every character's eyes sit at row 3, columns 3-4 and 7-8 — see the
/// `eyes_are_uniform_across_every_character` test, which is what keeps a new
/// character from drifting into the "scary" look the first draft had.
pub struct Character {
    pub id: &'static str,
    pub name: &'static str,
    pub body: Color,
    pub dark: Color,
    pub ink: Color,
    pub light: Color,
    pub accent: Color,
    pub frames: [[&'static str; SPRITE_H]; 2],
    /// Sitting pose. Rows 0-4 are copied verbatim from one of the walk frames
    /// so the eyes stay at the same coordinates in every pose — only rows 5-7
    /// (the lower body) change. See `poses_keep_the_eyes_uniform`.
    pub sit: [&'static str; SPRITE_H],
    /// Dozing pose, same rule as [`Self::sit`] but settled lower/wider.
    pub sleep: [&'static str; SPRITE_H],
}

/// The id used when nothing is stored yet, or when a stored id no longer
/// exists (a character removed in a later release must not break startup).
pub const DEFAULT_ID: &str = "duck";

pub const ALL: &[Character] = &[
    Character {
        id: "duck",
        name: "rubber duck",
        body: Color::Rgb(255, 210, 63),
        dark: Color::Rgb(229, 180, 32),
        ink: Color::Rgb(42, 35, 32),
        light: Color::Rgb(255, 248, 208),
        accent: Color::Rgb(245, 118, 43),
        frames: [
            [
                "....AAAA....",
                "..AAAAAAAA..",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AEEEEEEA..",
                "...AAAAAA...",
                "...AAAAAA...",
                "...E....E...",
            ],
            [
                "....AAAA....",
                "..AAAAAAAA..",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AEEEEEEA..",
                "...AAAAAA...",
                "...AAAAAA...",
                "....E..E....",
            ],
        ],
        sit: [
            "....AAAA....",
            "..AAAAAAAA..",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AEEEEEEA..",
            "...AAAAAA...",
            ".AAAAAAAAAA.",
            "..EE....EE..",
        ],
        sleep: [
            "....AAAA....",
            "..AAAAAAAA..",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AEEEEEEA..",
            "...AAAAAA...",
            ".AAAAAAAAAA.",
            ".BBBBBBBBBB.",
        ],
    },
    Character {
        id: "bunny",
        name: "bunny",
        body: Color::Rgb(242, 238, 240),
        dark: Color::Rgb(216, 204, 212),
        ink: Color::Rgb(58, 46, 54),
        light: Color::Rgb(255, 255, 255),
        accent: Color::Rgb(245, 168, 188),
        frames: [
            [
                "...AA..AA...",
                "...AE..EA...",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                "..AAAAAAAA..",
                "...AAAAAA...",
                "..AA....AA..",
            ],
            [
                "...AA..AA...",
                "...AE..EA...",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                "..AAAAAAAA..",
                "...AAAAAA...",
                "...AA..AA...",
            ],
        ],
        sit: [
            "...AA..AA...",
            "...AE..EA...",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            "..AAAAAAAA..",
            ".AAAAAAAAAA.",
            "..AA....AA..",
        ],
        sleep: [
            "...AA..AA...",
            "...AE..EA...",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            "..AAAAAAAA..",
            ".AAAAAAAAAA.",
            ".BBBBBBBBBB.",
        ],
    },
    Character {
        id: "chick",
        name: "chick",
        body: Color::Rgb(255, 217, 94),
        dark: Color::Rgb(224, 176, 48),
        ink: Color::Rgb(42, 35, 32),
        light: Color::Rgb(255, 246, 200),
        accent: Color::Rgb(240, 138, 48),
        frames: [
            [
                "....AAAA....",
                "..AAAAAAAA..",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                "...AAAAAA...",
                "....AAAA....",
                "...E....E...",
            ],
            [
                "....AAAA....",
                "..AAAAAAAA..",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                "...AAAAAA...",
                "....AAAA....",
                "....E..E....",
            ],
        ],
        sit: [
            "....AAAA....",
            "..AAAAAAAA..",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            "...AAAAAA...",
            "..AAAAAAAA..",
            "...E....E...",
        ],
        sleep: [
            "....AAAA....",
            "..AAAAAAAA..",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            "...AAAAAA...",
            ".AAAAAAAAAA.",
            ".BBBBBBBBBB.",
        ],
    },
    Character {
        id: "slime",
        name: "slime",
        body: Color::Rgb(110, 208, 168),
        dark: Color::Rgb(63, 160, 124),
        ink: Color::Rgb(28, 56, 48),
        light: Color::Rgb(47, 92, 76),
        accent: Color::Rgb(56, 128, 106),
        frames: [
            [
                "....AAAA....",
                "...AAAAAA...",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                ".AAAAAAAAAA.",
                ".AAAAAAAAAA.",
                "..DDDDDDDD..",
            ],
            [
                "............",
                "....AAAA....",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                ".AAAAAAAAAA.",
                "AAAAAAAAAAAA",
                ".DDDDDDDDDD.",
            ],
        ],
        sit: [
            "....AAAA....",
            "...AAAAAA...",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            ".AAAAAAAAAA.",
            "AAAAAAAAAAAA",
            "..DDDDDDDD..",
        ],
        sleep: [
            "....AAAA....",
            "...AAAAAA...",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            ".AAAAAAAAAA.",
            "AAAAAAAAAAAA",
            "DDDDDDDDDDDD",
        ],
    },
    Character {
        id: "penguin",
        name: "penguin",
        body: Color::Rgb(61, 74, 99),
        dark: Color::Rgb(42, 52, 72),
        ink: Color::Rgb(18, 24, 31),
        light: Color::Rgb(242, 245, 250),
        accent: Color::Rgb(240, 160, 60),
        frames: [
            [
                "...AAAAAA...",
                "..AAAAAAAA..",
                "..ADDDDDDA..",
                "..DCCDDCCD..",
                "..ADDEEDDA..",
                "..ADDDDDDA..",
                ".AADDDDDDAA.",
                "...EE..EE...",
            ],
            [
                "...AAAAAA...",
                "..AAAAAAAA..",
                "..ADDDDDDA..",
                "..DCCDDCCD..",
                "..ADDEEDDA..",
                "..ADDDDDDA..",
                ".AADDDDDDAA.",
                "..EE....EE..",
            ],
        ],
        sit: [
            "...AAAAAA...",
            "..AAAAAAAA..",
            "..ADDDDDDA..",
            "..DCCDDCCD..",
            "..ADDEEDDA..",
            ".AADDDDDDAA.",
            ".AADDDDDDAA.",
            "..AAAAAAAA..",
        ],
        sleep: [
            "...AAAAAA...",
            "..AAAAAAAA..",
            "..ADDDDDDA..",
            "..DCCDDCCD..",
            "..ADDEEDDA..",
            "..ADDDDDDA..",
            ".AADDDDDDAA.",
            ".BBBBBBBBBB.",
        ],
    },
    Character {
        id: "octopus",
        name: "octopus",
        body: Color::Rgb(224, 132, 176),
        dark: Color::Rgb(184, 95, 138),
        ink: Color::Rgb(58, 32, 48),
        light: Color::Rgb(255, 208, 228),
        accent: Color::Rgb(160, 58, 104),
        frames: [
            [
                "....AAAA....",
                "..AAAAAAAA..",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                ".AAAAAAAAAA.",
                ".A.A.A.A.A.A",
                "A...A...A...",
            ],
            [
                "....AAAA....",
                "..AAAAAAAA..",
                "..AAAAAAAA..",
                "..ACCAACCA..",
                "..AAAEEAAA..",
                ".AAAAAAAAAA.",
                "A.A.A.A.A.A.",
                "...A...A...A",
            ],
        ],
        sit: [
            "....AAAA....",
            "..AAAAAAAA..",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            ".AAAAAAAAAA.",
            ".AAAAAAAAAA.",
            ".A.A.A.A.A.A",
        ],
        sleep: [
            "....AAAA....",
            "..AAAAAAAA..",
            "..AAAAAAAA..",
            "..ACCAACCA..",
            "..AAAEEAAA..",
            ".AAAAAAAAAA.",
            "AAAAAAAAAAAA",
            "A.A.A.A.A.A.",
        ],
    },
];

/// Index of `id` in [`ALL`], falling back to [`DEFAULT_ID`] when the stored id
/// is unknown (renamed/removed character) so a stale `state.json` can never
/// panic or leave the strip blank.
pub fn index_of(id: &str) -> usize {
    ALL.iter()
        .position(|c| c.id == id)
        .or_else(|| ALL.iter().position(|c| c.id == DEFAULT_ID))
        .unwrap_or(0)
}

pub fn get(index: usize) -> &'static Character {
    &ALL[index.min(ALL.len() - 1)]
}

fn color(ch: &Character, c: char) -> Option<Color> {
    match c {
        'A' => Some(ch.body),
        'B' => Some(ch.dark),
        'C' => Some(ch.ink),
        'D' => Some(ch.light),
        'E' => Some(ch.accent),
        _ => None,
    }
}

fn half_block(top: Option<Color>, bot: Option<Color>) -> Span<'static> {
    match (top, bot) {
        (Some(t), Some(b)) => Span::styled("▀", Style::default().fg(t).bg(b)),
        (Some(t), None) => Span::styled("▀", Style::default().fg(t)),
        (None, Some(b)) => Span::styled("▄", Style::default().fg(b)),
        (None, None) => Span::raw(" "),
    }
}

/// Fold a pixel canvas of `rows` rows into terminal rows.
fn fold(canvas: &[Vec<Option<Color>>], rows: usize, width: usize) -> Vec<Line<'static>> {
    (0..rows)
        .step_by(2)
        .map(|y| {
            Line::from(
                (0..width)
                    .map(|x| half_block(canvas[y][x], canvas[y + 1][x]))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Turn a cell count into `(x, facing_right)`, sweeping back and forth. `x` may
/// be negative or past `width` — the character walks fully off both edges before
/// turning around, so it never "pops" at the boundary.
fn sweep(cells: usize, width: usize) -> (isize, bool) {
    let span = width + SPRITE_W;
    let pos = cells % (span * 2);
    if pos < span {
        (pos as isize - SPRITE_W as isize, true)
    } else {
        ((span * 2 - pos) as isize - SPRITE_W as isize, false)
    }
}

/// The art and vertical lift for a motion at a given point within its beat.
fn pose(ch: &Character, motion: Motion, tick: usize, phase: usize) -> (&[&'static str], usize) {
    match motion {
        Motion::Walk => (&ch.frames[(tick / TICKS_PER_FRAME) % ch.frames.len()], 0),
        // Feet together reads as standing rather than mid-stride.
        Motion::Pause => (&ch.frames[1], 0),
        Motion::Hop => {
            // Airborne for the first half of each hop, grounded for the second.
            let lift = if phase % TICKS_PER_HOP < TICKS_PER_HOP / 2 {
                HOP_LIFT
            } else {
                0
            };
            (&ch.frames[(tick / TICKS_PER_FRAME) % ch.frames.len()], lift)
        }
        Motion::Sit => (&ch.sit, 0),
        // A slow one-pixel breathe. Sit and sleep otherwise differ only in
        // color at this size, and half a row of drift reads as breathing.
        Motion::Sleep => {
            let breathe = usize::from((phase / TICKS_PER_BREATH) % 2 == 1);
            (&ch.sleep, breathe)
        }
    }
}

/// The full strip for `width` terminal columns: [`HEIGHT`] rows, the last one
/// being the ground line. Returns an empty vec when the area is too narrow to
/// hold the sprite — callers skip drawing rather than rendering a clipped mess.
pub fn lines(ch: &Character, tick: usize, width: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    if w < SPRITE_W + 2 {
        return Vec::new();
    }
    let (motion, cells, phase) = state_at(tick, w);
    let (x, facing_right) = sweep(cells, w);
    let (art, lift) = pose(ch, motion, tick, phase);
    // Sprite normally rests on the ground (offset HEADROOM); a hop lifts it.
    let top = HEADROOM - lift.min(HEADROOM);

    let mut canvas: Vec<Vec<Option<Color>>> = vec![vec![None; w]; CANVAS_H];
    for (row, line) in art.iter().enumerate() {
        for (col, c) in line.chars().enumerate() {
            let Some(color) = color(ch, c) else { continue };
            // Mirror instead of storing a second art set for the other facing.
            let sx = if facing_right {
                col
            } else {
                SPRITE_W - 1 - col
            };
            let dx = x + sx as isize;
            if dx >= 0 && (dx as usize) < w {
                canvas[top + row][dx as usize] = Some(color);
            }
        }
    }

    let mut out = fold(&canvas, CANVAS_H, w);
    out.push(Line::from(Span::styled(
        "─".repeat(w),
        Style::default().fg(GROUND),
    )));
    out
}

/// A stationary `SPRITE_W`-wide portrait for the picker list — same fold, no
/// ground line, no headroom and no movement, so the rows line up next to a label.
pub fn portrait(ch: &Character, frame: usize) -> Vec<Line<'static>> {
    let art = &ch.frames[frame % ch.frames.len()];
    let mut canvas: Vec<Vec<Option<Color>>> = vec![vec![None; SPRITE_W]; SPRITE_H];
    for (row, line) in art.iter().enumerate() {
        for (col, c) in line.chars().enumerate() {
            canvas[row][col] = color(ch, c);
        }
    }
    fold(&canvas, SPRITE_H, SPRITE_W)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_row_is_exactly_sprite_w_and_uses_known_palette_chars() {
        for ch in ALL {
            for (fi, frame) in ch.frames.iter().enumerate() {
                for (ri, row) in frame.iter().enumerate() {
                    assert_eq!(
                        row.chars().count(),
                        SPRITE_W,
                        "{} frame {fi} row {ri}: {row:?}",
                        ch.id
                    );
                    for c in row.chars() {
                        assert!(
                            matches!(c, 'A' | 'B' | 'C' | 'D' | 'E' | '.'),
                            "{} frame {fi} row {ri}: unknown palette char {c:?}",
                            ch.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_character_actually_animates() {
        for ch in ALL {
            assert_ne!(
                ch.frames[0], ch.frames[1],
                "{}: both frames identical, nothing would move",
                ch.id
            );
        }
    }

    /// The whole point of the redraw: the first attempt looked frightening
    /// because eyes drifted in size and position between characters. Every
    /// character's eyes must sit at exactly row 3, columns 3-4 and 7-8 (the
    /// bunny's treatment), in BOTH frames — so blinking/leg motion can never
    /// move an eye either.
    #[test]
    fn eyes_are_uniform_across_every_character() {
        const EYE_ROW: usize = 3;
        const EYE_COLS: [usize; 4] = [3, 4, 7, 8];
        for ch in ALL {
            for (fi, frame) in ch.frames.iter().enumerate() {
                let found: Vec<usize> = frame
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.contains('C'))
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(
                    found,
                    vec![EYE_ROW],
                    "{} frame {fi}: eyes must appear only on row {EYE_ROW}",
                    ch.id
                );
                let cols: Vec<usize> = frame[EYE_ROW]
                    .chars()
                    .enumerate()
                    .filter(|(_, c)| *c == 'C')
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(cols, EYE_COLS, "{} frame {fi}: eye columns", ch.id);
            }
        }
    }

    #[test]
    fn default_is_the_rubber_duck_and_resolves_to_a_real_character() {
        assert_eq!(ALL[index_of(DEFAULT_ID)].id, "duck");
        assert_eq!(get(index_of("duck")).name, "rubber duck");
    }

    #[test]
    fn unknown_stored_id_falls_back_to_the_default_instead_of_panicking() {
        assert_eq!(ALL[index_of("no-such-character")].id, DEFAULT_ID);
        assert_eq!(ALL[index_of("")].id, DEFAULT_ID);
        // Out-of-range index is clamped, never an index panic.
        assert_eq!(get(usize::MAX).id, ALL[ALL.len() - 1].id);
    }

    #[test]
    fn character_ids_are_unique() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.id, b.id, "duplicate character id {}", a.id);
            }
        }
    }

    #[test]
    fn strip_keeps_a_stable_size_across_a_whole_walk_cycle() {
        let ch = get(index_of("duck"));
        let width = 40u16;
        let span = (width as usize + SPRITE_W) * 2 * TICKS_PER_CELL;
        for tick in 0..span {
            let lines = lines(ch, tick, width);
            assert_eq!(lines.len(), HEIGHT as usize, "row count at tick {tick}");
            for line in &lines {
                assert_eq!(line.width(), width as usize, "line width at tick {tick}");
            }
        }
    }

    #[test]
    fn sweep_reaches_both_edges_and_turns_around_one_cell_at_a_time() {
        let w = 40usize;
        let span = w + SPRITE_W;
        let xs: Vec<(isize, bool)> = (0..(span * 2)).map(|cells| sweep(cells, w)).collect();
        // Fully off-screen on both sides, so it never pops into existence.
        assert_eq!(xs[0], (-(SPRITE_W as isize), true));
        assert!(xs.iter().any(|(x, _)| *x >= w as isize));
        // Both facings occur.
        assert!(xs.iter().any(|(_, right)| *right));
        assert!(xs.iter().any(|(_, right)| !*right));
        // Movement is continuous — no teleporting, including across the wrap.
        for pair in xs.windows(2) {
            assert!(
                (pair[0].0 - pair[1].0).abs() <= 1,
                "jumped from {:?} to {:?}",
                pair[0],
                pair[1]
            );
        }
        let (first, last): (isize, isize) = (xs[0].0, xs[xs.len() - 1].0);
        assert!((first - last).abs() <= 1, "cycle does not close cleanly");
    }

    /// Renders one frame per motion so `--nocapture` shows the actual poses,
    /// and asserts each one is a well-formed strip. The visual check is why
    /// this prints: unit assertions can prove a lift happened but not that the
    /// character *looks* like it is sitting.
    #[test]
    fn each_motion_renders_a_wellformed_strip() {
        let ch = get(index_of("duck"));
        let w = 40u16;
        // The schedule is width-dependent, so find each motion rather than
        // hard-coding tick offsets. Hop is probed at two points in its bounce.
        let find = |want: Motion, nth: usize| -> usize {
            (0..cycle_ticks(w as usize))
                .filter(|t| state_at(*t, w as usize).0 == want)
                .nth(nth)
                .unwrap_or_else(|| panic!("{want:?} never occurs at width {w}"))
        };
        let probes = [
            ("Walk", find(Motion::Walk, 20)),
            ("Pause", find(Motion::Pause, 10)),
            ("Hop a", find(Motion::Hop, 1)),
            ("Hop b", find(Motion::Hop, 5)),
            ("Sit", find(Motion::Sit, 20)),
            ("Sleep", find(Motion::Sleep, 20)),
        ];
        for (label, tick) in probes {
            let (motion, _, _) = state_at(tick, w as usize);
            let rows = lines(ch, tick, w);
            assert_eq!(rows.len(), HEIGHT as usize, "{label}: row count");
            for r in &rows {
                assert_eq!(r.width(), w as usize, "{label}: width");
            }
            println!("--- {label} (tick {tick}) -> {motion:?} ---");
            for r in &rows {
                let text: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
                println!("|{text}|");
            }
        }
    }

    /// Widths the strip realistically gets, including the narrow end.
    const WIDTHS: [u16; 5] = [20, 26, 40, 80, 120];

    /// The bug the position-keyed model exists to prevent: a stationary pose
    /// froze wherever the sweep happened to be, which could be off the edge —
    /// the strip then sat empty for the whole beat. Every stop must now land
    /// fully on screen, at every width.
    #[test]
    fn a_stationary_pose_is_always_on_screen() {
        let ch = get(index_of("duck"));
        for w in WIDTHS {
            let mut stationary = 0;
            for tick in 0..cycle_ticks(w as usize) {
                let (motion, cells, _) = state_at(tick, w as usize);
                if motion.advances() {
                    continue;
                }
                stationary += 1;
                let (x, _) = sweep(cells, w as usize);
                assert!(
                    x >= 0 && x + (SPRITE_W as isize) <= w as isize,
                    "w={w} tick={tick} {motion:?}: parked at x={x}, not fully visible"
                );
                // And it must actually paint a body, not a sliver.
                let painted: usize = lines(ch, tick, w)
                    .iter()
                    .take(HEIGHT as usize - 1)
                    .map(|l| l.spans.iter().filter(|s| s.content.as_ref() != " ").count())
                    .sum();
                assert!(
                    painted >= SPRITE_W,
                    "w={w} tick={tick} {motion:?}: painted only {painted} cells"
                );
            }
            assert!(stationary > 0, "w={w}: no stop ever happened");
        }
    }

    #[test]
    fn every_motion_occurs_within_one_cycle_at_every_width() {
        use std::collections::HashSet;
        for w in WIDTHS {
            let seen: HashSet<Motion> = (0..cycle_ticks(w as usize))
                .map(|t| state_at(t, w as usize).0)
                .collect();
            for m in [
                Motion::Walk,
                Motion::Pause,
                Motion::Hop,
                Motion::Sit,
                Motion::Sleep,
            ] {
                assert!(seen.contains(&m), "w={w}: {m:?} never occurs");
            }
        }
    }

    #[test]
    fn stops_hold_position_and_movement_never_skips_a_cell() {
        for w in WIDTHS {
            let cyc = cycle_ticks(w as usize);
            for tick in 0..cyc {
                let (m0, c0, _) = state_at(tick, w as usize);
                let (m1, c1, _) = state_at(tick + 1, w as usize);
                if !m0.advances() && m0 == m1 {
                    assert_eq!(c0, c1, "w={w} tick={tick}: {m0:?} advanced");
                }
                // Never more than one cell per tick, so nothing teleports.
                assert!(
                    c1 == c0 || c1 == c0 + 1 || c1 < c0,
                    "w={w} tick={tick}: jumped {c0} -> {c1}"
                );
            }
        }
    }

    #[test]
    fn stops_are_sorted_so_the_walk_through_cannot_miss_one() {
        for w in WIDTHS {
            let stops = stops_for(w as usize);
            for pair in stops.windows(2) {
                assert!(
                    pair[0].at < pair[1].at,
                    "w={w}: stops out of order ({} then {})",
                    pair[0].at,
                    pair[1].at
                );
            }
            assert!(
                stops.last().unwrap().at < (w as usize + SPRITE_W) * 2,
                "w={w}: a stop sits past the end of the sweep"
            );
        }
    }

    #[test]
    fn a_hop_leaves_the_ground_and_lands_again() {
        let ch = get(0);
        let lifts: Vec<usize> = (0..TICKS_PER_HOP)
            .map(|i| pose(ch, Motion::Hop, i, i).1)
            .collect();
        assert!(lifts.contains(&HOP_LIFT), "hop never lifts off");
        assert!(lifts.contains(&0), "hop never lands");
    }

    #[test]
    fn the_cycle_repeats_exactly() {
        for w in WIDTHS {
            let cyc = cycle_ticks(w as usize);
            for probe in [0usize, 7, 123, cyc - 1] {
                assert_eq!(
                    state_at(probe, w as usize),
                    state_at(probe + cyc, w as usize),
                    "w={w}: cycle does not repeat at tick {probe}"
                );
            }
        }
    }

    /// Mirrors `state_at`'s own cycle length, so the tests above can sweep
    /// exactly one full pass without hard-coding it.
    fn cycle_ticks(width: usize) -> usize {
        let total_cells = (width + SPRITE_W) * 2;
        let stop_ticks: usize = stops_for(width).iter().map(|s| s.ticks).sum();
        total_cells * TICKS_PER_CELL + stop_ticks
    }

    #[test]
    fn poses_keep_the_eyes_uniform() {
        // The anti-scary invariant must hold for sit/sleep too, not just walk.
        for ch in ALL {
            for (label, art) in [("sit", &ch.sit), ("sleep", &ch.sleep)] {
                let rows: Vec<usize> = art
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.contains('C'))
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(rows, vec![3], "{} {label}: eyes must be on row 3", ch.id);
                let cols: Vec<usize> = art[3]
                    .chars()
                    .enumerate()
                    .filter(|(_, c)| *c == 'C')
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(cols, vec![3, 4, 7, 8], "{} {label}: eye columns", ch.id);
                for (ri, row) in art.iter().enumerate() {
                    assert_eq!(
                        row.chars().count(),
                        SPRITE_W,
                        "{} {label} row {ri}: {row:?}",
                        ch.id
                    );
                }
            }
            // A pose that matches a walk frame's lower body would be invisible.
            for (label, art) in [("sit", &ch.sit), ("sleep", &ch.sleep)] {
                for (wi, walk) in ch.frames.iter().enumerate() {
                    assert_ne!(
                        art[5..],
                        walk[5..],
                        "{} {label}: lower body identical to walk frame {wi}",
                        ch.id
                    );
                }
            }
            assert_ne!(
                ch.sit[5..],
                ch.sleep[5..],
                "{}: sit and sleep look the same",
                ch.id
            );
        }
    }

    #[test]
    fn too_narrow_an_area_draws_nothing_rather_than_a_clipped_sprite() {
        let ch = get(0);
        assert!(lines(ch, 0, 0).is_empty());
        assert!(lines(ch, 0, SPRITE_W as u16).is_empty());
        assert!(!lines(ch, 0, (SPRITE_W + 2) as u16).is_empty());
    }

    #[test]
    fn mirroring_preserves_the_silhouette_width() {
        // Walking left must cover the same columns as walking right, so the
        // character doesn't appear to shrink when it turns around.
        let ch = get(index_of("octopus"));
        let w = 40u16;
        let painted = |tick: usize| -> usize {
            lines(ch, tick, w)
                .iter()
                .take(SPRITE_H / 2)
                .map(|l| l.spans.iter().filter(|s| s.content.as_ref() != " ").count())
                .sum()
        };
        // A tick where it walks right vs. the mirrored point walking left.
        let span = w as usize + SPRITE_W;
        let mid = span / 2;
        let right = painted(mid * TICKS_PER_CELL);
        let left = painted((span + mid) * TICKS_PER_CELL);
        assert_eq!(right, left, "mirrored sprite covers a different area");
    }

    #[test]
    fn portrait_is_sprite_sized_and_has_no_ground_line() {
        for ch in ALL {
            let p = portrait(ch, 0);
            assert_eq!(p.len(), SPRITE_H / 2, "{}: portrait rows", ch.id);
            for line in &p {
                assert_eq!(line.width(), SPRITE_W, "{}: portrait width", ch.id);
            }
        }
    }
}
