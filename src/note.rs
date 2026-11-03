//! Ransom notes: the figure spells a message to the user with letters torn
//! off the screen. Each letter that appears in the active window is cut out
//! (pixels and a scrap of its page) and flies into place; letters that
//! aren't on screen are cut-out tiles in a random colour. The note hangs for
//! a while, then the letters drop and fade.
//!
//! Pure layout and animation; `main.rs` captures the letters and makes the
//! sprites.

use crate::geom::{v2, Rect, V2};
use crate::render::{DrawList, SpriteDraw};

/// Tile height, points.
const TILE_H: f32 = 34.0;
const GAP: f32 = 4.0;
const SPACE: f32 = 16.0;
/// Letters per line before wrapping.
const LINE_CHARS: usize = 12;
/// Each letter takes off this long after the previous one.
const STAGGER: f32 = 0.08;
const FLY: f32 = 0.6;
/// The note hangs this long after the last letter lands.
const HOLD: f32 = 10.0;
const DROP: f32 = 2.2;

/// Paper colours for cut-out tiles.
const PAPERS: [[f32; 3]; 6] = [
    [1.0, 0.95, 0.55],
    [0.75, 0.9, 1.0],
    [1.0, 0.78, 0.82],
    [0.8, 1.0, 0.8],
    [1.0, 1.0, 1.0],
    [0.95, 0.85, 1.0],
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub ch: char,
    /// Where it was torn from (a letter on screen), if anywhere.
    pub source: Option<Rect>,
    /// Sprite and its drawn half size, once made.
    pub sprite: Option<(u32, V2)>,
    pub from: V2,
    pub pos: V2,
    pub target: V2,
    pub rot: f32,
    /// Seconds until it takes off.
    pub delay: f32,
    pub paper: [f32; 3],
    vel: V2,
    spin: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
    pub tiles: Vec<Tile>,
    pub t: f32,
}

/// Lines of `text`, wrapped at word boundaries.
fn wrap(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= LINE_CHARS => {
                l.push(' ');
                l.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

/// The rough width of a tile for `ch` before its sprite exists.
fn tile_w(ch: char) -> f32 {
    if ch.is_ascii_digit() || ch.is_alphabetic() {
        if "MW".contains(ch) {
            32.0
        } else if "IJL1!".contains(ch) {
            18.0
        } else {
            26.0
        }
    } else {
        20.0
    }
}

impl Note {
    /// Plan `text` centred under `top_center`, kept between `min_x` and
    /// `max_x`. `letters` are on-screen letters (upper case, global rects) to
    /// tear from; cut-outs fly from `hands`. `seed` picks colours and tilts.
    pub fn plan(
        text: &str,
        letters: &[(char, Rect)],
        top_center: V2,
        min_x: f32,
        max_x: f32,
        hands: V2,
        seed: u64,
    ) -> Note {
        let mut rng = seed | 1;
        let mut rand = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut used: Vec<usize> = Vec::new();
        let mut tiles = Vec::new();
        let mut order = 0;
        for (row, line) in wrap(text).iter().enumerate() {
            let width: f32 = line
                .chars()
                .map(|c| if c == ' ' { SPACE } else { tile_w(c) + GAP })
                .sum();
            let left = (top_center.x - width / 2.0).clamp(min_x, (max_x - width).max(min_x));
            let y = top_center.y + row as f32 * (TILE_H + 10.0) + TILE_H / 2.0;
            let mut x = left;
            for ch in line.chars() {
                if ch == ' ' {
                    x += SPACE;
                    continue;
                }
                let w = tile_w(ch);
                let target = v2(x + w / 2.0, y + (rand() - 0.5) * 6.0);
                x += w + GAP;
                // A matching letter on screen, not used yet.
                let up = ch.to_ascii_uppercase();
                let source = letters
                    .iter()
                    .enumerate()
                    .filter(|(i, (c, _))| *c == up && !used.contains(i))
                    .map(|(i, (_, r))| (i, *r))
                    .next();
                if let Some((i, _)) = source {
                    used.push(i);
                }
                let from = source.map_or(hands, |(_, r)| {
                    v2((r.x + r.width / 2.0) as f32, (r.y + r.height / 2.0) as f32)
                });
                tiles.push(Tile {
                    ch,
                    source: source.map(|(_, r)| r),
                    sprite: None,
                    from,
                    pos: from,
                    target,
                    rot: (rand() - 0.5) * 0.35,
                    delay: order as f32 * STAGGER,
                    paper: PAPERS[(rand() * PAPERS.len() as f32) as usize % PAPERS.len()],
                    vel: V2::ZERO,
                    spin: 0.0,
                });
                order += 1;
            }
        }
        Note {
            text: text.to_string(),
            tiles,
            t: 0.0,
        }
    }

    /// When the last letter lands, seconds.
    fn assembled_at(&self) -> f32 {
        self.tiles.last().map_or(0.0, |t| t.delay + FLY)
    }

    pub fn done(&self) -> bool {
        self.t > self.assembled_at() + HOLD + DROP
    }

    pub fn update(&mut self, dt: f32, floor: f32) {
        self.t += dt;
        let dropping = self.t > self.assembled_at() + HOLD;
        for tile in &mut self.tiles {
            if dropping {
                if tile.vel == V2::ZERO {
                    tile.vel = v2(0.0, -60.0);
                    tile.spin = tile.rot.signum() * 2.0;
                }
                tile.vel.y += 1400.0 * dt;
                tile.pos += tile.vel * dt;
                tile.rot += tile.spin * dt;
                if tile.pos.y > floor - 12.0 {
                    tile.pos.y = floor - 12.0;
                    tile.vel = v2(0.0, 0.0001);
                    tile.spin = 0.0;
                }
                continue;
            }
            let k = ((self.t - tile.delay) / FLY).clamp(0.0, 1.0);
            let e = crate::stickman::smoothstep(k);
            // An arc: up and over.
            let lift = (k * std::f32::consts::PI).sin() * 60.0;
            tile.pos = tile.from.lerp(tile.target, e) - v2(0.0, lift);
        }
    }

    /// Whether the letters are moving (frames needed).
    pub fn animating(&self) -> bool {
        let a = self.assembled_at();
        self.t < a + 0.1 || self.t > a + HOLD
    }

    fn alpha(&self) -> f32 {
        let fade_from = self.assembled_at() + HOLD + DROP * 0.5;
        (1.0 - (self.t - fade_from) / (DROP * 0.5)).clamp(0.0, 1.0)
    }

    pub fn draw(&self, out: &mut DrawList) {
        let a = self.alpha();
        for tile in &self.tiles {
            if self.t < tile.delay {
                continue;
            }
            let Some((id, half)) = tile.sprite else {
                continue;
            };
            let p = tile.paper;
            // The scrap of paper it's cut from, with a dark edge.
            out.rect(
                tile.pos,
                half + V2::splat(4.0),
                2.0,
                [0.1, 0.1, 0.12, 0.9 * a],
            );
            if tile.source.is_none() {
                out.rect(tile.pos, half + V2::splat(3.0), 2.0, [p[0], p[1], p[2], a]);
            }
            out.sprite(SpriteDraw {
                id,
                center: tile.pos,
                half,
                rotation: tile.rot,
                alpha: a,
            });
        }
    }

    pub fn sprites(&self) -> Vec<u32> {
        self.tiles
            .iter()
            .filter_map(|t| t.sprite.map(|s| s.0))
            .collect()
    }
}

/// How big to draw a torn-off letter of `rect` (half size): scaled to tile
/// height, but never blown up more than 3×.
pub fn torn_half(rect: Rect) -> V2 {
    let h = (rect.height as f32).max(1.0);
    let k = (TILE_H / h).min(3.0);
    v2(rect.width as f32 * k / 2.0, h * k / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            width: 8.0,
            height: 12.0,
        }
    }

    #[test]
    fn tears_letters_from_the_screen_and_cuts_out_the_rest() {
        let letters = [
            ('I', r(10.0, 10.0)),
            ('S', r(30.0, 10.0)),
            ('E', r(50.0, 10.0)),
            ('E', r(70.0, 10.0)),
        ];
        let n = Note::plan(
            "I SEE YOU",
            &letters,
            v2(900.0, 200.0),
            0.0,
            1800.0,
            v2(900.0, 900.0),
            3,
        );
        let chars: String = n.tiles.iter().map(|t| t.ch).collect();
        assert_eq!(chars, "ISEEYOU");
        let torn = n.tiles.iter().filter(|t| t.source.is_some()).count();
        assert_eq!(torn, 4, "I, S and both Es come off the screen");
        // Each on-screen letter is used once.
        let es: Vec<_> = n
            .tiles
            .iter()
            .filter(|t| t.ch == 'E')
            .map(|t| t.source)
            .collect();
        assert_ne!(es[0], es[1]);
        // Y, O, U are cut out and fly from the hands.
        assert!(n
            .tiles
            .iter()
            .filter(|t| t.source.is_none())
            .all(|t| t.from == v2(900.0, 900.0)));
    }

    #[test]
    fn lines_wrap_and_stay_on_screen() {
        let n = Note::plan(
            "YOU WILL PAY FOR THIS",
            &[],
            v2(20.0, 100.0),
            0.0,
            1800.0,
            V2::ZERO,
            1,
        );
        let rows: std::collections::BTreeSet<i32> =
            n.tiles.iter().map(|t| (t.target.y / 10.0) as i32).collect();
        assert!(rows.len() >= 2, "wrapped onto two lines");
        assert!(n.tiles.iter().all(|t| t.target.x > 0.0));
    }

    #[test]
    fn flies_in_holds_then_drops_and_ends() {
        let mut n = Note::plan(
            "HI",
            &[],
            v2(900.0, 200.0),
            0.0,
            1800.0,
            v2(900.0, 900.0),
            1,
        );
        for t in &mut n.tiles {
            t.sprite = Some((1, v2(10.0, 15.0)));
        }
        for _ in 0..90 {
            n.update(1.0 / 60.0, 1100.0);
        }
        for t in &n.tiles {
            assert!((t.pos - t.target).length() < 1.0, "in place: {:?}", t.pos);
        }
        assert!(!n.animating(), "a hanging note needs no frames");
        let mut dl = DrawList::default();
        n.draw(&mut dl);
        assert!(!dl.is_empty());
        for _ in 0..(14 * 60) {
            n.update(1.0 / 60.0, 1100.0);
        }
        assert!(n.done());
    }
}
