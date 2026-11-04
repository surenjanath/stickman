//! Its war on the cursor: the cursor's health (kept between launches), what
//! each way of fighting has achieved so far, and the judgement calls built on
//! that: which tactic to use next, when to start a fight on its own, and when
//! to fall back and recover.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agent::Tactic;
use crate::boss::Blow;

/// The cursor's health in the first round.
pub const START_HP: f32 = 120.0;
/// It comes back stronger each time, up to this.
const MAX_HP: f32 = 600.0;
/// How much stronger it comes back.
const COMEBACK: f32 = 1.25;
/// Seconds with one tactic before it takes stock.
pub const STINT: f32 = 20.0;
/// Shorter stints than this don't count (the fight barely started).
const MIN_STINT: f32 = 3.0;
/// How much a hit from the user costs, in damage dealt.
const HURT_COST: f32 = 2.0;
/// No tactic's chance of being picked ever drops below this.
pub const MIN_ODDS: f32 = 0.05;
/// How sharply results tilt the odds: a tactic that does this much more net
/// damage per second than another is e (≈2.7) times as likely.
const TILT: f32 = 1.0;
/// Stints before a tactic's results count fully (until then its odds stay
/// close to the even share).
const TRUST_STINTS: f32 = 3.0;

/// What one tactic has achieved over all its stints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub stints: u32,
    pub secs: f32,
    pub damage: f32,
    /// Times the user hit it meanwhile.
    pub hurt: u32,
}

impl Record {
    /// Damage per second, net of hits taken. A small prior keeps one lucky (or
    /// unlucky) stint from deciding everything.
    pub fn rate(&self) -> f32 {
        (self.damage + 2.0 - self.hurt as f32 * HURT_COST) / (self.secs + 10.0)
    }
}

/// The tactic in use right now, and how it's going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stint {
    pub tactic: Tactic,
    pub secs: f32,
    pub damage: f32,
    pub hurt: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct War {
    pub cursor_hp: f32,
    pub cursor_max: f32,
    /// Times it has destroyed the cursor.
    pub victories: u32,
    /// By tactic name.
    pub record: BTreeMap<String, Record>,
    #[serde(skip)]
    pub stint: Option<Stint>,
}

impl Default for War {
    fn default() -> Self {
        War {
            cursor_hp: START_HP,
            cursor_max: START_HP,
            victories: 0,
            record: BTreeMap::new(),
            stint: None,
        }
    }
}

impl War {
    /// The plan right now (`Any` between fights).
    pub fn tactic(&self) -> Tactic {
        self.stint.map_or(Tactic::Any, |s| s.tactic)
    }

    pub fn fraction(&self) -> f32 {
        (self.cursor_hp / self.cursor_max.max(1.0)).clamp(0.0, 1.0)
    }

    pub fn record_of(&self, t: Tactic) -> Record {
        self.record.get(t.name()).copied().unwrap_or_default()
    }

    /// A blow landed on the cursor. Returns whether that destroyed it (then it
    /// comes back, stronger).
    pub fn blow(&mut self, b: Blow) -> bool {
        let d = b.damage();
        if let Some(s) = &mut self.stint {
            s.damage += d;
        }
        self.cursor_hp -= d;
        if self.cursor_hp > 0.0 {
            return false;
        }
        self.victories += 1;
        self.cursor_max = (self.cursor_max * COMEBACK).min(MAX_HP);
        self.cursor_hp = self.cursor_max;
        true
    }

    /// The user hit it.
    pub fn hurt(&mut self) {
        if let Some(s) = &mut self.stint {
            s.hurt += 1;
        }
    }

    pub fn begin(&mut self, t: Tactic) {
        self.stint = Some(Stint {
            tactic: t,
            secs: 0.0,
            damage: 0.0,
            hurt: 0,
        });
    }

    /// Advance the current stint. Returns it when it's over: time to take
    /// stock (still fighting) or the fight ended.
    pub fn tick(&mut self, dt: f32, fighting: bool) -> Option<Stint> {
        let s = self.stint.as_mut()?;
        if fighting {
            s.secs += dt;
            if s.secs < STINT {
                return None;
            }
        }
        self.end()
    }

    fn end(&mut self) -> Option<Stint> {
        let s = self.stint.take()?;
        if s.secs >= MIN_STINT {
            let r = self.record.entry(s.tactic.name().to_string()).or_default();
            r.stints += 1;
            r.secs += s.secs;
            r.damage += s.damage;
            r.hurt += s.hurt;
        }
        Some(s)
    }

    /// The chance of picking each of `available` next. It starts fair (an
    /// equal share each), tilts toward what has done more net damage per
    /// second as the record grows (a tactic's results count for more the
    /// more stints it has had), and never drops below `MIN_ODDS`.
    pub fn odds(&self, available: &[Tactic]) -> Vec<(Tactic, f32)> {
        let n = available.len();
        if n == 0 {
            return Vec::new();
        }
        let even = 1.0 / n as f32;
        // How results alone would share it out (softmax of the rates).
        let rates: Vec<f32> = available
            .iter()
            .map(|t| self.record_of(*t).rate())
            .collect();
        let top = rates.iter().copied().fold(f32::MIN, f32::max);
        let pull: Vec<f32> = rates.iter().map(|r| ((r - top) / TILT).exp()).collect();
        let total: f32 = pull.iter().sum();
        // Blend with the even share by how much each record can be trusted.
        let mut p: Vec<f32> = available
            .iter()
            .zip(&pull)
            .map(|(t, w)| {
                let k = self.record_of(*t).stints as f32;
                let trust = k / (k + TRUST_STINTS);
                (1.0 - trust) * even + trust * w / total
            })
            .collect();
        normalize(&mut p);
        floor_odds(&mut p, MIN_ODDS.min(even));
        available.iter().copied().zip(p).collect()
    }

    /// Pick a tactic from `available` at random by its [`War::odds`] (`roll`
    /// is a uniform random number in 0..1).
    pub fn choose(&self, available: &[Tactic], roll: f32) -> Tactic {
        let odds = self.odds(available);
        let mut acc = 0.0;
        for (t, p) in &odds {
            acc += p;
            if roll < acc {
                return *t;
            }
        }
        odds.last().map_or(Tactic::Any, |(t, _)| *t)
    }

    /// The odds in words: "words 46% · blade 30% · fists 8%".
    pub fn odds_text(&self, available: &[Tactic]) -> String {
        let mut o = self.odds(available);
        o.sort_by(|a, b| b.1.total_cmp(&a.1));
        o.iter()
            .map(|(t, p)| format!("{} {:.0}%", t.name(), p * 100.0))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Its best and worst tactics so far (each tried at least twice).
    fn best_and_worst(&self) -> Option<(Tactic, Tactic)> {
        let known: Vec<(Tactic, f32)> = Tactic::ALL
            .iter()
            .map(|t| (*t, self.record_of(*t)))
            .filter(|(_, r)| r.stints >= 2)
            .map(|(t, r)| (t, r.rate()))
            .collect();
        let best = known.iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
        let worst = known.iter().min_by(|a, b| a.1.total_cmp(&b.1))?;
        Some((best.0, worst.0))
    }

    /// The war in words, for the model.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "Your war on the cursor: you have destroyed it {} times; it now has {:.0}/{:.0} health.",
            self.victories, self.cursor_hp, self.cursor_max
        );
        let tried: Vec<String> = Tactic::ALL
            .iter()
            .filter_map(|t| {
                let r = self.record_of(*t);
                (r.stints > 0).then(|| {
                    format!(
                        "{} {:.1} damage/s over {} tries, hit {} times",
                        t.describe(),
                        (r.damage / r.secs.max(1.0)),
                        r.stints,
                        r.hurt
                    )
                })
            })
            .collect();
        if !tried.is_empty() {
            s.push_str(&format!(" What has worked: {}.", tried.join("; ")));
        }
        if let Some((best, worst)) = self.best_and_worst().filter(|(b, w)| b != w) {
            s.push_str(&format!(
                " Your best weapon is {}; {} works worst.",
                best.describe(),
                worst.describe()
            ));
        }
        s
    }

    /// What it says about switching from `done` to `next`, if anything.
    pub fn verdict(done: &Stint, next: Tactic) -> Option<String> {
        let rate = done.damage / done.secs.max(1.0);
        let worked = done.damage >= 20.0 || rate >= 1.0;
        let flopped = done.secs >= 8.0 && done.damage < 8.0;
        if done.tactic == next {
            worked.then(|| format!("{} works. More of that.", capital(next.describe())))
        } else if flopped {
            Some(format!(
                "{} isn't working. Switching to {}.",
                capital(done.tactic.describe()),
                next.describe()
            ))
        } else if done.hurt >= 3 {
            Some(format!("Too many hits. Trying {}.", next.describe()))
        } else {
            None
        }
    }
}

fn normalize(p: &mut [f32]) {
    let sum: f32 = p.iter().sum();
    if sum > 0.0 {
        p.iter_mut().for_each(|v| *v /= sum);
    }
}

/// Raise every share below `floor` to it, taking the difference from the
/// others in proportion (repeated, since that can push another under).
fn floor_odds(p: &mut [f32], floor: f32) {
    for _ in 0..p.len() {
        let low: f32 = p.iter().filter(|v| **v <= floor).count() as f32 * floor;
        let rest: f32 = p.iter().filter(|v| **v > floor).sum();
        if rest <= 0.0 {
            break;
        }
        let scale = (1.0 - low) / rest;
        let mut again = false;
        for v in p.iter_mut() {
            if *v <= floor {
                *v = floor;
            } else {
                *v *= scale;
                again |= *v < floor;
            }
        }
        if !again {
            break;
        }
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// What it weighs before picking a fight on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Odds {
    /// Its own health, 0..1.
    pub health: f32,
    pub cursor_dist: f32,
    /// Cursor speed, points/s.
    pub cursor_speed: f32,
    /// The user moved the mouse lately (no point attacking an empty room).
    pub user_active: bool,
    /// Seconds since it last started a fight on its own.
    pub since_last: f32,
    /// The temper slider, 0..1.
    pub aggression: f32,
    /// 0..100.
    pub resentment: f32,
    /// Scared of the app in front.
    pub scared: bool,
}

/// Seconds between fights it starts on its own: ~45 s when it's fierce.
pub fn initiative_gap(aggression: f32) -> f32 {
    45.0 + 90.0 * (1.0 - aggression.clamp(0.0, 1.0))
}

/// Should it start a fight now (checked about once a second)? Only when the
/// odds look good: healthy, the user around, and the cursor close or idle.
pub fn take_initiative(o: &Odds, roll: f32) -> bool {
    if !o.user_active
        || o.scared
        || o.aggression < 0.05
        || o.health < 0.7
        || o.since_last < initiative_gap(o.aggression)
    {
        return false;
    }
    let close = o.cursor_dist < 600.0;
    let still = o.cursor_speed < 120.0;
    if !(close || still) {
        return false;
    }
    let chance =
        0.08 + 0.3 * o.aggression + o.resentment / 300.0 + if close && still { 0.15 } else { 0.0 };
    roll < chance
}

/// Fall back below 30% health, return to the fight above 70%. Never while
/// enraged: rage doesn't retreat.
pub fn retreat(retreating: bool, health: f32, enraged: bool) -> bool {
    !enraged
        && if retreating {
            health < 0.7
        } else {
            health < 0.3
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destroying_the_cursor_brings_it_back_stronger() {
        let mut w = War::default();
        let mut blows = 0;
        while !w.blow(Blow::Thrown) {
            blows += 1;
            assert!(blows < 100);
        }
        assert_eq!(w.victories, 1);
        assert!(w.cursor_max > START_HP);
        assert_eq!(w.cursor_hp, w.cursor_max);
        for _ in 0..50 {
            while !w.blow(Blow::Thrown) {}
        }
        assert_eq!(w.cursor_max, MAX_HP);
    }

    /// Fight one stint with `t`, landing `blows` thrown words and taking `hits`.
    fn stint(w: &mut War, t: Tactic, blows: u32, hits: u32) {
        w.begin(t);
        for _ in 0..blows {
            w.blow(Blow::Word);
        }
        for _ in 0..hits {
            w.hurt();
        }
        let mut done = None;
        while done.is_none() {
            done = w.tick(0.5, true);
        }
    }

    #[test]
    fn it_learns_which_tactic_works_and_mostly_sticks_to_it() {
        let mut w = War::default();
        let all = Tactic::ALL;
        let odds = |w: &War, t: Tactic| w.odds(&all).iter().find(|o| o.0 == t).unwrap().1;
        // Fair to begin with: an equal share each.
        for t in all {
            assert!((odds(&w, t) - 0.2).abs() < 1e-4);
        }
        for _ in 0..3 {
            stint(&mut w, Tactic::Fists, 0, 3);
            stint(&mut w, Tactic::Words, 6, 0);
            stint(&mut w, Tactic::Grab, 1, 1);
            stint(&mut w, Tactic::Powers, 1, 0);
            stint(&mut w, Tactic::Blade, 0, 0);
        }
        // Words worked best: it's the favourite, fists the least likely, and
        // nothing is ever below 5%.
        let words = odds(&w, Tactic::Words);
        assert!(all
            .iter()
            .all(|t| *t == Tactic::Words || odds(&w, *t) < words));
        assert!(odds(&w, Tactic::Fists) < odds(&w, Tactic::Blade));
        let sum: f32 = w.odds(&all).iter().map(|o| o.1).sum();
        assert!((sum - 1.0).abs() < 1e-4);
        // A runaway favourite still leaves everything else at least 5%.
        for _ in 0..40 {
            stint(&mut w, Tactic::Words, 40, 0);
        }
        for t in all {
            assert!(odds(&w, t) >= MIN_ODDS - 1e-4, "{t:?} at {}", odds(&w, t));
        }
        assert!(odds(&w, Tactic::Words) > 0.55);
        // Once the losers have a long record too, the floor is what holds them up.
        for _ in 0..40 {
            for t in [Tactic::Fists, Tactic::Grab, Tactic::Powers, Tactic::Blade] {
                stint(&mut w, t, 0, 0);
            }
        }
        for t in all {
            assert!(odds(&w, t) >= MIN_ODDS - 1e-4, "{t:?} at {}", odds(&w, t));
        }
        assert!((odds(&w, Tactic::Fists) - MIN_ODDS).abs() < 0.01);
        assert!((odds(&w, Tactic::Words) - 0.8).abs() < 0.01);
        // The pick follows the odds: the roll walks the shares in order.
        assert_eq!(w.choose(&all, 0.0), Tactic::Fists);
        assert_eq!(w.choose(&all, 0.5), Tactic::Words);
        assert_eq!(w.choose(&all, 0.999), Tactic::Blade);
        // Only what's available.
        assert_ne!(w.choose(&[Tactic::Fists, Tactic::Grab], 0.5), Tactic::Words);
        assert!(w.odds_text(&all).starts_with("words "));
        assert!(w.summary().contains("best weapon is throwing words"));
    }

    #[test]
    fn short_stints_dont_count_and_ending_a_fight_ends_the_stint() {
        let mut w = War::default();
        w.begin(Tactic::Fists);
        w.tick(1.0, true);
        assert!(w.tick(0.1, false).is_some());
        assert_eq!(w.tactic(), Tactic::Any);
        assert_eq!(w.record_of(Tactic::Fists).stints, 0);
    }

    #[test]
    fn it_says_when_a_plan_flops() {
        let flop = Stint {
            tactic: Tactic::Fists,
            secs: 20.0,
            damage: 0.0,
            hurt: 0,
        };
        assert_eq!(
            War::verdict(&flop, Tactic::Words).as_deref(),
            Some("My fists isn't working. Switching to throwing words.")
        );
        let win = Stint {
            damage: 40.0,
            ..flop
        };
        assert_eq!(
            War::verdict(&win, Tactic::Fists).as_deref(),
            Some("My fists works. More of that.")
        );
        assert_eq!(War::verdict(&win, Tactic::Words), None);
    }

    #[test]
    fn it_picks_its_moments() {
        let good = Odds {
            health: 1.0,
            cursor_dist: 300.0,
            cursor_speed: 50.0,
            user_active: true,
            since_last: 200.0,
            aggression: 0.6,
            resentment: 20.0,
            scared: false,
        };
        assert!(take_initiative(&good, 0.3));
        assert!(!take_initiative(&good, 0.99), "not every time");
        for bad in [
            Odds {
                health: 0.5,
                ..good
            },
            Odds {
                user_active: false,
                ..good
            },
            Odds {
                scared: true,
                ..good
            },
            Odds {
                since_last: 10.0,
                ..good
            },
            Odds {
                aggression: 0.0,
                ..good
            },
            Odds {
                cursor_dist: 1500.0,
                cursor_speed: 900.0,
                ..good
            },
        ] {
            assert!(!take_initiative(&bad, 0.0), "{bad:?}");
        }
    }

    #[test]
    fn it_falls_back_when_hurt_and_returns_when_healed() {
        assert!(!retreat(false, 0.5, false));
        assert!(retreat(false, 0.2, false));
        assert!(retreat(true, 0.5, false));
        assert!(!retreat(true, 0.8, false));
        assert!(!retreat(false, 0.1, true), "rage doesn't retreat");
    }
}
