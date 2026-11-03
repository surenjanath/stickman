//! Safe wrappers around the raw Objective-C calls on the overlay's `NSWindow`.

// objc 0.2's `msg_send!` expands to `cfg(feature = "cargo-clippy")` checks.
#![allow(unexpected_cfgs)]

use anyhow::{bail, Result};
use objc::runtime::{Object, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::{Rect, OVERLAY_COLLECTION_BEHAVIOR, OVERLAY_WINDOW_LEVEL};

type NSInteger = isize;
type NSUInteger = usize;

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

/// Borrowed handle to the `NSWindow` behind a winit window.
///
/// Invariants: the pointer is a live `NSWindow` owned by the borrowed winit
/// `Window` (the lifetime ties them together), and it is only messaged from the
/// main thread — winit hands out `&Window` there and `Window` methods that reach
/// AppKit are main-thread-only.
pub struct OverlayWindow<'a> {
    ns_window: *mut Object,
    _owner: &'a Window,
}

impl<'a> OverlayWindow<'a> {
    pub fn from_winit(window: &'a Window) -> Result<Self> {
        let handle = window.window_handle()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            bail!("expected an AppKit window handle");
        };
        let ns_view = appkit.ns_view.as_ptr() as *mut Object;
        // SAFETY: `ns_view` is the live content NSView winit created for this
        // window; `-[NSView window]` returns its (non-owning) NSWindow.
        let ns_window: *mut Object = unsafe { msg_send![ns_view, window] };
        if ns_window.is_null() {
            bail!("NSView is not attached to an NSWindow");
        }
        Ok(Self {
            ns_window,
            _owner: window,
        })
    }

    /// Turn the window into a desktop overlay with the given AppKit `frame`
    /// (bottom-left-origin screen coordinates).
    pub fn configure(&self, frame: Rect, click_through: bool) {
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

            // canJoinAllSpaces | stationary | ignoresCycle | fullScreenAuxiliary.
            let _: () = msg_send![w, setCollectionBehavior: OVERLAY_COLLECTION_BEHAVIOR];
            let _: () = msg_send![w, setLevel: OVERLAY_WINDOW_LEVEL];
            let _: () = msg_send![w, setHidesOnDeactivate: NO];

            // A borderless window is not constrained below the menu bar, so this
            // covers the full display.
            let _: () = msg_send![w, setFrame: frame display: YES];
        }
        self.set_click_through(click_through);
    }

    /// PRD 3.1: when `true`, every mouse event passes through to the windows
    /// underneath. Later phases flip this off while the cursor is over the
    /// stickman's collider so clicks can be intercepted.
    pub fn set_click_through(&self, enabled: bool) {
        let flag: BOOL = if enabled { YES } else { NO };
        // SAFETY: see type invariants.
        unsafe {
            let _: () = msg_send![self.ns_window, setIgnoresMouseEvents: flag];
        }
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
/// area not covered by the menu bar or the Dock. Must be called on the main
/// thread after the event loop has started.
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
