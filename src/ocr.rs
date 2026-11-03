//! Text traversal (PRD 3.2): capture the active window, find lines of text with
//! Apple's Vision framework, and turn the top of each line into a one-way platform
//! the figure can walk on.
//!
//! The PRD names `tesseract-sys`. Vision is used instead: it ships with macOS,
//! runs on the Neural Engine, needs no Homebrew install and is much faster on
//! Apple Silicon. Only bounding boxes are used, never the recognised text.
//!
//! Runs on its own thread (claude.md rule 1). Every capture is released when the
//! `CGImage` drops, and all Objective-C temporaries live in an autorelease pool
//! per request (claude.md rule 4).

#![allow(unexpected_cfgs)] // objc 0.2 macros

use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window as cgw;
use objc::runtime::{Object, BOOL, NO};
use objc::{class, msg_send, sel, sel_impl};

use crate::geom::Rect;
use crate::physics::{Platform, PlatformKind};

#[link(name = "Vision", kind = "framework")]
extern "C" {}

/// Ask for the text lines of window `id`, whose frame is `rect` (overlay points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Request {
    pub id: u32,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Result {
    pub id: u32,
    pub rect: Rect,
    /// Text line boxes, normalised to the window (Vision: origin bottom-left).
    pub lines: Vec<Rect>,
}

/// Whether this process may capture other apps' windows.
pub fn screen_capture_allowed() -> bool {
    core_graphics::access::ScreenCaptureAccess.preflight()
}

/// Ask macOS for Screen Recording permission (shows the system prompt once).
pub fn request_screen_capture() -> bool {
    core_graphics::access::ScreenCaptureAccess.request()
}

fn capture(id: u32) -> Option<CGImage> {
    let null = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0.0, 0.0),
    );
    cgw::create_image(
        null,
        cgw::kCGWindowListOptionIncludingWindow,
        id,
        cgw::kCGWindowImageBoundsIgnoreFraming | cgw::kCGWindowImageNominalResolution,
    )
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
// SAFETY: same layout and encoding as CGRect on 64-bit.
unsafe impl objc::Encode for NSRect {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

/// Run Vision text detection on `image`; returns normalised line boxes.
fn recognize(image: &CGImage) -> Vec<Rect> {
    use foreign_types::ForeignType;
    let mut out = Vec::new();
    objc::rc::autoreleasepool(|| {
        // SAFETY: documented Vision API; every object is either autoreleased or
        // released below, and nothing escapes the pool. `image` outlives the call.
        unsafe {
            let request: *mut Object = msg_send![class!(VNRecognizeTextRequest), alloc];
            let request: *mut Object = msg_send![request, init];
            if request.is_null() {
                return;
            }
            // VNRequestTextRecognitionLevelFast = 1: boxes are all we need.
            let _: () = msg_send![request, setRecognitionLevel: 1isize];
            let _: () = msg_send![request, setUsesLanguageCorrection: NO];
            let options: *mut Object = msg_send![class!(NSDictionary), dictionary];
            let handler: *mut Object = msg_send![class!(VNImageRequestHandler), alloc];
            let handler: *mut Object =
                msg_send![handler, initWithCGImage: image.as_ptr() options: options];
            let requests: *mut Object = msg_send![class!(NSArray), arrayWithObject: request];
            let mut error: *mut Object = std::ptr::null_mut();
            let ok: BOOL = msg_send![handler, performRequests: requests error: &mut error];
            if ok != NO {
                let results: *mut Object = msg_send![request, results];
                let n: usize = if results.is_null() {
                    0
                } else {
                    msg_send![results, count]
                };
                for i in 0..n {
                    let obs: *mut Object = msg_send![results, objectAtIndex: i];
                    let b: NSRect = msg_send![obs, boundingBox];
                    out.push(Rect {
                        x: b.x,
                        y: b.y,
                        width: b.w,
                        height: b.h,
                    });
                }
            }
            let _: () = msg_send![handler, release];
            let _: () = msg_send![request, release];
        }
    });
    out
}

/// Text-line platforms for a window. `lines` are Vision boxes (normalised,
/// bottom-left origin) inside `window` (overlay points).
pub fn text_platforms(id: u32, window: Rect, lines: &[Rect]) -> Vec<Platform> {
    lines
        .iter()
        .filter_map(|n| {
            let x0 = window.x + n.x * window.width;
            let w = n.width * window.width;
            let h = n.height * window.height;
            let top = window.y + (1.0 - n.y - n.height) * window.height;
            // Skip the title-bar strip, huge headings and scraps.
            let ok = (8.0..=60.0).contains(&h) && w >= 40.0 && top > window.y + 28.0;
            ok.then_some(Platform {
                x0: x0 as f32,
                x1: (x0 + w) as f32,
                y: top as f32,
                kind: PlatformKind::Text,
                owner: id,
            })
        })
        .collect()
}

/// Capture + recognise on a worker thread. Requests that pile up are coalesced to
/// the newest one.
pub fn spawn_worker(rx: Receiver<Request>, tx: Sender<Result>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("styk-ocr".into())
        .spawn(move || {
            while let Ok(mut req) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    req = newer;
                }
                let Some(image) = capture(req.id) else {
                    log::debug!("ocr: capture of window {} failed", req.id);
                    continue;
                };
                let lines = recognize(&image);
                drop(image); // release the pixels before anything else (claude.md rule 4)
                if tx
                    .send(Result {
                        id: req.id,
                        rect: req.rect,
                        lines,
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .expect("spawn OCR thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_vision_boxes_to_platform_tops() {
        let win = Rect {
            x: 100.0,
            y: 200.0,
            width: 1000.0,
            height: 500.0,
        };
        // A line 20% up from the bottom, 4% tall (20 pt), spanning 10%..60%.
        let lines = [Rect {
            x: 0.1,
            y: 0.2,
            width: 0.5,
            height: 0.04,
        }];
        let p = text_platforms(7, win, &lines);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].x0, p[0].x1), (200.0, 700.0));
        // top = 200 + (1 - 0.2 - 0.04) * 500 = 580
        assert!((p[0].y - 580.0).abs() < 1e-3);
        assert_eq!((p[0].kind, p[0].owner), (PlatformKind::Text, 7));
    }

    #[test]
    fn drops_title_bar_headings_and_scraps() {
        let win = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        };
        let lines = [
            Rect {
                x: 0.1,
                y: 0.975,
                width: 0.3,
                height: 0.02,
            }, // title bar
            Rect {
                x: 0.1,
                y: 0.5,
                width: 0.3,
                height: 0.1,
            }, // 100 pt heading
            Rect {
                x: 0.1,
                y: 0.4,
                width: 0.02,
                height: 0.02,
            }, // 20 pt wide scrap
            Rect {
                x: 0.1,
                y: 0.3,
                width: 0.3,
                height: 0.02,
            }, // keeper
        ];
        assert_eq!(text_platforms(1, win, &lines).len(), 1);
    }

    /// End-to-end Vision check on a rendered image with known text.
    #[test]
    fn vision_finds_text_lines() {
        if std::env::var_os("STYK_SKIP_GUI_TESTS").is_some() {
            return;
        }
        let img = render_text_image(&["The quick brown fox jumps", "over the lazy dog 12345"]);
        let lines = recognize(&img);
        assert!(lines.len() >= 2, "Vision found {} lines", lines.len());
        for l in &lines {
            assert!(
                l.x >= 0.0 && l.y >= 0.0 && l.right() <= 1.001 && l.bottom() <= 1.001,
                "{l:?}"
            );
        }
    }

    /// Render black text on white with AppKit (test helper).
    fn render_text_image(lines: &[&str]) -> CGImage {
        use objc::runtime::Object;
        // SAFETY: test-only AppKit drawing into an offscreen NSImage on one thread.
        unsafe {
            #[link(name = "AppKit", kind = "framework")]
            extern "C" {}
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct NSSize {
                w: f64,
                h: f64,
            }
            unsafe impl objc::Encode for NSSize {
                fn encode() -> objc::Encoding {
                    unsafe { objc::Encoding::from_str("{CGSize=dd}") }
                }
            }
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct NSPoint {
                x: f64,
                y: f64,
            }
            unsafe impl objc::Encode for NSPoint {
                fn encode() -> objc::Encoding {
                    unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
                }
            }
            let size = NSSize { w: 900.0, h: 300.0 };
            let image: *mut Object = msg_send![class!(NSImage), alloc];
            let image: *mut Object = msg_send![image, initWithSize: size];
            let _: () = msg_send![image, lockFocus];
            let white: *mut Object = msg_send![class!(NSColor), whiteColor];
            let _: () = msg_send![white, setFill];
            let r = NSRect {
                x: 0.0,
                y: 0.0,
                w: 900.0,
                h: 300.0,
            };
            let path: *mut Object = msg_send![class!(NSBezierPath), bezierPathWithRect: r];
            let _: () = msg_send![path, fill];
            let font: *mut Object = msg_send![class!(NSFont), systemFontOfSize: 40.0f64];
            let key: *mut Object =
                msg_send![class!(NSString), stringWithUTF8String: c"NSFont".as_ptr()];
            let attrs: *mut Object =
                msg_send![class!(NSDictionary), dictionaryWithObject: font forKey: key];
            for (i, l) in lines.iter().enumerate() {
                let c = std::ffi::CString::new(*l).unwrap();
                let s: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
                let at = NSPoint {
                    x: 30.0,
                    y: 200.0 - i as f64 * 90.0,
                };
                let _: () = msg_send![s, drawAtPoint: at withAttributes: attrs];
            }
            let _: () = msg_send![image, unlockFocus];
            let cg: *mut core_graphics::sys::CGImage = msg_send![image, CGImageForProposedRect: std::ptr::null_mut::<NSRect>() context: std::ptr::null_mut::<Object>() hints: std::ptr::null_mut::<Object>()];
            use foreign_types::ForeignType;
            let img = CGImage::from_ptr(cg);
            let copy = img.clone(); // retain: the NSImage owns `cg`
            std::mem::forget(img);
            copy
        }
    }
}
