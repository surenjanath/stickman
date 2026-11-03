//! The jail-break intro: the figure starts locked in a cell, rattles the bars,
//! kicks the door off and goes after the cursor.
//!
//! This module is the cell (drawing, flying bars) and the timeline. It emits
//! [`Cue`]s; `main.rs` turns them into the figure's actions, speech and sound.

use crate::geom::{v2, V2};
use crate::render::DrawList;

const STEEL: [f32; 4] = [0.5, 0.53, 0.58, 1.0];
const STEEL_DARK: [f32; 4] = [0.16, 0.17, 0.2, 1.0];
const GRAVITY: f32 = 1900.0;

/// Timeline, seconds.
const T_RATTLE: f32 = 1.6;
const T_RATTLE_END: f32 = 3.4;
const T_GRUMBLE: f32 = 3.6;
const T_KICK: f32 = 4.4;
/// When the kick connects.
const T_BREAK: f32 = 4.62;
const T_FREE: f32 = 5.2;
/// The broken cell lingers, then fades.
const T_FADE: f32 = 11.0;
const FADE: f32 = 2.0;

/// Something the rest of the app should do now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cue {
    Say(&'static str),
    /// Grab the bars and shake them.
    Rattle,
    /// A bar clanks (sound).
    Clank,
    /// Kick the door (facing `dir`).
    Kick {
        dir: f32,
    },
    /// The door flew off at `at`.
    Break {
        at: V2,
    },
    /// Out: go get the cursor.
    Free,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Piece {
    /// Centre, velocity, rotation and spin; `half` is half the bar's length.
    pos: V2,
    vel: V2,
    rot: f32,
    spin: f32,
    half: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Jail {
    /// Floor centre of the cell (world points).
    base: V2,
    /// Size factor (1 for a medium figure).
    k: f32,
    /// The door side: +1 right, -1 left.
    door: f32,
    pub t: f32,
    broken: bool,
    pieces: Vec<Piece>,
    next_clank: f32,
}

impl Jail {
    /// A cell around a figure standing at `feet`, of rig `scale`, whose door
    /// is on the `door` side (+1 right, -1 left).
    pub fn new(feet: V2, scale: f32, door: f32) -> Jail {
        Jail {
            base: feet,
            k: scale / 0.55,
            door: door.signum(),
            t: 0.0,
            broken: false,
            pieces: Vec::new(),
            next_clank: T_RATTLE,
        }
    }

    fn half_width(&self) -> f32 {
        70.0 * self.k
    }

    fn height(&self) -> f32 {
        190.0 * self.k
    }

    /// x offsets of the bars from the centre.
    fn bar_xs(&self) -> Vec<f32> {
        let n = 8;
        let w = self.half_width();
        (0..n)
            .map(|i| -w + 2.0 * w * i as f32 / (n - 1) as f32)
            .collect()
    }

    /// Bars on the door half that blow off.
    fn is_door_bar(&self, x: f32) -> bool {
        x * self.door > 0.0 && x.abs() < self.half_width() - 1.0
    }

    pub fn broken(&self) -> bool {
        self.broken
    }

    /// Over: faded away.
    pub fn done(&self) -> bool {
        self.t > T_FADE + FADE
    }

    /// Still locked up (the figure must stay put).
    pub fn locked(&self) -> bool {
        self.t < T_FREE
    }

    /// The door blows off now (the user clicked the figure, say).
    pub fn break_now(&mut self, cues: &mut Vec<Cue>) {
        if self.broken {
            return;
        }
        self.broken = true;
        let h = self.height();
        for x in self.bar_xs() {
            if self.is_door_bar(x) {
                let center = self.base + v2(x, -h / 2.0);
                let out = self.door * (450.0 + 260.0 * x.abs() / self.half_width());
                self.pieces.push(Piece {
                    pos: center,
                    vel: v2(out, -380.0 - 120.0 * (x.abs() / self.half_width())),
                    rot: 0.0,
                    spin: self.door * (9.0 + x.abs() * 0.05),
                    half: h / 2.0,
                });
            }
        }
        cues.push(Cue::Break {
            at: self.base + v2(self.door * self.half_width() * 0.5, -h * 0.45),
        });
        if self.t < T_FREE {
            self.t = T_FREE - 0.3; // skip ahead: out it comes
        }
    }

    /// Advance by `dt`; returns what happens now.
    pub fn update(&mut self, dt: f32) -> Vec<Cue> {
        let mut cues = Vec::new();
        let (t0, t1) = (self.t, self.t + dt);
        let crossed = |at: f32| t0 < at && t1 >= at || (t0 == 0.0 && at == 0.0);
        self.t = t1;
        if crossed(0.0) {
            cues.push(Cue::Say("Hey! Where am I?"));
        }
        if crossed(T_RATTLE) {
            cues.push(Cue::Say("Let me out!"));
            cues.push(Cue::Rattle);
        }
        if (T_RATTLE..T_RATTLE_END).contains(&t1) && t1 >= self.next_clank && !self.broken {
            self.next_clank = t1 + 0.28;
            cues.push(Cue::Clank);
        }
        if crossed(T_GRUMBLE) && !self.broken {
            cues.push(Cue::Say("Fine. I'll do it myself."));
        }
        if crossed(T_KICK) && !self.broken {
            cues.push(Cue::Kick { dir: self.door });
        }
        if crossed(T_BREAK) {
            self.break_now(&mut cues);
        }
        if crossed(T_FREE) {
            cues.push(Cue::Say("Now… where's that cursor?"));
            cues.push(Cue::Free);
        }
        // Flying bars: tumble and bounce on the floor.
        let floor = self.base.y;
        for p in &mut self.pieces {
            p.vel.y += GRAVITY * dt;
            p.pos += p.vel * dt;
            p.rot += p.spin * dt;
            // Lowest end of the bar.
            let reach = (p.half * p.rot.cos().abs()).max(4.0);
            if p.pos.y + reach > floor {
                p.pos.y = floor - reach;
                if p.vel.y > 120.0 {
                    p.vel.y = -p.vel.y * 0.35;
                    p.vel.x *= 0.6;
                    p.spin *= 0.5;
                } else {
                    // Settle flat.
                    p.vel = V2::ZERO;
                    p.spin = 0.0;
                    p.rot += (std::f32::consts::FRAC_PI_2 - p.rot.rem_euclid(std::f32::consts::PI))
                        * 0.2;
                }
            }
        }
        cues
    }

    /// Something moves every frame (always, while it exists).
    pub fn animating(&self) -> bool {
        !self.done()
    }

    pub fn draw(&self, out: &mut DrawList) {
        let alpha = 1.0 - ((self.t - T_FADE) / FADE).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let a = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * alpha];
        let (w, h) = (self.half_width(), self.height());
        let rattling = (T_RATTLE..T_RATTLE_END).contains(&self.t) && !self.broken;
        let shake = if rattling {
            2.2 * (self.t * 47.0).sin() * self.k
        } else {
            0.0
        };
        let bar = 3.0 * self.k;
        let rail = 4.5 * self.k;
        let base = self.base + v2(shake, 0.0);
        let mut line = |p: V2, q: V2, half: f32| {
            out.capsule(p, q, half + 1.4, a(STEEL_DARK));
            out.capsule(p, q, half, a(STEEL));
        };
        for x in self.bar_xs() {
            if self.broken && self.is_door_bar(x) {
                continue;
            }
            line(base + v2(x, 0.0), base + v2(x, -h), bar);
        }
        // Rails: the door half of the top and bottom rails goes with the door.
        for y in [0.0, -h] {
            if self.broken {
                let edge = self.door * w;
                line(base + v2(-edge, y), base + v2(0.0, y), rail);
            } else {
                line(base + v2(-w, y), base + v2(w, y), rail);
            }
        }
        for p in &self.pieces {
            let d = v2(p.rot.sin(), -p.rot.cos()) * p.half;
            line(p.pos - d, p.pos + d, bar);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(j: &mut Jail, secs: f32) -> Vec<Cue> {
        let mut all = Vec::new();
        for _ in 0..(secs * 60.0) as usize {
            all.extend(j.update(1.0 / 60.0));
        }
        all
    }

    #[test]
    fn the_breakout_plays_in_order() {
        let mut j = Jail::new(v2(900.0, 1100.0), 0.55, 1.0);
        let cues = run(&mut j, 6.0);
        let at = |f: &dyn Fn(&Cue) -> bool| cues.iter().position(f).expect("missing cue");
        let rattle = at(&|c| *c == Cue::Rattle);
        let kick = at(&|c| matches!(c, Cue::Kick { dir } if *dir == 1.0));
        let brk = at(&|c| matches!(c, Cue::Break { .. }));
        let free = at(&|c| *c == Cue::Free);
        assert!(matches!(cues[0], Cue::Say(_)));
        assert!(rattle < kick && kick < brk && brk < free);
        assert!(cues.iter().filter(|c| **c == Cue::Clank).count() >= 4);
        assert!(j.broken() && !j.locked());
    }

    #[test]
    fn door_bars_fly_out_and_settle_on_the_floor_then_it_fades() {
        let mut j = Jail::new(v2(900.0, 1100.0), 0.55, -1.0);
        run(&mut j, T_BREAK + 0.05);
        assert!(!j.pieces.is_empty());
        assert!(
            j.pieces.iter().all(|p| p.vel.x < 0.0),
            "fly out of the left door"
        );
        run(&mut j, 4.0);
        for p in &j.pieces {
            assert!(
                p.pos.y <= 1100.0 && p.pos.y > 1080.0,
                "lying on the floor: {:?}",
                p.pos
            );
            assert!(p.pos.x < 900.0 - 70.0, "outside the cell: {:?}", p.pos);
            assert_eq!(p.vel, V2::ZERO);
        }
        run(&mut j, T_FADE + FADE);
        assert!(j.done());
        let mut dl = DrawList::default();
        j.draw(&mut dl);
        assert!(dl.is_empty());
    }

    #[test]
    fn a_click_breaks_it_open_early() {
        let mut j = Jail::new(v2(900.0, 1100.0), 0.55, 1.0);
        run(&mut j, 1.0);
        let mut cues = Vec::new();
        j.break_now(&mut cues);
        assert!(matches!(cues[0], Cue::Break { .. }));
        let later = run(&mut j, 1.0);
        assert!(later.contains(&Cue::Free), "comes out right away");
        assert!(!later.iter().any(|c| matches!(c, Cue::Kick { .. })));
    }

    #[test]
    fn draws_bars_and_rails() {
        let j = Jail::new(v2(900.0, 1100.0), 0.55, 1.0);
        let mut dl = DrawList::default();
        j.draw(&mut dl);
        assert_eq!(dl.len(), 2 * (8 + 2), "8 bars + 2 rails, outlined");
    }
}
