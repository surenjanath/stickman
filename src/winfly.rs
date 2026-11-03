//! A window thrown by telekinesis: it floats up and wobbles, then flies,
//! bounces off the screen edges and slides to a stop, always kept on screen.
//! Pure physics; the app moves the real window to [`WindowFlight::pos`].

use crate::geom::{v2, Rect, V2};

const GRAVITY: f32 = 1500.0;
const LIFT_TIME: f32 = 1.1;
const LIFT_RISE: f32 = 50.0;
const MAX_FLIGHT: f32 = 3.5;
/// Keep windows this far below the top of the screen (the menu bar).
const MENU_BAR: f32 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// Floating up from where it was.
    Lift,
    Fly,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFlight {
    pub pid: i64,
    pub id: u32,
    /// Top-left, global points.
    pub pos: V2,
    pub size: V2,
    origin: V2,
    pub vel: V2,
    pub t: f32,
    pub stage: Stage,
    /// Where it must stay: its screen, above `floor` (the Dock).
    area: Rect,
    floor: f32,
    /// Already struck the cursor on this flight.
    pub hit_cursor: bool,
}

impl WindowFlight {
    pub fn new(pid: i64, id: u32, rect: Rect, screen: Rect, floor: f32) -> WindowFlight {
        let pos = v2(rect.x as f32, rect.y as f32);
        WindowFlight {
            pid,
            id,
            pos,
            size: v2(rect.width as f32, rect.height as f32),
            origin: pos,
            vel: V2::ZERO,
            t: 0.0,
            stage: Stage::Lift,
            area: screen,
            floor,
            hit_cursor: false,
        }
    }

    pub fn center(&self) -> V2 {
        self.pos + self.size / 2.0
    }

    /// Lowest and highest allowed top-left corner.
    fn bounds(&self) -> (V2, V2) {
        let lo = v2(self.area.x as f32, self.area.y as f32 + MENU_BAR);
        let hi = v2(
            (self.area.right() as f32 - self.size.x).max(lo.x),
            (self.floor - self.size.y).max(lo.y),
        );
        (lo, hi)
    }

    /// Throw it: at `target` (its centre passes there), or off to `side`.
    pub fn throw(&mut self, target: Option<V2>, side: f32) {
        self.vel = match target {
            Some(t) => {
                let d = t - self.center();
                let time = (d.length() / 1400.0).clamp(0.3, 0.7);
                v2(d.x / time, d.y / time - 0.5 * GRAVITY * time)
            }
            None => v2(side.signum() * 1300.0, -650.0),
        };
        self.stage = Stage::Fly;
        self.t = 0.0;
    }

    /// The spell broke: it just drops.
    pub fn drop_now(&mut self) {
        if self.stage == Stage::Lift {
            self.vel = V2::ZERO;
            self.stage = Stage::Fly;
            self.t = 0.0;
        }
    }

    pub fn update(&mut self, dt: f32) {
        self.t += dt;
        let (lo, hi) = self.bounds();
        match self.stage {
            Stage::Lift => {
                let k = crate::stickman::smoothstep(self.t / 0.6);
                let wobble = v2(5.0 * (self.t * 19.0).sin(), 3.0 * (self.t * 13.0).cos());
                self.pos = (self.origin + v2(0.0, -LIFT_RISE * k) + wobble * k).clamp(lo, hi);
                if self.t > LIFT_TIME * 3.0 {
                    self.drop_now(); // never hang in the air
                }
            }
            Stage::Fly => {
                self.vel.y += GRAVITY * dt;
                self.pos += self.vel * dt;
                if self.pos.x < lo.x || self.pos.x > hi.x {
                    self.pos.x = self.pos.x.clamp(lo.x, hi.x);
                    self.vel.x = -self.vel.x * 0.45;
                }
                if self.pos.y < lo.y {
                    self.pos.y = lo.y;
                    self.vel.y = self.vel.y.abs() * 0.3;
                }
                if self.pos.y >= hi.y {
                    self.pos.y = hi.y;
                    if self.vel.y > 120.0 {
                        self.vel.y = -self.vel.y * 0.3;
                        self.vel.x *= 0.7;
                    } else {
                        // Sliding along the floor.
                        self.vel.y = 0.0;
                        self.vel.x *= (-6.0 * dt).exp();
                    }
                }
                let resting = self.pos.y >= hi.y - 0.5 && self.vel.length() < 25.0;
                if resting || self.t > MAX_FLIGHT {
                    self.stage = Stage::Done;
                }
            }
            Stage::Done => {}
        }
    }

    /// The window's rectangle right now.
    pub fn rect(&self) -> Rect {
        Rect {
            x: f64::from(self.pos.x),
            y: f64::from(self.pos.y),
            width: f64::from(self.size.x),
            height: f64::from(self.size.y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1800.0,
        height: 1169.0,
    };

    fn flight() -> WindowFlight {
        let win = Rect {
            x: 300.0,
            y: 200.0,
            width: 700.0,
            height: 500.0,
        };
        WindowFlight::new(1, 2, win, SCREEN, 1089.0)
    }

    fn run(f: &mut WindowFlight, secs: f32) -> (f32, f32) {
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        for _ in 0..(secs * 60.0) as usize {
            f.update(1.0 / 60.0);
            min_x = min_x.min(f.pos.x);
            max_x = max_x.max(f.pos.x + f.size.x);
            let fits = f.size.y <= 1089.0 - 30.0;
            assert!(f.pos.y >= 30.0 - 1e-3, "{:?}", f.pos);
            assert!(!fits || f.pos.y + f.size.y <= 1089.0 + 1e-3, "{:?}", f.pos);
        }
        (min_x, max_x)
    }

    #[test]
    fn lifts_then_flies_and_stays_on_screen() {
        let mut f = flight();
        run(&mut f, 0.7);
        assert!(f.pos.y < 200.0 - 30.0, "floated up: {:?}", f.pos);
        f.throw(None, 1.0);
        let (min_x, max_x) = run(&mut f, 4.0);
        assert!(min_x >= 0.0 && max_x <= 1800.0 + 1e-3);
        assert_eq!(f.stage, Stage::Done);
        assert!(
            (f.pos.y + f.size.y - 1089.0).abs() < 1.0,
            "rests on the Dock line"
        );
        assert!(f.pos.x > 300.0, "went right");
    }

    #[test]
    fn aimed_throws_pass_the_target() {
        let mut f = flight();
        run(&mut f, 0.7);
        let target = v2(1500.0, 300.0);
        f.throw(Some(target), 1.0);
        let mut closest = f32::MAX;
        for _ in 0..60 {
            f.update(1.0 / 60.0);
            let r = f.rect();
            if r.contains(target) {
                closest = 0.0;
            }
            closest = closest.min((f.center() - target).length());
        }
        assert!(closest < f.size.x / 2.0, "missed the cursor by {closest}");
    }

    #[test]
    fn a_huge_window_is_kept_under_the_menu_bar() {
        let big = Rect {
            x: 0.0,
            y: 40.0,
            width: 1800.0,
            height: 1100.0,
        };
        let mut f = WindowFlight::new(1, 2, big, SCREEN, 1089.0);
        run(&mut f, 0.5);
        f.throw(None, -1.0);
        run(&mut f, 4.0);
        assert_eq!(f.pos.y, 30.0);
        assert_eq!(f.pos.x, 0.0);
    }
}
