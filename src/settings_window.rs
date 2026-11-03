//! The Settings window: every option in one place, in three tabs
//! (Personality, Abilities, Hobbies & Sound), built from native AppKit
//! controls.
//!
//! Controls talk to the main loop through the same Objective-C target as the
//! menu bar: checkboxes and pop-up menus send [`Command`] tags, sliders and the
//! name field send [`crate::settings::Change`]s. Main thread only.

#![allow(unexpected_cfgs)] // objc 0.2 macros

use std::ffi::CString;

use objc::runtime::{Object, NO, YES};
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};

use crate::menubar::{action_target, SLIDER_AGGRESSION, SLIDER_CHATTINESS};
use crate::settings::{Behaviour, Command, FigureColor, Language, Settings, Size, WordStay};

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

/// The settings window. Built once, shown and hidden on demand. Holds +1
/// references to the window (released in Drop); the controls are owned by the
/// window's view hierarchy, which lives as long as the window.
pub struct SettingsWindow {
    window: *mut Object,
    checks: Vec<(Command, *mut Object)>,
    popups: Vec<Popup>,
    sliders: Vec<(isize, *mut Object)>,
    name: *mut Object,
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
                initWithContentRect: NSRect { x: 0.0, y: 0.0, w: 520.0, h: 470.0 }
                styleMask: 1usize | 2 | 4
                backing: 2usize
                defer: NO];
            let _: () = msg_send![window, setReleasedWhenClosed: NO];
            let _: () = msg_send![window, setTitle: ns_string("Stickman Settings")];
            // Floating: stays above the app you were in while you adjust things.
            let _: () = msg_send![window, setLevel: 3isize];
            w.window = window;

            let tabs: *mut Object = msg_send![class!(NSTabView), new];
            let personality = w.personality_tab();
            let abilities = w.abilities_tab();
            let hobbies = w.hobbies_tab();
            for (label, view) in [
                ("Personality", personality),
                ("Abilities", abilities),
                ("Hobbies & Sound", hobbies),
            ] {
                let item: *mut Object = msg_send![class!(NSTabViewItem), alloc];
                let item: *mut Object = msg_send![item, initWithIdentifier: ns_string(label)];
                let _: () = msg_send![item, setLabel: ns_string(label)];
                let _: () = msg_send![item, setView: view];
                let _: () = msg_send![tabs, addTabViewItem: item];
                let _: () = msg_send![item, release];
            }
            let _: () = msg_send![window, setContentView: tabs];
            let _: () = msg_send![tabs, release];
        }
        w
    }

    // --- Controls -------------------------------------------------------------

    unsafe fn label(text: &str) -> *mut Object {
        msg_send![class!(NSTextField), labelWithString: ns_string(text)]
    }

    /// Small grey help text.
    unsafe fn hint(text: &str) -> *mut Object {
        let l = Self::label(text);
        let font: *mut Object = msg_send![class!(NSFont), systemFontOfSize: 11.0f64];
        let grey: *mut Object = msg_send![class!(NSColor), secondaryLabelColor];
        let _: () = msg_send![l, setFont: font];
        let _: () = msg_send![l, setTextColor: grey];
        l
    }

    unsafe fn width(view: *mut Object, w: f64) {
        let anchor: *mut Object = msg_send![view, widthAnchor];
        let c: *mut Object = msg_send![anchor, constraintEqualToConstant: w];
        let _: () = msg_send![c, setActive: YES];
    }

    unsafe fn check(&mut self, title: &str, cmd: Command) -> *mut Object {
        let b: *mut Object = msg_send![class!(NSButton),
            checkboxWithTitle: ns_string(title) target: action_target() action: sel!(menuAction:)];
        let _: () = msg_send![b, setTag: cmd.tag()];
        self.checks.push((cmd, b));
        b
    }

    unsafe fn popup(&mut self, choices: Vec<(&str, Command)>) -> *mut Object {
        let p: *mut Object = msg_send![class!(NSPopUpButton), alloc];
        let p: *mut Object = msg_send![p,
            initWithFrame: NSRect { x: 0.0, y: 0.0, w: 220.0, h: 26.0 } pullsDown: NO];
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
        Self::width(s, 240.0);
        self.sliders.push((tag, s));
        s
    }

    /// A tab's content: a two-column grid (labels | controls) at the top.
    unsafe fn grid(rows: Vec<(&str, *mut Object)>) -> *mut Object {
        let empty: *mut Object = msg_send![class!(NSGridCell), emptyContentView];
        let n_rows = rows.len();
        let rows: Vec<*mut Object> = rows
            .into_iter()
            .map(|(label, control)| {
                let l = if label.is_empty() {
                    empty
                } else {
                    Self::label(label)
                };
                ns_array(&[l, control])
            })
            .collect();
        let grid: *mut Object = msg_send![class!(NSGridView), gridViewWithViews: ns_array(&rows)];
        let _: () = msg_send![grid, setRowSpacing: 9.0f64];
        let _: () = msg_send![grid, setColumnSpacing: 12.0f64];
        // Keep rows at their natural height; spare space goes below the grid.
        let _: () = msg_send![grid, setContentHuggingPriority: 999.0f32 forOrientation: 1isize];
        for i in 0..n_rows {
            let row: *mut Object = msg_send![grid, rowAtIndex: i as isize];
            let _: () = msg_send![row, setYPlacement: 4isize]; // centre
        }
        let first: *mut Object = msg_send![grid, columnAtIndex: 0isize];
        let _: () = msg_send![first, setXPlacement: 3isize]; // trailing
                                                             // A spacer under the grid soaks up the rest of the tab's height.
        let spacer: *mut Object = msg_send![class!(NSView), new];
        let _: () = msg_send![spacer, setContentHuggingPriority: 1.0f32 forOrientation: 1isize];
        let _: *mut Object = msg_send![spacer, autorelease];
        let stack: *mut Object =
            msg_send![class!(NSStackView), stackViewWithViews: ns_array(&[grid, spacer])];
        let _: () = msg_send![stack, setOrientation: 1isize]; // vertical
        let _: () = msg_send![stack, setAlignment: 5isize]; // leading
        let _: () = msg_send![stack, setEdgeInsets: NSEdgeInsets { top: 18.0, left: 24.0, bottom: 18.0, right: 24.0 }];
        stack
    }

    unsafe fn personality_tab(&mut self) -> *mut Object {
        let name: *mut Object =
            msg_send![class!(NSTextField), textFieldWithString: ns_string("Stick")];
        let _: () = msg_send![name, setTarget: action_target()];
        let _: () = msg_send![name, setAction: sel!(textAction:)];
        let cell: *mut Object = msg_send![name, cell];
        let _: () = msg_send![cell, setSendsActionOnEndEditing: YES];
        Self::width(name, 240.0);
        self.name = name;
        let aggression = self.slider(SLIDER_AGGRESSION);
        let chattiness = self.slider(SLIDER_CHATTINESS);
        let language = self.popup(
            Language::ALL
                .iter()
                .map(|l| (l.label(), Command::SetLanguage(*l)))
                .collect(),
        );
        let bleep = self.check("Bleep swear words (#$%&!)", Command::ToggleBleep);
        let behaviour = self.popup(
            Behaviour::ALL
                .iter()
                .map(|b| (b.label(), Command::SetBehaviour(*b)))
                .collect(),
        );
        let color = self.popup(
            FigureColor::ALL
                .iter()
                .map(|c| (c.label(), Command::SetColor(*c)))
                .collect(),
        );
        let forgive: *mut Object = msg_send![class!(NSButton),
            buttonWithTitle: ns_string("Forgive & Reset Level") target: action_target() action: sel!(menuAction:)];
        let _: () = msg_send![forgive, setTag: Command::ResetStats.tag()];
        let size = self.popup(
            Size::ALL
                .iter()
                .map(|s| (s.label(), Command::SetSize(*s)))
                .collect(),
        );
        Self::grid(vec![
            ("Name:", name),
            ("Temper:", aggression),
            (
                "",
                Self::hint("Gentle  ←  how often it picks fights  →  Fierce"),
            ),
            ("Chattiness:", chattiness),
            ("", Self::hint("Quiet  ←  how much it talks  →  Chatterbox")),
            ("Language:", language),
            ("", bleep),
            (
                "",
                Self::hint("Salty swears, but never slurs or anything hateful."),
            ),
            ("Behaviour:", behaviour),
            ("Colour:", color),
            ("Size:", size),
            ("Record:", forgive),
            ("", Self::hint("Wipes its experience, level and grudge.")),
        ])
    }

    unsafe fn abilities_tab(&mut self) -> *mut Object {
        let push = self.check(
            "Punches push my cursor, grabs throw it",
            Command::ToggleCursorPush,
        );
        let shake = self.check("Stomps shake my windows", Command::ToggleWindowShake);
        let shove = self.check("Kicks slide my windows around", Command::ToggleWindowShoves);
        let walk = self.check("Walk on text (reads the screen)", Command::ToggleWalkOnText);
        let words = self.check(
            "Rip words off the screen & throw them",
            Command::ToggleThrowWords,
        );
        let icons = self.check(
            "Rip icons off the Dock & throw them",
            Command::TogglePeltIcons,
        );
        let stay = self.popup(
            WordStay::ALL
                .iter()
                .map(|w| (w.label(), Command::SetWordStay(*w)))
                .collect(),
        );
        let powers = self.check("Super jump & telekinesis", Command::TogglePowers);
        let throw_win = self.check(
            "Telekinesis can throw my windows",
            Command::ToggleThrowWindows,
        );
        let intro = self.check("Jail-break intro at launch", Command::ToggleJailIntro);
        let rival = self.check(
            "A rival drops in to duel now and then",
            Command::ToggleRivalVisits,
        );
        Self::grid(vec![
            ("Cursor:", push),
            ("Windows:", shake),
            ("", shove),
            (
                "",
                Self::hint("Window effects need Accessibility permission."),
            ),
            ("Text:", walk),
            ("", words),
            ("Dock:", icons),
            ("Thrown words stay:", stay),
            ("Powers:", powers),
            ("", throw_win),
            ("Intro:", intro),
            ("Rival:", rival),
        ])
    }

    unsafe fn hobbies_tab(&mut self) -> *mut Object {
        let naps = self.check("Naps when I'm away", Command::ToggleNaps);
        let dance = self.check("Dancing", Command::ToggleDancing);
        let doodles = self.check("Doodles in the air", Command::ToggleDoodles);
        let sit = self.check("Sitting on window edges", Command::ToggleSitting);
        let music = self.check("Dance when music plays", Command::ToggleDanceToMusic);
        let speech = self.check("Speech bubbles", Command::ToggleSpeech);
        let sound = self.check("Sound effects", Command::ToggleSound);
        Self::grid(vec![
            ("Hobbies:", naps),
            ("", dance),
            ("", doodles),
            ("", sit),
            ("", music),
            ("Talking:", speech),
            ("Sound:", sound),
        ])
    }

    // --- State ----------------------------------------------------------------

    /// Bring the window up, showing the current settings.
    pub fn show(&mut self, s: &Settings) {
        self.shown = None;
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
