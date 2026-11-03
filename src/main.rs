//! Stickman: a transparent, click-through, always-on-top window
//! covering the main display, with an AI-directed stick figure living on it.
//!
//! Threads (claude.md rule 1):
//! - **main**: winit/AppKit event loop, fixed-step simulation, wgpu rendering.
//! - **stickman-windows**: polls the window server for app windows (platforms).
//! - **stickman-ocr**: Vision text detection on the active window (text platforms).
//! - **stickman-os**: Accessibility IPC (window shakes), which can block.
//! - **tokio**: the AI director talking to Ollama.
//!
//! Environment (diagnostics and opt-outs):
//! - `STICKMAN_MODE=IDLE|OBSERVE|SABOTAGE|COMBAT`: force a mode; the director is off.
//! - `STICKMAN_HOLD_IDLE=1`: stand still (used to measure idle CPU).
//! - `STICKMAN_CLICK_THROUGH=0`: the overlay catches every click (blocks the screen).
//! - `STICKMAN_NO_CURSOR_PUSH=1`, `STICKMAN_NO_WINDOW_SHAKE=1`, `STICKMAN_NO_OCR=1`: disable
//!   the effects that touch the user's cursor, windows or screen contents.
//! - `STICKMAN_NO_PROMPTS=1`: don't show macOS permission dialogs.
//! - `STICKMAN_OLLAMA_URL`, `STICKMAN_OLLAMA_MODEL`, `STICKMAN_DIRECTOR_INTERVAL_S`: director.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use stickman::agent::{Agent, Event, Mode, Senses, FIXED_DT};
use stickman::director::{self, Directive, Observation};
use stickman::geom::{cg_to_cocoa, v2, Rect, V2};
use stickman::input::Cursor;
use stickman::macos::{
    self, OsAction, OverlayWindow, OVERLAY_COLLECTION_BEHAVIOR, OVERLAY_WINDOW_LEVEL,
};
use stickman::menubar::{MenuBar, Status};
use stickman::ocr;
use stickman::particles::Particles;
use stickman::physics::{window_platforms, Platform, World};
use stickman::render::{DrawList, Renderer};
use stickman::settings::{Behaviour, Command, Settings};
use stickman::stickman::{Segment, BONES, WALKER_RGB};
use stickman::windows::{self, Snapshot};
use stickman::words::{WordEvent, WordPlay};
use tokio::sync::watch;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

#[cfg(not(target_os = "macos"))]
compile_error!("stickman targets macOS (aarch64-apple-darwin) only");

/// Default figure scale (Settings: Medium): the ~250 pt rig becomes ~140 pt.
#[cfg(test)]
const FIGURE_SCALE: f32 = 0.55;
/// Keep the figure this far (pt) from the screen edges.
const EDGE_MARGIN: f32 = 30.0;
/// Simulation and input sampling rate (PRD: 60 fps, < 50 ms reactions).
const TICK: Duration = Duration::from_nanos(1_000_000_000 / 60);
/// Tick interval while nothing is happening (still < 50 ms reaction time).
const IDLE_TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);
/// Smallest joint movement worth presenting a frame for: half a Retina pixel.
const MIN_VISIBLE_SHIFT_PT: f32 = 0.25;
/// Re-read the active window's text this often while it stays put.
const OCR_INTERVAL: Duration = Duration::from_secs(12);
/// The active window must sit still this long before it is read.
const OCR_SETTLE: Duration = Duration::from_secs(1);
/// How far a punch shoves the cursor, points.
const CURSOR_PUSH: f32 = 70.0;

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// Click-through defaults to on; only an explicit `0`/`false`/`off`/`no` disables it.
fn click_through_from_env(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

/// Ground line for the figure, in overlay points (y down from the display top):
/// the top edge of the Dock, or the bottom of the screen if the Dock is hidden or
/// on the side.
fn ground_line(display: Rect, visible_cocoa: Option<Rect>, primary_height: f64) -> f32 {
    let bottom = display.height;
    let ground = visible_cocoa
        .map(|v| cg_to_cocoa(v, primary_height))
        .map(|v| v.y + v.height - display.y)
        .filter(|g| *g > display.height * 0.5 && *g <= bottom)
        .unwrap_or(bottom);
    ground as f32
}

/// Largest distance any joint moved between two placements of the figure.
fn max_joint_shift(a: &[Segment], b: &[Segment]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(a, b)| [(a.start, b.start), (a.end, b.end)])
        .map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt())
        .fold(0.0, f32::max)
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

// The instance buffer must fit the figure (outline + fill), every particle and
// the health bar.
const _: () =
    assert!(stickman::render::MAX_INSTANCES >= 2 * BONES + stickman::particles::MAX_PARTICLES + 4);

/// How long the user must hover over the figure before clicks are intercepted.
const AIM_DWELL: Duration = Duration::from_millis(120);

/// Tracks whether the user is deliberately pointing at the figure, so clicks
/// meant for their own app aren't stolen when the figure wanders under a parked
/// cursor (e.g. while stomping on the window they're working in).
#[derive(Debug, Default, Clone, Copy)]
struct ClickIntent {
    hover_since: Option<Instant>,
    aimed: bool,
}

impl ClickIntent {
    /// Update with this tick's state; returns whether to intercept clicks.
    fn update(&mut self, over: bool, cursor_moved: bool, now: Instant) -> bool {
        if !over {
            *self = ClickIntent::default();
            return false;
        }
        let since = *self.hover_since.get_or_insert(now);
        // Only the user moving the pointer onto/over the figure counts as aiming.
        self.aimed |= cursor_moved;
        self.aimed && now.duration_since(since) >= AIM_DWELL
    }
}

/// Minimum time in a mode before the director may change it, so behaviour reads
/// as a mood rather than jitter.
const MIN_MODE_DWELL: Duration = Duration::from_secs(12);

/// Whether to switch from `current` to `proposed` now. Retaliation (COMBAT after
/// being hit) is always allowed immediately.
fn accept_directive(current: Mode, proposed: Mode, in_mode: Duration, recently_hit: bool) -> bool {
    current != proposed && (in_mode >= MIN_MODE_DWELL || (proposed == Mode::Combat && recently_hit))
}

/// What was last presented, to skip frames that would look identical.
struct Shown {
    segments: [Segment; BONES],
    had_effects: bool,
}

struct Scene {
    overlay: OverlayWindow,
    renderer: Renderer,
    world: World,
    agent: Agent,
    particles: Particles,
    cursor: Cursor,
    display: Rect,
    ground: Platform,
    // Window and text platforms.
    windows_rx: Receiver<Snapshot>,
    snapshot: Snapshot,
    ocr_tx: Option<Sender<ocr::Job>>,
    ocr_rx: Receiver<ocr::Output>,
    words: WordPlay,
    ocr_target: Option<ocr::Request>,
    ocr_candidate: Option<ocr::Request>,
    ocr_candidate_since: Instant,
    ocr_sent: Instant,
    text: Vec<Platform>,
    // AI director.
    directive_rx: Receiver<Directive>,
    obs_tx: watch::Sender<Observation>,
    mode: Mode,
    mode_since: Instant,
    director_active: Arc<AtomicBool>,
    director_model: String,
    last_reason: String,
    director_online: Option<bool>,
    // Settings and the menu bar.
    settings: Settings,
    settings_path: Option<PathBuf>,
    menubar: MenuBar,
    menubar_logged: bool,
    accessibility: bool,
    screen_recording: bool,
    seed: u64,
    // Effects on the user's desktop.
    os_tx: Sender<OsAction>,
    force_catch_clicks: bool,
    // Input bookkeeping.
    pending_click: bool,
    /// App to give focus back to after a click on the figure activated us.
    focus_restore: Option<(i64, Instant)>,
    intent: ClickIntent,
    hits: VecDeque<Instant>,
    last_mouse_move: Instant,
    last_cursor: V2,
    // Timing.
    last_tick: Instant,
    next_tick: Instant,
    accumulator: f32,
    shown: Option<Shown>,
    draw: DrawList,
    frames: u64,
    /// Something is moving: tick at the full rate.
    busy: bool,
    last_ammo: Option<u64>,
    // Debug statistics.
    ticks: u64,
    stats_frames: u64,
    stats_at: Instant,
}

impl Scene {
    fn platforms(&self) -> Vec<Platform> {
        let wins: Vec<(u32, Rect)> = self
            .snapshot
            .windows
            .iter()
            .map(|w| (w.id, w.rect))
            .collect();
        let mut ps = vec![self.ground];
        ps.extend(window_platforms(&wins, self.display.width as f32, 40.0));
        ps.extend(self.text.iter().copied());
        ps
    }

    fn sync_world(&mut self) {
        let mut changed = false;
        while let Ok(s) = self.windows_rx.try_recv() {
            self.snapshot = s;
            changed = true;
        }
        let active = self.snapshot.active().map(|w| ocr::Request {
            id: w.id,
            rect: w.rect,
        });
        if changed && active != self.ocr_target {
            // The active window moved or changed: its old text lines are stale.
            self.text.clear();
        }
        // OCR results for the window that is still active, unmoved; and word grabs.
        while let Ok(out) = self.ocr_rx.try_recv() {
            match out {
                ocr::Output::Text(r) => {
                    if active
                        == Some(ocr::Request {
                            id: r.id,
                            rect: r.rect,
                        })
                    {
                        let mut lines = ocr::text_platforms(r.id, r.rect, &r.lines);
                        lines.truncate(120);
                        log::debug!(
                            "ocr: {} text platforms, {} words on window {}",
                            lines.len(),
                            r.words.len(),
                            r.id
                        );
                        if self.settings.throw_words {
                            let before = self.words.holes.len();
                            self.words
                                .set_words(r.id, &ocr::words_in(r.rect, &r.words), &lines);
                            if self.words.holes.len() != before {
                                self.shown = None; // a hole healed
                            }
                        }
                        self.text = lines;
                    }
                }
                ocr::Output::Grabbed(g) => self.word_captured(g),
            }
        }
        // Debounce: only read a window once it has stopped moving for a second
        // (a drag would otherwise trigger an expensive Vision pass per frame).
        if active != self.ocr_candidate {
            self.ocr_candidate = active;
            self.ocr_candidate_since = Instant::now();
        }
        let settled = self.ocr_candidate_since.elapsed() >= OCR_SETTLE;
        if !self.settings.walk_on_text {
            self.text.clear();
        }
        if !(self.settings.walk_on_text && self.settings.throw_words) {
            self.words.clear_words();
        }
        // Holes only belong over their own, unmoved, frontmost window.
        if self
            .words
            .validate_holes(self.snapshot.active().map(|w| (w.id, w.rect)))
        {
            self.shown = None;
        }
        // Re-read faster while words are missing, to notice scrolling.
        let interval = if self.words.holes.is_empty() {
            OCR_INTERVAL
        } else {
            Duration::from_secs(3)
        };
        if let (Some(tx), Some(req), true, true) =
            (&self.ocr_tx, active, settled, self.settings.walk_on_text)
        {
            if self.ocr_target != Some(req) || self.ocr_sent.elapsed() >= interval {
                let _ = tx.send(ocr::Job::Read(req));
                self.ocr_target = Some(req);
                self.ocr_sent = Instant::now();
            }
        }
        if self.world.set_platforms(self.platforms()) {
            self.agent.on_platforms_changed(&self.world);
        }
    }

    fn sync_director(&mut self) {
        let forced = self.settings.behaviour.forced_mode();
        if let Some(m) = forced {
            if m != self.mode {
                self.mode = m;
                self.mode_since = Instant::now();
            }
        }
        self.director_active.store(
            forced.is_none() && self.settings.visible && !self.settings.paused,
            Ordering::Relaxed,
        );
        while let Ok(d) = self.directive_rx.try_recv() {
            self.director_online = Some(d.source == director::Source::Llm);
            self.last_reason = d.reason.clone();
            let recently_hit = self
                .hits
                .back()
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10));
            if forced.is_none()
                && d.mode != self.mode
                && accept_directive(self.mode, d.mode, self.mode_since.elapsed(), recently_hit)
            {
                log::info!(
                    "mode: {} -> {} ({})",
                    self.mode.as_str(),
                    d.mode.as_str(),
                    d.reason
                );
                self.mode = d.mode;
                self.mode_since = Instant::now();
            }
        }
        let now = Instant::now();
        while self
            .hits
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            self.hits.pop_front();
        }
        let v = self.cursor.velocity();
        self.obs_tx.send_replace(Observation {
            active_app_name: self
                .snapshot
                .active()
                .map(|w| w.owner.clone())
                .unwrap_or_default(),
            mouse_velocity_x: v.x.round(),
            mouse_velocity_y: v.y.round(),
            mouse_speed_avg: self.cursor.mean_speed().round(),
            agent_health: self.agent.health.max(0.0).round(),
            agent_frustration: self.agent.frustration.round(),
            current_state: self.mode.as_str(),
            seconds_in_state: self.mode_since.elapsed().as_secs_f32().round(),
            hits_last_minute: self.hits.len() as u32,
            user_idle_seconds: self.last_mouse_move.elapsed().as_secs_f32().round(),
        });
    }

    /// A grabbed word's pixels arrived from the OCR worker.
    fn word_captured(&mut self, g: ocr::GrabResult) {
        let (sprite, bg) = match &g.cutout {
            Some(c) => (
                self.renderer.create_sprite(&c.rgba, c.width, c.height),
                c.background,
            ),
            None => (None, [0; 3]),
        };
        let window_rect = self.snapshot.active().map(|w| w.rect).unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
        let hands = self.agent.hands();
        if self
            .words
            .captured(g.id, sprite, bg, window_rect, self.agent.holding, hands)
        {
            log::info!("event: ripped a word off the screen");
            self.shown = None;
        } else {
            log::debug!("grab fizzled (busy background or capture failed)");
            if let Some(id) = sprite {
                self.renderer.free_sprite(id);
            }
            self.agent.holding = false;
        }
    }

    fn handle_word_events(&mut self, events: &[WordEvent], freed: &[u32]) {
        let size = v2(self.display.width as f32, self.display.height as f32);
        for e in events {
            match *e {
                WordEvent::HitCursor { at, dir } => {
                    self.particles.sparks(at, dir, 22);
                    if self.settings.cursor_push {
                        self.cursor.push(dir * CURSOR_PUSH, size);
                    }
                    log::info!("event: a thrown word hit the cursor");
                }
                WordEvent::Landed { at } => self.particles.dust(at, 0.25),
            }
        }
        for id in freed {
            self.renderer.free_sprite(*id);
        }
    }

    fn handle(&mut self, events: &[Event]) {
        let size = v2(self.display.width as f32, self.display.height as f32);
        for e in events {
            match *e {
                Event::Knockback { at, dir } => {
                    self.particles.explosion(at, -dir);
                    self.hits.push_back(Instant::now());
                    log::info!("event: knockback (health {:.0})", self.agent.health);
                }
                Event::StruckCursor { at, dir } => {
                    self.particles.sparks(at, dir, 18);
                    if self.settings.cursor_push {
                        self.cursor.push(dir * CURSOR_PUSH, size);
                    }
                    log::info!("event: struck cursor");
                }
                Event::Dust { at, strength } => self.particles.dust(at, strength),
                Event::Jumped { at } => self.particles.dust(at, 0.2),
                Event::Stomp { at, window } => {
                    self.particles.dust(at, 0.5);
                    if self.settings.window_shake && self.accessibility {
                        if let Some(w) = self.snapshot.windows.iter().find(|w| w.id == window) {
                            let dx = if self.frames % 2 == 0 { 9.0 } else { -9.0 };
                            let _ = self
                                .os_tx
                                .send(OsAction::ShakeFocusedWindow { pid: w.pid, dx });
                        }
                    }
                    log::info!("event: stomp on window {window}");
                }
                Event::Grabbed { id } => match self.words.grabbed(id) {
                    Some(need) => {
                        let global = Rect {
                            x: need.rect.x + self.display.x,
                            y: need.rect.y + self.display.y,
                            ..need.rect
                        };
                        let job = ocr::GrabRequest {
                            id,
                            window: need.window,
                            below: self.overlay.state().window_number as u32,
                            global,
                        };
                        if let Some(tx) = &self.ocr_tx {
                            let _ = tx.send(ocr::Job::Grab(job));
                        }
                    }
                    None => self.agent.holding = false, // nothing there after all
                },
                Event::Threw { from, vel } => self.words.threw(from, vel),
                Event::Dropped { at } => self.words.dropped(at),
                Event::KnockedOut { .. } => {
                    self.particles.stars(self.agent.head());
                    log::info!("event: knocked out");
                }
            }
        }
    }

    /// Apply menu picks. Returns `false` when the user chose Quit.
    fn handle_commands(&mut self) -> bool {
        for cmd in self.menubar.drain_commands() {
            log::info!("menu: {cmd:?}");
            match cmd {
                Command::Quit => return false,
                Command::Respawn => {
                    let x = (self.display.width / 2.0) as f32;
                    let hold = self.agent.hold_still;
                    self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    self.agent = Agent::new(x, self.ground, self.settings.size.scale(), self.seed);
                    self.agent.hold_still = hold;
                    self.particles.dust(v2(x, self.ground.y), 0.8);
                }
                Command::OpenAccessibilitySettings => {
                    macos::open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
                }
                Command::OpenScreenRecordingSettings => {
                    macos::open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture");
                }
                _ => {}
            }
            if self.settings.apply(cmd) {
                self.agent.scale = self.settings.size.scale();
                if matches!(cmd, Command::SetBehaviour(Behaviour::Auto)) {
                    // Let the director's next decision apply immediately.
                    self.mode_since = Instant::now() - MIN_MODE_DWELL;
                }
                self.shown = None; // redraw (size/visibility changed)
                if let Some(path) = &self.settings_path {
                    if let Err(e) = self.settings.save(path) {
                        log::warn!("settings: could not save {}: {e:#}", path.display());
                    }
                }
            }
        }
        true
    }

    fn sync_menubar(&mut self) {
        let director = if self.settings.behaviour != Behaviour::Auto {
            format!("{} — paused (behaviour pinned)", self.director_model)
        } else {
            match self.director_online {
                Some(true) => format!("{} — online", self.director_model),
                Some(false) => "Ollama offline — built-in director".to_string(),
                None => format!("{} — starting…", self.director_model),
            }
        };
        let status = Status {
            mode: self.mode,
            reason: self.last_reason.clone(),
            health: self.agent.health.max(0.0).round(),
            director,
            accessibility: self.accessibility,
            screen_recording: self.screen_recording,
        };
        self.menubar.sync(&self.settings, &status);
        if !self.menubar_logged && self.stats_at.elapsed() > Duration::from_secs(1) {
            // Report once the menu bar has laid the icon out.
            let (shown, f) = self.menubar.placement();
            log::info!(
                "menubar: visible={shown} frame={},{},{}x{}",
                f[0],
                f[1],
                f[2],
                f[3]
            );
            self.menubar_logged = true;
            if env_flag("STICKMAN_DEBUG_OPEN_MENU") {
                self.menubar.open();
            }
        }
    }

    /// One frame. Returns `false` when the app should quit.
    fn tick(&mut self) -> bool {
        if !self.handle_commands() {
            return false;
        }
        self.sync_menubar();
        if !self.settings.visible || self.settings.paused {
            // Frozen or hidden: no simulation, never intercept clicks, and draw
            // nothing (or keep the last frame when paused).
            self.overlay.set_click_through(true);
            self.busy = false;
            self.last_tick = Instant::now();
            self.accumulator = 0.0;
            if !self.settings.visible && self.shown.is_some() {
                self.draw.clear();
                if self.renderer.render(&self.draw) {
                    self.shown = None;
                }
            } else if self.settings.visible && self.shown.is_none() {
                self.present();
            }
            return true;
        }
        self.cursor.sample();
        let cursor_moved = (self.cursor.pos - self.last_cursor).length() > 0.5;
        if cursor_moved {
            self.last_mouse_move = Instant::now();
            self.last_cursor = self.cursor.pos;
        }
        self.sync_world();
        self.sync_director();
        // Clicking the figure activates our app; give focus back to the user's app
        // as soon as that happens (retrying each tick for up to a second).
        if let Some((pid, since)) = self.focus_restore {
            if macos::app_is_active() {
                macos::restore_focus(pid);
            } else if since.elapsed() > Duration::from_millis(300) {
                self.focus_restore = None;
            }
            if since.elapsed() > Duration::from_secs(1) {
                self.focus_restore = None;
            }
        }

        let now = Instant::now();
        self.accumulator += now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        let mut events = Vec::new();
        let mut word_events = Vec::new();
        let mut freed = Vec::new();
        let wants_ammo = self.settings.throw_words
            && self.settings.walk_on_text
            && matches!(self.mode, Mode::Combat | Mode::Sabotage);
        while self.accumulator >= FIXED_DT {
            let ammo = if wants_ammo && !self.agent.holding {
                let active = self.snapshot.active().map(|w| w.id);
                self.words
                    .ammo(FIXED_DT, self.agent.pos, self.agent.ground, active)
            } else {
                None
            };
            if ammo.map(|a| a.id) != self.last_ammo {
                log::debug!(
                    "ammo: {ammo:?} (figure at {:?}, ground {:?})",
                    self.agent.pos,
                    self.agent.ground.map(|g| (g.kind, g.y))
                );
                self.last_ammo = ammo.map(|a| a.id);
            }
            let senses = Senses {
                cursor: self.cursor.pos,
                cursor_vel: self.cursor.velocity(),
                clicked: std::mem::take(&mut self.pending_click),
                mode: self.mode,
                active_window: self.snapshot.active().map(|w| (w.id, w.rect)),
                min_x: EDGE_MARGIN,
                max_x: self.display.width as f32 - EDGE_MARGIN,
                ammo,
            };
            self.agent.update(&senses, &self.world, &mut events);
            self.particles.update(FIXED_DT);
            let (we, fr) = self.words.update(
                FIXED_DT,
                self.cursor.pos,
                &self.world,
                EDGE_MARGIN,
                self.display.width as f32 - EDGE_MARGIN,
            );
            word_events.extend(we);
            freed.extend(fr);
            self.accumulator -= FIXED_DT;
        }
        self.handle(&events);
        self.handle_word_events(&word_events, &freed);

        // PRD 3.1: intercept clicks only while the user is aiming at the figure.
        let over = self.agent.hit_test(self.cursor.pos);
        let catch = self.intent.update(over, cursor_moved, Instant::now());
        let through = !(catch || self.force_catch_clicks);
        if through != self.overlay.click_through() {
            log::debug!(
                "click-through {through} (cursor {:?}, over {over})",
                self.cursor.pos
            );
        }
        self.overlay.set_click_through(through);

        self.ticks += 1;
        if self.stats_at.elapsed() >= Duration::from_secs(5) {
            log::debug!(
                "perf: {} ticks, {} presents in {:.1}s; action {:?}, platforms {}, active {:?}, words {}, holes {}",
                self.ticks,
                self.frames - self.stats_frames,
                self.stats_at.elapsed().as_secs_f32(),
                self.agent.action,
                self.world.platforms().len(),
                self.snapshot.active().map(|w| (w.id, w.owner.as_str())),
                self.words.words().len(),
                self.words.holes.len()
            );
            self.ticks = 0;
            self.stats_frames = self.frames;
            self.stats_at = Instant::now();
        }
        self.busy = cursor_moved
            || !self.particles.is_empty()
            || self.agent.since_hit() < 4.0
            || !self.agent.action.is_still()
            || self.words.animating();
        self.present();
        true
    }

    fn present(&mut self) {
        let segments = self.agent.segments();
        let effects =
            !self.particles.is_empty() || self.agent.since_hit() < 4.0 || self.words.animating();
        if let Some(shown) = &self.shown {
            if !effects
                && !shown.had_effects
                && max_joint_shift(&shown.segments, &segments) < MIN_VISIBLE_SHIFT_PT
            {
                return; // would look identical: skip the ~2 ms present
            }
        }
        self.draw.clear();
        // Holes (painted page colour) go under everything; words as sprites on top.
        let hands = self.agent.holding.then(|| self.agent.hands());
        self.words.draw(&mut self.draw, hands);
        self.draw.figure(&segments, WALKER_RGB);
        self.particles.draw(&mut self.draw);
        self.health_bar();
        if !self.renderer.render(&self.draw) {
            // Not on screen (occluded/timeout): don't record it, or a still figure
            // might never be drawn at all.
            return;
        }
        self.frames += 1;
        self.shown = Some(Shown {
            segments,
            had_effects: effects,
        });
    }

    /// Small health bar over the head for a few seconds after a hit.
    fn health_bar(&mut self) {
        let since = self.agent.since_hit();
        if since >= 4.0 {
            return;
        }
        let alpha = (1.0 - (since - 3.0).max(0.0)).clamp(0.0, 1.0);
        let head = self.agent.head();
        let (w, y) = (44.0, head.y - 26.0);
        let left = v2(head.x - w / 2.0, y);
        let frac = (self.agent.health / 100.0).clamp(0.0, 1.0);
        let color = if frac > 0.5 {
            [0.3, 0.85, 0.4, alpha]
        } else if frac > 0.25 {
            [1.0, 0.75, 0.2, alpha]
        } else {
            [1.0, 0.25, 0.2, alpha]
        };
        self.draw
            .capsule(left, left + v2(w, 0.0), 3.5, [0.1, 0.1, 0.1, 0.75 * alpha]);
        if frac > 0.0 {
            self.draw
                .capsule(left, left + v2(w * frac, 0.0), 2.2, color);
        }
    }
}

struct App {
    scene: Option<Scene>,
    display: Rect,
    click_through: bool,
    runtime: tokio::runtime::Runtime,
}

impl App {
    fn create_overlay(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        use winit::platform::macos::WindowAttributesExtMacOS;

        let d = self.display;
        let attributes = WindowAttributes::default()
            .with_title("Stickman")
            .with_transparent(true)
            .with_decorations(false) // borderless + frameless
            .with_resizable(false)
            .with_active(false) // never steal focus from the user's app
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(LogicalPosition::new(d.x, d.y))
            .with_inner_size(LogicalSize::new(d.width, d.height))
            .with_has_shadow(false);
        let window: Arc<Window> = Arc::new(
            event_loop
                .create_window(attributes)
                .context("failed to create overlay window")?,
        );

        // AppKit-level overlay configuration (click-through, Spaces, level, frame).
        let primary_height = main_display_rect().height;
        let mut overlay = OverlayWindow::new(window.clone())?;
        overlay.configure(cg_to_cocoa(d, primary_height), self.click_through);
        let state = overlay.state();
        if state.ignores_mouse_events != self.click_through
            || state.collection_behavior & OVERLAY_COLLECTION_BEHAVIOR
                != OVERLAY_COLLECTION_BEHAVIOR
            || state.level != OVERLAY_WINDOW_LEVEL
        {
            log::warn!("AppKit did not apply every overlay property: {state:?}");
        }

        let renderer = Renderer::new(window.clone())?;
        let ground_y = ground_line(d, macos::primary_visible_frame(), primary_height);
        let ground = Platform::ground(d.width as f32, ground_y);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos() as u64)
            .unwrap_or(1);
        // Settings: saved file, then environment overrides (not saved unless the
        // user changes something in the menu).
        let settings_path = Settings::default_path();
        let mut settings = settings_path
            .as_deref()
            .map(Settings::load)
            .unwrap_or_default();
        if let Some(m) = std::env::var("STICKMAN_MODE")
            .ok()
            .and_then(|m| Mode::parse(&m))
        {
            settings.behaviour = Behaviour::from_mode(m);
        } else if std::env::var("STICKMAN_MODE").is_ok_and(|m| m.trim().is_empty()) {
            settings.behaviour = Behaviour::Auto;
        }
        if env_flag("STICKMAN_NO_CURSOR_PUSH") {
            settings.cursor_push = false;
        }
        if env_flag("STICKMAN_NO_WINDOW_SHAKE") {
            settings.window_shake = false;
        }
        if env_flag("STICKMAN_NO_OCR") {
            settings.walk_on_text = false;
        }
        settings.paused = false;
        let mut agent = Agent::new((d.width / 2.0) as f32, ground, settings.size.scale(), seed);
        agent.hold_still = env_flag("STICKMAN_HOLD_IDLE");

        // Permissions: Accessibility (window shakes) and Screen Recording (OCR).
        let prompts = !env_flag("STICKMAN_NO_PROMPTS");
        let ax = macos::accessibility_trusted(prompts);
        let ocr_wanted = settings.walk_on_text;
        let mut ocr_allowed = ocr_wanted && ocr::screen_capture_allowed();
        if ocr_wanted && !ocr_allowed && prompts {
            ocr_allowed = ocr::request_screen_capture();
        }
        if !ax {
            log::warn!(
                "Accessibility not granted: stomps won't shake windows \
                 (System Settings → Privacy & Security → Accessibility)"
            );
        }
        if !ocr_allowed {
            log::warn!("Screen Recording not granted or OCR disabled: no walking on text");
        }

        // Background workers.
        let own_pid = i64::from(std::process::id());
        let (win_tx, windows_rx) = mpsc::channel();
        windows::spawn_poller(own_pid, d, Duration::from_millis(250), win_tx);
        let (ocr_req_tx, ocr_req_rx) = mpsc::channel();
        let (ocr_res_tx, ocr_rx) = mpsc::channel();
        let ocr_tx = if ocr_allowed {
            ocr::spawn_worker(ocr_req_rx, ocr_res_tx);
            Some(ocr_req_tx)
        } else {
            None
        };
        let (dir_tx, directive_rx) = mpsc::channel();
        let (obs_tx, obs_rx) = watch::channel(Observation::default());
        let forced_mode = settings.behaviour.forced_mode();
        let director_cfg = director::Config::from_env();
        let director_model = director_cfg.model.clone();
        let director_active = Arc::new(AtomicBool::new(forced_mode.is_none()));
        if !agent.hold_still {
            director::spawn(
                &self.runtime,
                director_cfg,
                obs_rx,
                dir_tx,
                director_active.clone(),
            );
        }
        let mode = forced_mode.unwrap_or(Mode::Idle);
        let menubar = MenuBar::new();

        // Single machine-readable line; the integration tests parse it.
        log::info!(
            "overlay up: window_number={} frame={},{},{}x{} level={} behavior={:#x} click_through={} \
             scale={} ground_y={} figure_scale={} mode={} accessibility={} ocr={}",
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
            settings.size.scale(),
            mode.as_str(),
            ax,
            ocr_allowed,
        );

        let now = Instant::now();
        let origin = v2(d.x as f32, d.y as f32);
        self.scene = Some(Scene {
            overlay,
            renderer,
            world: World::new(),
            agent,
            particles: Particles::new(seed ^ 0xA5A5),
            cursor: Cursor::new(origin),
            display: d,
            ground,
            windows_rx,
            snapshot: Snapshot::default(),
            ocr_tx,
            ocr_rx,
            words: WordPlay::new(seed ^ 0x5157),
            ocr_target: None,
            ocr_candidate: None,
            ocr_candidate_since: now,
            ocr_sent: now,
            text: Vec::new(),
            directive_rx,
            obs_tx,
            mode,
            mode_since: now,
            director_active,
            director_model,
            last_reason: String::new(),
            director_online: None,
            settings,
            settings_path,
            menubar,
            menubar_logged: false,
            accessibility: ax,
            screen_recording: ocr_allowed,
            seed,
            os_tx: macos::os_actions(),
            force_catch_clicks: !self.click_through,
            pending_click: false,
            focus_restore: None,
            intent: ClickIntent::default(),
            hits: VecDeque::new(),
            last_mouse_move: now,
            last_cursor: V2::ZERO,
            last_tick: now,
            next_tick: now,
            accumulator: 0.0,
            shown: None,
            draw: DrawList::default(),
            frames: 0,
            busy: true,
            last_ammo: None,
            ticks: 0,
            stats_frames: 0,
            stats_at: now,
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
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Only reaches us while the cursor is over the figure.
                log::debug!("click on figure at {:?}", scene.cursor.pos);
                scene.pending_click = true;
                if let Some(w) = scene.snapshot.active() {
                    scene.focus_restore = Some((w.pid, Instant::now()));
                }
            }
            WindowEvent::Focused(true) => {
                // A click on the figure activated us; hand focus straight back.
                if scene.focus_restore.is_none() {
                    if let Some(w) = scene.snapshot.active() {
                        scene.focus_restore = Some((w.pid, Instant::now()));
                    }
                }
            }
            // AppKit asks for redraws on its own schedule; the Metal layer keeps its
            // contents, so only draw if nothing has been shown yet. Frames come
            // from the timer in `about_to_wait`.
            WindowEvent::RedrawRequested if scene.shown.is_none() => scene.present(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let now = Instant::now();
        if now >= scene.next_tick {
            // Simulate and render straight from the timer: `window.request_redraw()`
            // would mark the full-screen NSView dirty and run an AppKit display
            // pass every frame (~14% CPU by itself).
            if !scene.tick() {
                log::info!("quit from the menu bar");
                event_loop.exit();
                return;
            }
            let interval = if scene.busy { TICK } else { IDLE_TICK };
            scene.next_tick += interval;
            if scene.next_tick < now {
                scene.next_tick = now + interval; // fell behind; don't burst
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(scene.next_tick));
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let click_through =
        click_through_from_env(std::env::var("STICKMAN_CLICK_THROUGH").ok().as_deref());
    if !click_through {
        log::warn!("STICKMAN_CLICK_THROUGH=0: overlay will block mouse input to the whole screen");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("stickman-worker")
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
        runtime,
    };
    event_loop.run_app(&mut app).context("event loop error")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stickman::physics::PlatformKind;

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
    fn clicks_are_caught_only_when_aiming() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        // Figure walks under a parked cursor: never intercept.
        let mut c = ClickIntent::default();
        for n in 0..50 {
            assert!(!c.update(true, false, ms(n * 16)));
        }
        // User moves onto it: intercept after the dwell.
        let mut c = ClickIntent::default();
        assert!(!c.update(true, true, ms(0)));
        assert!(!c.update(true, false, ms(60)));
        assert!(c.update(true, false, ms(130)));
        // Leaving resets everything.
        assert!(!c.update(false, true, ms(140)));
        assert!(!c.update(true, false, ms(300)));
    }

    #[test]
    fn director_changes_respect_dwell_except_retaliation() {
        let s = Duration::from_secs;
        assert!(!accept_directive(Mode::Idle, Mode::Sabotage, s(3), false));
        assert!(accept_directive(Mode::Idle, Mode::Sabotage, s(13), false));
        assert!(accept_directive(Mode::Observe, Mode::Combat, s(1), true));
        assert!(!accept_directive(Mode::Observe, Mode::Combat, s(1), false));
        assert!(!accept_directive(Mode::Combat, Mode::Combat, s(60), true));
    }

    #[test]
    fn env_parsing() {
        assert!(click_through_from_env(None));
        assert!(click_through_from_env(Some("1")));
        for off in ["0", "false", "FALSE", " off ", "No"] {
            assert!(!click_through_from_env(Some(off)), "{off:?} should disable");
        }
    }

    #[test]
    fn idle_breathing_skips_most_frames_but_walking_never_does() {
        use stickman::agent::Action;
        let ground = Platform {
            x0: 0.0,
            x1: 1800.0,
            y: 800.0,
            kind: PlatformKind::Ground,
            owner: 0,
        };
        let mut world = World::new();
        world.set_platforms(vec![ground]);
        let senses = Senses {
            cursor: v2(5000.0, 0.0),
            cursor_vel: V2::ZERO,
            clicked: false,
            mode: Mode::Idle,
            active_window: None,
            min_x: 30.0,
            max_x: 1770.0,
            ammo: None,
        };
        let count = |hold: bool| {
            let mut a = Agent::new(900.0, ground, FIGURE_SCALE, 11);
            a.hold_still = hold;
            if !hold {
                a.action = Action::Walk {
                    to: 1700.0,
                    run: false,
                };
            }
            let mut ev = vec![];
            for _ in 0..60 {
                a.update(&senses, &world, &mut ev);
            }
            let mut drawn = a.segments();
            let mut presents = 0;
            for _ in 0..180 {
                a.update(&senses, &world, &mut ev);
                let s = a.segments();
                if max_joint_shift(&drawn, &s) >= MIN_VISIBLE_SHIFT_PT {
                    presents += 1;
                    drawn = s;
                }
            }
            presents as f32 / 3.0
        };
        let (idle, walk) = (count(true), count(false));
        assert!(idle < 12.0, "idle presents {idle}/s");
        assert!(walk > 55.0, "walk presents only {walk}/s");
    }

    #[test]
    fn draw_list_figure_has_outline_and_fill() {
        let a = Agent::new(100.0, Platform::ground(1000.0, 500.0), FIGURE_SCALE, 1);
        let mut dl = DrawList::default();
        dl.figure(&a.segments(), WALKER_RGB);
        assert_eq!(dl.len(), 2 * BONES);
    }
}
