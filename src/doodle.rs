//! Doodles: little drawings the figure scribbles in the air beside it (a heart,
//! a smiley, a star…). Each shape is a few pen strokes in unit coordinates; the
//! figure's pen moves along them over a few seconds, then the drawing lingers
//! and fades.

use std::f32::consts::{PI, TAU};

use crate::geom::{v2, V2};
use crate::render::DrawList;

/// How long a finished doodle stays, then fades out.
pub const LIFETIME: f32 = 30.0;
const FADE: f32 = 3.0;
const MAX_DOODLES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Heart,
    Smiley,
    Star,
    House,
    Sun,
    Spiral,
    /// Weapons it draws to fight with (not idle doodles). Drawn pointing
    /// right (+x); [`Shape::grip`] is where its hand goes.
    Cannon,
    Sword,
    Hammer,
    Bow,
    Bomb,
}

impl Shape {
    pub const ALL: [Shape; 6] = [
        Shape::Heart,
        Shape::Smiley,
        Shape::Star,
        Shape::House,
        Shape::Sun,
        Shape::Spiral,
    ];

    pub const WEAPONS: [Shape; 5] = [
        Shape::Cannon,
        Shape::Sword,
        Shape::Hammer,
        Shape::Bow,
        Shape::Bomb,
    ];

    /// A weapon (drawn bold, then taken off the page into its hand).
    pub fn weapon(self) -> bool {
        Shape::WEAPONS.contains(&self)
    }

    /// Where the hand holds it, in the shape's unit box.
    pub fn grip(self) -> V2 {
        match self {
            Shape::Cannon => v2(-0.2, 0.0),
            Shape::Sword => v2(-0.7, 0.0),
            Shape::Hammer => v2(-0.75, 0.0),
            Shape::Bow => v2(0.35, 0.0),
            Shape::Bomb => v2(-0.1, 0.2),
            _ => V2::ZERO,
        }
    }

    pub fn color(self) -> [f32; 4] {
        match self {
            Shape::Heart => [0.9, 0.2, 0.3, 1.0],
            Shape::Smiley => [0.95, 0.6, 0.1, 1.0],
            Shape::Star => [0.95, 0.78, 0.1, 1.0],
            Shape::House => [0.2, 0.45, 0.85, 1.0],
            Shape::Sun => [1.0, 0.55, 0.1, 1.0],
            Shape::Spiral => [0.55, 0.3, 0.85, 1.0],
            Shape::Cannon => [0.2, 0.22, 0.3, 1.0],
            Shape::Sword => [0.2, 0.5, 0.95, 1.0],
            Shape::Hammer => [0.88, 0.42, 0.12, 1.0],
            Shape::Bow => [0.15, 0.62, 0.3, 1.0],
            Shape::Bomb => [0.16, 0.16, 0.22, 1.0],
        }
    }

    /// Pen strokes, in a unit box ([-1, 1], y down).
    pub fn strokes(self) -> Vec<Vec<V2>> {
        let circle = |c: V2, r: f32, n: usize| -> Vec<V2> {
            (0..=n)
                .map(|i| {
                    let a = -PI / 2.0 + i as f32 / n as f32 * TAU;
                    c + v2(a.cos(), a.sin()) * r
                })
                .collect()
        };
        match self {
            Shape::Heart => vec![(0..=40)
                .map(|i| {
                    let t = i as f32 / 40.0 * TAU;
                    let x = 16.0 * t.sin().powi(3);
                    let y = -(13.0 * t.cos()
                        - 5.0 * (2.0 * t).cos()
                        - 2.0 * (3.0 * t).cos()
                        - (4.0 * t).cos());
                    v2(x, y - 2.5) / 17.0
                })
                .collect()],
            Shape::Smiley => vec![
                circle(V2::ZERO, 1.0, 32),
                circle(v2(-0.35, -0.3), 0.1, 8),
                circle(v2(0.35, -0.3), 0.1, 8),
                (0..=12)
                    .map(|i| {
                        let a = PI * (0.2 + 0.6 * i as f32 / 12.0);
                        v2(a.cos(), a.sin()) * 0.55 + v2(0.0, 0.05)
                    })
                    .collect(),
            ],
            Shape::Star => vec![(0..=10)
                .map(|i| {
                    let a = -PI / 2.0 + i as f32 / 10.0 * TAU;
                    let r = if i % 2 == 0 { 1.0 } else { 0.42 };
                    v2(a.cos(), a.sin()) * r
                })
                .collect()],
            Shape::House => vec![
                vec![
                    v2(-0.7, 0.0),
                    v2(-0.7, 1.0),
                    v2(0.7, 1.0),
                    v2(0.7, 0.0),
                    v2(-0.7, 0.0),
                    v2(0.0, -0.8),
                    v2(0.7, 0.0),
                ],
                vec![v2(-0.15, 1.0), v2(-0.15, 0.5), v2(0.15, 0.5), v2(0.15, 1.0)],
            ],
            Shape::Sun => {
                let mut s = vec![circle(V2::ZERO, 0.5, 24)];
                for i in 0..8 {
                    let a = i as f32 / 8.0 * TAU;
                    let d = v2(a.cos(), a.sin());
                    s.push(vec![d * 0.68, d]);
                }
                s
            }
            Shape::Cannon => {
                // A tapered barrel with a rounded breech, bands and a
                // muzzle, on a spoked wheel, with a lit fuse.
                let breech: Vec<V2> = (0..=10)
                    .map(|i| {
                        let a = PI / 2.0 + i as f32 / 10.0 * PI;
                        v2(-0.75 + 0.18 * a.cos(), -0.09 - 0.29 * a.sin())
                    })
                    .collect();
                let mut s = vec![
                    vec![
                        v2(-0.75, 0.2),
                        v2(0.85, 0.08),
                        v2(0.85, -0.5),
                        v2(-0.75, -0.38),
                    ],
                    breech,
                    circle(v2(-0.95, -0.09), 0.06, 8),
                    vec![
                        v2(0.85, -0.6),
                        v2(0.95, -0.6),
                        v2(0.95, 0.18),
                        v2(0.85, 0.18),
                    ],
                    vec![v2(0.0, -0.45), v2(0.0, 0.14)],
                    vec![v2(0.45, -0.48), v2(0.45, 0.11)],
                    circle(v2(-0.2, 0.5), 0.42, 20),
                ];
                for i in 0..3 {
                    let a = i as f32 / 3.0 * PI + 0.3;
                    let d = v2(a.cos(), a.sin()) * 0.42;
                    s.push(vec![v2(-0.2, 0.5) - d, v2(-0.2, 0.5) + d]);
                }
                s.push(vec![v2(-0.6, -0.36), v2(-0.68, -0.6), v2(-0.55, -0.8)]);
                for i in 0..4 {
                    let a = i as f32 / 4.0 * TAU;
                    let d = v2(a.cos(), a.sin()) * 0.1;
                    s.push(vec![v2(-0.55, -0.88) + d * 0.3, v2(-0.55, -0.88) + d]);
                }
                s
            }
            Shape::Sword => vec![
                // Blade, point to the right, with a fuller down the middle.
                vec![
                    v2(-0.4, -0.12),
                    v2(0.8, -0.12),
                    v2(1.0, 0.0),
                    v2(0.8, 0.12),
                    v2(-0.4, 0.12),
                ],
                vec![v2(-0.3, 0.0), v2(0.7, 0.0)],
                // Crossguard, grip and pommel.
                vec![
                    v2(-0.5, -0.42),
                    v2(-0.4, -0.42),
                    v2(-0.4, 0.42),
                    v2(-0.5, 0.42),
                    v2(-0.5, -0.42),
                ],
                vec![
                    v2(-0.5, -0.07),
                    v2(-0.86, -0.07),
                    v2(-0.86, 0.07),
                    v2(-0.5, 0.07),
                ],
                vec![v2(-0.62, -0.07), v2(-0.68, 0.07)],
                vec![v2(-0.74, -0.07), v2(-0.8, 0.07)],
                circle(v2(-0.93, 0.0), 0.07, 8),
            ],
            Shape::Hammer => vec![
                // Handle, then a big block head with bands.
                vec![
                    v2(0.45, -0.07),
                    v2(-1.0, -0.07),
                    v2(-1.0, 0.07),
                    v2(0.45, 0.07),
                ],
                vec![
                    v2(0.45, -0.55),
                    v2(0.95, -0.55),
                    v2(0.95, 0.55),
                    v2(0.45, 0.55),
                    v2(0.45, -0.55),
                ],
                vec![v2(0.45, -0.32), v2(0.95, -0.32)],
                vec![v2(0.45, 0.32), v2(0.95, 0.32)],
                vec![v2(-0.88, -0.07), v2(-0.8, 0.07)],
                vec![v2(-0.74, -0.07), v2(-0.66, 0.07)],
            ],
            Shape::Bow => {
                // The bow bends right; the string drawn back with an arrow on it.
                let limb: Vec<V2> = (0..=20)
                    .map(|i| {
                        let y = -0.95 + 1.9 * i as f32 / 20.0;
                        v2(-0.2 + 0.55 * (1.0 - y * y), y)
                    })
                    .collect();
                vec![
                    limb,
                    vec![v2(-0.2, -0.95), v2(-0.45, 0.0), v2(-0.2, 0.95)],
                    vec![v2(-0.45, 0.0), v2(0.95, 0.0)],
                    vec![v2(0.8, -0.1), v2(0.97, 0.0), v2(0.8, 0.1)],
                    vec![v2(-0.6, -0.12), v2(-0.45, 0.0), v2(-0.6, 0.12)],
                ]
            }
            Shape::Bomb => {
                let shine: Vec<V2> = (0..=6)
                    .map(|i| {
                        let a = PI * (1.1 + 0.35 * i as f32 / 6.0);
                        v2(-0.1, 0.2) + v2(a.cos(), a.sin()) * 0.48
                    })
                    .collect();
                let mut s = vec![
                    circle(v2(-0.1, 0.2), 0.7, 28),
                    shine,
                    vec![
                        v2(-0.25, -0.5),
                        v2(-0.25, -0.66),
                        v2(0.05, -0.66),
                        v2(0.05, -0.5),
                    ],
                    vec![
                        v2(-0.1, -0.66),
                        v2(0.05, -0.85),
                        v2(0.28, -0.8),
                        v2(0.42, -0.9),
                    ],
                ];
                for i in 0..4 {
                    let a = i as f32 / 4.0 * TAU + 0.4;
                    let d = v2(a.cos(), a.sin()) * 0.11;
                    s.push(vec![v2(0.48, -0.9) + d * 0.3, v2(0.48, -0.9) + d]);
                }
                s
            }
            Shape::Spiral => vec![(0..=60)
                .map(|i| {
                    let k = i as f32 / 60.0;
                    let a = k * 3.0 * TAU;
                    v2(a.cos(), a.sin()) * (0.08 + 0.92 * k)
                })
                .collect()],
        }
    }
}

/// A point of a shape's unit box on the page: centred on `origin`, `size`
/// points to the edge, mirrored left-right when `mirror` (facing left).
pub fn place(p: V2, origin: V2, size: f32, mirror: bool) -> V2 {
    let x = if mirror { -p.x } else { p.x };
    origin + v2(x, p.y) * size
}

/// Ink `shape` up to `progress` (0..1), each unit-box point placed by `map`.
/// Weapons are drawn with a bold marker, doodles with a fine pen.
pub fn ink(out: &mut DrawList, shape: Shape, progress: f32, alpha: f32, map: &dyn Fn(V2) -> V2) {
    if alpha <= 0.0 || progress <= 0.0 {
        return;
    }
    let strokes = shape.strokes();
    let total: f32 = strokes.iter().map(|s| length(s)).sum();
    let c = shape.color();
    let (outline, line) = if shape.weapon() {
        (3.6, 2.3)
    } else {
        (2.6, 1.5)
    };
    for (color, half) in [
        ([1.0, 1.0, 1.0, 0.9 * alpha], outline),
        ([c[0], c[1], c[2], alpha], line),
    ] {
        let mut left = progress * total;
        'strokes: for s in &strokes {
            for w in s.windows(2) {
                if left <= 0.0 {
                    break 'strokes;
                }
                let d = (w[1] - w[0]).length();
                let end = if left < d {
                    w[0].lerp(w[1], left / d)
                } else {
                    w[1]
                };
                left -= d;
                out.capsule(map(w[0]), map(end), half, color);
            }
        }
    }
}

fn length(stroke: &[V2]) -> f32 {
    stroke.windows(2).map(|w| (w[1] - w[0]).length()).sum()
}

/// Where the pen is at progress `k` (0..1 over all strokes, in unit coords).
pub fn pen_at(shape: Shape, k: f32) -> V2 {
    let strokes = shape.strokes();
    let total: f32 = strokes.iter().map(|s| length(s)).sum();
    let mut left = k.clamp(0.0, 1.0) * total;
    for s in &strokes {
        for w in s.windows(2) {
            let d = (w[1] - w[0]).length();
            if left <= d {
                return w[0].lerp(w[1], if d > 0.0 { left / d } else { 0.0 });
            }
            left -= d;
        }
    }
    strokes
        .last()
        .and_then(|s| s.last())
        .copied()
        .unwrap_or(V2::ZERO)
}

/// One drawing on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Doodle {
    pub shape: Shape,
    /// Centre, world points.
    pub origin: V2,
    /// Half size, points.
    pub size: f32,
    /// Drawn facing left.
    pub mirror: bool,
    /// How much has been drawn (0..1).
    pub progress: f32,
    pub age: f32,
}

impl Doodle {
    /// Draw the inked part.
    pub fn draw(&self, out: &mut DrawList) {
        let alpha = 1.0 - ((self.age - LIFETIME) / FADE).clamp(0.0, 1.0);
        let (o, size, mirror) = (self.origin, self.size, self.mirror);
        ink(out, self.shape, self.progress, alpha, &|p| {
            place(p, o, size, mirror)
        });
    }
}

/// Every doodle on screen.
#[derive(Debug, Default)]
pub struct Doodles {
    pub list: Vec<Doodle>,
}

impl Doodles {
    pub fn start(&mut self, shape: Shape, origin: V2, size: f32, mirror: bool) {
        if self.list.len() >= MAX_DOODLES {
            self.list.remove(0);
        }
        self.list.push(Doodle {
            shape,
            origin,
            size,
            mirror,
            progress: 0.0,
            age: 0.0,
        });
    }

    /// The figure's pen has reached `k` on the newest doodle.
    pub fn set_progress(&mut self, k: f32) {
        if let Some(d) = self.list.last_mut() {
            d.progress = d.progress.max(k.clamp(0.0, 1.0));
        }
    }

    /// Take the newest drawing of `shape` off the page (it's picked up).
    pub fn take(&mut self, shape: Shape) -> Option<Doodle> {
        let i = self.list.iter().rposition(|d| d.shape == shape)?;
        Some(self.list.remove(i))
    }

    pub fn update(&mut self, dt: f32) {
        for d in &mut self.list {
            d.age += dt;
        }
        self.list.retain(|d| d.age < LIFETIME + FADE);
    }

    /// Something changes every frame (fading out).
    pub fn animating(&self) -> bool {
        self.list.iter().any(|d| d.age > LIFETIME)
    }

    pub fn draw(&self, out: &mut DrawList) {
        for d in &self.list {
            d.draw(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_fit_the_unit_box_and_pen_moves_continuously() {
        for shape in Shape::ALL.into_iter().chain(Shape::WEAPONS) {
            for s in shape.strokes() {
                assert!(s.len() >= 2);
                assert!(
                    s.iter().all(|p| p.x.abs() <= 1.05 && p.y.abs() <= 1.05),
                    "{shape:?}"
                );
            }
            assert!((pen_at(shape, 0.0) - shape.strokes()[0][0]).length() < 1e-4);
            // Pen travel per 1% step is small (no teleports within a stroke).
            let mut prev = pen_at(shape, 0.0);
            let mut jumps = 0;
            for i in 1..=100 {
                let p = pen_at(shape, i as f32 / 100.0);
                if (p - prev).length() > 0.4 {
                    jumps += 1; // lifting the pen to the next stroke
                }
                prev = p;
            }
            assert!(jumps < shape.strokes().len(), "{shape:?}: {jumps} jumps");
        }
    }

    #[test]
    fn weapons_are_drawn_bolder_and_mirror_when_facing_left() {
        let right = place(v2(1.0, 0.5), v2(100.0, 100.0), 10.0, false);
        let left = place(v2(1.0, 0.5), v2(100.0, 100.0), 10.0, true);
        assert_eq!(right, v2(110.0, 105.0));
        assert_eq!(left, v2(90.0, 105.0));
        for w in Shape::WEAPONS {
            assert!(w.weapon());
            let g = w.grip();
            assert!(g.x.abs() <= 1.0 && g.y.abs() <= 1.0);
        }
        assert!(!Shape::Heart.weapon());
    }

    #[test]
    fn draws_only_what_has_been_inked_then_fades() {
        let mut d = Doodles::default();
        d.start(Shape::Star, v2(100.0, 100.0), 25.0, false);
        let count = |d: &Doodles| {
            let mut dl = DrawList::default();
            d.draw(&mut dl);
            dl.len()
        };
        assert_eq!(count(&d), 0);
        d.set_progress(0.5);
        let half = count(&d);
        d.set_progress(1.0);
        let full = count(&d);
        assert!(half > 0 && full > half, "{half} {full}");
        d.set_progress(0.2);
        assert_eq!(count(&d), full, "progress never goes back");
        for _ in 0..(31 * 60) {
            d.update(1.0 / 60.0);
        }
        assert!(d.animating());
        for _ in 0..(3 * 60) {
            d.update(1.0 / 60.0);
        }
        assert!(d.list.is_empty());
    }
}
