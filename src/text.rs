//! Text rasterisation for speech bubbles: AppKit lays out and draws the string
//! into a CoreGraphics bitmap, which becomes a GPU sprite.
//!
//! Main thread only (AppKit text drawing through `NSGraphicsContext`).

#![allow(unexpected_cfgs)] // objc 0.2 macros

use std::ffi::CString;

use core_graphics::base::kCGImageAlphaPremultipliedLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use foreign_types::ForeignType;
use objc::runtime::{Object, NO};
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    w: f64,
    h: f64,
}
// SAFETY: same layout/encoding as CGSize on 64-bit.
unsafe impl Encode for NSSize {
    fn encode() -> Encoding {
        // SAFETY: a valid Objective-C type encoding for CGSize.
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
        // SAFETY: a valid Objective-C type encoding for CGPoint.
        unsafe { Encoding::from_str("{CGPoint=dd}") }
    }
}

/// A rasterised block of text: premultiplied RGBA8, top row first.
pub struct TextImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Size in points (pixels / scale).
    pub size_pt: (f32, f32),
}

/// Autoreleased NSString.
fn ns_string(s: &str) -> *mut Object {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    // SAFETY: valid NUL-terminated UTF-8; the result is autoreleased.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

/// Draw `lines` (centred, bold system font of `point_size`, colour `rgb`) at
/// `scale` pixels per point. Returns `None` for empty text or if AppKit fails.
pub fn render_lines(
    lines: &[String],
    point_size: f64,
    rgb: [f64; 3],
    scale: f64,
) -> Option<TextImage> {
    if lines.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    // SAFETY: main thread (module contract). Every message below is documented
    // AppKit API with these argument types; all returned objects are
    // autoreleased and only used inside this function. The CGContext outlives
    // the NSGraphicsContext wrapping it (both end with this scope), and the
    // graphics state is restored before returning.
    unsafe {
        let font: *mut Object = msg_send![class!(NSFont), boldSystemFontOfSize: point_size];
        let color: *mut Object = msg_send![class!(NSColor),
            colorWithSRGBRed: rgb[0] green: rgb[1] blue: rgb[2] alpha: 1.0f64];
        if font.is_null() || color.is_null() {
            return None;
        }
        // NSFontAttributeName == @"NSFont", NSForegroundColorAttributeName == @"NSColor".
        let keys = [ns_string("NSFont"), ns_string("NSColor")];
        let values = [font, color];
        let attrs: *mut Object = msg_send![class!(NSDictionary),
            dictionaryWithObjects: values.as_ptr() forKeys: keys.as_ptr() count: 2usize];

        let strings: Vec<*mut Object> = lines.iter().map(|l| ns_string(l)).collect();
        let sizes: Vec<NSSize> = strings
            .iter()
            .map(|s| msg_send![*s, sizeWithAttributes: attrs])
            .collect();
        let line_h = sizes.iter().map(|s| s.h).fold(0.0, f64::max).ceil();
        let w_pt = sizes.iter().map(|s| s.w).fold(0.0, f64::max).ceil() + 2.0;
        let h_pt = line_h * lines.len() as f64;
        let (w, h) = (
            (w_pt * scale).ceil() as usize,
            (h_pt * scale).ceil() as usize,
        );
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return None;
        }

        let space = CGColorSpace::create_device_rgb();
        let mut ctx = CGContext::create_bitmap_context(
            None,
            w,
            h,
            8,
            w * 4,
            &space,
            kCGImageAlphaPremultipliedLast,
        );
        ctx.scale(scale, scale);
        let ns_ctx: *mut Object = msg_send![class!(NSGraphicsContext),
            graphicsContextWithCGContext: ctx.as_ptr() as *mut Object flipped: NO];
        if ns_ctx.is_null() {
            return None;
        }
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ns_ctx];
        for (i, (s, size)) in strings.iter().zip(&sizes).enumerate() {
            // CoreGraphics is y-up: the first line goes at the top.
            let p = NSPoint {
                x: ((w_pt - size.w) / 2.0).max(0.0),
                y: h_pt - line_h * (i as f64 + 1.0),
            };
            let _: () = msg_send![*s, drawAtPoint: p withAttributes: attrs];
        }
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];

        Some(TextImage {
            rgba: ctx.data().to_vec(),
            width: w as u32,
            height: h as u32,
            size_pt: ((w as f64 / scale) as f32, (h as f64 / scale) as f32),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_visible_dark_text() {
        let img = render_lines(
            &["Hey!".into(), "Stop that".into()],
            13.0,
            [0.1, 0.1, 0.1],
            2.0,
        )
        .expect("rendered");
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        assert!(
            img.size_pt.0 > 40.0 && img.size_pt.1 > 25.0,
            "{:?}",
            img.size_pt
        );
        let inked = img.rgba.chunks(4).filter(|p| p[3] > 128).count();
        assert!(inked > 100, "only {inked} opaque pixels");
        // Dark glyphs: opaque pixels are dark.
        assert!(img
            .rgba
            .chunks(4)
            .filter(|p| p[3] == 255)
            .all(|p| p[0] < 90));
        // The first line is at the top: some ink in the top half.
        let top = &img.rgba[..img.rgba.len() / 2];
        assert!(top.chunks(4).any(|p| p[3] > 128));
    }

    #[test]
    fn empty_text_gives_nothing() {
        assert!(render_lines(&[" ".into()], 13.0, [0.0; 3], 2.0).is_none());
        assert!(render_lines(&[], 13.0, [0.0; 3], 2.0).is_none());
    }
}
