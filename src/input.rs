//! Global mouse tracking (PRD 3.3): cursor position from the window server every
//! tick, a smoothed velocity estimate, and the "the figure punched the cursor"
//! push that physically moves the user's pointer.

use std::time::Instant;

use core_graphics::display::CGDisplay;
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::geom::{v2, V2};

/// Global cursor position in CoreGraphics coordinates (points, top-left origin).
pub fn global_cursor() -> Option<V2> {
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    let event = CGEvent::new(source).ok()?;
    let p = event.location();
    Some(v2(p.x as f32, p.y as f32))
}

/// Exponentially smoothed velocity from position samples.
#[derive(Debug, Clone, Copy)]
pub struct VelocityEstimator {
    last: Option<V2>,
    vel: V2,
    /// Time constant of the smoothing, seconds.
    tau: f32,
}

impl VelocityEstimator {
    pub fn new(tau: f32) -> Self {
        VelocityEstimator {
            last: None,
            vel: V2::ZERO,
            tau,
        }
    }

    pub fn update(&mut self, pos: V2, dt: f32) -> V2 {
        if let Some(last) = self.last {
            if dt > 0.0 {
                let raw = (pos - last) / dt;
                let k = 1.0 - (-dt / self.tau).exp();
                self.vel += (raw - self.vel) * k;
            }
        }
        self.last = Some(pos);
        self.vel
    }

    /// Forget the last sample (after we moved the cursor ourselves).
    pub fn rebase(&mut self, pos: V2) {
        self.last = Some(pos);
    }

    pub fn velocity(&self) -> V2 {
        self.vel
    }
}

/// The cursor as seen from the overlay.
pub struct Cursor {
    /// Overlay-local position (global minus the display origin).
    pub pos: V2,
    origin: V2,
    estimator: VelocityEstimator,
    last_sample: Instant,
    /// Speed history for the director (mean over the last few seconds).
    mean_speed: f32,
}

impl Cursor {
    pub fn new(display_origin: V2) -> Cursor {
        Cursor {
            pos: V2::ZERO,
            origin: display_origin,
            estimator: VelocityEstimator::new(0.05),
            last_sample: Instant::now(),
            mean_speed: 0.0,
        }
    }

    /// Read the pointer from the window server. Cheap enough for every tick.
    pub fn sample(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_sample).as_secs_f32();
        self.last_sample = now;
        if let Some(g) = global_cursor() {
            self.pos = g - self.origin;
            let v = self.estimator.update(self.pos, dt);
            let k = 1.0 - (-dt / 3.0).exp();
            self.mean_speed += (v.length() - self.mean_speed) * k;
        }
    }

    pub fn velocity(&self) -> V2 {
        self.estimator.velocity()
    }

    /// Average speed over the last few seconds (points/s).
    pub fn mean_speed(&self) -> f32 {
        self.mean_speed
    }

    /// Shove the pointer by `by` points (clamped to the display). The user's
    /// physical mouse is unaffected afterwards: the pointer just jumps.
    pub fn push(&mut self, by: V2, display_size: V2) {
        let target = (self.pos + by).clamp(V2::splat(2.0), display_size - V2::splat(2.0));
        let global = target + self.origin;
        if CGDisplay::warp_mouse_cursor_position(CGPoint::new(
            f64::from(global.x),
            f64::from(global.y),
        ))
        .is_ok()
        {
            // Re-attach the pointer to the mouse immediately (warping otherwise
            // freezes it for ~250 ms).
            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
            self.pos = target;
            self.estimator.rebase(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocity_tracks_constant_motion() {
        let mut e = VelocityEstimator::new(0.05);
        let mut v = V2::ZERO;
        for i in 0..60 {
            v = e.update(v2(i as f32 * 10.0, 0.0), 1.0 / 60.0);
        }
        assert!((v.x - 600.0).abs() < 5.0, "{v:?}");
    }

    #[test]
    fn velocity_reacts_within_50ms() {
        // PRD 5: spike → reaction under 50 ms. After 3 ticks (50 ms) of a fast
        // swipe the estimate must already exceed the dodge threshold (1100 pt/s).
        let mut e = VelocityEstimator::new(0.05);
        for _ in 0..30 {
            e.update(V2::ZERO, 1.0 / 60.0);
        }
        let mut x = 0.0;
        let mut v = V2::ZERO;
        for _ in 0..3 {
            x += 3000.0 / 60.0;
            v = e.update(v2(x, 0.0), 1.0 / 60.0);
        }
        assert!(v.x > 1100.0, "{v:?}");
    }

    #[test]
    fn rebase_avoids_a_fake_spike() {
        let mut e = VelocityEstimator::new(0.05);
        e.update(v2(0.0, 0.0), 1.0 / 60.0);
        e.rebase(v2(500.0, 0.0));
        let v = e.update(v2(500.0, 0.0), 1.0 / 60.0);
        assert!(v.length() < 1.0);
    }

    #[test]
    fn cursor_is_readable() {
        // Needs a window-server session; skip gracefully on headless CI.
        if std::env::var_os("STICKMAN_SKIP_GUI_TESTS").is_none() {
            assert!(global_cursor().is_some());
        }
    }
}
