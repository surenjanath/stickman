//! The figure's record, kept between launches: experience and level, and a
//! grudge against the user that grows when they hit it and fades with time.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Grudge gained per hit, and lost per minute.
const GRUDGE_PER_HIT: f32 = 6.0;
const GRUDGE_FADE_PER_MIN: f32 = 2.0;

/// Things that earn experience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feat {
    HitCursor,
    WordHit,
    ThrewCursor,
    WonDuel,
    Petted,
}

impl Feat {
    fn xp(self) -> u32 {
        match self {
            Feat::HitCursor => 2,
            Feat::WordHit => 3,
            Feat::ThrewCursor => 6,
            Feat::WonDuel => 25,
            Feat::Petted => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Stats {
    pub xp: u32,
    /// Times the user hit it.
    pub hits_taken: u32,
    pub knockouts: u32,
    pub duels_won: u32,
    pub duels_lost: u32,
    /// 0 (forgiven) .. 100 (seething).
    pub grudge: f32,
}

impl Stats {
    /// Level from experience: 1, 2 at 25 xp, 3 at 100, 4 at 225, …
    pub fn level(&self) -> u32 {
        1 + ((self.xp as f32 / 25.0).sqrt()) as u32
    }

    /// A name for its rank.
    pub fn title(&self) -> &'static str {
        match self.level() {
            1 => "Doodle",
            2..=3 => "Scrapper",
            4..=6 => "Brawler",
            7..=9 => "Champion",
            _ => "Legend",
        }
    }

    /// Earn experience. Returns the new level if it went up.
    pub fn gain(&mut self, f: Feat) -> Option<u32> {
        let before = self.level();
        self.xp = self.xp.saturating_add(f.xp());
        (self.level() > before).then(|| self.level())
    }

    /// The user hit it.
    pub fn hurt(&mut self, knocked_out: bool) {
        self.hits_taken += 1;
        self.knockouts += u32::from(knocked_out);
        self.grudge = (self.grudge + GRUDGE_PER_HIT).min(100.0);
    }

    /// Time heals (a little).
    pub fn fade(&mut self, dt: f32) {
        self.grudge = (self.grudge - GRUDGE_FADE_PER_MIN * dt / 60.0).max(0.0);
    }

    /// Extra health from levels.
    pub fn max_health(&self) -> f32 {
        (100.0 + 8.0 * (self.level() - 1) as f32).min(180.0)
    }

    /// Walking/running speed multiplier from levels.
    pub fn speed(&self) -> f32 {
        (1.0 + 0.03 * (self.level() - 1) as f32).min(1.3)
    }

    /// Next to the settings file.
    pub fn path_next_to(settings: &Path) -> PathBuf {
        settings.with_file_name("stats.json")
    }

    pub fn load(path: &Path) -> Stats {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_come_from_experience() {
        let mut s = Stats::default();
        assert_eq!((s.level(), s.title()), (1, "Doodle"));
        let mut ups = vec![];
        for _ in 0..60 {
            if let Some(l) = s.gain(Feat::HitCursor) {
                ups.push(l);
            }
        }
        assert_eq!(s.xp, 120);
        assert_eq!(ups, [2, 3]);
        s.xp = 10_000;
        assert_eq!(s.title(), "Legend");
        assert_eq!(s.max_health(), 180.0);
        assert_eq!(s.speed(), 1.3);
    }

    #[test]
    fn grudges_grow_with_hits_and_fade_slowly() {
        let mut s = Stats::default();
        for _ in 0..30 {
            s.hurt(false);
        }
        assert_eq!(s.grudge, 100.0);
        s.fade(600.0); // ten minutes
        assert!((s.grudge - 80.0).abs() < 0.01);
        s.fade(1e6);
        assert_eq!(s.grudge, 0.0);
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("stickman-stats-{}", std::process::id()));
        let path = dir.join("stats.json");
        let s = Stats {
            xp: 321,
            grudge: 42.0,
            duels_won: 3,
            ..Stats::default()
        };
        s.save(&path).unwrap();
        assert_eq!(Stats::load(&path), s);
        assert_eq!(Stats::load(&dir.join("missing.json")), Stats::default());
        std::fs::remove_dir_all(dir).ok();
    }
}
