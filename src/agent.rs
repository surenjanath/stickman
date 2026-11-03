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

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::doodle::{self, Shape};
use crate::geom::{clamp_between, v2, Rect, V2};
use crate::physics::{Platform, PlatformKind, World};
use crate::poses;
use crate::stickman::{
    depth_below_root, forward_kinematics, gait_speed_for, idle_pose, mirror_pose, smoothstep,
    walk_pose, Pose, Segment, BONES, FOREARM, GAIT_PERIOD, SHIN,
};

/// Physics tick (claude.md rule 3: fixed 60 Hz step).
pub const FIXED_DT: f32 = 1.0 / 60.0;
/// Cartoon gravity, points/s².
pub const GRAVITY: f32 = 1900.0;
/// Highest platform a jump can reach, points above the feet.
pub const MAX_JUMP_RISE: f32 = 330.0;
/// A super jump (powers) reaches this high.
pub const SUPER_RISE: f32 = 1000.0;
const RUN_PERIOD: f32 = 0.62;
const BLEND_SECONDS: f32 = 0.18;
/// The user away this long (no mouse movement): time for a nap.
pub const SLEEP_AFTER: f32 = 75.0;
/// Stroke the figure this long (seconds) to make it happy.
const PET_TIME: f32 = 1.0;

/// How the figure's body is placed relative to `pos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// Lowest point on the ground (standing, walking, lying).
    Feet,
    /// Hip at standing height above `pos` (airborne).
    Hip,
    /// Hip resting on the surface (sitting on an edge).
    Seat,
}

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
    /// Horizontal walking range (can span several screens).
    pub min_x: f32,
    pub max_x: f32,
    /// Top of the screen the figure is on.
    pub top: f32,
    /// A word the figure could rip off the screen and throw.
    pub ammo: Option<Ammo>,
    /// May grab and throw the user's cursor (the app moves the real pointer).
    pub can_grab_cursor: bool,
    /// The user is holding the figure with the mouse (press and drag).
    pub held: bool,
    /// Seconds since the user last moved the mouse.
    pub user_idle: f32,
    /// May shove the active window around (Accessibility granted, allowed).
    pub can_shove: bool,
    /// Words telekinesis could lift right now (near the figure).
    pub tk_words_near: usize,
    /// Music is playing: dance to it.
    pub music: bool,
    /// May lift and throw the active window with telekinesis.
    pub can_throw_window: bool,
    /// May rip a weapon (a scrollbar) off the active window.
    pub weapon_available: bool,
}

/// What telekinesis is working on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mind {
    /// Lift the cursor, wobble it in the air, fling it.
    Cursor,
    /// Lift a group of words off the page and fire them at the cursor.
    Words,
    /// Lift the active window and throw it (at the cursor, in combat).
    Window,
}

/// A grabbable word on a text line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ammo {
    pub id: u64,
    /// Centre of the word.
    pub at: V2,
    /// Top of the text line it belongs to: stand here to grab it.
    pub line_y: f32,
}

/// Gravity for thrown words (lighter than the figure's, so lobs read well).
pub const PROJECTILE_GRAVITY: f32 = 1300.0;

/// Launch velocity for a lob from `from` that reaches `to` after a flight time
/// that grows with distance.
pub fn lob_velocity(from: V2, to: V2) -> V2 {
    let d = to - from;
    let t = (d.length() / 1100.0).clamp(0.28, 0.85);
    v2(d.x / t, d.y / t - 0.5 * PROJECTILE_GRAVITY * t)
}

/// Things the rest of the app should react to (particles, cursor push, window shake).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Ripped word `id` off the screen; the figure is now carrying it.
    Grabbed {
        id: u64,
    },
    /// Let go of the carried word with this velocity (aimed throw).
    Threw {
        from: V2,
        vel: V2,
    },
    /// Dropped the carried word (got hit).
    Dropped {
        at: V2,
    },
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
    /// Started a punch or kick (a whoosh).
    Swing {
        at: V2,
    },
    /// Hopped out of the way of a fast swipe.
    Dodged {
        at: V2,
    },
    KnockedOut {
        at: V2,
    },
    /// Caught the cursor: the app should pin the pointer to the hands.
    CaughtCursor {
        at: V2,
    },
    /// Threw the cursor from `from` with velocity `vel`.
    ThrewCursor {
        from: V2,
        vel: V2,
    },
    /// The user picked the figure up with the mouse.
    PickedUp {
        at: V2,
    },
    /// The user let go of the figure, flinging it with `vel`.
    Flung {
        at: V2,
        vel: V2,
    },
    /// Lying down for a nap.
    Yawned {
        at: V2,
    },
    Woke {
        at: V2,
    },
    /// The user stroked it with the cursor: hearts.
    Petted {
        at: V2,
    },
    StartedDancing {
        at: V2,
    },
    /// Started drawing `shape` centred at `origin`, `size` points across (half).
    DoodleStarted {
        shape: Shape,
        origin: V2,
        size: f32,
    },
    /// Kicked the window it stands on sideways by `dx` points.
    ShoveWindow {
        at: V2,
        window: u32,
        dx: f32,
    },
    /// Launched a super jump.
    SuperJumped {
        at: V2,
    },
    /// Telekinesis took hold of the cursor (the app levitates the pointer).
    MindLiftCursor {
        at: V2,
    },
    /// Telekinesis flings the cursor from `from` with `vel`.
    MindFlingCursor {
        from: V2,
        vel: V2,
    },
    /// Hit by another figure (a duel).
    Struck {
        at: V2,
        dir: V2,
    },
    /// Reached for a weapon: the app finds and captures one.
    WantWeapon {
        at: V2,
    },
    /// A sword cut hit the cursor.
    Slashed {
        at: V2,
        dir: V2,
    },
    /// Let go of the weapon (got hit): it falls from `at`.
    WeaponDropped {
        at: V2,
    },
    /// Hurled the worn-out weapon from `from` with `vel`.
    WeaponThrown {
        from: V2,
        vel: V2,
    },
    /// Bounced off a trampoline.
    Boing {
        at: V2,
    },
    /// Telekinesis lifts words near `at` (the app picks and captures them).
    MindLiftWords {
        at: V2,
    },
    /// Fire the lifted words at `target`.
    MindFireWords {
        target: V2,
    },
    /// Telekinesis takes hold of the active window.
    MindLiftWindow {
        at: V2,
    },
    /// Throw the lifted window: at `target` (combat), or off to `side`.
    MindThrowWindow {
        target: Option<V2>,
        side: f32,
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
    /// Bend down and rip word `id` up out of the line underfoot.
    Grab {
        id: u64,
    },
    /// Lob the carried word at `target`.
    Throw {
        target: V2,
    },
    Ko,
    GetUp,
    Stomp,
    /// Lunge at the cursor with both hands.
    SnatchCursor {
        aim: f32,
    },
    /// Hold the caught cursor overhead.
    HoldCursor,
    /// Lob the cursor at `target`.
    ThrowCursor {
        target: V2,
    },
    /// Hanging from the user's pointer.
    Dangled,
    /// Kick in mid-air (after leaping at a cursor overhead).
    FlyingKick {
        aim: f32,
    },
    LieDown,
    Sleep,
    Dance,
    /// Sitting on the edge of the platform, legs dangling.
    Sit,
    /// Happy after being petted.
    Cheer,
    /// Drawing `shape` in the air at `origin`.
    Doodle {
        shape: Shape,
        origin: V2,
        size: f32,
    },
    /// Kick the window underfoot sideways.
    Shove {
        dx: f32,
    },
    /// Powering up a super jump; launches with `vel`.
    Charge {
        vel: V2,
    },
    /// Telekinesis.
    Telekinesis {
        on: Mind,
    },
    /// Shaking the bars of a cell.
    Rattle,
    /// Yank a weapon off the window.
    RipWeapon,
    /// Swing the weapon toward `aim`.
    Slash {
        aim: f32,
    },
    /// Throw the weapon at `target`.
    ThrowWeapon {
        target: V2,
    },
    /// Spelling out a note with letters (hands raised, concentrating).
    Compose,
}

/// Hits a weapon lasts before the figure throws it.
const WEAPON_HITS: u32 = 8;

/// Seconds the pen takes to draw a doodle.
const DOODLE_DRAW: f32 = 2.6;
/// The arm comes up before the pen starts moving.
const DOODLE_LEAD: f32 = 0.3;

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
            Action::Grab { .. } => 0.55,
            Action::Throw { .. } => 0.55,
            Action::SnatchCursor { .. } => 0.4,
            Action::HoldCursor => 0.75,
            Action::ThrowCursor { .. } => 0.55,
            Action::FlyingKick { .. } => 0.45,
            Action::LieDown => 0.9,
            Action::Dance => 4.6,
            Action::Cheer => 1.6,
            Action::Doodle { .. } => DOODLE_LEAD + DOODLE_DRAW + 0.5,
            Action::Shove { .. } => 0.6,
            Action::Charge { .. } => 0.55,
            Action::RipWeapon => 0.6,
            Action::Compose => 2.2,
            Action::Slash { .. } => 0.45,
            Action::ThrowWeapon { .. } => 0.55,
            Action::Telekinesis { on: Mind::Cursor } => 1.5,
            Action::Telekinesis { on: Mind::Words } => 2.2,
            Action::Telekinesis { on: Mind::Window } => 2.0,
            Action::Taunt => 1.6,
            Action::Think => 3.0,
            _ => return None,
        })
    }

    /// Can behaviour replace this action right now?
    fn interruptible(self) -> bool {
        matches!(
            self,
            Action::Stand | Action::Walk { .. } | Action::Think | Action::Taunt | Action::Sit
        )
    }

    /// Standing, thinking or knocked out: nothing that needs full-rate ticks.
    pub fn is_still(self) -> bool {
        matches!(
            self,
            Action::Stand | Action::Think | Action::Ko | Action::Sleep | Action::Sit
        )
    }

    fn airborne(self) -> bool {
        matches!(
            self,
            Action::Air
                | Action::Fall
                | Action::Dodge
                | Action::Tumble
                | Action::Dangled
                | Action::FlyingKick { .. }
        )
    }
}

/// Optional behaviours the user can switch off, and temperament.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Traits {
    pub naps: bool,
    pub dancing: bool,
    pub doodles: bool,
    pub sitting: bool,
    pub powers: bool,
    /// 0 (gentle) .. 1 (fierce).
    pub aggression: f32,
    /// Health it heals up to (grows with level).
    pub max_health: f32,
    /// Walking speed multiplier (grows with level).
    pub speed: f32,
}

impl Default for Traits {
    fn default() -> Self {
        Traits {
            naps: true,
            dancing: true,
            doodles: true,
            sitting: true,
            powers: true,
            aggression: 0.5,
            max_health: 100.0,
            speed: 1.0,
        }
    }
}

pub struct Agent {
    pub traits: Traits,
    pub pos: V2,
    pub vel: V2,
    pub facing: f32,
    pub scale: f32,
    pub health: f32,
    pub frustration: f32,
    pub action: Action,
    /// Diagnostics: stand still (no behaviour), e.g. to measure idle CPU.
    pub hold_still: bool,
    /// Following a script (the jail-break intro): no behaviour of its own.
    pub scripted: bool,
    /// In a duel: no healing until it's over.
    pub dueling: bool,
    /// Holding a weapon this long (points), set by the app once it's in hand.
    pub weapon: Option<f32>,
    weapon_hits: u32,
    rip_cooldown: f32,
    /// Chance of grabbing a slow cursor in reach instead of hitting it.
    pub snatch_chance: f32,
    /// Carrying a word overhead.
    pub holding: bool,
    /// Holding the user's cursor. The app clears this when the user yanks free.
    pub holding_cursor: bool,
    /// The grab/throw moment of the current action already happened.
    act_fired: bool,
    /// Platform currently stood on.
    pub ground: Option<Platform>,
    t: f32,
    clock: f32,
    gait_phase: f32,
    spin: f32,
    spin_rate: f32,
    /// Where the pointer holds the figure (pos minus cursor) while dangling.
    grip: V2,
    /// Pendulum lean while dangling.
    swing: f32,
    /// This jump was a leap at the cursor: kick it on the way up.
    air_attack: bool,
    /// Hits on the cursor since the last word grab.
    combo: u32,
    /// Walking to an edge to sit on it.
    sit_plan: bool,
    /// Dancing to music (to announce it only once).
    grooving: bool,
    /// Trampoline bounces in a row (it sticks the landing after a few).
    bounces: u32,
    /// This flight is a super jump (glowing trail).
    super_air: bool,
    /// Seconds until telekinesis can be used again.
    tk_cooldown: f32,
    /// Seconds until the next super jump at the cursor.
    super_cooldown: f32,
    /// Where telekinesis is aimed (the cursor).
    tk_aim: V2,
    /// The second scripted moment of the current action happened.
    act_fired2: bool,
    /// Seconds of being stroked by the cursor.
    pet: f32,
    pet_cooldown: f32,
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
    anchored: Anchor,
    /// Time constant of `root_correction`'s decay, seconds.
    settle: f32,
    /// Pose as last drawn; new actions blend from here.
    shown: (Pose, f32),
    /// Current action's target pose, angle-unwrapped to stay continuous.
    target: (Pose, f32),
}

impl Agent {
    /// A figure standing at `x` on `ground`.
    pub fn new(x: f32, ground: Platform, scale: f32, seed: u64) -> Agent {
        Agent {
            traits: Traits::default(),
            pos: v2(x, ground.y),
            vel: V2::ZERO,
            facing: 1.0,
            scale,
            health: 100.0,
            frustration: 0.0,
            action: Action::Stand,
            hold_still: false,
            scripted: false,
            dueling: false,
            weapon: None,
            weapon_hits: 0,
            rip_cooldown: 3.0,
            snatch_chance: 0.3,
            holding: false,
            holding_cursor: false,
            act_fired: false,
            ground: Some(ground),
            t: 0.0,
            clock: 0.0,
            gait_phase: 0.0,
            spin: 0.0,
            spin_rate: 0.0,
            grip: V2::ZERO,
            swing: 0.0,
            air_attack: false,
            combo: 0,
            sit_plan: false,
            grooving: false,
            bounces: 0,
            super_air: false,
            tk_cooldown: 1.5,
            super_cooldown: 0.0,
            tk_aim: V2::ZERO,
            act_fired2: false,
            pet: 0.0,
            pet_cooldown: 0.0,
            spin_from: 0.0,
            blend_from: (idle_pose(0.0), 0.0),
            blend_left: 0.0,
            hit_landed: false,
            attack_cooldown: 0.0,
            decide_left: 0.5,
            since_hit: 99.0,
            rng: seed | 1,
            root_correction: 0.0,
            anchored: Anchor::Feet,
            settle: 0.08,
            shown: (idle_pose(0.0), 0.0),
            target: (idle_pose(0.0), 0.0),
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

    /// Scripted moves for the jail-break intro.
    pub fn script_rattle(&mut self, dir: f32) {
        self.face(dir);
        self.set_action(Action::Rattle);
    }

    pub fn script_kick(&mut self, dir: f32) {
        self.face(dir);
        self.set_action(Action::Kick { aim: 0.15 });
    }

    pub fn script_think(&mut self) {
        self.set_action(Action::Think);
    }

    /// Start a named action right away (debugging and demos): `sleep`,
    /// `dance`, `cheer`, `doodle`, `superjump` or `mindwords`. Returns whether the name was known.
    pub fn perform(&mut self, name: &str, events: &mut Vec<Event>) -> bool {
        match name.trim() {
            "sleep" => self.set_action(Action::Sleep),
            "dance" => self.set_action(Action::Dance),
            "cheer" => self.set_action(Action::Cheer),
            "compose" => {
                if self.ground.is_some() && self.action.interruptible() {
                    self.set_action(Action::Compose);
                }
            }
            "superjump" => self.super_jump_to(self.pos - v2(0.0, 600.0)),
            "mindwords" => self.set_action(Action::Telekinesis { on: Mind::Words }),
            "throwwindow" => self.set_action(Action::Telekinesis { on: Mind::Window }),
            "doodle" => {
                let size = 20.0 * (self.scale / 0.55);
                let origin = self.shoulder() + v2(self.facing * (self.reach() + size + 4.0), -8.0);
                let shape = Shape::Heart;
                self.set_action(Action::Doodle {
                    shape,
                    origin,
                    size,
                });
                events.push(Event::DoodleStarted {
                    shape,
                    origin,
                    size,
                });
            }
            _ => return false,
        }
        true
    }

    /// Seconds since the figure was last hit (for the health bar).
    pub fn since_hit(&self) -> f32 {
        self.since_hit
    }

    /// Stride time; quicker at higher levels (same stride, so feet stay planted).
    fn gait_period(&self, run: bool) -> f32 {
        let base = if run { RUN_PERIOD } else { GAIT_PERIOD };
        base / self.traits.speed.max(0.5)
    }

    fn walk_speed(&self, run: bool) -> f32 {
        let period = self.gait_period(run);
        if run {
            crate::stickman::RUN.speed(period) * self.scale
        } else {
            gait_speed_for(period) * self.scale
        }
    }

    fn set_action(&mut self, a: Action) {
        if matches!(
            a,
            Action::Punch { .. }
                | Action::Kick { .. }
                | Action::FlyingKick { .. }
                | Action::Slash { .. }
        ) {
            self.hit_landed = false;
        }
        self.act_fired = false;
        self.act_fired2 = false;
        if a == Action::GetUp {
            self.spin_from = self.spin;
        }
        if !matches!(a, Action::Walk { .. }) {
            self.gait_phase = 0.0;
        }
        if !matches!(a, Action::Walk { .. } | Action::Sit) {
            self.sit_plan = false;
        }
        self.action = a;
        self.t = 0.0;
        self.start_blend();
    }

    fn face(&mut self, dir: f32) {
        if dir != 0.0 && dir.signum() != self.facing {
            // Re-express what's on screen in the new facing frame, so this blend and
            // any action started this tick continue from the drawn pose.
            let (p, s) = self.shown;
            self.shown = (mirror_pose(&p), -s);
            self.facing = dir.signum();
            self.start_blend();
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
        (53.0 + 48.0) * self.scale + self.weapon.unwrap_or(0.0) * 0.8
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
            Action::Dangled => poses::dangle(self.clock),
            Action::LieDown => poses::lie_down(k(self.action)),
            Action::Sleep => poses::sleep(self.clock),
            Action::Dance => poses::dance(t),
            Action::Sit => poses::sit(self.clock),
            Action::Cheer => poses::cheer(t),
            Action::Doodle { .. } => {
                let pen = self.pen().unwrap_or(self.shoulder());
                poses::punch(0.5, self.aim_at(pen, -0.75, 0.9))
            }
            Action::Shove { .. } => poses::stomp(k(self.action)),
            Action::Charge { .. } => poses::charge(t),
            Action::Rattle => poses::rattle(t),
            Action::RipWeapon => poses::grab(k(self.action)),
            Action::Compose => poses::telekinesis(t, -0.5),
            Action::Slash { aim } => poses::slash(k(self.action), aim),
            Action::ThrowWeapon { target } => {
                poses::throw(k(self.action), self.aim_at(target, -1.2, 1.2))
            }
            Action::Telekinesis { .. } => {
                poses::telekinesis(t, self.aim_at(self.tk_aim, -0.75, 0.9))
            }
            Action::FlyingKick { aim } => poses::flying_kick(k(self.action), aim),
            Action::Tumble => poses::tumble(),
            Action::Ko => idle_pose(0.0),
            Action::GetUp => poses::crouch(1.0 - smoothstep((t - 0.3) / 0.4)),
            Action::Stomp => poses::stomp(k(self.action)),
            Action::Grab { .. } => poses::grab(k(self.action)),
            Action::Throw { target } | Action::ThrowCursor { target } => {
                poses::throw(k(self.action), self.aim_at(target, -1.2, 1.2))
            }
            Action::SnatchCursor { aim } => poses::snatch(k(self.action), aim),
            Action::HoldCursor => poses::hoist(t),
        };
        let mut p = p;
        let carrying_pose = !matches!(
            self.action,
            Action::Grab { .. }
                | Action::Throw { .. }
                | Action::Tumble
                | Action::Ko
                | Action::GetUp
                | Action::Fall
        );
        if self.holding && carrying_pose {
            poses::hold_arms(&mut p);
        }
        let spin = match self.action {
            Action::Tumble => self.spin,
            Action::Ko => self.spin,
            Action::Dangled => self.swing,
            Action::GetUp => self.spin_from * (1.0 - smoothstep(t / 0.45)),
            _ => 0.0,
        };
        // Fighting stance while combat-idle.
        (p, spin)
    }

    /// Re-evaluate the action's target pose, unwrapping every angle to the
    /// representative nearest the previous target. The target path is then
    /// continuous, so blends can interpolate linearly: a shortest-arc lerp would
    /// flip direction whenever an animated target swings past the antipode of
    /// the blend's start (an arm snapping round mid-blend).
    fn refresh_target(&mut self) {
        let (raw, raw_spin) = self.target_pose();
        let near = |prev: f32, a: f32| a + TAU * ((prev - a) / TAU).round();
        for (cont, raw) in self.target.0.iter_mut().zip(raw) {
            *cont = near(*cont, raw);
        }
        self.target.1 = near(self.target.1, raw_spin);
    }

    /// Start a blend from what's on screen: unwrap the new target next to it so
    /// the transition takes the short way round.
    fn start_blend(&mut self) {
        self.blend_from = self.shown;
        self.blend_left = BLEND_SECONDS;
        self.target = self.shown;
        self.refresh_target();
    }

    fn pose_and_spin(&self) -> (Pose, f32) {
        let (p, s) = self.target;
        if self.blend_left > 0.0 {
            let k = smoothstep(1.0 - self.blend_left / BLEND_SECONDS);
            let from = &self.blend_from;
            (
                std::array::from_fn(|i| from.0[i] + (p[i] - from.0[i]) * k),
                from.1 + (s - from.1) * k,
            )
        } else {
            (p, s)
        }
    }

    fn anchor(&self) -> Anchor {
        if self.action == Action::Sit {
            Anchor::Seat
        } else if self.action.airborne() && self.action != Action::Tumble {
            Anchor::Hip
        } else {
            Anchor::Feet
        }
    }

    /// Hip height for the given pose under an anchoring rule.
    fn root_y(&self, rotated: &Pose, anchor: Anchor) -> f32 {
        match anchor {
            // Airborne: hip at standing height above `pos`, so tucking the legs
            // doesn't make the body drop.
            Anchor::Hip => self.pos.y - (poses::STAND_HIP + 5.0) * self.scale,
            Anchor::Feet => self.pos.y - depth_below_root(rotated, self.scale),
            Anchor::Seat => self.pos.y - 4.0 * self.scale,
        }
    }

    /// Where the doodling pen is now, world points.
    fn pen(&self) -> Option<V2> {
        match self.action {
            Action::Doodle {
                shape,
                origin,
                size,
            } => Some(origin + doodle::pen_at(shape, self.doodle_k()) * size),
            _ => None,
        }
    }

    /// How much of the current doodle is drawn, while doodling.
    pub fn doodle_progress(&self) -> Option<f32> {
        matches!(self.action, Action::Doodle { .. }).then(|| self.doodle_k())
    }

    fn doodle_k(&self) -> f32 {
        ((self.t - DOODLE_LEAD) / DOODLE_DRAW).clamp(0.0, 1.0)
    }

    /// Flying high on a super jump (for the glowing trail).
    pub fn super_flight(&self) -> bool {
        self.super_air && self.ground.is_none()
    }

    /// Powering up: crouched for a super jump.
    pub fn charging(&self) -> bool {
        matches!(self.action, Action::Charge { .. })
    }

    /// The reaching hand while using telekinesis, and on what.
    pub fn telekinesis(&self) -> Option<(V2, Mind)> {
        match self.action {
            Action::Telekinesis { on } => Some((self.segments()[FOREARM[0]].end.into(), on)),
            _ => None,
        }
    }

    /// The weapon's grip (the lead hand) and tip, while armed.
    pub fn blade(&self) -> Option<(V2, V2)> {
        let len = self.weapon?;
        let f = self.segments()[FOREARM[0]];
        let (a, b) = (V2::from(f.start), V2::from(f.end));
        let dir = (b - a).normalize_or(v2(self.facing, 0.0));
        Some((b, b + dir * len))
    }

    /// The pen tip and the hand holding it, while doodling.
    pub fn pen_stroke(&self) -> Option<(V2, V2)> {
        let pen = self.pen()?;
        Some((self.segments()[FOREARM[0]].end.into(), pen))
    }

    fn rotated_pose(&self) -> Pose {
        let (pose, spin) = self.pose_and_spin();
        std::array::from_fn(|i| pose[i] + spin)
    }

    /// The figure's bones in overlay points.
    pub fn segments(&self) -> [Segment; BONES] {
        let rotated = self.rotated_pose();
        let root_y = self.root_y(&rotated, self.anchor()) + self.root_correction;
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

    /// Between the two hands: where a carried word sits.
    pub fn hands(&self) -> V2 {
        let s = self.segments();
        let (a, b) = (s[FOREARM[0]].end, s[FOREARM[1]].end);
        v2((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0)
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
        // How far "the same" surface may have moved between updates: a dragged
        // window jumps a lot between polls; a re-read text line only jitters (a
        // bigger shift means it's a different line, or the page scrolled).
        let tolerance = match old.kind {
            PlatformKind::Ground => f32::MAX,
            PlatformKind::Window => 400.0,
            PlatformKind::Text => 12.0,
            PlatformKind::Drawn | PlatformKind::Trampoline => 1.0,
        };
        // A window edge of the same width that moved sideways carries the
        // figure along with it.
        let shift = |p: &Platform| {
            if old.kind == PlatformKind::Window && (p.width() - old.width()).abs() < 2.0 {
                p.x0 - old.x0
            } else {
                0.0
            }
        };
        // The nearest matching surface, never just the first: windows have many
        // text lines, and picking the first made the figure teleport between them.
        let same = world
            .platforms()
            .iter()
            .copied()
            .filter(|p| {
                p.kind == old.kind
                    && p.owner == old.owner
                    && p.contains_x(self.pos.x + shift(p))
                    && (p.y - old.y).abs() <= tolerance
            })
            .min_by(|a, b| (a.y - old.y).abs().total_cmp(&(b.y - old.y).abs()));
        match same {
            Some(p) => {
                let dx = shift(&p);
                self.pos.x += dx;
                if let Action::Walk { to, .. } = &mut self.action {
                    *to += dx;
                }
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
        self.refresh_target();
        self.attack_cooldown -= dt;
        self.decide_left -= dt;
        self.since_hit += dt;
        if self.action != Action::Ko && self.since_hit > 3.0 && !self.dueling {
            self.health = (self.health + 4.0 * dt).min(self.traits.max_health);
        }
        self.frustration = (self.frustration - 1.5 * dt).max(0.0);

        let before_root = self.root_y(&self.rotated_pose(), self.anchored) + self.root_correction;
        let before_pos = self.pos.y;

        if s.held {
            self.dangle(s, events);
        } else {
            if self.action == Action::Dangled {
                self.fling(s, events);
            }
            self.act(s, world, events);
        }

        // Keep the drawn body continuous when the anchoring rule flips.
        let anchor = self.anchor();
        if anchor != self.anchored {
            let moved = self.pos.y - before_pos;
            let new_root = self.root_y(&self.rotated_pose(), anchor);
            self.root_correction = before_root + moved - new_root;
            // Sitting down or getting up from a seat settles more slowly.
            self.settle = if anchor == Anchor::Seat || self.anchored == Anchor::Seat {
                0.16
            } else {
                0.08
            };
            self.anchored = anchor;
        }
        self.refresh_target();
        self.shown = self.pose_and_spin();
        self.root_correction *= (-dt / self.settle).exp();
        if self.root_correction.abs() < 0.01 {
            self.root_correction = 0.0;
        }
    }

    /// Hanging from the user's pointer: follow it, swinging with its motion.
    fn dangle(&mut self, s: &Senses, events: &mut Vec<Event>) {
        let dt = FIXED_DT;
        if self.action != Action::Dangled {
            if self.holding {
                self.holding = false;
                events.push(Event::Dropped { at: self.hands() });
            }
            self.holding_cursor = false;
            self.air_attack = false;
            self.grip = self.pos - s.cursor;
            self.swing = 0.0;
            self.ground = None;
            self.set_action(Action::Dangled);
            events.push(Event::PickedUp { at: s.cursor });
        }
        // Slide so it hangs by the scruff of the neck, just under the pointer.
        let hang = v2(0.0, (poses::STAND_HIP + 5.0 + 78.0) * self.scale);
        self.grip += (hang - self.grip) * (1.0 - (-dt / 0.25).exp());
        self.pos = s.cursor + self.grip;
        self.vel = s.cursor_vel;
        // Body lags behind the pointer like a pendulum.
        let lean = (-s.cursor_vel.x * 0.0006).clamp(-0.7, 0.7);
        self.swing += (lean - self.swing) * (1.0 - (-dt / 0.12).exp());
        self.frustration = (self.frustration + 3.0 * dt).min(100.0);
    }

    /// Let go after being dragged: fly off with the pointer's velocity.
    fn fling(&mut self, s: &Senses, events: &mut Vec<Event>) {
        let vel = s.cursor_vel.clamp_length_max(2600.0);
        self.vel = vel;
        self.ground = None;
        let speed = vel.length();
        if speed < 200.0 {
            self.set_action(Action::Fall);
        } else {
            self.spin = self.swing;
            self.spin_rate = (vel.x * 0.01).clamp(-16.0, 16.0) * self.facing;
            self.set_action(Action::Tumble);
            if speed > 1400.0 {
                // Hurled hard: that hurts.
                self.health -= 15.0;
                self.since_hit = 0.0;
                self.frustration = (self.frustration + 20.0).min(100.0);
            }
        }
        events.push(Event::Flung { at: self.pos, vel });
    }

    /// One tick of normal life: reflexes, actions, behaviour, physics.
    fn act(&mut self, s: &Senses, world: &World, events: &mut Vec<Event>) {
        self.pet_cooldown -= FIXED_DT;
        self.tk_cooldown -= FIXED_DT;
        self.rip_cooldown -= FIXED_DT;
        self.super_cooldown -= FIXED_DT;
        self.tk_aim = s.cursor;
        // Asleep: wake up when the user is back, or a fight starts.
        if matches!(self.action, Action::Sleep | Action::LieDown)
            && (s.user_idle < 0.5 || s.clicked || s.mode == Mode::Combat)
        {
            self.set_action(Action::GetUp);
            events.push(Event::Woke { at: self.head() });
        }
        self.reflexes(s, events);
        self.petting(s, events);
        if self.action == Action::Dance {
            // Turn around every four beats.
            let period = 4.0 / 2.6;
            if (self.t / period).floor() != ((self.t - FIXED_DT) / period).floor() {
                self.face(-self.facing);
            }
        }
        // Leaping at the cursor: kick it on the way.
        if self.action == Action::Air
            && self.air_attack
            && s.mode == Mode::Combat
            && (s.cursor - self.center()).length() < self.reach() * 1.7
        {
            self.air_attack = false;
            let aim = self.aim_at(s.cursor, -0.6, 0.8);
            self.face((s.cursor.x - self.pos.x).signum());
            self.set_action(Action::FlyingKick { aim });
            events.push(Event::Swing { at: self.center() });
        }
        // The user yanked the cursor free (or a click made us let go).
        let lost_grip = match self.action {
            Action::SnatchCursor { .. } => self.act_fired && !self.holding_cursor,
            Action::HoldCursor => !self.holding_cursor,
            Action::ThrowCursor { .. } => !self.holding_cursor && !self.act_fired,
            Action::Telekinesis { on: Mind::Cursor } => {
                self.act_fired && !self.act_fired2 && !self.holding_cursor
            }
            _ => false,
        };
        if lost_grip {
            self.set_action(Action::Stand);
        }
        self.finish_actions(events);
        self.attack_contacts(s, events);
        if self.ground.is_some()
            && self.action.interruptible()
            && !self.hold_still
            && !self.scripted
        {
            self.behave(s, world, events);
        }
        self.integrate(s, world, events);
    }

    /// Slow strokes of the cursor over the figure (outside combat) make it happy.
    fn petting(&mut self, s: &Senses, events: &mut Vec<Event>) {
        let speed = s.cursor_vel.length();
        let stroking = self.hit_test(s.cursor)
            && (25.0..900.0).contains(&speed)
            && s.mode != Mode::Combat
            && self.ground.is_some()
            && !s.clicked;
        self.pet = if stroking {
            self.pet + FIXED_DT
        } else {
            (self.pet - 2.0 * FIXED_DT).max(0.0)
        };
        if self.pet >= PET_TIME && self.pet_cooldown <= 0.0 && self.action.interruptible() {
            self.pet = 0.0;
            self.pet_cooldown = 3.0;
            self.frustration = (self.frustration - 40.0).max(0.0);
            self.health = (self.health + 15.0).min(self.traits.max_health);
            self.face((s.cursor.x - self.pos.x).signum());
            self.set_action(Action::Cheer);
            events.push(Event::Petted { at: self.head() });
        }
    }

    /// Sent flying along `away` (plus `extra` velocity), losing `damage` health.
    fn knock(&mut self, away: V2, extra: V2, damage: f32, events: &mut Vec<Event>) {
        self.holding_cursor = false;
        if self.weapon.take().is_some() {
            events.push(Event::WeaponDropped { at: self.hands() });
        }
        if self.holding {
            self.holding = false;
            events.push(Event::Dropped { at: self.hands() });
        }
        let knock = (away * 620.0 + v2(0.0, -520.0) + extra).clamp_length_max(1500.0);
        self.vel = knock;
        self.ground = None;
        self.spin_rate = 15.0 * knock.x.signum() * self.facing;
        if self.action != Action::Tumble {
            self.spin = 0.0;
        }
        self.health -= damage;
        self.frustration = (self.frustration + 22.0).min(100.0);
        self.since_hit = 0.0;
        if self.action != Action::Tumble {
            self.set_action(Action::Tumble);
        }
    }

    /// Hit by another figure's punch or kick along `dir`.
    pub fn take_hit(&mut self, dir: V2, damage: f32, events: &mut Vec<Event>) {
        if matches!(self.action, Action::Ko | Action::GetUp) {
            return; // no hitting someone who's down
        }
        let dir = dir.normalize_or(v2(-self.facing, 0.0));
        self.knock(v2(dir.x, dir.y.min(0.0)), V2::ZERO, damage, events);
        events.push(Event::Struck {
            at: self.center(),
            dir,
        });
    }

    /// Drop in from above at `at` (falling).
    pub fn drop_in(&mut self, at: V2) {
        self.pos = at;
        self.vel = V2::ZERO;
        self.ground = None;
        self.set_action(Action::Fall);
    }

    /// Walk to `x` (scripted).
    pub fn go_to(&mut self, x: f32) {
        if !matches!(self.action, Action::Walk { .. })
            && self.ground.is_some()
            && self.action.interruptible()
        {
            self.walk_to(x, true);
        }
    }

    /// Knocked out right now.
    pub fn knocked_out(&self) -> bool {
        self.action == Action::Ko
    }

    fn reflexes(&mut self, s: &Senses, events: &mut Vec<Event>) {
        // Clicks land even mid-air (juggling), except while knocked out.
        if s.clicked && self.action != Action::Ko {
            let away = (self.center() - s.cursor).normalize_or(v2(-self.facing, 0.0));
            self.knock(away, s.cursor_vel * 0.25, 20.0, events);
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
                events.push(Event::Swing {
                    at: self.shoulder(),
                });
            } else {
                let away = to_us.x.signum();
                self.face(-away);
                self.vel = v2(away * 340.0, -440.0);
                self.ground = None;
                self.frustration = (self.frustration + 4.0).min(100.0);
                self.set_action(Action::Dodge);
                events.push(Event::Dodged { at: self.center() });
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
            Action::Charge { vel } => {
                self.vel = vel;
                self.ground = None;
                self.super_air = true;
                self.set_action(Action::Air);
                events.push(Event::SuperJumped { at: self.pos });
            }
            Action::Ko => {
                // Back from a knockout with some health.
                self.health = self.health.max(40.0);
                self.set_action(Action::GetUp)
            }
            Action::LieDown => self.set_action(Action::Sleep),
            Action::FlyingKick { .. } => self.set_action(Action::Air),
            Action::SnatchCursor { .. } if self.holding_cursor => {
                self.set_action(Action::HoldCursor)
            }
            Action::HoldCursor if self.holding_cursor => {
                // Fling it far across the screen, high.
                let side = if self.random(0.0, 1.0) < 0.5 {
                    -1.0
                } else {
                    1.0
                };
                let target =
                    self.pos + v2(side * self.random(450.0, 850.0), -self.random(300.0, 600.0));
                self.face(side);
                self.set_action(Action::ThrowCursor { target });
            }
            Action::GetUp => {
                self.spin = 0.0;
                self.set_action(Action::Stand);
            }
            Action::Think | Action::Taunt => self.t = 0.0, // loop until behaviour changes
            _ => self.set_action(Action::Stand),
        }
    }

    /// A slow cursor within arm's reach, between hip and head height, is
    /// sometimes grabbed instead of punched.
    fn wants_to_snatch(&mut self, s: &Senses) -> bool {
        let hip_y = self.pos.y - poses::STAND_HIP * self.scale;
        let head_y = self.pos.y - 240.0 * self.scale;
        s.can_grab_cursor
            && s.cursor_vel.length() < 350.0
            && (head_y..=hip_y).contains(&s.cursor.y)
            && self.random(0.0, 1.0) < self.snatch_chance
    }

    fn snatch(&mut self, target: V2) {
        self.face((target.x - self.pos.x).signum());
        self.attack_cooldown = 1.5;
        let aim = self.aim_at(target, -0.7, 0.9);
        self.set_action(Action::SnatchCursor { aim });
    }

    fn attack(&mut self, target: V2) {
        self.face((target.x - self.pos.x).signum());
        if self.weapon.is_some() {
            // Armed: cut, or throw it once it's worn out.
            self.attack_cooldown = 0.55 - 0.3 * self.traits.aggression;
            if self.weapon_hits >= WEAPON_HITS {
                self.set_action(Action::ThrowWeapon { target });
            } else {
                let aim = self.aim_at(target, -0.6, 0.9);
                self.set_action(Action::Slash { aim });
            }
            return;
        }
        let hip_y = self.pos.y - poses::STAND_HIP * self.scale;
        let low = target.y > hip_y - 10.0;
        let a = if low || self.random(0.0, 1.0) < 0.3 {
            Action::Kick {
                aim: self.aim_at(target, -0.4, 1.0),
            }
        } else {
            // Steeper than ~43° up would swing the arm through the head; higher
            // targets get a jump attack instead.
            Action::Punch {
                aim: self.aim_at(target, -0.75, 0.9),
            }
        };
        // Fiercer figures attack in quicker succession.
        self.attack_cooldown = 0.6 - 0.45 * self.traits.aggression;
        self.set_action(a);
    }

    fn attack_contacts(&mut self, s: &Senses, events: &mut Vec<Event>) {
        match self.action {
            Action::Grab { id } if !self.act_fired && self.t >= 0.28 => {
                self.act_fired = true;
                self.combo = 0;
                self.holding = true;
                events.push(Event::Grabbed { id });
                events.push(Event::Dust {
                    at: self.pos,
                    strength: 0.15,
                });
                return;
            }
            Action::SnatchCursor { .. } if !self.act_fired && (0.14..=0.26).contains(&self.t) => {
                if (self.hands() - s.cursor).length() < 42.0 {
                    self.act_fired = true;
                    self.holding_cursor = true;
                    events.push(Event::CaughtCursor { at: s.cursor });
                }
                return;
            }
            Action::ThrowCursor { target }
                if !self.act_fired && self.t >= 0.22 && self.holding_cursor =>
            {
                self.act_fired = true;
                self.holding_cursor = false;
                let from = self.hands();
                events.push(Event::ThrewCursor {
                    from,
                    vel: lob_velocity(from, target),
                });
                self.frustration = (self.frustration - 20.0).max(0.0);
                return;
            }
            Action::Throw { target } if !self.act_fired && self.t >= 0.22 && self.holding => {
                self.act_fired = true;
                self.holding = false;
                let from = self.hands();
                events.push(Event::Threw {
                    from,
                    vel: lob_velocity(from, target),
                });
                self.frustration = (self.frustration - 10.0).max(0.0);
                return;
            }
            _ => {}
        }
        match self.action {
            Action::Telekinesis { on: Mind::Cursor } => {
                if !self.act_fired && self.t >= 0.2 {
                    self.act_fired = true;
                    self.holding_cursor = true;
                    events.push(Event::MindLiftCursor { at: s.cursor });
                }
                if self.act_fired && !self.act_fired2 && self.t >= 1.1 && self.holding_cursor {
                    self.act_fired2 = true;
                    self.holding_cursor = false;
                    // Away from the figure and up.
                    let away = (s.cursor - self.center()).normalize_or(v2(self.facing, 0.0));
                    let vel = v2(away.x.signum() * 1100.0, -650.0 + away.y * 300.0);
                    events.push(Event::MindFlingCursor {
                        from: s.cursor,
                        vel,
                    });
                    self.frustration = (self.frustration - 25.0).max(0.0);
                }
                return;
            }
            Action::Telekinesis { on: Mind::Window } => {
                if !self.act_fired && self.t >= 0.25 {
                    self.act_fired = true;
                    events.push(Event::MindLiftWindow { at: self.center() });
                }
                if !self.act_fired2 && self.t >= 1.35 {
                    self.act_fired2 = true;
                    let target = (s.mode == Mode::Combat).then_some(s.cursor);
                    events.push(Event::MindThrowWindow {
                        target,
                        side: self.facing,
                    });
                    self.frustration = (self.frustration - 30.0).max(0.0);
                }
                return;
            }
            Action::Telekinesis { on: Mind::Words } => {
                if !self.act_fired && self.t >= 0.25 {
                    self.act_fired = true;
                    events.push(Event::MindLiftWords { at: self.center() });
                }
                if !self.act_fired2 && self.t >= 1.6 {
                    self.act_fired2 = true;
                    events.push(Event::MindFireWords {
                        target: s.cursor + s.cursor_vel * 0.15,
                    });
                    self.frustration = (self.frustration - 25.0).max(0.0);
                }
                return;
            }
            _ => {}
        }
        match self.action {
            Action::RipWeapon if !self.act_fired && self.t >= 0.3 => {
                self.act_fired = true;
                events.push(Event::WantWeapon { at: self.hands() });
                return;
            }
            Action::Slash { aim } if !self.hit_landed && (0.12..=0.32).contains(&self.t) => {
                if let Some((grip, tip)) = self.blade() {
                    // Anywhere along the blade counts.
                    let (ab, ap) = (tip - grip, s.cursor - grip);
                    let k = (ap.dot(ab) / ab.length_squared().max(1e-3)).clamp(0.0, 1.0);
                    if (s.cursor - (grip + ab * k)).length() < 26.0 {
                        self.hit_landed = true;
                        self.weapon_hits += 1;
                        let dir = v2(aim.cos() * self.facing, aim.sin()).normalize_or_zero();
                        events.push(Event::Slashed { at: s.cursor, dir });
                        self.frustration = (self.frustration - 15.0).max(0.0);
                    }
                }
                return;
            }
            Action::ThrowWeapon { target } if !self.act_fired && self.t >= 0.22 => {
                self.act_fired = true;
                if self.weapon.take().is_some() {
                    let from = self.hands();
                    events.push(Event::WeaponThrown {
                        from,
                        vel: lob_velocity(from, target),
                    });
                }
                return;
            }
            _ => {}
        }
        if let Action::Shove { dx } = self.action {
            if !self.act_fired && self.t >= 0.3 {
                self.act_fired = true;
                if let Some(g) = self.ground.filter(|g| g.kind == PlatformKind::Window) {
                    events.push(Event::ShoveWindow {
                        at: self.pos,
                        window: g.owner,
                        dx,
                    });
                    events.push(Event::Dust {
                        at: self.pos,
                        strength: 0.6,
                    });
                }
            }
            return;
        }
        let (window, bone, radius, aim) = match self.action {
            Action::Punch { aim } => ((0.16, 0.28), FOREARM[0], 30.0, aim),
            Action::Kick { aim } => ((0.22, 0.34), SHIN[0], 34.0, aim),
            Action::FlyingKick { aim } => ((0.12, 0.32), SHIN[0], 38.0, aim),
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
            self.combo += 1;
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
        let vel = self.jump_vel(target);
        self.face(vel.x.signum());
        self.set_action(Action::Crouch { vel });
    }

    /// A power-up and a huge leap to `target` (powers).
    fn super_jump_to(&mut self, target: V2) {
        let vel = self.jump_vel(target);
        self.face(vel.x.signum());
        self.set_action(Action::Charge { vel });
    }

    /// Ballistic launch velocity that lands on `target`, clearing it by 40 pt.
    fn jump_vel(&self, target: V2) -> V2 {
        let rise = (self.pos.y - target.y + 40.0).max(30.0);
        let vy = -(2.0 * GRAVITY * rise).sqrt();
        let t_up = -vy / GRAVITY;
        let t_down = (2.0 * (rise - (self.pos.y - target.y)).max(1.0) / GRAVITY).sqrt();
        v2((target.x - self.pos.x) / (t_up + t_down), vy)
    }

    /// Use telekinesis in combat, now and then.
    fn mind_powers(&mut self, s: &Senses) -> bool {
        if !self.traits.powers || self.tk_cooldown > 0.0 || self.holding {
            return false;
        }
        let dist = (s.cursor - self.center()).length();
        // Now and then the big one: the whole window.
        let window_roll = self.random(0.0, 1.0);
        let on = if s.can_throw_window && window_roll < 0.2 && dist > self.reach() + 60.0 {
            Mind::Window
        } else if s.tk_words_near >= 2 && dist > self.reach() + 60.0 && dist < 1400.0 {
            Mind::Words
        } else if s.can_grab_cursor
            && (self.reach() + 60.0..1000.0).contains(&dist)
            && s.cursor_vel.length() < 400.0
        {
            Mind::Cursor
        } else {
            return false;
        };
        let base = match on {
            Mind::Words => 0.5,
            Mind::Cursor => 0.35,
            Mind::Window => 0.6,
        };
        let chance = base * (0.5 + self.traits.aggression);
        if self.random(0.0, 1.0) >= chance {
            self.tk_cooldown = 2.0; // not this time
            return false;
        }
        self.tk_cooldown = 7.0;
        self.face((s.cursor.x - self.pos.x).signum());
        self.set_action(Action::Telekinesis { on });
        true
    }

    fn behave(&mut self, s: &Senses, world: &World, events: &mut Vec<Event>) {
        let dx = s.cursor.x - self.pos.x;
        // The user's away: nap (wherever it is).
        if self.traits.naps
            && matches!(s.mode, Mode::Idle | Mode::Observe)
            && s.user_idle > SLEEP_AFTER
            && !self.holding
            && !matches!(self.action, Action::Walk { .. })
        {
            self.set_action(Action::LieDown);
            events.push(Event::Yawned { at: self.head() });
            return;
        }
        match s.mode {
            Mode::Combat => {
                if self.mind_powers(s) {
                    return;
                }
                // Now and then, arm up: rip a scrollbar off the window.
                if s.weapon_available
                    && self.weapon.is_none()
                    && !self.holding
                    && self.rip_cooldown <= 0.0
                {
                    self.rip_cooldown = 4.0;
                    if self.random(0.0, 1.0) < 0.45 {
                        self.weapon_hits = 0;
                        self.set_action(Action::RipWeapon);
                        return;
                    }
                }
                if self.holding {
                    // Pelt the cursor, leading it a little.
                    if self.attack_cooldown <= 0.0 && (s.cursor - self.pos).length() < 1500.0 {
                        let target = s.cursor + s.cursor_vel * 0.2;
                        self.face((target.x - self.pos.x).signum());
                        self.attack_cooldown = 0.6;
                        self.set_action(Action::Throw { target });
                        return;
                    }
                } else if let Some(a) = s.ammo {
                    // Close-range fists beat fetching ammo, until a few hits
                    // in a row haven't driven the cursor off: then mix it up.
                    let near = (s.cursor - self.shoulder()).length() < self.reach() + 25.0;
                    if (!near || self.combo >= 3) && self.fetch(a, world) {
                        return;
                    }
                }
                if (s.cursor - self.shoulder()).length() < self.reach() + 25.0 {
                    if self.attack_cooldown <= 0.0 {
                        if self.wants_to_snatch(s) {
                            self.snatch(s.cursor);
                        } else {
                            self.attack(s.cursor);
                        }
                        events.push(Event::Swing {
                            at: self.shoulder(),
                        });
                    }
                } else if s.cursor.y < self.shoulder().y - 40.0
                    && dx.abs() < 260.0
                    && self.attack_cooldown <= 0.0
                {
                    // Cursor above punching reach: leap at it (and flying-kick it).
                    // Way up high: a super jump, with powers.
                    let high = self.pos.y - s.cursor.y > MAX_JUMP_RISE;
                    let super_ready = self.traits.powers && self.super_cooldown <= 0.0;
                    let max = if super_ready {
                        SUPER_RISE
                    } else {
                        MAX_JUMP_RISE
                    };
                    let rise = (self.pos.y - s.cursor.y).min(max);
                    self.attack_cooldown = 1.2;
                    let target = v2(s.cursor.x, self.pos.y - rise + 40.0);
                    if high && super_ready {
                        self.super_cooldown = 6.0;
                        self.super_jump_to(target);
                    } else {
                        self.jump_to(target);
                    }
                    self.air_attack = true;
                } else if dx.abs() > self.reach() * 0.8 {
                    let stop = s.cursor.x - dx.signum() * self.reach() * 0.7;
                    self.walk_to(stop, dx.abs() > 160.0);
                } else if !matches!(self.action, Action::Taunt) && self.decide_left <= 0.0 {
                    self.face(dx.signum());
                    self.decide_left = 2.0;
                    self.set_action(Action::Taunt);
                }
            }
            Mode::Observe if !(s.music && self.traits.dancing) => {
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
            Mode::Idle | Mode::Observe if s.music && self.traits.dancing && !self.holding => {
                // Music's on: dance (again and again, while it plays).
                if !matches!(self.action, Action::Walk { .. }) {
                    self.set_action(Action::Dance);
                    if !self.grooving {
                        events.push(Event::StartedDancing { at: self.head() });
                    }
                    self.grooving = true;
                }
            }
            Mode::Idle => self.wander(s, world, events),
            // Watching while carrying a word, music on: just keep watching.
            Mode::Observe => {}
        }
        if !s.music {
            self.grooving = false;
        }
        // Arrived?
        if let Action::Walk { to, .. } = self.action {
            if (self.facing > 0.0 && self.pos.x >= to) || (self.facing < 0.0 && self.pos.x <= to) {
                if self.sit_plan {
                    self.sit_plan = false;
                    self.decide_left = self.random(6.0, 14.0);
                    self.set_action(Action::Sit);
                } else {
                    self.set_action(Action::Stand);
                }
            }
        }
    }

    /// Go and grab `a`: walk to it on this line, jump up to its line, climb toward
    /// it, or drop down through the current platform. Returns whether it acted.
    fn fetch(&mut self, a: Ammo, world: &World) -> bool {
        let Some(ground) = self.ground else {
            return false;
        };
        let dx = a.at.x - self.pos.x;
        // On the word's line (or on the Dock, over the icon).
        let on_line = matches!(ground.kind, PlatformKind::Text | PlatformKind::Ground)
            && (ground.y - a.line_y).abs() < 2.0
            && ground.contains_x(a.at.x);
        if on_line {
            if dx.abs() > 5.0 {
                self.walk_to(a.at.x, dx.abs() > 200.0);
            } else {
                self.set_action(Action::Grab { id: a.id });
            }
            return true;
        }
        let rise = self.pos.y - a.line_y;
        if rise > 20.0 {
            if dx.abs() > 240.0 {
                // Get closer before jumping.
                self.walk_to(a.at.x - dx.signum() * 120.0, true);
                return true;
            }
            if rise <= MAX_JUMP_RISE {
                self.jump_to(v2(a.at.x, a.line_y));
                return true;
            }
            if self.traits.powers && rise <= SUPER_RISE {
                self.super_jump_to(v2(a.at.x, a.line_y));
                return true;
            }
            // Too high: climb a step toward it.
            if let Some(i) =
                world.platform_above(self.pos, MAX_JUMP_RISE, a.at.x - 200.0, a.at.x + 200.0)
            {
                let p = world.platforms()[i];
                self.jump_to(v2(clamp_between(a.at.x, p.x0 + 20.0, p.x1 - 20.0), p.y));
                return true;
            }
            return false;
        }
        if rise < -2.0 && ground.kind != PlatformKind::Ground {
            // Below us: walk over it and drop through this platform.
            if dx.abs() > 8.0 && ground.contains_x(a.at.x) {
                self.walk_to(a.at.x, false);
            } else {
                self.pos.y += 2.0; // just under our platform, so landing skips it
                self.ground = None;
                self.vel = V2::ZERO;
                self.set_action(Action::Fall);
            }
            return true;
        }
        false
    }

    fn sabotage(&mut self, s: &Senses, world: &World) {
        // Mischief with powers: pick up the user's window and toss it aside.
        if self.traits.powers
            && s.can_throw_window
            && self.tk_cooldown <= 0.0
            && !self.holding
            && self.decide_left <= 0.0
        {
            if self.random(0.0, 1.0) < 0.25 {
                self.tk_cooldown = 15.0;
                self.set_action(Action::Telekinesis { on: Mind::Window });
                return;
            }
            self.tk_cooldown = 3.0;
        }
        // Steal words from the page and fling them away.
        if self.holding {
            if self.decide_left <= 0.0 {
                let side = if self.random(0.0, 1.0) < 0.5 {
                    -1.0
                } else {
                    1.0
                };
                let target =
                    self.pos + v2(side * self.random(250.0, 600.0), -self.random(150.0, 400.0));
                self.face(side);
                self.decide_left = self.random(0.5, 1.2);
                self.set_action(Action::Throw { target });
            }
            return;
        }
        if let Some(a) = s.ammo {
            if self.fetch(a, world) {
                return;
            }
        }
        let Some((id, win)) = s.active_window else {
            return self.wander(s, world, &mut Vec::new());
        };
        let ground = self.ground.unwrap_or(Platform::ground(0.0, self.pos.y));
        if ground.kind == PlatformKind::Window && ground.owner == id {
            if self.decide_left <= 0.0 {
                self.decide_left = self.random(0.7, 1.6);
                let roll = self.random(0.0, 1.0);
                if roll < 0.25 {
                    let x = self.random(ground.x0 + 20.0, ground.x1 - 20.0);
                    self.walk_to(x, false);
                } else if roll < 0.5 && s.can_shove {
                    // Kick the window across the screen (and ride it).
                    let dir = if self.random(0.0, 1.0) < 0.5 {
                        -1.0
                    } else {
                        1.0
                    };
                    let dx = dir * self.random(90.0, 170.0);
                    self.face(-dir); // kick backwards to push it the other way
                    self.set_action(Action::Shove { dx });
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
            Some(p)
                if self.pos.y - top
                    <= if self.traits.powers {
                        SUPER_RISE
                    } else {
                        MAX_JUMP_RISE
                    }
                    && top < self.pos.y - 20.0 =>
            {
                let launch = clamp_between(self.pos.x, p.x0 + 40.0, p.x1 - 40.0);
                let launch = clamp_between(launch, ground.x0 + 10.0, ground.x1 - 10.0);
                if (launch - self.pos.x).abs() > 25.0 {
                    self.walk_to(launch, false);
                } else {
                    let land_x =
                        clamp_between(self.pos.x, p.x0 + 30.0, p.x1 - 30.0) + self.facing * 10.0;
                    let target = v2(clamp_between(land_x, p.x0 + 20.0, p.x1 - 20.0), p.y);
                    if self.pos.y - top > MAX_JUMP_RISE {
                        self.super_jump_to(target);
                    } else {
                        self.jump_to(target);
                    }
                }
            }
            _ => {
                // Out of reach: stand under it and shake a fist.
                let under = clamp_between(win.x as f32 + win.width as f32 / 2.0, s.min_x, s.max_x);
                if (under - self.pos.x).abs() > 60.0 {
                    self.walk_to(under, false);
                } else if !matches!(self.action, Action::Taunt) {
                    self.set_action(Action::Taunt);
                }
            }
        }
    }

    fn wander(&mut self, s: &Senses, world: &World, events: &mut Vec<Event>) {
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
                let x = clamp_between(self.pos.x, p.x0 + 30.0, p.x1 - 30.0);
                if (x - self.pos.x).abs() < 200.0 {
                    return self.jump_to(v2(x, p.y));
                }
            } else if self.traits.powers && roll < 0.05 {
                // Nothing in reach: show off with a super jump to somewhere high.
                if let Some(i) = world.platform_above(
                    self.pos,
                    SUPER_RISE,
                    self.pos.x - 300.0,
                    self.pos.x + 300.0,
                ) {
                    let p = world.platforms()[i];
                    let x = clamp_between(self.pos.x, p.x0 + 30.0, p.x1 - 30.0);
                    return self.super_jump_to(v2(x, p.y));
                }
            }
        }
        if (0.25..0.37).contains(&roll) && ground.kind == PlatformKind::Ground {
            // Another screen next door: go and visit it.
            let (left, right) = (ground.x0 - s.min_x, s.max_x - ground.x1);
            if left > 150.0 || right > 150.0 {
                let x = if right >= left {
                    ground.x1 + self.random(80.0, right.min(600.0))
                } else {
                    ground.x0 - self.random(80.0, left.min(600.0))
                };
                return self.walk_to(clamp_between(x, s.min_x, s.max_x), false);
            }
        }
        if (0.15..0.25).contains(&roll) && ground.kind != PlatformKind::Ground {
            // Wander off the edge of a window and drop down.
            let edge = if self.random(0.0, 1.0) < 0.5 {
                ground.x0 - 40.0
            } else {
                ground.x1 + 40.0
            };
            return self.walk_to(clamp_between(edge, s.min_x, s.max_x), false);
        }
        if (0.37..0.49).contains(&roll)
            && self.traits.sitting
            && ground.kind != PlatformKind::Ground
            && ground.width() > 120.0
        {
            // Go and sit on the nearer edge, facing out over it.
            let (left, right) = (ground.x0 + 6.0, ground.x1 - 6.0);
            let to = if self.pos.x - left < right - self.pos.x {
                left
            } else {
                right
            };
            if (to - self.pos.x).abs() > 3.0 {
                self.walk_to(to, false);
                self.sit_plan = true;
                return;
            }
        }
        if (0.49..0.54).contains(&roll) && self.traits.dancing {
            self.set_action(Action::Dance);
            events.push(Event::StartedDancing { at: self.head() });
            return;
        }
        if (0.54..0.6).contains(&roll) && self.traits.doodles && s.mode == Mode::Idle {
            // Doodle in the air in front of it, if there's room on screen.
            // Just beyond arm's reach, so the pen in its hand can trace it.
            let size = 20.0 * (self.scale / 0.55);
            let origin = self.shoulder() + v2(self.facing * (self.reach() + size + 4.0), -8.0);
            let room = origin.x - size > s.min_x
                && origin.x + size < s.max_x
                && origin.y - size > s.top + 40.0;
            if room {
                let i =
                    (self.random(0.0, Shape::ALL.len() as f32) as usize).min(Shape::ALL.len() - 1);
                let shape = Shape::ALL[i];
                self.set_action(Action::Doodle {
                    shape,
                    origin,
                    size,
                });
                events.push(Event::DoodleStarted {
                    shape,
                    origin,
                    size,
                });
                return;
            }
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
                let period = self.gait_period(run);
                self.gait_phase = (self.gait_phase + dt / period * ramp).rem_euclid(1.0);
                self.pos.x += self.facing * self.walk_speed(run) * ramp * dt;
                if self.pos.x <= s.min_x || self.pos.x >= s.max_x {
                    self.pos.x = clamp_between(self.pos.x, s.min_x, s.max_x);
                    self.set_action(Action::Stand);
                }
            }
            self.pos.y = ground.y;
            if ground.kind == PlatformKind::Trampoline && self.bounces < 3 {
                // Stepped onto a trampoline: up it goes.
                self.bounces += 1;
                let vx = match self.action {
                    Action::Walk { run, .. } => self.facing * self.walk_speed(run),
                    _ => 0.0,
                };
                self.vel = v2(vx, -1250.0);
                self.ground = None;
                self.super_air = true;
                self.set_action(Action::Air);
                events.push(Event::Boing { at: self.pos });
                return;
            }
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
        let ceiling = s.top + 40.0 + 230.0 * self.scale;
        if next.y < ceiling {
            next.y = ceiling;
            self.vel.y = self.vel.y.abs() * 0.3;
        }
        if next.x < s.min_x || next.x > s.max_x {
            next.x = clamp_between(next.x, s.min_x, s.max_x);
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
            if p.kind == PlatformKind::Trampoline && self.bounces < 3 {
                // Boing: up again, higher than it came down.
                self.bounces += 1;
                self.vel = v2(self.vel.x * 0.9, -(impact * 1.05).clamp(1150.0, 2200.0));
                self.pos = next - v2(0.0, 0.5);
                self.super_air = true;
                if self.action != Action::Tumble {
                    self.set_action(Action::Air);
                }
                events.push(Event::Boing { at: next });
                return;
            }
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
            self.air_attack = false;
            self.super_air = false;
            if p.kind != PlatformKind::Trampoline {
                self.bounces = 0;
            }
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
        // Safety net: never lose the figure below the screen. Use the ground
        // of the screen it's over (several screens, several grounds).
        let grounds = || {
            world
                .platforms()
                .iter()
                .filter(|p| p.kind == PlatformKind::Ground)
        };
        let under = grounds()
            .find(|p| p.contains_x(self.pos.x))
            .or_else(|| grounds().next());
        if let Some(g) = under {
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
            top: 0.0,
            ammo: None,
            can_grab_cursor: false,
            held: false,
            user_idle: 0.0,
            can_shove: false,
            tk_words_near: 0,
            music: false,
            can_throw_window: false,
            weapon_available: false,
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

    fn text_line(x0: f32, x1: f32, y: f32) -> Platform {
        Platform {
            x0,
            x1,
            y,
            kind: PlatformKind::Text,
            owner: 77,
        }
    }

    #[test]
    fn lob_reaches_its_target() {
        for (from, to) in [
            (v2(100.0, 800.0), v2(600.0, 500.0)),
            (v2(900.0, 700.0), v2(300.0, 750.0)),
            (v2(0.0, 0.0), v2(60.0, -40.0)),
        ] {
            let mut p = from;
            let mut v = lob_velocity(from, to);
            let mut best = f32::MAX;
            for _ in 0..240 {
                v.y += PROJECTILE_GRAVITY / 240.0;
                p += v / 240.0;
                best = best.min((p - to).length());
            }
            assert!(best < 6.0, "lob from {from:?} missed {to:?} by {best}");
        }
    }

    #[test]
    fn combat_grabs_a_word_underfoot_and_pelts_the_cursor() {
        let line = text_line(500.0, 1100.0, 800.0);
        let w = world(vec![line]);
        let mut a = Agent::new(700.0, line, SCALE, 5);
        let mut s = senses(Mode::Combat);
        s.cursor = v2(1400.0, 450.0);
        s.ammo = Some(Ammo {
            id: 9,
            at: v2(850.0, 808.0),
            line_y: 800.0,
        });
        let mut ev = vec![];
        for _ in 0..(5.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            if ev.iter().any(|e| matches!(e, Event::Grabbed { .. })) {
                s.ammo = None; // the word is gone from the page now
            }
        }
        let grabbed = ev
            .iter()
            .position(|e| *e == Event::Grabbed { id: 9 })
            .expect("never grabbed the word");
        let threw = ev
            .iter()
            .position(|e| matches!(e, Event::Threw { .. }))
            .expect("never threw it");
        assert!(threw > grabbed);
        if let Event::Threw { from, vel } = ev[threw] {
            assert!(vel.x > 0.0, "thrown away from the cursor: {vel:?}");
            assert!((from - a.pos).length() < 200.0);
        }
        assert!(!a.holding);
    }

    #[test]
    fn jumps_up_to_a_text_line_for_ammo() {
        let line = text_line(600.0, 1200.0, 900.0);
        let w = world(vec![line]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(200.0, 300.0);
        s.ammo = Some(Ammo {
            id: 3,
            at: v2(950.0, 908.0),
            line_y: 900.0,
        });
        let mut ev = vec![];
        run(&mut a, &w, &s, 6.0, &mut ev);
        assert!(
            ev.iter().any(|e| matches!(e, Event::Jumped { .. })),
            "never jumped"
        );
        assert!(
            ev.contains(&Event::Grabbed { id: 3 }),
            "never grabbed after jumping up"
        );
    }

    #[test]
    fn drops_the_word_when_hit() {
        let w = world(vec![]);
        let mut a = agent();
        a.holding = true;
        let mut s = senses(Mode::Idle);
        s.cursor = a.center() + v2(-15.0, 0.0);
        s.clicked = true;
        let mut ev = vec![];
        a.update(&s, &w, &mut ev);
        assert!(!a.holding);
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { .. })));
    }

    #[test]
    fn sabotage_steals_words_and_flings_them() {
        let line = text_line(500.0, 1100.0, 800.0);
        let w = world(vec![line]);
        let mut a = Agent::new(800.0, line, SCALE, 8);
        let mut s = senses(Mode::Sabotage);
        s.ammo = Some(Ammo {
            id: 4,
            at: v2(760.0, 808.0),
            line_y: 800.0,
        });
        let mut ev = vec![];
        for _ in 0..(6.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            if ev.iter().any(|e| matches!(e, Event::Grabbed { .. })) {
                s.ammo = None;
            }
        }
        assert!(ev.contains(&Event::Grabbed { id: 4 }));
        assert!(ev.iter().any(|e| matches!(e, Event::Threw { .. })));
    }

    /// Hold a slow cursor in reach until the figure snatches it.
    fn snatched(seed: u64) -> (Agent, World, Senses, Vec<Event>) {
        let w = world(vec![]);
        let mut a = Agent::new(900.0, Platform::ground(1800.0, GROUND_Y), SCALE, seed);
        let mut s = senses(Mode::Combat);
        s.can_grab_cursor = true;
        s.cursor = v2(960.0, GROUND_Y - 100.0);
        let mut ev = vec![];
        for _ in 0..(20.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            if a.holding_cursor {
                break;
            }
        }
        (a, w, s, ev)
    }

    #[test]
    fn combat_snatches_a_slow_cursor_and_throws_it() {
        let (mut a, w, mut s, mut ev) = snatched(21);
        assert!(a.holding_cursor, "never caught the cursor");
        assert!(ev.iter().any(|e| matches!(e, Event::CaughtCursor { .. })));
        let mut threw = None;
        for _ in 0..(3.0 / FIXED_DT) as usize {
            // The app pins the pointer to the hands while held.
            if a.holding_cursor {
                s.cursor = a.hands();
            }
            let n = ev.len();
            a.update(&s, &w, &mut ev);
            if let Some(Event::ThrewCursor { from, vel }) = ev[n..]
                .iter()
                .find(|e| matches!(e, Event::ThrewCursor { .. }))
            {
                threw = Some((*from, *vel));
            }
        }
        let (from, vel) = threw.expect("held the cursor but never threw it");
        assert!(!a.holding_cursor);
        assert!(vel.y < -300.0, "a high lob: {vel:?}");
        assert!(vel.x.abs() > 300.0, "thrown far: {vel:?}");
        assert!((from - a.pos).length() < 200.0);
    }

    #[test]
    fn never_snatches_when_not_allowed() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(960.0, GROUND_Y - 100.0);
        let mut ev = vec![];
        for _ in 0..(20.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            assert!(!matches!(a.action, Action::SnatchCursor { .. }));
        }
        assert!(ev.iter().any(|e| matches!(e, Event::StruckCursor { .. })));
    }

    #[test]
    fn yanking_free_or_clicking_makes_it_let_go() {
        let (mut a, w, s, mut ev) = snatched(21);
        assert!(a.holding_cursor);
        run(&mut a, &w, &s, 0.1, &mut ev);
        a.holding_cursor = false; // the app noticed the user pull away
        a.update(&s, &w, &mut ev);
        assert_eq!(a.action, Action::Stand);
        let (mut a, w, mut s, mut ev) = snatched(21);
        s.cursor = a.center();
        s.clicked = true;
        a.update(&s, &w, &mut ev);
        assert!(!a.holding_cursor);
        assert_eq!(a.action, Action::Tumble);
    }

    #[test]
    fn wanders_onto_a_lower_screen_next_door() {
        // Laptop ground at 1100 on 0..1800, a monitor to the right whose floor
        // is lower (1300).
        let mut w = World::new();
        w.set_platforms(vec![
            Platform::ground(1800.0, GROUND_Y),
            Platform {
                x0: 1800.0,
                x1: 4360.0,
                y: 1300.0,
                kind: PlatformKind::Ground,
                owner: 0,
            },
        ]);
        for seed in [77, 5, 1234] {
            let mut a = Agent::new(1500.0, Platform::ground(1800.0, GROUND_Y), SCALE, seed);
            let mut s = senses(Mode::Idle);
            s.max_x = 4330.0;
            let mut ev = vec![];
            let mut crossed = false;
            for _ in 0..(240.0 / FIXED_DT) as usize {
                a.update(&s, &w, &mut ev);
                if a.ground.is_some_and(|g| g.y == 1300.0) {
                    crossed = true;
                    break;
                }
            }
            assert!(
                crossed,
                "seed {seed}: never visited the other screen ({:?})",
                a.pos
            );
            assert!(a.pos.x > 1800.0 && a.pos.y == 1300.0);
        }
    }

    #[test]
    fn stays_on_its_own_text_line_when_text_is_re_read() {
        // The figure stands on the third of four lines; OCR re-reads them with
        // a little jitter, top line first.
        let lines = |dy: f32| {
            vec![
                Platform::ground(1800.0, GROUND_Y),
                text_line(500.0, 1100.0, 600.0 + dy),
                text_line(500.0, 1100.0, 640.0 - dy),
                text_line(500.0, 1100.0, 680.0 + dy),
                text_line(500.0, 1100.0, 720.0),
            ]
        };
        let mut w = world(vec![]);
        w.set_platforms(lines(0.0));
        let mut a = Agent::new(800.0, text_line(500.0, 1100.0, 680.0), SCALE, 3);
        for dy in [1.0, -1.5, 2.0, 0.0] {
            w.set_platforms(lines(dy));
            a.on_platforms_changed(&w);
            assert_eq!(a.pos.y, 680.0 + dy, "jumped to another line");
            assert!(a.ground.is_some());
        }
        // The page scrolled: its line is gone from under it, so it falls rather
        // than teleporting onto whichever line is now nearby.
        w.set_platforms(vec![
            Platform::ground(1800.0, GROUND_Y),
            text_line(500.0, 1100.0, 650.0),
            text_line(500.0, 1100.0, 710.0),
        ]);
        a.on_platforms_changed(&w);
        assert_eq!(a.action, Action::Fall);
        let mut ev = vec![];
        run(&mut a, &w, &senses(Mode::Idle), 1.0, &mut ev);
        assert_eq!(a.pos.y, 710.0, "landed on the line below");
    }

    #[test]
    fn dragged_figure_hangs_from_the_pointer_and_flies_when_let_go() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        let mut ev = vec![];
        run(&mut a, &w, &s, 0.3, &mut ev);
        // Press on its belly and drag it up and to the left.
        s.cursor = a.center();
        s.held = true;
        a.update(&s, &w, &mut ev);
        assert_eq!(a.action, Action::Dangled);
        assert!(ev.iter().any(|e| matches!(e, Event::PickedUp { .. })));
        for i in 0..60 {
            s.cursor += v2(-6.0, -4.0);
            s.cursor_vel = v2(-360.0, -240.0);
            a.update(&s, &w, &mut ev);
            if i > 40 {
                // Hanging just under the pointer by now.
                let below = a.head().y - s.cursor.y;
                // Held by the scruff of the neck: the head sits just around the pointer.
                assert!(
                    (-35.0..40.0).contains(&below),
                    "head {below} pt below pointer"
                );
                assert!((a.head().x - s.cursor.x).abs() < 40.0);
            }
        }
        assert!(a.ground.is_none());
        // Swing it hard to the right and let go.
        s.cursor_vel = v2(2000.0, -300.0);
        a.update(&s, &w, &mut ev);
        s.held = false;
        a.update(&s, &w, &mut ev);
        assert_eq!(a.action, Action::Tumble);
        assert!(a.vel.x > 1500.0, "{:?}", a.vel);
        assert!(a.health < 100.0, "a hard throw hurts");
        assert!(ev.iter().any(|e| matches!(e, Event::Flung { .. })));
        s.cursor_vel = V2::ZERO;
        run(&mut a, &w, &s, 5.0, &mut ev);
        assert!(
            a.ground.is_some() && !a.action.airborne() && a.action != Action::Tumble,
            "{:?}",
            a.action
        );
    }

    #[test]
    fn gently_set_down_just_drops() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        let mut ev = vec![];
        s.cursor = a.center();
        s.held = true;
        run(&mut a, &w, &s, 0.5, &mut ev);
        s.held = false;
        a.update(&s, &w, &mut ev);
        assert_eq!(a.action, Action::Fall);
        assert_eq!(a.health, 100.0);
    }

    #[test]
    fn leaps_and_flying_kicks_a_cursor_overhead() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(930.0, GROUND_Y - 300.0);
        let mut ev = vec![];
        let (mut kicked, mut landed_after) = (false, false);
        for _ in 0..(6.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            kicked |= matches!(a.action, Action::FlyingKick { .. });
            landed_after |= kicked && a.ground.is_some();
        }
        assert!(kicked, "never flying-kicked");
        assert!(
            ev.iter().any(|e| matches!(e, Event::StruckCursor { .. })),
            "kick missed"
        );
        assert!(landed_after, "never came back down");
    }

    #[test]
    fn mixes_in_word_throws_when_punches_dont_move_the_cursor() {
        // Standing on a text line right under a cursor that never moves.
        let line = text_line(500.0, 1100.0, 800.0);
        let w = world(vec![line]);
        let mut a = Agent::new(700.0, line, SCALE, 5);
        let mut s = senses(Mode::Combat);
        s.cursor = v2(740.0, 800.0 - 110.0);
        s.ammo = Some(Ammo {
            id: 9,
            at: v2(705.0, 806.0),
            line_y: 800.0,
        });
        let mut ev = vec![];
        run(&mut a, &w, &s, 6.0, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::StruckCursor { .. })));
        assert!(ev.contains(&Event::Grabbed { id: 9 }), "only ever punched");
    }

    #[test]
    fn never_just_taunts_at_a_cursor_slightly_overhead() {
        // A cursor ~2 head-heights above the shoulder: out of punching reach,
        // below the old leap threshold. It used to taunt forever.
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(940.0, GROUND_Y - 220.0);
        let mut ev = vec![];
        run(&mut a, &w, &s, 4.0, &mut ev);
        assert!(
            ev.iter().any(|e| matches!(e, Event::Jumped { .. })),
            "never leapt"
        );
        assert!(
            ev.iter().any(|e| matches!(e, Event::StruckCursor { .. })),
            "never hit it"
        );
    }

    #[test]
    fn naps_while_the_user_is_away_and_wakes_when_they_return() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        s.user_idle = SLEEP_AFTER + 1.0;
        let mut ev = vec![];
        run(&mut a, &w, &s, 8.0, &mut ev);
        assert_eq!(a.action, Action::Sleep);
        assert!(ev.iter().any(|e| matches!(e, Event::Yawned { .. })));
        // Lying down: the whole body is close to the ground.
        let top = a
            .segments()
            .iter()
            .map(|g| g.start[1].min(g.end[1]))
            .fold(f32::MAX, f32::min);
        assert!(GROUND_Y - top < 70.0, "not lying down (top {top})");
        assert!(a.action.is_still(), "sleeping should be cheap");
        // Mouse moves: up it gets.
        s.user_idle = 0.0;
        a.update(&s, &w, &mut ev);
        assert_eq!(a.action, Action::GetUp);
        assert!(ev.iter().any(|e| matches!(e, Event::Woke { .. })));
        run(&mut a, &w, &s, 1.5, &mut ev);
        assert!(a.action.interruptible() && (feet_y(&a) - GROUND_Y).abs() < 1.0);
    }

    #[test]
    fn stroking_it_slowly_makes_it_happy() {
        let w = world(vec![]);
        let mut a = agent();
        a.frustration = 60.0;
        a.health = 50.0;
        a.hold_still = true;
        let mut s = senses(Mode::Idle);
        let mut ev = vec![];
        for i in 0..90 {
            // Back and forth over the belly at ~300 pt/s.
            let k = (i as f32 * 0.25).sin();
            s.cursor = a.center() + v2(12.0 * k, 0.0);
            s.cursor_vel = v2(300.0 * (i as f32 * 0.25).cos(), 0.0);
            a.update(&s, &w, &mut ev);
        }
        assert!(
            ev.iter().any(|e| matches!(e, Event::Petted { .. })),
            "no hearts"
        );
        assert!(a.frustration < 25.0 && a.health > 60.0);
        // A fast swipe is not petting.
        let mut b = agent();
        b.hold_still = true;
        s.cursor = b.center();
        s.cursor_vel = v2(2000.0, 0.0);
        let mut ev2 = vec![];
        run(&mut b, &w, &s, 1.5, &mut ev2);
        assert!(!ev2.iter().any(|e| matches!(e, Event::Petted { .. })));
    }

    #[test]
    fn sits_on_window_edges_dances_and_doodles_now_and_then() {
        let top = Platform {
            x0: 600.0,
            x1: 1200.0,
            y: 800.0,
            kind: PlatformKind::Window,
            owner: 9,
        };
        let w = world(vec![top]);
        let (mut sat, mut danced, mut doodled) = (false, false, false);
        for seed in [1u64, 2, 3, 4, 5, 6] {
            let mut a = Agent::new(900.0, top, SCALE, seed);
            let s = senses(Mode::Idle);
            let mut ev = vec![];
            for _ in 0..(90.0 / FIXED_DT) as usize {
                a.update(&s, &w, &mut ev);
                if a.action == Action::Sit && a.t > 0.5 && a.ground == Some(top) {
                    sat = true;
                    // On the edge, facing out, legs hanging below the surface.
                    let edge_gap = (a.pos.x - 600.0).abs().min((a.pos.x - 1200.0).abs());
                    assert!(edge_gap < 10.0, "sitting {edge_gap} pt from the edge");
                    assert!(feet_y(&a) > top.y + 15.0, "legs don't dangle");
                    let hip = a.segments()[0].start[1];
                    assert!((hip - top.y).abs() < 6.0, "hip at {hip}, edge at {}", top.y);
                }
            }
            danced |= ev.iter().any(|e| matches!(e, Event::StartedDancing { .. }));
            doodled |= ev.iter().any(|e| matches!(e, Event::DoodleStarted { .. }));
        }
        assert!(sat, "never sat on the edge");
        assert!(danced, "never danced");
        assert!(doodled, "never doodled");
    }

    #[test]
    fn doodle_pen_traces_the_shape_beside_it() {
        let w = world(vec![]);
        let mut a = agent();
        let origin = a.shoulder() + v2(a.reach() + 30.0, -8.0);
        a.set_action(Action::Doodle {
            shape: Shape::Star,
            origin,
            size: 26.0,
        });
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        let mut far = 0.0f32;
        for _ in 0..((DOODLE_LEAD + DOODLE_DRAW) / FIXED_DT) as usize + 1 {
            a.update(&s, &w, &mut ev);
            let (hand, pen) = a.pen_stroke().expect("drawing");
            if a.t < DOODLE_LEAD {
                continue; // arm still coming up; nothing inked yet
            }
            far = far.max((pen - origin).length());
            assert!(
                (hand - pen).length() < 60.0,
                "pen {} pt from the hand: hand {hand:?} pen {pen:?} shoulder {:?} t {}",
                (hand - pen).length(),
                a.shoulder(),
                a.t
            );
        }
        assert!(far > 20.0, "pen barely moved");
        assert!((a.doodle_progress().unwrap() - 1.0).abs() < 0.02);
    }

    #[test]
    fn shoves_the_window_and_rides_it_sideways() {
        let top = Platform {
            x0: 700.0,
            x1: 1300.0,
            y: 850.0,
            kind: PlatformKind::Window,
            owner: 42,
        };
        let mut w = world(vec![top]);
        let mut a = Agent::new(1000.0, top, SCALE, 4);
        let mut s = senses(Mode::Sabotage);
        s.can_shove = true;
        s.active_window = Some((
            42,
            Rect {
                x: 700.0,
                y: 850.0,
                width: 600.0,
                height: 300.0,
            },
        ));
        let mut ev = vec![];
        run(&mut a, &w, &s, 15.0, &mut ev);
        let dx = ev
            .iter()
            .find_map(|e| match e {
                Event::ShoveWindow { window: 42, dx, .. } => Some(*dx),
                _ => None,
            })
            .expect("never shoved the window");
        assert!((90.0..=170.0).contains(&dx.abs()));
        // The window moves: the figure moves with it.
        let x = a.pos.x;
        w.set_platforms(vec![
            Platform::ground(1800.0, GROUND_Y),
            Platform {
                x0: top.x0 + dx,
                x1: top.x1 + dx,
                ..top
            },
        ]);
        a.on_platforms_changed(&w);
        assert!((a.pos.x - (x + dx)).abs() < 1e-3, "didn't ride along");
        assert!(a.ground.is_some());
        // Without permission it only stomps.
        let mut b = Agent::new(1000.0, top, SCALE, 4);
        s.can_shove = false;
        let mut ev2 = vec![];
        run(&mut b, &world(vec![top]), &s, 15.0, &mut ev2);
        assert!(!ev2.iter().any(|e| matches!(e, Event::ShoveWindow { .. })));
    }

    #[test]
    fn super_jumps_at_a_cursor_way_up_high() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(960.0, GROUND_Y - 800.0);
        let mut ev = vec![];
        let mut peak = 0.0f32;
        let mut charged = false;
        for _ in 0..(5.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            peak = peak.max(GROUND_Y - a.pos.y);
            charged |= a.charging();
        }
        assert!(charged, "never powered up");
        assert!(ev.iter().any(|e| matches!(e, Event::SuperJumped { .. })));
        assert!(peak > MAX_JUMP_RISE + 200.0, "only rose {peak}");
        // Without powers: an ordinary jump.
        let mut b = agent();
        b.traits.powers = false;
        let mut ev2 = vec![];
        let mut peak2 = 0.0f32;
        for _ in 0..(5.0 / FIXED_DT) as usize {
            b.update(&s, &w, &mut ev2);
            peak2 = peak2.max(GROUND_Y - b.pos.y);
        }
        assert!(!ev2.iter().any(|e| matches!(e, Event::SuperJumped { .. })));
        assert!(peak2 <= MAX_JUMP_RISE + 60.0, "rose {peak2} without powers");
    }

    #[test]
    fn telekinesis_lifts_and_flings_the_cursor_from_afar() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.can_grab_cursor = true;
        s.cursor = v2(1400.0, GROUND_Y - 150.0);
        let mut ev = vec![];
        let mut lifted = false;
        let mut flung = None;
        for _ in 0..(30.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            lifted |= ev.iter().any(|e| matches!(e, Event::MindLiftCursor { .. }));
            if let Some(Event::MindFlingCursor { vel, .. }) = ev
                .iter()
                .find(|e| matches!(e, Event::MindFlingCursor { .. }))
            {
                flung = Some(*vel);
                break;
            }
        }
        assert!(lifted, "never lifted the cursor");
        let vel = flung.expect("never flung it");
        assert!(vel.x > 500.0 && vel.y < 0.0, "flung away and up: {vel:?}");
        assert!((a.pos.x - 900.0).abs() < 600.0 || a.telekinesis().is_none());
    }

    #[test]
    fn telekinesis_raises_words_and_fires_them() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.cursor = v2(1500.0, GROUND_Y - 300.0);
        s.tk_words_near = 6;
        let mut ev = vec![];
        run(&mut a, &w, &s, 30.0, &mut ev);
        let lift = ev
            .iter()
            .position(|e| matches!(e, Event::MindLiftWords { .. }));
        let fire = ev
            .iter()
            .position(|e| matches!(e, Event::MindFireWords { .. }));
        assert!(lift.is_some() && fire > lift, "lift {lift:?} fire {fire:?}");
        // Powers off: never.
        let mut b = agent();
        b.traits.powers = false;
        let mut ev2 = vec![];
        run(&mut b, &w, &s, 30.0, &mut ev2);
        assert!(!ev2.iter().any(|e| matches!(e, Event::MindLiftWords { .. })));
    }

    #[test]
    fn dances_while_music_plays() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Idle);
        s.music = true;
        let mut ev = vec![];
        let mut dancing = 0;
        for _ in 0..(20.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            dancing += usize::from(a.action == Action::Dance);
        }
        assert!(
            dancing as f32 * FIXED_DT > 15.0,
            "danced only {:.1} s",
            dancing as f32 * FIXED_DT
        );
        let started = ev
            .iter()
            .filter(|e| matches!(e, Event::StartedDancing { .. }))
            .count();
        assert_eq!(started, 1, "announced once");
        // Music stops: back to normal life.
        s.music = false;
        run(&mut a, &w, &s, 8.0, &mut ev);
        assert_ne!(a.action, Action::Dance);
    }

    #[test]
    fn trampolines_bounce_it_high_then_it_sticks_the_landing() {
        let tramp = Platform {
            x0: 850.0,
            x1: 950.0,
            y: GROUND_Y - 10.0,
            kind: PlatformKind::Trampoline,
            owner: 0x7000_0001,
        };
        let w = world(vec![tramp]);
        let mut a = agent();
        a.pos = v2(900.0, GROUND_Y - 300.0);
        a.ground = None;
        a.set_action(Action::Fall);
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        let mut peak = 0.0f32;
        for _ in 0..(1.5 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            if ev.iter().any(|e| matches!(e, Event::Boing { .. })) {
                peak = peak.max(GROUND_Y - a.pos.y);
            }
        }
        assert!(
            ev.iter().any(|e| matches!(e, Event::Boing { .. })),
            "no bounce"
        );
        assert!(peak > 300.0, "bounced only {peak} pt");
        run(&mut a, &w, &s, 8.0, &mut ev);
        let boings = ev
            .iter()
            .filter(|e| matches!(e, Event::Boing { .. }))
            .count();
        assert!(boings >= 3, "{boings} bounces");
        assert!(a.ground.is_some(), "never stopped bouncing");
    }

    #[test]
    fn higher_levels_walk_faster_without_sliding() {
        let w = world(vec![]);
        let mut a = agent();
        a.traits.speed = 1.3;
        a.walk_to(1700.0, false);
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        run(&mut a, &w, &s, 0.5, &mut ev);
        let x0 = a.pos.x;
        let mut worst = 0.0f32;
        let mut prev = a.segments();
        let mut prev_phase = a.gait_phase;
        for _ in 0..60 {
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
        let fast = a.pos.x - x0;
        let mut b = agent();
        b.walk_to(1700.0, false);
        run(&mut b, &w, &s, 0.5, &mut ev);
        let x1 = b.pos.x;
        run(&mut b, &w, &s, 1.0, &mut ev);
        assert!(
            fast > (b.pos.x - x1) * 1.2,
            "not faster: {fast} vs {}",
            b.pos.x - x1
        );
    }

    #[test]
    fn telekinesis_throws_whole_windows_at_the_cursor() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.can_throw_window = true;
        s.cursor = v2(1500.0, GROUND_Y - 400.0);
        let mut ev = vec![];
        run(&mut a, &w, &s, 40.0, &mut ev);
        let lift = ev
            .iter()
            .position(|e| matches!(e, Event::MindLiftWindow { .. }));
        let throw = ev.iter().position(|e| {
            matches!(
                e,
                Event::MindThrowWindow {
                    target: Some(_),
                    ..
                }
            )
        });
        assert!(
            lift.is_some() && throw > lift,
            "lift {lift:?} throw {throw:?}"
        );
        // Not allowed: never.
        let mut b = agent();
        s.can_throw_window = false;
        let mut ev2 = vec![];
        run(&mut b, &w, &s, 40.0, &mut ev2);
        assert!(!ev2
            .iter()
            .any(|e| matches!(e, Event::MindLiftWindow { .. })));
    }

    #[test]
    fn arms_up_with_a_weapon_slashes_then_throws_it() {
        let w = world(vec![]);
        let mut a = agent();
        let mut s = senses(Mode::Combat);
        s.weapon_available = true;
        s.cursor = v2(990.0, GROUND_Y - 110.0);
        let mut ev = vec![];
        for _ in 0..(60.0 / FIXED_DT) as usize {
            a.update(&s, &w, &mut ev);
            if ev.iter().any(|e| matches!(e, Event::WantWeapon { .. })) {
                break;
            }
        }
        assert!(
            ev.iter().any(|e| matches!(e, Event::WantWeapon { .. })),
            "never reached for one"
        );
        // The app hands it a 90 pt scrollbar.
        a.weapon = Some(90.0);
        s.weapon_available = false;
        ev.clear();
        run(&mut a, &w, &s, 30.0, &mut ev);
        let cuts = ev
            .iter()
            .filter(|e| matches!(e, Event::Slashed { .. }))
            .count();
        assert!(cuts >= WEAPON_HITS as usize, "only {cuts} cuts");
        assert!(
            ev.iter().any(|e| matches!(e, Event::WeaponThrown { .. })),
            "never threw it"
        );
        assert!(a.weapon.is_none());
    }

    #[test]
    fn a_hit_knocks_the_weapon_out_of_its_hand() {
        let w = world(vec![]);
        let mut a = agent();
        a.weapon = Some(80.0);
        let mut s = senses(Mode::Idle);
        s.cursor = a.center();
        s.clicked = true;
        let mut ev = vec![];
        a.update(&s, &w, &mut ev);
        assert!(a.weapon.is_none());
        assert!(ev.iter().any(|e| matches!(e, Event::WeaponDropped { .. })));
    }

    /// Regression: a window edge narrower than the margins (72 pt, mostly
    /// covered by other windows) made `f32::clamp` panic during sabotage.
    #[test]
    fn narrow_platforms_never_crash_it() {
        for width in [30.0, 45.0, 59.0, 72.0] {
            let sliver = Platform {
                x0: 450.0,
                x1: 450.0 + width,
                y: 900.0,
                kind: PlatformKind::Window,
                owner: 42,
            };
            let w = world(vec![sliver]);
            for mode in [Mode::Sabotage, Mode::Idle, Mode::Combat] {
                for seed in [1u64, 2, 3] {
                    let mut a = Agent::new(400.0, Platform::ground(1800.0, GROUND_Y), SCALE, seed);
                    let mut s = senses(mode);
                    s.active_window = Some((
                        42,
                        Rect {
                            x: 450.0,
                            y: 900.0,
                            width: f64::from(width),
                            height: 150.0,
                        },
                    ));
                    s.cursor = v2(480.0, 700.0);
                    let mut ev = vec![];
                    run(&mut a, &w, &s, 20.0, &mut ev);
                }
            }
        }
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
    fn planted_foot_does_not_slide_while_running() {
        let w = world(vec![]);
        let mut a = agent();
        a.walk_to(1700.0, true);
        let s = senses(Mode::Idle);
        let mut ev = vec![];
        run(&mut a, &w, &s, 0.5, &mut ev);
        let duty = crate::stickman::RUN.duty;
        let mut worst = 0.0f32;
        let mut prev = a.segments();
        let mut prev_phase = a.gait_phase;
        for _ in 0..60 {
            a.update(&s, &w, &mut ev);
            let now = a.segments();
            for side in 0..2 {
                let q0 = (prev_phase + side as f32 * 0.5).rem_euclid(1.0);
                let q1 = (a.gait_phase + side as f32 * 0.5).rem_euclid(1.0);
                if q0 < duty && q1 < duty && q1 > q0 {
                    worst = worst.max((prev[SHIN[side]].end[0] - now[SHIN[side]].end[0]).abs());
                }
            }
            prev = now;
            prev_phase = a.gait_phase;
        }
        assert!(matches!(a.action, Action::Walk { run: true, .. }));
        assert!(
            worst < 0.05,
            "planted foot slid {worst} pt per tick while running"
        );
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
        let w = world(vec![top, text_line(650.0, 1150.0, 950.0)]);
        for (mode, seed) in [Mode::Idle, Mode::Observe, Mode::Sabotage, Mode::Combat]
            .into_iter()
            .flat_map(|m| [1234u64, 7, 99, 31337].map(|s| (m, s)))
        {
            let mut a = Agent::new(900.0, Platform::ground(1800.0, GROUND_Y), SCALE, seed);
            let mut s = senses(mode);
            s.can_grab_cursor = true;
            s.active_window = Some((
                9,
                Rect {
                    x: 600.0,
                    y: 850.0,
                    width: 600.0,
                    height: 300.0,
                },
            ));
            // Ammo that comes back after being thrown (exercises grab/carry/throw).
            let ammo = Ammo {
                id: 1,
                at: v2(900.0, 958.0),
                line_y: 950.0,
            };
            s.ammo = Some(ammo);
            let mut ev = vec![];
            let mut prev = a.segments();
            let mut prev_pos = a.pos;
            for i in 0..(30.0 / FIXED_DT) as usize {
                s.ammo = if a.holding { None } else { Some(ammo) };
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
                        "{mode:?}/seed {seed}: bone {b} jumped {d} pt at t={t:.2} in {:?}",
                        a.action
                    );
                }
                prev_pos = a.pos;
                prev = now;
            }
        }
    }
}
