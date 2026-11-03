//! Drawing for the figure: in draw mode the user sketches lines with the
//! mouse and they become platforms (or, drawn with Shift, trampolines).
//!
//! Physics platforms are horizontal, so each stroke is fitted with a flat
//! platform at its median height; the stroke is drawn slightly straightened so
//! the figure's feet line up with what you see.

use crate::geom::{v2, V2};
use crate::physics::{Platform, PlatformKind};
use crate::render::DrawList;

/// Shortest stroke (horizontal span, points) that makes a platform.
pub const MIN_SPAN: f32 = 30.0;
/// At most this many drawings; the oldest goes.
pub const MAX_DRAWINGS: usize = 12;
/// Owner ids for drawn platforms start here (window ids are much smaller).
const OWNER_BASE: u32 = 0x7000_0000;

#[derive(Debug, Clone, PartialEq)]
pub struct Drawing {
    pub points: Vec<V2>,
    pub platform: Platform,
}

/// The flat platform for a stroke: from its leftmost to its rightmost point,
/// at the median height. `None` if it's too short.
pub fn fit(points: &[V2], trampoline: bool, owner: u32) -> Option<Platform> {
    let x0 = points.iter().map(|p| p.x).fold(f32::MAX, f32::min);
    let x1 = points.iter().map(|p| p.x).fold(f32::MIN, f32::max);
    if points.len() < 2 || x1 - x0 < MIN_SPAN {
        return None;
    }
    let mut ys: Vec<f32> = points.iter().map(|p| p.y).collect();
    ys.sort_by(f32::total_cmp);
    let y = ys[ys.len() / 2];
    Some(Platform {
        x0,
        x1,
        y,
        kind: if trampoline {
            PlatformKind::Trampoline
        } else {
            PlatformKind::Drawn
        },
        owner,
    })
}

/// Everything drawn so far, and the stroke in progress.
#[derive(Debug, Default)]
pub struct Sketchpad {
    pub drawings: Vec<Drawing>,
    pub stroke: Option<Vec<V2>>,
    next_owner: u32,
}

impl Sketchpad {
    pub fn begin(&mut self, at: V2) {
        self.stroke = Some(vec![at]);
    }

    /// Add a point to the stroke in progress (skipping tiny moves).
    pub fn extend(&mut self, at: V2) {
        if let Some(s) = &mut self.stroke {
            if s.last().is_none_or(|p| (*p - at).length() >= 3.0) {
                s.push(at);
            }
        }
    }

    /// Finish the stroke. Returns the new platform, if it made one.
    pub fn finish(&mut self, trampoline: bool) -> Option<Platform> {
        let points = self.stroke.take()?;
        let platform = fit(&points, trampoline, OWNER_BASE + self.next_owner)?;
        self.next_owner = self.next_owner.wrapping_add(1);
        // Straighten it most of the way to the platform line.
        let points = points
            .into_iter()
            .map(|p| v2(p.x, platform.y + (p.y - platform.y) * 0.15))
            .collect();
        if self.drawings.len() >= MAX_DRAWINGS {
            self.drawings.remove(0);
        }
        self.drawings.push(Drawing { points, platform });
        Some(platform)
    }

    pub fn clear(&mut self) {
        self.drawings.clear();
        self.stroke = None;
    }

    pub fn platforms(&self) -> impl Iterator<Item = Platform> + '_ {
        self.drawings.iter().map(|d| d.platform)
    }

    pub fn draw(&self, out: &mut DrawList) {
        let ink = |out: &mut DrawList, pts: &[V2], color: [f32; 4]| {
            for (c, half) in [([1.0, 1.0, 1.0, 0.9], 3.6), (color, 2.3)] {
                for w in pts.windows(2) {
                    out.capsule(w[0], w[1], half, c);
                }
            }
        };
        for d in &self.drawings {
            if d.platform.kind == PlatformKind::Trampoline {
                // Springs under the mat.
                let (x0, x1, y) = (d.platform.x0, d.platform.x1, d.platform.y);
                let n = ((x1 - x0) / 28.0).ceil().max(2.0) as usize;
                for i in 0..=n {
                    let x = x0 + (x1 - x0) * i as f32 / n as f32;
                    let zig: Vec<V2> = (0..=4)
                        .map(|k| v2(x + if k % 2 == 0 { 0.0 } else { 5.0 }, y + 4.0 * k as f32))
                        .collect();
                    ink(out, &zig, [0.35, 0.35, 0.4, 1.0]);
                }
                ink(out, &d.points, [0.95, 0.45, 0.1, 1.0]);
            } else {
                ink(out, &d.points, [0.15, 0.4, 0.9, 1.0]);
            }
        }
        if let Some(s) = &self.stroke {
            ink(out, s, [0.15, 0.4, 0.9, 0.8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wobbly_stroke_becomes_a_flat_platform() {
        let pts: Vec<V2> = (0..30)
            .map(|i| v2(100.0 + 10.0 * i as f32, 500.0 + 6.0 * (i as f32).sin()))
            .collect();
        let p = fit(&pts, false, 1).unwrap();
        assert_eq!((p.x0, p.x1), (100.0, 390.0));
        assert!((p.y - 500.0).abs() < 6.0);
        assert_eq!(p.kind, PlatformKind::Drawn);
        assert!(
            fit(&[v2(0.0, 0.0), v2(10.0, 50.0)], false, 1).is_none(),
            "too short"
        );
        assert_eq!(fit(&pts, true, 1).unwrap().kind, PlatformKind::Trampoline);
    }

    #[test]
    fn sketchpad_keeps_the_last_dozen_and_straightens_strokes() {
        let mut pad = Sketchpad::default();
        for k in 0..(MAX_DRAWINGS + 3) {
            pad.begin(v2(0.0, 100.0 + k as f32));
            for i in 1..20 {
                pad.extend(v2(
                    5.0 * i as f32,
                    100.0 + k as f32 + if i % 2 == 0 { 8.0 } else { -8.0 },
                ));
            }
            assert!(pad.finish(false).is_some());
        }
        assert_eq!(pad.drawings.len(), MAX_DRAWINGS);
        let d = &pad.drawings[0];
        assert!(d.points.iter().all(|p| (p.y - d.platform.y).abs() <= 2.5));
        let owners: std::collections::HashSet<u32> = pad.platforms().map(|p| p.owner).collect();
        assert_eq!(
            owners.len(),
            MAX_DRAWINGS,
            "each drawing is its own platform"
        );
        pad.begin(v2(0.0, 0.0));
        assert!(pad.finish(true).is_none(), "a dot isn't a platform");
        pad.clear();
        assert!(pad.drawings.is_empty());
    }
}
