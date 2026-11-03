//! Hats the figure earns with achievements and levels, drawn on its head
//! from SDF primitives so they tilt and tumble with it.

use serde::{Deserialize, Serialize};

use crate::geom::{v2, V2};
use crate::render::DrawList;
use crate::stats::Stats;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
pub enum Hat {
    #[default]
    None,
    Bandana,
    TopHat,
    Crown,
    Pirate,
    Party,
    Wizard,
    Horns,
    Viking,
}

impl Hat {
    pub const ALL: [Hat; 9] = [
        Hat::None,
        Hat::Bandana,
        Hat::TopHat,
        Hat::Crown,
        Hat::Pirate,
        Hat::Party,
        Hat::Wizard,
        Hat::Horns,
        Hat::Viking,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Hat::None => "No hat",
            Hat::Bandana => "Ninja bandana",
            Hat::TopHat => "Top hat",
            Hat::Crown => "Crown",
            Hat::Pirate => "Pirate hat",
            Hat::Party => "Party hat",
            Hat::Wizard => "Wizard hat",
            Hat::Horns => "Devil horns",
            Hat::Viking => "Viking helmet",
        }
    }

    /// How to earn it (`None`: always available).
    pub fn how(self) -> Option<&'static str> {
        Some(match self {
            Hat::None => return None,
            Hat::Bandana => "reach level 2",
            Hat::TopHat => "reach level 5 (Veteran)",
            Hat::Crown => "win 3 duels (Desk Champion)",
            Hat::Pirate => "throw a window (Window Wrecker)",
            Hat::Party => "be petted 20 times (Beloved)",
            Hat::Wizard => "reflect 10 times (Philosopher)",
            Hat::Horns => "lose its temper (Seething)",
            Hat::Viking => "win a boss fight (Boss Slayer)",
        })
    }

    pub fn unlocked(self, s: &Stats) -> bool {
        match self {
            Hat::None => true,
            Hat::Bandana => s.level() >= 2,
            Hat::TopHat => s.level() >= 5,
            Hat::Crown => s.has("desk_champion"),
            Hat::Pirate => s.has("window_wrecker"),
            Hat::Party => s.has("beloved"),
            Hat::Wizard => s.has("philosopher"),
            Hat::Horns => s.has("seething"),
            Hat::Viking => s.has("boss_slayer"),
        }
    }

    /// Draw on a head ring of radius `r` centred at `c`, with `up` the head's
    /// up direction (unit).
    pub fn draw(self, out: &mut DrawList, c: V2, up: V2, r: f32) {
        let right = v2(-up.y, up.x);
        let at = |x: f32, y: f32| c + right * (x * r) + up * (y * r);
        // Strokes are collected, then drawn as one white halo (keeps hats
        // readable on dark desktops) under all the colour, so overlapping
        // strokes read as one solid shape.
        let mut strokes: Vec<(V2, V2, f32, [f32; 4])> = Vec::new();
        let mut ink = |a: V2, b: V2, half: f32, color: [f32; 4]| strokes.push((a, b, half, color));
        let mut dots: Vec<(V2, f32, [f32; 4])> = Vec::new();
        let s = r / 22.0; // stroke scale
        match self {
            Hat::None => {}
            Hat::Bandana => {
                let band = [0.1, 0.45, 0.25, 1.0];
                ink(at(-1.0, 0.35), at(1.0, 0.35), 3.0 * s, band);
                ink(at(-1.0, 0.35), at(-1.7, 0.1), 2.0 * s, band);
                ink(at(-1.0, 0.35), at(-1.6, 0.55), 2.0 * s, band);
            }
            Hat::TopHat => {
                let black = [0.08, 0.08, 0.1, 1.0];
                ink(at(-1.25, 0.95), at(1.25, 0.95), 2.4 * s, black);
                for k in 0..6 {
                    let x = -0.55 + 0.22 * k as f32;
                    ink(at(x, 1.0), at(x, 2.0), 4.0 * s, black);
                }
                ink(
                    at(-0.7, 1.15),
                    at(0.7, 1.15),
                    2.2 * s,
                    [0.75, 0.1, 0.1, 1.0],
                );
            }
            Hat::Crown => {
                let gold = [0.95, 0.75, 0.15, 1.0];
                ink(at(-0.85, 0.9), at(0.85, 0.9), 3.0 * s, gold);
                for x in [-0.8, 0.0, 0.8] {
                    ink(at(x * 0.95, 0.9), at(x, 1.65), 2.4 * s, gold);
                }
                ink(at(-0.4, 0.9), at(-0.4, 1.3), 2.4 * s, gold);
                ink(at(0.4, 0.9), at(0.4, 1.3), 2.4 * s, gold);
                dots.push((at(0.0, 1.0), 2.6 * s, [0.85, 0.1, 0.2, 1.0]));
            }
            Hat::Pirate => {
                let black = [0.1, 0.08, 0.08, 1.0];
                ink(at(-1.45, 0.85), at(1.45, 0.85), 3.0 * s, black);
                ink(at(-1.45, 0.85), at(0.0, 1.7), 4.0 * s, black);
                ink(at(1.45, 0.85), at(0.0, 1.7), 4.0 * s, black);
                ink(at(-0.6, 1.1), at(0.6, 1.1), 5.0 * s, black);
                dots.push((at(0.0, 1.2), 3.0 * s, [0.95, 0.95, 0.95, 1.0]));
            }
            Hat::Party => {
                let colors = [
                    [0.95, 0.3, 0.5, 1.0],
                    [0.3, 0.6, 0.95, 1.0],
                    [0.98, 0.8, 0.2, 1.0],
                ];
                for k in 0..6 {
                    let y = 0.9 + 0.17 * k as f32;
                    let w = 0.75 * (1.0 - k as f32 / 6.0);
                    ink(at(-w, y), at(w, y), 2.6 * s, colors[k % 3]);
                }
                dots.push((at(0.0, 2.0), 3.6 * s, [1.0, 1.0, 1.0, 1.0]));
            }
            Hat::Wizard => {
                let purple = [0.35, 0.15, 0.65, 1.0];
                ink(at(-1.3, 0.9), at(1.3, 0.9), 2.4 * s, purple);
                for k in 0..7 {
                    let y = 0.95 + 0.25 * k as f32;
                    let w = 0.7 * (1.0 - k as f32 / 7.0);
                    let lean = 0.06 * k as f32;
                    ink(at(-w - lean, y), at(w - lean, y), 2.8 * s, purple);
                }
                dots.push((at(0.1, 1.45), 2.8 * s, [1.0, 0.85, 0.2, 1.0]));
            }
            Hat::Horns => {
                let red = [0.8, 0.08, 0.08, 1.0];
                ink(at(-0.7, 0.75), at(-1.0, 1.3), 3.2 * s, red);
                ink(at(-1.0, 1.3), at(-0.8, 1.7), 2.2 * s, red);
                ink(at(0.7, 0.75), at(1.0, 1.3), 3.2 * s, red);
                ink(at(1.0, 1.3), at(0.8, 1.7), 2.2 * s, red);
            }
            Hat::Viking => {
                let steel = [0.6, 0.62, 0.66, 1.0];
                let bone = [0.96, 0.93, 0.85, 1.0];
                for k in 0..4 {
                    let y = 0.7 + 0.18 * k as f32;
                    let w = 1.05 * (1.0 - (k as f32 / 4.0).powi(2));
                    ink(at(-w, y), at(w, y), 3.0 * s, steel);
                }
                ink(at(-1.0, 0.8), at(-1.55, 1.4), 2.8 * s, bone);
                ink(at(-1.55, 1.4), at(-1.4, 1.8), 2.0 * s, bone);
                ink(at(1.0, 0.8), at(1.55, 1.4), 2.8 * s, bone);
                ink(at(1.55, 1.4), at(1.4, 1.8), 2.0 * s, bone);
            }
        }
        for &(a, b, half, _) in &strokes {
            out.capsule(a, b, half + 1.2, [1.0, 1.0, 1.0, 0.75]);
        }
        for &(a, b, half, color) in &strokes {
            out.capsule(a, b, half, color);
        }
        for (c, r, color) in dots {
            out.disc(c, r, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hats_unlock_with_achievements_and_levels() {
        let mut s = Stats::default();
        assert!(Hat::None.unlocked(&s));
        assert!(!Hat::Bandana.unlocked(&s));
        s.xp = 30;
        assert!(Hat::Bandana.unlocked(&s));
        assert!(!Hat::Crown.unlocked(&s));
        s.duels_won = 3;
        s.check_achievements(0);
        assert!(Hat::Crown.unlocked(&s));
        assert!(Hat::ALL
            .iter()
            .all(|h| *h == Hat::None || h.how().is_some()));
    }

    #[test]
    fn every_hat_draws_something_except_none() {
        for h in Hat::ALL {
            let mut dl = DrawList::default();
            h.draw(&mut dl, v2(100.0, 100.0), v2(0.0, -1.0), 12.0);
            assert_eq!(dl.is_empty(), h == Hat::None, "{h:?}");
        }
    }
}
