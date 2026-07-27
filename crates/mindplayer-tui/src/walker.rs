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

/// Sprite rows plus the ground line the character walks on.
pub const HEIGHT: u16 = (SPRITE_H / 2) as u16 + 1;

/// Ticks per cell of horizontal travel (larger = slower).
const TICKS_PER_CELL: usize = 3;
/// Ticks each walk frame is held.
const TICKS_PER_FRAME: usize = 6;

const GROUND: Color = Color::Rgb(60, 70, 88);

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

/// Fold a pixel canvas (`SPRITE_H` rows of `width` optional colors) into
/// terminal rows.
fn fold(canvas: &[Vec<Option<Color>>], width: usize) -> Vec<Line<'static>> {
    (0..SPRITE_H)
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

/// Where the character is at `tick`, as `(x, facing_right)`. `x` may be
/// negative or past `width` — it walks fully off both edges before turning
/// around, so it never "pops" at the boundary.
fn position(tick: usize, width: usize) -> (isize, bool) {
    let span = width + SPRITE_W;
    let pos = (tick / TICKS_PER_CELL) % (span * 2);
    if pos < span {
        (pos as isize - SPRITE_W as isize, true)
    } else {
        ((span * 2 - pos) as isize - SPRITE_W as isize, false)
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
    let (x, facing_right) = position(tick, w);
    let frame = &ch.frames[(tick / TICKS_PER_FRAME) % ch.frames.len()];

    let mut canvas: Vec<Vec<Option<Color>>> = vec![vec![None; w]; SPRITE_H];
    for (row, art) in frame.iter().enumerate() {
        for (col, c) in art.chars().enumerate() {
            let Some(color) = color(ch, c) else { continue };
            // Mirror instead of storing a second art set for the other facing.
            let sx = if facing_right {
                col
            } else {
                SPRITE_W - 1 - col
            };
            let dx = x + sx as isize;
            if dx >= 0 && (dx as usize) < w {
                canvas[row][dx as usize] = Some(color);
            }
        }
    }

    let mut out = fold(&canvas, w);
    out.push(Line::from(Span::styled(
        "─".repeat(w),
        Style::default().fg(GROUND),
    )));
    out
}

/// A stationary `SPRITE_W`-wide portrait for the picker list — same fold, no
/// ground line and no movement, so the rows line up next to a label.
pub fn portrait(ch: &Character, frame: usize) -> Vec<Line<'static>> {
    let art = &ch.frames[frame % ch.frames.len()];
    let mut canvas: Vec<Vec<Option<Color>>> = vec![vec![None; SPRITE_W]; SPRITE_H];
    for (row, line) in art.iter().enumerate() {
        for (col, c) in line.chars().enumerate() {
            canvas[row][col] = color(ch, c);
        }
    }
    fold(&canvas, SPRITE_W)
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
    fn walk_reaches_both_edges_and_turns_around_one_cell_at_a_time() {
        let w = 40usize;
        let span = w + SPRITE_W;
        let mut xs = Vec::new();
        for step in 0..(span * 2) {
            xs.push(position(step * TICKS_PER_CELL, w));
        }
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
        let (first, last) = (xs[0].0, xs[xs.len() - 1].0);
        assert!((first - last).abs() <= 1, "cycle does not close cleanly");
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
