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
    /// A cannon it draws to fight with (not one of its idle doodles).
    Cannon,
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

    pub fn color(self) -> [f32; 4] {
        match self {
            Shape::Heart => [0.9, 0.2, 0.3, 1.0],
            Shape::Smiley => [0.95, 0.6, 0.1, 1.0],
            Shape::Star => [0.95, 0.78, 0.1, 1.0],
            Shape::House => [0.2, 0.45, 0.85, 1.0],
            Shape::Sun => [1.0, 0.55, 0.1, 1.0],
            Shape::Spiral => [0.55, 0.3, 0.85, 1.0],
            Shape::Cannon => [0.1, 0.1, 0.12, 1.0],
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
            Shape::Cannon => vec![
                // Barrel, pointing right.
                vec![
                    v2(-0.9, -0.35),
                    v2(0.9, -0.5),
                    v2(0.9, 0.05),
                    v2(-0.9, 0.2),
                    v2(-0.9, -0.35),
                ],
                // Muzzle ring and the wheel.
                vec![v2(0.9, -0.62), v2(1.0, -0.22), v2(0.9, 0.17)],
                circle(v2(-0.35, 0.55), 0.38, 16),
                // A fuse.
                vec![v2(-0.9, -0.1), v2(-1.0, -0.5), v2(-0.8, -0.75)],
            ],
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
    /// How much has been drawn (0..1).
    pub progress: f32,
    pub age: f32,
}

impl Doodle {
    /// Draw the inked part.
    pub fn draw(&self, out: &mut DrawList) {
        let alpha = 1.0 - ((self.age - LIFETIME) / FADE).clamp(0.0, 1.0);
        if alpha <= 0.0 || self.progress <= 0.0 {
            return;
        }
        let strokes = self.shape.strokes();
        let total: f32 = strokes.iter().map(|s| length(s)).sum();
        let ink = self.shape.color();
        for (color, half) in [
            ([1.0, 1.0, 1.0, 0.85 * alpha], 2.6),
            ([ink[0], ink[1], ink[2], alpha], 1.5),
        ] {
            let mut left = self.progress * total;
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
                    out.capsule(
                        self.origin + w[0] * self.size,
                        self.origin + end * self.size,
                        half,
                        color,
                    );
                }
            }
        }
    }
}

/// Every doodle on screen.
#[derive(Debug, Default)]
pub struct Doodles {
    pub list: Vec<Doodle>,
}

impl Doodles {
    pub fn start(&mut self, shape: Shape, origin: V2, size: f32) {
        if self.list.len() >= MAX_DOODLES {
            self.list.remove(0);
        }
        self.list.push(Doodle {
            shape,
            origin,
            size,
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
        for shape in Shape::ALL {
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
    fn draws_only_what_has_been_inked_then_fades() {
        let mut d = Doodles::default();
        d.start(Shape::Star, v2(100.0, 100.0), 25.0);
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
