//! Active-window tracking (PRD 3.2). A background thread polls the window server
//! for on-screen application windows (front to back) and sends a snapshot to the
//! main thread whenever something moved, opened or closed. Their top edges become
//! one-way platforms.
//!
//! `CGWindowListCopyWindowInfo` reports window bounds and owners without any
//! permission, so this works even before Accessibility is granted.

use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::window as cgw;

use crate::geom::Rect;

/// One window as the window server reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub id: u32,
    pub pid: i64,
    pub owner: String,
    pub layer: i64,
    /// Global CoreGraphics coordinates (points, top-left origin).
    pub bounds: Rect,
    pub on_screen: bool,
    pub alpha: f64,
}

fn key(k: CFStringRef) -> CFString {
    // SAFETY: the kCGWindow* keys are valid static CFStrings.
    unsafe { CFString::wrap_under_get_rule(k) }
}

/// Raw window list (front to back for on-screen queries).
pub fn list_windows(
    option: cgw::CGWindowListOption,
    relative_to: cgw::CGWindowID,
) -> Vec<WindowInfo> {
    let Some(list) = cgw::copy_window_info(option, relative_to) else {
        return vec![];
    };
    let num = |d: &CFDictionary<CFString, CFType>, k| {
        d.find(key(k))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_f64())
    };
    list.iter()
        .map(|item| {
            // SAFETY: CGWindowListCopyWindowInfo returns an array of CFDictionaries.
            let d: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(*item as CFDictionaryRef) };
            // SAFETY (all `unsafe` below): reading immutable static CFString keys.
            let b = d
                .find(key(unsafe { cgw::kCGWindowBounds }))
                .and_then(|v| v.downcast::<CFDictionary>())
                .and_then(|b| CGRect::from_dict_representation(&b))
                .unwrap_or_else(|| CGRect::new(&CGPoint::new(0., 0.), &CGSize::new(0., 0.)));
            WindowInfo {
                id: num(&d, unsafe { cgw::kCGWindowNumber }).unwrap_or(0.0) as u32,
                pid: num(&d, unsafe { cgw::kCGWindowOwnerPID }).unwrap_or(-1.0) as i64,
                owner: d
                    .find(key(unsafe { cgw::kCGWindowOwnerName }))
                    .and_then(|v| v.downcast::<CFString>())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                layer: num(&d, unsafe { cgw::kCGWindowLayer }).unwrap_or(-1.0) as i64,
                bounds: Rect {
                    x: b.origin.x,
                    y: b.origin.y,
                    width: b.size.width,
                    height: b.size.height,
                },
                on_screen: d
                    .find(key(unsafe { cgw::kCGWindowIsOnscreen }))
                    .and_then(|v| v.downcast::<CFBoolean>())
                    .map(bool::from)
                    .unwrap_or(false),
                alpha: num(&d, unsafe { cgw::kCGWindowAlpha }).unwrap_or(0.0),
            }
        })
        .collect()
}

/// A normal application window.
#[derive(Debug, Clone, PartialEq)]
pub struct AppWindow {
    pub id: u32,
    pub pid: i64,
    pub owner: String,
    /// Global CoreGraphics points.
    pub rect: Rect,
}

/// Application windows on screen, frontmost first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub windows: Vec<AppWindow>,
}

impl Snapshot {
    /// The frontmost application window (≈ the active app).
    pub fn active(&self) -> Option<&AppWindow> {
        self.windows.first()
    }
}

/// Keep ordinary app windows on any of the `screens`: layer 0, visible, a
/// sensible size, not ours.
pub fn filter_windows(raw: &[WindowInfo], own_pid: i64, screens: &[Rect]) -> Vec<AppWindow> {
    raw.iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.01 && w.pid != own_pid)
        .filter(|w| w.bounds.width >= 120.0 && w.bounds.height >= 60.0)
        .filter(|w| {
            screens.iter().any(|d| {
                w.bounds.x < d.right()
                    && w.bounds.right() > d.x
                    && w.bounds.y < d.bottom()
                    && w.bounds.bottom() > d.y
            })
        })
        .map(|w| AppWindow {
            id: w.id,
            pid: w.pid,
            owner: w.owner.clone(),
            rect: w.bounds,
        })
        .collect()
}

pub fn snapshot(own_pid: i64, screens: &[Rect]) -> Snapshot {
    let raw = list_windows(
        cgw::kCGWindowListOptionOnScreenOnly | cgw::kCGWindowListExcludeDesktopElements,
        cgw::kCGNullWindowID,
    );
    Snapshot {
        windows: filter_windows(&raw, own_pid, screens),
    }
}

/// Poll every `interval` on a background thread; send only on change. The
/// screen list is shared so it can change when displays are plugged in.
pub fn spawn_poller(
    own_pid: i64,
    screens: std::sync::Arc<std::sync::Mutex<Vec<Rect>>>,
    interval: Duration,
    tx: Sender<Snapshot>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("stickman-windows".into())
        .spawn(move || {
            let mut last = None;
            loop {
                let frames = screens.lock().map(|s| s.clone()).unwrap_or_default();
                let snap = snapshot(own_pid, &frames);
                if last.as_ref() != Some(&snap) {
                    if tx.send(snap.clone()).is_err() {
                        return; // app shut down
                    }
                    last = Some(snap);
                }
                std::thread::sleep(interval);
            }
        })
        .expect("spawn window poller thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(id: u32, pid: i64, layer: i64, x: f64, y: f64, w: f64, h: f64) -> WindowInfo {
        WindowInfo {
            id,
            pid,
            owner: format!("app{pid}"),
            layer,
            bounds: Rect {
                x,
                y,
                width: w,
                height: h,
            },
            on_screen: true,
            alpha: 1.0,
        }
    }

    #[test]
    fn keeps_only_normal_visible_foreign_windows() {
        let display = Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1169.0,
        };
        let list = [
            raw(1, 10, 0, 100.0, 100.0, 800.0, 600.0), // normal
            raw(2, 99, 0, 0.0, 0.0, 1800.0, 1169.0),   // ours
            raw(3, 11, 25, 0.0, 0.0, 300.0, 24.0),     // menu-bar item
            raw(4, 12, 0, 10.0, 10.0, 40.0, 40.0),     // tiny
            raw(5, 13, 0, 3000.0, 0.0, 800.0, 600.0),  // other display
        ];
        let w = filter_windows(&list, 99, &[display]);
        assert_eq!(w.iter().map(|w| w.id).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn keeps_windows_on_every_screen_in_global_coordinates() {
        let screens = [
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1800.0,
                height: 1169.0,
            },
            Rect {
                x: -1920.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
        ];
        let list = [
            raw(1, 10, 0, -1700.0, 50.0, 400.0, 300.0),
            raw(2, 10, 0, 5000.0, 50.0, 400.0, 300.0),
        ];
        let w = filter_windows(&list, 99, &screens);
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].rect.x, w[0].rect.y), (-1700.0, 50.0));
    }

    #[test]
    fn live_snapshot_excludes_our_process() {
        if std::env::var_os("STICKMAN_SKIP_GUI_TESTS").is_some() {
            return;
        }
        let display = Rect {
            x: -1e5,
            y: -1e5,
            width: 2e5,
            height: 2e5,
        };
        let snap = snapshot(i64::from(std::process::id()), &[display]);
        assert!(snap
            .windows
            .iter()
            .all(|w| w.pid != i64::from(std::process::id())));
    }
}
