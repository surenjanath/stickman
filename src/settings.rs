//! User settings, edited from the menu bar and saved to
//! `~/Library/Application Support/Stickman/settings.json` (or `$STICKMAN_SETTINGS`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::Mode;

/// Behaviour selection: let the AI director decide, or pin one mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Behaviour {
    #[default]
    Auto,
    Idle,
    Observe,
    Sabotage,
    Combat,
}

impl Behaviour {
    pub const ALL: [Behaviour; 5] = [
        Behaviour::Auto,
        Behaviour::Idle,
        Behaviour::Observe,
        Behaviour::Sabotage,
        Behaviour::Combat,
    ];

    /// The pinned mode, or `None` for the AI director.
    pub fn forced_mode(self) -> Option<Mode> {
        match self {
            Behaviour::Auto => None,
            Behaviour::Idle => Some(Mode::Idle),
            Behaviour::Observe => Some(Mode::Observe),
            Behaviour::Sabotage => Some(Mode::Sabotage),
            Behaviour::Combat => Some(Mode::Combat),
        }
    }

    pub fn from_mode(m: Mode) -> Behaviour {
        match m {
            Mode::Idle => Behaviour::Idle,
            Mode::Observe => Behaviour::Observe,
            Mode::Sabotage => Behaviour::Sabotage,
            Mode::Combat => Behaviour::Combat,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Behaviour::Auto => "Auto (AI director)",
            Behaviour::Idle => "Idle — wander around",
            Behaviour::Observe => "Observe — watch me",
            Behaviour::Sabotage => "Sabotage — stomp my windows",
            Behaviour::Combat => "Combat — fight my cursor",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Size {
    Small,
    #[default]
    Medium,
    Large,
}

impl Size {
    pub const ALL: [Size; 3] = [Size::Small, Size::Medium, Size::Large];

    /// Rig scale (the source rig is ~250 pt tall).
    pub fn scale(self) -> f32 {
        match self {
            Size::Small => 0.4,
            Size::Medium => 0.55,
            Size::Large => 0.75,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Size::Small => "Small",
            Size::Medium => "Medium",
            Size::Large => "Large",
        }
    }
}

/// How long thrown words stay where they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WordStay {
    #[default]
    Seconds5,
    Seconds15,
    Seconds30,
    Minute,
    Forever,
}

impl WordStay {
    pub const ALL: [WordStay; 5] = [
        WordStay::Seconds5,
        WordStay::Seconds15,
        WordStay::Seconds30,
        WordStay::Minute,
        WordStay::Forever,
    ];

    /// Seconds before fading, or `None` to stay for good.
    pub fn seconds(self) -> Option<f32> {
        match self {
            WordStay::Seconds5 => Some(5.0),
            WordStay::Seconds15 => Some(15.0),
            WordStay::Seconds30 => Some(30.0),
            WordStay::Minute => Some(60.0),
            WordStay::Forever => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WordStay::Seconds5 => "5 Seconds",
            WordStay::Seconds15 => "15 Seconds",
            WordStay::Seconds30 => "30 Seconds",
            WordStay::Minute => "1 Minute",
            WordStay::Forever => "Forever (until there are too many)",
        }
    }
}

/// How the figure talks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    /// Family-friendly.
    Polite,
    /// Playful taunts, no swearing.
    #[default]
    Cheeky,
    /// Swears like a sailor (bleeped unless you turn that off).
    Salty,
}

impl Language {
    pub const ALL: [Language; 3] = [Language::Polite, Language::Cheeky, Language::Salty];

    pub fn label(self) -> &'static str {
        match self {
            Language::Polite => "Polite",
            Language::Cheeky => "Cheeky",
            Language::Salty => "Salty (swears)",
        }
    }
}

/// The figure's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FigureColor {
    #[default]
    Classic,
    Red,
    Blue,
    Green,
    Purple,
    Orange,
    Pink,
}

impl FigureColor {
    pub const ALL: [FigureColor; 7] = [
        FigureColor::Classic,
        FigureColor::Red,
        FigureColor::Blue,
        FigureColor::Green,
        FigureColor::Purple,
        FigureColor::Orange,
        FigureColor::Pink,
    ];

    pub fn rgb(self) -> [u8; 3] {
        match self {
            FigureColor::Classic => crate::stickman::WALKER_RGB,
            FigureColor::Red => [200, 40, 40],
            FigureColor::Blue => [30, 90, 210],
            FigureColor::Green => [30, 150, 60],
            FigureColor::Purple => [120, 50, 190],
            FigureColor::Orange => [230, 120, 20],
            FigureColor::Pink => [225, 80, 160],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FigureColor::Classic => "Classic (black)",
            FigureColor::Red => "Red",
            FigureColor::Blue => "Blue",
            FigureColor::Green => "Green",
            FigureColor::Purple => "Purple",
            FigureColor::Orange => "Orange",
            FigureColor::Pink => "Pink",
        }
    }
}

/// A settings change that carries a value (sliders, text fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Aggression(u8),
    Chattiness(u8),
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// What the figure is called (the AI director uses it).
    pub name: String,
    /// 0 (gentle) .. 100 (picks fights constantly).
    pub aggression: u8,
    /// 0 (quiet) .. 100 (never shuts up).
    pub chattiness: u8,
    pub language: Language,
    /// Replace swear words with #$%&!.
    pub bleep: bool,
    pub color: FigureColor,
    pub behaviour: Behaviour,
    pub size: Size,
    /// Punches shove the real pointer.
    pub cursor_push: bool,
    /// Stomps shake the real window.
    pub window_shake: bool,
    /// Read the active window's text (OCR) so the figure can walk on it.
    pub walk_on_text: bool,
    /// Let the figure rip words off the screen and throw them (they look missing
    /// until the page changes).
    pub throw_words: bool,
    /// Show speech bubbles.
    pub speech: bool,
    /// Play sound effects.
    pub sound: bool,
    /// How long thrown words stay where they land.
    pub word_stay: WordStay,
    /// Hobbies: nap when the user is away, dance, doodle, sit on edges.
    pub naps: bool,
    pub dancing: bool,
    pub doodles: bool,
    pub sitting: bool,
    /// Dance when the Mac is playing audio.
    pub dance_to_music: bool,
    /// Kick the active window so it slides (needs Accessibility).
    pub window_shoves: bool,
    /// Super jumps and telekinesis.
    pub powers: bool,
    /// Start locked in a jail cell and break out.
    pub jail_intro: bool,
    /// A rival drops in now and then to duel.
    pub rival_visits: bool,
    /// Telekinesis may pick up and throw the active window (Accessibility).
    pub throw_windows: bool,
    /// Rip app icons up off the Dock and throw them (Accessibility).
    pub pelt_icons: bool,
    /// Rip a scrollbar off the active window and fight with it.
    pub weapons: bool,
    /// Reflect on its memories with the local model (rewrites its beliefs).
    pub self_reflection: bool,
    /// Spell out messages with letters torn from the screen.
    pub notes: bool,
    /// Lose its temper when resentment boils over (rage: swearing, red,
    /// relentless attacks).
    pub lose_temper: bool,
    pub visible: bool,
    pub paused: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            name: "Stick".into(),
            aggression: 50,
            chattiness: 50,
            language: Language::Cheeky,
            bleep: true,
            color: FigureColor::Classic,
            behaviour: Behaviour::Auto,
            size: Size::Medium,
            cursor_push: true,
            window_shake: true,
            walk_on_text: true,
            throw_words: true,
            speech: true,
            sound: true,
            word_stay: WordStay::Seconds5,
            naps: true,
            dancing: true,
            doodles: true,
            sitting: true,
            dance_to_music: true,
            window_shoves: true,
            powers: true,
            jail_intro: true,
            rival_visits: true,
            throw_windows: true,
            pelt_icons: true,
            weapons: true,
            self_reflection: true,
            notes: true,
            lose_temper: true,
            visible: true,
            paused: false,
        }
    }
}

/// Something the user picked in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SetBehaviour(Behaviour),
    SetSize(Size),
    ToggleCursorPush,
    ToggleWindowShake,
    ToggleWalkOnText,
    ToggleThrowWords,
    ToggleSpeech,
    ToggleSound,
    SetWordStay(WordStay),
    ToggleNaps,
    ToggleDancing,
    ToggleDoodles,
    ToggleSitting,
    ToggleDanceToMusic,
    ToggleWindowShoves,
    TogglePowers,
    ToggleJailIntro,
    SetLanguage(Language),
    ToggleBleep,
    SetColor(FigureColor),
    OpenSettings,
    /// Enter or leave draw mode (sketch platforms with the mouse).
    ToggleDrawMode,
    ClearDrawings,
    /// Forget experience and grudges.
    ResetStats,
    SummonRival,
    ToggleRivalVisits,
    ToggleThrowWindows,
    TogglePeltIcons,
    ToggleWeapons,
    ToggleReflection,
    ToggleNotes,
    ToggleTemper,
    /// Reflect on its memories right now.
    ReflectNow,
    /// Forget everything it remembers and feels.
    ResetMind,
    /// Spell out a message now.
    WriteNote,
    ToggleVisible,
    TogglePaused,
    Respawn,
    OpenAccessibilitySettings,
    OpenScreenRecordingSettings,
    Quit,
}

impl Command {
    /// Stable integer tags for `NSMenuItem.tag`.
    pub fn tag(self) -> isize {
        match self {
            Command::SetBehaviour(b) => {
                100 + Behaviour::ALL.iter().position(|x| *x == b).unwrap_or(0) as isize
            }
            Command::SetSize(s) => {
                200 + Size::ALL.iter().position(|x| *x == s).unwrap_or(0) as isize
            }
            Command::ToggleCursorPush => 300,
            Command::ToggleWindowShake => 301,
            Command::ToggleWalkOnText => 302,
            Command::ToggleVisible => 303,
            Command::TogglePaused => 304,
            Command::ToggleThrowWords => 305,
            Command::ToggleSpeech => 306,
            Command::ToggleSound => 307,
            Command::ToggleNaps => 310,
            Command::ToggleDancing => 311,
            Command::ToggleDoodles => 312,
            Command::ToggleSitting => 313,
            Command::ToggleDanceToMusic => 318,
            Command::ToggleWindowShoves => 314,
            Command::TogglePowers => 315,
            Command::ToggleJailIntro => 316,
            Command::ToggleBleep => 317,
            Command::OpenSettings => 401,
            Command::ToggleDrawMode => 402,
            Command::ClearDrawings => 403,
            Command::ResetStats => 404,
            Command::SummonRival => 405,
            Command::ToggleRivalVisits => 319,
            Command::ToggleThrowWindows => 320,
            Command::TogglePeltIcons => 321,
            Command::ToggleWeapons => 322,
            Command::ToggleReflection => 323,
            Command::ToggleNotes => 324,
            Command::ToggleTemper => 325,
            Command::ReflectNow => 408,
            Command::ResetMind => 406,
            Command::WriteNote => 407,
            Command::SetLanguage(l) => {
                700 + Language::ALL.iter().position(|x| *x == l).unwrap_or(0) as isize
            }
            Command::SetColor(c) => {
                800 + FigureColor::ALL.iter().position(|x| *x == c).unwrap_or(0) as isize
            }
            Command::SetWordStay(w) => {
                600 + WordStay::ALL.iter().position(|x| *x == w).unwrap_or(0) as isize
            }
            Command::Respawn => 400,
            Command::OpenAccessibilitySettings => 500,
            Command::OpenScreenRecordingSettings => 501,
            Command::Quit => 900,
        }
    }

    pub fn from_tag(tag: isize) -> Option<Command> {
        Some(match tag {
            100..=104 => Command::SetBehaviour(Behaviour::ALL[(tag - 100) as usize]),
            200..=202 => Command::SetSize(Size::ALL[(tag - 200) as usize]),
            300 => Command::ToggleCursorPush,
            301 => Command::ToggleWindowShake,
            302 => Command::ToggleWalkOnText,
            303 => Command::ToggleVisible,
            304 => Command::TogglePaused,
            305 => Command::ToggleThrowWords,
            306 => Command::ToggleSpeech,
            307 => Command::ToggleSound,
            310 => Command::ToggleNaps,
            311 => Command::ToggleDancing,
            312 => Command::ToggleDoodles,
            313 => Command::ToggleSitting,
            318 => Command::ToggleDanceToMusic,
            314 => Command::ToggleWindowShoves,
            315 => Command::TogglePowers,
            316 => Command::ToggleJailIntro,
            317 => Command::ToggleBleep,
            401 => Command::OpenSettings,
            402 => Command::ToggleDrawMode,
            403 => Command::ClearDrawings,
            404 => Command::ResetStats,
            405 => Command::SummonRival,
            319 => Command::ToggleRivalVisits,
            320 => Command::ToggleThrowWindows,
            321 => Command::TogglePeltIcons,
            322 => Command::ToggleWeapons,
            323 => Command::ToggleReflection,
            324 => Command::ToggleNotes,
            325 => Command::ToggleTemper,
            408 => Command::ReflectNow,
            406 => Command::ResetMind,
            407 => Command::WriteNote,
            700..=702 => Command::SetLanguage(Language::ALL[(tag - 700) as usize]),
            800..=806 => Command::SetColor(FigureColor::ALL[(tag - 800) as usize]),
            600..=604 => Command::SetWordStay(WordStay::ALL[(tag - 600) as usize]),
            400 => Command::Respawn,
            500 => Command::OpenAccessibilitySettings,
            501 => Command::OpenScreenRecordingSettings,
            900 => Command::Quit,
            _ => return None,
        })
    }
}

impl Settings {
    /// Apply a settings-changing command. Returns whether anything changed
    /// (commands that are actions, like Quit, change nothing).
    pub fn apply(&mut self, cmd: Command) -> bool {
        let before = self.clone();
        match cmd {
            Command::SetBehaviour(b) => self.behaviour = b,
            Command::SetSize(s) => self.size = s,
            Command::ToggleCursorPush => self.cursor_push = !self.cursor_push,
            Command::ToggleWindowShake => self.window_shake = !self.window_shake,
            Command::ToggleWalkOnText => self.walk_on_text = !self.walk_on_text,
            Command::ToggleThrowWords => self.throw_words = !self.throw_words,
            Command::ToggleSpeech => self.speech = !self.speech,
            Command::ToggleSound => self.sound = !self.sound,
            Command::SetWordStay(w) => self.word_stay = w,
            Command::ToggleNaps => self.naps = !self.naps,
            Command::ToggleDancing => self.dancing = !self.dancing,
            Command::ToggleDoodles => self.doodles = !self.doodles,
            Command::ToggleSitting => self.sitting = !self.sitting,
            Command::ToggleDanceToMusic => self.dance_to_music = !self.dance_to_music,
            Command::ToggleWindowShoves => self.window_shoves = !self.window_shoves,
            Command::TogglePowers => self.powers = !self.powers,
            Command::ToggleJailIntro => self.jail_intro = !self.jail_intro,
            Command::ToggleRivalVisits => self.rival_visits = !self.rival_visits,
            Command::ToggleThrowWindows => self.throw_windows = !self.throw_windows,
            Command::TogglePeltIcons => self.pelt_icons = !self.pelt_icons,
            Command::ToggleWeapons => self.weapons = !self.weapons,
            Command::ToggleReflection => self.self_reflection = !self.self_reflection,
            Command::ToggleNotes => self.notes = !self.notes,
            Command::ToggleTemper => self.lose_temper = !self.lose_temper,
            Command::SetLanguage(l) => self.language = l,
            Command::ToggleBleep => self.bleep = !self.bleep,
            Command::SetColor(c) => self.color = c,
            Command::ToggleVisible => self.visible = !self.visible,
            Command::TogglePaused => self.paused = !self.paused,
            _ => {}
        }
        *self != before
    }

    /// Whether a toggle is on, or a choice is the selected one (for check
    /// marks in the menu and the settings window).
    pub fn checked(&self, cmd: Command) -> bool {
        match cmd {
            Command::SetBehaviour(b) => self.behaviour == b,
            Command::SetSize(s) => self.size == s,
            Command::SetWordStay(w) => self.word_stay == w,
            Command::SetLanguage(l) => self.language == l,
            Command::SetColor(c) => self.color == c,
            Command::ToggleCursorPush => self.cursor_push,
            Command::ToggleWindowShake => self.window_shake,
            Command::ToggleWalkOnText => self.walk_on_text,
            Command::ToggleThrowWords => self.throw_words && self.walk_on_text,
            Command::ToggleSpeech => self.speech,
            Command::ToggleSound => self.sound,
            Command::ToggleNaps => self.naps,
            Command::ToggleDancing => self.dancing,
            Command::ToggleDoodles => self.doodles,
            Command::ToggleSitting => self.sitting,
            Command::ToggleDanceToMusic => self.dance_to_music,
            Command::ToggleWindowShoves => self.window_shoves && self.window_shake,
            Command::TogglePowers => self.powers,
            Command::ToggleJailIntro => self.jail_intro,
            Command::ToggleRivalVisits => self.rival_visits,
            Command::ToggleThrowWindows => self.throw_windows,
            Command::TogglePeltIcons => self.pelt_icons,
            Command::ToggleWeapons => self.weapons,
            Command::ToggleReflection => self.self_reflection,
            Command::ToggleNotes => self.notes,
            Command::ToggleTemper => self.lose_temper,
            Command::ToggleBleep => self.bleep,
            Command::ToggleVisible => self.visible,
            Command::TogglePaused => self.paused,
            _ => false,
        }
    }

    /// Apply a slider or text change. Returns whether anything changed.
    pub fn change(&mut self, c: Change) -> bool {
        let before = self.clone();
        match c {
            Change::Aggression(v) => self.aggression = v.min(100),
            Change::Chattiness(v) => self.chattiness = v.min(100),
            Change::Name(n) => {
                let n: String = n.trim().chars().take(24).collect();
                if !n.is_empty() {
                    self.name = n;
                }
            }
        }
        *self != before
    }

    /// Default location, overridable with `STICKMAN_SETTINGS`.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("STICKMAN_SETTINGS") {
            return Some(PathBuf::from(p));
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/Stickman/settings.json"))
    }

    /// Load from `path`; a missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!(
                    "settings: {} is invalid ({e}); using defaults",
                    path.display()
                );
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Pause is a momentary state; never come back paused after a restart.
        let persisted = Settings {
            paused: false,
            ..self.clone()
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&persisted)?)?;
        std::fs::rename(&tmp, path)?; // atomic replace
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_friendly() {
        let s = Settings::default();
        assert_eq!(s.behaviour, Behaviour::Auto);
        assert_eq!(s.size.scale(), 0.55);
        assert!(s.visible && !s.paused && s.cursor_push && s.window_shake && s.walk_on_text);
    }

    #[test]
    fn every_command_round_trips_through_its_tag() {
        let mut all = vec![
            Command::ToggleCursorPush,
            Command::ToggleWindowShake,
            Command::ToggleWalkOnText,
            Command::ToggleThrowWords,
            Command::ToggleSpeech,
            Command::ToggleSound,
            Command::ToggleNaps,
            Command::ToggleDancing,
            Command::ToggleDoodles,
            Command::ToggleSitting,
            Command::ToggleDanceToMusic,
            Command::ToggleWindowShoves,
            Command::TogglePowers,
            Command::ToggleJailIntro,
            Command::ToggleBleep,
            Command::OpenSettings,
            Command::ToggleDrawMode,
            Command::ClearDrawings,
            Command::ResetStats,
            Command::SummonRival,
            Command::ToggleRivalVisits,
            Command::ToggleThrowWindows,
            Command::TogglePeltIcons,
            Command::ToggleWeapons,
            Command::ToggleReflection,
            Command::ToggleNotes,
            Command::ToggleTemper,
            Command::ReflectNow,
            Command::ResetMind,
            Command::WriteNote,
            Command::ToggleVisible,
            Command::TogglePaused,
            Command::Respawn,
            Command::OpenAccessibilitySettings,
            Command::OpenScreenRecordingSettings,
            Command::Quit,
        ];
        all.extend(Behaviour::ALL.map(Command::SetBehaviour));
        all.extend(Size::ALL.map(Command::SetSize));
        all.extend(WordStay::ALL.map(Command::SetWordStay));
        all.extend(Language::ALL.map(Command::SetLanguage));
        all.extend(FigureColor::ALL.map(Command::SetColor));
        let mut tags: Vec<isize> = all.iter().map(|c| c.tag()).collect();
        for c in &all {
            assert_eq!(Command::from_tag(c.tag()), Some(*c));
        }
        tags.sort();
        tags.dedup();
        assert_eq!(tags.len(), all.len(), "tags must be unique");
        assert_eq!(Command::from_tag(0), None);
        assert_eq!(Command::from_tag(105), None);
    }

    #[test]
    fn apply_changes_only_what_it_should() {
        let mut s = Settings::default();
        assert!(s.apply(Command::ToggleCursorPush));
        assert!(!s.cursor_push);
        assert!(s.apply(Command::SetBehaviour(Behaviour::Combat)));
        assert_eq!(s.behaviour.forced_mode(), Some(Mode::Combat));
        assert!(!s.apply(Command::SetBehaviour(Behaviour::Combat)), "no-op");
        assert!(!s.apply(Command::Quit));
        assert!(s.apply(Command::SetSize(Size::Large)));
        assert_eq!(s.size.scale(), 0.75);
    }

    #[test]
    fn value_changes_are_clamped_and_names_trimmed() {
        let mut s = Settings::default();
        assert!(s.change(Change::Aggression(250)));
        assert_eq!(s.aggression, 100);
        assert!(
            !s.change(Change::Name("   ".into())),
            "blank names are ignored"
        );
        assert!(s.change(Change::Name(
            "  Captain Scribbles the Magnificent III  ".into()
        )));
        assert_eq!(s.name, "Captain Scribbles the Ma");
    }

    #[test]
    fn save_and_load_round_trip_without_pause() {
        let dir = std::env::temp_dir().join(format!("stickman-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let s = Settings {
            behaviour: Behaviour::Sabotage,
            size: Size::Small,
            window_shake: false,
            paused: true,
            ..Settings::default()
        };
        s.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded, Settings { paused: false, ..s });
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_or_corrupt_files_give_defaults() {
        let dir =
            std::env::temp_dir().join(format!("stickman-settings-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Settings::load(&dir.join("nope.json")), Settings::default());
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        assert_eq!(Settings::load(&bad), Settings::default());
        // Unknown/missing fields are tolerated (forward/backward compatible).
        std::fs::write(&bad, r#"{"size":"Large","future_option":true}"#).unwrap();
        assert_eq!(Settings::load(&bad).size, Size::Large);
        std::fs::remove_dir_all(dir).ok();
    }
}
