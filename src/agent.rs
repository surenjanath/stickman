//! The figure's brain and body: reflexes, mode-driven behaviour and a fixed-step
//! physics integrator over the rapier platform world.
//!
//! Layers, evaluated every fixed tick:
//! 1. **Reflexes:** a click knocks it flying; a fast incoming cursor makes it dodge
//!    (or counter-attack in combat).
//! 2. **Behaviour:** the macro [`Mode`] chosen by the AI director: wander, watch the
//!    user, attack the cursor, or climb the active window and stomp on it.
//! 3. **Physics:** walking keeps feet planted; in the air, gravity, one-way landings
//!    on window and text platforms, bounces while tumbling, screen-edge walls.
//!
//! Coordinates are overlay points, y down. `pos` is the figure's ground contact
//! point: hip x, feet y.

use std::f32::consts::{FRAC_PI_2, PI};

use crate::geom::{v2, Rect, V2};
use crate::physics::{Platform, PlatformKind, World};
use crate::poses;
use crate::stickman::{
    depth_below_root, forward_kinematics, gait_speed_for, idle_pose, lerp_angle, lerp_pose,
    mirror_pose, smoothstep, walk_pose, Pose, Segment, BONES, FOREARM, GAIT_PERIOD, SHIN,
};

/// Physics tick (claude.md rule 3: fixed 60 Hz step).
pub const FIXED_DT: f32 = 1.0 / 60.0;
/// Cartoon gravity, points/s².
pub const GRAVITY: f32 = 1900.0;
/// Highest platform a jump can reach, points above the feet.
pub const MAX_JUMP_RISE: f32 = 330.0;
const RUN_PERIOD: f32 = 0.62;
const BLEND_SECONDS: f32 = 0.18;

/// Macro behaviour, normally chosen by the AI director.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Idle,
    Observe,
    Sabotage,
    Combat,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Idle => "IDLE",
            Mode::Observe => "OBSERVE",
            Mode::Sabotage => "SABOTAGE",
            Mode::Combat => "COMBAT",
        }
    }

    /// Parses director output, tolerating case and the PRD's long names.
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_uppercase().as_str() {
            "IDLE" => Some(Mode::Idle),
            "OBSERVE" => Some(Mode::Observe),
            "SABOTAGE" | "SABOTAGE_WORK" => Some(Mode::Sabotage),
            "COMBAT" | "FRENZY_ATTACK" | "ATTACK" => Some(Mode::Combat),
            _ => None,
        }
    }
}

/// What the figure perceives this tick.
#[derive(Debug, Clone, Copy)]
pub struct Senses {
    pub cursor: V2,
    /// Smoothed cursor velocity, points/s.
    pub cursor_vel: V2,
    /// The user clicked on the figure since the last tick.
    pub clicked: bool,
    pub mode: Mode,
    /// Frontmost window: (CGWindowID, frame in overlay points).
    pub active_window: Option<(u32, Rect)>,
    /// Horizontal walking range.
    pub min_x: f32,
    pub max_x: f32,
}

/// Things the rest of the app should react to (particles, cursor push, window shake).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// The user clicked the figure: explosion at the cursor.
    Knockback {
        at: V2,
        dir: V2,
    },
    /// The figure punched or kicked the cursor: push it along `dir`.
    StruckCursor {
        at: V2,
        dir: V2,
    },
    /// Hard landing or bounce.
    Dust {
        at: V2,
        strength: f32,
    },
    /// A stomp on a window: shake it.
    Stomp {
        at: V2,
        window: u32,
    },
    Jumped {
        at: V2,
    },
    KnockedOut {
        at: V2,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Stand,
    Walk {
        to: f32,
        run: bool,
    },
    Think,
    Taunt,
    /// Crouching before a jump; launches with `vel`.
    Crouch {
        vel: V2,
    },
    Air,
    Fall,
    Land,
    Punch {
        aim: f32,
    },
    Kick {
        aim: f32,
    },
    Dodge,
    Tumble,
    Ko,
    GetUp,
    Stomp,
}

impl Action {
    fn duration(self) -> Option<f32> {
        Some(match self {
            Action::Crouch { .. } => 0.14,
            Action::Land => 0.2,
            Action::Punch { .. } => 0.42,
            Action::Kick { .. } => 0.55,
            Action::GetUp => 0.7,
            Action::Ko => 2.5,
            Action::Stomp => 0.6,
            Action::Taunt => 1.6,
            Action::Think => 3.0,
            _ => return None,
        })
    }

    /// Can behaviour replace this action right now?
    fn interruptible(self) -> bool {
        matches!(
            self,
            Action::Stand | Action::Walk { .. } | Action::Think | Action::Taunt
        )
    }

    /// Standing, thinking or knocked out: nothing that needs full-rate ticks.
    pub fn is_still(self) -> bool {
        matches!(self, Action::Stand | Action::Think | Action::Ko)
    }

    fn airborne(self) -> bool {
        matches!(
            self,
            Action::Air | Action::Fall | Action::Dodge | Action::Tumble
        )
    }
}

pub struct Agent {
    pub pos: V2,
    pub vel: V2,
    pub facing: f32,
    pub scale: f32,
    pub health: f32,
    pub frustration: f32,
    pub action: Action,
    /// Diagnostics: stand still (no behaviour), e.g. to measure idle CPU.
    pub hold_still: bool,
    /// Platform currently stood on.
    pub ground: Option<Platform>,
    t: f32,
    clock: f32,
    gait_phase: f32,
    spin: f32,
    spin_rate: f32,
    spin_from: f32,
    blend_from: (Pose, f32),
    blend_left: f32,
    hit_landed: bool,
    attack_cooldown: f32,
    decide_left: f32,
    since_hit: f32,
    rng: u64,
    /// Visual offset that smooths the switch between hip-anchored (airborne) and
    /// foot-anchored (grounded) placement; decays to zero in ~80 ms.
    root_correction: f32,
    hip_anchored: bool,
    /// Pose as last drawn; new actions blend from here.
    shown: (Pose, f32),
}

impl Agent {
    /// A figure standing at `x` on `ground`.
    pub fn new(x: f32, ground: Platform, scale: f32, seed: u64) -> Agent {
        Agent {
            pos: v2(x, ground.y),
            vel: V2::ZERO,
            facing: 1.0,
            scale,
            health: 100.0,
            frustration: 0.0,
            action: Action::Stand,
            hold_still: false,
            ground: Some(ground),
            t: 0.0,
            clock: 0.0,
            gait_phase: 0.0,
            spin: 0.0,
            spin_rate: 0.0,
            spin_from: 0.0,
            blend_from: (idle_pose(0.0), 0.0),
            blend_left: 0.0,
            hit_landed: false,
            attack_cooldown: 0.0,
            decide_left: 0.5,
            since_hit: 99.0,
            rng: seed | 1,
            root_correction: 0.0,
            hip_anchored: false,
            shown: (idle_pose(0.0), 0.0),
        }
    }

    fn random(&mut self, lo: f32, hi: f32) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        lo + (hi - lo) * (r as f32 / (1u64 << 24) as f32)
    }

    /// Seconds since the figure was last hit (for the health bar).
    pub fn since_hit(&self) -> f32 {
        self.since_hit
    }

    fn walk_speed(&self, run: bool) -> f32 {
        gait_speed_for(if run { RUN_PERIOD } else { GAIT_PERIOD }) * self.scale
    }

    fn set_action(&mut self, a: Action) {
        self.blend_from = self.shown;
        self.blend_left = BLEND_SECONDS;
        if matches!(a, Action::Punch { .. } | Action::Kick { .. }) {
            self.hit_landed = false;
        }
        if a == Action::GetUp {
            self.spin_from = self.spin;
        }
        if !matches!(a, Action::Walk { .. }) {
            self.gait_phase = 0.0;
        }
        self.action = a;
        self.t = 0.0;
    }

    fn face(&mut self, dir: f32) {
        if dir != 0.0 && dir.signum() != self.facing {
            // Re-express what's on screen in the new facing frame, so this blend and
            // any action started this tick continue from the drawn pose.
            let (p, s) = self.shown;
            self.shown = (mirror_pose(&p), -s);
            self.blend_from = self.shown;
            self.blend_left = BLEND_SECONDS;
            self.facing = dir.signum();
        }
    }

    /// Body centre (roughly the belly), for click directions and dodging.
    pub fn center(&self) -> V2 {
        self.pos - v2(0.0, 90.0 * self.scale * 1.0)
    }

    fn shoulder(&self) -> V2 {
        self.pos - v2(0.0, 200.0 * self.scale)
    }

    /// Aim angle from the shoulder to `target`, in the local (facing-right) frame.
    fn aim_at(&self, target: V2, lo: f32, hi: f32) -> f32 {
        let d = target - self.shoulder();
        d.y.atan2(d.x * self.facing).clamp(lo, hi)
    }

    fn reach(&self) -> f32 {
        (53.0 + 48.0) * self.scale
    }

    // --- Pose -------------------------------------------------------------------

    fn target_pose(&self) -> (Pose, f32) {
        let t = self.t;
        let k = |a: Action| a.duration().map_or(0.0, |d| (t / d).min(1.0));
        let p = match self.action {
            Action::Stand => idle_pose(self.clock),
            Action::Walk { run: false, .. } => walk_pose(self.gait_phase),
            Action::Walk { run: true, .. } => poses::run_pose(self.gait_phase),
            Action::Think => poses::think(self.clock),
            Action::Taunt => poses::taunt(t),
            Action::Crouch { .. } => poses::crouch(smoothstep(t / 0.14)),
            Action::Air => poses::air(self.vel.y),
            Action::Fall => poses::flail(t),
            Action::Land => poses::land(k(self.action)),
            Action::Punch { aim } => poses::punch(k(self.action), aim),
            Action::Kick { aim } => poses::kick(k(self.action), aim),
            Action::Dodge => poses::dodge(),
            Action::Tumble => poses::tumble(),
            Action::Ko => idle_pose(0.0),
            Action::GetUp => poses::crouch(1.0 - smoothstep((t - 0.3) / 0.4)),
            Action::Stomp => poses::stomp(k(self.action)),
        };
        let spin = match self.action {
            Action::Tumble => self.spin,
            Action::Ko => self.spin,
            Action::GetUp => self.spin_from * (1.0 - smoothstep(t / 0.45)),
            _ => 0.0,
        };
        // Fighting stance while combat-idle.
        (p, spin)
    }

    fn pose_and_spin(&self) -> (Pose, f32) {
        let (p, s) = self.target_pose();
        if self.blend_left > 0.0 {
            let k = smoothstep(1.0 - self.blend_left / BLEND_SECONDS);
            (
                lerp_pose(&self.blend_from.0, &p, k),
                lerp_angle(self.blend_from.1, s, k),
            )
        } else {
            (p, s)
        }
    }

    fn uses_hip_anchor(&self) -> bool {
        self.action.airborne() && self.action != Action::Tumble
    }

    /// Hip height for the given pose under either anchoring rule.
    fn root_y(&self, rotated: &Pose, hip_anchor: bool) -> f32 {
        if hip_anchor {
            // Airborne: hip at standing height above `pos`, so tucking the legs
            // doesn't make the body drop.
            self.pos.y - (poses::STAND_HIP + 5.0) * self.scale
        } else {
            self.pos.y - depth_below_root(rotated, self.scale)
        }
    }

    fn rotated_pose(&self) -> Pose {
        let (pose, spin) = self.pose_and_spin();
        std::array::from_fn(|i| pose[i] + spin)
    }

    /// The figure's bones in overlay points.
    pub fn segments(&self) -> [Segment; BONES] {
        let rotated = self.rotated_pose();
        let root_y = self.root_y(&rotated, self.uses_hip_anchor()) + self.root_correction;
        forward_kinematics(&rotated, [self.pos.x, root_y], self.scale, self.facing)
    }

    /// Bounding box of the figure, padded by `pad` points.
    pub fn bounds(&self, pad: f32) -> Rect {
        let s = self.segments();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for g in &s {
            for p in [g.start, g.end] {
                x0 = x0.min(p[0]);
                y0 = y0.min(p[1]);
                x1 = x1.max(p[0]);
                y1 = y1.max(p[1]);
            }
        }
        // The head is a ring around its segment.
        let r = 22.0 * self.scale;
        Rect {
            x: f64::from(x0 - pad - r),
            y: f64::from(y0 - pad - r),
            width: f64::from(x1 - x0 + 2.0 * (pad + r)),
            height: f64::from(y1 - y0 + 2.0 * (pad + r)),
        }
    }

    /// Centre of the head ring.
    pub fn head(&self) -> V2 {
        let h = self.segments()[crate::stickman::HEAD];
        v2((h.start[0] + h.end[0]) / 2.0, (h.start[1] + h.end[1]) / 2.0)
    }

    pub fn hit_test(&self, p: V2) -> bool {
        self.bounds(10.0).contains(p)
    }

    // --- World changes ---------------------------------------------------------

    /// Platforms moved (windows dragged, closed, text re-read). Ride along with
    /// the platform we stand on, or fall if it vanished from under us.
    pub fn on_platforms_changed(&mut self, world: &World) {
        let Some(old) = self.ground else { return };
        let same = world.platforms().iter().copied().find(|p| {
            p.kind == old.kind
                && p.owner == old.owner
                && p.contains_x(self.pos.x)
                && (p.y - old.y).abs() < 400.0
        });
        match same {
            Some(p) => {
                self.pos.y = p.y;
                self.ground = Some(p);
            }
            None => {
                self.ground = None;
                self.vel = V2::ZERO;
                self.set_action(Action::Fall);
            }
        }
    }

    // --- Tick -------------------------------------------------------------------

    pub fn update(&mut self, s: &Senses, world: &World, events: &mut Vec<Event>) {
        let dt = FIXED_DT;
        self.clock += dt;
        self.t += dt;
        self.blend_left = (self.blend_left - dt).max(0.0);
        self.attack_cooldown -= dt;
        self.decide_left -= dt;
        self.since_hit += dt;
        if self.action != Action::Ko && self.since_hit > 3.0 {
            self.health = (self.health + 4.0 * dt).min(100.0);
        }
        self.frustration = (self.frustration - 1.5 * dt).max(0.0);

        let before_root =
            self.root_y(&self.rotated_pose(), self.hip_anchored) + self.root_correction;
        let before_pos = self.pos.y;

        self.reflexes(s, events);
        self.finish_actions(events);
        self.attack_contacts(s, events);
        if self.ground.is_some() && self.action.interruptible() && !self.hold_still {
            self.behave(s, world, events);
        }
        self.integrate(s, world, events);

        // Keep the drawn body continuous when the anchoring rule flips.
        let anchor = self.uses_hip_anchor();
        if anchor != self.hip_anchored {
            let moved = self.pos.y - before_pos;
            let new_root = self.root_y(&self.rotated_pose(), anchor);
            self.root_correction = before_root + moved - new_root;
            self.hip_anchored = anchor;
        }
        self.shown = self.pose_and_spin();
        self.root_correction *= (-dt / 0.08).exp();
        if self.root_correction.abs() < 0.01 {
            self.root_correction = 0.0;
        }
    }

    fn reflexes(&mut self, s: &Senses, events: &mut Vec<Event>) {
        // Clicks land even mid-air (juggling), except while knocked out.
        if s.clicked && self.action != Action::Ko {
            let away = (self.center() - s.cursor).normalize_or(v2(-self.facing, 0.0));
            let mut knock = away * 620.0 + v2(0.0, -520.0) + s.cursor_vel * 0.25;
            knock = knock.clamp_length_max(1500.0);
            self.vel = knock;
            self.ground = None;
            self.spin_rate = 15.0 * knock.x.signum() * self.facing;
            if self.action != Action::Tumble {
                self.spin = 0.0;
            }
            self.health -= 20.0;
            self.frustration = (self.frustration + 22.0).min(100.0);
            self.since_hit = 0.0;
            if self.action != Action::Tumble {
                self.set_action(Action::Tumble);
            }
            events.push(Event::Knockback {
                at: s.cursor,
                dir: away,
            });
            return;
        }
        // A fast swipe coming at us.
        if self.ground.is_none() || !self.action.interruptible() {
            return;
        }
        let to_us = self.center() - s.cursor;
        let dist = to_us.length();
        let closing = s.cursor_vel.dot(to_us.normalize_or_zero());
        if dist < 240.0 && closing > 1100.0 {
            if s.mode == Mode::Combat && (s.cursor - self.shoulder()).length() < self.reach() * 1.4
            {
                self.attack(s.cursor);
            } else {
                let away = to_us.x.signum();
                self.face(-away);
                self.vel = v2(away * 340.0, -440.0);
                self.ground = None;
                self.frustration = (self.frustration + 4.0).min(100.0);
                self.set_action(Action::Dodge);
            }
        }
    }

    fn finish_actions(&mut self, events: &mut Vec<Event>) {
        let Some(d) = self.action.duration() else {
            return;
        };
        if self.t < d {
            return;
        }
        match self.action {
            Action::Crouch { vel } => {
                self.vel = vel;
                self.ground = None;
                self.set_action(Action::Air);
                events.push(Event::Jumped { at: self.pos });
            }
            Action::Ko => self.set_action(Action::GetUp),
            Action::GetUp => {
                self.spin = 0.0;
                self.health = self.health.max(40.0);
                self.set_action(Action::Stand);
            }
            Action::Think | Action::Taunt => self.t = 0.0, // loop until behaviour changes
            _ => self.set_action(Action::Stand),
        }
    }

    fn attack(&mut self, target: V2) {
        self.face((target.x - self.pos.x).signum());
        let hip_y = self.pos.y - poses::STAND_HIP * self.scale;
        let low = target.y > hip_y - 10.0;
        let a = if low || self.random(0.0, 1.0) < 0.3 {
            Action::Kick {
                aim: self.aim_at(target, -0.4, 1.0),
            }
        } else {
            Action::Punch {
                aim: self.aim_at(target, -1.3, 0.9),
            }
        };
        self.attack_cooldown = 0.35;
        self.set_action(a);
    }

    fn attack_contacts(&mut self, s: &Senses, events: &mut Vec<Event>) {
        let (window, bone, radius, aim) = match self.action {
            Action::Punch { aim } => ((0.16, 0.28), FOREARM[0], 30.0, aim),
            Action::Kick { aim } => ((0.22, 0.34), SHIN[0], 34.0, aim),
            Action::Stomp => {
                if self.t >= 0.3 && self.t - FIXED_DT < 0.3 {
                    let window = self.ground.map_or(0, |g| g.owner);
                    events.push(Event::Stomp {
                        at: self.pos,
                        window,
                    });
                    events.push(Event::Dust {
                        at: self.pos,
                        strength: 0.6,
                    });
                    self.frustration = (self.frustration - 6.0).max(0.0);
                }
                return;
            }
            _ => return,
        };
        if self.hit_landed || self.t < window.0 || self.t > window.1 {
            return;
        }
        let tip = self.segments()[bone].end;
        if (v2(tip[0], tip[1]) - s.cursor).length() < radius {
            self.hit_landed = true;
            let dir = v2(aim.cos() * self.facing, aim.sin()).normalize_or_zero();
            events.push(Event::StruckCursor { at: s.cursor, dir });
            self.frustration = (self.frustration - 15.0).max(0.0);
        }
    }

    fn walk_to(&mut self, x: f32, run: bool) {
        self.face((x - self.pos.x).signum());
        let keep_phase = self.gait_phase;
        if !matches!(self.action, Action::Walk { run: r, .. } if r == run) {
            self.set_action(Action::Walk { to: x, run });
            self.gait_phase = keep_phase;
        } else {
            self.action = Action::Walk { to: x, run };
        }
    }

    /// Ballistic launch that lands on `target` (feet), clearing it by 40 pt.
    fn jump_to(&mut self, target: V2) {
        let rise = (self.pos.y - target.y + 40.0).max(30.0);
        let vy = -(2.0 * GRAVITY * rise).sqrt();
        let t_up = -vy / GRAVITY;
        let t_down = (2.0 * (rise - (self.pos.y - target.y)).max(1.0) / GRAVITY).sqrt();
        let vx = (target.x - self.pos.x) / (t_up + t_down);
        self.face(vx.signum());
        self.set_action(Action::Crouch { vel: v2(vx, vy) });
    }

    fn behave(&mut self, s: &Senses, world: &World, _events: &mut Vec<Event>) {
        let dx = s.cursor.x - self.pos.x;
        match s.mode {
            Mode::Combat => {
                if (s.cursor - self.shoulder()).length() < self.reach() + 25.0 {
                    if self.attack_cooldown <= 0.0 {
                        self.attack(s.cursor);
                    }
                } else if s.cursor.y < self.shoulder().y - 140.0
                    && dx.abs() < 260.0
                    && self.attack_cooldown <= 0.0
                {
                    // Cursor overhead: leap at it.
                    let rise = (self.pos.y - s.cursor.y).min(MAX_JUMP_RISE);
                    self.attack_cooldown = 1.2;
                    self.jump_to(v2(s.cursor.x, self.pos.y - rise + 40.0));
                } else if dx.abs() > self.reach() * 0.8 {
                    let stop = s.cursor.x - dx.signum() * self.reach() * 0.7;
                    self.walk_to(stop, dx.abs() > 160.0);
                } else if !matches!(self.action, Action::Taunt) && self.decide_left <= 0.0 {
                    self.face(dx.signum());
                    self.decide_left = 2.0;
                    self.set_action(Action::Taunt);
                }
            }
            Mode::Observe => {
                if dx.abs() > 450.0 {
                    self.walk_to(s.cursor.x - dx.signum() * 320.0, false);
                } else if !matches!(self.action, Action::Think | Action::Walk { .. }) {
                    self.face(dx.signum());
                    self.set_action(Action::Think);
                } else if matches!(self.action, Action::Think) {
                    self.face(dx.signum());
                }
            }
            Mode::Sabotage => self.sabotage(s, world),
            Mode::Idle => self.wander(s, world),
        }
        // Arrived?
        if let Action::Walk { to, .. } = self.action {
            if (self.facing > 0.0 && self.pos.x >= to) || (self.facing < 0.0 && self.pos.x <= to) {
                self.set_action(Action::Stand);
            }
        }
    }

    fn sabotage(&mut self, s: &Senses, world: &World) {
        let Some((id, win)) = s.active_window else {
            return self.wander(s, world);
        };
        let ground = self.ground.unwrap_or(Platform::ground(0.0, self.pos.y));
        if ground.kind == PlatformKind::Window && ground.owner == id {
            if self.decide_left <= 0.0 {
                self.decide_left = self.random(0.7, 1.6);
                if self.random(0.0, 1.0) < 0.3 {
                    let x = self.random(ground.x0 + 20.0, ground.x1 - 20.0);
                    self.walk_to(x, false);
                } else {
                    self.set_action(Action::Stomp);
                }
            }
            return;
        }
        // Climb onto the window if its top edge is reachable and visible.
        let top = win.y as f32;
        let target = world
            .platforms()
            .iter()
            .filter(|p| p.owner == id && p.kind == PlatformKind::Window)
            .min_by(|a, b| {
                let da = (a.x0.max(self.pos.x.min(a.x1)) - self.pos.x).abs();
                let db = (b.x0.max(self.pos.x.min(b.x1)) - self.pos.x).abs();
                da.total_cmp(&db)
            })
            .copied();
        match target {
            Some(p) if self.pos.y - top <= MAX_JUMP_RISE && top < self.pos.y - 20.0 => {
                let launch = self.pos.x.clamp(p.x0 + 40.0, p.x1 - 40.0);
                let launch = launch.clamp(ground.x0 + 10.0, ground.x1 - 10.0);
                if (launch - self.pos.x).abs() > 25.0 {
                    self.walk_to(launch, false);
                } else {
                    let land_x = self.pos.x.clamp(p.x0 + 30.0, p.x1 - 30.0) + self.facing * 10.0;
                    self.jump_to(v2(land_x.clamp(p.x0 + 20.0, p.x1 - 20.0), p.y));
                }
            }
            _ => {
                // Out of reach: stand under it and shake a fist.
                let under = (win.x as f32 + win.width as f32 / 2.0).clamp(s.min_x, s.max_x);
                if (under - self.pos.x).abs() > 60.0 {
                    self.walk_to(under, false);
                } else if !matches!(self.action, Action::Taunt) {
                    self.set_action(Action::Taunt);
                }
            }
        }
    }

    fn wander(&mut self, s: &Senses, world: &World) {
        if self.decide_left > 0.0 || matches!(self.action, Action::Walk { .. }) {
            return;
        }
        self.decide_left = self.random(1.5, 4.5);
        let ground = self.ground.unwrap_or(Platform::ground(s.max_x, self.pos.y));
        let (lo, hi) = (ground.x0.max(s.min_x) + 20.0, ground.x1.min(s.max_x) - 20.0);
        if self.clock < 1.0 && hi > lo + 400.0 {
            // Coming alive: always set off on a stroll first.
            let x = if self.pos.x - lo > hi - self.pos.x {
                lo + 50.0
            } else {
                hi - 50.0
            };
            return self.walk_to(x, false);
        }
        let roll = self.random(0.0, 1.0);
        if roll < 0.15 {
            // Explore: hop up onto something nearby.
            if let Some(i) = world.platform_above(
                self.pos,
                MAX_JUMP_RISE,
                self.pos.x - 220.0,
                self.pos.x + 220.0,
            ) {
                let p = world.platforms()[i];
                let x = self.pos.x.clamp(p.x0 + 30.0, p.x1 - 30.0);
                if (x - self.pos.x).abs() < 200.0 {
                    return self.jump_to(v2(x, p.y));
                }
            }
        }
        if roll < 0.25 && ground.kind != PlatformKind::Ground {
            // Wander off the edge of a window and drop down.
            let edge = if self.random(0.0, 1.0) < 0.5 {
                ground.x0 - 40.0
            } else {
                ground.x1 + 40.0
            };
            return self.walk_to(edge.clamp(s.min_x, s.max_x), false);
        }
        if roll < 0.75 && hi > lo + 40.0 {
            let x = self.random(lo, hi);
            self.walk_to(x, false);
        } else if !matches!(self.action, Action::Stand) {
            self.set_action(Action::Stand);
        }
    }

    fn integrate(&mut self, s: &Senses, world: &World, events: &mut Vec<Event>) {
        let dt = FIXED_DT;
        if let Some(ground) = self.ground {
            if let Action::Walk { run, .. } = self.action {
                // Ease into the stride so feet don't skate while the blend runs.
                let ramp = if self.blend_left > 0.0 {
                    smoothstep(1.0 - self.blend_left / BLEND_SECONDS)
                } else {
                    1.0
                };
                let period = if run { RUN_PERIOD } else { GAIT_PERIOD };
                self.gait_phase = (self.gait_phase + dt / period * ramp).rem_euclid(1.0);
                self.pos.x += self.facing * self.walk_speed(run) * ramp * dt;
                if self.pos.x <= s.min_x || self.pos.x >= s.max_x {
                    self.pos.x = self.pos.x.clamp(s.min_x, s.max_x);
                    self.set_action(Action::Stand);
                }
            }
            self.pos.y = ground.y;
            if world.support(self.pos).is_none() {
                // Walked off the edge.
                let vx = match self.action {
                    Action::Walk { run, .. } => self.facing * self.walk_speed(run),
                    _ => 0.0,
                };
                self.vel = v2(vx, 0.0);
                self.ground = None;
                self.set_action(Action::Fall);
            }
            return;
        }

        // Airborne.
        self.vel.y += GRAVITY * dt;
        let mut next = self.pos + self.vel * dt;
        let ceiling = 40.0 + 230.0 * self.scale;
        if next.y < ceiling {
            next.y = ceiling;
            self.vel.y = self.vel.y.abs() * 0.3;
        }
        if next.x < s.min_x || next.x > s.max_x {
            next.x = next.x.clamp(s.min_x, s.max_x);
            self.vel.x = -self.vel.x * 0.5;
            if self.action == Action::Tumble {
                self.spin_rate = -self.spin_rate * 0.7;
            }
        }
        if self.action == Action::Tumble {
            self.spin += self.spin_rate * dt;
        }
        if let Some(i) = world.landing(self.pos, next) {
            let p = world.platforms()[i];
            next.y = p.y;
            let impact = self.vel.y;
            if self.action == Action::Tumble && impact > 380.0 {
                // Bounce.
                self.vel.y = -impact * 0.38;
                self.vel.x *= 0.65;
                self.spin_rate *= 0.6;
                events.push(Event::Dust {
                    at: next,
                    strength: (impact / 1200.0).min(1.0),
                });
                self.pos = next - v2(0.0, 0.5);
                return;
            }
            self.pos = next;
            self.vel = V2::ZERO;
            self.ground = Some(p);
            if impact > 700.0 {
                events.push(Event::Dust {
                    at: next,
                    strength: (impact / 1500.0).min(1.0),
                });
            }
            if self.action == Action::Tumble {
                if self.health <= 0.0 {
                    // Lie flat, then get back up.
                    self.spin = if self.spin.rem_euclid(2.0 * PI) < PI {
                        FRAC_PI_2
                    } else {
                        -FRAC_PI_2
                    };
                    events.push(Event::KnockedOut { at: next });
                    self.set_action(Action::Ko);
                } else {
                    self.spin = (self.spin + PI).rem_euclid(2.0 * PI) - PI;
                    self.set_action(Action::GetUp);
                }
            } else {
                self.set_action(Action::Land);
            }
            return;
        }
        self.pos = next;
        // Safety net: never lose the figure below the screen.
        if let Some(g) = world
            .platforms()
            .iter()
            .find(|p| p.kind == PlatformKind::Ground)
        {
            if self.pos.y > g.y + 80.0 {
                self.pos.y = g.y;
                self.vel = V2::ZERO;
                self.ground = Some(*g);
                self.set_action(Action::Land);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: f32 = 0.55;
    const GROUND_Y: f32 = 1100.0;

    fn world(extra: Vec<Platform>) -> World {
        let mut w = World::new();
        let mut ps = vec![Platform::ground(1800.0, GROUND_Y)];
        ps.extend(extra);
        w.set_platforms(ps);
        w
    }

    fn senses(mode: Mode) -> Senses {
        Senses {
            cursor: v2(1500.0, 300.0),
            cursor_vel: V2::ZERO,
            clicked: false,
            mode,
            active_window: None,
            min_x: 60.0,
            max_x: 1740.0,
        }
    }

    fn agent() -> Agent {
        Agent::new(900.0, Platform::ground(1800.0, GROUND_Y), SCALE, 1234)
    }

    fn run(a: &mut Agent, w: &World, s: &Senses, secs: f32, ev: &mut Vec<Event>) {
        for _ in 0..(secs / FIXED_DT) as usize {
            a.update(s, w, ev);
        }
    }

    fn feet_y(a: &Agent) -> f32 {
        a.segments()
            .iter()
            .map(|g| g.start[1].max(g.end[1]) + g.thickness / 2.0)
            .fold(f32::MIN, f32::max)
    }

    #[test]
    fn mode_parsing_accepts_prd_names() {
        assert_eq!(Mode::parse("combat"), Some(Mode::Combat));
        assert_eq!(Mode::parse("FRENZY_ATTACK"), Some(Mode::Combat));
        assert_eq!(Mode::parse(" sabotage_work "), Some(Mode::Sabotage));
        assert_eq!(Mode::parse("observe"), Some(Mode::Observe));
        assert_eq!(Mode::parse("dance"), None);
        for m in [Mode::Idle, Mode::Observe, Mode::Sabotage, Mode::Combat] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn idle_wanders_on_the_ground_with_feet_down() {
        let w = world(vec![]);
        let mut a = agent();
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        let mut moved = 0.0f32;
        let start = a.pos.x;
        for _ in 0..(20.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            moved = moved.max((a.pos.x - start).abs());
            if a.ground.is_some() && matches!(a.action, Action::Walk { .. } | Action::Stand) {
                assert!(
                    (feet_y(&a) - GROUND_Y).abs() < 0.5,
                    "feet at {}",
                    feet_y(&a)
                );
            }
            assert!((60.0..=1740.0).contains(&a.pos.x));
        }
        assert!(moved > 100.0, "never wandered ({moved} pt)");
    }

    #[test]
    fn planted_foot_does_not_slide_while_walking() {
        let w = world(vec![]);
        let mut a = agent();
        a.walk_to(1600.0, false);
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        run(&mut a, &w, &s, 0.5, &mut ev); // past the blend
        let mut worst = 0.0f32;
        let mut prev = a.segments();
        let mut prev_phase = a.gait_phase;
        for _ in 0..120 {
            a.update(&s, &w, &mut ev);
            let now = a.segments();
            for side in 0..2 {
                let q0 = (prev_phase + side as f32 * 0.5).rem_euclid(1.0);
                let q1 = (a.gait_phase + side as f32 * 0.5).rem_euclid(1.0);
                if q0 < crate::stickman::DUTY && q1 < crate::stickman::DUTY && q1 > q0 {
                    worst = worst.max((prev[SHIN[side]].end[0] - now[SHIN[side]].end[0]).abs());
                }
            }
            prev = now;
            prev_phase = a.gait_phase;
        }
        assert!(worst < 0.05, "planted foot slid {worst} pt per tick");
    }

    #[test]
    fn click_knocks_back_then_recovers() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        let mut ev = vec![];
        s.cursor = a.center() + v2(-20.0, 0.0); // click from the left
        s.clicked = true;
        a.update(&s, &w, &mut ev);
        s.clicked = false;
        assert!(matches!(ev[0], Event::Knockback { .. }));
        assert_eq!(a.action, Action::Tumble);
        assert!(
            a.vel.x > 0.0 && a.vel.y < 0.0,
            "should fly right and up: {:?}",
            a.vel
        );
        assert!(a.health < 100.0);
        let mut max_height = 0.0f32;
        for _ in 0..(4.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            max_height = max_height.max(GROUND_Y - a.pos.y);
        }
        assert!(max_height > 40.0, "barely left the ground: {max_height}");
        assert!(
            a.ground.is_some() && a.action.interruptible(),
            "still {:?}",
            a.action
        );
        assert!(a.pos.x > 920.0, "knocked away from the cursor");
        assert!(
            ev.iter().any(|e| matches!(e, Event::Dust { .. })),
            "no landing dust"
        );
    }

    #[test]
    fn repeated_hits_knock_out_then_get_up() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        let mut ev = vec![];
        let mut ko = false;
        for _ in 0..6 {
            s.cursor = a.center() + v2(15.0, 0.0);
            s.clicked = true;
            a.update(&s, &w, &mut ev);
            s.clicked = false;
            for _ in 0..(0.6 / FIXED_DT) as usize {
                a.update(&s, &w, &mut ev);
                ko |= a.action == Action::Ko;
            }
        }
        for _ in 0..(6.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            ko |= a.action == Action::Ko;
        }
        assert!(ko, "never knocked out (health {})", a.health);
        assert!(ev.iter().any(|e| matches!(e, Event::KnockedOut { .. })));
        assert!(
            a.action.interruptible() && a.health >= 40.0,
            "{:?} {}",
            a.action,
            a.health
        );
    }

    #[test]
    fn combat_chases_and_hits_the_cursor() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(1300.0, GROUND_Y - 105.0);
        let mut ev = vec![];
        run(&mut a, &w, &s, 8.0, &mut ev);
        let hit = ev.iter().find_map(|e| match e {
            Event::StruckCursor { dir, .. } => Some(*dir),
            _ => None,
        });
        let dir = hit.expect("never struck the cursor");
        assert!(dir.x > 0.5, "should strike toward the cursor: {dir:?}");
        assert!(
            (a.pos.x - 1300.0).abs() < 120.0,
            "didn't close distance: {}",
            a.pos.x
        );
    }

    #[test]
    fn fast_swipe_triggers_a_dodge_outside_combat() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        let mut ev = vec![];
        run(&mut a, &w, &s, 0.2, &mut ev);
        s.cursor = a.center() + v2(-150.0, 0.0);
        s.cursor_vel = v2(2500.0, 0.0);
        a.update(&s, &w, &mut ev);
        assert_eq!(a.action, Action::Dodge);
        assert!(
            a.vel.x > 0.0 && a.vel.y < 0.0,
            "should hop away: {:?}",
            a.vel
        );
        assert_eq!(a.facing, -1.0, "keeps facing the threat");
        s.cursor_vel = V2::ZERO;
        run(&mut a, &w, &s, 1.5, &mut ev);
        assert!(a.ground.is_some());
    }

    #[test]
    fn sabotage_climbs_the_active_window_and_stomps() {
        let win = Rect {
            x: 700.0,
            y: 850.0,
            width: 600.0,
            height: 400.0,
        };
        let top = Platform {
            x0: 700.0,
            x1: 1300.0,
            y: 850.0,
            kind: PlatformKind::Window,
            owner: 42,
        };
        let w = world(vec![top]);
        let mut a = agent();
        a.pos.x = 400.0;
        let mut s = senses(Mode::Sabotage);
        s.active_window = Some((42, win));
        let mut ev = vec![];
        run(&mut a, &w, &s, 12.0, &mut ev);
        assert_eq!(
            a.ground.map(|g| g.owner),
            Some(42),
            "not on the window: {:?} at {:?}",
            a.action,
            a.pos
        );
        assert!(ev.iter().any(|e| matches!(e, Event::Jumped { .. })));
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::Stomp { window: 42, .. })),
            "no stomps"
        );
    }

    #[test]
    fn walking_off_a_window_falls_to_the_ground() {
        let top = Platform {
            x0: 700.0,
            x1: 1000.0,
            y: 800.0,
            kind: PlatformKind::Window,
            owner: 5,
        };
        let w = world(vec![top]);
        let mut a = Agent::new(950.0, top, SCALE, 3);
        a.walk_to(1200.0, false);
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        let (mut fell, mut reached_ground) = (false, false);
        for _ in 0..(4.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            fell |= a.action == Action::Fall;
            reached_ground |=
                a.ground.map(|g| g.kind) == Some(PlatformKind::Ground) && a.pos.y == GROUND_Y;
        }
        assert!(fell, "never fell off the edge");
        assert!(reached_ground, "never landed on the ground");
    }

    #[test]
    fn rides_a_moving_window_and_falls_when_it_closes() {
        let top = Platform {
            x0: 700.0,
            x1: 1000.0,
            y: 800.0,
            kind: PlatformKind::Window,
            owner: 5,
        };
        let mut w = world(vec![top]);
        let mut a = Agent::new(850.0, top, SCALE, 3);
        w.set_platforms(vec![
            Platform::ground(1800.0, GROUND_Y),
            Platform { y: 650.0, ..top },
        ]);
        a.on_platforms_changed(&w);
        assert_eq!(a.pos.y, 650.0);
        w.set_platforms(vec![Platform::ground(1800.0, GROUND_Y)]);
        a.on_platforms_changed(&w);
        assert_eq!(a.action, Action::Fall);
        let mut ev = vec![];
        run(&mut a, &w, &senses(Mode::Idle), 2.0, &mut ev);
        assert_eq!(a.pos.y, GROUND_Y);
    }

    #[test]
    fn observe_follows_at_a_distance_and_thinks() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Observe);
        s.cursor = v2(1600.0, 500.0);
        let mut ev = vec![];
        run(&mut a, &w, &s, 10.0, &mut ev);
        let gap = (s.cursor.x - a.pos.x).abs();
        assert!((200.0..=460.0).contains(&gap), "gap {gap}");
        assert_eq!(a.action, Action::Think);
        assert_eq!(a.facing, 1.0);
    }

    #[test]
    fn hit_test_covers_the_body_only() {
        let a = agent();
        assert!(a.hit_test(a.center()));
        assert!(a.hit_test(a.pos - v2(0.0, 2.0)));
        assert!(!a.hit_test(a.center() + v2(200.0, 0.0)));
    }

    /// No joint may teleport between frames, except when a click launches it.
    #[test]
    fn motion_is_continuous_in_every_mode() {
        let top = Platform {
            x0: 600.0,
            x1: 1200.0,
            y: 850.0,
            kind: PlatformKind::Window,
            owner: 9,
        };
        let w = world(vec![top]);
        for mode in [Mode::Idle, Mode::Observe, Mode::Sabotage, Mode::Combat] {
            let mut a = agent();
            let mut s = senses(mode);
            s.active_window = Some((
                9,
                Rect {
                    x: 600.0,
                    y: 850.0,
                    width: 600.0,
                    height: 300.0,
                },
            ));
            let mut ev = vec![];
            let mut prev = a.segments();
            let mut prev_pos = a.pos;
            for i in 0..(30.0 / FIXED_DT) as usize {
                // Move the cursor around in a slow loop.
                let t = i as f32 * FIXED_DT;
                s.cursor = v2(
                    900.0 + 500.0 * (t * 0.4).sin(),
                    GROUND_Y - 120.0 - 200.0 * (t * 0.3).sin().abs(),
                );
                a.update(&s, &w, &mut ev);
                let now = a.segments();
                // Joint motion relative to the body: travelling fast through the air
                // is fine, limbs teleporting is not.
                let body = a.pos - prev_pos;
                for b in 0..BONES {
                    let d = ((now[b].end[0] - prev[b].end[0] - body.x).powi(2)
                        + (now[b].end[1] - prev[b].end[1] - body.y).powi(2))
                    .sqrt();
                    assert!(
                        d < 45.0,
                        "{mode:?}: bone {b} jumped {d} pt at t={t:.2} in {:?}",
                        a.action
                    );
                }
                prev_pos = a.pos;
                prev = now;
            }
        }
    }
}
