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

use crate::agent::{Dread, Mode};
use crate::war::War;

/// How many memories it keeps (oldest forgotten first).
const MAX_MEMORIES: usize = 300;
/// How many thoughts it keeps.
const MAX_THOUGHTS: usize = 40;
/// Resentment at which it loses its temper, and below which it calms down.
pub const RAGE_AT: f32 = 75.0;
pub const CALM_AT: f32 = 55.0;
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
    /// Its inner voice right now, one sentence.
    #[serde(default)]
    pub thought: String,
}

/// One day's diary entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiaryEntry {
    /// "2026-11-03".
    pub date: String,
    pub text: String,
}

/// Hits in one app before it starts to dread that app.
pub const DREAD_HITS: u32 = 3;

/// Diary entries kept (oldest dropped first).
const MAX_DIARY: usize = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Mind {
    /// Its diary, newest last.
    pub diary: VecDeque<DiaryEntry>,
    /// The day being tallied ("2026-11-03"), and what happened that day.
    pub day: String,
    pub day_counts: BTreeMap<String, u32>,
    pub memories: VecDeque<Memory>,
    pub feelings: Feelings,
    /// What it has concluded about the user (rewritten by reflection).
    pub beliefs: Vec<String>,
    /// Its current plan ("Operation: …").
    pub scheme: String,
    /// The next message it wants to spell out.
    pub note: String,
    pub reflections: u32,
    /// Its inner monologue, newest last.
    pub thoughts: VecDeque<String>,
    /// Lost its temper (resentment crossed RAGE_AT; calms below CALM_AT).
    pub enraged: bool,
    /// Lifetime counts per kind (memories get forgotten; these don't).
    pub totals: BTreeMap<String, u32>,
    /// Hits by app: where the user is meanest.
    pub hits_by_app: BTreeMap<String, u32>,
    /// Its war on the cursor: the cursor's health and what has worked.
    pub war: War,
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
    // A stray last letter (a word cut short by an older version) goes.
    if let Some((head, last)) = out.rsplit_once(' ') {
        if last.chars().count() == 1 && !matches!(last, "I" | "A") {
            out = head.to_string();
        }
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
    /// Add a line to its inner monologue.
    pub fn think(&mut self, thought: impl Into<String>) {
        let t = tidy(&thought.into(), 140);
        if t.is_empty() || self.thoughts.back() == Some(&t) {
            return;
        }
        self.thoughts.push_back(t);
        while self.thoughts.len() > MAX_THOUGHTS {
            self.thoughts.pop_front();
        }
    }

    /// What it's thinking right now.
    pub fn current_thought(&self) -> &str {
        self.thoughts.back().map_or("…", |s| s.as_str())
    }

    /// Update the temper. Returns `Some(true)` when it just lost it,
    /// `Some(false)` when it just calmed down.
    pub fn update_temper(&mut self, allowed: bool) -> Option<bool> {
        let r = self.feelings.resentment;
        if !self.enraged && allowed && r >= RAGE_AT {
            self.enraged = true;
            self.think("THAT'S IT. I'VE HAD IT. NO MORE MR. NICE STICK.");
            return Some(true);
        }
        if self.enraged && (r < CALM_AT || !allowed) {
            self.enraged = false;
            self.think("Okay. Breathe. I'm calm. I'm calm now.");
            return Some(false);
        }
        None
    }

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
        *self.day_counts.entry(format!("{what:?}")).or_default() += 1;
        if what == Kind::Hit && !app.is_empty() {
            *self.hits_by_app.entry(app.to_string()).or_default() += 1;
        }
        self.memories.push_back(Memory {
            when: now,
            what,
            app: app.to_string(),
        });
        // A quick thought about it, now and then.
        let n = self.total(what);
        let in_app = if app.is_empty() {
            String::new()
        } else {
            format!(" in {app}")
        };
        let thought = match what {
            Kind::Hit if n % 5 == 0 => Some(format!("That's {n} hits{in_app}. I'm keeping count.")),
            Kind::Hit if recent_hits >= 4 => Some("Stop it stop it STOP IT.".to_string()),
            Kind::Flung => Some("Did they just THROW me?!".to_string()),
            Kind::Petted if self.feelings.affection > 60.0 => {
                Some("Okay… that was actually nice.".to_string())
            }
            Kind::KnockedOut => Some(format!("Knocked out{in_app}. This will not be forgotten.")),
            Kind::ThrewCursor => Some("The cursor flew! I am unstoppable.".to_string()),
            Kind::CursorEscaped => Some("It slipped away. Next time.".to_string()),
            Kind::WonDuel => Some("Victory! Who's the stick now?".to_string()),
            Kind::LostDuel => Some("Beaten by a red nobody. Humiliating.".to_string()),
            Kind::Ignored => Some("They forgot about me. Again.".to_string()),
            _ => None,
        };
        if let Some(t) = thought {
            self.think(t);
        }
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

    /// How it feels about `app`, if that's where the user hits it most (at
    /// least `DREAD_HITS` times). The beatings leave a scar that lasts after
    /// the feelings fade, so a much-abused app stays scary; affection heals it.
    /// Rage, or resentment that outweighs the fear, makes it vengeful instead.
    pub fn dread(&self, app: &str) -> Dread {
        let n = match self.worst_app() {
            Some((worst, n)) if !app.is_empty() && worst == app && n >= DREAD_HITS => n,
            _ => return Dread::None,
        };
        let f = &self.feelings;
        let scar = (n as f32 * 3.0).min(45.0) * (1.0 - f.affection / 100.0).max(0.0);
        let fear = f.fear + scar;
        if self.enraged || (f.resentment >= 40.0 && f.resentment >= fear) {
            Dread::Vengeful
        } else if fear >= 35.0 {
            Dread::Scared
        } else {
            Dread::None
        }
    }

    /// A note about the app it dreads ("NO MORE CHROME"), or `None`.
    pub fn dread_note(&self, app: &str, roll: f32) -> Option<String> {
        let name = app.split_whitespace().last()?.to_uppercase();
        let pick = |opts: &[&str]| {
            let i = ((roll * 7.3).fract() * opts.len() as f32) as usize % opts.len();
            opts[i].replace("{}", &name)
        };
        match self.dread(app) {
            Dread::None => None,
            Dread::Scared => Some(pick(&["NO MORE {}", "NOT {}", "{} HURTS"])),
            Dread::Vengeful => Some(pick(&["I HATE {}", "{} MUST GO", "CLOSE {}"])),
        }
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
        if self.enraged {
            s.push(
                "You are ENRAGED: you swear, insult the cursor and attack relentlessly."
                    .to_string(),
            );
        }
        s.push(self.war.summary());
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
             matching your mood, \"thought\": your inner voice right now, one vivid sentence}}. \
             {style}",
            self.summary(),
            self.recent(12).join("\n")
        )
    }

    /// What happened on the day being tallied, in words.
    fn day_summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (k, n) in &self.day_counts {
            let kind = match k.as_str() {
                "Hit" => Kind::Hit,
                "Flung" => Kind::Flung,
                "Petted" => Kind::Petted,
                "KnockedOut" => Kind::KnockedOut,
                "StruckCursor" => Kind::StruckCursor,
                "ThrewCursor" => Kind::ThrewCursor,
                "CursorEscaped" => Kind::CursorEscaped,
                "WonDuel" => Kind::WonDuel,
                "LostDuel" => Kind::LostDuel,
                _ => Kind::Ignored,
            };
            parts.push(format!(
                "{} {n} time{}",
                kind.describe(),
                if *n == 1 { "" } else { "s" }
            ));
        }
        if parts.is_empty() {
            "Nothing much happened.".into()
        } else {
            format!("Today: {}.", parts.join("; "))
        }
    }

    /// The day changed to `today`: returns the prompt for the diary entry of
    /// the day that just ended (or `None` if nothing happened), and starts
    /// tallying the new day.
    pub fn new_day(&mut self, today: &str, name: &str, salty: bool) -> Option<(String, String)> {
        if self.day.is_empty() {
            self.day = today.to_string();
            return None;
        }
        if self.day == today {
            return None;
        }
        let ended = std::mem::replace(&mut self.day, today.to_string());
        let had_events = !self.day_counts.is_empty();
        let prompt = self.diary_prompt(&ended, name, salty);
        self.day_counts.clear();
        had_events.then_some((ended, prompt))
    }

    /// The prompt for the entry of `date` (from today's tally so far).
    /// Its latest diary entry, ready to paste and share.
    pub fn shareable_diary(&self, name: &str) -> Option<String> {
        let e = self.diary.back()?;
        Some(format!(
            "From the diary of {name}, the stick figure that lives on my Mac desktop ({}):\n\n{}\n\n(Stickman, written by a local AI: https://github.com/surenjanath/stickman)",
            e.date,
            e.text.trim()
        ))
    }

    pub fn diary_prompt(&self, date: &str, name: &str, salty: bool) -> String {
        let style = if salty || self.enraged {
            "It may swear (no slurs, nothing hateful)."
        } else {
            "No swearing."
        };
        let thoughts: Vec<&str> = self
            .thoughts
            .iter()
            .rev()
            .take(5)
            .map(|s| s.as_str())
            .collect();
        format!(
            "You are {name}, a mischievous stick figure living on the user's desktop, in a \
             cartoon feud with the mouse cursor. Write today's diary entry ({date}): 3 to 5 \
             short sentences, first person, funny and dramatic, starting with \"Dear diary,\". \
             {}\nHow you feel: {}. Your scheme: {}. Recent thoughts: {}.\n\
             Reply with JSON: {{\"text\": the entry}}. {style}",
            self.day_summary(),
            self.feelings.mood(),
            if self.scheme.is_empty() {
                "none yet"
            } else {
                &self.scheme
            },
            thoughts.join(" / ")
        )
    }

    /// A diary entry without the model.
    pub fn diary_offline(&self, date: &str) -> String {
        let f = &self.feelings;
        let hits = self.day_counts.get("Hit").copied().unwrap_or(0);
        let pets = self.day_counts.get("Petted").copied().unwrap_or(0);
        let mood = match f.mood() {
            "vengeful" => "I will have my revenge.",
            "scared" => "I am scared. I hid behind a window.",
            "fond" => "Honestly? The human is growing on me.",
            "cocky" => "That cursor doesn't stand a chance.",
            _ => "All in all, a quiet day.",
        };
        format!(
            "Dear diary, {date}. The human hit me {hits} time{} and petted me {pets} time{}. {mood}",
            if hits == 1 { "" } else { "s" },
            if pets == 1 { "" } else { "s" }
        )
    }

    pub fn add_diary(&mut self, date: &str, text: &str) {
        let text = tidy(text, 600);
        if text.is_empty() {
            return;
        }
        self.diary.retain(|e| e.date != date);
        self.diary.push_back(DiaryEntry {
            date: date.to_string(),
            text,
        });
        while self.diary.len() > MAX_DIARY {
            self.diary.pop_front();
        }
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
        if !r.thought.trim().is_empty() {
            self.think(r.thought.clone());
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
        let thought = match f.mood() {
            "vengeful" => "Every click is written down. Every. Single. One.",
            "scared" => "Maybe if I stay very still they'll stop.",
            "fond" => "This human isn't so bad.",
            "cocky" => "That cursor doesn't stand a chance.",
            _ => "What is the human doing now?",
        };
        Reflection {
            beliefs,
            scheme: scheme.into(),
            note: note.into(),
            thought: thought.into(),
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
        if self.enraged {
            return pick(&["FUCK YOU", "YOU ARE DEAD", "I HATE YOU", "DIE CURSOR"]);
        }
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
    fn it_dreads_the_app_where_it_gets_beaten() {
        let mut m = Mind::default();
        // Two hits is nothing to dread yet.
        m.record(Kind::Hit, "Google Chrome");
        m.record(Kind::Hit, "Google Chrome");
        assert_eq!(m.dread("Google Chrome"), Dread::None);
        // A proper beating, then it calms down: still scared of Chrome
        // (the scar outlasts the feelings), but not of anywhere else.
        for _ in 0..10 {
            m.record(Kind::Hit, "Google Chrome");
        }
        m.enraged = false;
        m.feelings.resentment = 10.0;
        m.feelings.fear = 5.0;
        m.feelings.affection = 0.0;
        assert_eq!(m.dread("Google Chrome"), Dread::Scared);
        assert_eq!(m.dread("Safari"), Dread::None);
        assert_eq!(m.dread(""), Dread::None);
        assert!(m
            .dread_note("Google Chrome", 0.3)
            .unwrap()
            .contains("CHROME"));
        assert_eq!(m.dread_note("Safari", 0.3), None);
        // Angry more than afraid: it wants revenge on Chrome instead.
        m.feelings.resentment = 90.0;
        assert_eq!(m.dread("Google Chrome"), Dread::Vengeful);
        m.feelings.resentment = 10.0;
        m.enraged = true;
        assert_eq!(m.dread("Google Chrome"), Dread::Vengeful);
        // Lots of affection heals the scar.
        m.enraged = false;
        m.feelings.affection = 100.0;
        assert_eq!(m.dread("Google Chrome"), Dread::None);
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
        assert_eq!(fit_note("GRRR! YOU WILL NOT P"), "GRRR! YOU WILL NOT");
        assert_eq!(fit_note("I SEE A"), "I SEE A");
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
    fn enough_abuse_makes_it_lose_its_temper_then_calm_down() {
        let mut m = Mind::default();
        for _ in 0..30 {
            m.record(Kind::Hit, "Chrome");
        }
        assert!(
            m.thoughts.iter().any(|t| t.contains("hits in Chrome")),
            "{:?}",
            m.thoughts
        );
        assert_eq!(m.update_temper(false), None, "not allowed to rage");
        assert_eq!(m.update_temper(true), Some(true));
        assert!(m.enraged);
        assert!(m.current_thought().contains("HAD IT"));
        assert!(m.summary().contains("ENRAGED"));
        assert!(["FUCK YOU", "YOU ARE DEAD", "I HATE YOU", "DIE CURSOR"]
            .contains(&m.note_for(Mode::Idle, 0.9).as_str()));
        // Petting brings it back down below the calm line.
        for _ in 0..20 {
            m.record(Kind::Petted, "");
        }
        assert_eq!(m.update_temper(true), Some(false));
        assert!(!m.enraged);
    }

    #[test]
    fn the_latest_diary_entry_is_ready_to_share() {
        let mut m = Mind::default();
        assert_eq!(m.shareable_diary("Stick"), None);
        m.add_diary("2026-11-02", "Old news.");
        m.add_diary("2026-11-03", "  Dear diary, Safari again.  ");
        let s = m.shareable_diary("Stick").unwrap();
        assert!(s.starts_with("From the diary of Stick"));
        assert!(s.contains("(2026-11-03)"));
        assert!(s.contains("\n\nDear diary, Safari again.\n\n"));
        assert!(!s.contains("Old news"));
        assert!(s.contains("github.com/surenjanath/stickman"));
    }

    #[test]
    fn a_new_day_turns_yesterday_into_a_diary_entry() {
        let mut m = Mind::default();
        assert!(
            m.new_day("2026-11-03", "Stick", false).is_none(),
            "first day: just starts"
        );
        for _ in 0..3 {
            m.record(Kind::Hit, "Mail");
        }
        assert!(
            m.new_day("2026-11-03", "Stick", false).is_none(),
            "same day"
        );
        let (date, prompt) = m
            .new_day("2026-11-04", "Stick", false)
            .expect("yesterday's entry");
        assert_eq!(date, "2026-11-03");
        assert!(prompt.contains("the user hit you 3 times") && prompt.contains("Dear diary"));
        assert!(m.day_counts.is_empty() && m.day == "2026-11-04");
        // A quiet day writes nothing.
        assert!(m.new_day("2026-11-05", "Stick", false).is_none());
        m.add_diary("2026-11-03", "Dear diary, the human was rude.");
        m.add_diary("2026-11-03", "Dear diary, rewritten.");
        assert_eq!(m.diary.len(), 1);
        assert!(m.diary_offline("2026-11-05").starts_with("Dear diary"));
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
