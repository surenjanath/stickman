//! The figure's mind: what it remembers, how it feels about the user, and
//! what it believes and schemes — kept between launches and rewritten by its
//! own reflections.
//!
//! This is a small, local take on the "self-improving harness" idea (Google
//! Research's Harness²): the model's weights never change, but every couple of
//! minutes the figure reflects on its memories with the local LLM and rewrites
//! the beliefs, scheme and note it carries into its next decisions. Feelings
//! move with every event and fade slowly; everything can be wiped from the
//! settings.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::Mode;

/// How many memories it keeps (oldest forgotten first).
const MAX_MEMORIES: usize = 300;
/// Feelings drift back toward neutral by this much per minute.
const FADE_PER_MIN: f32 = 0.6;

/// Something that happened, worth remembering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// The user clicked it.
    Hit,
    /// The user dragged it around and threw it.
    Flung,
    /// The user stroked it.
    Petted,
    KnockedOut,
    /// It landed a blow on the cursor.
    StruckCursor,
    /// It caught and threw the cursor.
    ThrewCursor,
    /// The cursor got away.
    CursorEscaped,
    WonDuel,
    LostDuel,
    /// The user left it alone for a long while.
    Ignored,
}

impl Kind {
    fn describe(self) -> &'static str {
        match self {
            Kind::Hit => "the user hit you",
            Kind::Flung => "the user picked you up and threw you",
            Kind::Petted => "the user petted you",
            Kind::KnockedOut => "you were knocked out",
            Kind::StruckCursor => "you landed a blow on the cursor",
            Kind::ThrewCursor => "you grabbed and threw the cursor",
            Kind::CursorEscaped => "the cursor escaped your grip",
            Kind::WonDuel => "you beat the rival",
            Kind::LostDuel => "the rival beat you",
            Kind::Ignored => "the user ignored you for a long time",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Unix seconds.
    pub when: u64,
    pub what: Kind,
    /// The app the user was in.
    #[serde(default)]
    pub app: String,
}

/// How it feels about the user, each 0..100.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Feelings {
    pub resentment: f32,
    pub fear: f32,
    pub affection: f32,
    /// Its confidence in itself (wins, cursor victories).
    pub pride: f32,
}

impl Default for Feelings {
    fn default() -> Self {
        Feelings {
            resentment: 0.0,
            fear: 0.0,
            affection: 30.0,
            pride: 30.0,
        }
    }
}

impl Feelings {
    fn nudge(v: &mut f32, by: f32) {
        *v = (*v + by).clamp(0.0, 100.0);
    }

    fn react(&mut self, k: Kind, recent_hits: usize) {
        let n = Self::nudge;
        match k {
            Kind::Hit => {
                n(&mut self.resentment, 4.0);
                n(&mut self.affection, -2.0);
                // A beating makes it scared, not just angry.
                n(&mut self.fear, if recent_hits >= 4 { 3.0 } else { 1.0 });
            }
            Kind::Flung => {
                n(&mut self.resentment, 3.0);
                n(&mut self.fear, 3.0);
            }
            Kind::KnockedOut => {
                n(&mut self.resentment, 6.0);
                n(&mut self.fear, 4.0);
                n(&mut self.pride, -5.0);
            }
            Kind::Petted => {
                n(&mut self.affection, 7.0);
                n(&mut self.resentment, -4.0);
                n(&mut self.fear, -3.0);
            }
            Kind::StruckCursor => n(&mut self.pride, 1.5),
            Kind::ThrewCursor => n(&mut self.pride, 4.0),
            Kind::CursorEscaped => {
                n(&mut self.pride, -2.0);
                n(&mut self.resentment, 1.0);
            }
            Kind::WonDuel => n(&mut self.pride, 12.0),
            Kind::LostDuel => {
                n(&mut self.pride, -10.0);
                n(&mut self.fear, 2.0);
            }
            Kind::Ignored => {
                n(&mut self.affection, -3.0);
                n(&mut self.resentment, 1.0);
            }
        }
    }

    /// Drift toward neutral over `dt` seconds.
    fn fade(&mut self, dt: f32) {
        let step = FADE_PER_MIN * dt / 60.0;
        let toward = |v: &mut f32, rest: f32| {
            if *v > rest {
                *v = (*v - step).max(rest);
            } else {
                *v = (*v + step).min(rest);
            }
        };
        toward(&mut self.resentment, 0.0);
        toward(&mut self.fear, 0.0);
        toward(&mut self.affection, 30.0);
        toward(&mut self.pride, 30.0);
    }

    /// The strongest feeling, as a word.
    pub fn mood(&self) -> &'static str {
        let f = [
            (self.resentment, "vengeful"),
            (self.fear, "scared"),
            (self.affection, "fond"),
            (self.pride, "cocky"),
        ];
        let (v, word) = f
            .iter()
            .copied()
            .fold((0.0, "calm"), |a, b| if b.0 > a.0 { b } else { a });
        if v < 40.0 {
            "calm"
        } else {
            word
        }
    }
}

/// What a reflection produced.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Reflection {
    #[serde(default)]
    pub beliefs: Vec<String>,
    #[serde(default)]
    pub scheme: String,
    /// A short message to spell out to the user.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Mind {
    pub memories: VecDeque<Memory>,
    pub feelings: Feelings,
    /// What it has concluded about the user (rewritten by reflection).
    pub beliefs: Vec<String>,
    /// Its current plan ("Operation: …").
    pub scheme: String,
    /// The next message it wants to spell out.
    pub note: String,
    pub reflections: u32,
    /// Lifetime counts per kind (memories get forgotten; these don't).
    pub totals: BTreeMap<String, u32>,
    /// Hits by app: where the user is meanest.
    pub hits_by_app: BTreeMap<String, u32>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A note of at most 20 characters, cut at a word boundary, upper case.
pub fn fit_note(text: &str) -> String {
    let text = tidy(text, 200).to_uppercase();
    let mut out = String::new();
    for word in text.split_whitespace() {
        let extra = if out.is_empty() { 0 } else { 1 } + word.chars().count();
        if out.chars().count() + extra > 20 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out = text.chars().take(20).collect(); // one long word
    }
    out.trim_end_matches([',', ';', ':', '-']).to_string()
}

/// Make a model's line safe to keep: one line, trimmed, short.
fn tidy(s: &str, max: usize) -> String {
    let s = s.replace(['\n', '\r', '"'], " ");
    let s: String = s.trim().chars().take(max).collect();
    s.trim().to_string()
}

impl Mind {
    /// Remember `what` (while the user was in `app`).
    pub fn record(&mut self, what: Kind, app: &str) {
        let now = now_secs();
        let recent_hits = self
            .memories
            .iter()
            .rev()
            .take_while(|m| now.saturating_sub(m.when) < 60)
            .filter(|m| m.what == Kind::Hit)
            .count();
        self.feelings.react(what, recent_hits);
        *self.totals.entry(format!("{what:?}")).or_default() += 1;
        if what == Kind::Hit && !app.is_empty() {
            *self.hits_by_app.entry(app.to_string()).or_default() += 1;
        }
        self.memories.push_back(Memory {
            when: now,
            what,
            app: app.to_string(),
        });
        while self.memories.len() > MAX_MEMORIES {
            self.memories.pop_front();
        }
    }

    pub fn fade(&mut self, dt: f32) {
        self.feelings.fade(dt);
    }

    fn total(&self, k: Kind) -> u32 {
        self.totals.get(&format!("{k:?}")).copied().unwrap_or(0)
    }

    /// The app where the user hits it most.
    pub fn worst_app(&self) -> Option<(&str, u32)> {
        self.hits_by_app
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(a, n)| (a.as_str(), *n))
    }

    /// Extra aggression from how it feels (resentment up, fear down).
    pub fn aggression_bias(&self) -> f32 {
        (self.feelings.resentment - self.feelings.fear * 0.6 - self.feelings.affection * 0.3)
            / 250.0
    }

    /// Plain-English memory and psychology, for the model.
    pub fn summary(&self) -> String {
        let mut s = Vec::new();
        let counts = [
            Kind::Hit,
            Kind::Flung,
            Kind::Petted,
            Kind::KnockedOut,
            Kind::ThrewCursor,
            Kind::WonDuel,
            Kind::LostDuel,
        ]
        .iter()
        .filter_map(|k| {
            let n = self.total(*k);
            (n > 0).then(|| format!("{} {n} time{}", k.describe(), if n == 1 { "" } else { "s" }))
        })
        .collect::<Vec<_>>();
        if counts.is_empty() {
            s.push("You have no memories of the user yet.".to_string());
        } else {
            s.push(format!("You remember: {}.", counts.join("; ")));
        }
        if let Some((app, n)) = self.worst_app().filter(|(_, n)| *n >= 3) {
            s.push(format!(
                "The user hits you most while using {app} ({n} times)."
            ));
        }
        let f = &self.feelings;
        s.push(format!(
            "You feel: resentment {:.0}/100, fear {:.0}/100, affection {:.0}/100, pride {:.0}/100 (mostly {}).",
            f.resentment,
            f.fear,
            f.affection,
            f.pride,
            f.mood()
        ));
        if !self.beliefs.is_empty() {
            s.push(format!(
                "What you've concluded: {}.",
                self.beliefs.join("; ")
            ));
        }
        if !self.scheme.is_empty() {
            s.push(format!("Your current scheme: {}.", self.scheme));
        }
        s.join(" ")
    }

    /// The most recent memories, oldest first, as short lines.
    pub fn recent(&self, n: usize) -> Vec<String> {
        let now = now_secs();
        self.memories
            .iter()
            .rev()
            .take(n)
            .rev()
            .map(|m| {
                let ago = now.saturating_sub(m.when);
                let when = if ago < 120 {
                    format!("{ago} s ago")
                } else if ago < 7200 {
                    format!("{} min ago", ago / 60)
                } else {
                    format!("{} h ago", ago / 3600)
                };
                if m.app.is_empty() {
                    format!("{when}: {}", m.what.describe())
                } else {
                    format!("{when}: {} (in {})", m.what.describe(), m.app)
                }
            })
            .collect()
    }

    /// The prompt for a reflection.
    pub fn reflection_prompt(&self, name: &str, salty: bool) -> String {
        let style = if salty {
            "It may swear (no slurs, nothing hateful)."
        } else {
            "No swearing."
        };
        format!(
            "You are the inner voice of {name}, a mischievous stick figure living on the user's \
             desktop who is in an ongoing cartoon feud with the mouse cursor. Reflect on your \
             memories and decide what you now believe and plan. {}\n\
             Recent events:\n{}\n\
             Reply with JSON: {{\"beliefs\": up to 4 short conclusions about the user (each under \
             12 words), \"scheme\": your next cartoonish plan against the cursor or the user's \
             windows (under 18 words, start with 'Operation'), \"note\": a message to spell out to \
             the user with letters torn from the screen, ALL CAPS, at most 20 characters, \
             matching your mood}}. {style}",
            self.summary(),
            self.recent(12).join("\n")
        )
    }

    /// Take in a reflection's conclusions.
    pub fn apply(&mut self, r: Reflection) {
        let beliefs: Vec<String> = r
            .beliefs
            .iter()
            .map(|b| tidy(b, 90))
            .filter(|b| !b.is_empty())
            .take(4)
            .collect();
        if !beliefs.is_empty() {
            self.beliefs = beliefs;
        }
        let scheme = tidy(&r.scheme, 120);
        if !scheme.is_empty() {
            self.scheme = scheme;
        }
        let note = fit_note(&r.note);
        if !note.is_empty() {
            self.note = note;
        }
        self.reflections += 1;
    }

    /// A reflection without the model: simple rules over the same memories.
    pub fn reflect_offline(&self) -> Reflection {
        let f = &self.feelings;
        let mut beliefs = Vec::new();
        if self.total(Kind::Hit) >= 5 {
            beliefs.push("The user enjoys hitting me".to_string());
        }
        if let Some((app, n)) = self.worst_app().filter(|(_, n)| *n >= 3) {
            beliefs.push(format!("{app} makes the user violent ({n} hits)"));
        }
        if self.total(Kind::Petted) >= 3 {
            beliefs.push("Sometimes the user is kind".to_string());
        }
        if self.total(Kind::ThrewCursor) >= 3 {
            beliefs.push("The cursor can be thrown".to_string());
        }
        let (scheme, note) = match f.mood() {
            "vengeful" => (
                "Operation Payback: steal every word on screen",
                "I REMEMBER",
            ),
            "scared" => ("Operation Hide: stay far from the cursor", "PLEASE STOP"),
            "fond" => ("Operation Friendship: dance near the user", "YOU ARE OK"),
            "cocky" => ("Operation Pointer Doom: toss the cursor again", "FEAR ME"),
            _ => ("Operation Curious: watch the user closely", "I SEE YOU"),
        };
        Reflection {
            beliefs,
            scheme: scheme.into(),
            note: note.into(),
        }
    }

    /// A message to spell out for the current mode and mood.
    pub fn note_for(&self, mode: Mode, roll: f32) -> String {
        if !self.note.is_empty() && roll < 0.5 {
            return self.note.clone();
        }
        let f = &self.feelings;
        let pick = |opts: &[&str]| {
            opts[((roll * 7.3).fract() * opts.len() as f32) as usize % opts.len()].to_string()
        };
        if f.fear > 60.0 {
            return pick(&["PLEASE STOP", "NO MORE", "TRUCE?"]);
        }
        if f.resentment > 60.0 {
            return pick(&["I REMEMBER", "REVENGE SOON", "YOU WILL PAY"]);
        }
        if f.affection > 70.0 {
            return pick(&["YOU ARE OK", "THANKS", "FRIENDS?"]);
        }
        match mode {
            Mode::Combat => pick(&["FIGHT ME", "I WILL GET YOU", "CURSOR MUST GO"]),
            Mode::Sabotage => pick(&["STOP WORKING", "BORING", "PLAY WITH ME"]),
            Mode::Observe => pick(&["I SEE YOU", "WATCHING YOU", "HMMM"]),
            Mode::Idle => pick(&["HELLO", "BORED", "HI THERE"]),
        }
    }

    pub fn path_next_to(settings: &Path) -> PathBuf {
        settings.with_file_name("mind.json")
    }

    pub fn load(path: &Path) -> Mind {
        let mut m: Mind = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        m.note = fit_note(&m.note);
        m
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

/// Parse a reflection from an Ollama `/api/generate` body.
pub fn parse_reflection(body: &str) -> anyhow::Result<Reflection> {
    #[derive(Deserialize)]
    struct Outer {
        response: String,
    }
    let outer: Outer = serde_json::from_str(body)?;
    let t = outer.response.trim();
    let (a, b) = (t.find('{'), t.rfind('}'));
    let inner = match (a, b) {
        (Some(a), Some(b)) if b > a => &t[a..=b],
        _ => anyhow::bail!("no JSON in reflection"),
    };
    // Small models sometimes put raw line breaks inside strings.
    let cleaned: String = inner
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    Ok(serde_json::from_str(&cleaned)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abuse_builds_resentment_then_fear_and_petting_softens_it() {
        let mut m = Mind::default();
        for _ in 0..10 {
            m.record(Kind::Hit, "Safari");
        }
        assert!(m.feelings.resentment >= 40.0);
        assert!(
            m.feelings.fear > 15.0,
            "a beating is scary: {}",
            m.feelings.fear
        );
        assert_eq!(m.worst_app(), Some(("Safari", 10)));
        let before = m.feelings.resentment;
        for _ in 0..5 {
            m.record(Kind::Petted, "Safari");
        }
        assert!(m.feelings.resentment < before);
        assert!(m.summary().contains("hit you 10 times"));
        assert!(m.summary().contains("Safari"));
    }

    #[test]
    fn memories_are_capped_but_totals_are_not() {
        let mut m = Mind::default();
        for _ in 0..(MAX_MEMORIES + 50) {
            m.record(Kind::StruckCursor, "");
        }
        assert_eq!(m.memories.len(), MAX_MEMORIES);
        assert_eq!(m.total(Kind::StruckCursor), (MAX_MEMORIES + 50) as u32);
    }

    #[test]
    fn feelings_drift_back_to_neutral() {
        let mut m = Mind::default();
        for _ in 0..20 {
            m.record(Kind::Hit, "");
        }
        m.fade(60.0 * 600.0);
        assert_eq!(m.feelings.resentment, 0.0);
        assert_eq!(m.feelings.affection, 30.0);
    }

    #[test]
    fn reflections_are_tidied_and_change_what_it_carries() {
        let mut m = Mind::default();
        let body = r#"{"response":"{\"beliefs\":[\"The user\nis cruel\",\"\"],\"scheme\":\"Operation Doom: steal the cursor\",\"note\":\"you will pay, human, you really will\"}"}"#;
        let r = parse_reflection(body).unwrap();
        m.apply(r);
        assert_eq!(m.beliefs, ["The user is cruel"]);
        assert_eq!(m.scheme, "Operation Doom: steal the cursor");
        assert_eq!(m.note, "YOU WILL PAY, HUMAN");
        assert_eq!(fit_note("grrr! you will not pass"), "GRRR! YOU WILL NOT");
        assert_eq!(m.reflections, 1);
        assert!(m.summary().contains("Operation Doom"));
        // An empty reflection keeps the old conclusions.
        m.apply(Reflection::default());
        assert_eq!(m.beliefs, ["The user is cruel"]);
        assert!(parse_reflection(r#"{"response":"nope"}"#).is_err());
    }

    #[test]
    fn offline_reflection_follows_the_mood() {
        let mut m = Mind::default();
        for _ in 0..25 {
            m.record(Kind::Hit, "Mail");
        }
        let r = m.reflect_offline();
        assert!(r.beliefs.iter().any(|b| b.contains("Mail")));
        assert!(!r.note.is_empty() && r.note.len() <= 20);
        let mut fond = Mind::default();
        for _ in 0..12 {
            fond.record(Kind::Petted, "");
        }
        assert_eq!(fond.feelings.mood(), "fond");
        assert_eq!(fond.reflect_offline().note, "YOU ARE OK");
    }

    #[test]
    fn notes_fit_and_match_the_mood() {
        let m = Mind::default();
        for mode in [Mode::Idle, Mode::Observe, Mode::Sabotage, Mode::Combat] {
            for i in 0..10 {
                let n = m.note_for(mode, i as f32 / 10.0);
                assert!(!n.is_empty() && n.chars().count() <= 20, "{n}");
            }
        }
        let mut scared = Mind::default();
        scared.feelings.fear = 80.0;
        assert!(["PLEASE STOP", "NO MORE", "TRUCE?"]
            .contains(&scared.note_for(Mode::Combat, 0.9).as_str()));
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("stickman-mind-{}", std::process::id()));
        let path = dir.join("mind.json");
        let mut m = Mind::default();
        m.record(Kind::Hit, "Xcode");
        m.scheme = "Operation Test".into();
        m.save(&path).unwrap();
        assert_eq!(Mind::load(&path), m);
        std::fs::remove_dir_all(dir).ok();
    }
}
