//! The lasso: twirled overhead, thrown at the cursor, and hauled in hand over
//! hand like a horse on a rope. The user can fight it: pulling away drags the
//! figure along and pays out rope, and a hard enough yank snaps it.
//!
//! Pure logic and drawing: the agent feeds in its hand and the cursor each
//! tick, and the app puts the real pointer where [`Lasso::holds`] says.

use crate::geom::{v2, V2};
use crate::render::DrawList;
use crate::stickman::smoothstep;

/// How long it twirls the loop before it throws.
pub const SPIN_TIME: f32 = 0.9;
/// Farthest it can throw (points).
pub const RANGE: f32 = 720.0;
/// The loop flies this fast, with a little gravity.
const SPEED: f32 = 1500.0;
const GRAVITY: f32 = 600.0;
/// A loop this close to the pointer catches it.
const CATCH: f32 = 42.0;
/// A throw that hasn't caught anything by now has missed.
const FLIGHT_TIME: f32 = 0.8;
/// Rope it hauls in per second.
const REEL: f32 = 240.0;
/// Of each point the user pulls away, how much rope it lets out.
const GIVE: f32 = 0.5;
/// Strain (points pulled, fading over `STRAIN_TAU` s) that snaps the rope.
const SNAP: f32 = 320.0;
const STRAIN_TAU: f32 = 0.8;
/// It gives up hauling after this long.
const MAX_PULL: f32 = 8.0;
/// A missed loop is pulled back in this long.
const RETRACT: f32 = 0.35;
/// Reeled in this close to its hands: it has the pointer.
pub const REELED_IN: f32 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// Twirling overhead.
    Spin,
    /// The loop in the air.
    Fly { pos: V2, vel: V2 },
    /// Round the pointer: it's at `at`, `len` points of rope out.
    Caught { at: V2, len: f32 },
    /// Missed: the loop comes back from `from`.
    Retract { from: V2 },
}

/// What a tick of the lasso did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Nothing,
    Caught {
        at: V2,
    },
    Missed,
    /// The user pulled `by` points against the rope this tick, along `dir`
    /// (from the figure toward the pointer).
    Tugged {
        by: f32,
        dir: V2,
    },
    Snapped {
        dir: V2,
    },
    ReeledIn {
        at: V2,
    },
    /// Held on too long: the loop slipped off.
    Slipped,
    /// A missed loop is back in its hand.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lasso {
    pub stage: Stage,
    /// Seconds in this stage.
    pub t: f32,
    strain: f32,
    /// The last pointer position it saw (one sample can span several ticks).
    seen: V2,
}

impl Default for Lasso {
    fn default() -> Self {
        Self::new()
    }
}

impl Lasso {
    pub fn new() -> Lasso {
        Lasso {
            stage: Stage::Spin,
            t: 0.0,
            strain: 0.0,
            seen: V2::ZERO,
        }
    }

    /// Let the loop go from `hand` at `target`, arcing up a little so it
    /// arrives there despite gravity.
    pub fn throw(&mut self, hand: V2, target: V2) {
        let d = target - hand;
        let tof = d.length() / SPEED;
        let mut vel = d.normalize_or(v2(1.0, 0.0)) * SPEED;
        vel.y -= 0.5 * GRAVITY * tof;
        self.stage = Stage::Fly { pos: hand, vel };
        self.t = 0.0;
    }

    pub fn spinning(&self) -> bool {
        self.stage == Stage::Spin
    }

    pub fn flying(&self) -> bool {
        matches!(self.stage, Stage::Fly { .. })
    }

    /// Where the pointer is held, while the loop is round it.
    pub fn holds(&self) -> Option<V2> {
        match self.stage {
            Stage::Caught { at, .. } => Some(at),
            _ => None,
        }
    }

    /// How close the rope is to snapping, 0..1.
    pub fn tension(&self) -> f32 {
        (self.strain / SNAP).min(1.0)
    }

    /// The loop's position (for the pose to aim at, and drawing).
    pub fn loop_pos(&self, hand: V2) -> V2 {
        match self.stage {
            Stage::Spin => hand + v2(0.0, -18.0),
            Stage::Fly { pos, .. } => pos,
            Stage::Caught { at, .. } => at,
            Stage::Retract { from } => from + (hand - from) * smoothstep(self.t / RETRACT),
        }
    }

    /// One tick: `hand` holds the rope, `cursor` is the pointer as sampled.
    pub fn step(&mut self, dt: f32, hand: V2, cursor: V2) -> Step {
        self.t += dt;
        let fresh = cursor != self.seen;
        self.seen = cursor;
        match self.stage {
            Stage::Spin => Step::Nothing,
            Stage::Fly { mut pos, mut vel } => {
                vel.y += GRAVITY * dt;
                pos += vel * dt;
                if (pos - cursor).length() < CATCH {
                    self.stage = Stage::Caught {
                        at: cursor,
                        len: (cursor - hand).length(),
                    };
                    self.t = 0.0;
                    self.strain = 0.0;
                    return Step::Caught { at: cursor };
                }
                if (pos - hand).length() > RANGE || self.t > FLIGHT_TIME {
                    self.stage = Stage::Retract { from: pos };
                    self.t = 0.0;
                    return Step::Missed;
                }
                self.stage = Stage::Fly { pos, vel };
                Step::Nothing
            }
            Stage::Caught { at, mut len } => {
                let d = cursor - hand;
                let dist = d.length();
                let dir = d.normalize_or(v2(1.0, 0.0));
                // The user moved the pointer since we last put it at `at`:
                // the part of that away from the figure strains the rope.
                let pull = if fresh {
                    (cursor - at).dot(dir).max(0.0)
                } else {
                    0.0
                };
                self.strain = self.strain * (-dt / STRAIN_TAU).exp() + pull;
                if self.strain > SNAP {
                    return Step::Snapped { dir };
                }
                if self.t > MAX_PULL {
                    return Step::Slipped;
                }
                len = (len + pull * GIVE - REEL * dt).max(0.0);
                let at = hand + dir * dist.min(len);
                self.stage = Stage::Caught { at, len };
                if dist.min(len) < REELED_IN {
                    return Step::ReeledIn { at };
                }
                if pull > 0.5 {
                    Step::Tugged { by: pull, dir }
                } else {
                    Step::Nothing
                }
            }
            Stage::Retract { .. } if self.t >= RETRACT => Step::Done,
            Stage::Retract { .. } => Step::Nothing,
        }
    }
}

const ROPE: [f32; 4] = [0.80, 0.63, 0.40, 1.0];
const OUTLINE: [f32; 4] = [1.0, 1.0, 1.0, 0.85];

/// Capsules along `points`, outlined so the rope shows on any background.
fn polyline(out: &mut DrawList, points: &[V2], half: f32) {
    for w in points.windows(2) {
        out.capsule(w[0], w[1], half + 1.2, OUTLINE);
    }
    for w in points.windows(2) {
        out.capsule(w[0], w[1], half, ROPE);
    }
}

/// An ellipse of rope round `c` (radii `rx`, `ry`, tilted `tilt` radians).
fn hoop(out: &mut DrawList, c: V2, rx: f32, ry: f32, tilt: f32) {
    let (s, k) = tilt.sin_cos();
    let pts: Vec<V2> = (0..=20)
        .map(|i| {
            let a = i as f32 / 20.0 * std::f32::consts::TAU;
            let (x, y) = (rx * a.cos(), ry * a.sin());
            c + v2(x * k - y * s, x * s + y * k)
        })
        .collect();
    polyline(out, &pts, 1.6);
}

/// The rope from `hand` to `end`, sagging by `sag` in the middle and
/// thrumming when taut (`tension` 0..1). `clock`: seconds.
fn rope(out: &mut DrawList, hand: V2, end: V2, sag: f32, tension: f32, clock: f32) {
    let d = end - hand;
    let normal = v2(-d.y, d.x).normalize_or(v2(0.0, 1.0));
    let thrum = 2.5 * tension * (clock * 70.0).sin();
    let pts: Vec<V2> = (0..=16)
        .map(|i| {
            let u = i as f32 / 16.0;
            let bow = 4.0 * u * (1.0 - u);
            hand + d * u + v2(0.0, sag * bow) + normal * thrum * bow
        })
        .collect();
    polyline(out, &pts, 1.6);
}

/// Draw the lasso held in `hand` (`facing` ±1). `clock`: seconds, for the twirl.
pub fn draw(out: &mut DrawList, lasso: &Lasso, hand: V2, facing: f32, clock: f32) {
    match lasso.stage {
        Stage::Spin => {
            // A flat hoop circling overhead, the rope running up to its edge.
            let ph = clock * 14.0;
            let c = hand + v2(12.0 * ph.cos(), -22.0 + 3.0 * ph.sin());
            hoop(out, c, 36.0, 11.0, 0.08 * ph.sin());
            let edge = c + v2(-36.0 * ph.cos(), 11.0 * ph.sin().abs());
            rope(out, hand, edge, 0.0, 0.0, clock);
        }
        Stage::Fly { pos, vel } => {
            // The open loop flying, tilted along its path.
            let tilt = vel.y.atan2(vel.x);
            hoop(out, pos, 22.0, 13.0, tilt);
            let back = pos - vel.normalize_or(v2(facing, 0.0)) * 22.0;
            rope(out, hand, back, 30.0, 0.0, clock);
        }
        Stage::Caught { at, .. } => {
            // Cinched round the arrow's neck, the rope tightening with strain.
            let neck = at + v2(4.0, 9.0);
            hoop(out, neck, 8.0, 4.5, 0.6);
            let t = lasso.tension();
            rope(out, hand, neck, 22.0 * (1.0 - t), t, clock);
        }
        Stage::Retract { from } => {
            let k = smoothstep(lasso.t / RETRACT);
            let at = from + (hand - from) * k;
            hoop(out, at, 20.0 * (1.0 - k) + 6.0, 12.0 * (1.0 - k) + 4.0, 0.0);
            rope(out, hand, at, 25.0 * (1.0 - k), 0.0, clock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn thrown(hand: V2, cursor: V2) -> (Lasso, Step) {
        let mut l = Lasso::new();
        l.throw(hand, cursor);
        for _ in 0..120 {
            let s = l.step(DT, hand, cursor);
            if s != Step::Nothing {
                return (l, s);
            }
        }
        (l, Step::Nothing)
    }

    #[test]
    fn a_throw_at_a_still_cursor_in_range_catches_it_and_reels_it_in() {
        let hand = v2(100.0, 500.0);
        let mut cursor = v2(560.0, 380.0);
        let (mut l, s) = thrown(hand, cursor);
        assert!(matches!(s, Step::Caught { .. }), "{s:?}");
        let mut last = (cursor - hand).length();
        for k in 0..600 {
            let s = l.step(DT, hand, cursor);
            if let Step::ReeledIn { at } = s {
                assert!((at - hand).length() < REELED_IN);
                assert!(k as f32 * DT < 3.0, "took {:.1}s", k as f32 * DT);
                return;
            }
            assert!(!matches!(s, Step::Snapped { .. } | Step::Slipped), "{s:?}");
            // The app puts the pointer where the rope holds it.
            cursor = l.holds().unwrap();
            let d = (cursor - hand).length();
            assert!(d <= last + 0.01, "the pointer moved away");
            last = d;
        }
        panic!("never reeled in");
    }

    #[test]
    fn a_cursor_out_of_reach_is_missed_and_the_loop_comes_back() {
        let hand = v2(100.0, 500.0);
        let (mut l, s) = thrown(hand, v2(100.0 + RANGE + 300.0, 500.0));
        assert_eq!(s, Step::Missed);
        let done = (0..60).any(|_| l.step(DT, hand, v2(0.0, 0.0)) == Step::Done);
        assert!(done);
    }

    #[test]
    fn a_hard_yank_snaps_the_rope() {
        let hand = v2(100.0, 500.0);
        let cursor = v2(500.0, 500.0);
        let (mut l, _) = thrown(hand, cursor);
        // Yank away 60 points a tick (3600 pt/s).
        for _ in 0..30 {
            let at = l.holds().unwrap();
            if let Step::Snapped { dir } = l.step(DT, hand, at + v2(60.0, 0.0)) {
                assert!(dir.x > 0.9);
                return;
            }
        }
        panic!("the rope never snapped");
    }

    #[test]
    fn a_gentle_tug_pays_out_rope_without_snapping() {
        let hand = v2(100.0, 500.0);
        let cursor = v2(500.0, 500.0);
        let (mut l, _) = thrown(hand, cursor);
        let mut tugs = 0;
        // Pull away at 180 pt/s for 2 s: it holds, letting a little out.
        for _ in 0..120 {
            let at = l.holds().unwrap();
            match l.step(DT, hand, at + v2(3.0, 0.0)) {
                Step::Tugged { .. } => tugs += 1,
                Step::Snapped { .. } => panic!("snapped on a gentle tug"),
                _ => {}
            }
        }
        assert!(tugs > 100);
        assert!(l.tension() > 0.3 && l.tension() < 1.0, "{}", l.tension());
    }

    #[test]
    fn one_cursor_sample_over_several_ticks_counts_once() {
        let hand = v2(100.0, 500.0);
        let (mut l, _) = thrown(hand, v2(500.0, 500.0));
        let pulled = l.holds().unwrap() + v2(40.0, 0.0);
        let first = l.step(DT, hand, pulled);
        let second = l.step(DT, hand, pulled);
        assert!(matches!(first, Step::Tugged { .. }));
        assert_eq!(second, Step::Nothing);
    }
}
