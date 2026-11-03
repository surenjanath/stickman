//! Action poses for the Walker rig (all original). Authored facing right (+x is
//! forward), absolute angles, y down, `-π/2` = up. Legs on the ground are solved
//! with two-bone IK so planted feet stay planted in every pose.
//!
//! Aim angles are in the same local frame: 0 = straight ahead, negative = upward.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::stickman::{
    gait_pose, idle_pose, leg_ik, lerp_pose, smoothstep, Pose, BONES, FOREARM, HEAD, HIP_HEIGHT,
    LEG_LEN, RUN, SHIN, THIGH, TORSO, UPPER_ARM,
};

const H: f32 = FRAC_PI_2;
const A: usize = 0; // lead (front) limb side
const B: usize = 1; // rear limb side

/// Plant both feet: ankles at forward offsets `fa`/`fb` (source px), lifted by
/// `la`/`lb`, with the hip `hip` px above the ground.
fn plant(p: &mut Pose, hip: f32, fa: f32, la: f32, fb: f32, lb: f32) {
    for (side, (x, lift)) in [(fa, la), (fb, lb)].into_iter().enumerate() {
        let (thigh, shin) = leg_ik([x, hip - lift]);
        p[THIGH[side]] = thigh;
        p[SHIN[side]] = shin;
    }
}

fn arm(p: &mut Pose, side: usize, upper: f32, fore: f32) {
    p[UPPER_ARM[side]] = upper;
    p[FOREARM[side]] = fore;
}

/// Piecewise blend through `keys` (time, pose) for `k` in [0, 1], eased.
fn sequence(keys: &[(f32, Pose)], k: f32) -> Pose {
    let k = k.clamp(0.0, 1.0);
    for w in keys.windows(2) {
        let ((t0, p0), (t1, p1)) = (w[0], w[1]);
        if k <= t1 {
            let u = if t1 > t0 { (k - t0) / (t1 - t0) } else { 1.0 };
            return lerp_pose(&p0, &p1, smoothstep(u));
        }
    }
    keys[keys.len() - 1].1
}

/// Fighting stance: knees bent, lead foot forward, fists up, a light bounce.
pub fn stance(t: f32) -> Pose {
    let bounce = (t * TAU * 1.8).sin();
    let mut p = [0.0; BONES];
    plant(&mut p, 0.86 * LEG_LEN + 2.0 * bounce, 30.0, 0.0, -32.0, 0.0);
    p[TORSO] = -H + 0.16 + 0.015 * bounce;
    p[HEAD] = -H + 0.12;
    arm(&mut p, A, 0.95 + 0.05 * bounce, -0.95);
    arm(&mut p, B, 1.25, -1.35 + 0.05 * bounce);
    p
}

/// Running: the walk cycle with a forward lean and pumping, bent arms.
pub fn run_pose(phase: f32) -> Pose {
    let mut p = gait_pose(phase, &RUN);
    p[TORSO] = -H + 0.24;
    p[HEAD] = -H + 0.18;
    for side in [A, B] {
        // Arms pump opposite the legs, elbows bent ~90°.
        let n = ((p[UPPER_ARM[side]] - H) / 0.38).clamp(-1.0, 1.0);
        p[UPPER_ARM[side]] = H + 0.75 * n;
        p[FOREARM[side]] = p[UPPER_ARM[side]] - 1.45;
    }
    p
}

/// Straight punch along `aim`. `k` ∈ [0,1]: wind-up, strike at k ≈ 0.45, recover.
pub fn punch(k: f32, aim: f32) -> Pose {
    let base = stance(0.0);
    let mut windup = base;
    windup[TORSO] = -H + 0.04;
    arm(&mut windup, A, 2.1, -0.4);
    let mut strike = base;
    plant(&mut strike, 0.84 * LEG_LEN, 48.0, 0.0, -34.0, 0.0);
    // Lean into straight punches, but back for upward ones so the arm clears
    // the head.
    let up = aim.min(0.0);
    strike[TORSO] = -H + 0.3 + 0.9 * up;
    strike[HEAD] = -H + 0.25 + 0.9 * up;
    arm(&mut strike, A, aim, aim);
    arm(&mut strike, B, 1.4, -1.2);
    sequence(
        &[
            (0.0, base),
            (0.3, windup),
            (0.45, strike),
            (0.62, strike),
            (1.0, base),
        ],
        k,
    )
}

/// Front kick along `aim` with the lead leg. Strike at k ≈ 0.45.
pub fn kick(k: f32, aim: f32) -> Pose {
    let base = stance(0.0);
    let mut chamber = base;
    chamber[TORSO] = -H - 0.12;
    chamber[THIGH[A]] = -0.35;
    chamber[SHIN[A]] = 1.25;
    chamber[THIGH[B]] = H + 0.05;
    chamber[SHIN[B]] = H;
    arm(&mut chamber, A, 0.5, -0.2);
    arm(&mut chamber, B, 2.4, 2.8);
    let mut strike = chamber;
    strike[TORSO] = -H - 0.3;
    strike[THIGH[A]] = aim.min(0.6);
    strike[SHIN[A]] = aim;
    sequence(
        &[
            (0.0, base),
            (0.28, chamber),
            (0.45, strike),
            (0.6, strike),
            (0.8, chamber),
            (1.0, base),
        ],
        k,
    )
}

/// Crouch before a jump (`k` = depth 0..1).
pub fn crouch(k: f32) -> Pose {
    let k = k.clamp(0.0, 1.0);
    let mut p = idle_pose(0.0);
    plant(&mut p, LEG_LEN * (0.95 - 0.3 * k), 18.0, 0.0, -18.0, 0.0);
    p[TORSO] = -H + 0.05 + 0.35 * k;
    p[HEAD] = -H + 0.05 + 0.3 * k;
    arm(&mut p, A, H + 0.7 * k, H + 0.5 * k);
    arm(&mut p, B, H + 0.8 * k, H + 0.6 * k);
    p
}

/// Airborne: tucked while rising, legs reaching down and arms out while falling.
/// `vy` is vertical velocity (negative = rising).
pub fn air(vy: f32) -> Pose {
    let mut rise = [0.0; BONES];
    rise[TORSO] = -H + 0.12;
    rise[HEAD] = -H + 0.1;
    rise[THIGH[A]] = -0.25;
    rise[SHIN[A]] = 1.3;
    rise[THIGH[B]] = 0.35;
    rise[SHIN[B]] = 1.9;
    arm(&mut rise, A, -0.25, -0.55);
    arm(&mut rise, B, -2.8, -3.0);
    let mut fall = [0.0; BONES];
    fall[TORSO] = -H + 0.04;
    fall[HEAD] = -H;
    fall[THIGH[A]] = H - 0.25;
    fall[SHIN[A]] = H - 0.05;
    fall[THIGH[B]] = H + 0.2;
    fall[SHIN[B]] = H + 0.3;
    arm(&mut fall, A, 0.15, -0.25);
    arm(&mut fall, B, PI - 0.15, PI + 0.25);
    lerp_pose(&rise, &fall, smoothstep((vy + 250.0) / 600.0))
}

/// Falling off something unexpectedly: windmilling arms, bicycling legs.
pub fn flail(t: f32) -> Pose {
    // Arms start hanging and only windmill (in opposite directions) once the
    // entry blend is over: a target sweeping past ±π mid-blend would make the
    // shortest-arc interpolation flip.
    let w = 11.0 * (t - 0.2).max(0.0);
    let mut p = [0.0; BONES];
    p[TORSO] = -H + 0.1 * (t * 7.0).sin();
    p[HEAD] = p[TORSO];
    arm(&mut p, A, H + w, H + w + 0.8);
    arm(&mut p, B, H - w, H - w - 0.8);
    p[THIGH[A]] = H + 0.6 * (t * 12.0).sin();
    p[SHIN[A]] = p[THIGH[A]] + 0.7;
    p[THIGH[B]] = H - 0.6 * (t * 12.0).sin();
    p[SHIN[B]] = p[THIGH[B]] + 0.7;
    p
}

/// Curled ball while tumbling through the air after a hit.
pub fn tumble() -> Pose {
    let mut p = [0.0; BONES];
    p[TORSO] = -H + 0.55;
    p[HEAD] = -H + 0.7;
    p[THIGH[A]] = -0.55;
    p[SHIN[A]] = 1.55;
    p[THIGH[B]] = -0.35;
    p[SHIN[B]] = 1.75;
    arm(&mut p, A, 0.35, 1.5);
    arm(&mut p, B, 0.6, 1.7);
    p
}

/// Quick evasive hop: leaning back, guard up, knees tucked.
pub fn dodge() -> Pose {
    let mut p = [0.0; BONES];
    p[TORSO] = -H - 0.32;
    p[HEAD] = -H - 0.3;
    p[THIGH[A]] = 0.55;
    p[SHIN[A]] = 1.85;
    p[THIGH[B]] = 0.95;
    p[SHIN[B]] = 2.1;
    arm(&mut p, A, 0.15, -1.25);
    arm(&mut p, B, 0.5, -1.05);
    p
}

/// Stomping on a window: knee up, slam at k ≈ 0.5, recover.
pub fn stomp(k: f32) -> Pose {
    let mut base = idle_pose(0.0);
    plant(&mut base, 0.93 * LEG_LEN, 16.0, 0.0, -16.0, 0.0);
    let mut raise = base;
    raise[THIGH[A]] = -0.45;
    raise[SHIN[A]] = 1.05;
    raise[TORSO] = -H - 0.05;
    arm(&mut raise, A, -0.45, -0.7);
    arm(&mut raise, B, -2.65, -2.9);
    let mut slam = base;
    plant(&mut slam, 0.82 * LEG_LEN, 22.0, 0.0, -16.0, 0.0);
    slam[TORSO] = -H + 0.3;
    arm(&mut slam, A, 0.6, 1.2);
    arm(&mut slam, B, 0.9, 1.4);
    sequence(
        &[
            (0.0, base),
            (0.36, raise),
            (0.52, slam),
            (0.7, slam),
            (1.0, base),
        ],
        k,
    )
}

/// "Come on, then": beckoning with the lead hand, other fist on the hip.
pub fn taunt(t: f32) -> Pose {
    let mut p = stance(t);
    plant(&mut p, 0.9 * LEG_LEN, 34.0, 0.0, -36.0, 0.0);
    p[TORSO] = -H - 0.06;
    p[HEAD] = -H - 0.1;
    let curl = 0.5 + 0.5 * (t * 11.0).sin();
    arm(&mut p, A, 0.15, -0.25 - 1.1 * curl);
    arm(&mut p, B, 2.3, 0.85);
    p
}

/// Watching the user: hand on chin, slight sway.
pub fn think(t: f32) -> Pose {
    let mut p = idle_pose(t);
    p[TORSO] = -H + 0.04;
    p[HEAD] = -H + 0.12;
    arm(&mut p, B, 0.95, -1.55);
    arm(&mut p, A, 1.15, 0.15);
    p
}

/// Raise both arms to carry something overhead (applied over any base pose).
pub fn hold_arms(p: &mut Pose) {
    arm(p, A, -0.3, -1.15);
    arm(p, B, -0.05, -0.95);
}

/// Bend down and rip something up out of the line underfoot (k: 0..1, the
/// pull happens at k ≈ 0.5).
pub fn grab(k: f32) -> Pose {
    let mut reach = crouch(1.0);
    reach[TORSO] = -H + 0.75;
    reach[HEAD] = -H + 0.7;
    arm(&mut reach, A, 0.9, 1.2);
    arm(&mut reach, B, 1.1, 1.35);
    let mut lift = idle_pose(0.0);
    hold_arms(&mut lift);
    sequence(
        &[
            (0.0, idle_pose(0.0)),
            (0.35, reach),
            (0.5, reach),
            (0.85, lift),
            (1.0, lift),
        ],
        k,
    )
}

/// Underhand lob toward `aim` (local frame): the arm swings back low, then up
/// and forward. (An overhand throw would sweep the arm through the head in a
/// side view.) Release at k ≈ 0.4.
pub fn throw(k: f32, aim: f32) -> Pose {
    let mut base = stance(0.0);
    hold_arms(&mut base);
    let mut windup = stance(0.0);
    windup[TORSO] = -H + 0.28;
    windup[HEAD] = -H + 0.22;
    arm(&mut windup, A, 2.45, 2.7);
    arm(&mut windup, B, aim.max(-0.15), aim.max(-0.15));
    let mut release = stance(0.0);
    plant(&mut release, 0.84 * LEG_LEN, 46.0, 0.0, -34.0, 0.0);
    release[TORSO] = -H + 0.12;
    release[HEAD] = -H + 0.1;
    // The flight path is computed separately; the arm only gestures toward it.
    arm(&mut release, A, aim.max(-0.3), aim.max(-0.3) - 0.1);
    arm(&mut release, B, 1.9, 2.1);
    let follow = stance(0.0);
    sequence(
        &[
            (0.0, base),
            (0.25, windup),
            (0.4, release),
            (0.6, release),
            (1.0, follow),
        ],
        k,
    )
}

/// Lunge and snatch something at `aim` with both hands, then hoist it
/// overhead. Hands close at k ≈ 0.45; overhead by k = 1.
pub fn snatch(k: f32, aim: f32) -> Pose {
    let aim = aim.clamp(-0.7, 0.9);
    let base = stance(0.0);
    let mut reach = base;
    plant(&mut reach, 0.82 * LEG_LEN, 50.0, 0.0, -34.0, 0.0);
    let up = aim.min(0.0);
    reach[TORSO] = -H + 0.32 + 0.9 * up;
    reach[HEAD] = -H + 0.26 + 0.9 * up;
    arm(&mut reach, A, aim - 0.05, aim);
    arm(&mut reach, B, aim + 0.12, aim + 0.1);
    let mut lift = stance(0.0);
    hold_arms(&mut lift);
    sequence(&[(0.0, base), (0.4, reach), (0.55, reach), (1.0, lift)], k)
}

/// Holding a prize overhead and shaking it (t: seconds).
pub fn hoist(t: f32) -> Pose {
    let mut p = stance(t);
    hold_arms(&mut p);
    // Only ever shake forward, away from the head.
    let shake = 0.09 * (0.5 + 0.5 * (t * TAU * 5.0).sin());
    for side in [A, B] {
        p[UPPER_ARM[side]] += shake;
        p[FOREARM[side]] += shake;
    }
    p
}

/// Flying kick in mid-air: the kick, with the other leg tucked up.
pub fn flying_kick(k: f32, aim: f32) -> Pose {
    let mut p = kick(k, aim);
    p[THIGH[B]] = H - 0.65;
    p[SHIN[B]] = H + 0.75;
    p
}

/// Dangling from the user's pointer: legs swinging, arms flailing.
pub fn dangle(t: f32) -> Pose {
    let mut p = [0.0; BONES];
    p[TORSO] = -H + 0.05 * (t * 5.0).sin();
    p[HEAD] = p[TORSO] - 0.1;
    // Arms flap in front and behind, never overhead (the head is up there).
    let flap = (t * TAU * 2.2).sin();
    arm(&mut p, A, 0.1 + 0.35 * flap, -0.5 + 0.35 * flap);
    arm(&mut p, B, PI - 0.1 + 0.35 * flap, PI + 0.5 + 0.35 * flap);
    let sway = 0.25 * (t * TAU * 1.3).sin();
    p[THIGH[A]] = H + 0.15 + sway;
    p[SHIN[A]] = H + 0.05 + sway * 1.3;
    p[THIGH[B]] = H - 0.1 - sway;
    p[SHIN[B]] = H - 0.2 - sway * 1.3;
    p
}

/// Landing squash (k = 0 at touchdown → 1 recovered).
pub fn land(k: f32) -> Pose {
    crouch(0.65 * (1.0 - smoothstep(k)))
}

/// Standing height of the hip above the ground for poses using `plant`.
pub const STAND_HIP: f32 = HIP_HEIGHT;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stickman::{depth_below_root, forward_kinematics};

    fn walk_pose_for_test(p: f32) -> Pose {
        crate::stickman::walk_pose(p)
    }

    fn all_poses() -> Vec<(&'static str, Pose)> {
        let mut v = vec![
            ("stance", stance(0.3)),
            ("run", run_pose(0.4)),
            ("crouch", crouch(1.0)),
            ("air-up", air(-800.0)),
            ("air-down", air(600.0)),
            ("flail", flail(0.7)),
            ("tumble", tumble()),
            ("dodge", dodge()),
            ("taunt", taunt(0.4)),
            ("think", think(1.0)),
            ("land", land(0.0)),
        ];
        for q in [0.0, 0.2, 0.4, 0.6, 0.8] {
            v.push(("run", run_pose(q)));
        }
        for t in [0.0, 0.05, 0.1, 0.15] {
            v.push(("hoist", hoist(t)));
            v.push(("dangle", dangle(t * 3.0)));
        }
        let mut held = walk_pose_for_test(0.3);
        hold_arms(&mut held);
        v.push(("carry", held));
        for k in [0.0, 0.3, 0.45, 0.7, 1.0] {
            v.push(("grab", grab(k)));
            v.push(("throw", throw(k, -0.3)));
            v.push(("throw up", throw(k, -0.75)));
            v.push(("throw down", throw(k, 0.6)));
            v.push(("uppercut", punch(k, -0.75)));
            v.push(("punch", punch(k, -0.2)));
            v.push(("kick", kick(k, 0.2)));
            v.push(("stomp", stomp(k)));
            v.push(("snatch", snatch(k, 0.0)));
            v.push(("flying kick", flying_kick(k, 0.2)));
            v.push(("flying kick up", flying_kick(k, -0.4)));
            v.push(("snatch up", snatch(k, -0.7)));
            v.push(("snatch down", snatch(k, 0.9)));
        }
        v
    }

    #[test]
    fn every_pose_is_finite_and_connected() {
        for (name, p) in all_poses() {
            assert!(p.iter().all(|a| a.is_finite()), "{name} has NaN");
            let s = forward_kinematics(&p, [0.0, 0.0], 1.0, 1.0);
            assert_eq!(s[HEAD].start, s[TORSO].end, "{name}");
        }
    }

    #[test]
    fn punch_extends_the_fist_along_the_aim() {
        let reach = |k| {
            let s = forward_kinematics(&punch(k, 0.0), [0.0, 0.0], 1.0, 1.0);
            s[FOREARM[A]].end[0] - s[TORSO].end[0]
        };
        assert!(reach(0.5) > 90.0, "strike reach {}", reach(0.5));
        assert!(reach(0.3) < reach(0.5) - 60.0, "wind-up should pull back");
        // Aimed upward: fist ends above the shoulder.
        let up = forward_kinematics(&punch(0.5, -0.8), [0.0, 0.0], 1.0, 1.0);
        assert!(up[FOREARM[A]].end[1] < up[TORSO].end[1] - 50.0);
    }

    #[test]
    fn kick_raises_the_foot_forward() {
        let s = forward_kinematics(&kick(0.5, 0.0), [0.0, 0.0], 1.0, 1.0);
        let foot = s[SHIN[A]].end;
        assert!(foot[0] > 90.0, "kick reach {}", foot[0]);
        assert!(
            foot[1] < 40.0,
            "kicking foot should be near hip height, got {}",
            foot[1]
        );
    }

    #[test]
    fn grounded_poses_keep_both_feet_down() {
        for (name, p) in [
            ("stance", stance(0.0)),
            ("punch", punch(0.5, 0.0)),
            ("taunt", taunt(0.2)),
        ] {
            let s = forward_kinematics(&p, [0.0, 0.0], 1.0, 1.0);
            let d = depth_below_root(&p, 1.0);
            for side in [A, B] {
                let foot =
                    s[SHIN[side]].end[1] + crate::stickman::WALKER[SHIN[side]].thickness / 2.0;
                assert!(
                    (foot - d).abs() < 1.5,
                    "{name}: foot {side} at {foot}, ground {d}"
                );
            }
        }
    }

    /// Fists and elbows must never end up inside the head ring.
    #[test]
    fn hands_stay_out_of_the_head() {
        for (name, p) in all_poses() {
            let s = forward_kinematics(&p, [0.0, 0.0], 1.0, 1.0);
            let h = s[HEAD];
            let c = [(h.start[0] + h.end[0]) / 2.0, (h.start[1] + h.end[1]) / 2.0];
            let r = 22.0 + 4.0;
            let dist = |a: [f32; 2], b: [f32; 2]| {
                // Distance from the head centre to segment a-b.
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let t = (((c[0] - a[0]) * dx + (c[1] - a[1]) * dy) / (dx * dx + dy * dy).max(1e-6))
                    .clamp(0.0, 1.0);
                ((a[0] + t * dx - c[0]).powi(2) + (a[1] + t * dy - c[1]).powi(2)).sqrt()
            };
            for side in [A, B] {
                let u = s[UPPER_ARM[side]];
                // The shoulder touches the head ring; the rest of the upper arm must not.
                let mid = [(u.start[0] + u.end[0]) / 2.0, (u.start[1] + u.end[1]) / 2.0];
                let d = ((mid[0] - c[0]).powi(2) + (mid[1] - c[1]).powi(2)).sqrt();
                assert!(
                    d > r,
                    "{name}: upper arm {side} crosses the head ({d:.1} < {r})"
                );
                let elbow = u.end;
                let d = ((elbow[0] - c[0]).powi(2) + (elbow[1] - c[1]).powi(2)).sqrt();
                assert!(d > r, "{name}: elbow {side} inside the head ({d:.1} < {r})");
                let f = s[FOREARM[side]];
                let d = dist(f.start, f.end);
                assert!(
                    d > r,
                    "{name}: forearm {side} crosses the head ({d:.1} < {r})"
                );
            }
        }
    }

    #[test]
    fn crouch_lowers_the_body() {
        let tall = depth_below_root(&crouch(0.0), 1.0);
        let low = depth_below_root(&crouch(1.0), 1.0);
        assert!(tall - low > 25.0, "crouch only lowers {}", tall - low);
    }

    #[test]
    fn sequence_hits_keys_and_clamps() {
        let a = stance(0.0);
        let b = tumble();
        let keys = [(0.0, a), (0.5, b), (1.0, a)];
        let same = |x: Pose, y: Pose| {
            x.iter()
                .zip(y)
                .all(|(p, q)| ((p - q + PI).rem_euclid(TAU) - PI).abs() < 1e-5)
        };
        assert!(same(sequence(&keys, 0.5), b));
        assert!(same(sequence(&keys, -1.0), a));
        assert!(same(sequence(&keys, 2.0), a));
    }
}
