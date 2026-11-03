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
    /// Running counts of deeds, by name ("words_thrown", …).
    pub counts: std::collections::BTreeMap<String, u32>,
    /// Achievements earned: id → unix time.
    pub achievements: std::collections::BTreeMap<String, u64>,
}

/// Something worth counting (for achievements).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deed {
    CursorHit,
    CursorThrown,
    WordThrown,
    IconThrown,
    WindowThrown,
    SwordHit,
    Note,
    Petted,
    Enraged,
    BossWon,
    BossLost,
    Minions,
}

impl Deed {
    fn key(self) -> &'static str {
        match self {
            Deed::CursorHit => "cursor_hits",
            Deed::CursorThrown => "cursor_throws",
            Deed::WordThrown => "words_thrown",
            Deed::IconThrown => "icons_thrown",
            Deed::WindowThrown => "windows_thrown",
            Deed::SwordHit => "sword_hits",
            Deed::Note => "notes",
            Deed::Petted => "petted",
            Deed::Enraged => "rages",
            Deed::BossWon => "boss_wins",
            Deed::BossLost => "boss_losses",
            Deed::Minions => "minion_summons",
        }
    }
}

/// An achievement: id, name, how to earn it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Achievement {
    pub id: &'static str,
    pub name: &'static str,
    pub how: &'static str,
}

pub const ACHIEVEMENTS: [Achievement; 17] = [
    Achievement {
        id: "first_blood",
        name: "First Blood",
        how: "land a blow on the cursor",
    },
    Achievement {
        id: "brawler",
        name: "Brawler",
        how: "hit the cursor 50 times",
    },
    Achievement {
        id: "cursor_tosser",
        name: "Cursor Tosser",
        how: "throw the cursor 3 times",
    },
    Achievement {
        id: "pickpocket",
        name: "Pickpocket",
        how: "throw 10 words",
    },
    Achievement {
        id: "icon_thief",
        name: "Icon Thief",
        how: "throw 5 Dock icons",
    },
    Achievement {
        id: "window_wrecker",
        name: "Window Wrecker",
        how: "throw a window",
    },
    Achievement {
        id: "swordsman",
        name: "Swordsman",
        how: "land 8 sword hits",
    },
    Achievement {
        id: "duelist",
        name: "Duelist",
        how: "win a duel",
    },
    Achievement {
        id: "desk_champion",
        name: "Desk Champion",
        how: "win 3 duels",
    },
    Achievement {
        id: "punching_bag",
        name: "Punching Bag",
        how: "get knocked out 5 times",
    },
    Achievement {
        id: "beloved",
        name: "Beloved",
        how: "be petted 20 times",
    },
    Achievement {
        id: "seething",
        name: "Seething",
        how: "lose its temper",
    },
    Achievement {
        id: "pen_pal",
        name: "Pen Pal",
        how: "leave 5 notes",
    },
    Achievement {
        id: "philosopher",
        name: "Philosopher",
        how: "reflect 10 times",
    },
    Achievement {
        id: "veteran",
        name: "Veteran",
        how: "reach level 5",
    },
    Achievement {
        id: "boss_slayer",
        name: "Boss Slayer",
        how: "beat you in a boss fight",
    },
    Achievement {
        id: "general",
        name: "General",
        how: "summon a minion army",
    },
];

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

    pub fn count(&self, d: Deed) -> u32 {
        self.counts.get(d.key()).copied().unwrap_or(0)
    }

    pub fn add(&mut self, d: Deed) {
        *self.counts.entry(d.key().to_string()).or_default() += 1;
    }

    pub fn has(&self, id: &str) -> bool {
        self.achievements.contains_key(id)
    }

    /// Achievements newly earned (given how many reflections it has done).
    /// Records them and returns them, so the app can celebrate.
    pub fn check_achievements(&mut self, reflections: u32) -> Vec<Achievement> {
        let met = |id: &str, s: &Stats| -> bool {
            let c = |d| s.count(d);
            match id {
                "first_blood" => c(Deed::CursorHit) >= 1,
                "brawler" => c(Deed::CursorHit) >= 50,
                "cursor_tosser" => c(Deed::CursorThrown) >= 3,
                "pickpocket" => c(Deed::WordThrown) >= 10,
                "icon_thief" => c(Deed::IconThrown) >= 5,
                "window_wrecker" => c(Deed::WindowThrown) >= 1,
                "swordsman" => c(Deed::SwordHit) >= 8,
                "duelist" => s.duels_won >= 1,
                "desk_champion" => s.duels_won >= 3,
                "punching_bag" => s.knockouts >= 5,
                "beloved" => c(Deed::Petted) >= 20,
                "seething" => c(Deed::Enraged) >= 1,
                "pen_pal" => c(Deed::Note) >= 5,
                "philosopher" => reflections >= 10,
                "veteran" => s.level() >= 5,
                "boss_slayer" => c(Deed::BossWon) >= 1,
                "general" => c(Deed::Minions) >= 1,
                _ => false,
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut new = Vec::new();
        for a in ACHIEVEMENTS {
            if !self.has(a.id) && met(a.id, self) {
                self.achievements.insert(a.id.to_string(), now);
                new.push(a);
            }
        }
        new
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
    fn achievements_are_earned_once() {
        let mut s = Stats::default();
        assert!(s.check_achievements(0).is_empty());
        s.add(Deed::CursorHit);
        let new = s.check_achievements(0);
        assert_eq!(
            new.iter().map(|a| a.id).collect::<Vec<_>>(),
            ["first_blood"]
        );
        assert!(s.check_achievements(0).is_empty(), "only once");
        for _ in 0..10 {
            s.add(Deed::WordThrown);
        }
        s.duels_won = 3;
        let ids: Vec<_> = s.check_achievements(12).iter().map(|a| a.id).collect();
        assert_eq!(
            ids,
            ["pickpocket", "duelist", "desk_champion", "philosopher"]
        );
        assert!(ACHIEVEMENTS.iter().all(|a| !a.how.is_empty()));
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
