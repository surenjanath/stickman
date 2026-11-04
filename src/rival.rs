//! A rival stick figure that drops in now and then to duel the user's figure.
//!
//! During a duel each figure treats the other as its "cursor": the combat
//! behaviour chases, punches, kicks, leaps and super-jumps at it, and every
//! hit that would have struck the cursor knocks the opponent back instead.
//! Whoever is knocked out first loses; then the rival walks off screen.

use crate::agent::{Agent, Dread, Event, Mode, Senses, Tactic, Traits};
use crate::geom::{v2, V2};
use crate::physics::{Platform, World};

/// The rival's colour: a red ninja.
pub const RIVAL_RGB: [u8; 3] = [190, 30, 35];
/// Damage per hit in a duel.
const DUEL_DAMAGE: f32 = 26.0;
/// A duel ends after this long, winner or not.
const DUEL_LIMIT: f32 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// Falling in from the top of the screen.
    Entering,
    Fighting,
    /// Walking off the edge of the screen toward `x`.
    Leaving {
        x: f32,
    },
    /// Off screen: remove it.
    Gone,
}

/// What happened, for speech, sound and the record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cue {
    /// The rival landed and challenges.
    Arrived { at: V2 },
    /// The user's figure won (the rival is knocked out).
    Won,
    /// The rival won.
    Lost,
    /// Time ran out.
    Draw,
}

pub struct Rival {
    pub agent: Agent,
    pub phase: Phase,
    /// Seconds in the current phase.
    pub t: f32,
}

impl Rival {
    /// A rival dropping in at `x`, from just under the top of the screen
    /// (`top`), onto `ground`.
    pub fn new(x: f32, top: f32, ground: Platform, scale: f32, seed: u64) -> Rival {
        let mut agent = Agent::new(x, ground, scale, seed);
        agent.traits = Traits {
            naps: false,
            dancing: false,
            doodles: false,
            sitting: false,
            powers: true,
            telekinesis: false,
            aggression: 0.85,
            max_health: 100.0,
            speed: 1.1,
        };
        agent.snatch_chance = 0.0;
        agent.drop_in(v2(x, top + 60.0 + 230.0 * scale));
        Rival {
            agent,
            phase: Phase::Entering,
            t: 0.0,
        }
    }

    pub fn fighting(&self) -> bool {
        self.phase == Phase::Fighting
    }

    /// Senses for a figure whose opponent stands in for the cursor.
    pub fn duel_senses(base: &Senses, opponent: &Agent) -> Senses {
        Senses {
            cursor: opponent.center(),
            cursor_vel: opponent.vel,
            mode: Mode::Combat,
            can_grab_cursor: false,
            ammo: None,
            tk_words_near: 0,
            music: false,
            can_throw_window: false,
            weapon_available: false,
            dread: Dread::None,
            tactic: Tactic::Any,
            retreat: false,
            ..*base
        }
    }

    /// One fixed step. `me` is the user's figure, `me_base`/`rival_base` its
    /// and the rival's ordinary senses (cursor, clicks, walking range).
    /// Updates both figures while fighting; otherwise only the rival (the
    /// caller updates `me` as usual and passes `me_updated = true`).
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        me: &mut Agent,
        me_updated: bool,
        world: &World,
        me_base: &Senses,
        rival_base: &Senses,
        me_events: &mut Vec<Event>,
        rival_events: &mut Vec<Event>,
    ) -> Vec<Cue> {
        let mut cues = Vec::new();
        self.t += crate::agent::FIXED_DT;
        match self.phase {
            Phase::Entering => {
                self.agent.scripted = true;
                let s = Self::duel_senses(rival_base, me);
                self.agent.update(&s, world, rival_events);
                if self.agent.ground.is_some() && self.t > 0.3 {
                    self.agent.scripted = false;
                    self.phase = Phase::Fighting;
                    self.t = 0.0;
                    cues.push(Cue::Arrived {
                        at: self.agent.head(),
                    });
                }
            }
            Phase::Fighting => {
                self.agent.dueling = true;
                me.dueling = true;
                let rs = Self::duel_senses(rival_base, me);
                let mut hits_on_me = Vec::new();
                self.agent.update(&rs, world, &mut hits_on_me);
                if !me_updated {
                    let ms = Self::duel_senses(me_base, &self.agent);
                    let mut mine = Vec::new();
                    me.update(&ms, world, &mut mine);
                    // My blows land on the rival instead of the cursor.
                    for e in mine {
                        match e {
                            Event::StruckCursor { dir, .. } => {
                                self.agent.take_hit(dir, DUEL_DAMAGE, rival_events)
                            }
                            other => me_events.push(other),
                        }
                    }
                }
                for e in hits_on_me {
                    match e {
                        Event::StruckCursor { dir, .. } => me.take_hit(dir, DUEL_DAMAGE, me_events),
                        other => rival_events.push(other),
                    }
                }
                let over = if self.agent.knocked_out() {
                    Some(Cue::Won)
                } else if me.knocked_out() {
                    Some(Cue::Lost)
                } else if self.t > DUEL_LIMIT {
                    Some(Cue::Draw)
                } else {
                    None
                };
                if let Some(c) = over {
                    cues.push(c);
                    self.agent.dueling = false;
                    me.dueling = false;
                    // Off the nearer side of the walking range.
                    let (lo, hi) = (rival_base.min_x, rival_base.max_x);
                    let x = if self.agent.pos.x - lo < hi - self.agent.pos.x {
                        lo
                    } else {
                        hi
                    };
                    self.phase = Phase::Leaving { x };
                    self.t = 0.0;
                }
            }
            Phase::Leaving { x } => {
                self.agent.scripted = true;
                let s = Senses {
                    mode: Mode::Idle,
                    clicked: rival_base.clicked,
                    ..*rival_base
                };
                self.agent.update(&s, world, rival_events);
                self.agent.go_to(x);
                if (self.agent.pos.x - x).abs() < 4.0 || self.t > 20.0 {
                    self.phase = Phase::Gone;
                }
            }
            Phase::Gone => {}
        }
        cues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::FIXED_DT;

    const GROUND_Y: f32 = 1100.0;

    fn senses() -> Senses {
        Senses {
            cursor: v2(-999.0, -999.0),
            cursor_vel: V2::ZERO,
            clicked: false,
            mode: Mode::Idle,
            active_window: None,
            min_x: 40.0,
            max_x: 1760.0,
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
            dread: crate::agent::Dread::None,
            dread_level: 0.0,
            dock: None,
            tactic: crate::agent::Tactic::Any,
            retreat: false,
        }
    }

    #[test]
    fn a_duel_has_a_winner_and_the_rival_leaves() {
        let ground = Platform::ground(1800.0, GROUND_Y);
        let mut w = World::new();
        w.set_platforms(vec![ground]);
        for seed in [1u64, 2, 3] {
            let mut me = Agent::new(500.0, ground, 0.55, seed);
            let mut r = Rival::new(1300.0, 0.0, ground, 0.55, seed + 10);
            let s = senses();
            let mut cues = Vec::new();
            let (mut mine, mut theirs) = (Vec::new(), Vec::new());
            let mut struck = 0;
            let mut ticks = 0;
            for _ in 0..(90.0 / FIXED_DT) as usize {
                ticks += 1usize;
                let fighting = r.fighting();
                if !fighting {
                    me.update(&s, &w, &mut mine);
                }
                cues.extend(r.step(&mut me, !fighting, &w, &s, &s, &mut mine, &mut theirs));
                struck += mine
                    .iter()
                    .chain(&theirs)
                    .filter(|e| matches!(e, Event::Struck { .. }))
                    .count();
                mine.clear();
                theirs.clear();
                if r.phase == Phase::Gone {
                    break;
                }
            }
            assert!(
                cues.iter().any(|c| matches!(c, Cue::Won | Cue::Lost)),
                "seed {seed}: no knockout in {:.0} s: {cues:?}",
                ticks as f32 * FIXED_DT
            );
            assert!(
                matches!(cues.first(), Some(Cue::Arrived { .. })),
                "seed {seed}: {cues:?}"
            );
            assert!(struck >= 3, "seed {seed}: only {struck} hits landed");
            assert!(
                cues.iter()
                    .any(|c| matches!(c, Cue::Won | Cue::Lost | Cue::Draw)),
                "seed {seed}: never ended: {cues:?}"
            );
            assert_eq!(r.phase, Phase::Gone, "seed {seed}: never left");
        }
    }

    #[test]
    fn duel_senses_point_at_the_opponent() {
        let ground = Platform::ground(1800.0, GROUND_Y);
        let other = Agent::new(1200.0, ground, 0.55, 1);
        let s = Rival::duel_senses(&senses(), &other);
        assert_eq!(s.cursor, other.center());
        assert_eq!(s.mode, Mode::Combat);
        assert!(!s.can_grab_cursor);
    }
}
