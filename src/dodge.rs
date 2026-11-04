//! The cursor fights back. While the user leaves the mouse alone, the real
//! pointer sidesteps the figure's punches and kicks, slips out of its grip and
//! backs off when it charges. The moment the user touches the mouse, they're
//! in control again: nothing here moves the pointer while they do.
//!
//! Pure logic: the caller reads the pointer, says what the figure is doing,
//! and moves the pointer where [`Step::to`] says.

use crate::geom::{v2, Rect, V2};

/// The mouse must be still this long before the cursor starts acting alone.
pub const IDLE_BEFORE: f32 = 0.6;
/// How far from where the user left it the cursor may wander.
const LEASH: f32 = 380.0;
/// Attacks from farther than this aren't worth dodging.
const NOTICE: f32 = 260.0;
/// Chance to sidestep a strike, and to dodge a grab.
const STRIKE_DODGE: f32 = 0.55;
const GRAB_DODGE: f32 = 0.6;
/// Chance (once per hold) to wriggle out of the figure's hands.
const SLIP: f32 = 0.35;
/// Held at least this long before it tries to slip free.
const SLIP_AFTER: f32 = 0.5;

/// What the figure is doing to the cursor right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threat {
    /// A punch, kick, flying kick or sword slash.
    Strike,
    /// Reaching to snatch it.
    Grab,
    /// Running at it.
    Charge,
}

/// This tick's view of the fight.
#[derive(Debug, Clone, Copy)]
pub struct Input {
    pub cursor: V2,
    /// Seconds since the user last moved the mouse (not counting our moves).
    pub idle: f32,
    /// The figure's chest.
    pub figure: V2,
    pub threat: Option<Threat>,
    /// Seconds into the figure's current action (falls back to 0 when a new
    /// one starts: that's how a fresh attack is noticed).
    pub action_t: f32,
    /// Seconds the figure has been holding the cursor, if it is.
    pub held_for: Option<f32>,
    /// The screen the cursor is on.
    pub area: Rect,
}

/// What the cursor does this tick.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Step {
    /// Move the pointer here.
    pub to: Option<V2>,
    /// It just sidestepped an attack (the figure will be annoyed).
    pub dodged: bool,
    /// It just wriggled out of the figure's hands.
    pub slipped: bool,
}

/// A short eased move of the pointer.
#[derive(Debug, Clone, Copy)]
struct Glide {
    from: V2,
    to: V2,
    t: f32,
    dur: f32,
}

#[derive(Debug, Clone)]
pub struct Dodger {
    glide: Option<Glide>,
    /// Where the user left it.
    anchor: Option<V2>,
    cooldown: f32,
    rng: u64,
    /// Last tick's threat and action time.
    last: Option<(Threat, f32)>,
    /// Tried to slip out of this hold already.
    tried_slip: bool,
}

impl Dodger {
    pub fn new(seed: u64) -> Dodger {
        Dodger {
            glide: None,
            anchor: None,
            cooldown: 0.0,
            rng: seed | 1,
            last: None,
            tried_slip: false,
        }
    }

    /// Uniform in 0..1 (splitmix64: fine even from small seeds).
    fn random(&mut self) -> f32 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Stop whatever it was doing (the user took the mouse, or it's off).
    pub fn reset(&mut self) {
        self.glide = None;
        self.anchor = None;
        self.last = None;
    }

    pub fn step(&mut self, i: &Input, dt: f32) -> Step {
        let mut out = Step::default();
        self.cooldown -= dt;
        // A new attack: a different threat, or the same kind started over.
        let fresh = match (i.threat, self.last) {
            (Some(t), Some((lt, at))) => t != lt || i.action_t + dt * 0.5 < at,
            (Some(_), None) => true,
            (None, _) => false,
        };
        self.last = i.threat.map(|t| (t, i.action_t));
        if i.held_for.is_none() {
            self.tried_slip = false;
        }
        if i.idle < IDLE_BEFORE {
            self.glide = None;
            self.anchor = None;
            return out;
        }
        let anchor = *self.anchor.get_or_insert(i.cursor);

        // Caught: once per hold, a chance to wriggle free.
        if let Some(held) = i.held_for {
            self.glide = None;
            if held >= SLIP_AFTER && !self.tried_slip {
                self.tried_slip = true;
                if self.random() < SLIP {
                    let away = (i.cursor - i.figure).normalize_or(v2(1.0, -1.0).normalize());
                    let to = self.keep(i.cursor + away * 130.0 + v2(0.0, -40.0), anchor, i.area);
                    self.glide = Some(Glide {
                        from: i.cursor,
                        to,
                        t: 0.0,
                        dur: 0.14,
                    });
                    self.cooldown = 0.6;
                    out.slipped = true;
                }
            }
            if !out.slipped {
                return out;
            }
        }

        let close = (i.cursor - i.figure).length() < NOTICE;
        if self.glide.is_none() && self.cooldown <= 0.0 && close {
            match i.threat {
                Some(t @ (Threat::Strike | Threat::Grab)) if fresh => {
                    let chance = if t == Threat::Grab {
                        GRAB_DODGE
                    } else {
                        STRIKE_DODGE
                    };
                    if self.random() < chance {
                        let away = (i.cursor - i.figure).normalize_or(v2(1.0, 0.0));
                        // Mostly away, partly up or down: a hop aside.
                        let side = if self.random() < 0.5 { 1.0 } else { -1.0 };
                        let across = v2(-away.y, away.x) * side;
                        let dist = 70.0 + 50.0 * self.random();
                        let to = i.cursor + away * dist + across * (25.0 + 30.0 * self.random());
                        self.glide = Some(Glide {
                            from: i.cursor,
                            to: self.keep(to, anchor, i.area),
                            t: 0.0,
                            dur: 0.12,
                        });
                        self.cooldown = 0.35;
                        out.dodged = true;
                    }
                }
                Some(Threat::Charge) => {
                    // Back off, a little at a time, as it comes.
                    let away = v2((i.cursor.x - i.figure.x).signum(), -0.35).normalize();
                    let to = self.keep(i.cursor + away * 55.0, anchor, i.area);
                    if (to - i.cursor).length() > 10.0 {
                        self.glide = Some(Glide {
                            from: i.cursor,
                            to,
                            t: 0.0,
                            dur: 0.4,
                        });
                    }
                    self.cooldown = 0.8;
                }
                _ => {}
            }
        }

        if let Some(g) = &mut self.glide {
            g.t += dt;
            let k = (g.t / g.dur).clamp(0.0, 1.0);
            let ease = k * k * (3.0 - 2.0 * k);
            out.to = Some(g.from + (g.to - g.from) * ease);
            if k >= 1.0 {
                self.glide = None;
            }
        }
        out
    }

    /// Keep `p` on the leash around `anchor` and on screen.
    fn keep(&self, p: V2, anchor: V2, area: Rect) -> V2 {
        let off = p - anchor;
        let p = if off.length() > LEASH {
            anchor + off.normalize() * LEASH
        } else {
            p
        };
        let lo = v2(area.x as f32 + 12.0, area.y as f32 + 40.0);
        let hi = v2(area.right() as f32 - 12.0, area.bottom() as f32 - 12.0);
        p.clamp(lo, hi.max(lo))
    }
}

/// What the figure says when the cursor gets away from it, by how many times
/// it has in the last half minute: annoyed, then cursing, then losing it.
pub fn taunt_lines(streak: usize) -> (&'static [&'static str], Fury) {
    match streak {
        0..=2 => (
            &[
                "Hold still!",
                "Hey! Get back here!",
                "Missed?! How?",
                "Stop dodging!",
                "Quit squirming!",
            ],
            Fury::Annoyed,
        ),
        3..=5 => (
            &[
                "Stop moving, you little shit!",
                "Damn it, hold STILL!",
                "Fight me, you coward arrow!",
                "What the hell?! Get over here!",
                "Son of a bitch, STAY PUT!",
            ],
            Fury::Cursing,
        ),
        _ => (
            &[
                "AAAAARGH! I'LL TEAR YOU APART!",
                "THAT'S IT! YOU'RE FUCKING DEAD, CURSOR!",
                "I'M GONNA RIP YOU OFF THE SCREEN!",
                "STOP! DODGING! ME!",
                "RAAAAAGH! HOLD STILL, DAMN IT!",
            ],
            Fury::Berserk,
        ),
    }
}

/// How worked up a dodge streak has made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fury {
    Annoyed,
    Cursing,
    /// Past caring: its resentment spikes until it loses its temper.
    Berserk,
}

impl Fury {
    /// Extra resentment from one more dodge at this level.
    pub fn resentment(self) -> f32 {
        match self {
            Fury::Annoyed => 2.0,
            Fury::Cursing => 4.0,
            Fury::Berserk => 9.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn area() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1100.0,
        }
    }

    fn input(threat: Option<Threat>, action_t: f32) -> Input {
        Input {
            cursor: v2(900.0, 600.0),
            idle: 2.0,
            figure: v2(850.0, 640.0),
            threat,
            action_t,
            held_for: None,
            area: area(),
        }
    }

    /// Run an attack (a fresh strike, then the swing going on) and return
    /// where the pointer ended up and whether it dodged.
    fn attack(d: &mut Dodger, mut i: Input, threat: Threat) -> (V2, bool) {
        let mut dodged = false;
        for n in 0..30 {
            i.threat = Some(threat);
            i.action_t = n as f32 * DT;
            let s = d.step(&i, DT);
            dodged |= s.dodged;
            if let Some(p) = s.to {
                i.cursor = p;
            }
        }
        (i.cursor, dodged)
    }

    #[test]
    fn idle_it_sidesteps_some_attacks_and_only_away_from_the_figure() {
        let mut dodges = 0;
        for seed in 1..200 {
            let mut d = Dodger::new(seed);
            let i = input(None, 0.0);
            let (end, dodged) = attack(&mut d, i, Threat::Strike);
            if dodged {
                dodges += 1;
                let before = (i.cursor - i.figure).length();
                let after = (end - i.figure).length();
                assert!(
                    after > before + 40.0,
                    "dodged toward it: {before} -> {after}"
                );
                assert!((end - i.cursor).length() <= LEASH + 1.0);
            } else {
                assert_eq!(end, i.cursor);
            }
        }
        // Roughly STRIKE_DODGE of the time: sometimes, not always.
        assert!((70..150).contains(&dodges), "{dodges}/199 dodged");
    }

    #[test]
    fn it_never_moves_while_the_user_has_the_mouse() {
        for seed in 1..50 {
            let mut d = Dodger::new(seed);
            let mut i = input(None, 0.0);
            i.idle = 0.2;
            let (end, dodged) = attack(&mut d, i, Threat::Strike);
            assert!(!dodged);
            assert_eq!(end, i.cursor);
            // A glide in progress stops the moment the user moves.
            let mut d = Dodger::new(seed);
            let mut i = input(Some(Threat::Strike), 0.0);
            let s = d.step(&i, DT);
            if s.to.is_some() {
                i.idle = 0.0;
                assert_eq!(d.step(&i, DT).to, None);
            }
        }
    }

    #[test]
    fn one_attack_one_chance() {
        // The same swing going on doesn't get dodged again and again.
        let mut d = Dodger::new(7);
        let mut n = 0;
        for k in 0..120 {
            let i = input(Some(Threat::Strike), k as f32 * DT);
            n += usize::from(d.step(&i, DT).dodged);
        }
        assert!(n <= 1);
    }

    #[test]
    fn it_backs_off_from_a_charge_and_sometimes_slips_a_grip() {
        let mut d = Dodger::new(3);
        let mut i = input(Some(Threat::Charge), 0.0);
        for _ in 0..30 {
            if let Some(p) = d.step(&i, DT).to {
                i.cursor = p;
            }
            i.action_t += DT;
        }
        assert!(i.cursor.x > 900.0 + 20.0, "didn't back off: {:?}", i.cursor);
        let slipped = (1..200)
            .filter(|seed| {
                let mut d = Dodger::new(*seed);
                let mut i = input(None, 0.0);
                (0..60).any(|k| {
                    i.held_for = Some(k as f32 * DT);
                    d.step(&i, DT).slipped
                })
            })
            .count();
        assert!((40..110).contains(&slipped), "{slipped}/199 slipped");
    }

    #[test]
    fn dodging_it_again_and_again_drives_it_berserk() {
        assert_eq!(taunt_lines(1).1, Fury::Annoyed);
        assert_eq!(taunt_lines(4).1, Fury::Cursing);
        assert_eq!(taunt_lines(9).1, Fury::Berserk);
        assert!(Fury::Berserk.resentment() > Fury::Cursing.resentment());
    }
}
