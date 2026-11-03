//! Renders every pose of the figure into a contact sheet PNG (no window needed):
//!
//! ```sh
//! cargo run --release --example pose_sheet -- docs/poses.png
//! ```

#![allow(unexpected_cfgs)] // objc 0.2 macros

use core_graphics::base::kCGImageAlphaPremultipliedLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::{CGContext, CGLineCap};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use stickman::poses;
use stickman::stickman::{
    depth_below_root, forward_kinematics, idle_pose, walk_pose, BoneKind, Pose,
};

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

const CELL: f64 = 220.0;
const COLS: usize = 7;
const SCALE: f32 = 0.7;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "poses.png".into());
    let h = std::f32::consts::FRAC_PI_2;
    let ko: Pose = std::array::from_fn(|i| idle_pose(0.0)[i] + h);
    let sheet: Vec<(&str, Pose)> = vec![
        ("idle", idle_pose(0.0)),
        ("walk 0%", walk_pose(0.0)),
        ("walk 25%", walk_pose(0.25)),
        ("walk 50%", walk_pose(0.5)),
        ("walk 75%", walk_pose(0.75)),
        ("run", poses::run_pose(0.3)),
        ("run", poses::run_pose(0.8)),
        ("stance", poses::stance(0.0)),
        ("punch wind-up", poses::punch(0.3, -0.15)),
        ("punch", poses::punch(0.5, -0.15)),
        ("uppercut", poses::punch(0.5, -0.75)),
        ("kick chamber", poses::kick(0.28, 0.1)),
        ("kick", poses::kick(0.5, 0.1)),
        ("high kick", poses::kick(0.5, -0.35)),
        ("crouch", poses::crouch(1.0)),
        ("jump (rising)", poses::air(-800.0)),
        ("jump (falling)", poses::air(600.0)),
        ("flail", poses::flail(0.6)),
        ("tumble", poses::tumble()),
        ("dodge", poses::dodge()),
        ("land", poses::land(0.0)),
        ("stomp raise", poses::stomp(0.36)),
        ("stomp", poses::stomp(0.55)),
        ("taunt", poses::taunt(0.0)),
        ("taunt", poses::taunt(0.15)),
        ("think", poses::think(0.0)),
        ("grab (reach)", poses::grab(0.42)),
        ("carry", {
            let mut p = walk_pose(0.3);
            poses::hold_arms(&mut p);
            p
        }),
        ("throw wind-up", poses::throw(0.25, -0.3)),
        ("throw release", poses::throw(0.45, -0.3)),
        ("snatch cursor", poses::snatch(0.45, -0.2)),
        ("hoist cursor", poses::hoist(0.0)),
        ("flying kick", poses::flying_kick(0.5, 0.1)),
        ("dangling", poses::dangle(0.3)),
        ("asleep", poses::sleep(0.0)),
        ("dance", poses::dance(0.2)),
        ("dance", poses::dance(0.6)),
        ("sit on edge", poses::sit(0.0)),
        ("cheer", poses::cheer(0.1)),
        ("super-jump charge", poses::charge(0.0)),
        ("telekinesis", poses::telekinesis(0.0, -0.2)),
        ("sword raised", poses::slash(0.3, 0.0)),
        ("sword cut", poses::slash(0.45, 0.0)),
        ("knocked out", ko),
        ("rest (source)", stickman::stickman::rest_pose()),
    ];
    let rows = sheet.len().div_ceil(COLS);
    let (w, hgt) = (CELL * COLS as f64, CELL * rows as f64);
    let ctx = CGContext::create_bitmap_context(
        None,
        w as usize * 2,
        hgt as usize * 2,
        8,
        0,
        &CGColorSpace::create_device_rgb(),
        kCGImageAlphaPremultipliedLast,
    );
    ctx.scale(2.0, 2.0);
    ctx.set_rgb_fill_color(0.96, 0.96, 0.97, 1.0);
    ctx.fill_rect(CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(w, hgt)));
    ctx.set_line_cap(CGLineCap::CGLineCapRound);
    // Flip to y-down so poses draw upright.
    ctx.translate(0.0, hgt);
    ctx.scale(1.0, -1.0);

    for (i, (name, pose)) in sheet.iter().enumerate() {
        let (col, row) = (i % COLS, i / COLS);
        let (ox, oy) = (col as f64 * CELL, row as f64 * CELL);
        // Cell border and ground line.
        ctx.set_rgb_stroke_color(0.85, 0.85, 0.88, 1.0);
        ctx.set_line_width(1.0);
        ctx.stroke_rect(CGRect::new(&CGPoint::new(ox, oy), &CGSize::new(CELL, CELL)));
        let ground = oy + CELL - 30.0;
        ctx.move_to_point(ox + 20.0, ground);
        ctx.add_line_to_point(ox + CELL - 20.0, ground);
        ctx.stroke_path();
        // Figure, feet on the ground line.
        let root_y = ground as f32 - depth_below_root(pose, SCALE);
        let segs = forward_kinematics(pose, [ox as f32 + CELL as f32 / 2.0, root_y], SCALE, 1.0);
        ctx.set_rgb_stroke_color(43.0 / 255.0, 43.0 / 255.0, 43.0 / 255.0, 1.0);
        for s in &segs {
            ctx.set_line_width(f64::from(s.thickness));
            let (a, b) = (s.start, s.end);
            match s.kind {
                BoneKind::Line => {
                    ctx.move_to_point(f64::from(a[0]), f64::from(a[1]));
                    ctx.add_line_to_point(f64::from(b[0]), f64::from(b[1]));
                    ctx.stroke_path();
                }
                BoneKind::Circle => {
                    let (cx, cy) = (f64::from(a[0] + b[0]) / 2.0, f64::from(a[1] + b[1]) / 2.0);
                    let r = f64::from(((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()) / 2.0;
                    ctx.stroke_ellipse_in_rect(CGRect::new(
                        &CGPoint::new(cx - r, cy - r),
                        &CGSize::new(2.0 * r, 2.0 * r),
                    ));
                }
            }
        }
        label(&ctx, name, ox + 10.0, oy + CELL - 10.0);
    }

    let image = ctx.create_image().expect("render sheet");
    save_png(&image, &out);
    println!("wrote {out} ({} poses)", sheet.len());
}

/// Draw a label with AppKit text drawing (flipped context aware).
fn label(ctx: &CGContext, text: &str, x: f64, y: f64) {
    use foreign_types::ForeignTypeRef;
    // Labels are drawn in a small un-flipped transform.
    ctx.save();
    ctx.translate(x, y);
    ctx.scale(1.0, -1.0);
    let ns_ctx: *mut Object = unsafe {
        let c: *mut Object = msg_send![class!(NSGraphicsContext), graphicsContextWithCGContext: ctx.as_ptr() flipped: false];
        c
    };
    unsafe {
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ns_ctx];
        let font: *mut Object = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
        let key: *mut Object =
            msg_send![class!(NSString), stringWithUTF8String: c"NSFont".as_ptr()];
        let attrs: *mut Object =
            msg_send![class!(NSDictionary), dictionaryWithObject: font forKey: key];
        let c = std::ffi::CString::new(text).unwrap();
        let s: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
        #[repr(C)]
        struct P(f64, f64);
        unsafe impl objc::Encode for P {
            fn encode() -> objc::Encoding {
                unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
            }
        }
        let _: () = msg_send![s, drawAtPoint: P(0.0, 0.0) withAttributes: attrs];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
    }
    ctx.restore();
}

fn save_png(image: &core_graphics::image::CGImage, path: &str) {
    use foreign_types::ForeignType;
    unsafe {
        let rep: *mut Object = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: *mut Object = msg_send![rep, initWithCGImage: image.as_ptr()];
        let props: *mut Object = msg_send![class!(NSDictionary), dictionary];
        // NSBitmapImageFileTypePNG = 4
        let data: *mut Object = msg_send![rep, representationUsingType: 4usize properties: props];
        let c = std::ffi::CString::new(path).unwrap();
        let p: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
        let ok: objc::runtime::BOOL = msg_send![data, writeToFile: p atomically: true];
        assert!(ok != objc::runtime::NO, "could not write {path}");
    }
}
