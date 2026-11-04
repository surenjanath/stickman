//! Particle effects: hit explosions, sparks, landing dust, shockwave rings and KO
//! stars. Simulated on the CPU (a few hundred particles at most) and drawn on the
//! GPU as instanced SDF discs, streaks and rings in the figure's draw call.

use crate::geom::{v2, V2};
use crate::render::DrawList;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Disc,
    /// A line along the velocity (sparks).
    Streak,
    /// Expanding outline (shockwave).
    Ring,
    /// A little heart (petting).
    Heart,
}

#[derive(Debug, Clone, Copy)]
struct Particle {
    pos: V2,
    vel: V2,
    age: f32,
    life: f32,
    size0: f32,
    size1: f32,
    color: [f32; 4],
    shape: Shape,
    gravity: f32,
    drag: f32,
}

pub const MAX_PARTICLES: usize = 600;

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const HOT: [f32; 4] = [1.0, 0.86, 0.35, 1.0];
const FIRE: [f32; 4] = [1.0, 0.45, 0.12, 1.0];
const SMOKE: [f32; 4] = [0.55, 0.55, 0.58, 0.8];
const EMBER: [f32; 4] = [1.0, 0.25, 0.05, 1.0];
const STAR: [f32; 4] = [1.0, 0.85, 0.2, 1.0];
const LOVE: [f32; 4] = [0.95, 0.25, 0.4, 1.0];

pub struct Particles {
    list: Vec<Particle>,
    rng: u64,
}

impl Particles {
    pub fn new(seed: u64) -> Particles {
        Particles {
            list: Vec::with_capacity(MAX_PARTICLES),
            rng: seed | 1,
        }
    }

    fn rand(&mut self, lo: f32, hi: f32) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        lo + (hi - lo) * (r as f32 / (1u64 << 24) as f32)
    }

    fn push(&mut self, p: Particle) {
        if self.list.len() < MAX_PARTICLES {
            self.list.push(p);
        }
    }

    fn dir(&mut self, base: f32, spread: f32) -> V2 {
        let a = base + self.rand(-spread, spread);
        v2(a.cos(), a.sin())
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Click explosion: fireball, sparks and a shockwave ring, biased along `dir`.
    pub fn explosion(&mut self, at: V2, dir: V2) {
        let base = dir.y.atan2(dir.x);
        self.push(Particle {
            pos: at,
            vel: V2::ZERO,
            age: 0.0,
            life: 0.35,
            size0: 8.0,
            size1: 70.0,
            color: WHITE,
            shape: Shape::Ring,
            gravity: 0.0,
            drag: 0.0,
        });
        for i in 0..30 {
            let d = if i % 3 == 0 {
                self.dir(base, 0.9)
            } else {
                self.dir(0.0, std::f32::consts::PI)
            };
            let speed = self.rand(120.0, 520.0);
            let hot = self.rand(0.0, 1.0) < 0.4;
            let (life, size0) = (self.rand(0.35, 0.75), self.rand(6.0, 13.0));
            self.push(Particle {
                pos: at,
                vel: d * speed,
                age: 0.0,
                life,
                size0,
                size1: 0.0,
                color: if hot { HOT } else { FIRE },
                shape: Shape::Disc,
                gravity: 350.0,
                drag: 2.5,
            });
        }
        self.sparks(at, dir, 16);
        for _ in 0..8 {
            let d = self.dir(-std::f32::consts::FRAC_PI_2, 1.2);
            let speed = self.rand(30.0, 90.0);
            let (life, size0) = (self.rand(0.6, 1.1), self.rand(8.0, 14.0));
            self.push(Particle {
                pos: at,
                vel: d * speed,
                age: 0.0,
                life,
                size0,
                size1: 18.0,
                color: SMOKE,
                shape: Shape::Disc,
                gravity: -40.0,
                drag: 1.5,
            });
        }
    }

    /// Flames licking up from `points` (a burning body). Call every frame;
    /// `heat` (0..1) scales how many.
    pub fn flames(&mut self, points: &[V2], heat: f32) {
        for &at in points {
            if self.rand(0.0, 1.0) > 0.06 + 0.22 * heat {
                continue;
            }
            let pos = at + v2(self.rand(-5.0, 5.0), self.rand(-5.0, 5.0));
            let roll = self.rand(0.0, 1.0);
            let mut color = if roll < 0.35 {
                HOT
            } else if roll < 0.85 {
                FIRE
            } else {
                EMBER
            };
            color[3] = 0.8;
            let vel = v2(self.rand(-20.0, 20.0), self.rand(-160.0, -80.0));
            let life = self.rand(0.2, 0.45);
            let size0 = self.rand(3.0, 6.5) * (0.6 + 0.7 * heat);
            self.push(Particle {
                pos,
                vel,
                age: 0.0,
                life,
                size0,
                size1: 0.0,
                color,
                shape: Shape::Disc,
                gravity: -220.0,
                drag: 1.2,
            });
            if self.rand(0.0, 1.0) < 0.08 {
                let vel = v2(self.rand(-15.0, 15.0), self.rand(-60.0, -30.0));
                let life = self.rand(0.8, 1.4);
                self.push(Particle {
                    pos: at,
                    vel,
                    age: 0.0,
                    life,
                    size0: 6.0,
                    size1: 20.0,
                    color: SMOKE,
                    shape: Shape::Disc,
                    gravity: -30.0,
                    drag: 1.0,
                });
            }
        }
    }

    /// A burnt body crumbling: ash falls from `points`, embers drift up, smoke.
    pub fn ash(&mut self, points: &[V2]) {
        for &at in points {
            for _ in 0..3 {
                let grey = self.rand(0.12, 0.35);
                let pos = at + v2(self.rand(-4.0, 4.0), self.rand(-4.0, 4.0));
                let vel = v2(self.rand(-40.0, 40.0), self.rand(-30.0, 20.0));
                let life = self.rand(0.9, 1.6);
                let size0 = self.rand(3.0, 6.0);
                self.push(Particle {
                    pos,
                    vel,
                    age: 0.0,
                    life,
                    size0,
                    size1: 2.0,
                    color: [grey, grey * 0.95, grey * 0.9, 1.0],
                    shape: Shape::Disc,
                    gravity: 520.0,
                    drag: 1.6,
                });
            }
            let vel = v2(self.rand(-20.0, 20.0), self.rand(-90.0, -40.0));
            let life = self.rand(1.0, 1.8);
            self.push(Particle {
                pos: at,
                vel,
                age: 0.0,
                life,
                size0: 2.5,
                size1: 0.5,
                color: EMBER,
                shape: Shape::Disc,
                gravity: -40.0,
                drag: 0.8,
            });
            let vel = v2(self.rand(-20.0, 20.0), self.rand(-50.0, -20.0));
            let life = self.rand(1.2, 2.0);
            self.push(Particle {
                pos: at,
                vel,
                age: 0.0,
                life,
                size0: 10.0,
                size1: 30.0,
                color: SMOKE,
                shape: Shape::Disc,
                gravity: -25.0,
                drag: 0.9,
            });
        }
    }

    /// Bright streaks flying mostly along `dir` (punch impacts).
    pub fn sparks(&mut self, at: V2, dir: V2, n: usize) {
        let base = dir.y.atan2(dir.x);
        for _ in 0..n {
            let d = self.dir(base, 0.8);
            let speed = self.rand(450.0, 950.0);
            let life = self.rand(0.15, 0.32);
            let color = if self.rand(0.0, 1.0) < 0.5 {
                WHITE
            } else {
                HOT
            };
            self.push(Particle {
                pos: at,
                vel: d * speed,
                age: 0.0,
                life,
                size0: 2.2,
                size1: 0.6,
                color,
                shape: Shape::Streak,
                gravity: 900.0,
                drag: 1.0,
            });
        }
    }

    /// Puffs kicked up sideways along the ground.
    pub fn dust(&mut self, at: V2, strength: f32) {
        let n = (6.0 + 14.0 * strength) as usize;
        for i in 0..n {
            let side = if i % 2 == 0 { 1.0 } else { -1.0 };
            let speed = self.rand(40.0, 160.0) * (0.5 + strength);
            let up = self.rand(-60.0, -10.0);
            let (life, size0) = (self.rand(0.35, 0.7), self.rand(3.0, 6.0));
            let offset = side * self.rand(0.0, 10.0);
            self.push(Particle {
                pos: at + v2(offset, -2.0),
                vel: v2(side * speed, up),
                age: 0.0,
                life,
                size0,
                size1: size0 * 2.2,
                color: SMOKE,
                shape: Shape::Disc,
                gravity: 60.0,
                drag: 3.0,
            });
        }
    }

    /// Little stars circling a knocked-out head.
    pub fn stars(&mut self, at: V2) {
        for i in 0..5 {
            let a = i as f32 / 5.0 * std::f32::consts::TAU;
            self.push(Particle {
                pos: at + v2(a.cos() * 22.0, a.sin() * 6.0 - 10.0),
                vel: v2(-a.sin() * 50.0, a.cos() * 14.0),
                age: 0.0,
                life: 2.2,
                size0: 3.5,
                size1: 2.0,
                color: STAR,
                shape: Shape::Disc,
                gravity: 0.0,
                drag: 0.0,
            });
        }
    }

    /// A wisp of telekinetic glow at `at` (call every tick while it lasts).
    pub fn aura(&mut self, at: V2, strength: f32) {
        let n = if self.rand(0.0, 1.0) < strength.fract() {
            strength as usize + 1
        } else {
            strength as usize
        };
        for _ in 0..n {
            let d = self.dir(0.0, std::f32::consts::PI);
            let r = self.rand(4.0, 16.0);
            let vel = d * self.rand(20.0, 70.0) + v2(0.0, -30.0);
            let size = self.rand(2.0, 4.0);
            self.push(Particle {
                pos: at + d * r,
                vel,
                age: 0.0,
                life: 0.45,
                size0: size,
                size1: 0.5,
                color: [0.72, 0.45, 1.0, 0.9],
                shape: Shape::Disc,
                gravity: 0.0,
                drag: 2.0,
            });
        }
    }

    /// Crackling energy while powering up a super jump.
    pub fn charge_sparks(&mut self, at: V2) {
        for _ in 0..2 {
            let d = self.dir(-std::f32::consts::FRAC_PI_2, 1.2);
            let vel = d * self.rand(120.0, 260.0);
            let x = self.rand(-18.0, 18.0);
            self.push(Particle {
                pos: at + v2(x, 0.0),
                vel,
                age: 0.0,
                life: 0.3,
                size0: 1.6,
                size1: 0.6,
                color: [0.55, 0.85, 1.0, 1.0],
                shape: Shape::Streak,
                gravity: 0.0,
                drag: 4.0,
            });
        }
    }

    /// The glowing trail of a super jump.
    pub fn trail(&mut self, at: V2) {
        let j = v2(self.rand(-6.0, 6.0), self.rand(-6.0, 6.0));
        self.push(Particle {
            pos: at + j,
            vel: V2::ZERO,
            age: 0.0,
            life: 0.35,
            size0: 7.0,
            size1: 1.0,
            color: [0.55, 0.85, 1.0, 0.55],
            shape: Shape::Disc,
            gravity: 0.0,
            drag: 0.0,
        });
    }

    /// A ground-level shockwave ring (super jump launch).
    pub fn shockwave(&mut self, at: V2) {
        self.push(Particle {
            pos: at,
            vel: V2::ZERO,
            age: 0.0,
            life: 0.45,
            size0: 8.0,
            size1: 90.0,
            color: [0.7, 0.9, 1.0, 0.9],
            shape: Shape::Ring,
            gravity: 0.0,
            drag: 0.0,
        });
        self.dust(at, 1.0);
    }

    /// A puff of steam from an angry head.
    pub fn steam(&mut self, at: V2) {
        let side = if self.rand(0.0, 1.0) < 0.5 { -1.0 } else { 1.0 };
        let vel = v2(side * self.rand(30.0, 70.0), self.rand(-110.0, -70.0));
        let size = self.rand(4.0, 6.0);
        self.push(Particle {
            pos: at + v2(side * 14.0, -10.0),
            vel,
            age: 0.0,
            life: 0.9,
            size0: size,
            size1: 13.0,
            color: [0.85, 0.85, 0.88, 0.7],
            shape: Shape::Disc,
            gravity: -40.0,
            drag: 1.5,
        });
    }

    /// Hearts floating up from a happy head.
    pub fn hearts(&mut self, at: V2) {
        for i in 0..6 {
            let x = self.rand(-26.0, 26.0);
            let (y, vy, size) = (
                self.rand(-10.0, 4.0),
                self.rand(-75.0, -45.0),
                self.rand(5.0, 7.5),
            );
            self.push(Particle {
                pos: at + v2(x, y),
                vel: v2(x * 0.6, vy),
                age: -(i as f32) * 0.12, // staggered: appear one after another
                life: 1.8,
                size0: size,
                size1: 4.0,
                color: LOVE,
                shape: Shape::Heart,
                gravity: -10.0,
                drag: 0.8,
            });
        }
    }

    pub fn update(&mut self, dt: f32) {
        for p in &mut self.list {
            p.age += dt;
            p.vel.y += p.gravity * dt;
            p.vel *= (-p.drag * dt).exp();
            p.pos += p.vel * dt;
        }
        self.list.retain(|p| p.age < p.life);
    }

    pub fn draw(&self, out: &mut DrawList) {
        for p in &self.list {
            if p.age < 0.0 {
                continue; // not born yet
            }
            let k = (p.age / p.life).clamp(0.0, 1.0);
            let size = p.size0 + (p.size1 - p.size0) * k;
            let fade = 1.0 - k * k;
            let color = [p.color[0], p.color[1], p.color[2], p.color[3] * fade];
            match p.shape {
                Shape::Disc => out.disc(p.pos, size, color),
                Shape::Streak => out.capsule(p.pos, p.pos - p.vel * 0.025, size, color),
                Shape::Ring => out.ring(p.pos, size, 3.0 * (1.0 - k) + 0.5, color),
                Shape::Heart => {
                    // Two lobes and a point.
                    let r = size * 0.55;
                    let (l, rt) = (p.pos + v2(-r * 0.8, 0.0), p.pos + v2(r * 0.8, 0.0));
                    let tip = p.pos + v2(0.0, size * 1.25);
                    out.disc(l, r, color);
                    out.disc(rt, r, color);
                    out.capsule(l + v2(-r * 0.25, r * 0.3), tip, r * 0.55, color);
                    out.capsule(rt + v2(r * 0.25, r * 0.3), tip, r * 0.55, color);
                    out.disc(p.pos + v2(0.0, r * 0.5), r * 0.9, color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explosion_spawns_and_burns_out() {
        let mut p = Particles::new(1);
        p.explosion(v2(100.0, 100.0), v2(1.0, 0.0));
        assert!(p.len() > 40);
        for _ in 0..30 {
            p.update(1.0 / 60.0);
        }
        assert!(!p.is_empty(), "too short-lived");
        for _ in 0..90 {
            p.update(1.0 / 60.0);
        }
        assert!(p.is_empty(), "{} particles still alive after 2 s", p.len());
    }

    #[test]
    fn flames_rise_and_ash_falls() {
        let body = [v2(0.0, 0.0), v2(0.0, 40.0), v2(10.0, 80.0)];
        let mut p = Particles::new(3);
        for _ in 0..40 {
            p.flames(&body, 1.0);
        }
        assert!(p.len() > 10);
        p.update(0.2);
        let mean_y: f32 = p.list.iter().map(|q| q.pos.y).sum::<f32>() / p.len() as f32;
        assert!(mean_y < 30.0, "flames should rise: {mean_y}");
        let mut a = Particles::new(4);
        a.ash(&body);
        a.update(0.5);
        assert!(a.list.iter().any(|q| q.pos.y > 100.0), "no ash fell");
        for _ in 0..150 {
            a.update(1.0 / 60.0);
        }
        assert!(a.is_empty(), "ash should settle and fade");
    }

    #[test]
    fn sparks_fly_along_the_hit_direction() {
        let mut p = Particles::new(2);
        p.sparks(v2(0.0, 0.0), v2(1.0, 0.0), 40);
        p.update(0.05);
        let mean_x: f32 = p.list.iter().map(|q| q.pos.x).sum::<f32>() / p.len() as f32;
        assert!(mean_x > 15.0, "{mean_x}");
    }

    #[test]
    fn capped_at_max() {
        let mut p = Particles::new(3);
        for _ in 0..50 {
            p.explosion(v2(0.0, 0.0), v2(0.0, -1.0));
        }
        assert_eq!(p.len(), MAX_PARTICLES);
    }

    #[test]
    fn hearts_float_up_and_appear_in_turn() {
        let mut p = Particles::new(5);
        p.hearts(v2(100.0, 100.0));
        let mut dl = DrawList::default();
        p.draw(&mut dl);
        assert_eq!(dl.len(), 5, "only the first heart is born yet");
        for _ in 0..60 {
            p.update(1.0 / 60.0);
        }
        assert!(p.list.iter().all(|h| h.pos.y < 100.0), "hearts rise");
        for _ in 0..120 {
            p.update(1.0 / 60.0);
        }
        assert!(p.is_empty());
    }

    #[test]
    fn draws_one_instance_per_particle() {
        let mut p = Particles::new(4);
        p.dust(v2(0.0, 0.0), 1.0);
        let mut dl = DrawList::default();
        p.draw(&mut dl);
        assert_eq!(dl.len(), p.len());
    }
}
