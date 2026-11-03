//! The stick figure: StykAnimate's "Walker" rig, a procedural walk with planted
//! feet, an idle pose, and a small behaviour state machine.
//!
//! The rig is ported verbatim from StykAnimate's `src/demo.mjs`
//! (`defaultFigure()`, see docs/STYKANIMATE_ANALYSIS.md). StykAnimate's own demo
//! walk is authored *in place* (feet slide, knees bend backwards when the figure
//! travels), so the walk here is our own gait: stance feet stay planted on the
//! ground and legs are solved with two-bone IK so knees always bend forward.
//!
//! Conventions match StykAnimate: angles are radians, *absolute* (not relative to
//! the parent bone), in a y-down screen space, so `-π/2` points up. The root
//! (parent 0) is the hip. Poses are authored facing right (+x).

use std::f32::consts::{FRAC_PI_2, PI, TAU};

pub const BONES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoneKind {
    Line,
    /// A circle drawn along the bone: diameter = length (the head).
    Circle,
}

#[derive(Debug, Clone, Copy)]
pub struct Bone {
    /// Parent bone id (1-based), or 0 for the root/hip.
    pub parent: usize,
    pub length: f32,
    pub rest_angle: f32,
    pub thickness: f32,
    pub kind: BoneKind,
}

const fn line(parent: usize, length: f32, rest_angle: f32, thickness: f32) -> Bone {
    Bone {
        parent,
        length,
        rest_angle,
        thickness,
        kind: BoneKind::Line,
    }
}

/// `defaultFigure()` from StykAnimate `src/demo.mjs`. Index i is bone id i + 1.
pub const WALKER: [Bone; BONES] = [
    line(0, 78.0, -FRAC_PI_2, 11.0), // 1 torso (hip -> neck)
    Bone {
        parent: 1,
        length: 44.0,
        rest_angle: -FRAC_PI_2,
        thickness: 8.0,
        kind: BoneKind::Circle,
    }, // 2 head
    line(1, 53.0, 1.05, 9.0),        // 3 upper arm A
    line(3, 48.0, 1.32, 8.0),        // 4 forearm A
    line(1, 53.0, 2.02, 9.0),        // 5 upper arm B
    line(5, 48.0, 1.65, 8.0),        // 6 forearm B
    line(0, 66.0, 1.12, 11.0),       // 7 thigh A
    line(7, 63.0, 1.52, 10.0),       // 8 shin A
    line(0, 66.0, 2.00, 11.0),       // 9 thigh B
    line(9, 63.0, 1.68, 10.0),       // 10 shin B
];

// Bone indices (0-based).
const TORSO: usize = 0;
const HEAD: usize = 1;
const UPPER_ARM: [usize; 2] = [2, 4];
const FOREARM: [usize; 2] = [3, 5];
const THIGH: [usize; 2] = [6, 8];
const SHIN: [usize; 2] = [7, 9];

/// StykAnimate figure colour `rgb(43, 43, 43)`.
pub const WALKER_RGB: [u8; 3] = [43, 43, 43];

/// Absolute angle per bone.
pub type Pose = [f32; BONES];

/// The figure's authored rest pose (`angle_rad` of each segment).
pub fn rest_pose() -> Pose {
    std::array::from_fn(|i| WALKER[i].rest_angle)
}

/// Reflect a pose left/right (used to turn around without a visible pop).
pub fn mirror_pose(p: &Pose) -> Pose {
    std::array::from_fn(|i| PI - p[i])
}

fn lerp_angle(a: f32, b: f32, k: f32) -> f32 {
    let d = (b - a + PI).rem_euclid(TAU) - PI; // shortest arc
    a + d * k
}

pub fn lerp_pose(a: &Pose, b: &Pose, k: f32) -> Pose {
    std::array::from_fn(|i| lerp_angle(a[i], b[i], k))
}

fn smoothstep(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

// --- Idle -----------------------------------------------------------------------

/// Standing pose with slow breathing and a little weight shift.
pub fn idle_pose(t: f32) -> Pose {
    let breath = (t * TAU / 3.2).sin();
    let sway = (t * TAU / 5.7).sin();
    let h = FRAC_PI_2;
    [
        -h + 0.03 + 0.008 * breath,
        -h + 0.05 + 0.015 * breath,
        h - 0.18 + 0.008 * breath,
        h - 0.32 + 0.012 * breath,
        h + 0.16 - 0.008 * breath,
        h + 0.02 - 0.012 * breath,
        h - 0.12 + 0.02 * sway,
        h - 0.05 + 0.02 * sway,
        h + 0.13 + 0.02 * sway,
        h + 0.08 + 0.02 * sway,
    ]
}

// --- Procedural walk --------------------------------------------------------------
// All lengths in source pixels (scale 1); the Figure scales everything.

const THIGH_LEN: f32 = 66.0;
const SHIN_LEN: f32 = 63.0;
const LEG_LEN: f32 = THIGH_LEN + SHIN_LEN;

/// Seconds for a full gait cycle (two steps).
pub const GAIT_PERIOD: f32 = 1.05;
/// Fraction of the cycle each foot spends planted (> 0.5 gives double support).
const DUTY: f32 = 0.6;
/// Planted foot travels from +R to -R relative to the hip.
const HALF_STANCE: f32 = 0.26 * LEG_LEN;
/// Hip height above the ankles and its bob (highest over the planted foot).
const HIP_HEIGHT: f32 = 0.955 * LEG_LEN;
const HIP_BOB: f32 = 0.018 * LEG_LEN;
/// Peak height of the swinging foot.
const FOOT_LIFT: f32 = 0.10 * LEG_LEN;

/// Body speed (source px/s) at which planted feet don't slide.
pub fn gait_speed() -> f32 {
    2.0 * HALF_STANCE / (DUTY * GAIT_PERIOD)
}

/// Ankle position relative to the hip for a foot at local phase `q` ∈ [0, 1):
/// (forward offset, height above the ground).
fn foot_track(q: f32) -> (f32, f32) {
    if q < DUTY {
        let u = q / DUTY;
        (HALF_STANCE * (1.0 - 2.0 * u), 0.0)
    } else {
        let u = (q - DUTY) / (1.0 - DUTY);
        // Ease the foot forward; lift peaks a bit early so the toe clears.
        let x = -HALF_STANCE + 2.0 * HALF_STANCE * smoothstep(u);
        let lift = FOOT_LIFT * (PI * u.powf(0.8)).sin().max(0.0);
        (x, lift)
    }
}

/// Two-bone IK: thigh/shin angles placing the ankle at `target` (relative to the
/// hip, y down) with the knee bending forward (+x).
fn leg_ik(target: [f32; 2]) -> (f32, f32) {
    let (a, b) = (THIGH_LEN, SHIN_LEN);
    let len = (target[0] * target[0] + target[1] * target[1]).sqrt();
    let d = len.clamp((a - b).abs() + 1e-3, a + b - 1e-3);
    let theta = target[1].atan2(target[0]);
    let alpha = ((a * a + d * d - b * b) / (2.0 * a * d))
        .clamp(-1.0, 1.0)
        .acos();
    let thigh = theta - alpha;
    let knee = [a * thigh.cos(), a * thigh.sin()];
    let ankle = [theta.cos() * d, theta.sin() * d];
    let shin = (ankle[1] - knee[1]).atan2(ankle[0] - knee[0]);
    (thigh, shin)
}

/// Walk pose at gait phase `p` ∈ [0, 1). Leg A plants at p = 0, leg B at p = 0.5.
pub fn walk_pose(p: f32) -> Pose {
    let p = p.rem_euclid(1.0);
    let bob = (2.0 * TAU * (p - DUTY / 2.0)).cos(); // +1 over a planted foot
    let hip = HIP_HEIGHT + HIP_BOB * bob;
    let mut pose = [0.0; BONES];
    for (side, offset) in [0.0f32, 0.5].into_iter().enumerate() {
        let (fx, lift) = foot_track((p + offset).rem_euclid(1.0));
        let (thigh, shin) = leg_ik([fx, hip - lift]);
        pose[THIGH[side]] = thigh;
        pose[SHIN[side]] = shin;
        // Arm swings opposite to the leg on the same side; elbow bends more
        // on the forward swing.
        let n = (fx / HALF_STANCE).clamp(-1.0, 1.0);
        pose[UPPER_ARM[side]] = FRAC_PI_2 + 0.38 * n;
        pose[FOREARM[side]] = pose[UPPER_ARM[side]] - 0.18 - 0.32 * (-n).max(0.0);
    }
    pose[TORSO] = -FRAC_PI_2 + 0.07 + 0.012 * bob;
    pose[HEAD] = -FRAC_PI_2 + 0.06;
    pose
}

// --- Forward kinematics -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: [f32; 2],
    pub end: [f32; 2],
    pub thickness: f32,
    pub kind: BoneKind,
}

/// Places every bone. `facing` is +1 (right, as authored) or -1 (mirrored).
pub fn forward_kinematics(
    pose: &Pose,
    root: [f32; 2],
    scale: f32,
    facing: f32,
) -> [Segment; BONES] {
    let mut ends = [[0.0f32; 2]; BONES];
    let mut out = [Segment {
        start: root,
        end: root,
        thickness: 0.0,
        kind: BoneKind::Line,
    }; BONES];
    // WALKER is ordered so every parent precedes its children.
    for (i, bone) in WALKER.iter().enumerate() {
        let start = if bone.parent == 0 {
            root
        } else {
            ends[bone.parent - 1]
        };
        let len = bone.length * scale;
        let end = [
            start[0] + facing * len * pose[i].cos(),
            start[1] + len * pose[i].sin(),
        ];
        ends[i] = end;
        out[i] = Segment {
            start,
            end,
            thickness: bone.thickness * scale,
            kind: bone.kind,
        };
    }
    out
}

/// How far below the hip the lowest point of the figure (foot cap) reaches.
pub fn depth_below_root(pose: &Pose, scale: f32) -> f32 {
    forward_kinematics(pose, [0.0, 0.0], scale, 1.0)
        .iter()
        .map(|s| s.start[1].max(s.end[1]) + s.thickness / 2.0)
        .fold(f32::MIN, f32::max)
}

// --- Behaviour ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Walk,
}

const BLEND_SECONDS: f32 = 0.3;

/// The on-screen agent: walks back and forth along the ground line, pausing to
/// idle now and then and turning around at the edges. Units are screen points.
pub struct Figure {
    pub state: State,
    pub x: f32,
    pub facing: f32,
    pub scale: f32,
    state_left: f32,
    clock: f32,
    gait_phase: f32,
    blend_from: Pose,
    blend_left: f32,
    speed: f32,
    rng: u64,
    hold_idle: bool,
}

impl Figure {
    pub fn new(x: f32, scale: f32, seed: u64) -> Self {
        let mut f = Figure {
            state: State::Walk,
            x,
            facing: 1.0,
            scale,
            state_left: 0.0,
            clock: 0.0,
            gait_phase: 0.0,
            blend_from: rest_pose(),
            blend_left: BLEND_SECONDS, // ease out of the rest pose on launch
            speed: gait_speed() * scale,
            rng: seed | 1,
            hold_idle: false,
        };
        f.state_left = f.random(5.0, 10.0);
        f
    }

    fn random(&mut self, lo: f32, hi: f32) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let r = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        lo + (hi - lo) * (r as f32 / (1u64 << 24) as f32)
    }

    fn target_pose(&self) -> Pose {
        match self.state {
            State::Walk => walk_pose(self.gait_phase),
            State::Idle => idle_pose(self.clock),
        }
    }

    fn blend_k(&self) -> f32 {
        smoothstep(1.0 - self.blend_left / BLEND_SECONDS)
    }

    pub fn pose(&self) -> Pose {
        let target = self.target_pose();
        if self.blend_left > 0.0 {
            lerp_pose(&self.blend_from, &target, self.blend_k())
        } else {
            target
        }
    }

    /// Diagnostics: stand idle forever (used to measure idle CPU).
    pub fn hold_idle(&mut self) {
        self.hold_idle = true;
        self.switch(State::Idle);
    }

    /// Frames per second worth rendering right now: full rate while moving,
    /// a low rate for slow idle breathing (PRD: < 5% CPU when idle).
    pub fn wanted_fps(&self) -> u32 {
        if self.state == State::Walk || self.blend_left > 0.0 {
            60
        } else {
            20
        }
    }

    fn switch(&mut self, to: State) {
        self.blend_from = self.pose();
        self.blend_left = BLEND_SECONDS;
        self.state = to;
        self.clock = 0.0;
        self.gait_phase = 0.0;
        self.state_left = match to {
            State::Walk => self.random(5.0, 12.0),
            State::Idle => self.random(1.5, 4.0),
        };
    }

    /// Flip facing; the current pose is mirrored so the turn blends smoothly.
    fn turn_around(&mut self) {
        self.blend_from = mirror_pose(&self.pose());
        self.blend_left = BLEND_SECONDS;
        self.facing = -self.facing;
    }

    /// Advance by `dt` seconds within the horizontal range `[min_x, max_x]`.
    pub fn update(&mut self, dt: f32, min_x: f32, max_x: f32) {
        self.clock += dt;
        self.state_left -= dt;

        if self.state == State::Walk {
            // Ramp speed in with the blend so feet don't skate while starting.
            let ramp = if self.blend_left > 0.0 {
                self.blend_k()
            } else {
                1.0
            };
            self.gait_phase = (self.gait_phase + dt / GAIT_PERIOD * ramp).rem_euclid(1.0);
            self.x += self.facing * self.speed * ramp * dt;
            let hit_edge =
                (self.facing > 0.0 && self.x >= max_x) || (self.facing < 0.0 && self.x <= min_x);
            if hit_edge {
                self.x = self.x.clamp(min_x, max_x);
                self.switch(State::Idle); // stop, glance around, then head back
                self.state_left = self.random(0.6, 1.5);
                self.turn_around();
            }
        }
        self.blend_left = (self.blend_left - dt).max(0.0);

        if self.state_left <= 0.0 && !self.hold_idle {
            let next = if self.state == State::Walk {
                State::Idle
            } else {
                State::Walk
            };
            self.switch(next);
            if next == State::Walk && self.random(0.0, 1.0) < 0.35 {
                self.turn_around();
            }
        }
    }

    /// Bones in screen points, feet resting on `ground_y` (y down).
    pub fn segments(&self, ground_y: f32) -> [Segment; BONES] {
        let pose = self.pose();
        let root_y = ground_y - depth_below_root(&pose, self.scale);
        forward_kinematics(&pose, [self.x, root_y], self.scale, self.facing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn rig_matches_stykanimate_default_figure() {
        let lengths: Vec<f32> = WALKER.iter().map(|b| b.length).collect();
        assert_eq!(lengths, [78., 44., 53., 48., 53., 48., 66., 63., 66., 63.]);
        let parents: Vec<usize> = WALKER.iter().map(|b| b.parent).collect();
        assert_eq!(parents, [0, 1, 1, 3, 1, 5, 0, 7, 0, 9]);
        assert_eq!(WALKER[HEAD].kind, BoneKind::Circle);
        assert_eq!(WALKER[THIGH[0]].length + WALKER[SHIN[0]].length, LEG_LEN);
        for (i, b) in WALKER.iter().enumerate() {
            assert!(b.parent <= i, "bone {} parent must come first", i + 1);
        }
    }

    #[test]
    fn lerp_takes_shortest_arc() {
        let r = lerp_angle(3.0, -3.0, 0.5); // across ±π, not through 0
        assert!(r.abs() > 3.0, "{r}");
    }

    #[test]
    fn fk_rest_pose_geometry() {
        let s = forward_kinematics(&rest_pose(), [100.0, 200.0], 1.0, 1.0);
        assert!(approx(s[TORSO].end[0], 100.0, 1e-3) && approx(s[TORSO].end[1], 122.0, 1e-3));
        assert_eq!(s[HEAD].start, s[TORSO].end); // head sits on the neck
        assert_eq!(s[SHIN[0]].start, s[THIGH[0]].end);
        assert_eq!(s[THIGH[0]].start, [100.0, 200.0]); // thighs start at the hip
    }

    #[test]
    fn fk_mirrors_and_scales() {
        let p = walk_pose(0.3);
        let r = forward_kinematics(&p, [0.0, 0.0], 0.5, 1.0);
        let l = forward_kinematics(&p, [0.0, 0.0], 0.5, -1.0);
        let m = forward_kinematics(&mirror_pose(&p), [0.0, 0.0], 0.5, -1.0);
        for i in 0..BONES {
            assert!(approx(r[i].end[0], -l[i].end[0], 1e-4));
            assert!(approx(r[i].end[1], l[i].end[1], 1e-4));
            // Mirrored pose drawn mirrored == original: turning doesn't pop.
            assert!(
                approx(r[i].end[0], m[i].end[0], 1e-3) && approx(r[i].end[1], m[i].end[1], 1e-3)
            );
        }
    }

    #[test]
    fn ik_reaches_target_with_knee_forward() {
        for p in 0..100 {
            let p = p as f32 / 100.0;
            let bob = (2.0 * TAU * (p - DUTY / 2.0)).cos();
            let hip = HIP_HEIGHT + HIP_BOB * bob;
            let pose = walk_pose(p);
            let s = forward_kinematics(&pose, [0.0, 0.0], 1.0, 1.0);
            for (side, off) in [0.0f32, 0.5].into_iter().enumerate() {
                let (fx, lift) = foot_track((p + off).rem_euclid(1.0));
                let ankle = s[SHIN[side]].end;
                assert!(
                    approx(ankle[0], fx, 0.05) && approx(ankle[1], hip - lift, 0.05),
                    "p={p} side={side}"
                );
                // Knee in front of the hip->ankle line.
                let knee = s[THIGH[side]].end;
                let cross = ankle[0] * knee[1] - ankle[1] * knee[0];
                assert!(cross < 0.0, "knee bends backwards at p={p} side={side}");
            }
        }
    }

    #[test]
    fn legs_alternate_and_arms_oppose_legs() {
        let s = forward_kinematics(&walk_pose(0.0), [0.0, 0.0], 1.0, 1.0);
        // At p = 0 foot A has just planted in front, foot B is behind.
        assert!(s[SHIN[0]].end[0] > 0.0 && s[SHIN[1]].end[0] < 0.0);
        // Arm A is back, arm B forward.
        assert!(s[FOREARM[0]].end[0] < s[FOREARM[1]].end[0]);
    }

    #[test]
    fn figure_is_about_250_px_tall() {
        let p = idle_pose(0.0);
        let s = forward_kinematics(&p, [0.0, 0.0], 1.0, 1.0);
        let top = s
            .iter()
            .map(|g| g.end[1].min(g.start[1]))
            .fold(f32::MAX, f32::min);
        let bottom = depth_below_root(&p, 1.0);
        assert!((220.0..270.0).contains(&(bottom - top)), "{}", bottom - top);
    }

    /// The defining property of a good walk: a planted foot doesn't move in the
    /// world while the body travels over it.
    #[test]
    fn planted_foot_does_not_slide() {
        let mut f = Figure::new(500.0, 1.0, 9);
        f.blend_left = 0.0;
        f.state_left = 100.0;
        let dt = 1.0 / 240.0;
        let mut worst: f32 = 0.0;
        let mut prev = f.segments(800.0);
        let mut prev_phase = f.gait_phase;
        for _ in 0..(240.0 * GAIT_PERIOD * 3.0) as usize {
            f.update(dt, 0.0, 1.0e6);
            let now = f.segments(800.0);
            for side in 0..2 {
                let q0 = (prev_phase + side as f32 * 0.5).rem_euclid(1.0);
                let q1 = (f.gait_phase + side as f32 * 0.5).rem_euclid(1.0);
                if q0 < DUTY && q1 < DUTY && q1 > q0 {
                    let a = prev[SHIN[side]].end;
                    let b = now[SHIN[side]].end;
                    worst = worst.max((a[0] - b[0]).abs()).max((a[1] - b[1]).abs());
                }
            }
            prev = now;
            prev_phase = f.gait_phase;
        }
        assert!(worst < 0.05, "planted foot slid {worst} px in one tick");
    }

    #[test]
    fn swing_foot_clears_the_ground() {
        let peak = (0..100)
            .map(|i| foot_track(DUTY + (1.0 - DUTY) * i as f32 / 100.0).1)
            .fold(0.0, f32::max);
        assert!(peak > 0.08 * LEG_LEN, "foot lift {peak}");
    }

    #[test]
    fn feet_rest_on_ground_every_frame() {
        let mut f = Figure::new(500.0, 0.55, 7);
        for _ in 0..600 {
            f.update(1.0 / 60.0, 0.0, 1000.0);
            let s = f.segments(800.0);
            let lowest = s
                .iter()
                .map(|g| g.start[1].max(g.end[1]) + g.thickness / 2.0)
                .fold(f32::MIN, f32::max);
            assert!(approx(lowest, 800.0, 0.01), "lowest point {lowest}");
        }
    }

    #[test]
    fn walk_speed_is_natural() {
        // ~1 leg length per second is a relaxed human walk.
        let legs_per_s = gait_speed() / LEG_LEN;
        assert!(
            (0.7..1.4).contains(&legs_per_s),
            "{legs_per_s} leg lengths/s"
        );
    }

    #[test]
    fn figure_stays_in_bounds_and_turns_around() {
        let mut f = Figure::new(50.0, 1.0, 42);
        let mut saw_left = false;
        for _ in 0..60 * 120 {
            f.update(1.0 / 60.0, 0.0, 300.0);
            assert!((0.0..=300.0).contains(&f.x));
            saw_left |= f.facing < 0.0;
        }
        assert!(saw_left, "never turned around");
    }

    #[test]
    fn state_machine_alternates_and_never_pops() {
        let mut f = Figure::new(500.0, 1.0, 3);
        let (mut idles, mut walks, mut last) = (0, 0, f.state);
        let mut prev = f.segments(800.0);
        for _ in 0..60 * 120 {
            f.update(1.0 / 60.0, 100.0, 900.0);
            if f.state != last {
                match f.state {
                    State::Idle => idles += 1,
                    State::Walk => walks += 1,
                }
                last = f.state;
            }
            // No joint may jump more than a few px between 60 Hz frames,
            // including across state changes and turnarounds.
            let now = f.segments(800.0);
            for i in 0..BONES {
                let d = ((now[i].end[0] - prev[i].end[0]).powi(2)
                    + (now[i].end[1] - prev[i].end[1]).powi(2))
                .sqrt();
                assert!(d < 12.0, "bone {i} jumped {d} px in one frame");
            }
            prev = now;
        }
        assert!(idles > 2 && walks > 2, "idles={idles} walks={walks}");
    }

    #[test]
    fn idle_renders_at_low_rate() {
        let mut f = Figure::new(500.0, 1.0, 5);
        assert_eq!(f.wanted_fps(), 60);
        f.switch(State::Idle);
        f.update(1.0, 0.0, 1000.0);
        assert_eq!(f.wanted_fps(), 20);
    }
}
