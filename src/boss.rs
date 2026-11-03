//! Boss fights: the cursor becomes a boss with a health bar. The figure
//! fights it with everything it has; the user fights back by clicking the
//! figure. Whoever runs out of health first loses; time runs out after 90 s.

/// The cursor's health.
pub const CURSOR_HP: f32 = 100.0;
/// The banner shows this long before the fight starts.
const INTRO: f32 = 2.5;
/// The fight ends in a draw after this long.
const LIMIT: f32 = 90.0;
/// The result shows this long.
const OUTRO: f32 = 3.5;

/// A blow that lands on the cursor boss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blow {
    Punch,
    Word,
    Thrown,
    Sword,
    Minion,
}

impl Blow {
    pub fn damage(self) -> f32 {
        match self {
            Blow::Punch => 7.0,
            Blow::Word => 9.0,
            Blow::Thrown => 15.0,
            Blow::Sword => 10.0,
            Blow::Minion => 3.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The figure beat the cursor.
    Won,
    /// The user knocked the figure out.
    Lost,
    Draw,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Intro,
    Fight,
    Over(Outcome),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Boss {
    pub cursor_hp: f32,
    pub phase: Phase,
    /// Seconds in the current phase.
    pub t: f32,
    /// Seconds since the cursor last took a hit (for the flash).
    pub since_hit: f32,
}

impl Default for Boss {
    fn default() -> Self {
        Boss {
            cursor_hp: CURSOR_HP,
            phase: Phase::Intro,
            t: 0.0,
            since_hit: 99.0,
        }
    }
}

impl Boss {
    pub fn fighting(&self) -> bool {
        self.phase == Phase::Fight
    }

    /// Finished showing its result: remove it.
    pub fn done(&self) -> bool {
        matches!(self.phase, Phase::Over(_)) && self.t > OUTRO
    }

    pub fn fraction(&self) -> f32 {
        (self.cursor_hp / CURSOR_HP).clamp(0.0, 1.0)
    }

    /// A blow landed on the cursor (ignored outside the fight).
    pub fn hit(&mut self, b: Blow) {
        if self.fighting() {
            self.cursor_hp = (self.cursor_hp - b.damage()).max(0.0);
            self.since_hit = 0.0;
        }
    }

    /// Advance; `figure_down` is whether the figure is knocked out. Returns
    /// the outcome the moment the fight ends.
    pub fn update(&mut self, dt: f32, figure_down: bool) -> Option<Outcome> {
        self.t += dt;
        self.since_hit += dt;
        match self.phase {
            Phase::Intro if self.t > INTRO => {
                self.phase = Phase::Fight;
                self.t = 0.0;
                None
            }
            Phase::Fight => {
                let end = if self.cursor_hp <= 0.0 {
                    Some(Outcome::Won)
                } else if figure_down {
                    Some(Outcome::Lost)
                } else if self.t > LIMIT {
                    Some(Outcome::Draw)
                } else {
                    None
                };
                if let Some(o) = end {
                    self.phase = Phase::Over(o);
                    self.t = 0.0;
                }
                end
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blows_only_count_during_the_fight() {
        let mut b = Boss::default();
        b.hit(Blow::Thrown);
        assert_eq!(b.cursor_hp, CURSOR_HP, "not during the intro");
        for _ in 0..200 {
            b.update(1.0 / 60.0, false);
        }
        assert!(b.fighting());
        let mut out = None;
        while out.is_none() {
            b.hit(Blow::Punch);
            out = b.update(1.0 / 60.0, false);
        }
        assert_eq!(out, Some(Outcome::Won));
        assert_eq!(b.fraction(), 0.0);
        assert!(!b.done());
        for _ in 0..300 {
            b.update(1.0 / 60.0, false);
        }
        assert!(b.done());
    }

    #[test]
    fn a_knockout_loses_and_time_runs_out() {
        let mut b = Boss::default();
        for _ in 0..200 {
            b.update(1.0 / 60.0, false);
        }
        assert_eq!(b.update(1.0 / 60.0, true), Some(Outcome::Lost));
        let mut d = Boss::default();
        let mut out = None;
        for _ in 0..(100 * 60) {
            out = out.or(d.update(1.0 / 60.0, false));
        }
        assert_eq!(out, Some(Outcome::Draw));
    }
}
