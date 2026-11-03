//! Collision world: everything the figure can stand on, stored as rapier2d static
//! segment colliders and queried with rapier's ray and shape casts.
//!
//! Platforms are **one-way**: the figure can jump up through a window's title bar
//! from below and land on it from above, like a platformer. That is done with a
//! query-filter predicate that ignores any platform above the feet.

use rapier2d::prelude::*;

use crate::geom::V2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformKind {
    /// Top of the Dock / bottom of the screen. Always present.
    Ground,
    /// Visible part of a window's top edge.
    Window,
    /// Top of a line of text recognised by OCR.
    Text,
}

/// A horizontal surface from `x0` to `x1` at height `y` (overlay points, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Platform {
    pub x0: f32,
    pub x1: f32,
    pub y: f32,
    pub kind: PlatformKind,
    /// CGWindowID of the window this belongs to (0 for the ground).
    pub owner: u32,
}

impl Platform {
    pub fn ground(width: f32, y: f32) -> Platform {
        Platform {
            x0: 0.0,
            x1: width,
            y,
            kind: PlatformKind::Ground,
            owner: 0,
        }
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn contains_x(&self, x: f32) -> bool {
        x >= self.x0 && x <= self.x1
    }
}

/// How far below the feet a platform may be and still count as "standing on it".
const SUPPORT_TOLERANCE: f32 = 3.0;

pub struct World {
    pw: PhysicsWorld,
    colliders: Vec<ColliderHandle>,
    platforms: Vec<Platform>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> World {
        World {
            pw: PhysicsWorld::new(),
            colliders: Vec::new(),
            platforms: Vec::new(),
        }
    }

    pub fn platforms(&self) -> &[Platform] {
        &self.platforms
    }

    /// Replace the platform set. Returns `false` (and does nothing) if unchanged.
    pub fn set_platforms(&mut self, platforms: Vec<Platform>) -> bool {
        if platforms == self.platforms {
            return false;
        }
        for h in self.colliders.drain(..) {
            self.pw.remove_collider(h);
        }
        for (i, p) in platforms.iter().enumerate() {
            let collider = ColliderBuilder::segment(V2::new(p.x0, p.y), V2::new(p.x1, p.y))
                .user_data(i as u128)
                .build();
            self.colliders.push(self.pw.insert_collider(collider, None));
        }
        self.platforms = platforms;
        // Static colliders enter the query BVH on the next step; with no bodies a
        // step is just a broad-phase update.
        self.pw.step();
        true
    }

    fn platform_of(&self, c: &Collider) -> Option<&Platform> {
        self.platforms.get(c.user_data as usize)
    }

    /// Platform directly under `feet` (within a few points), if any.
    pub fn support(&self, feet: V2) -> Option<usize> {
        let pred = |_h: ColliderHandle, c: &Collider| {
            self.platform_of(c)
                .is_some_and(|p| p.y >= feet.y - SUPPORT_TOLERANCE)
        };
        let ray = Ray::new(
            V2::new(feet.x, feet.y - SUPPORT_TOLERANCE),
            V2::new(0.0, 1.0),
        );
        self.pw
            .cast_ray(
                &ray,
                2.0 * SUPPORT_TOLERANCE,
                true,
                QueryFilter::default().predicate(&pred),
            )
            .map(|(h, _)| self.pw.colliders[h].user_data as usize)
    }

    /// One-way landing test for feet moving from `from` to `to`: the first platform
    /// at or below the starting feet that the motion crosses. Returns its index.
    pub fn landing(&self, from: V2, to: V2) -> Option<usize> {
        if to.y <= from.y {
            return None; // moving up: platforms are pass-through from below
        }
        let pred = |_h: ColliderHandle, c: &Collider| {
            self.platform_of(c).is_some_and(|p| p.y >= from.y - 0.5)
        };
        // A continuous ray over the whole motion: no tunnelling at any speed, and
        // the same point semantics as `support`, so edges agree exactly.
        let ray = Ray::new(from, to - from);
        self.pw
            .cast_ray(&ray, 1.0, true, QueryFilter::default().predicate(&pred))
            .map(|(h, _)| self.pw.colliders[h].user_data as usize)
    }

    /// Highest platform reachable by a jump of at most `max_rise` from `feet`, whose
    /// span overlaps `[x_lo, x_hi]` and which is above the feet. Used for climbing.
    pub fn platform_above(&self, feet: V2, max_rise: f32, x_lo: f32, x_hi: f32) -> Option<usize> {
        self.platforms
            .iter()
            .enumerate()
            .filter(|(_, p)| p.y < feet.y - 20.0 && feet.y - p.y <= max_rise)
            .filter(|(_, p)| p.x1 >= x_lo && p.x0 <= x_hi && p.width() >= 40.0)
            .min_by(|a, b| a.1.y.total_cmp(&b.1.y))
            .map(|(i, _)| i)
    }
}

/// Visible top edges of windows, front-to-back order in, occluded parts removed.
/// `windows` are (id, rect) in overlay points, frontmost first.
pub fn window_platforms(
    windows: &[(u32, crate::geom::Rect)],
    width: f32,
    min_y: f32,
) -> Vec<Platform> {
    let mut out = Vec::new();
    for (i, (id, r)) in windows.iter().enumerate() {
        let y = r.y as f32;
        if y < min_y {
            continue;
        }
        let mut spans = vec![((r.x as f32).max(0.0), (r.right() as f32).min(width))];
        // Remove parts covered by any window in front whose area contains this edge.
        for (_, front) in &windows[..i] {
            let (fy0, fy1) = (front.y as f32, front.bottom() as f32);
            if y < fy0 - 1.0 || y > fy1 {
                continue;
            }
            let (cx0, cx1) = (front.x as f32, front.right() as f32);
            spans = spans
                .into_iter()
                .flat_map(|(a, b)| {
                    let mut parts = Vec::new();
                    if cx0 > a {
                        parts.push((a, b.min(cx0)));
                    }
                    if cx1 < b {
                        parts.push((a.max(cx1), b));
                    }
                    parts
                })
                .filter(|(a, b)| b - a > 0.0)
                .collect();
        }
        out.extend(
            spans
                .into_iter()
                .filter(|(a, b)| b - a >= 30.0)
                .map(|(x0, x1)| Platform {
                    x0,
                    x1,
                    y,
                    kind: PlatformKind::Window,
                    owner: *id,
                }),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{v2, Rect};

    fn world_with(ps: Vec<Platform>) -> World {
        let mut w = World::new();
        assert!(w.set_platforms(ps));
        w
    }

    fn plat(x0: f32, x1: f32, y: f32) -> Platform {
        Platform {
            x0,
            x1,
            y,
            kind: PlatformKind::Window,
            owner: 7,
        }
    }

    #[test]
    fn support_finds_platform_under_feet_only() {
        let w = world_with(vec![
            Platform::ground(1000.0, 900.0),
            plat(100.0, 400.0, 500.0),
        ]);
        assert_eq!(w.support(v2(200.0, 500.0)), Some(1));
        assert_eq!(w.support(v2(200.0, 901.0)), Some(0));
        assert_eq!(w.support(v2(450.0, 500.0)), None, "off the window edge");
        assert_eq!(
            w.support(v2(200.0, 520.0)),
            None,
            "20 pt below the window top"
        );
    }

    #[test]
    fn landing_is_one_way() {
        let w = world_with(vec![
            Platform::ground(1000.0, 900.0),
            plat(100.0, 400.0, 500.0),
        ]);
        // Falling onto the window from above.
        assert_eq!(w.landing(v2(200.0, 480.0), v2(200.0, 510.0)), Some(1));
        // Rising through it from below: no collision.
        assert_eq!(w.landing(v2(200.0, 520.0), v2(200.0, 490.0)), None);
        // Falling from just below its top: passes through to the ground.
        assert_eq!(w.landing(v2(200.0, 501.0), v2(200.0, 905.0)), Some(0));
        // Diagonal fall that crosses the edge.
        assert_eq!(w.landing(v2(90.0, 495.0), v2(110.0, 505.0)), Some(1));
    }

    #[test]
    fn fast_fall_does_not_tunnel() {
        let w = world_with(vec![Platform::ground(1000.0, 900.0)]);
        assert_eq!(w.landing(v2(500.0, 100.0), v2(500.0, 2000.0)), Some(0));
    }

    #[test]
    fn set_platforms_detects_no_change() {
        let mut w = world_with(vec![Platform::ground(1000.0, 900.0)]);
        assert!(!w.set_platforms(vec![Platform::ground(1000.0, 900.0)]));
        assert!(w.set_platforms(vec![Platform::ground(1000.0, 880.0)]));
        assert_eq!(w.support(v2(10.0, 880.0)), Some(0));
    }

    #[test]
    fn platform_above_picks_reachable_highest() {
        let w = world_with(vec![
            Platform::ground(1000.0, 900.0),
            plat(100.0, 400.0, 700.0),
            plat(100.0, 400.0, 600.0),
            plat(100.0, 400.0, 200.0), // too high
        ]);
        assert_eq!(
            w.platform_above(v2(250.0, 900.0), 350.0, 200.0, 300.0),
            Some(2)
        );
        assert_eq!(
            w.platform_above(v2(250.0, 900.0), 350.0, 500.0, 600.0),
            None
        );
    }

    #[test]
    fn window_edges_are_clipped_by_windows_in_front() {
        let r = |x, y, w, h| Rect {
            x,
            y,
            width: w,
            height: h,
        };
        // Front window covers the middle of the back window's top edge.
        let windows = [
            (1, r(300.0, 100.0, 200.0, 300.0)),
            (2, r(100.0, 200.0, 600.0, 400.0)),
        ];
        let ps = window_platforms(&windows, 1800.0, 30.0);
        let back: Vec<_> = ps
            .iter()
            .filter(|p| p.owner == 2)
            .map(|p| (p.x0, p.x1))
            .collect();
        assert_eq!(back, [(100.0, 300.0), (500.0, 700.0)]);
        assert!(ps
            .iter()
            .any(|p| p.owner == 1 && p.x0 == 300.0 && p.x1 == 500.0));
    }

    #[test]
    fn window_edges_ignore_menu_bar_and_slivers_and_clip_to_screen() {
        let r = |x, y, w, h| Rect {
            x,
            y,
            width: w,
            height: h,
        };
        let ps = window_platforms(
            &[
                (1, r(0.0, 10.0, 500.0, 300.0)),
                (2, r(1700.0, 300.0, 400.0, 200.0)),
                (3, r(50.0, 400.0, 20.0, 100.0)),
            ],
            1800.0,
            30.0,
        );
        assert_eq!(ps.len(), 1);
        assert_eq!((ps[0].x0, ps[0].x1), (1700.0, 1800.0));
    }
}
