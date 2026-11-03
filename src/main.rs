//! StykAnimate desktop overlay: a transparent, click-through, always-on-top window
//! covering the main display, with the StykAnimate "Walker" stick figure strolling
//! along the top of the Dock.
//!
//! Threading model (claude.md rule 1): winit/AppKit and the wgpu render loop own
//! the main thread. The tokio runtime runs on its own worker threads and is
//! reserved for the AI Director (Ollama), window polling and OCR.
//!
//! Diagnostics: `STYK_CLICK_THROUGH=0` starts the overlay *catching* mouse events
//! (it then blocks clicks to the whole screen — debugging/tests only).
//! `STYK_HOLD_IDLE=1` keeps the figure standing idle (used to measure idle CPU).

mod macos;
mod render;
mod stickman;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{WindowAttributes, WindowId, WindowLevel};

#[cfg(not(target_os = "macos"))]
compile_error!("stykanimate-overlay targets macOS (aarch64-apple-darwin) only");

// NSWindowCollectionBehavior bits (AppKit/NSWindow.h).
const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const STATIONARY: usize = 1 << 4;
const IGNORES_CYCLE: usize = 1 << 6;
const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

/// Present on every Space, don't slide during Space swipes, stay out of Cmd-`
/// cycling, and allow overlaying other apps' full-screen Spaces.
const OVERLAY_COLLECTION_BEHAVIOR: usize =
    CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY;

/// NSStatusWindowLevel (kCGStatusWindowLevel = 25): above normal and floating
/// windows and the menu bar, below system alerts and the screen saver.
const OVERLAY_WINDOW_LEVEL: isize = 25;

/// Figure scale: the source rig is ~250 pt tall, this makes it ~140 pt.
const FIGURE_SCALE: f32 = 0.55;
/// Keep the figure this far (pt) from the screen edges.
const EDGE_MARGIN: f32 = 60.0;

/// Smallest joint movement worth presenting a frame for: half a Retina pixel.
const MIN_VISIBLE_SHIFT_PT: f32 = 0.25;

/// Largest distance any joint moved between two placements of the figure.
fn max_joint_shift(a: &[stickman::Segment], b: &[stickman::Segment]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(a, b)| [(a.start, b.start), (a.end, b.end)])
        .map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt())
        .fold(0.0, f32::max)
}

/// A rectangle in points: `x`, `y` is the origin, in whichever coordinate space the
/// caller documents.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Convert between CoreGraphics rects (global, top-left origin, y down) and AppKit
/// screen frames (bottom-left origin of the primary display, y up). The mapping
/// is its own inverse.
fn cg_to_cocoa(rect: Rect, primary_height: f64) -> Rect {
    Rect {
        y: primary_height - (rect.y + rect.height),
        ..rect
    }
}

/// Ground line for the figure, in window points (y down from the display top):
/// the top edge of the Dock, or the bottom of the screen if the Dock is hidden
/// or on the side.
fn ground_line(display: Rect, visible_cocoa: Option<Rect>, primary_height: f64) -> f32 {
    let bottom = display.height;
    let ground = visible_cocoa
        .map(|v| cg_to_cocoa(v, primary_height))
        .map(|v| v.y + v.height - display.y)
        .filter(|g| *g > display.height * 0.5 && *g <= bottom)
        .unwrap_or(bottom);
    ground as f32
}

/// Click-through defaults to on; only an explicit `0`/`false`/`off`/`no` disables it.
fn click_through_from_env(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

struct Scene {
    renderer: render::Renderer,
    figure: stickman::Figure,
    ground_y: f32,
    last_frame: Instant,
    next_frame: Instant,
    /// What is currently on screen; frames that wouldn't visibly differ are skipped.
    drawn: Option<[stickman::Segment; stickman::BONES]>,
}

impl Scene {
    /// Advance the simulation to now and draw one frame.
    fn frame(&mut self, display_width: f32) {
        let now = Instant::now();
        // Clamp so a stall (sleep, Space switch) doesn't teleport the figure.
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.figure
            .update(dt, EDGE_MARGIN, display_width - EDGE_MARGIN);
        let segments = self.figure.segments(self.ground_y);
        // Each present costs ~2 ms of CPU in Metal/WindowServer plumbing, so don't
        // pay it for a frame that would look identical (mostly idle breathing).
        if let Some(drawn) = &self.drawn {
            if max_joint_shift(drawn, &segments) < MIN_VISIBLE_SHIFT_PT {
                return;
            }
        }
        self.renderer.render(&segments, stickman::WALKER_RGB);
        self.drawn = Some(segments);
    }
}

struct App {
    scene: Option<Scene>,
    display: Rect,
    click_through: bool,
    /// Background runtime for async work; kept alive for the life of the app.
    _runtime: tokio::runtime::Runtime,
}

impl App {
    fn create_overlay(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        use winit::platform::macos::WindowAttributesExtMacOS;

        let d = self.display;
        let attributes = WindowAttributes::default()
            .with_title("StykAnimate Overlay")
            .with_transparent(true)
            .with_decorations(false) // borderless + frameless
            .with_resizable(false)
            .with_active(false) // never steal focus from the user's app
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(LogicalPosition::new(d.x, d.y))
            .with_inner_size(LogicalSize::new(d.width, d.height))
            .with_has_shadow(false);

        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("failed to create overlay window")?,
        );

        // AppKit-level overlay configuration (click-through, Spaces, level, exact frame).
        let primary_height = main_display_rect().height;
        let state = {
            let overlay = macos::OverlayWindow::from_winit(&window)?;
            overlay.configure(cg_to_cocoa(d, primary_height), self.click_through);
            overlay.state()
        };
        // Read back from AppKit so a silently ignored call is caught.
        if state.ignores_mouse_events != self.click_through
            || state.collection_behavior & OVERLAY_COLLECTION_BEHAVIOR
                != OVERLAY_COLLECTION_BEHAVIOR
            || state.level != OVERLAY_WINDOW_LEVEL
        {
            log::warn!("AppKit did not apply every overlay property: {state:?}");
        }

        let renderer = render::Renderer::new(window.clone())?;
        let ground_y = ground_line(d, macos::primary_visible_frame(), primary_height);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos() as u64)
            .unwrap_or(1);
        let mut figure = stickman::Figure::new((d.width / 2.0) as f32, FIGURE_SCALE, seed);
        if std::env::var_os("STYK_HOLD_IDLE").is_some() {
            figure.hold_idle();
        }

        // Single machine-readable line; the integration tests parse it.
        log::info!(
            "overlay up: window_number={} frame={},{},{}x{} level={} behavior={:#x} \
             click_through={} scale={} ground_y={} figure_scale={}",
            state.window_number,
            d.x,
            d.y,
            d.width,
            d.height,
            state.level,
            state.collection_behavior,
            state.ignores_mouse_events,
            window.scale_factor(),
            ground_y,
            FIGURE_SCALE,
        );

        let now = Instant::now();
        self.scene = Some(Scene {
            renderer,
            figure,
            ground_y,
            last_frame: now,
            next_frame: now,
            drawn: None,
        });
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.scene.is_some() {
            return;
        }
        if let Err(err) = self.create_overlay(event_loop) {
            log::error!("{err:#}");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::Resized(size) => scene.renderer.resize(size.width, size.height),
            // Only sent by AppKit itself (e.g. on first show); our frames come
            // from the timer in `about_to_wait`.
            WindowEvent::RedrawRequested => scene.frame(self.display.width as f32),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let now = Instant::now();
        if now >= scene.next_frame {
            // Render directly instead of `window.request_redraw()`: on macOS that
            // marks the full-screen NSView dirty and runs an AppKit display pass
            // every frame, which alone cost ~14% CPU at 60 fps. Metal presents
            // straight to the layer, so AppKit doesn't need to be involved.
            scene.frame(self.display.width as f32);
            // Adaptive rate: 60 fps while moving, 20 fps for idle breathing.
            let interval = Duration::from_secs_f64(1.0 / f64::from(scene.figure.wanted_fps()));
            scene.next_frame += interval;
            if scene.next_frame < now {
                scene.next_frame = now + interval; // fell behind; don't burst
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(scene.next_frame));
    }
}

/// The display carrying the menu bar, in global top-left-origin points — the same
/// space winit's `LogicalPosition` uses on macOS.
fn main_display_rect() -> Rect {
    let b = core_graphics::display::CGDisplay::main().bounds();
    Rect {
        x: b.origin.x,
        y: b.origin.y,
        width: b.size.width,
        height: b.size.height,
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let click_through = click_through_from_env(std::env::var("STYK_CLICK_THROUGH").ok().as_deref());
    if !click_through {
        log::warn!("STYK_CLICK_THROUGH=0: overlay will block mouse input to the whole screen");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("styk-worker")
        .enable_all()
        .build()
        .context("failed to start tokio runtime")?;

    let event_loop = {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        EventLoop::builder()
            // Accessory: no Dock icon, no app menu, and the window may float over
            // other apps' full-screen Spaces.
            .with_activation_policy(ActivationPolicy::Accessory)
            // winit activates the app on launch by default, which would steal
            // keyboard focus from whatever the user is doing.
            .with_activate_ignoring_other_apps(false)
            .with_default_menu(false)
            .build()
            .context("failed to create event loop")?
    };

    let mut app = App {
        scene: None,
        display: main_display_rect(),
        click_through,
        _runtime: runtime,
    };
    event_loop.run_app(&mut app).context("event loop error")?;
    Ok(())
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
    fn primary_display_maps_to_origin() {
        let d = Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1169.0,
        };
        assert_eq!(cg_to_cocoa(d, 1169.0), d);
    }

    #[test]
    fn display_below_primary_gets_negative_cocoa_y() {
        let d = Rect {
            x: 0.0,
            y: 1169.0,
            width: 1920.0,
            height: 1080.0,
        };
        let c = cg_to_cocoa(d, 1169.0);
        assert_eq!(
            (c.x, c.y, c.width, c.height),
            (0.0, -1080.0, 1920.0, 1080.0)
        );
    }

    #[test]
    fn display_above_primary_gets_positive_cocoa_y() {
        let d = Rect {
            x: -200.0,
            y: -900.0,
            width: 1600.0,
            height: 900.0,
        };
        let c = cg_to_cocoa(d, 1169.0);
        assert_eq!((c.x, c.y), (-200.0, 1169.0));
    }

    #[test]
    fn conversion_round_trips() {
        let d = Rect {
            x: 37.5,
            y: -412.25,
            width: 1280.0,
            height: 720.0,
        };
        assert_eq!(cg_to_cocoa(cg_to_cocoa(d, 982.0), 982.0), d);
    }

    #[test]
    fn ground_sits_on_top_of_bottom_dock() {
        let d = Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1169.0,
        };
        // Dock 80 pt tall at the bottom, menu bar 37 pt at the top (AppKit coords).
        let visible = Rect {
            x: 0.0,
            y: 80.0,
            width: 1800.0,
            height: 1169.0 - 80.0 - 37.0,
        };
        assert_eq!(ground_line(d, Some(visible), 1169.0), 1089.0);
    }

    #[test]
    fn ground_falls_back_to_screen_bottom() {
        let d = Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1169.0,
        };
        // Hidden / side Dock: visible area reaches the bottom.
        let visible = Rect {
            x: 70.0,
            y: 0.0,
            width: 1730.0,
            height: 1132.0,
        };
        assert_eq!(ground_line(d, Some(visible), 1169.0), 1169.0);
        assert_eq!(ground_line(d, None, 1169.0), 1169.0);
    }

    #[test]
    fn idle_breathing_skips_most_frames_but_walking_never_does() {
        use stickman::{Figure, State};
        // Presents per second over `secs`, after a 1 s settle. Walking lasts at
        // least 5 s from launch, so the walk sample stays inside it.
        let count_presents = |idle: bool, secs: u32| {
            let mut f = Figure::new(500.0, FIGURE_SCALE, 11);
            if idle {
                f.hold_idle();
            }
            for _ in 0..60 {
                f.update(1.0 / 60.0, 0.0, 1.0e6);
            }
            let fps = f.wanted_fps();
            let mut drawn = f.segments(800.0);
            let mut presents = 0;
            for _ in 0..fps * secs {
                f.update(1.0 / fps as f32, 0.0, 1.0e6);
                let s = f.segments(800.0);
                if max_joint_shift(&drawn, &s) >= MIN_VISIBLE_SHIFT_PT {
                    presents += 1;
                    drawn = s;
                }
            }
            assert_eq!(f.state == State::Idle, idle);
            presents as f32 / secs as f32
        };
        let (idle, walk) = (count_presents(true, 10), count_presents(false, 3));
        println!("presents/s: idle {idle}, walk {walk}");
        assert!(idle < 12.0, "idle presents {idle}/s");
        assert!(walk > 55.0, "walk presents only {walk}/s");
    }

    #[test]
    fn click_through_env_parsing() {
        assert!(click_through_from_env(None));
        assert!(click_through_from_env(Some("1")));
        assert!(click_through_from_env(Some("")));
        assert!(click_through_from_env(Some("yes")));
        for off in ["0", "false", "FALSE", " off ", "No"] {
            assert!(!click_through_from_env(Some(off)), "{off:?} should disable");
        }
    }
}
