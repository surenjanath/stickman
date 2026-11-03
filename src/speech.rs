//! Speech bubbles: short lines the figure says, from the AI director (mood
//! changes) and as quips when things happen (getting hit, grabbing, throwing).
//!
//! Pure logic and drawing; the text itself is rasterised by [`crate::text`] and
//! uploaded as a sprite by the caller, which attaches it with [`Speech::attach`].

use crate::geom::{v2, V2};
use crate::render::{DrawList, SpriteDraw};
use crate::settings::Language;

/// Characters per line before wrapping, and at most this many lines.
pub const WRAP_CHARS: usize = 24;
pub const MAX_LINES: usize = 3;
/// Bubble padding around the text, points.
const PAD: V2 = V2::new(9.0, 6.0);
const FADE_IN: f32 = 0.12;
const FADE_OUT: f32 = 0.25;
/// Minimum seconds between two quips.
const QUIP_COOLDOWN: f32 = 5.0;

/// Who is talking: a higher priority replaces a lower one mid-sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Quip,
    Director,
    /// Must be seen (e.g. getting knocked out).
    Event,
}

/// Something worth a quip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    Hit,
    KnockedOut,
    GrabbedWord,
    ThrewWord,
    StruckCursor,
    Stomp,
    CaughtCursor,
    ThrewCursor,
    CursorEscaped,
    Dodged,
    PickedUp,
    Flung,
    Yawned,
    Woke,
    Petted,
    Dancing,
    Doodling,
    Shove,
    SuperJump,
    Boing,
    MindCursor,
    MindWords,
    /// Hit by the rival.
    Struck,
}

/// Words the bleep filter hides (whole words, any case).
const SWEARS: [&str; 18] = [
    "damn", "dammit", "damned", "hell", "crap", "crappy", "ass", "asshole", "shit", "shitty",
    "bullshit", "fuck", "fucking", "fucker", "bitch", "bastard", "piss", "wtf",
];

/// Replace swear words with grawlix (`#$%&!`), keeping their length.
pub fn bleep(text: &str) -> String {
    const GRAWLIX: [char; 5] = ['#', '$', '%', '&', '!'];
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if SWEARS.contains(&word.to_lowercase().as_str()) {
            out.extend((0..word.chars().count()).map(|i| GRAWLIX[i % GRAWLIX.len()]));
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphabetic() {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

impl Moment {
    /// Gentler lines for the Polite setting, where the usual ones are cheeky.
    fn polite(self) -> Option<&'static [&'static str]> {
        Some(match self {
            Moment::Hit => &["Ow!", "Oof!", "Ouch!", "Please don't!"],
            Moment::StruckCursor => &["Boop!", "Tag, you're it!", "Hiyah!"],
            Moment::Stomp => &["Stomp stomp!", "Knock knock!"],
            Moment::Flung => &["Wheeee!", "Aaaaah!"],
            Moment::CursorEscaped => &["Aw, come back!"],
            _ => return None,
        })
    }

    /// Extra lines for the Salty setting.
    fn salty(self) -> &'static [&'static str] {
        match self {
            Moment::Hit => &[
                "Son of a—!",
                "Damn it!",
                "What the hell?!",
                "Ow, you jerk!",
                "Oh, you little sh—",
            ],
            Moment::KnockedOut => &["Oh, crap…", "Damn… that hurt…"],
            Moment::GrabbedWord => &["Mine, dammit!", "This word's crap anyway."],
            Moment::ThrewWord => &["Eat it!", "Catch, ass!"],
            Moment::StruckCursor => &[
                "Eat it, arrow!",
                "How'd that feel, huh?!",
                "Take that, damn it!",
            ],
            Moment::Stomp => &["Stupid damn window!", "Wake the hell up!"],
            Moment::CaughtCursor => &["Got you, you little crap!", "Gotcha, dammit!"],
            Moment::ThrewCursor => &["Screw you!", "Get the hell out!"],
            Moment::CursorEscaped => &["Get back here, you little…", "Damn it!"],
            Moment::Dodged => &["Ha! Missed, jackass!", "Too slow, dammit!"],
            Moment::PickedUp => &["Put me down, dammit!", "Hands off, ass!", "What the hell?!"],
            Moment::Flung => &[
                "Screw youuuu!",
                "Aaaah, crap!",
                "You'll pay for that, damn it!",
            ],
            Moment::Yawned => &["Damn, I'm tired.", "Screw it, nap time."],
            Moment::Woke => &["What the hell?! I'm up!", "Damn it, I was sleeping!"],
            Moment::Petted => &["Hehe… damn, that's nice.", "Fine, you're forgiven. Ass."],
            Moment::Dancing => &["Hell yeah!", "Damn, I'm good."],
            Moment::Doodling => &["Shut up, it's art."],
            Moment::Shove => &["Move your damn window!", "Outta my way!"],
            Moment::SuperJump => &["Hell yeah!", "Up, damn it!"],
            Moment::Boing => &["Boing, baby!", "Hell yeah, trampoline!"],
            Moment::MindCursor => &["Get over here, you damn arrow!", "With my damn mind!"],
            Moment::MindWords => &["Eat words, jerk!", "Incoming, dammit!"],
            Moment::Struck => &["Son of a—!", "You little sh—!", "Damn, that hurt!"],
        }
    }

    /// (lines to pick from, chance of saying anything).
    fn lines(self) -> (&'static [&'static str], f32) {
        match self {
            Moment::Hit => (
                &["Ow!", "Hey!", "Rude!", "Ouch!", "Not cool!", "Watch it!"],
                0.55,
            ),
            Moment::KnockedOut => (&["Ugh…", "x_x", "Five more minutes…"], 1.0),
            Moment::GrabbedWord => (&["Mine!", "Yoink!", "Ooh, a word.", "Nice font."], 0.5),
            Moment::ThrewWord => (&["Catch!", "Heads up!", "Take this!"], 0.35),
            Moment::StruckCursor => (&["Take that!", "Hiyah!", "Pow!", "Ha!"], 0.25),
            Moment::Stomp => (&["Wake up!", "Stomp stomp!", "Boring window."], 0.15),
            Moment::CaughtCursor => (&["Gotcha!", "Got you now!", "Hello there."], 0.9),
            Moment::ThrewCursor => (&["Bye!", "Fetch!", "Wheee!"], 0.8),
            Moment::CursorEscaped => (&["Hey, come back!", "Slippery!"], 0.8),
            Moment::Dodged => (&["Missed me!", "Too slow!", "Nope!"], 0.3),
            Moment::PickedUp => (
                &[
                    "Hey! Put me down!",
                    "Whoa!",
                    "Unhand me!",
                    "Not the collar!",
                ],
                0.8,
            ),
            Moment::Flung => (&["Wheeee!", "Aaaaah!", "You'll pay for that!"], 0.7),
            Moment::Yawned => (&["*yawn*", "Nap time.", "Wake me if you need me."], 1.0),
            Moment::Woke => (
                &["Huh?! I'm up!", "I wasn't sleeping!", "Oh! You're back."],
                1.0,
            ),
            Moment::Petted => (
                &["Hehe!", "Aww.", "That tickles!", "Okay, you're forgiven."],
                0.9,
            ),
            Moment::Dancing => (
                &[
                    "♪ ♫",
                    "Dance break!",
                    "Watch this!",
                    "Ooh, I love this song!",
                ],
                0.6,
            ),
            Moment::Doodling => (&["Art time.", "Hold still…", "A masterpiece!"], 0.4),
            Moment::Shove => (&["Scoot over!", "Move it!", "Wheee, window ride!"], 0.5),
            Moment::SuperJump => (&["Up, up and away!", "Super jump!", "Hup!"], 0.6),
            Moment::Boing => (&["Boing!", "Wheee!", "Again! Again!"], 0.5),
            Moment::MindCursor => (
                &["With my mind!", "Hmmmmm…", "You're under my control."],
                0.8,
            ),
            Moment::MindWords => (&["Words, rise!", "Behold!", "Incoming!"], 0.9),
            Moment::Struck => (&["Oof!", "Cheap shot!", "Is that all?", "Grr!"], 0.35),
        }
    }
}

/// Word-wrap `text` into at most [`MAX_LINES`] lines of about `width` chars,
/// ending with "…" if it had to be cut.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cut = false;
    for word in text.split_whitespace() {
        // Very long words are split hard.
        let chars: Vec<char> = word.chars().collect();
        for chunk in chars.chunks(width.max(1)) {
            let w: String = chunk.iter().collect();
            if cur.is_empty() {
                cur = w;
            } else if cur.chars().count() + 1 + w.chars().count() <= width {
                cur.push(' ');
                cur.push_str(&w);
            } else {
                lines.push(std::mem::take(&mut cur));
                cur = w;
            }
            if lines.len() == MAX_LINES {
                cut = true;
                break;
            }
        }
        if cut {
            break;
        }
    }
    if !cut && !cur.is_empty() {
        lines.push(cur);
    }
    if cut {
        if let Some(last) = lines.last_mut() {
            let keep: String = last.chars().take(width.saturating_sub(1)).collect();
            *last = format!("{}…", keep.trim_end());
        }
    }
    lines
}

/// Seconds a line stays up: long enough to read.
pub fn duration_for(text: &str) -> f32 {
    (1.4 + 0.055 * text.chars().count() as f32).min(5.0)
}

#[derive(Debug, Clone, PartialEq)]
struct Bubble {
    text: String,
    priority: Priority,
    age: f32,
    life: f32,
    /// Text sprite and its size in points, once the caller attached it.
    sprite: Option<(u32, V2)>,
}

pub struct Speech {
    pub language: Language,
    /// 0 (quiet) .. 1 (chatterbox).
    pub chattiness: f32,
    /// Hide swear words.
    pub bleep: bool,
    bubble: Option<Bubble>,
    since_quip: f32,
    rng: u64,
    /// Sprites the caller should free.
    retired: Vec<u32>,
}

impl Speech {
    pub fn new(seed: u64) -> Speech {
        Speech {
            language: Language::Cheeky,
            chattiness: 0.5,
            bleep: true,
            bubble: None,
            since_quip: f32::MAX / 2.0,
            rng: seed | 1,
            retired: Vec::new(),
        }
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Start saying `text`. Returns the wrapped lines to rasterise if accepted
    /// (then call [`attach`](Self::attach)), or `None` if something more
    /// important is being said.
    pub fn say(&mut self, text: &str, priority: Priority) -> Option<Vec<String>> {
        let bleeped;
        let text = if self.bleep {
            bleeped = bleep(text.trim());
            bleeped.as_str()
        } else {
            text.trim()
        };
        if text.is_empty() {
            return None;
        }
        if let Some(b) = &self.bubble {
            let fading = b.age > b.life;
            if b.priority > priority && !fading {
                return None;
            }
            if b.text == text && !fading {
                return None; // already saying it
            }
        }
        self.retire();
        self.bubble = Some(Bubble {
            text: text.to_string(),
            priority,
            age: 0.0,
            life: duration_for(text),
            sprite: None,
        });
        Some(wrap(text, WRAP_CHARS))
    }

    /// Maybe say something about `m` (random, rate-limited).
    pub fn quip(&mut self, m: Moment) -> Option<Vec<String>> {
        let (base, chance) = m.lines();
        let important = matches!(m, Moment::KnockedOut | Moment::Woke | Moment::Yawned);
        // Chattiness scales how often it talks and how long it waits between.
        let c = self.chattiness.clamp(0.0, 1.0);
        let chance = (chance * (0.15 + 1.7 * c)).min(1.0);
        let cooldown = QUIP_COOLDOWN * (2.4 - 2.0 * c);
        if !important && (c <= 0.0 || self.since_quip < cooldown || self.rand() >= chance) {
            return None;
        }
        let roll = self.rand();
        let lines = match self.language {
            Language::Polite => m.polite().unwrap_or(base),
            Language::Cheeky => base,
            // Mostly salty, sometimes the usual.
            Language::Salty if roll < 0.7 => m.salty(),
            Language::Salty => base,
        };
        let line = lines[((self.rand() * lines.len() as f32) as usize).min(lines.len() - 1)];
        let p = if important {
            Priority::Event
        } else {
            Priority::Quip
        };
        let out = self.say(line, p);
        if out.is_some() {
            self.since_quip = 0.0;
        }
        out
    }

    /// The rasterised text for the current line (`size` in points).
    pub fn attach(&mut self, sprite: u32, size: V2) {
        match &mut self.bubble {
            Some(b) if b.sprite.is_none() => b.sprite = Some((sprite, size)),
            _ => self.retired.push(sprite),
        }
    }

    /// Stop talking now (e.g. hidden).
    pub fn clear(&mut self) {
        self.retire();
    }

    fn retire(&mut self) {
        if let Some(b) = self.bubble.take() {
            if let Some((id, _)) = b.sprite {
                self.retired.push(id);
            }
        }
    }

    pub fn update(&mut self, dt: f32) {
        self.since_quip += dt;
        if let Some(b) = &mut self.bubble {
            b.age += dt;
            if b.age > b.life + FADE_OUT {
                self.retire();
            }
        }
    }

    /// Sprites no longer needed.
    pub fn take_retired(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.retired)
    }

    pub fn visible(&self) -> bool {
        self.bubble.is_some()
    }

    /// Text currently shown (for logs and tests).
    pub fn text(&self) -> Option<&str> {
        self.bubble.as_ref().map(|b| b.text.as_str())
    }

    fn alpha(b: &Bubble) -> f32 {
        let a_in = (b.age / FADE_IN).min(1.0);
        let a_out = 1.0 - ((b.age - b.life) / FADE_OUT).clamp(0.0, 1.0);
        a_in.min(a_out)
    }

    /// Where the bubble goes for a head at `head`: above it, kept inside
    /// `[min_x, max_x]` and below `min_y`. Returns (centre, half size).
    pub fn layout(text: V2, head: V2, head_r: f32, min_x: f32, max_x: f32, min_y: f32) -> (V2, V2) {
        let half = text / 2.0 + PAD;
        let mut c = v2(head.x + 0.35 * half.x, head.y - head_r - 30.0 - half.y);
        c.x =
            c.x.clamp(min_x + half.x, (max_x - half.x).max(min_x + half.x));
        if c.y - half.y < min_y {
            c.y = min_y + half.y;
        }
        (c, half)
    }

    /// Draw the bubble over a head at `head` (radius `head_r`).
    pub fn draw(&self, out: &mut DrawList, head: V2, head_r: f32, bounds: (f32, f32, f32)) {
        let Some(b) = &self.bubble else { return };
        let Some((sprite, size)) = b.sprite else {
            return;
        };
        let a = Self::alpha(b);
        if a <= 0.0 {
            return;
        }
        let (c, half) = Self::layout(size, head, head_r, bounds.0, bounds.1, bounds.2);
        let ink = [0.12, 0.12, 0.12, 0.95 * a];
        let paper = [1.0, 1.0, 1.0, 0.97 * a];
        // Tail from the bubble's underside toward the head.
        let base = v2(
            c.x.clamp(head.x - half.x * 0.6, head.x + half.x * 0.6),
            c.y + half.y - 4.0,
        );
        let tip = head + (v2(base.x, base.y) - head).normalize_or_zero() * (head_r + 5.0);
        out.rect(c, half + V2::splat(1.6), 9.5, ink);
        // A tapering wedge, built from short round-capped segments.
        const STEPS: usize = 8;
        for (color, inset) in [(ink, 0.0), (paper, 1.6)] {
            for i in 0..STEPS {
                let (k0, k1) = (i as f32 / STEPS as f32, (i + 1) as f32 / STEPS as f32);
                let half_w = 5.5 - 4.0 * k1 - inset;
                if half_w < 0.5 {
                    break;
                }
                out.capsule(base.lerp(tip, k0), base.lerp(tip, k1), half_w, color);
            }
        }
        out.rect(c, half, 8.0, paper);
        out.sprite(SpriteDraw {
            id: sprite,
            center: c,
            half: size / 2.0,
            rotation: 0.0,
            alpha: a,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_words_and_truncates() {
        assert_eq!(wrap("Hey!", 24), ["Hey!"]);
        assert_eq!(
            wrap("you hit me, now it's war and I mean it", 16),
            ["you hit me, now", "it's war and I", "mean it"]
        );
        let long = wrap(&"blah ".repeat(40), 10);
        assert_eq!(long.len(), MAX_LINES);
        assert!(long[2].ends_with('…'), "{long:?}");
        assert!(long.iter().all(|l| l.chars().count() <= 10), "{long:?}");
        assert_eq!(wrap("supercalifragilistic", 8).len(), 3);
        assert!(wrap("   ", 10).is_empty());
    }

    #[test]
    fn higher_priority_wins_and_lines_expire() {
        let mut s = Speech::new(1);
        assert!(s.say("watching you", Priority::Director).is_some());
        s.attach(7, v2(80.0, 16.0));
        assert!(
            s.say("Ow!", Priority::Quip).is_none(),
            "quip can't interrupt"
        );
        assert!(s.say("Ugh…", Priority::Event).is_some());
        assert_eq!(s.take_retired(), [7], "old sprite freed");
        s.attach(8, v2(30.0, 16.0));
        for _ in 0..600 {
            s.update(1.0 / 60.0);
        }
        assert!(!s.visible());
        assert_eq!(s.take_retired(), [8]);
    }

    #[test]
    fn late_sprites_for_replaced_lines_are_freed() {
        let mut s = Speech::new(1);
        s.say("one", Priority::Quip);
        s.say("two", Priority::Director);
        s.attach(3, v2(10.0, 10.0)); // belongs to "two"
        s.attach(4, v2(10.0, 10.0)); // stray
        assert_eq!(s.take_retired(), [4]);
    }

    #[test]
    fn quips_are_rate_limited() {
        let mut s = Speech::new(9);
        assert!(s.quip(Moment::CaughtCursor).is_some() || s.quip(Moment::CaughtCursor).is_some());
        let again = (0..20)
            .filter(|_| s.quip(Moment::CaughtCursor).is_some())
            .count();
        assert_eq!(again, 0, "cooldown");
        for _ in 0..(8 * 60) {
            s.update(1.0 / 60.0);
        }
        assert!((0..20).any(|_| s.quip(Moment::ThrewCursor).is_some()));
        // A knockout is always announced.
        assert!(s.quip(Moment::KnockedOut).is_some());
    }

    #[test]
    fn bleeps_whole_swear_words_only() {
        assert_eq!(bleep("What the hell?!"), "What the #$%&?!");
        assert_eq!(bleep("Hello, shell! Damn it."), "Hello, shell! #$%& it.");
        assert_eq!(bleep("WTF"), "#$%");
        let mut s = Speech::new(1);
        s.say("Damn window", Priority::Quip);
        assert_eq!(s.text(), Some("#$%& window"));
        s.bleep = false;
        s.say("Damn window!", Priority::Event);
        assert_eq!(s.text(), Some("Damn window!"));
    }

    #[test]
    fn language_and_chattiness_shape_the_quips() {
        let mut salty = 0;
        for seed in 0..60 {
            let mut s = Speech::new(seed);
            s.language = Language::Salty;
            s.bleep = false;
            s.chattiness = 1.0;
            if s.quip(Moment::Hit).is_some() && Moment::Hit.salty().contains(&s.text().unwrap()) {
                salty += 1;
            }
        }
        assert!(salty > 20, "salty lines {salty}/60");
        for seed in 0..60 {
            let mut s = Speech::new(seed);
            s.language = Language::Polite;
            s.chattiness = 1.0;
            if s.quip(Moment::Hit).is_some() {
                assert!(Moment::Hit.polite().unwrap().contains(&s.text().unwrap()));
            }
        }
        // Silent at zero chattiness, except moments that must be seen.
        let mut s = Speech::new(3);
        s.chattiness = 0.0;
        assert!((0..50).all(|_| s.quip(Moment::CaughtCursor).is_none()));
        assert!(s.quip(Moment::KnockedOut).is_some());
    }

    #[test]
    fn bubble_stays_on_screen() {
        let text = v2(120.0, 32.0);
        let (c, half) = Speech::layout(text, v2(10.0, 30.0), 12.0, 0.0, 1800.0, 0.0);
        assert!(c.x - half.x >= 0.0 && c.y - half.y >= 0.0);
        let (c, half) = Speech::layout(text, v2(1795.0, 600.0), 12.0, 0.0, 1800.0, 0.0);
        assert!(c.x + half.x <= 1800.0);
        assert!(c.y + half.y < 600.0, "above the head");
    }

    #[test]
    fn draws_bubble_only_once_text_is_ready() {
        let mut s = Speech::new(1);
        s.say("hi", Priority::Quip);
        let mut dl = DrawList::default();
        s.draw(&mut dl, v2(500.0, 500.0), 12.0, (0.0, 1000.0, 0.0));
        assert!(dl.is_empty());
        s.attach(1, v2(20.0, 16.0));
        s.update(0.5);
        s.draw(&mut dl, v2(500.0, 500.0), 12.0, (0.0, 1000.0, 0.0));
        assert!(dl.len() > 10, "{}", dl.len());
    }
}
