//! End-to-end tests: launch the real overlay binary and inspect it from outside
//! through the window server (CoreGraphics) and AppKit, the way another app would
//! see it.
//!
//! Custom harness (`harness = false`): AppKit queries want the main thread, and the
//! tests must run one at a time — two overlays would intercept each other's checks.
//! Needs a logged-in GUI session; set `STYK_SKIP_GUI_TESTS=1` to skip (e.g. on CI).
//! While the click-through control test runs (~1 s) the screen briefly ignores clicks.
//! The click test drives the real pointer, so it only runs with
//! `STYK_INTERACTIVE_TESTS=1` (keep your hands off the mouse while it runs).

#![allow(unexpected_cfgs)] // objc 0.2 macros

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGDisplay;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::window as cgw;
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl, Encode, Encoding};

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

const BIN: &str = env!("CARGO_BIN_EXE_stykanimate-overlay");
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Overlay process handle
// ---------------------------------------------------------------------------

struct Overlay {
    child: Child,
    window_number: u32,
    log_line: String,
    startup: Duration,
    logs: Arc<Mutex<Vec<String>>>,
}

impl Overlay {
    fn spawn(envs: &[(&str, &str)]) -> Overlay {
        let started = Instant::now();
        let mut child = Command::new(BIN)
            .env("RUST_LOG", "info")
            // Defaults keep test runs off the user's cursor, windows and dialogs,
            // and the behaviour deterministic (no LLM). Tests override as needed.
            .env("STYK_NO_PROMPTS", "1")
            .env("STYK_NO_CURSOR_PUSH", "1")
            .env("STYK_NO_WINDOW_SHAKE", "1")
            .env("STYK_MODE", "IDLE")
            // Never read or write the user's real settings file.
            .env(
                "STYK_SETTINGS",
                std::env::temp_dir()
                    .join(format!("styk-test-settings-{}.json", std::process::id())),
            )
            .envs(envs.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn overlay binary");

        // Forward the "overlay up" line; keep draining stderr so the pipe never fills.
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("overlay up:") || line.contains("ERROR") {
                    let _ = tx.send(line.clone());
                }
                sink.lock().unwrap().push(line);
            }
        });
        let log_line = match rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(l) if l.contains("overlay up:") => l,
            Ok(l) => {
                let _ = child.kill();
                panic!("overlay failed to start: {l}");
            }
            Err(_) => {
                let _ = child.kill();
                panic!("overlay did not report startup within {STARTUP_TIMEOUT:?}");
            }
        };
        let window_number = field(&log_line, "window_number")
            .parse()
            .expect("window_number in log");
        let overlay = Overlay {
            child,
            window_number,
            log_line,
            startup: started.elapsed(),
            logs,
        };
        // Let the window server finish ordering the window in.
        pump_run_loop(Duration::from_millis(400));
        overlay
    }

    fn pid(&self) -> i64 {
        i64::from(self.child.id())
    }

    /// First log line containing `pattern`, waiting up to `timeout`.
    fn wait_for(&self, pattern: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(l) = self
                .logs
                .lock()
                .unwrap()
                .iter()
                .find(|l| l.contains(pattern))
            {
                return Some(l.clone());
            }
            if Instant::now() > deadline {
                return None;
            }
            pump_run_loop(Duration::from_millis(50));
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Wait for the window server to drop the window so the next test starts
        // from a clean screen.
        let deadline = Instant::now() + Duration::from_secs(2);
        while window_info(self.window_number).is_some() && Instant::now() < deadline {
            pump_run_loop(Duration::from_millis(30));
        }
    }
}

/// Value of `key=value` in the startup log line.
fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let pat = format!("{key}=");
    let start = line
        .find(&pat)
        .unwrap_or_else(|| panic!("{key} missing in {line}"))
        + pat.len();
    line[start..].split_whitespace().next().unwrap()
}

fn pump_run_loop(d: Duration) {
    // SAFETY: kCFRunLoopDefaultMode is a valid static CFString.
    CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, d, false);
}

// ---------------------------------------------------------------------------
// Window-server queries (CoreGraphics)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct WinInfo {
    number: i64,
    owner_pid: i64,
    owner_name: String,
    layer: i64,
    bounds: CGRect,
    on_screen: bool,
    alpha: f64,
}

fn key(k: CFStringRef) -> CFString {
    // SAFETY: the kCGWindow* keys are valid static CFStrings.
    unsafe { CFString::wrap_under_get_rule(k) }
}

fn windows(option: cgw::CGWindowListOption, relative_to: cgw::CGWindowID) -> Vec<WinInfo> {
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
            let bounds = d
                .find(key(unsafe { cgw::kCGWindowBounds }))
                .and_then(|v| v.downcast::<CFDictionary>())
                .and_then(|b| CGRect::from_dict_representation(&b))
                .unwrap_or_else(|| CGRect::new(&CGPoint::new(0., 0.), &CGSize::new(0., 0.)));
            WinInfo {
                number: num(&d, unsafe { cgw::kCGWindowNumber }).unwrap_or(-1.) as i64,
                owner_pid: num(&d, unsafe { cgw::kCGWindowOwnerPID }).unwrap_or(-1.) as i64,
                owner_name: d
                    .find(key(unsafe { cgw::kCGWindowOwnerName }))
                    .and_then(|v| v.downcast::<CFString>())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                layer: num(&d, unsafe { cgw::kCGWindowLayer }).unwrap_or(-1.) as i64,
                bounds,
                on_screen: d
                    .find(key(unsafe { cgw::kCGWindowIsOnscreen }))
                    .and_then(|v| v.downcast::<CFBoolean>())
                    .map(bool::from)
                    .unwrap_or(false),
                alpha: num(&d, unsafe { cgw::kCGWindowAlpha }).unwrap_or(0.),
            }
        })
        .collect()
}

fn window_info(number: u32) -> Option<WinInfo> {
    windows(cgw::kCGWindowListOptionIncludingWindow, number)
        .into_iter()
        .find(|w| w.number == i64::from(number))
}

fn main_display() -> CGRect {
    CGDisplay::main().bounds()
}

// ---------------------------------------------------------------------------
// AppKit queries
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
// SAFETY: matches CGPoint layout and encoding on 64-bit.
unsafe impl Encode for NSPoint {
    fn encode() -> Encoding {
        unsafe { Encoding::from_str("{CGPoint=dd}") }
    }
}

/// Window that would receive a mouse-down at a CoreGraphics (top-left) point.
fn window_hit_at(x: f64, y_top_left: f64) -> isize {
    let p = NSPoint {
        x,
        y: main_display().size.height - y_top_left,
    };
    // SAFETY: documented AppKit class method; called on the main thread.
    unsafe {
        msg_send![class!(NSWindow), windowNumberAtPoint: p belowWindowWithWindowNumber: 0isize]
    }
}

fn frontmost_app_pid() -> i64 {
    pump_run_loop(Duration::from_millis(200)); // refresh NSWorkspace's cached state
                                               // SAFETY: documented AppKit API; main thread.
    unsafe {
        let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut Object = msg_send![ws, frontmostApplication];
        if app.is_null() {
            return -1;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        i64::from(pid)
    }
}

/// NSApplicationActivationPolicy: 0 regular, 1 accessory, 2 prohibited.
fn activation_policy(pid: i64) -> isize {
    // SAFETY: documented AppKit API; main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid as i32];
        assert!(!app.is_null(), "no NSRunningApplication for pid {pid}");
        msg_send![app, activationPolicy]
    }
}

fn cpu_seconds(pid: i64) -> f64 {
    let out = Command::new("ps")
        .args(["-o", "cputime=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    // Format: [[dd-]hh:]mm:ss.cc
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().split(':').fold(0.0, |acc, part| {
        acc * 60.0 + part.parse::<f64>().unwrap_or(0.0)
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn test_window_covers_main_display_at_status_level(o: &Overlay) {
    let w = window_info(o.window_number).expect("overlay window known to the window server");
    let d = main_display();
    assert_eq!(w.owner_pid, o.pid(), "window owned by the overlay process");
    assert!(w.on_screen, "window is on screen");
    assert_eq!(w.layer, 25, "window level is NSStatusWindowLevel");
    assert!(
        w.alpha > 0.99,
        "window alpha is 1 (transparency comes from content)"
    );
    assert_eq!(
        (
            w.bounds.origin.x,
            w.bounds.origin.y,
            w.bounds.size.width,
            w.bounds.size.height
        ),
        (d.origin.x, d.origin.y, d.size.width, d.size.height),
        "window frame equals the main display, including the menu bar strip"
    );
}

fn test_window_is_above_all_normal_windows(o: &Overlay) {
    let d = main_display();
    for (x, y) in [
        (d.size.width / 2., d.size.height / 2.),
        (5., 5.), // menu bar, top-left
        (d.size.width - 5., d.size.height - 5.),
    ] {
        let p = CGPoint::new(d.origin.x + x, d.origin.y + y);
        let top = windows(cgw::kCGWindowListOptionOnScreenOnly, cgw::kCGNullWindowID)
            .into_iter()
            .filter(|w| w.layer <= 25 && w.bounds.contains(&p))
            .find(|w| w.layer == 25 || w.owner_pid != o.pid())
            .expect("some window at point");
        assert_eq!(
            top.number,
            i64::from(o.window_number),
            "at ({x},{y}) the frontmost window up to status level is {} ({}), not the overlay",
            top.number,
            top.owner_name
        );
    }
}

fn test_mouse_events_pass_through(o: &Overlay) {
    let d = main_display();
    for (x, y) in [
        (d.size.width / 2., d.size.height / 2.),
        (10., 10.),
        (d.size.width - 10., d.size.height - 10.),
        (d.size.width / 4., d.size.height * 0.75),
    ] {
        let hit = window_hit_at(x, y);
        assert_ne!(
            hit as i64,
            i64::from(o.window_number),
            "a click at ({x},{y}) would land on the overlay"
        );
    }
}

fn test_appkit_properties_applied(o: &Overlay) {
    assert_eq!(field(&o.log_line, "level"), "25");
    let behavior =
        usize::from_str_radix(field(&o.log_line, "behavior").trim_start_matches("0x"), 16)
            .expect("hex behavior");
    assert_eq!(behavior & 0x1, 0x1, "canJoinAllSpaces");
    assert_eq!(behavior & 0x10, 0x10, "stationary");
    assert_eq!(behavior & 0x40, 0x40, "ignoresCycle");
    assert_eq!(behavior & 0x100, 0x100, "fullScreenAuxiliary");
    assert_eq!(field(&o.log_line, "click_through"), "true");
}

/// Visible pixels of one capture of the overlay window, in points.
struct Capture {
    visible: usize,
    total: usize,
    /// min_x, min_y, max_x, max_y of visible pixels, in points
    bbox: Option<[f64; 4]>,
    centroid_x: f64,
}

fn capture(o: &Overlay) -> Capture {
    let null_rect = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0., 0.),
    );
    let image = cgw::create_image(
        null_rect,
        cgw::kCGWindowListOptionIncludingWindow,
        o.window_number,
        cgw::kCGWindowImageBoundsIgnoreFraming,
    )
    .expect("capture overlay window (grant Screen Recording to your terminal if this fails)");
    let (w, h, bpr) = (image.width(), image.height(), image.bytes_per_row());
    assert!(w > 0 && h > 0, "captured image is empty");
    let px_per_pt = w as f64 / main_display().size.width;
    let data = image.data();
    let bytes = data.bytes();
    let (mut visible, mut sum_x) = (0usize, 0f64);
    let mut bbox: Option<[f64; 4]> = None;
    for row in 0..h {
        let line = &bytes[row * bpr..row * bpr + w * 4];
        for (col, px) in line.chunks_exact(4).enumerate() {
            // Premultiplied 32-bit pixels: transparent == all-zero bytes.
            if px.iter().all(|&b| b == 0) {
                continue;
            }
            visible += 1;
            sum_x += col as f64;
            let (x, y) = (col as f64, row as f64);
            bbox = Some(match bbox {
                None => [x, y, x, y],
                Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)],
            });
        }
    }
    Capture {
        visible,
        total: w * h,
        bbox: bbox.map(|b| {
            [
                b[0] / px_per_pt,
                b[1] / px_per_pt,
                (b[2] + 1.0) / px_per_pt,
                (b[3] + 1.0) / px_per_pt,
            ]
        }),
        centroid_x: if visible > 0 {
            sum_x / visible as f64 / px_per_pt
        } else {
            0.0
        },
    }
}

fn test_stickman_is_drawn_on_the_ground() {
    // Standing still, so it can't have wandered onto a window by now.
    let o = &Overlay::spawn(&[("STYK_HOLD_IDLE", "1")]);
    pump_run_loop(Duration::from_millis(400));
    let c = capture(o);
    let bbox = c
        .bbox
        .expect("no visible pixels: the stickman is not being drawn");
    let share = c.visible as f64 / c.total as f64 * 100.0;
    let height = bbox[3] - bbox[1];
    let ground: f64 = field(&o.log_line, "ground_y").parse().unwrap();
    println!(
        "      stickman: {} px visible ({share:.3}% of screen), bbox x {:.0}..{:.0} y {:.0}..{:.0} pt, ground_y {ground}",
        c.visible, bbox[0], bbox[2], bbox[1], bbox[3]
    );
    // Everything except the figure must stay transparent.
    assert!(share < 1.0, "{share:.2}% of the screen is covered");
    // ~250 pt rig x 0.55 scale, plus outline.
    assert!(
        (110.0..190.0).contains(&height),
        "figure is {height:.0} pt tall"
    );
    // PRD 5: collision/visual alignment within 5 px. Feet (with 1.5 pt outline) on the ground line.
    assert!(
        (bbox[3] - ground).abs() <= 4.0,
        "feet at {:.1} pt but ground is {ground}",
        bbox[3]
    );
}

fn test_stickman_animates_and_walks(o: &Overlay) {
    let first = capture(o);
    let mut max_shift: f64 = 0.0;
    let mut pose_changed = false;
    for _ in 0..4 {
        pump_run_loop(Duration::from_millis(400));
        let next = capture(o);
        assert!(next.visible > 0, "stickman disappeared");
        max_shift = max_shift.max((next.centroid_x - first.centroid_x).abs());
        pose_changed |= next.visible != first.visible || next.bbox != first.bbox;
    }
    println!("      walked {max_shift:.1} pt in 1.6 s");
    assert!(
        pose_changed,
        "frames are identical: the render loop is not running"
    );
    assert!(
        max_shift > 10.0,
        "figure did not travel (moved {max_shift:.1} pt)"
    );
}

fn test_does_not_steal_focus_or_show_in_dock(o: &Overlay) {
    assert_eq!(
        activation_policy(o.pid()),
        1,
        "accessory app (no Dock icon / Cmd-Tab)"
    );
    assert_ne!(
        frontmost_app_pid(),
        o.pid(),
        "overlay must not become the active app"
    );
}

fn cpu_percent(o: &Overlay, secs: u64) -> f64 {
    let before = cpu_seconds(o.pid());
    pump_run_loop(Duration::from_secs(secs));
    (cpu_seconds(o.pid()) - before) / secs as f64 * 100.0
}

/// Regression guard while walking at 60 fps. Each Metal present costs ~2 ms of
/// CPU (submit + completion + WindowServer IPC), so 60 fps lands around 12-16%.
/// The PRD's 5% budget applies to idle, tested separately.
fn test_cpu_while_walking_is_bounded(o: &Overlay) {
    let pct = cpu_percent(o, 3);
    println!("      CPU while walking (60 fps): {pct:.2}% of one core");
    assert!(pct < 25.0, "CPU {pct:.2}% while walking (regression guard)");
}

/// PRD 5: under 5% CPU during idle observation.
fn test_idle_cpu_is_under_prd_budget() {
    let o = Overlay::spawn(&[("STYK_HOLD_IDLE", "1")]);
    // Warm up: first OCR pass loads Vision's model, first frames compile pipelines.
    pump_run_loop(Duration::from_secs(5));
    let pct = cpu_percent(&o, 8);
    println!("      CPU while idle (steady state): {pct:.2}% of one core");
    // The budget applies to the shipped build. Debug builds enable wgpu's
    // validation layer and skip optimisation, so they only report the number.
    if cfg!(debug_assertions) {
        println!("      (debug build: PRD budget enforced only with `cargo test --release`)");
    } else {
        assert!(pct < 5.0, "idle CPU {pct:.2}% exceeds the PRD's 5% budget");
    }
}

fn test_startup_time(o: &Overlay) {
    println!("      startup: {:?}", o.startup);
    assert!(
        o.startup < Duration::from_secs(5),
        "startup took {:?}",
        o.startup
    );
}

/// Control: with click-through off, the same hit test must find the overlay.
/// Proves `test_mouse_events_pass_through` can actually detect a failure.
fn test_control_hit_test_detects_opaque_overlay() {
    let o = Overlay::spawn(&[("STYK_CLICK_THROUGH", "0")]);
    assert_eq!(field(&o.log_line, "click_through"), "false");
    let d = main_display();
    let hit = window_hit_at(d.size.width / 2., d.size.height / 2.);
    assert_eq!(
        hit as i64,
        i64::from(o.window_number),
        "hit test should hit the overlay"
    );
}

fn test_exits_cleanly_on_sigint_and_removes_window() {
    let mut o = Overlay::spawn(&[]);
    let n = o.window_number;
    let status = Command::new("kill")
        .args(["-INT", &o.pid().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    let exit = loop {
        if let Some(s) = o.child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "overlay ignored SIGINT for 5 s");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        exit.success() || exit.signal() == Some(2),
        "unexpected exit status {exit:?}"
    );
    pump_run_loop(Duration::from_millis(300));
    assert!(
        window_info(n).is_none(),
        "window still registered after exit"
    );
}

// --- Full-app behaviour ----------------------------------------------------------

fn post_mouse(kind: core_graphics::event::CGEventType, x: f64, y: f64) {
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGMouseButton};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState).expect("event source");
    let e = CGEvent::new_mouse_event(src, kind, CGPoint::new(x, y), CGMouseButton::Left)
        .expect("mouse event");
    e.post(CGEventTapLocation::HID);
}

fn cursor_now() -> CGPoint {
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    let src = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();
    CGEvent::new(src).unwrap().location()
}

/// PRD 3.1 + 3.3: aim at the figure and click it. It must intercept the click
/// (dynamic click-through), get knocked back, burst into GPU particles, and hand
/// keyboard focus straight back to the app the user was in.
fn test_click_on_figure_knocks_it_back() {
    use core_graphics::event::CGEventType;
    if std::env::var_os("STYK_INTERACTIVE_TESTS").is_none() {
        println!("      (skipped: moves the real pointer; run with STYK_INTERACTIVE_TESTS=1 and hands off the mouse)");
        return;
    }
    let o = Overlay::spawn(&[
        ("STYK_HOLD_IDLE", "1"),
        ("RUST_LOG", "info,stykanimate_overlay=debug"),
    ]);
    let before_app = frontmost_app_pid();
    let resting = capture(&o);
    let b = resting.bbox.expect("figure visible");
    let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
    let home = cursor_now();
    // Glide onto the figure like a user aiming at it, hover, then click.
    for i in 0..=12 {
        let k = i as f64 / 12.0;
        post_mouse(CGEventType::MouseMoved, cx - 160.0 * (1.0 - k), cy);
        pump_run_loop(Duration::from_millis(16));
    }
    pump_run_loop(Duration::from_millis(200));
    // Re-place the pointer right before clicking, in case the real mouse moved.
    post_mouse(CGEventType::MouseMoved, cx + 1.0, cy);
    pump_run_loop(Duration::from_millis(200));
    post_mouse(CGEventType::LeftMouseDown, cx + 1.0, cy);
    pump_run_loop(Duration::from_millis(30));
    post_mouse(CGEventType::LeftMouseUp, cx + 1.0, cy);
    let hit = o.wait_for("event: knockback", Duration::from_secs(2));
    pump_run_loop(Duration::from_millis(60));
    let burst = capture(&o);
    // Put the user's pointer back where it was.
    post_mouse(CGEventType::MouseMoved, home.x, home.y);
    if hit.is_none() {
        for l in o.logs.lock().unwrap().iter() {
            println!("      log: {l}");
        }
        println!("      figure bbox {b:?}, clicked at ({cx:.0}, {cy:.0})");
    }
    let hit = hit.expect("clicking the figure did not knock it back (is Accessibility granted to the terminal for posting events?)");
    println!("      {}", hit.split("] ").nth(1).unwrap_or(&hit));
    println!(
        "      visible px: resting {} -> after click {}",
        resting.visible, burst.visible
    );
    assert!(burst.visible > resting.visible * 2, "no particle burst");
    pump_run_loop(Duration::from_millis(600));
    let after_app = frontmost_app_pid();
    assert_ne!(
        after_app,
        o.pid(),
        "clicking the figure left the overlay focused"
    );
    assert_eq!(
        after_app, before_app,
        "focus was not handed back to the previous app"
    );
}

/// PRD 3.4: the director runs on its own and its decisions reach the figure.
fn test_ai_director_decides() {
    let ollama_up = std::net::TcpStream::connect_timeout(
        &"127.0.0.1:11434".parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok();
    let o = Overlay::spawn(&[("STYK_MODE", ""), ("STYK_DIRECTOR_INTERVAL_S", "1")]);
    // With Ollama up, a model decision must arrive (a transient fallback is fine);
    // without it, the built-in director must keep the figure going.
    let want = if ollama_up { "(Llm," } else { "(Fallback," };
    let line = o
        .wait_for(want, Duration::from_secs(20))
        .unwrap_or_else(|| panic!("no {want} decision within 20 s (ollama up: {ollama_up})"));
    println!("      {}", line.split("] ").nth(1).unwrap_or(&line));
}

/// PRD 3.2: OCR turns the active window's text into platforms.
fn test_ocr_finds_text_platforms() {
    let o = Overlay::spawn(&[("RUST_LOG", "info,stykanimate_overlay=debug")]);
    if field(&o.log_line, "ocr") != "true" {
        println!("      (skipped: Screen Recording not granted to the terminal)");
        return;
    }
    let line = o
        .wait_for("text platforms, ", Duration::from_secs(8))
        .expect("OCR produced no result for the active window");
    println!("      {}", line.split("] ").nth(1).unwrap_or(&line));
    // How many lines depends on what's in the user's active window; recognition
    // itself is verified against known text in `ocr::tests::vision_finds_text_lines`.
    assert!(line.contains("text platforms, ") && line.contains(" words on window"));
}

/// The menu bar extra exists, is visible, and sits in the menu bar strip.
fn test_menu_bar_icon(o: &Overlay) {
    let line = o
        .wait_for("menubar: visible=", Duration::from_secs(4))
        .expect("menu bar item never reported its placement");
    println!("      {}", line.split("] ").nth(1).unwrap_or(&line));
    assert!(line.contains("visible=true"), "{line}");
    let frame = field(&line, "frame");
    let nums: Vec<f64> = frame
        .split([',', 'x'])
        .filter_map(|n| n.parse().ok())
        .collect();
    let [x, y, w, h] = nums[..] else {
        panic!("bad frame {frame}")
    };
    let d = main_display();
    assert!(w >= 16.0 && h >= 16.0, "icon too small: {w}x{h}");
    // AppKit y is bottom-up: the menu bar strip is at the top of the main display.
    assert!(
        (y + h - d.size.height).abs() < 2.0,
        "not in the menu bar (y={y}, h={h})"
    );
    assert!(x >= 0.0 && x + w <= d.size.width, "off screen (x={x})");
}

/// The headline trick: in combat the figure rips words off the active window
/// and throws them at the cursor (pointer pushes are disabled in tests).
fn test_rips_words_and_throws_them() {
    let o = Overlay::spawn(&[
        ("STYK_MODE", "COMBAT"),
        ("RUST_LOG", "info,stykanimate_overlay=debug"),
    ]);
    if field(&o.log_line, "ocr") != "true" {
        println!("      (skipped: Screen Recording not granted to the terminal)");
        return;
    }
    let ripped = o.wait_for(
        "event: ripped a word off the screen",
        Duration::from_secs(25),
    );
    let Some(line) = ripped else {
        let words_seen = o
            .logs
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("ocr: ") && !l.contains(" 0 words"));
        assert!(
            !words_seen,
            "the active window had words but none were ripped off"
        );
        println!("      (skipped: the active window has no text right now)");
        return;
    };
    println!("      {}", line.split("] ").nth(1).unwrap_or(&line));
    let hit = o.wait_for(
        "event: a thrown word hit the cursor",
        Duration::from_secs(10),
    );
    let thrown = hit.is_some()
        || o.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains("ripped a word"))
            .count()
            > 1;
    assert!(thrown, "a word was grabbed but never thrown");
    if let Some(h) = hit {
        println!("      {}", h.split("] ").nth(1).unwrap_or(&h));
    }
}

type SharedTest = (&'static str, fn(&Overlay));

fn main() {
    if std::env::var_os("STYK_SKIP_GUI_TESTS").is_some() {
        println!("STYK_SKIP_GUI_TESTS set: skipping overlay GUI tests");
        return;
    }
    // AppKit connection for the hit-test / NSWorkspace queries.
    // SAFETY: main thread, documented API.
    let _: *mut Object = unsafe { msg_send![class!(NSApplication), sharedApplication] };

    let mut failures = 0;
    // `cargo test --test overlay -- <substring>` runs only matching tests.
    let filter: Option<String> = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let wanted = |name: &str| filter.as_deref().is_none_or(|f| name.contains(f));
    let mut run = |name: &str, f: &dyn Fn()| {
        if !wanted(name) {
            return;
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        println!(
            "test {name} ... {}",
            if r.is_ok() { "ok" } else { "FAILED" }
        );
        if r.is_err() {
            failures += 1;
        }
    };

    println!("\nrunning overlay GUI tests against {BIN}\n");
    {
        let shared: &[SharedTest] = &[
            ("startup_time", test_startup_time),
            ("menu_bar_icon", test_menu_bar_icon),
            (
                "stickman_animates_and_walks",
                test_stickman_animates_and_walks,
            ),
            ("appkit_properties_applied", test_appkit_properties_applied),
            (
                "window_covers_main_display_at_status_level",
                test_window_covers_main_display_at_status_level,
            ),
            (
                "window_is_above_all_normal_windows",
                test_window_is_above_all_normal_windows,
            ),
            ("mouse_events_pass_through", test_mouse_events_pass_through),
            (
                "does_not_steal_focus_or_show_in_dock",
                test_does_not_steal_focus_or_show_in_dock,
            ),
            (
                "cpu_while_walking_is_bounded",
                test_cpu_while_walking_is_bounded,
            ),
        ];
        if shared.iter().any(|(name, _)| wanted(name)) {
            let o = Overlay::spawn(&[]);
            println!(
                "      {}",
                o.log_line.split("overlay up: ").nth(1).unwrap_or("")
            );
            for &(name, t) in shared {
                run(name, &|| t(&o));
            }
        }
    }
    run(
        "stickman_is_drawn_on_the_ground",
        &test_stickman_is_drawn_on_the_ground,
    );
    run(
        "idle_cpu_is_under_prd_budget",
        &test_idle_cpu_is_under_prd_budget,
    );
    run(
        "click_on_figure_knocks_it_back",
        &test_click_on_figure_knocks_it_back,
    );
    run("ai_director_decides", &test_ai_director_decides);
    run(
        "rips_words_and_throws_them",
        &test_rips_words_and_throws_them,
    );
    run("ocr_finds_text_platforms", &test_ocr_finds_text_platforms);
    run(
        "control_hit_test_detects_opaque_overlay",
        &test_control_hit_test_detects_opaque_overlay,
    );
    run(
        "exits_cleanly_on_sigint_and_removes_window",
        &test_exits_cleanly_on_sigint_and_removes_window,
    );

    println!("\noverlay GUI tests: {failures} failed");
    if failures > 0 {
        std::process::exit(1);
    }
}
