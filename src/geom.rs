//! Shared geometry types. All overlay coordinates are points, origin at the top-left
//! of the main display, y pointing down.

/// 2D vector (glam `Vec2`, re-exported by rapier).
pub type V2 = rapier2d::math::Vector;

pub const fn v2(x: f32, y: f32) -> V2 {
    V2::new(x, y)
}

/// A rectangle in points: `x`, `y` is the origin, in whichever coordinate space the
/// caller documents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, p: V2) -> bool {
        let (x, y) = (f64::from(p.x), f64::from(p.y));
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }
}

/// Convert between CoreGraphics rects (global, top-left origin, y down) and AppKit
/// screen frames (bottom-left origin of the primary display, y up). The mapping
/// is its own inverse.
pub fn cg_to_cocoa(rect: Rect, primary_height: f64) -> Rect {
    Rect {
        y: primary_height - (rect.y + rect.height),
        ..rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_display_maps_to_origin() {
        let d = Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1169.0,
        };
        assert_eq!(cg_to_cocoa(d, 1169.0), d);
    }

    #[test]
    fn display_below_primary_gets_negative_cocoa_y() {
        let d = Rect {
            x: 0.0,
            y: 1169.0,
            width: 1920.0,
            height: 1080.0,
        };
        let c = cg_to_cocoa(d, 1169.0);
        assert_eq!(
            (c.x, c.y, c.width, c.height),
            (0.0, -1080.0, 1920.0, 1080.0)
        );
    }

    #[test]
    fn conversion_round_trips() {
        let d = Rect {
            x: 37.5,
            y: -412.25,
            width: 1280.0,
            height: 720.0,
        };
        assert_eq!(cg_to_cocoa(cg_to_cocoa(d, 982.0), 982.0), d);
    }

    #[test]
    fn rect_contains() {
        let r = Rect {
            x: 10.0,
            y: 10.0,
            width: 100.0,
            height: 50.0,
        };
        assert!(r.contains(v2(10.0, 60.0)));
        assert!(!r.contains(v2(9.9, 30.0)));
        assert_eq!((r.right(), r.bottom()), (110.0, 60.0));
    }
}
