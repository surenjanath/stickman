//! Its arsenal of tools: paint it throws, ink balls its drawn cannon fires,
//! the paint and ink they leave dripping down the screen, the tools as drawn
//! in its hands, and window toolbars it saws off (a copy dangles from one end
//! while the real window is untouched).
//!
//! Pure logic and drawing: the caller feeds in time and the cursor, and acts
//! on the [`Hit`]s that come back.

use crate::agent::Gadget;
use crate::geom::{v2, Rect, V2};
use crate::render::{DrawList, SpriteDraw};

/// Paint falls like a thrown word (the same arc); ink balls fly almost straight.
const PAINT_GRAVITY: f32 = crate::agent::PROJECTILE_GRAVITY;
const INK_SPEED: f32 = 1500.0;
const INK_GRAVITY: f32 = 250.0;
/// A shot this close to the cursor hits it.
const HIT_RADIUS: f32 = 24.0;
/// Shots older than this are gone (off screen, say).
const SHOT_LIFE: f32 = 2.5;
/// Splats drip, then fade out over the last `SPLAT_FADE` seconds.
const SPLAT_LIFE: f32 = 7.0;
const SPLAT_FADE: f32 = 2.0;
const MAX_SPLATS: usize = 40;
/// A sawn-off toolbar dangles this long, then fades.
const SAWN_LIFE: f32 = 12.0;
const SAWN_FADE: f32 = 1.0;

/// Paint colours, picked per bucket.
pub const PAINTS: [[f32; 4]; 5] = [
    [0.95, 0.25, 0.3, 1.0],
    [0.2, 0.65, 0.95, 1.0],
    [0.35, 0.8, 0.3, 1.0],
    [0.98, 0.75, 0.15, 1.0],
    [0.7, 0.35, 0.9, 1.0],
];
const INK: [f32; 4] = [0.08, 0.08, 0.1, 1.0];

#[derive(Debug, Clone, Copy, PartialEq)]
struct Shot {
    pos: V2,
    vel: V2,
    gravity: f32,
    color: [f32; 4],
    radius: f32,
    age: f32,
    /// Where it splats if it never hits anything.
    floor: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct Splat {
    at: V2,
    color: [f32; 4],
    /// Blobs around the centre: (offset, radius).
    blobs: Vec<(V2, f32)>,
    /// Drips: (x offset, speed, max length).
    drips: Vec<(f32, f32, f32)>,
    age: f32,
}

/// A toolbar strip sawn off a window: hinged at one end, swinging.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sawn {
    sprite: u32,
    /// Where it was on the window (painted over with the window's colour).
    rect: Rect,
    background: [f32; 4],
    window: u32,
    /// Hinged at the left end (else the right).
    left: bool,
    angle: f32,
    rate: f32,
    age: f32,
}

/// Something that happened this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    /// A shot hit the cursor, moving along `dir`.
    Cursor { at: V2, dir: V2, paint: bool },
    /// A shot splatted somewhere.
    Splat { at: V2 },
}

#[derive(Debug, Default)]
pub struct Arsenal {
    shots: Vec<Shot>,
    splats: Vec<Splat>,
    sawn: Vec<Sawn>,
    rng: u64,
    /// Sprites no longer drawn: the caller frees them.
    pub freed: Vec<u32>,
}

impl Arsenal {
    pub fn new(seed: u64) -> Arsenal {
        Arsenal {
            rng: seed | 1,
            ..Arsenal::default()
        }
    }

    fn random(&mut self) -> f32 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Anything on screen that moves (so frames must be drawn).
    pub fn animating(&self) -> bool {
        !self.shots.is_empty() || !self.splats.is_empty() || !self.sawn.is_empty()
    }

    /// Throw paint (a bucket) or fire an ink ball (the cannon) from `from` at
    /// `target`; shots that miss splat at `floor` (the ground line).
    pub fn fire(&mut self, gadget: Gadget, from: V2, target: V2, color: [f32; 4], floor: f32) {
        let (vel, gravity, color, radius) = match gadget {
            Gadget::Bucket => (
                crate::agent::lob_velocity(from, target),
                PAINT_GRAVITY,
                color,
                9.0,
            ),
            _ => {
                let dir = (target - from).normalize_or(v2(1.0, 0.0));
                (dir * INK_SPEED, INK_GRAVITY, INK, 6.0)
            }
        };
        self.shots.push(Shot {
            pos: from,
            vel,
            gravity,
            color,
            radius,
            age: 0.0,
            floor,
        });
    }

    fn splat(&mut self, at: V2, color: [f32; 4], size: f32) {
        if self.splats.len() >= MAX_SPLATS {
            self.splats.remove(0);
        }
        let n = 5 + (self.random() * 5.0) as usize;
        let blobs = (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU + self.random();
                let d = size * (0.3 + 0.9 * self.random());
                (
                    v2(a.cos(), a.sin()) * d,
                    size * (0.35 + 0.45 * self.random()),
                )
            })
            .chain(std::iter::once((V2::ZERO, size)))
            .collect();
        let drips = (0..3)
            .map(|_| {
                (
                    (self.random() - 0.5) * size * 1.6,
                    8.0 + 18.0 * self.random(),
                    size * (1.5 + 3.0 * self.random()),
                )
            })
            .collect();
        self.splats.push(Splat {
            at,
            color,
            blobs,
            drips,
            age: 0.0,
        });
    }

    /// A toolbar strip came off a window: `sprite` is its pixels, `rect` where
    /// it was, `background` the window's colour there. It hangs from the end
    /// away from `figure_x`.
    pub fn add_sawn(
        &mut self,
        sprite: u32,
        rect: Rect,
        background: [u8; 3],
        window: u32,
        figure_x: f32,
    ) {
        let mid = (rect.x + rect.width / 2.0) as f32;
        let left = figure_x > mid;
        let bg = background.map(|c| f32::from(c) / 255.0);
        self.sawn.push(Sawn {
            sprite,
            rect,
            background: [bg[0], bg[1], bg[2], 1.0],
            window,
            left,
            angle: 0.0,
            rate: if left { 1.2 } else { -1.2 },
            age: 0.0,
        });
    }

    /// The window moved or went away: its sawn-off strip goes too (the copy
    /// would no longer line up).
    pub fn window_changed(&mut self, window: u32) {
        for s in &mut self.sawn {
            if s.window == window && s.age < SAWN_LIFE {
                s.age = SAWN_LIFE;
            }
        }
    }

    /// Move shots, drip splats, swing sawn strips. Returns what happened.
    pub fn update(&mut self, dt: f32, cursor: V2) -> Vec<Hit> {
        let mut hits = Vec::new();
        let mut landed = Vec::new();
        self.shots.retain_mut(|s| {
            s.age += dt;
            s.vel.y += s.gravity * dt;
            let next = s.pos + s.vel * dt;
            // Closest approach to the cursor along this step.
            let seg = next - s.pos;
            let k = ((cursor - s.pos).dot(seg) / seg.length_squared().max(1e-3)).clamp(0.0, 1.0);
            if (s.pos + seg * k - cursor).length() < HIT_RADIUS + s.radius {
                let paint = s.color != INK;
                hits.push(Hit::Cursor {
                    at: cursor,
                    dir: s.vel.normalize_or(v2(1.0, 0.0)),
                    paint,
                });
                landed.push((cursor, s.color, s.radius * 2.0));
                return false;
            }
            s.pos = next;
            if s.pos.y >= s.floor || s.age > SHOT_LIFE {
                landed.push((v2(s.pos.x, s.pos.y.min(s.floor)), s.color, s.radius * 1.6));
                return false;
            }
            true
        });
        for (at, color, size) in landed {
            hits.push(Hit::Splat { at });
            self.splat(at, color, size);
        }
        for s in &mut self.splats {
            s.age += dt;
        }
        self.splats.retain(|s| s.age < SPLAT_LIFE);
        for s in &mut self.sawn {
            s.age += dt;
            // Hinged at one end: swings down under gravity, settles crooked.
            let rest = if s.left { 0.9 } else { -0.9 };
            s.rate += (-(s.angle - rest) * 30.0 - s.rate * 2.5) * dt;
            s.angle += s.rate * dt;
        }
        let freed = &mut self.freed;
        self.sawn.retain(|s| {
            let keep = s.age < SAWN_LIFE + SAWN_FADE;
            if !keep {
                freed.push(s.sprite);
            }
            keep
        });
        hits
    }

    /// Shots, splats and sawn strips (the patches go under, sprites on top).
    pub fn draw(&self, out: &mut DrawList) {
        for s in &self.sawn {
            let alpha = 1.0 - ((s.age - SAWN_LIFE) / SAWN_FADE).clamp(0.0, 1.0);
            let (w, h) = (s.rect.width as f32, s.rect.height as f32);
            let c = v2(s.rect.x as f32 + w / 2.0, s.rect.y as f32 + h / 2.0);
            // The gap it left, in the window's own colour.
            let mut bg = s.background;
            bg[3] = alpha;
            out.rect(c, v2(w / 2.0, h / 2.0), 0.0, bg);
            // The strip, turned about its hinge.
            let hinge = if s.left {
                v2(s.rect.x as f32, c.y)
            } else {
                v2(s.rect.right() as f32, c.y)
            };
            let arm = (c - hinge).rotate(V2::from_angle(s.angle));
            out.sprite(SpriteDraw {
                id: s.sprite,
                center: hinge + arm,
                half: v2(w / 2.0, h / 2.0),
                rotation: s.angle,
                alpha,
            });
        }
        for s in &self.splats {
            let alpha = 1.0 - ((s.age - (SPLAT_LIFE - SPLAT_FADE)) / SPLAT_FADE).clamp(0.0, 1.0);
            let mut c = s.color;
            c[3] *= alpha * 0.92;
            for (off, r) in &s.blobs {
                out.disc(s.at + *off, *r, c);
            }
            for (x, speed, max) in &s.drips {
                let len = (speed * s.age).min(*max);
                let top = s.at + v2(*x, 0.0);
                out.capsule(top, top + v2(0.0, len), 2.2, c);
            }
        }
        for s in &self.shots {
            out.disc(s.pos, s.radius + 1.5, [1.0, 1.0, 1.0, 0.8]);
            out.disc(s.pos, s.radius, s.color);
        }
    }
}

/// The tool in its hands, drawn from the grip toward `aim` (a point).
pub fn draw_held(out: &mut DrawList, gadget: Gadget, grip: V2, aim: V2, paint: [f32; 4]) {
    let dir = (aim - grip).normalize_or(v2(1.0, 0.0));
    let normal = v2(-dir.y, dir.x);
    let outline = [1.0, 1.0, 1.0, 0.85];
    match gadget {
        Gadget::Eraser => {
            // A chunky pink eraser.
            let tip = grip + dir * 30.0;
            out.capsule(grip, tip, 9.5, outline);
            out.capsule(grip, tip, 8.0, [0.96, 0.66, 0.72, 1.0]);
            out.capsule(grip, grip + dir * 9.0, 8.0, [0.35, 0.45, 0.85, 1.0]);
        }
        Gadget::Bucket => {
            // A bucket hanging from the hand, paint at the brim.
            let top = grip + v2(0.0, 6.0);
            let bottom = top + v2(0.0, 22.0);
            out.capsule(top - v2(9.0, 0.0), bottom - v2(6.0, 0.0), 3.4, outline);
            out.capsule(top + v2(9.0, 0.0), bottom + v2(6.0, 0.0), 3.4, outline);
            out.rect(
                (top + bottom) / 2.0,
                v2(8.5, 11.0),
                3.0,
                [0.62, 0.64, 0.7, 1.0],
            );
            out.rect(top + v2(0.0, 2.0), v2(8.0, 2.5), 2.0, paint);
            out.ring(grip, 6.0, 1.2, [0.3, 0.3, 0.32, 1.0]);
        }
        Gadget::Cannon => {
            // A hand-drawn cannon on its shoulder, barrel toward the aim.
            let back = grip - dir * 14.0;
            let muzzle = grip + dir * 34.0;
            out.capsule(back, muzzle, 9.0, outline);
            out.capsule(back, muzzle, 7.5, INK);
            out.capsule(muzzle - dir * 3.0, muzzle + normal * 0.0, 9.5, INK);
            out.disc(back - normal * 4.0, 6.0, [0.25, 0.25, 0.28, 1.0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn an_ink_ball_hits_a_cursor_in_its_path() {
        let mut a = Arsenal::new(1);
        a.fire(
            Gadget::Cannon,
            v2(100.0, 500.0),
            v2(700.0, 480.0),
            INK,
            1100.0,
        );
        let mut hit = false;
        for _ in 0..60 {
            hit |= a
                .update(DT, v2(700.0, 480.0))
                .iter()
                .any(|h| matches!(h, Hit::Cursor { paint: false, .. }));
        }
        assert!(hit);
        assert!(a.shots.is_empty());
        assert_eq!(a.splats.len(), 1, "ink splats where it hits");
    }

    #[test]
    fn thrown_paint_lobs_to_the_cursor_and_a_miss_splats_on_the_ground() {
        let mut a = Arsenal::new(2);
        a.fire(
            Gadget::Bucket,
            v2(100.0, 1000.0),
            v2(600.0, 700.0),
            PAINTS[0],
            1100.0,
        );
        let hit = (0..120).any(|_| {
            a.update(DT, v2(600.0, 700.0))
                .iter()
                .any(|h| matches!(h, Hit::Cursor { paint: true, .. }))
        });
        assert!(hit, "the lob never reached the cursor");
        // Cursor gone elsewhere: it lands on the floor and drips there.
        a.fire(
            Gadget::Bucket,
            v2(100.0, 1000.0),
            v2(600.0, 700.0),
            PAINTS[1],
            1100.0,
        );
        let mut splat = None;
        for _ in 0..240 {
            for h in a.update(DT, v2(1700.0, 100.0)) {
                if let Hit::Splat { at } = h {
                    splat = Some(at);
                }
            }
        }
        let at = splat.expect("never splatted");
        assert!((at.y - 1100.0).abs() < 1.0, "splatted at {at:?}");
    }

    #[test]
    fn splats_drip_then_fade_away() {
        let mut a = Arsenal::new(3);
        a.splat(v2(500.0, 500.0), PAINTS[2], 18.0);
        let mut dl = DrawList::default();
        a.draw(&mut dl);
        assert!(!dl.is_empty());
        for _ in 0..(SPLAT_LIFE / DT) as usize + 2 {
            a.update(DT, V2::ZERO);
        }
        assert!(a.splats.is_empty());
        assert!(!a.animating());
    }

    #[test]
    fn a_sawn_toolbar_swings_down_from_its_hinge_then_goes() {
        let mut a = Arsenal::new(4);
        let rect = Rect {
            x: 300.0,
            y: 100.0,
            width: 600.0,
            height: 50.0,
        };
        // Figure on the right half: it hangs from the left end.
        a.add_sawn(7, rect, [240, 240, 240], 42, 800.0);
        for _ in 0..120 {
            a.update(DT, V2::ZERO);
        }
        let s = a.sawn[0];
        assert!(s.left && s.angle > 0.5, "didn't swing down: {}", s.angle);
        a.window_changed(42);
        for _ in 0..(SAWN_FADE / DT) as usize + 2 {
            a.update(DT, V2::ZERO);
        }
        assert!(a.sawn.is_empty());
        assert_eq!(a.freed, [7]);
    }
}
