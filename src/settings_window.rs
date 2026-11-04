//! The Settings window: every option in one place, in four tabs (Mind,
//! Personality, Abilities, Fun & Sound), built from native AppKit controls.
//! The Mind tab is live: level bars for its feelings (yellow, then red when
//! they get bad), what it's thinking, its scheme, beliefs and memory log.
//!
//! Controls talk to the main loop through the same Objective-C target as the
//! menu bar: checkboxes, buttons and pop-up menus send [`Command`] tags,
//! sliders and the name field send [`crate::settings::Change`]s. Main thread
//! only.

#![allow(unexpected_cfgs)] // objc 0.2 macros

use std::ffi::CString;

use objc::runtime::{Object, NO, YES};
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};

use crate::hats::Hat;
use crate::menubar::{action_target, SLIDER_AGGRESSION, SLIDER_CHATTINESS, SLIDER_VOLUME};
use crate::settings::{
    Behaviour, Command, FigureColor, Language, PowerSaving, Settings, Size, WordStay,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
// SAFETY: same layout as CGRect on 64-bit.
unsafe impl Encode for NSRect {
    fn encode() -> Encoding {
        // SAFETY: a valid Objective-C type encoding for CGRect.
        unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSEdgeInsets {
    top: f64,
    left: f64,
    bottom: f64,
    right: f64,
}
// SAFETY: matches NSEdgeInsets (four CGFloats).
unsafe impl Encode for NSEdgeInsets {
    fn encode() -> Encoding {
        // SAFETY: a valid Objective-C type encoding for NSEdgeInsets.
        unsafe { Encoding::from_str("{NSEdgeInsets=dddd}") }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRange {
    location: usize,
    length: usize,
}
// SAFETY: matches NSRange (two NSUIntegers).
unsafe impl Encode for NSRange {
    fn encode() -> Encoding {
        // SAFETY: a valid Objective-C type encoding for NSRange.
        unsafe { Encoding::from_str("{_NSRange=QQ}") }
    }
}

/// Autoreleased NSString.
fn ns_string(s: &str) -> *mut Object {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    // SAFETY: valid NUL-terminated UTF-8; autoreleased.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

/// Autoreleased NSArray of `views`.
unsafe fn ns_array(views: &[*mut Object]) -> *mut Object {
    msg_send![class!(NSArray), arrayWithObjects: views.as_ptr() count: views.len()]
}

/// A pop-up menu and the choices it offers.
struct Popup {
    button: *mut Object,
    choices: Vec<Command>,
}

/// One row of a tab.
enum Row {
    /// A bold section heading (with a little space above it).
    Section(&'static str),
    /// A label and a control.
    Field(&'static str, *mut Object),
    /// A control (or a note) in the control column, no label.
    Under(*mut Object),
}

/// When a feeling's bar turns yellow and red.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Danger {
    /// High is bad (resentment, fear, grudge).
    High,
    /// Low is bad (affection, pride, health).
    Low,
    /// Just progress (experience).
    None,
}

/// The bars on the Mind tab, in order: (label, when it's bad).
pub const BARS: [(&str, Danger); 7] = [
    ("Resentment:", Danger::High),
    ("Fear:", Danger::High),
    ("Grudge:", Danger::High),
    ("Affection:", Danger::Low),
    ("Pride:", Danger::Low),
    ("Health:", Danger::Low),
    ("Next level:", Danger::None),
];

/// The mind's state, as the Mind tab shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MindView {
    /// "Mostly vengeful · level 3 Scrapper · ENRAGED".
    pub headline: String,
    /// (0..100 value, text beside it) per entry of [`BARS`].
    pub bars: Vec<(f64, String)>,
    pub thought: String,
    pub scheme: String,
    pub beliefs: String,
    pub note: String,
    /// Recent thoughts and memories, newest first.
    pub log: String,
    /// Diary entries, newest first.
    pub diary: String,
}

/// The settings window. Built once, shown and hidden on demand. Holds +1
/// references to the window (released in Drop); the controls are owned by the
/// window's view hierarchy, which lives as long as the window.
pub struct SettingsWindow {
    window: *mut Object,
    checks: Vec<(Command, *mut Object)>,
    popups: Vec<Popup>,
    sliders: Vec<(isize, *mut Object)>,
    name: *mut Object,
    headline: *mut Object,
    /// (level indicator, value label) per bar.
    bars: Vec<(*mut Object, *mut Object)>,
    /// Thought, scheme, beliefs, note.
    mind_labels: Vec<*mut Object>,
    log: *mut Object,
    diary: *mut Object,
    mind_shown: Option<MindView>,
    /// The hat pop-up and which hats are unlocked (as last shown).
    hat_popup: *mut Object,
    hats_shown: Vec<bool>,
    /// Achievements list label.
    achievements: *mut Object,
    tabs: *mut Object,
    shown: Option<Settings>,
    centered: bool,
}

impl SettingsWindow {
    /// Build the (hidden) window.
    pub fn new() -> SettingsWindow {
        let mut w = SettingsWindow {
            window: std::ptr::null_mut(),
            checks: Vec::new(),
            popups: Vec::new(),
            sliders: Vec::new(),
            name: std::ptr::null_mut(),
            headline: std::ptr::null_mut(),
            bars: Vec::new(),
            mind_labels: Vec::new(),
            log: std::ptr::null_mut(),
            diary: std::ptr::null_mut(),
            mind_shown: None,
            hat_popup: std::ptr::null_mut(),
            hats_shown: Vec::new(),
            achievements: std::ptr::null_mut(),
            tabs: std::ptr::null_mut(),
            shown: None,
            centered: false,
        };
        // SAFETY: main thread (module contract). All selectors are documented
        // AppKit API with these argument types. Views are autoreleased and
        // owned by their superviews once added; the window is +1 (alloc/init)
        // and kept until Drop.
        unsafe {
            let window: *mut Object = msg_send![class!(NSWindow), alloc];
            // Titled | closable | miniaturizable; buffered backing.
            let window: *mut Object = msg_send![window,
                initWithContentRect: NSRect { x: 0.0, y: 0.0, w: 640.0, h: 680.0 }
                styleMask: 1usize | 2 | 4
                backing: 2usize
                defer: NO];
            let _: () = msg_send![window, setReleasedWhenClosed: NO];
            let _: () = msg_send![window, setTitle: ns_string("Stickman Settings")];
            // Floating: stays above the app you were in while you adjust things.
            let _: () = msg_send![window, setLevel: 3isize];
            w.window = window;

            let tabs: *mut Object = msg_send![class!(NSTabView), new];
            let mind = w.mind_tab();
            let personality = w.personality_tab();
            let abilities = w.abilities_tab();
            let fun = w.fun_tab();
            for (label, view) in [
                ("Mind", mind),
                ("Personality", personality),
                ("Abilities", abilities),
                ("Fun & Sound", fun),
            ] {
                let item: *mut Object = msg_send![class!(NSTabViewItem), alloc];
                let item: *mut Object = msg_send![item, initWithIdentifier: ns_string(label)];
                let _: () = msg_send![item, setLabel: ns_string(label)];
                let _: () = msg_send![item, setView: view];
                let _: () = msg_send![tabs, addTabViewItem: item];
                let _: () = msg_send![item, release];
            }
            let _: () = msg_send![window, setContentView: tabs];
            w.tabs = tabs;
            let _: () = msg_send![tabs, release];
        }
        w
    }

    // --- Controls -------------------------------------------------------------

    unsafe fn label(text: &str) -> *mut Object {
        msg_send![class!(NSTextField), labelWithString: ns_string(text)]
    }

    unsafe fn font(l: *mut Object, size: f64, bold: bool) {
        let f: *mut Object = if bold {
            msg_send![class!(NSFont), boldSystemFontOfSize: size]
        } else {
            msg_send![class!(NSFont), systemFontOfSize: size]
        };
        let _: () = msg_send![l, setFont: f];
    }

    /// Small grey help text.
    unsafe fn hint(text: &str) -> *mut Object {
        let l = Self::wrapping(text, 400.0);
        Self::font(l, 11.0, false);
        let grey: *mut Object = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![l, setTextColor: grey];
        l
    }

    unsafe fn width(view: *mut Object, w: f64) {
        let anchor: *mut Object = msg_send![view, widthAnchor];
        let c: *mut Object = msg_send![anchor, constraintEqualToConstant: w];
        let _: () = msg_send![c, setActive: YES];
    }

    unsafe fn height(view: *mut Object, h: f64) {
        let anchor: *mut Object = msg_send![view, heightAnchor];
        let c: *mut Object = msg_send![anchor, constraintEqualToConstant: h];
        let _: () = msg_send![c, setActive: YES];
    }

    /// A multi-line label that wraps at `width`.
    unsafe fn wrapping(text: &str, width: f64) -> *mut Object {
        let l: *mut Object =
            msg_send![class!(NSTextField), wrappingLabelWithString: ns_string(text)];
        let _: () = msg_send![l, setPreferredMaxLayoutWidth: width];
        let _: () = msg_send![l, setMaximumNumberOfLines: 0isize];
        let _: () = msg_send![l, setLineBreakMode: 0isize]; // word wrap
        let cell: *mut Object = msg_send![l, cell];
        let _: () = msg_send![cell, setWraps: YES];
        // Grow to fit every line rather than being squeezed to one.
        let _: () =
            msg_send![l, setContentCompressionResistancePriority: 1000.0f32 forOrientation: 1isize];
        Self::width(l, width);
        l
    }

    /// Views side by side.
    unsafe fn hstack(views: &[*mut Object], spacing: f64) -> *mut Object {
        let st: *mut Object = msg_send![class!(NSStackView), stackViewWithViews: ns_array(views)];
        let _: () = msg_send![st, setOrientation: 0isize]; // horizontal
        let _: () = msg_send![st, setSpacing: spacing];
        st
    }

    unsafe fn check(&mut self, title: &str, cmd: Command) -> *mut Object {
        let b: *mut Object = msg_send![class!(NSButton),
            checkboxWithTitle: ns_string(title) target: action_target() action: sel!(menuAction:)];
        let _: () = msg_send![b, setTag: cmd.tag()];
        self.checks.push((cmd, b));
        b
    }

    unsafe fn button(title: &str, cmd: Command) -> *mut Object {
        let b: *mut Object = msg_send![class!(NSButton),
            buttonWithTitle: ns_string(title) target: action_target() action: sel!(menuAction:)];
        let _: () = msg_send![b, setTag: cmd.tag()];
        b
    }

    unsafe fn popup(&mut self, choices: Vec<(&str, Command)>) -> *mut Object {
        let p: *mut Object = msg_send![class!(NSPopUpButton), alloc];
        let p: *mut Object = msg_send![p,
            initWithFrame: NSRect { x: 0.0, y: 0.0, w: 240.0, h: 26.0 } pullsDown: NO];
        for (title, cmd) in &choices {
            let _: () = msg_send![p, addItemWithTitle: ns_string(title)];
            let item: *mut Object = msg_send![p, lastItem];
            let _: () = msg_send![item, setTag: cmd.tag()];
        }
        let _: () = msg_send![p, setTarget: action_target()];
        let _: () = msg_send![p, setAction: sel!(popupAction:)];
        let _: *mut Object = msg_send![p, autorelease];
        self.popups.push(Popup {
            button: p,
            choices: choices.into_iter().map(|(_, c)| c).collect(),
        });
        p
    }

    unsafe fn slider(&mut self, tag: isize) -> *mut Object {
        let s: *mut Object = msg_send![class!(NSSlider),
            sliderWithValue: 50.0f64 minValue: 0.0f64 maxValue: 100.0f64
            target: action_target() action: sel!(sliderAction:)];
        let _: () = msg_send![s, setTag: tag];
        let _: () = msg_send![s, setContinuous: YES];
        Self::width(s, 260.0);
        self.sliders.push((tag, s));
        s
    }

    /// A level bar 0..100 that turns yellow then red as `danger` says, with a
    /// value label beside it.
    unsafe fn bar(&mut self, danger: Danger) -> *mut Object {
        let l: *mut Object = msg_send![class!(NSLevelIndicator), alloc];
        let l: *mut Object =
            msg_send![l, initWithFrame: NSRect { x: 0.0, y: 0.0, w: 260.0, h: 16.0 }];
        let _: *mut Object = msg_send![l, autorelease];
        let _: () = msg_send![l, setLevelIndicatorStyle: 1usize]; // continuous capacity
        let _: () = msg_send![l, setMinValue: 0.0f64];
        let _: () = msg_send![l, setMaxValue: 100.0f64];
        // Critical below warning means *low* values are the bad ones.
        let (warn, crit) = match danger {
            Danger::High => (50.0f64, 75.0f64),
            Danger::Low => (20.0, 10.0),
            Danger::None => (0.0, 0.0),
        };
        let _: () = msg_send![l, setWarningValue: warn];
        let _: () = msg_send![l, setCriticalValue: crit];
        Self::width(l, 260.0);
        Self::height(l, 16.0);
        let value = Self::label("0");
        Self::font(value, 11.0, false);
        Self::width(value, 130.0);
        self.bars.push((l, value));
        Self::hstack(&[l, value], 10.0)
    }

    /// A tab's content: sections of labelled rows, top-aligned.
    unsafe fn page(rows: Vec<Row>) -> *mut Object {
        let empty = || -> *mut Object { msg_send![class!(NSGridCell), emptyContentView] };
        let mut views = Vec::with_capacity(rows.len());
        let mut sections = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            views.push(match row {
                Row::Section(title) => {
                    let l = Self::label(title);
                    Self::font(l, 13.0, true);
                    sections.push(i);
                    ns_array(&[l, empty()])
                }
                Row::Field(label, control) => ns_array(&[Self::label(label), *control]),
                Row::Under(control) => ns_array(&[empty(), *control]),
            });
        }
        let grid: *mut Object = msg_send![class!(NSGridView), gridViewWithViews: ns_array(&views)];
        let _: () = msg_send![grid, setRowSpacing: 8.0f64];
        let _: () = msg_send![grid, setColumnSpacing: 12.0f64];
        let _: () = msg_send![grid, setContentHuggingPriority: 999.0f32 forOrientation: 1isize];
        for i in 0..rows.len() {
            let row: *mut Object = msg_send![grid, rowAtIndex: i as isize];
            let _: () = msg_send![row, setYPlacement: 4isize]; // centre
        }
        for &i in &sections {
            let row: *mut Object = msg_send![grid, rowAtIndex: i as isize];
            if i > 0 {
                let _: () = msg_send![row, setTopPadding: 12.0f64];
            }
            // Headings span both columns, flush left.
            let _: () = msg_send![row, mergeCellsInRange: NSRange { location: 0, length: 2 }];
            let cell: *mut Object = msg_send![row, cellAtIndex: 0isize];
            let _: () = msg_send![cell, setXPlacement: 2isize]; // leading
        }
        let first: *mut Object = msg_send![grid, columnAtIndex: 0isize];
        let _: () = msg_send![first, setXPlacement: 3isize]; // labels trailing
                                                             // The same label column on every tab (and a tab with no labels at all
                                                             // doesn't collapse it).
        let _: () = msg_send![first, setWidth: 130.0f64];
        // A spacer under the grid soaks up the rest of the tab's height.
        let spacer: *mut Object = msg_send![class!(NSView), new];
        let _: () = msg_send![spacer, setContentHuggingPriority: 1.0f32 forOrientation: 1isize];
        let _: *mut Object = msg_send![spacer, autorelease];
        let stack: *mut Object =
            msg_send![class!(NSStackView), stackViewWithViews: ns_array(&[grid, spacer])];
        let _: () = msg_send![stack, setOrientation: 1isize]; // vertical
        let _: () = msg_send![stack, setAlignment: 5isize]; // leading
        let _: () = msg_send![stack, setEdgeInsets: NSEdgeInsets { top: 16.0, left: 24.0, bottom: 16.0, right: 24.0 }];
        stack
    }

    // --- Tabs -------------------------------------------------------------------

    unsafe fn mind_tab(&mut self) -> *mut Object {
        let headline = Self::wrapping("—", 420.0);
        Self::font(headline, 15.0, true);
        self.headline = headline;
        let bars: Vec<*mut Object> = BARS.iter().map(|(_, d)| self.bar(*d)).collect();
        let labels: Vec<*mut Object> = (0..4).map(|_| Self::wrapping("—", 420.0)).collect();
        self.mind_labels = labels.clone();
        // The memory log: a read-only scrolling text view.
        let scroll: *mut Object = msg_send![class!(NSTextView), scrollableTextView];
        let tv: *mut Object = msg_send![scroll, documentView];
        let _: () = msg_send![tv, setEditable: NO];
        let small: *mut Object = msg_send![class!(NSFont), systemFontOfSize: 11.0f64];
        let _: () = msg_send![tv, setFont: small];
        Self::width(scroll, 420.0);
        Self::height(scroll, 110.0);
        self.log = tv;
        let dscroll: *mut Object = msg_send![class!(NSTextView), scrollableTextView];
        let dtv: *mut Object = msg_send![dscroll, documentView];
        let _: () = msg_send![dtv, setEditable: NO];
        let _: () = msg_send![dtv, setFont: small];
        Self::width(dscroll, 420.0);
        Self::height(dscroll, 90.0);
        self.diary = dtv;
        let achievements = Self::wrapping("—", 420.0);
        self.achievements = achievements;
        let reflect = self.check(
            "Reflects on its memories with the local AI",
            Command::ToggleReflection,
        );
        let notes = self.check(
            "Spells notes with letters torn off the screen",
            Command::ToggleNotes,
        );
        let actions = Self::hstack(
            &[
                Self::button("Reflect Now", Command::ReflectNow),
                Self::button("Leave Me a Note", Command::WriteNote),
                Self::button("Write Diary Now", Command::WriteDiary),
                Self::button("Copy Latest Entry", Command::CopyDiary),
            ],
            8.0,
        );
        let resets = Self::hstack(
            &[
                Self::button("Wipe Its Memory", Command::ResetMind),
                Self::button("Forgive & Reset Level", Command::ResetStats),
            ],
            8.0,
        );
        let mut rows = vec![Row::Field("Mood:", headline), Row::Section("How it feels")];
        for ((name, _), bar) in BARS.iter().zip(bars) {
            rows.push(Row::Field(name, bar));
        }
        rows.extend([
            Row::Section("What it's thinking"),
            Row::Field("Right now:", labels[0]),
            Row::Field("Scheme:", labels[1]),
            Row::Field("Believes:", labels[2]),
            Row::Field("Next note:", labels[3]),
            Row::Field("Memory:", scroll),
            Row::Field("Diary:", dscroll),
            Row::Section("Achievements"),
            Row::Under(achievements),
            Row::Section("Its mind"),
            Row::Under(reflect),
            Row::Under(notes),
            Row::Under(actions),
            Row::Under(resets),
            Row::Under(Self::hint(
                "Wiping forgets every memory, feeling, belief and scheme. Resetting the level \
                 forgets its experience and grudge.",
            )),
        ]);
        Self::page(rows)
    }

    unsafe fn personality_tab(&mut self) -> *mut Object {
        let name: *mut Object =
            msg_send![class!(NSTextField), textFieldWithString: ns_string("Stick")];
        let _: () = msg_send![name, setTarget: action_target()];
        let _: () = msg_send![name, setAction: sel!(textAction:)];
        let cell: *mut Object = msg_send![name, cell];
        let _: () = msg_send![cell, setSendsActionOnEndEditing: YES];
        Self::width(name, 260.0);
        self.name = name;
        let color = self.popup(
            FigureColor::ALL
                .iter()
                .map(|c| (c.label(), Command::SetColor(*c)))
                .collect(),
        );
        let size = self.popup(
            Size::ALL
                .iter()
                .map(|s| (s.label(), Command::SetSize(*s)))
                .collect(),
        );
        let hat = self.popup(
            Hat::ALL
                .iter()
                .map(|h| (h.label(), Command::SetHat(*h)))
                .collect(),
        );
        // Locked hats are greyed out (we enable items ourselves).
        let _: () = msg_send![hat, setAutoenablesItems: NO];
        self.hat_popup = hat;
        let calls = self.check(
            "Quiet and harmless during video calls",
            Command::TogglePoliteCalls,
        );
        let nights = self.check("Sleepy late at night", Command::ToggleSleepyNights);
        let earn = self.check("Earn moves by levelling up", Command::ToggleEarnMoves);
        let aggression = self.slider(SLIDER_AGGRESSION);
        let chattiness = self.slider(SLIDER_CHATTINESS);
        let language = self.popup(
            Language::ALL
                .iter()
                .map(|l| (l.label(), Command::SetLanguage(*l)))
                .collect(),
        );
        let bleep = self.check("Bleep swear words (#$%&!)", Command::ToggleBleep);
        let temper = self.check(
            "Loses its temper when it's had enough",
            Command::ToggleTemper,
        );
        let behaviour = self.popup(
            Behaviour::ALL
                .iter()
                .map(|b| (b.label(), Command::SetBehaviour(*b)))
                .collect(),
        );
        Self::page(vec![
            Row::Section("Who it is"),
            Row::Field("Name:", name),
            Row::Field("Colour:", color),
            Row::Field("Size:", size),
            Row::Field("Hat:", hat),
            Row::Under(Self::hint("Hats are earned with achievements and levels.")),
            Row::Section("Temperament"),
            Row::Field("Temper:", aggression),
            Row::Under(Self::hint(
                "Gentle  ←  how often it picks fights  →  Fierce",
            )),
            Row::Field("Chattiness:", chattiness),
            Row::Under(Self::hint("Quiet  ←  how much it talks  →  Chatterbox")),
            Row::Field("Language:", language),
            Row::Under(bleep),
            Row::Under(temper),
            Row::Under(Self::hint(
                "When its resentment boils over it goes red, steams, swears whatever its \
                 language, and attacks without mercy until it calms down. Swearing never \
                 includes slurs or anything hateful.",
            )),
            Row::Section("What it does"),
            Row::Field("Behaviour:", behaviour),
            Row::Under(calls),
            Row::Under(nights),
            Row::Under(Self::hint(
                "While a camera is in use it whispers once, then goes silent and leaves your \
                 cursor and windows alone. After 11 pm it naps much sooner.",
            )),
            Row::Under(earn),
            Row::Under(Self::hint(
                "Like a game: super jump at level 2, telekinesis at 3, swords at 4, \
                 window throwing at 5, minions at 6. Off: everything from the start.",
            )),
            Row::Under(Self::hint(
                "Auto lets the local AI director decide; the others pin one mood.",
            )),
        ])
    }

    unsafe fn abilities_tab(&mut self) -> *mut Object {
        let push = self.check(
            "Punches push my cursor; grabs throw it",
            Command::ToggleCursorPush,
        );
        let shake = self.check("Stomps shake my windows", Command::ToggleWindowShake);
        let shove = self.check("Kicks slide my windows around", Command::ToggleWindowShoves);
        let throw_win = self.check(
            "Telekinesis can throw my windows",
            Command::ToggleThrowWindows,
        );
        let walk = self.check("Walk on text (reads the screen)", Command::ToggleWalkOnText);
        let words = self.check(
            "Rip words off the screen & throw them",
            Command::ToggleThrowWords,
        );
        let stay = self.popup(
            WordStay::ALL
                .iter()
                .map(|w| (w.label(), Command::SetWordStay(*w)))
                .collect(),
        );
        let icons = self.check(
            "Rip icons off the Dock & throw them",
            Command::TogglePeltIcons,
        );
        let weapons = self.check(
            "Rip scrollbars off windows as swords",
            Command::ToggleWeapons,
        );
        let powers = self.check("Super jump & telekinesis", Command::TogglePowers);
        let intro = self.check("Jail-break intro at launch", Command::ToggleJailIntro);
        let rival = self.check(
            "A rival drops in to duel now and then",
            Command::ToggleRivalVisits,
        );
        let piles = self.check(
            "Thrown words & icons pile up (it can climb them)",
            Command::TogglePiles,
        );
        let minions = self.check(
            "Calls in a minion army now and then",
            Command::ToggleMinions,
        );
        let boss = self.check(
            "Challenges your cursor to boss fights",
            Command::ToggleBossFights,
        );
        let now = Self::hstack(
            &[
                Self::button("Start a Boss Fight", Command::StartBoss),
                Self::button("Call the Minions", Command::CallMinions),
            ],
            8.0,
        );
        Self::page(vec![
            Row::Section("Your cursor"),
            Row::Under(push),
            Row::Section("Your windows"),
            Row::Under(shake),
            Row::Under(shove),
            Row::Under(throw_win),
            Row::Under(Self::hint(
                "Window effects need Accessibility permission. Windows always stay on screen.",
            )),
            Row::Section("Your screen"),
            Row::Under(walk),
            Row::Under(words),
            Row::Field("Thrown words stay:", stay),
            Row::Under(icons),
            Row::Under(weapons),
            Row::Under(piles),
            Row::Under(Self::hint(
                "Words, letters and icons are copies: nothing in your apps changes. Needs \
                 Screen Recording permission.",
            )),
            Row::Section("Powers and events"),
            Row::Under(powers),
            Row::Under(intro),
            Row::Under(rival),
            Row::Under(minions),
            Row::Under(boss),
            Row::Under(now),
            Row::Under(Self::hint(
                "Boss fight: your cursor gets a health bar; click the figure to fight back. \
                 Minions (3 small allies) come at level 6; click one to knock it out.",
            )),
        ])
    }

    unsafe fn fun_tab(&mut self) -> *mut Object {
        let naps = self.check("Naps when I'm away", Command::ToggleNaps);
        let dance = self.check("Dances now and then", Command::ToggleDancing);
        let music = self.check("Dances when music plays", Command::ToggleDanceToMusic);
        let doodles = self.check("Doodles in the air", Command::ToggleDoodles);
        let sit = self.check("Sits on window edges", Command::ToggleSitting);
        let speech = self.check("Speech bubbles", Command::ToggleSpeech);
        let sound = self.check("Sound effects", Command::ToggleSound);
        let volume = self.slider(SLIDER_VOLUME);
        let power = self.popup(
            PowerSaving::ALL
                .iter()
                .map(|p| (p.label(), Command::SetPowerSaving(*p)))
                .collect(),
        );
        Self::page(vec![
            Row::Section("Hobbies"),
            Row::Under(naps),
            Row::Under(dance),
            Row::Under(music),
            Row::Under(doodles),
            Row::Under(sit),
            Row::Section("Talking and sound"),
            Row::Under(speech),
            Row::Under(sound),
            Row::Field("Volume:", volume),
            Row::Under(Self::hint(
                "Every sound is made up on the spot (no audio files): punches, whooshes, \
                 booms, clanks, a babble when it talks.",
            )),
            Row::Under(Self::hint(
                "Drawing platforms (⌘D), notes and rivals are in the menu bar menu.",
            )),
            Row::Section("Power"),
            Row::Field("Power saving:", power),
            Row::Under(Self::hint(
                "Saving power: the local AI is asked less often (8 s apart, at least every \
                 two minutes) and it draws at 30 frames a second. Automatic does this only on \
                 battery, and says so when you unplug.",
            )),
        ])
    }

    // --- State ----------------------------------------------------------------

    /// Show the mind's state (cheap when nothing changed or hidden).
    pub fn set_mind(&mut self, view: &MindView) {
        if self.mind_shown.as_ref() == Some(view) || !self.is_visible() {
            return;
        }
        // SAFETY: main thread; the controls live as long as the window.
        unsafe {
            let _: () = msg_send![self.headline, setStringValue: ns_string(&view.headline)];
            for ((bar, label), (value, text)) in self.bars.iter().zip(&view.bars) {
                let _: () = msg_send![*bar, setDoubleValue: value.clamp(0.0, 100.0)];
                let _: () = msg_send![*label, setStringValue: ns_string(text)];
            }
            for (l, text) in self.mind_labels.iter().zip([
                &view.thought,
                &view.scheme,
                &view.beliefs,
                &view.note,
            ]) {
                let shown = if text.is_empty() {
                    "—"
                } else {
                    text.as_str()
                };
                let _: () = msg_send![*l, setStringValue: ns_string(shown)];
            }
            let _: () = msg_send![self.log, setString: ns_string(&view.log)];
            let diary = if view.diary.is_empty() {
                "No entries yet: it writes one at the end of each day."
            } else {
                view.diary.as_str()
            };
            let _: () = msg_send![self.diary, setString: ns_string(diary)];
        }
        self.mind_shown = Some(view.clone());
    }

    /// Show which hats are unlocked (locked ones say how to earn them), and
    /// the achievements text.
    pub fn set_progress(&mut self, unlocked: &[bool], achievements: &str) {
        if self.hats_shown.as_slice() != unlocked {
            // SAFETY: main thread; the pop-up lives as long as the window.
            unsafe {
                for (i, (hat, open)) in Hat::ALL.iter().zip(unlocked).enumerate() {
                    let item: *mut Object = msg_send![self.hat_popup, itemAtIndex: i as isize];
                    if item.is_null() {
                        continue;
                    }
                    let title = match (open, hat.how()) {
                        (false, Some(how)) => format!("🔒 {} — {how}", hat.label()),
                        _ => hat.label().to_string(),
                    };
                    let _: () = msg_send![item, setTitle: ns_string(&title)];
                    let _: () = msg_send![item, setEnabled: if *open { YES } else { NO }];
                }
            }
            self.hats_shown = unlocked.to_vec();
        }
        // SAFETY: main thread; the label lives as long as the window.
        unsafe {
            let _: () = msg_send![self.achievements, setStringValue: ns_string(achievements)];
        }
    }

    /// Bring the window up, showing the current settings.
    pub fn show(&mut self, s: &Settings) {
        self.shown = None;
        self.mind_shown = None;
        self.sync(s);
        // SAFETY: main thread; `window` is alive (see type invariants). An
        // accessory app must activate for its window to take keyboard input.
        unsafe {
            let name = ns_string(&s.name);
            let _: () = msg_send![self.name, setStringValue: name];
            if !self.centered {
                let _: () = msg_send![self.window, center];
                self.centered = true;
            }
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: YES];
            let _: () =
                msg_send![self.window, makeKeyAndOrderFront: std::ptr::null_mut::<Object>()];
        }
    }

    /// Switch to the tab labelled `label` (e.g. "Mind").
    /// Unknown labels are ignored (AppKit throws for them, which would abort).
    pub fn select_tab(&self, label: &str) {
        // SAFETY: main thread; the tab view lives as long as the window.
        // `indexOfTabViewItemWithIdentifier:` returns NSNotFound (isize::MAX)
        // rather than throwing, so we only select a tab that exists.
        unsafe {
            let id = ns_string(label);
            let i: isize = msg_send![self.tabs, indexOfTabViewItemWithIdentifier: id];
            if i == isize::MAX {
                log::warn!("settings window: no tab named {label:?}");
                return;
            }
            let _: () = msg_send![self.tabs, selectTabViewItemAtIndex: i];
        }
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: main thread; live window.
        unsafe {
            let v: objc::runtime::BOOL = msg_send![self.window, isVisible];
            v != NO
        }
    }

    pub fn window_number(&self) -> isize {
        // SAFETY: main thread; live window.
        unsafe { msg_send![self.window, windowNumber] }
    }

    /// Reflect `s` in the controls (cheap when nothing changed).
    pub fn sync(&mut self, s: &Settings) {
        if self.shown.as_ref() == Some(s) {
            return;
        }
        // SAFETY: main thread; the controls live as long as the window.
        unsafe {
            for (cmd, b) in &self.checks {
                let on = if s.checked(*cmd) { 1isize } else { 0 };
                let _: () = msg_send![*b, setState: on];
            }
            for p in &self.popups {
                if let Some(c) = p.choices.iter().find(|c| s.checked(**c)) {
                    let _: objc::runtime::BOOL = msg_send![p.button, selectItemWithTag: c.tag()];
                }
            }
            for (tag, slider) in &self.sliders {
                let v = match *tag {
                    SLIDER_AGGRESSION => s.aggression,
                    SLIDER_CHATTINESS => s.chattiness,
                    SLIDER_VOLUME => s.volume,
                    _ => continue,
                };
                let _: () = msg_send![*slider, setDoubleValue: f64::from(v)];
            }
        }
        self.shown = Some(s.clone());
    }
}

impl Default for SettingsWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        // SAFETY: we own one reference from alloc/init.
        unsafe {
            let _: () = msg_send![self.window, close];
            let _: () = msg_send![self.window, release];
        }
    }
}
