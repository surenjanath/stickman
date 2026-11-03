//! Menu bar extra (like the Wi-Fi icon): a little stickman icon whose pose shows
//! the figure's mood, with a menu of settings.
//!
//! Menu picks are delivered by an Objective-C target object to a queue that the
//! main loop drains every tick ([`MenuBar::drain_commands`]). Everything here runs
//! on the main thread (AppKit requirement); invariants are noted per `unsafe`.

#![allow(unexpected_cfgs)] // objc 0.2 macros

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Mutex, Once};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel, NO, YES};
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};

use crate::agent::Mode;
use crate::poses;
use crate::settings::{Behaviour, Command, Settings, Size};
use crate::stickman::{forward_kinematics, idle_pose, BoneKind, Pose};

/// Menu picks waiting for the main loop.
static QUEUE: Mutex<Vec<isize>> = Mutex::new(Vec::new());
static REGISTER: Once = Once::new();

/// `-[StykMenuTarget menuAction:]`: queue the clicked item's tag.
extern "C" fn menu_action(_this: &Object, _sel: Sel, sender: *mut Object) {
    if sender.is_null() {
        return;
    }
    // SAFETY: AppKit passes the NSMenuItem that was chosen.
    let tag: isize = unsafe { msg_send![sender, tag] };
    if let Ok(mut q) = QUEUE.lock() {
        q.push(tag);
    }
}

fn target_class() -> &'static Class {
    REGISTER.call_once(|| {
        let mut decl =
            ClassDecl::new("StykMenuTarget", class!(NSObject)).expect("register StykMenuTarget");
        // SAFETY: the signature matches an action method `- (void)menuAction:(id)sender`.
        unsafe {
            decl.add_method(
                sel!(menuAction:),
                menu_action as extern "C" fn(&Object, Sel, *mut Object),
            );
        }
        decl.register();
    });
    class!(StykMenuTarget)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    w: f64,
    h: f64,
}
// SAFETY: same layout/encoding as CGSize on 64-bit.
unsafe impl Encode for NSSize {
    fn encode() -> Encoding {
        unsafe { Encoding::from_str("{CGSize=dd}") }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
// SAFETY: same layout/encoding as CGPoint on 64-bit.
unsafe impl Encode for NSPoint {
    fn encode() -> Encoding {
        unsafe { Encoding::from_str("{CGPoint=dd}") }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}
// SAFETY: same layout/encoding as CGRect on 64-bit.
unsafe impl Encode for NSRect {
    fn encode() -> Encoding {
        unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

/// Autoreleased NSString. Main thread (or any thread with a pool).
fn ns_string(s: &str) -> *mut Object {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    // SAFETY: valid NUL-terminated UTF-8.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

/// The pose shown in the menu bar icon for each mood.
pub fn icon_pose(mode: Mode) -> Pose {
    match mode {
        Mode::Idle => idle_pose(0.0),
        Mode::Observe => poses::think(0.0),
        Mode::Sabotage => poses::stomp(0.36),
        Mode::Combat => poses::stance(0.0),
    }
}

/// Lines of an icon-sized figure in a `size`×`size` box, y up (AppKit image
/// space): (from, to, width, is_head_circle).
pub fn icon_strokes(pose: &Pose, size: f32) -> Vec<([f32; 2], [f32; 2], f32, bool)> {
    let raw = forward_kinematics(pose, [0.0, 0.0], 1.0, 1.0);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in &raw {
        let pad = if s.kind == BoneKind::Circle {
            22.0
        } else {
            0.0
        };
        for p in [s.start, s.end] {
            x0 = x0.min(p[0] - pad);
            x1 = x1.max(p[0] + pad);
            y0 = y0.min(p[1] - pad);
            y1 = y1.max(p[1] + pad);
        }
    }
    let margin = 1.2;
    let k = (size - 2.0 * margin) / (x1 - x0).max(y1 - y0);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let map = |p: [f32; 2]| [size / 2.0 + (p[0] - cx) * k, size / 2.0 - (p[1] - cy) * k];
    raw.iter()
        .map(|s| {
            (
                map(s.start),
                map(s.end),
                (s.thickness * k * 1.25).max(1.6),
                s.kind == BoneKind::Circle,
            )
        })
        .collect()
}

/// Render the mood icon as a template NSImage (+1 retained).
fn make_icon(mode: Mode) -> *mut Object {
    const SIZE: f32 = 18.0;
    let strokes = icon_strokes(&icon_pose(mode), SIZE);
    // SAFETY: offscreen drawing into an NSImage on the main thread; all objects
    // except the returned image are autoreleased.
    unsafe {
        let img: *mut Object = msg_send![class!(NSImage), alloc];
        let img: *mut Object =
            msg_send![img, initWithSize: NSSize { w: f64::from(SIZE), h: f64::from(SIZE) }];
        let _: () = msg_send![img, lockFocus];
        let black: *mut Object = msg_send![class!(NSColor), blackColor];
        let _: () = msg_send![black, setStroke];
        for (a, b, w, head) in strokes {
            let path: *mut Object = if head {
                let (cx, cy) = ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
                let r = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt() / 2.0;
                let rect = NSRect {
                    origin: NSPoint {
                        x: f64::from(cx - r),
                        y: f64::from(cy - r),
                    },
                    size: NSSize {
                        w: f64::from(2.0 * r),
                        h: f64::from(2.0 * r),
                    },
                };
                msg_send![class!(NSBezierPath), bezierPathWithOvalInRect: rect]
            } else {
                let p: *mut Object = msg_send![class!(NSBezierPath), bezierPath];
                let _: () =
                    msg_send![p, moveToPoint: NSPoint { x: f64::from(a[0]), y: f64::from(a[1]) }];
                let _: () =
                    msg_send![p, lineToPoint: NSPoint { x: f64::from(b[0]), y: f64::from(b[1]) }];
                p
            };
            let _: () = msg_send![path, setLineWidth: f64::from(w)];
            let _: () = msg_send![path, setLineCapStyle: 1usize]; // NSLineCapStyleRound
            let _: () = msg_send![path, stroke];
        }
        let _: () = msg_send![img, unlockFocus];
        // Template: AppKit tints it for light/dark menu bars, like system icons.
        let _: () = msg_send![img, setTemplate: YES];
        img
    }
}

/// Live status shown at the top of the menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub mode: Mode,
    pub reason: String,
    pub health: f32,
    /// "llama3.2:1b — online", "offline (built-in director)", …
    pub director: String,
    pub accessibility: bool,
    pub screen_recording: bool,
}

/// The status item and its menu. Main thread only (`!Send` via raw pointers).
pub struct MenuBar {
    item: *mut Object,
    target: *mut Object,
    items: HashMap<isize, *mut Object>,
    mood: *mut Object,
    health: *mut Object,
    director: *mut Object,
    grant_ax: *mut Object,
    grant_screen: *mut Object,
    icon_mode: Option<Mode>,
    shown: Option<(Settings, Status)>,
}

impl MenuBar {
    /// Create the menu bar icon. Call on the main thread once AppKit is running.
    pub fn new() -> MenuBar {
        // SAFETY: main thread; objects we keep are retained (+1) and released in
        // Drop; the menu retains its items and the status item retains its menu.
        unsafe {
            let target: *mut Object = msg_send![target_class(), new];
            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            let item: *mut Object = msg_send![bar, statusItemWithLength: -1.0f64]; // variable length
            let item: *mut Object = msg_send![item, retain]; // the status bar doesn't keep it alive
            let _: () = msg_send![item, setAutosaveName: ns_string("StykAnimate")];
            let button: *mut Object = msg_send![item, button];
            let _: () = msg_send![button, setToolTip: ns_string("StykAnimate")];

            let mut mb = MenuBar {
                item,
                target,
                items: HashMap::new(),
                mood: std::ptr::null_mut(),
                health: std::ptr::null_mut(),
                director: std::ptr::null_mut(),
                grant_ax: std::ptr::null_mut(),
                grant_screen: std::ptr::null_mut(),
                icon_mode: None,
                shown: None,
            };
            let menu = mb.build_menu();
            let _: () = msg_send![item, setMenu: menu];
            mb.set_icon(Mode::Idle);
            mb
        }
    }

    unsafe fn new_menu(title: &str) -> *mut Object {
        let m: *mut Object = msg_send![class!(NSMenu), alloc];
        let m: *mut Object = msg_send![m, initWithTitle: ns_string(title)];
        // We set enabled states ourselves.
        let _: () = msg_send![m, setAutoenablesItems: NO];
        msg_send![m, autorelease]
    }

    /// Add an item to `menu`; actionable when `cmd` is given. Returns the item.
    unsafe fn add(
        &mut self,
        menu: *mut Object,
        title: &str,
        cmd: Option<Command>,
        key: &str,
    ) -> *mut Object {
        let action = if cmd.is_some() {
            sel!(menuAction:)
        } else {
            sel!(description)
        };
        let it: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let it: *mut Object = msg_send![it, initWithTitle: ns_string(title) action: action keyEquivalent: ns_string(key)];
        let _: () = msg_send![menu, addItem: it];
        let _: () = msg_send![it, release]; // the menu holds it now
        match cmd {
            Some(c) => {
                let _: () = msg_send![it, setTarget: self.target];
                let _: () = msg_send![it, setTag: c.tag()];
                self.items.insert(c.tag(), it);
            }
            None => {
                let _: () = msg_send![it, setEnabled: NO];
            }
        }
        it
    }

    unsafe fn separator(menu: *mut Object) {
        let sep: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep];
    }

    unsafe fn submenu(&mut self, menu: *mut Object, title: &str) -> *mut Object {
        let parent = self.add(menu, title, None, "");
        let _: () = msg_send![parent, setEnabled: YES];
        let sub = Self::new_menu(title);
        let _: () = msg_send![parent, setSubmenu: sub];
        sub
    }

    unsafe fn build_menu(&mut self) -> *mut Object {
        let menu = Self::new_menu("StykAnimate");
        let title = self.add(menu, "StykAnimate", None, "");
        let bold: *mut Object = msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64];
        let key = ns_string("NSFont");
        let attrs: *mut Object =
            msg_send![class!(NSDictionary), dictionaryWithObject: bold forKey: key];
        let at: *mut Object = msg_send![class!(NSAttributedString), alloc];
        let at: *mut Object =
            msg_send![at, initWithString: ns_string("StykAnimate") attributes: attrs];
        let _: () = msg_send![title, setAttributedTitle: at];
        let _: () = msg_send![at, release];
        self.mood = self.add(menu, "Mood: —", None, "");
        self.health = self.add(menu, "Health: 100%", None, "");
        self.director = self.add(menu, "AI: —", None, "");
        Self::separator(menu);

        let behaviour = self.submenu(menu, "Behaviour");
        for b in Behaviour::ALL {
            self.add(behaviour, b.label(), Some(Command::SetBehaviour(b)), "");
            if b == Behaviour::Auto {
                Self::separator(behaviour);
            }
        }
        let size = self.submenu(menu, "Figure Size");
        for s in Size::ALL {
            self.add(size, s.label(), Some(Command::SetSize(s)), "");
        }
        Self::separator(menu);
        self.add(
            menu,
            "Punches Push My Cursor",
            Some(Command::ToggleCursorPush),
            "",
        );
        self.add(
            menu,
            "Stomps Shake My Windows",
            Some(Command::ToggleWindowShake),
            "",
        );
        self.add(
            menu,
            "Walk on Text (reads the screen)",
            Some(Command::ToggleWalkOnText),
            "",
        );
        Self::separator(menu);
        self.add(menu, "Show Figure", Some(Command::ToggleVisible), "h");
        self.add(menu, "Pause", Some(Command::TogglePaused), "p");
        self.add(menu, "Respawn", Some(Command::Respawn), "r");
        Self::separator(menu);
        self.grant_ax = self.add(
            menu,
            "Grant Accessibility (window shakes)…",
            Some(Command::OpenAccessibilitySettings),
            "",
        );
        self.grant_screen = self.add(
            menu,
            "Grant Screen Recording (walk on text)…",
            Some(Command::OpenScreenRecordingSettings),
            "",
        );
        Self::separator(menu);
        self.add(menu, "Quit StykAnimate", Some(Command::Quit), "q");
        menu
    }

    fn set_icon(&mut self, mode: Mode) {
        if self.icon_mode == Some(mode) {
            return;
        }
        let img = make_icon(mode);
        // SAFETY: main thread; the button retains the image, we drop our +1.
        unsafe {
            let button: *mut Object = msg_send![self.item, button];
            let _: () = msg_send![button, setImage: img];
            let _: () = msg_send![img, release];
            let tip = format!("StykAnimate — {}", mode_name(mode));
            let _: () = msg_send![button, setToolTip: ns_string(&tip)];
        }
        self.icon_mode = Some(mode);
    }

    fn set_title(item: *mut Object, text: &str) {
        // SAFETY: `item` is a live NSMenuItem owned by our menu; main thread.
        unsafe {
            let _: () = msg_send![item, setTitle: ns_string(text)];
        }
    }

    fn set_checked(&self, cmd: Command, on: bool) {
        if let Some(&it) = self.items.get(&cmd.tag()) {
            // SAFETY: as above. NSControlStateValueOn = 1, Off = 0.
            unsafe {
                let _: () = msg_send![it, setState: if on { 1isize } else { 0isize }];
            }
        }
    }

    fn set_hidden(item: *mut Object, hidden: bool) {
        // SAFETY: as above.
        unsafe {
            let _: () = msg_send![item, setHidden: if hidden { YES } else { NO }];
        }
    }

    /// Reflect settings and live status in the icon and menu (cheap when unchanged).
    pub fn sync(&mut self, settings: &Settings, status: &Status) {
        self.set_icon(status.mode);
        if self.shown.as_ref() == Some(&(settings.clone(), status.clone())) {
            return;
        }
        let auto = if settings.behaviour == Behaviour::Auto {
            "AI"
        } else {
            "pinned"
        };
        let mut mood = format!("Mood: {} ({auto})", mode_name(status.mode));
        if !status.reason.is_empty() && settings.behaviour == Behaviour::Auto {
            let reason: String = status.reason.chars().take(48).collect();
            mood.push_str(&format!(" — “{reason}”"));
        }
        Self::set_title(self.mood, &mood);
        Self::set_title(
            self.health,
            &format!("Health: {:.0}%", status.health.clamp(0.0, 100.0)),
        );
        Self::set_title(self.director, &format!("AI: {}", status.director));
        for b in Behaviour::ALL {
            self.set_checked(Command::SetBehaviour(b), settings.behaviour == b);
        }
        for s in Size::ALL {
            self.set_checked(Command::SetSize(s), settings.size == s);
        }
        self.set_checked(Command::ToggleCursorPush, settings.cursor_push);
        self.set_checked(Command::ToggleWindowShake, settings.window_shake);
        self.set_checked(Command::ToggleWalkOnText, settings.walk_on_text);
        self.set_checked(Command::ToggleVisible, settings.visible);
        self.set_checked(Command::TogglePaused, settings.paused);
        Self::set_hidden(self.grant_ax, status.accessibility);
        Self::set_hidden(self.grant_screen, status.screen_recording);
        self.shown = Some((settings.clone(), status.clone()));
    }

    /// Where the icon sits on screen (AppKit coordinates, bottom-left origin) and
    /// whether the status item is visible (users can hide menu bar extras).
    pub fn placement(&self) -> (bool, [f64; 4]) {
        // SAFETY: main thread; read-only getters on live objects.
        unsafe {
            let visible: objc::runtime::BOOL = msg_send![self.item, isVisible];
            let button: *mut Object = msg_send![self.item, button];
            let win: *mut Object = msg_send![button, window];
            if win.is_null() {
                return (visible != NO, [0.0; 4]);
            }
            let f: NSRect = msg_send![win, frame];
            (visible != NO, [f.origin.x, f.origin.y, f.size.w, f.size.h])
        }
    }

    /// Pop the menu open as if clicked (debugging/screenshots). Blocks while the
    /// menu is open.
    pub fn open(&self) {
        // SAFETY: main thread; live status item button.
        unsafe {
            let button: *mut Object = msg_send![self.item, button];
            let _: () = msg_send![button, performClick: std::ptr::null_mut::<Object>()];
        }
    }

    /// Menu picks since the last call.
    pub fn drain_commands(&self) -> Vec<Command> {
        QUEUE
            .lock()
            .map(|mut q| q.drain(..).filter_map(Command::from_tag).collect())
            .unwrap_or_default()
    }
}

impl Default for MenuBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        // SAFETY: main thread; release what we retained and remove the icon.
        unsafe {
            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            let _: () = msg_send![bar, removeStatusItem: self.item];
            let _: () = msg_send![self.item, release];
            let _: () = msg_send![self.target, release];
        }
    }
}

pub fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Idle => "Idle",
        Mode::Observe => "Observing",
        Mode::Sabotage => "Sabotage",
        Mode::Combat => "Combat",
    }
}

/// Simulate a menu pick (tests): runs the real Objective-C action method.
pub fn simulate_pick(cmd: Command) {
    // SAFETY: creates a throwaway NSMenuItem and calls our registered action.
    unsafe {
        let target: *mut Object = msg_send![target_class(), new];
        let it: *mut Object = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![it, setTag: cmd.tag()];
        let _: () = msg_send![target, menuAction: it];
        let _: () = msg_send![it, release];
        let _: () = msg_send![target, release];
    }
}

/// Drain picks without a menu bar (tests).
pub fn drain_queue() -> Vec<Command> {
    QUEUE
        .lock()
        .map(|mut q| q.drain(..).filter_map(Command::from_tag).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objc_action_reaches_the_queue() {
        drain_queue();
        simulate_pick(Command::ToggleWindowShake);
        simulate_pick(Command::SetBehaviour(Behaviour::Combat));
        assert_eq!(
            drain_queue(),
            [
                Command::ToggleWindowShake,
                Command::SetBehaviour(Behaviour::Combat)
            ]
        );
        assert!(drain_queue().is_empty());
    }

    #[test]
    fn icon_fits_its_box_for_every_mood() {
        for m in [Mode::Idle, Mode::Observe, Mode::Sabotage, Mode::Combat] {
            let strokes = icon_strokes(&icon_pose(m), 18.0);
            assert_eq!(strokes.len(), 10);
            for (a, b, w, _) in &strokes {
                for p in [a, b] {
                    assert!(
                        (0.0..=18.0).contains(&p[0]) && (0.0..=18.0).contains(&p[1]),
                        "{m:?}: {p:?} outside"
                    );
                }
                assert!(*w >= 1.6 && *w < 3.0, "{m:?}: stroke {w}");
            }
        }
    }

    #[test]
    fn icons_differ_by_mood() {
        let a = icon_strokes(&icon_pose(Mode::Idle), 18.0);
        let b = icon_strokes(&icon_pose(Mode::Combat), 18.0);
        assert_ne!(a, b);
    }
}
