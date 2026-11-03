//! Display layout: every connected screen, its ground line, and how the figure
//! may travel between screens.
//!
//! World coordinates are global CoreGraphics points: origin at the top-left of
//! the menu-bar screen, y down. Other screens can sit at negative coordinates.

#![allow(unexpected_cfgs)] // objc 0.2 macros

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};

use crate::geom::{cg_to_cocoa, v2, Rect, V2};
use crate::physics::{Platform, PlatformKind};

/// One screen: its frame and where the figure stands on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// Global CoreGraphics points.
    pub frame: Rect,
    /// Ground line (top of the Dock, or the bottom of the screen), global y.
    pub ground_y: f32,
}

/// Ground line for a screen: the top edge of the Dock if it sits along the
/// bottom, otherwise the bottom of the screen. `visible` is the screen's
/// visible frame (menu bar and Dock excluded), both in global CG points.
pub fn ground_line(frame: Rect, visible: Option<Rect>) -> f32 {
    let bottom = frame.bottom();
    visible
        .map(|v| v.bottom())
        .filter(|g| *g > frame.y + frame.height * 0.5 && *g <= bottom)
        .unwrap_or(bottom) as f32
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
// SAFETY: same layout as CGRect on 64-bit; a valid CGRect type encoding.
unsafe impl Encode for NSRect {
    fn encode() -> Encoding {
        // SAFETY: a valid Objective-C type encoding for CGRect.
        unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

/// Every connected screen, menu-bar screen first. Main thread only.
pub fn current() -> Vec<Screen> {
    let mut out = Vec::new();
    // SAFETY: documented NSScreen API on the main thread (caller contract);
    // the array and screens are autoreleased and only read here.
    unsafe {
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return out;
        }
        let count: usize = msg_send![screens, count];
        let mut primary_h = 0.0;
        for i in 0..count {
            let s: *mut Object = msg_send![screens, objectAtIndex: i];
            let f: NSRect = msg_send![s, frame];
            let v: NSRect = msg_send![s, visibleFrame];
            if i == 0 {
                primary_h = f.h;
            }
            let to_cg = |r: NSRect| {
                cg_to_cocoa(
                    Rect {
                        x: r.x,
                        y: r.y,
                        width: r.w,
                        height: r.h,
                    },
                    primary_h,
                )
            };
            let frame = to_cg(f);
            out.push(Screen {
                frame,
                ground_y: ground_line(frame, Some(to_cg(v))),
            });
        }
    }
    out
}

/// Where the figure may walk this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub min_x: f32,
    pub max_x: f32,
    /// Top of the screen the figure is on (its ceiling).
    pub top: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub screens: Vec<Screen>,
}

impl Layout {
    pub fn new(screens: Vec<Screen>) -> Layout {
        Layout { screens }
    }

    pub fn frames(&self) -> Vec<Rect> {
        self.screens.iter().map(|s| s.frame).collect()
    }

    /// The menu-bar screen.
    pub fn primary(&self) -> Screen {
        self.screens.first().copied().unwrap_or(Screen {
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 1440.0,
                height: 900.0,
            },
            ground_y: 900.0,
        })
    }

    /// One ground platform per screen.
    pub fn grounds(&self) -> Vec<Platform> {
        self.screens
            .iter()
            .map(|s| Platform {
                x0: s.frame.x as f32,
                x1: s.frame.right() as f32,
                y: s.ground_y,
                kind: PlatformKind::Ground,
                owner: 0,
            })
            .collect()
    }

    /// The screen containing `p`, or else the nearest one.
    pub fn index_at(&self, p: V2) -> usize {
        let dist = |r: &Rect| {
            let cx = f64::from(p.x).clamp(r.x, r.right());
            let cy = f64::from(p.y).clamp(r.y, r.bottom());
            (cx - f64::from(p.x)).powi(2) + (cy - f64::from(p.y)).powi(2)
        };
        self.screens
            .iter()
            .enumerate()
            .min_by(|a, b| dist(&a.1.frame).total_cmp(&dist(&b.1.frame)))
            .map_or(0, |(i, _)| i)
    }

    pub fn screen_at(&self, p: V2) -> Screen {
        self.screens
            .get(self.index_at(p))
            .copied()
            .unwrap_or_else(|| self.primary())
    }

    /// Bounding box of every screen.
    pub fn bounds(&self) -> Rect {
        let mut it = self.screens.iter().map(|s| s.frame);
        let Some(first) = it.next() else {
            return self.primary().frame;
        };
        it.fold(first, |a, b| {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
            Rect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            }
        })
    }

    /// Whether the figure, feet at `feet`, can pass from its screen into `next`
    /// across a shared edge: the edge must span the figure's height there, and
    /// the far side's ground must not be a step up.
    fn passable(next: &Screen, feet: V2) -> bool {
        let y = f64::from(feet.y) - 1.0;
        y >= next.frame.y && y <= next.frame.bottom() && next.ground_y >= feet.y - 2.0
    }

    /// Horizontal range reachable from `feet` (walls at outer screen edges and
    /// at edges that would be a step up), inset by `margin`.
    pub fn walk_range(&self, feet: V2, margin: f32) -> Range {
        let here = self.screen_at(feet - v2(0.0, 1.0));
        let (mut lo, mut hi) = (here.frame.x, here.frame.right());
        // Extend right, then left, through adjacent passable screens.
        for _ in 0..self.screens.len() {
            match self
                .screens
                .iter()
                .find(|s| (s.frame.x - hi).abs() < 1.0 && Self::passable(s, feet))
            {
                Some(s) => hi = s.frame.right(),
                None => break,
            }
        }
        for _ in 0..self.screens.len() {
            match self
                .screens
                .iter()
                .find(|s| (s.frame.right() - lo).abs() < 1.0 && Self::passable(s, feet))
            {
                Some(s) => lo = s.frame.x,
                None => break,
            }
        }
        Range {
            min_x: lo as f32 + margin,
            max_x: hi as f32 - margin,
            top: here.frame.y as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// Laptop (with Dock) on the left, a taller monitor to its right whose
    /// bottom is 100 pt lower, and a monitor above the laptop.
    fn layout() -> Layout {
        Layout::new(vec![
            Screen {
                frame: r(0.0, 0.0, 1800.0, 1169.0),
                ground_y: 1089.0,
            },
            Screen {
                frame: r(1800.0, -200.0, 2560.0, 1469.0),
                ground_y: 1269.0,
            },
            Screen {
                frame: r(0.0, -1080.0, 1920.0, 1080.0),
                ground_y: 0.0,
            },
        ])
    }

    #[test]
    fn ground_sits_on_top_of_a_bottom_dock() {
        let f = r(0.0, 0.0, 1800.0, 1169.0);
        // Menu bar 37 pt, Dock 80 pt.
        assert_eq!(ground_line(f, Some(r(0.0, 37.0, 1800.0, 1052.0))), 1089.0);
        // Dock on the side: ground is the screen bottom.
        assert_eq!(ground_line(f, Some(r(70.0, 37.0, 1730.0, 1132.0))), 1169.0);
        assert_eq!(ground_line(f, None), 1169.0);
        // A screen below the primary.
        let below = r(0.0, 1169.0, 1920.0, 1080.0);
        assert_eq!(
            ground_line(below, Some(r(0.0, 1169.0, 1920.0, 1080.0))),
            2249.0
        );
    }

    #[test]
    fn walks_across_into_a_lower_screen_but_not_back_up() {
        let l = layout();
        // On the laptop's Dock: the monitor's ground is lower, so it may walk
        // off the edge and drop onto it.
        let on_laptop = l.walk_range(v2(900.0, 1089.0), 30.0);
        assert_eq!((on_laptop.min_x, on_laptop.max_x), (30.0, 4330.0));
        assert_eq!(on_laptop.top, 0.0);
        // On the monitor's floor (below the laptop's bottom edge): a wall.
        let on_monitor = l.walk_range(v2(3000.0, 1269.0), 30.0);
        assert_eq!((on_monitor.min_x, on_monitor.max_x), (1830.0, 4330.0));
        assert_eq!(on_monitor.top, -200.0);
        // Standing on a window on the monitor at laptop height, the laptop's
        // Dock is a step up: still a wall.
        let on_window = l.walk_range(v2(3000.0, 1150.0), 30.0);
        assert_eq!(on_window.min_x, 1830.0);
        // On a window high on the monitor, the laptop's ground is below: open.
        let high = l.walk_range(v2(3000.0, 500.0), 30.0);
        assert_eq!(high.min_x, 30.0);
    }

    #[test]
    fn grounds_and_lookup() {
        let l = layout();
        let g = l.grounds();
        assert_eq!(g.len(), 3);
        assert_eq!((g[1].x0, g[1].x1, g[1].y), (1800.0, 4360.0, 1269.0));
        assert_eq!(l.index_at(v2(100.0, -500.0)), 2);
        assert_eq!(l.index_at(v2(5000.0, 0.0)), 1, "nearest when off-screen");
        let b = l.bounds();
        assert_eq!(
            (b.x, b.y, b.right(), b.bottom()),
            (0.0, -1080.0, 4360.0, 1269.0)
        );
    }

    #[test]
    fn single_screen_matches_the_old_behaviour() {
        let l = Layout::new(vec![Screen {
            frame: r(0.0, 0.0, 1800.0, 1169.0),
            ground_y: 1089.0,
        }]);
        let range = l.walk_range(v2(900.0, 1089.0), 30.0);
        assert_eq!((range.min_x, range.max_x, range.top), (30.0, 1770.0, 0.0));
        assert_eq!(l.grounds(), vec![Platform::ground(1800.0, 1089.0)]);
    }
}
