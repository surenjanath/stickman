//! Text traversal (PRD 3.2): capture the active window, find lines of text with
//! Apple's Vision framework, and turn the top of each line into a one-way platform
//! the figure can walk on.
//!
//! The PRD names `tesseract-sys`. Vision is used instead: it ships with macOS,
//! runs on the Neural Engine, needs no Homebrew install and is much faster on
//! Apple Silicon. Only bounding boxes leave this module: the recognised string is
//! used in memory to find word boundaries and is never logged, stored or sent.
//!
//! The same worker also "grabs" words: it captures the pixels of one word from
//! its window and keys out the background, so the figure can rip the word off
//! the page and throw it.
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

/// A recognised word: its box (normalised to the window, Vision's bottom-left
/// origin) and how many characters it has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Word {
    pub rect: Rect,
    pub chars: u16,
}

/// One recognised letter (upper-cased) and its box, normalised like [`Word`].
/// Used only in memory, to tear letters off for notes; never logged or kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letter {
    pub ch: char,
    pub rect: Rect,
}

/// Letter boxes kept per window: at most this many of each letter, and in all.
const LETTERS_PER_CHAR: usize = 4;
const MAX_LETTERS: usize = 260;

#[derive(Debug, Clone, PartialEq)]
pub struct Result {
    pub id: u32,
    pub rect: Rect,
    /// Text line boxes, normalised to the window (Vision: origin bottom-left).
    pub lines: Vec<Rect>,
    pub words: Vec<Word>,
    pub letters: Vec<Letter>,
}

/// Capture one word for the figure to carry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrabRequest {
    pub id: u64,
    pub window: u32,
    /// The overlay's window number: capture what's on screen *below* it, so a
    /// translucent window's background matches exactly what the user sees.
    /// 0 = capture `window` on its own.
    pub below: u32,
    /// Region in global CoreGraphics points.
    pub global: Rect,
    /// How to cut it out.
    pub cut: Cut,
}

/// How a grabbed region is cut out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cut {
    /// A word: the page colour is keyed out so only the glyphs fly.
    #[default]
    Keyed,
    /// An app icon: a rounded square, as is.
    Icon,
    /// A rectangle as is (a scrollbar, say).
    Plain,
}

/// Round the corners of an RGBA image (premultiplied): an app-icon shape.
pub fn round_corners(rgba: &mut [u8], w: u32, h: u32, radius: f32) {
    let (wf, hf) = (w as f32, h as f32);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = px.clamp(radius, wf - radius);
            let cy = py.clamp(radius, hf - radius);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let a = (radius + 0.5 - d).clamp(0.0, 1.0);
            if a < 1.0 {
                let i = ((y * w + x) * 4) as usize;
                for c in &mut rgba[i..i + 4] {
                    *c = (f32::from(*c) * a) as u8;
                }
            }
        }
    }
}

/// Premultiplied RGBA pixels of a grabbed word with its background keyed out.
#[derive(Debug, Clone, PartialEq)]
pub struct Cutout {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The page colour behind the word (used to paint the "hole").
    pub background: [u8; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrabResult {
    pub id: u64,
    pub cutout: Option<Cutout>,
}

/// Work for the OCR thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Job {
    Read(Request),
    Grab(GrabRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Text(Result),
    Grabbed(GrabResult),
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
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRange {
    location: usize,
    length: usize,
}
// SAFETY: matches NSRange's layout and encoding on 64-bit.
unsafe impl objc::Encode for NSRange {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{_NSRange=QQ}") }
    }
}

/// Words in `s` as (UTF-16 offset, UTF-16 length, char count). NSString ranges are
/// in UTF-16 code units. Words need at least one letter or digit.
pub fn word_ranges(s: &str) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut u16_pos = 0;
    let mut start: Option<(usize, usize, bool)> = None; // (u16 start, chars, has alnum)
    for ch in s.chars().chain(std::iter::once(' ')) {
        if ch.is_whitespace() {
            if let Some((st, n, alnum)) = start.take() {
                if alnum {
                    out.push((st, u16_pos - st, n));
                }
            }
        } else {
            let e = start.get_or_insert((u16_pos, 0, false));
            e.1 += 1;
            e.2 |= ch.is_alphanumeric();
        }
        u16_pos += ch.len_utf16();
    }
    out
}

/// Text lines and words in `image` (normalised boxes).
fn recognize(image: &CGImage) -> (Vec<Rect>, Vec<Word>, Vec<Letter>) {
    use foreign_types::ForeignType;
    let mut out = Vec::new();
    let mut words = Vec::new();
    let mut letters: Vec<Letter> = Vec::new();
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
                    if words.len() >= 300 {
                        continue;
                    }
                    // Word boxes from the top candidate; the text itself stays here.
                    let cands: *mut Object = msg_send![obs, topCandidates: 1usize];
                    let count: usize = if cands.is_null() {
                        0
                    } else {
                        msg_send![cands, count]
                    };
                    if count == 0 {
                        continue;
                    }
                    let cand: *mut Object = msg_send![cands, objectAtIndex: 0usize];
                    let ns: *mut Object = msg_send![cand, string];
                    let utf8: *const std::os::raw::c_char = msg_send![ns, UTF8String];
                    if utf8.is_null() {
                        continue;
                    }
                    let text = std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .into_owned();
                    for (loc, len, chars) in word_ranges(&text) {
                        let mut err: *mut Object = std::ptr::null_mut();
                        let r: *mut Object = msg_send![cand, boundingBoxForRange: NSRange { location: loc, length: len } error: &mut err];
                        if r.is_null() {
                            continue;
                        }
                        let wb: NSRect = msg_send![r, boundingBox];
                        // Single letters (for ransom notes): the word's box split
                        // evenly by its characters. (Vision's per-character boxes
                        // often land on a neighbour.)
                        let utf16: Vec<u16> = text.encode_utf16().collect();
                        let word: String =
                            String::from_utf16_lossy(utf16.get(loc..loc + len).unwrap_or(&[]));
                        let n = word.chars().count().max(1);
                        for (k, ch) in word.chars().enumerate() {
                            let up = ch.to_ascii_uppercase();
                            let enough = letters.len() >= MAX_LETTERS
                                || letters.iter().filter(|l| l.ch == up).count()
                                    >= LETTERS_PER_CHAR;
                            if ch.is_ascii_alphanumeric() && !enough {
                                let cw = wb.w / n as f64;
                                letters.push(Letter {
                                    ch: up,
                                    rect: Rect {
                                        x: wb.x + cw * k as f64,
                                        y: wb.y,
                                        width: cw,
                                        height: wb.h,
                                    },
                                });
                            }
                        }
                        words.push(Word {
                            rect: Rect {
                                x: wb.x,
                                y: wb.y,
                                width: wb.w,
                                height: wb.h,
                            },
                            chars: chars.min(u16::MAX as usize) as u16,
                        });
                    }
                }
            }
            let _: () = msg_send![handler, release];
            let _: () = msg_send![request, release];
        }
    });
    (out, words, letters)
}

/// Letters of a window in global points (letters too small, too big or in
/// the title bar left out).
pub fn letters_in(window: Rect, letters: &[Letter]) -> Vec<(char, Rect)> {
    letters
        .iter()
        .filter_map(|l| {
            let r = Rect {
                x: window.x + l.rect.x * window.width,
                y: window.y + (1.0 - l.rect.y - l.rect.height) * window.height,
                width: l.rect.width * window.width,
                height: l.rect.height * window.height,
            };
            let ok = (7.0..=60.0).contains(&r.height)
                && (3.0..=60.0).contains(&r.width)
                && r.y > window.y + 28.0;
            ok.then_some((l.ch, r))
        })
        .collect()
}

/// Words of a window in overlay points: (box, characters). `words` are Vision
/// boxes normalised to `window`.
pub fn words_in(window: Rect, words: &[Word]) -> Vec<(Rect, u16)> {
    words
        .iter()
        .filter_map(|w| {
            let r = Rect {
                x: window.x + w.rect.x * window.width,
                y: window.y + (1.0 - w.rect.y - w.rect.height) * window.height,
                width: w.rect.width * window.width,
                height: w.rect.height * window.height,
            };
            // Skip the title-bar strip, giant headings and slivers.
            let ok = (6.0..=60.0).contains(&r.height) && r.width >= 4.0 && r.y > window.y + 28.0;
            ok.then_some((r, w.chars))
        })
        .collect()
}

/// What's on screen in `global` just below the overlay window `below` (or,
/// with `below == 0`, window `window` alone).
fn capture_region(window: u32, below: u32, global: Rect) -> Option<CGImage> {
    let rect = CGRect::new(
        &CGPoint::new(global.x, global.y),
        &CGSize::new(global.width, global.height),
    );
    let (option, relative) = if below != 0 {
        (cgw::kCGWindowListOptionOnScreenBelowWindow, below)
    } else {
        (cgw::kCGWindowListOptionIncludingWindow, window)
    };
    cgw::create_image(
        rect,
        option,
        relative,
        cgw::kCGWindowImageBoundsIgnoreFraming | cgw::kCGWindowImageBestResolution,
    )
}

extern "C" {
    fn CGColorSpaceGetNumberOfComponents(space: *const std::ffi::c_void) -> usize;
}

/// Decode a CGImage into tightly packed premultiplied RGBA8, top row first.
fn to_rgba(image: &CGImage) -> Option<(Vec<u8>, u32, u32)> {
    use core_graphics::base::kCGImageAlphaPremultipliedLast;
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::CGContext;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    // Decode in the capture's own colour space: no conversion, so the pixels
    // (and the hole's page colour) match what the overlay later puts on screen.
    let own = image.color_space();
    // SAFETY: `own` is a live CGColorSpace for the duration of the call.
    let rgb = unsafe {
        use foreign_types::ForeignType;
        CGColorSpaceGetNumberOfComponents(own.as_ptr() as *const _) == 3
    };
    let space = if rgb {
        own
    } else {
        CGColorSpace::create_device_rgb()
    };
    let mut ctx = CGContext::create_bitmap_context(
        None,
        w,
        h,
        8,
        w * 4,
        &space,
        kCGImageAlphaPremultipliedLast,
    );
    ctx.draw_image(
        CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(w as f64, h as f64)),
        image,
    );
    Some((ctx.data().to_vec(), w as u32, h as u32))
}

/// Key out a word's background: estimate the page colour from the border, refuse
/// busy backgrounds (photos, gradients), and make page-coloured pixels
/// transparent so only the glyphs fly. Input/output are premultiplied RGBA8.
pub fn key_out(rgba: &[u8], w: u32, h: u32) -> Option<Cutout> {
    let (w, h) = (w as usize, h as usize);
    if w < 2 || h < 2 || rgba.len() < w * h * 4 {
        return None;
    }
    let px = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    // Page colour from the top and bottom rows: the capture is padded into the
    // line spacing there, while the left/right edges can touch neighbouring
    // glyphs (Vision's word boxes are tight).
    let mut border: Vec<[u8; 3]> = Vec::new();
    for x in 0..w {
        border.push(px(x, 0));
        border.push(px(x, h - 1));
    }
    let median = |c: usize| {
        let mut v: Vec<u8> = border.iter().map(|p| p[c]).collect();
        v.sort_unstable();
        v[v.len() / 2]
    };
    let bg = [median(0), median(1), median(2)];
    let dist = |p: [u8; 3]| {
        (0..3)
            .map(|c| (i32::from(p[c]) - i32::from(bg[c])).abs())
            .max()
            .unwrap_or(0)
    };
    // Most of that border must be page colour, or the patch would show.
    let plain = border.iter().filter(|p| dist(**p) <= 14).count();
    if plain * 10 < border.len() * 7 {
        log::debug!("grab: background too busy ({plain}/{} plain)", border.len());
        return None;
    }
    let mut out = vec![0u8; w * h * 4];
    let mut ink = 0usize;
    for y in 0..h {
        for x in 0..w {
            let p = px(x, y);
            let d = dist(p) as f32;
            let a = ((d - 16.0) / 50.0).clamp(0.0, 1.0);
            let a = a * a * (3.0 - 2.0 * a);
            if a > 0.5 {
                ink += 1;
            }
            let i = (y * w + x) * 4;
            for c in 0..3 {
                out[i + c] = (f32::from(p[c]) * a).round() as u8;
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    // Nothing to grab if there's no ink.
    if ink < 4 {
        return None;
    }
    Some(Cutout {
        rgba: out,
        width: w as u32,
        height: h as u32,
        background: bg,
    })
}

/// Extra margin captured above/below a word (into the line spacing), points.
pub const GRAB_PAD_Y: f64 = 3.0;

fn grab(req: GrabRequest) -> GrabResult {
    if req.cut != Cut::Keyed {
        // Icons and scrollbars: the pixels as they are.
        let captured = capture_region(req.window, req.below, req.global);
        let cutout = captured.and_then(|img| {
            let decoded = to_rgba(&img);
            drop(img); // release the capture right away (claude.md rule 4)
            decoded.map(|(mut rgba, w, h)| {
                if req.cut == Cut::Icon {
                    let r = 0.23 * w.min(h) as f32;
                    round_corners(&mut rgba, w, h, r);
                }
                Cutout {
                    rgba,
                    width: w,
                    height: h,
                    background: [0; 3],
                }
            })
        });
        return GrabResult { id: req.id, cutout };
    }
    let padded = Rect {
        y: req.global.y - GRAB_PAD_Y,
        height: req.global.height + 2.0 * GRAB_PAD_Y,
        ..req.global
    };
    let captured = capture_region(req.window, req.below, padded);
    if captured.is_none() {
        log::debug!("grab: capture of window {} failed", req.window);
    }
    let cutout = captured.and_then(|img| {
        let decoded = to_rgba(&img);
        drop(img); // release the capture right away (claude.md rule 4)
        decoded.and_then(|(rgba, w, h)| key_out(&rgba, w, h))
    });
    GrabResult { id: req.id, cutout }
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

/// Capture and read window `id` right now (diagnostics): lines, words.
pub fn read_window_now(id: u32) -> Option<(Vec<Rect>, Vec<Word>)> {
    let image = capture(id)?;
    let (lines, words, _) = recognize(&image);
    Some((lines, words))
}

/// Capture + recognise on a worker thread. Read requests that pile up are
/// coalesced to the newest one; grabs are always served, first.
pub fn spawn_worker(rx: Receiver<Job>, tx: Sender<Output>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("stickman-ocr".into())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut read = None;
                let mut grabs = Vec::new();
                for job in std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                    match job {
                        Job::Read(r) => read = Some(r),
                        Job::Grab(g) => grabs.push(g),
                    }
                }
                for g in grabs {
                    if tx.send(Output::Grabbed(grab(g))).is_err() {
                        return;
                    }
                }
                let Some(req) = read else { continue };
                let Some(image) = capture(req.id) else {
                    log::debug!("ocr: capture of window {} failed", req.id);
                    continue;
                };
                let (lines, words, letters) = recognize(&image);
                drop(image); // release the pixels before anything else (claude.md rule 4)
                if tx
                    .send(Output::Text(Result {
                        id: req.id,
                        rect: req.rect,
                        lines,
                        words,
                        letters,
                    }))
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
    fn icon_corners_are_rounded_and_the_middle_kept() {
        let (w, h) = (40u32, 40u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        round_corners(&mut rgba, w, h, 9.0);
        assert_eq!(rgba[3], 0, "corner pixel cleared");
        let mid = ((20 * w + 20) * 4 + 3) as usize;
        assert_eq!(rgba[mid], 255);
        let edge = ((20 * w) * 4 + 3) as usize;
        assert_eq!(rgba[edge], 255, "straight edges untouched");
    }

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
    fn word_ranges_use_utf16_offsets_and_skip_punctuation() {
        assert_eq!(
            word_ranges("Hello, world  42"),
            vec![(0, 6, 6), (7, 5, 5), (14, 2, 2)]
        );
        assert_eq!(word_ranges(" — ok"), vec![(3, 2, 2)]);
        // "é" is 1 UTF-16 unit, "😀" is 2.
        assert_eq!(word_ranges("😀 café"), vec![(3, 4, 4)]);
        assert!(word_ranges("   ").is_empty());
    }

    #[test]
    fn words_convert_to_overlay_space() {
        let win = Rect {
            x: 100.0,
            y: 200.0,
            width: 1000.0,
            height: 500.0,
        };
        let w = [Word {
            rect: Rect {
                x: 0.1,
                y: 0.2,
                width: 0.05,
                height: 0.04,
            },
            chars: 4,
        }];
        let r = words_in(win, &w);
        assert_eq!(r.len(), 1);
        let (b, n) = r[0];
        assert_eq!((b.x, b.width, n), (200.0, 50.0, 4));
        assert!((b.y - 580.0).abs() < 1e-9 && (b.height - 20.0).abs() < 1e-9);
    }

    fn page(w: usize, h: usize, bg: [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            v.extend_from_slice(&[bg[0], bg[1], bg[2], 255]);
        }
        v
    }

    #[test]
    fn key_out_makes_page_transparent_and_keeps_ink() {
        let (w, h) = (40, 16);
        let mut img = page(w, h, [250, 250, 245]);
        for y in 4..12 {
            for x in 10..30 {
                let i = (y * w + x) * 4;
                img[i..i + 3].copy_from_slice(&[20, 20, 20]);
            }
        }
        let c = key_out(&img, w as u32, h as u32).expect("plain page should key out");
        assert_eq!(c.background, [250, 250, 245]);
        assert_eq!(c.rgba[3], 0, "page pixel should be transparent");
        let ink = ((8 * w) + 20) * 4;
        assert_eq!(c.rgba[ink + 3], 255, "glyph pixel should be opaque");
    }

    #[test]
    fn key_out_rejects_busy_backgrounds_and_blank_regions() {
        let (w, h) = (20, 10);
        // Noisy "photo" background.
        let noisy: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                [
                    (i * 37 % 256) as u8,
                    (i * 91 % 256) as u8,
                    (i * 13 % 256) as u8,
                    255,
                ]
            })
            .collect();
        assert!(key_out(&noisy, w as u32, h as u32).is_none());
        // Blank page: nothing to grab.
        assert!(key_out(&page(w, h, [255, 255, 255]), w as u32, h as u32).is_none());
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
        if std::env::var_os("STICKMAN_SKIP_GUI_TESTS").is_some() {
            return;
        }
        let img = render_text_image(&["The quick brown fox jumps", "over the lazy dog 12345"]);
        let (lines, words, letters) = recognize(&img);
        assert!(lines.len() >= 2, "Vision found {} lines", lines.len());
        // Letters for ransom notes, with their boxes.
        assert!(
            letters.iter().any(|l| l.ch == 'Q'),
            "no Q among {} letters",
            letters.len()
        );
        assert!(letters.iter().filter(|l| l.ch == 'O').count() <= LETTERS_PER_CHAR);
        let q = letters.iter().find(|l| l.ch == 'Q').unwrap();
        assert!(q.rect.width > 0.0 && q.rect.width < 0.2, "{:?}", q.rect);
        assert!(words.len() >= 8, "Vision found {} words", words.len());
        assert!(
            words.iter().any(|w| w.chars == 5),
            "expected 5-letter words (quick, brown, jumps)"
        );
        // Words lie inside their lines and are narrower.
        for w in &words {
            assert!(w.rect.width < 0.6 && w.rect.width > 0.0, "{w:?}");
        }
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
