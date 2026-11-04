//! A minion army: small allied stick figures the figure calls in to gang up
//! on the cursor. Each one is an ordinary [`Agent`] in combat mode with the
//! cursor as its target; a click knocks one out and it vanishes in a puff.
//! After a while the survivors walk off screen.

use crate::agent::{Agent, Event, Mode, Senses, Traits};
use crate::geom::{v2, V2};
use crate::physics::{Platform, World};

/// Minion colours: a mixed crew.
pub const COLORS: [[u8; 3]; 4] = [
    [40, 120, 220],
    [30, 160, 80],
    [230, 140, 20],
    [150, 60, 200],
];
/// How many come.
pub const SQUAD: usize = 3;
/// Size relative to the figure.
const SMALL: f32 = 0.6;
/// They attack for this long, then go home.
const ATTACK_FOR: f32 = 25.0;
/// Seconds a knocked-out minion lies there before it puffs away.
const POOF_AFTER: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Entering,
    Attacking,
    Leaving { x: f32 },
    Gone,
}

/// What happened, for effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cue {
    Landed {
        at: V2,
    },
    /// A minion hit the cursor.
    StruckCursor {
        at: V2,
        dir: V2,
    },
    /// A click knocked one out and it vanished.
    Poofed {
        at: V2,
    },
    /// Some other figure event (dust, jumps, whooshes), for particles.
    Other(Event),
}

pub struct Minion {
    pub agent: Agent,
    pub rgb: [u8; 3],
    pub phase: Phase,
    t: f32,
    ko_for: f32,
    clicked: bool,
}

pub struct Squad {
    pub minions: Vec<Minion>,
}

impl Squad {
    /// A squad dropping in at `xs`, from under `top`, onto `ground`.
    pub fn new(xs: &[f32], top: f32, ground: Platform, scale: f32, seed: u64) -> Squad {
        let minions = xs
            .iter()
            .enumerate()
            .map(|(i, &x)| {
                let s = scale * SMALL;
                let mut agent = Agent::new(x, ground, s, seed.wrapping_add(i as u64 * 7919));
                agent.traits = Traits {
                    naps: false,
                    dancing: false,
                    doodles: false,
                    sitting: false,
                    powers: false,
                    telekinesis: false,
                    aggression: 0.95,
                    max_health: 30.0,
                    speed: 1.25,
                };
                agent.health = 30.0;
                agent.snatch_chance = 0.0;
                // Staggered, so they land one after another.
                agent.drop_in(v2(x, top + 40.0 + 120.0 * i as f32));
                Minion {
                    agent,
                    rgb: COLORS[i % COLORS.len()],
                    phase: Phase::Entering,
                    t: 0.0,
                    ko_for: 0.0,
                    clicked: false,
                }
            })
            .collect();
        Squad { minions }
    }

    /// Everyone has gone.
    pub fn done(&self) -> bool {
        self.minions.iter().all(|m| m.phase == Phase::Gone)
    }

    /// The minion under `p`, if any.
    pub fn hit_test(&self, p: V2) -> Option<usize> {
        self.minions
            .iter()
            .position(|m| m.phase != Phase::Gone && m.agent.hit_test(p))
    }

    /// The user clicked minion `i`.
    pub fn click(&mut self, i: usize) {
        if let Some(m) = self.minions.get_mut(i) {
            m.clicked = true;
        }
    }

    /// Send them all home now.
    pub fn retreat(&mut self, min_x: f32, max_x: f32) {
        for m in &mut self.minions {
            if matches!(m.phase, Phase::Entering | Phase::Attacking) {
                m.phase = Phase::Leaving {
                    x: nearer_edge(m.agent.pos.x, min_x, max_x),
                };
                m.t = 0.0;
            }
        }
    }

    /// One fixed step. `base` is an ordinary set of senses (cursor, walking
    /// range); each minion fights in combat mode with it.
    pub fn step(&mut self, world: &World, base: &Senses) -> Vec<Cue> {
        let mut cues = Vec::new();
        for m in &mut self.minions {
            m.t += crate::agent::FIXED_DT;
            let s = Senses {
                clicked: std::mem::take(&mut m.clicked),
                mode: match m.phase {
                    Phase::Attacking => Mode::Combat,
                    _ => Mode::Idle,
                },
                active_window: None,
                ammo: None,
                can_grab_cursor: false,
                held: false,
                user_idle: 0.0,
                can_shove: false,
                tk_words_near: 0,
                music: false,
                can_throw_window: false,
                weapon_available: false,
                ..*base
            };
            if m.phase == Phase::Gone {
                continue;
            }
            let mut ev = Vec::new();
            m.agent.scripted = matches!(m.phase, Phase::Entering | Phase::Leaving { .. });
            m.agent.update(&s, world, &mut ev);
            for e in ev {
                cues.push(match e {
                    Event::StruckCursor { at, dir } => Cue::StruckCursor { at, dir },
                    other => Cue::Other(other),
                });
            }
            // Knocked out: a moment on the floor, then a puff of smoke.
            if m.agent.knocked_out() {
                m.ko_for += crate::agent::FIXED_DT;
                if m.ko_for > POOF_AFTER {
                    m.phase = Phase::Gone;
                    cues.push(Cue::Poofed {
                        at: m.agent.center(),
                    });
                }
                continue;
            }
            match m.phase {
                Phase::Entering => {
                    if m.agent.ground.is_some() && m.t > 0.3 {
                        m.phase = Phase::Attacking;
                        m.t = 0.0;
                        cues.push(Cue::Landed { at: m.agent.pos });
                    }
                }
                Phase::Attacking => {
                    if m.t > ATTACK_FOR {
                        m.phase = Phase::Leaving {
                            x: nearer_edge(m.agent.pos.x, base.min_x, base.max_x),
                        };
                        m.t = 0.0;
                    }
                }
                Phase::Leaving { x } => {
                    m.agent.go_to(x);
                    if (m.agent.pos.x - x).abs() < 4.0 || m.t > 15.0 {
                        m.phase = Phase::Gone;
                    }
                }
                Phase::Gone => {}
            }
        }
        cues
    }
}

fn nearer_edge(x: f32, min_x: f32, max_x: f32) -> f32 {
    if x - min_x < max_x - x {
        min_x
    } else {
        max_x
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::FIXED_DT;

    fn senses(cursor: V2) -> Senses {
        Senses {
            cursor,
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
    fn minions_land_attack_the_cursor_and_go_home() {
        let ground = Platform::ground(1800.0, 1100.0);
        let mut w = World::new();
        w.set_platforms(vec![ground]);
        let mut sq = Squad::new(&[300.0, 900.0, 1500.0], 0.0, ground, 0.55, 5);
        let cursor = v2(900.0, 1060.0);
        let mut landed = 0;
        let mut hits = 0;
        for _ in 0..(60.0 / FIXED_DT) as usize {
            for c in sq.step(&w, &senses(cursor)) {
                match c {
                    Cue::Landed { .. } => landed += 1,
                    Cue::StruckCursor { .. } => hits += 1,
                    _ => {}
                }
            }
            if sq.done() {
                break;
            }
        }
        assert_eq!(landed, 3);
        assert!(hits > 0, "they gang up on the cursor");
        assert!(sq.done(), "and leave after a while");
    }

    #[test]
    fn a_click_knocks_a_minion_out_and_it_vanishes() {
        let ground = Platform::ground(1800.0, 1100.0);
        let mut w = World::new();
        w.set_platforms(vec![ground]);
        let mut sq = Squad::new(&[900.0], 0.0, ground, 0.55, 9);
        let far = v2(100.0, 200.0);
        for _ in 0..120 {
            sq.step(&w, &senses(far));
        }
        assert_eq!(sq.minions[0].phase, Phase::Attacking);
        let mut poofed = false;
        for _ in 0..6 {
            let at = sq.minions[0].agent.center();
            if sq.hit_test(at).is_some() {
                sq.click(0);
            }
            for _ in 0..90 {
                poofed |= sq
                    .step(&w, &senses(at))
                    .iter()
                    .any(|c| matches!(c, Cue::Poofed { .. }));
            }
            if poofed {
                break;
            }
        }
        assert!(poofed, "two clicks are enough");
        assert!(sq.done());
    }
}
