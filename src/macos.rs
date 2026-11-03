//! Safe wrappers around the raw Objective-C / Accessibility calls.
//!
//! Every `unsafe` block documents its invariants (claude.md rule 2). AppKit calls
//! happen on the main thread only; blocking Accessibility IPC runs on its own
//! worker thread so a hung app can never stall rendering.

// objc 0.2's `msg_send!` expands to `cfg(feature = "cargo-clippy")` checks.
#![allow(unexpected_cfgs)]

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedWindowAttribute, kAXPositionAttribute, kAXTrustedCheckOptionPrompt,
    kAXValueTypeCGPoint, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
    AXUIElementCopyAttributeValue, AXUIElementCreateApplication, AXUIElementRef,
    AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueCreate, AXValueGetValue,
    AXValueRef,
};
use anyhow::{bail, Result};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_graphics::geometry::CGPoint;
use objc::runtime::{Object, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::geom::Rect;

type NSInteger = isize;
type NSUInteger = usize;

// NSWindowCollectionBehavior bits (AppKit/NSWindow.h).
pub const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
pub const STATIONARY: usize = 1 << 4;
pub const IGNORES_CYCLE: usize = 1 << 6;
pub const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

/// Present on every Space, don't slide during Space swipes, stay out of Cmd-`
/// cycling, and allow overlaying other apps' full-screen Spaces.
pub const OVERLAY_COLLECTION_BEHAVIOR: usize =
    CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY;

/// NSStatusWindowLevel (kCGStatusWindowLevel = 25): above normal and floating
/// windows and the menu bar, below system alerts and the screen saver.
pub const OVERLAY_WINDOW_LEVEL: isize = 25;

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
// SAFETY: layout matches CGRect ({CGPoint, CGSize}; CGFloat = f64 on 64-bit),
// and repr(C) of four f64 has the same layout as the two nested pairs.
unsafe impl Encode for NSRect {
    fn encode() -> Encoding {
        // SAFETY: a valid Objective-C type-encoding string for CGRect.
        unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

impl From<NSRect> for Rect {
    fn from(r: NSRect) -> Rect {
        Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    }
}

/// Overlay properties as AppKit reports them after configuration.
#[derive(Debug, Clone, Copy)]
pub struct OverlayState {
    pub window_number: NSInteger,
    pub ignores_mouse_events: bool,
    pub collection_behavior: NSUInteger,
    pub level: NSInteger,
}

/// Handle to the `NSWindow` behind the overlay's winit window.
///
/// Invariants: holds an `Arc` to the winit `Window`, which owns the `NSWindow`, so
/// the pointer stays valid as long as this value lives. It is `!Send` (raw
/// pointer), so it can only be used on the thread that created it — the main
/// thread, where winit creates windows.
pub struct OverlayWindow {
    ns_window: *mut Object,
    _owner: Arc<Window>,
    click_through: bool,
}

impl OverlayWindow {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let ns_window = {
            let handle = window.window_handle()?;
            let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
                bail!("expected an AppKit window handle");
            };
            let ns_view = appkit.ns_view.as_ptr() as *mut Object;
            // SAFETY: `ns_view` is the live content NSView winit created for this
            // window; `-[NSView window]` returns its (non-owning) NSWindow.
            let w: *mut Object = unsafe { msg_send![ns_view, window] };
            w
        };
        if ns_window.is_null() {
            bail!("NSView is not attached to an NSWindow");
        }
        Ok(Self {
            ns_window,
            _owner: window,
            click_through: false,
        })
    }

    /// Turn the window into a desktop overlay with the given AppKit `frame`
    /// (bottom-left-origin screen coordinates).
    pub fn configure(&mut self, frame: Rect, click_through: bool) {
        let w = self.ns_window;
        let frame = NSRect {
            x: frame.x,
            y: frame.y,
            width: frame.width,
            height: frame.height,
        };
        // SAFETY: see type invariants; every selector below is a documented
        // NSWindow/NSColor method with exactly these argument types.
        unsafe {
            // Fully transparent backing: only pixels we draw will be visible.
            let clear: *mut Object = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![w, setOpaque: NO];
            let _: () = msg_send![w, setBackgroundColor: clear];
            let _: () = msg_send![w, setHasShadow: NO];
            let _: () = msg_send![w, setCollectionBehavior: OVERLAY_COLLECTION_BEHAVIOR];
            let _: () = msg_send![w, setLevel: OVERLAY_WINDOW_LEVEL];
            let _: () = msg_send![w, setHidesOnDeactivate: NO];
            // A borderless window is not constrained below the menu bar, so this
            // covers the full display.
            let _: () = msg_send![w, setFrame: frame display: YES];
        }
        self.click_through = !click_through; // force the first call through
        self.set_click_through(click_through);
    }

    /// PRD 3.1: when `true`, every mouse event passes through to the windows
    /// underneath. Turned off only while the cursor is over the figure, so clicks
    /// on it are intercepted. Cheap no-op when unchanged.
    pub fn set_click_through(&mut self, enabled: bool) {
        if enabled == self.click_through {
            return;
        }
        let flag: BOOL = if enabled { YES } else { NO };
        // SAFETY: see type invariants.
        unsafe {
            let _: () = msg_send![self.ns_window, setIgnoresMouseEvents: flag];
        }
        self.click_through = enabled;
    }

    pub fn click_through(&self) -> bool {
        self.click_through
    }

    pub fn state(&self) -> OverlayState {
        let w = self.ns_window;
        // SAFETY: see type invariants; read-only getters.
        unsafe {
            let ignores: BOOL = msg_send![w, ignoresMouseEvents];
            OverlayState {
                window_number: msg_send![w, windowNumber],
                ignores_mouse_events: ignores != NO,
                collection_behavior: msg_send![w, collectionBehavior],
                level: msg_send![w, level],
            }
        }
    }
}

/// `visibleFrame` of the primary (menu bar) screen in AppKit coordinates — the
/// area not covered by the menu bar or the Dock. Main thread only.
pub fn primary_visible_frame() -> Option<Rect> {
    // SAFETY: documented NSScreen API, main thread (caller contract). `screens`
    // returns an autoreleased NSArray; index 0 is the menu-bar screen.
    unsafe {
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }
        let count: NSUInteger = msg_send![screens, count];
        if count == 0 {
            return None;
        }
        let screen: *mut Object = msg_send![screens, objectAtIndex: 0usize];
        let frame: NSRect = msg_send![screen, visibleFrame];
        Some(frame.into())
    }
}

/// Whether our (accessory) app is currently the active app. Main thread only.
pub fn app_is_active() -> bool {
    // SAFETY: NSApp exists once the event loop runs; main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let active: BOOL = msg_send![app, isActive];
        active != NO
    }
}

/// Hand activation back to app `pid` after a click on the figure activated us.
/// Uses macOS 14's cooperative activation when available. Main thread only.
pub fn restore_focus(pid: i64) {
    // SAFETY: documented AppKit APIs, guarded by `respondsToSelector:` for the
    // macOS 14+ ones; main thread; all returned objects are autoreleased.
    unsafe {
        let me: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let target: *mut Object = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid as i32];
        if !target.is_null() {
            let can_yield: BOOL =
                msg_send![me, respondsToSelector: sel!(yieldActivationToApplication:)];
            let can_from: BOOL =
                msg_send![target, respondsToSelector: sel!(activateFromApplication:options:)];
            if can_yield != NO && can_from != NO {
                let current: *mut Object =
                    msg_send![class!(NSRunningApplication), currentApplication];
                let _: () = msg_send![me, yieldActivationToApplication: target];
                let _: BOOL = msg_send![target, activateFromApplication: current options: 0usize];
            } else {
                // NSApplicationActivateAllWindows (1).
                let _: BOOL = msg_send![target, activateWithOptions: 1usize];
            }
        }
        // Belt and braces: give up activation; macOS re-activates the previous app.
        let _: () = msg_send![me, deactivate];
    }
}

/// Open a URL (e.g. a System Settings privacy pane). Main thread only.
pub fn open_url(url: &str) -> bool {
    let c = std::ffi::CString::new(url).unwrap_or_default();
    // SAFETY: documented NSWorkspace/NSURL APIs on the main thread; autoreleased.
    unsafe {
        let s: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
        let u: *mut Object = msg_send![class!(NSURL), URLWithString: s];
        if u.is_null() {
            return false;
        }
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let ok: BOOL = msg_send![ws, openURL: u];
        ok != NO
    }
}

// --- Accessibility ---------------------------------------------------------------

/// Whether this process may control other apps' windows. With `prompt`, macOS
/// shows its "grant Accessibility access" dialog if not yet trusted.
pub fn accessibility_trusted(prompt: bool) -> bool {
    if !prompt {
        // SAFETY: argument-less C function.
        return unsafe { AXIsProcessTrusted() };
    }
    // SAFETY: kAXTrustedCheckOptionPrompt is a static CFString; the dictionary
    // lives for the duration of the call.
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let opts = CFDictionary::from_CFType_pairs(&[(
            key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef())
    }
}

/// Window manipulations performed off the main thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OsAction {
    /// Jolt the focused window of `pid` sideways by `dx` points and back.
    ShakeFocusedWindow { pid: i64, dx: f64 },
}

/// Owned AX element, released on drop.
struct AxElement(AXUIElementRef);

impl Drop for AxElement {
    fn drop(&mut self) {
        // SAFETY: we own one reference from a Create/Copy call.
        unsafe { core_foundation::base::CFRelease(self.0 as CFTypeRef) };
    }
}

fn copy_attr(el: &AxElement, attr: &str) -> Option<CFType> {
    let name = CFString::new(attr);
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: `el` is a valid AXUIElement; on success we own `value` (Copy rule).
    let err =
        unsafe { AXUIElementCopyAttributeValue(el.0, name.as_concrete_TypeRef(), &mut value) };
    // SAFETY: non-null `value` follows the Create rule.
    (err == kAXErrorSuccess && !value.is_null())
        .then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

fn window_position(win: &AxElement) -> Option<CGPoint> {
    let v = copy_attr(win, kAXPositionAttribute)?;
    let mut p = CGPoint::new(0.0, 0.0);
    // SAFETY: AXPosition is an AXValue of type CGPoint; `p` is a valid out-param.
    let ok = unsafe {
        AXValueGetValue(
            v.as_CFTypeRef() as AXValueRef,
            kAXValueTypeCGPoint,
            &mut p as *mut CGPoint as *mut c_void,
        )
    };
    ok.then_some(p)
}

fn set_window_position(win: &AxElement, p: CGPoint) -> bool {
    // SAFETY: creating an AXValue from a stack CGPoint (copied by the callee).
    let value =
        unsafe { AXValueCreate(kAXValueTypeCGPoint, &p as *const CGPoint as *const c_void) };
    if value.is_null() {
        return false;
    }
    // SAFETY: we own `value` (Create rule); released by the wrapper.
    let value = unsafe { CFType::wrap_under_create_rule(value as CFTypeRef) };
    let name = CFString::new(kAXPositionAttribute);
    // SAFETY: valid element, attribute name and value.
    unsafe {
        AXUIElementSetAttributeValue(win.0, name.as_concrete_TypeRef(), value.as_CFTypeRef())
            == kAXErrorSuccess
    }
}

fn shake(pid: i64, dx: f64) -> bool {
    // SAFETY: creates an owned element for any pid (no IPC yet).
    let app = AxElement(unsafe { AXUIElementCreateApplication(pid as i32) });
    // Never let an unresponsive app block us for long.
    // SAFETY: valid element.
    unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
    let Some(win) = copy_attr(&app, kAXFocusedWindowAttribute) else {
        return false;
    };
    // Keep the window element alive in our own wrapper.
    // SAFETY: `win` is an AXUIElement; retain it for AxElement's release.
    let win =
        AxElement(unsafe { core_foundation::base::CFRetain(win.as_CFTypeRef()) } as AXUIElementRef);
    let Some(p) = window_position(&win) else {
        return false;
    };
    let moved = set_window_position(&win, CGPoint::new(p.x + dx, p.y + dx.abs() * 0.3));
    std::thread::sleep(std::time::Duration::from_millis(45));
    set_window_position(&win, p) && moved
}

/// Worker thread for [`OsAction`]s (Accessibility IPC can block).
pub fn spawn_os_actions(rx: Receiver<OsAction>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("styk-os".into())
        .spawn(move || {
            while let Ok(action) = rx.recv() {
                match action {
                    OsAction::ShakeFocusedWindow { pid, dx } => {
                        if !shake(pid, dx) {
                            log::debug!("shake of pid {pid} failed (Accessibility not granted?)");
                        }
                    }
                }
            }
        })
        .expect("spawn OS action thread")
}

/// Convenience: a sender + worker pair.
pub fn os_actions() -> Sender<OsAction> {
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_os_actions(rx);
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_behavior_matches_appkit_bits() {
        // canJoinAllSpaces(1) | stationary(16) | ignoresCycle(64) | fullScreenAuxiliary(256)
        assert_eq!(OVERLAY_COLLECTION_BEHAVIOR, 0x151);
        // moveToActiveSpace (1 << 1) conflicts with canJoinAllSpaces; must be off.
        assert_eq!(OVERLAY_COLLECTION_BEHAVIOR & (1 << 1), 0);
        // transient (1 << 3) would hide the overlay in Mission Control; must be off.
        assert_eq!(OVERLAY_COLLECTION_BEHAVIOR & (1 << 3), 0);
    }

    #[test]
    fn window_level_is_status_level() {
        // NSStatusWindowLevel = kCGStatusWindowLevel = 25 (above menu bar = 24).
        assert_eq!(OVERLAY_WINDOW_LEVEL, 25);
    }

    #[test]
    fn accessibility_query_does_not_crash() {
        let _ = accessibility_trusted(false);
    }
}
