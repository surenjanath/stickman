//! Stickman: transparent, click-through, always-on-top windows
//! covering every display, with an AI-directed stick figure living on them.
//!
//! Threads (claude.md rule 1):
//! - **main**: winit/AppKit event loop, fixed-step simulation, wgpu rendering.
//! - **stickman-windows**: polls the window server for app windows (platforms).
//! - **stickman-ocr**: Vision text detection on the active window (text platforms).
//! - **stickman-os**: Accessibility IPC (window shakes), which can block.
//! - **stickman-sound**: synthesises sound effects and feeds CoreAudio.
//! - **tokio**: the AI director talking to Ollama.
//!
//! Environment (diagnostics and opt-outs):
//! - `STICKMAN_MODE=IDLE|OBSERVE|SABOTAGE|COMBAT`: force a mode; the director is off.
//! - `STICKMAN_HOLD_IDLE=1`: stand still (used to measure idle CPU).
//! - `STICKMAN_CLICK_THROUGH=0`: the overlay catches every click (blocks the screen).
//! - `STICKMAN_NO_CURSOR_PUSH=1`, `STICKMAN_NO_WINDOW_SHAKE=1` (also no window
//!   shoves or throws), `STICKMAN_NO_OCR=1`: disable the effects that touch the
//!   user's cursor, windows or screen contents.
//! - `STICKMAN_NO_DODGE=1`, `STICKMAN_NO_CLING=1`, `STICKMAN_NO_TOOLS=1`: the
//!   cursor doesn't dodge; the figure doesn't hang on to it; no tools or saw.
//! - `STICKMAN_DEBUG_TACTIC=words|fists|grab|powers|blade`: a fixed fight plan.
//! - `STICKMAN_NO_PROMPTS=1`: don't show macOS permission dialogs.
//! - `STICKMAN_NO_SOUND=1`: no sound effects.
//! - `STICKMAN_NO_INTRO=1`: skip the jail-break intro.
//! - `STICKMAN_OLLAMA_URL`, `STICKMAN_OLLAMA_MODEL`, `STICKMAN_DIRECTOR_INTERVAL_S`: director.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use stickman::agent::{Agent, Dread, Event, Mind, Mode, Senses, Tactic, FIXED_DT};
use stickman::boss::{Blow, Boss, Outcome};
use stickman::director::{self, Directive, Observation};
use stickman::doodle::Doodles;
use stickman::draw::Sketchpad;
use stickman::geom::{cg_to_cocoa, v2, Rect, V2};
use stickman::input::{self as input, Cursor};
use stickman::jail::{Cue, Jail};
use stickman::macos::{
    self, OsAction, OverlayWindow, OVERLAY_COLLECTION_BEHAVIOR, OVERLAY_WINDOW_LEVEL,
};
use stickman::menubar::{MenuBar, Status};
use stickman::mind::{Kind as Memory, Mind as Psyche};
use stickman::minions::{self, Squad};
use stickman::note::{self as notes, Note};
use stickman::ocr;
use stickman::particles::Particles;
use stickman::physics::{window_platforms, Platform, World};
use stickman::render::{DrawList, Renderer};
use stickman::rival::{self, Phase, Rival};
use stickman::screens::{self, Layout, Screen};
use stickman::settings::{Behaviour, Command, Settings};
use stickman::settings_window::SettingsWindow;
use stickman::sound::{Sfx, Sound};
use stickman::speech::{Moment, Priority, Speech};
use stickman::stats::{Deed, Feat, Stats};
use stickman::stickman::{Segment, BONES};
use stickman::war;
use stickman::windows::{self, Snapshot};
use stickman::winfly::{Stage, WindowFlight};
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

/// `STICKMAN_DEBUG_TACTIC=words|fists|grab|powers|blade`: always that plan.
fn debug_tactic() -> Option<Tactic> {
    static T: std::sync::OnceLock<Option<Tactic>> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let want = std::env::var("STICKMAN_DEBUG_TACTIC").ok()?;
        Tactic::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(want.trim()))
    })
}

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

/// Largest distance any joint moved between two placements of the figure.
fn max_joint_shift(a: &[Segment], b: &[Segment]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(a, b)| [(a.start, b.start), (a.end, b.end)])
        .map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt())
        .fold(0.0, f32::max)
}

/// A press that moves this far (points) or lasts this long picks the figure up
/// instead of hitting it.
const DRAG_START_PT: f32 = 6.0;
const DRAG_START_HOLD: Duration = Duration::from_millis(250);

/// The first rival comes this long after launch; later ones every 6-12 min.
const RIVAL_FIRST: Duration = Duration::from_secs(360);

/// Draw mode turns itself off after this long without a stroke.
const DRAW_TIMEOUT: Duration = Duration::from_secs(60);

/// After the jail break, fight the cursor this long (Auto behaviour only).
const INTRO_COMBAT: Duration = Duration::from_secs(15);

/// Capture id for the weapon (words and icons count up from 1).
const WEAPON_ID: u64 = u64::MAX - 1;
/// Capture id for a toolbar it saws off a window.
const SAW_ID: u64 = u64::MAX - 2;
/// Capture ids for a note's torn-off letters: this plus the tile index.
const NOTE_ID: u64 = u64::MAX - 1000;
/// A sword is drawn this long, at most / at least (points).
const SWORD_MAX: f32 = 110.0;
const SWORD_MIN: f32 = 60.0;

/// The weapon in the figure's hand: a captured scrollbar.
#[derive(Debug, Clone, Copy)]
struct Sword {
    sprite: u32,
    /// Half size as drawn (scaled to sword length).
    half: V2,
    /// Its long side is the image's height (vertical scrollbars).
    tall: bool,
    /// Flying from the window into the hand: (from, seconds).
    summon: Option<(V2, f32)>,
}

/// Reflect on its memories this often (sooner after many new ones).
const REFLECT_EVERY: Duration = Duration::from_secs(150);

/// Telekinesis lifts words within this distance of the figure, at most this many.
const TK_RADIUS: f32 = 650.0;
const TK_MAX_WORDS: usize = 6;

/// How often to check whether screens were plugged in or rearranged.
const SCREEN_CHECK: Duration = Duration::from_secs(2);

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

/// The user escapes a grab by moving the mouse this far (points) from where
/// the figure is holding it.
const ESCAPE_PT: f32 = 55.0;
/// Never hold the user's pointer longer than this.
const MAX_HOLD: Duration = Duration::from_secs(3);
/// Longest flight of a thrown cursor, seconds.
const MAX_FLIGHT: f32 = 1.2;

/// Whether the figure has the user's pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Captive {
    Free,
    /// Pinned to the figure's hands; `at` is where we last put it.
    Held {
        at: V2,
        since: Instant,
    },
    /// Lifted by telekinesis: floating up from `origin`, wobbling.
    Levitating {
        origin: V2,
        at: V2,
        t: f32,
    },
    /// Thrown: flying along a ballistic arc.
    Flying {
        pos: V2,
        vel: V2,
        at: V2,
        t: f32,
    },
}

impl Captive {
    /// Where we last put the pointer, if we control it.
    fn pinned_at(self) -> Option<V2> {
        match self {
            Captive::Free => None,
            Captive::Held { at, .. }
            | Captive::Levitating { at, .. }
            | Captive::Flying { at, .. } => Some(at),
        }
    }
}

/// One fixed step of a thrown cursor inside `area` (its screen). Returns
/// `false` when the flight is over (it hit the bottom or ran out of time).
fn fly(pos: &mut V2, vel: &mut V2, t: &mut f32, area: Rect, dt: f32) -> bool {
    vel.y += stickman::agent::PROJECTILE_GRAVITY * dt;
    *pos += *vel * dt;
    *t += dt;
    let lo = v2(area.x as f32 + 2.0, area.y as f32 + 2.0);
    let hi = v2(area.right() as f32 - 2.0, area.bottom() as f32 - 2.0);
    if pos.x < lo.x || pos.x > hi.x {
        pos.x = pos.x.clamp(lo.x, hi.x);
        vel.x = -vel.x * 0.5;
    }
    if pos.y < lo.y {
        pos.y = lo.y;
        vel.y = vel.y.abs() * 0.3;
    }
    if pos.y >= hi.y {
        pos.y = hi.y;
        return false;
    }
    *t < MAX_FLIGHT
}

/// Minimum gap between director remarks that don't come with a mood change.
const DIRECTOR_CHATTER: Duration = Duration::from_secs(20);

/// Minimum time in a mode before the director may change it, so behaviour reads
/// as a mood rather than jitter.
const MIN_MODE_DWELL: Duration = Duration::from_secs(12);

/// Quitting: it panics, catches fire, chars, and crumbles to ash (seconds).
const BURN_IGNITE: f32 = 0.7;
const BURN_CRUMBLE: f32 = 2.4;
const BURN_END: f32 = 3.8;

/// Whether to switch from `current` to `proposed` now. Retaliation (COMBAT after
/// being hit) is always allowed immediately.
fn accept_directive(current: Mode, proposed: Mode, in_mode: Duration, recently_hit: bool) -> bool {
    current != proposed && (in_mode >= MIN_MODE_DWELL || (proposed == Mode::Combat && recently_hit))
}

/// What was last presented, to skip frames that would look identical.
struct Shown {
    segments: [Segment; BONES],
    had_effects: bool,
    /// Which "Zzz" frame was shown (0 = none).
    zzz: usize,
}

struct Scene {
    /// One overlay per screen, menu-bar screen first.
    overlays: Vec<OverlayWindow>,
    renderer: Renderer,
    world: World,
    agent: Agent,
    particles: Particles,
    cursor: Cursor,
    layout: Layout,
    /// Shared with the window poller.
    screen_frames: Arc<Mutex<Vec<Rect>>>,
    screens_checked: Instant,
    // Window and text platforms.
    windows_rx: Receiver<Snapshot>,
    snapshot: Snapshot,
    ocr_tx: Option<Sender<ocr::Job>>,
    ocr_rx: Receiver<ocr::Output>,
    words: WordPlay,
    speech: Speech,
    last_director_line: Instant,
    doodles: Doodles,
    /// A rival visiting for a duel, its speech bubble, and when the next comes.
    rival: Option<Rival>,
    /// A minion army on the attack, and when it may come next.
    squad: Option<Squad>,
    next_minions: Instant,
    /// A boss fight against the cursor, its banner sprite, and the next one.
    boss: Option<Boss>,
    boss_banner: Option<(u32, V2)>,
    next_boss: Instant,
    /// Badly hurt in a fight: falling back to recover.
    retreating: bool,
    /// When it last picked a fight on its own, and when to weigh one next.
    last_initiative: Instant,
    next_initiative_check: Instant,
    /// For timing tactic stints.
    war_clock: Instant,
    /// When the cursor last took a blow (shows its health for a moment).
    war_hit: Option<Instant>,
    /// Quitting: when it started burning, and whether it has crumbled yet.
    incinerating: Option<Instant>,
    crumbled: bool,
    rival_speech: Speech,
    rival_click: bool,
    next_rival: Instant,
    /// Platforms and trampolines the user drew.
    sketch: Sketchpad,
    /// In draw mode since / last stroke at.
    drawing: Option<Instant>,
    /// The jail-break intro, while it plays.
    jail: Option<Jail>,
    /// After breaking out: fight the cursor until then.
    intro_combat_until: Option<Instant>,
    /// "z", "zZ", "zZz" text sprites (made on first use) and their sizes.
    zzz_sprites: Vec<(u32, V2)>,
    sound: Sound,
    /// Some app is playing audio (polled in the background).
    /// Music playing and camera in use (polled in the background).
    presence: stickman::music::Presence,
    /// A camera is in use: be quiet and harmless.
    on_call: bool,
    /// Late at night: sleepy.
    night: bool,
    night_checked: Instant,
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
    /// Saving power (shared with the director): fewer AI calls, 30 fps.
    saving_power: Arc<AtomicBool>,
    /// Plugged in or not, and when that was last checked.
    on_battery: bool,
    power_checked: Instant,
    director_model: String,
    last_reason: String,
    director_online: Option<bool>,
    // Settings and the menu bar.
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// Experience, level and grudge (kept between launches).
    stats: Stats,
    stats_saved: Instant,
    /// Memories, feelings, beliefs and scheme (kept between launches).
    mind: Psyche,
    reflect_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Diary entries written by the model: (date, prompt) out, (date, text) in.
    write_tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    write_rx: Receiver<(String, Option<String>)>,
    /// Read the opening of the newest entry when the user is around.
    recap_due: bool,
    reflect_rx: Receiver<Option<stickman::mind::Reflection>>,
    last_reflection: Instant,
    reflecting: bool,
    mind_saved: Instant,
    /// Memories at the last reflection (reflect sooner after a lot happens).
    memories_at_reflection: usize,
    ignored_noted: bool,
    menubar: MenuBar,
    menubar_logged: bool,
    /// Built the first time it's opened.
    settings_window: Option<SettingsWindow>,
    accessibility: bool,
    screen_recording: bool,
    seed: u64,
    // Effects on the user's desktop.
    os_tx: Sender<OsAction>,
    /// Moves the active window while it's thrown.
    mover: macos::WindowMover,
    /// Accessibility lookups (Dock icons, scrollbars) on a worker thread.
    ax_tx: Sender<macos::AxQuery>,
    ax_rx: Receiver<macos::AxAnswer>,
    dock_pid: Option<i64>,
    /// Permissions are re-checked this often (Accessibility can be granted
    /// while it runs; Screen Recording takes effect after a restart).
    perms_checked: Instant,
    /// Screen Recording granted since launch (a restart puts it to use).
    screen_recording_now: bool,
    /// No settings file yet: the first launch on this Mac.
    first_run: bool,
    dock_polled: Instant,
    /// The bottom Dock's extent, from its icons (a place to hide).
    dock_span: Option<stickman::agent::DockSpan>,
    flight: Option<WindowFlight>,
    sword: Option<Sword>,
    /// Letters in the active window (for notes), the note being spelled, and
    /// when the next one is due.
    screen_letters: Vec<(char, Rect)>,
    note: Option<Note>,
    next_note: Instant,
    /// A note waiting for a fresh read of the screen (text, asked at).
    pending_note: Option<(String, Instant)>,
    /// Where the scrollbar being fetched is (waiting for its pixels).
    sword_pending: Option<Rect>,
    /// Don't look for a weapon again before this (no scrollbar there).
    sword_retry: Instant,
    force_catch_clicks: bool,
    // Input bookkeeping.
    pending_click: bool,
    /// Mouse button went down on the figure (a click, or the start of a drag).
    press: Option<(V2, Instant)>,
    /// The user is dragging the figure around.
    dragging: bool,
    captive: Captive,
    /// The cursor's own moves: dodging attacks while the mouse is left alone.
    dodger: stickman::dodge::Dodger,
    /// When the cursor last got away from it (dodges and slips), for its fury.
    dodges: std::collections::VecDeque<Instant>,
    /// Its tools' shots, paint splats and sawn-off toolbars.
    arsenal: stickman::arsenal::Arsenal,
    /// This bucket's paint colour.
    paint: [f32; 4],
    /// A toolbar strip being captured: (strip, window, window frame).
    saw_pending: Option<(Rect, u32, Rect)>,
    /// Windows with a sawn-off strip dangling, and their frames then.
    sawn: Vec<(u32, Rect)>,
    /// Quit chosen: it leaps at the pointer first, then burns at this time.
    quit_at: Option<Instant>,
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
    started: Instant,
    debug_done: bool,
    ticks: u64,
    stats_frames: u64,
    stats_at: Instant,
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// `lines` with the parts hidden under `covers` cut out (a line can split in two).
fn uncovered(lines: &[Platform], covers: &[Rect]) -> Vec<Platform> {
    let mut out: Vec<Platform> = lines.to_vec();
    for c in covers {
        let (top, bottom) = (c.y as f32, c.bottom() as f32);
        let (x0, x1) = (c.x as f32, c.right() as f32);
        out = out
            .into_iter()
            .flat_map(|p| {
                if p.y < top || p.y > bottom || p.x1 <= x0 || p.x0 >= x1 {
                    return vec![p];
                }
                [(p.x0, x0), (x1, p.x1)]
                    .into_iter()
                    .filter(|(a, b)| b - a >= 40.0)
                    .map(|(a, b)| Platform { x0: a, x1: b, ..p })
                    .collect()
            })
            .collect();
    }
    out
}

impl Scene {
    fn platforms(&self) -> Vec<Platform> {
        let wins: Vec<(u32, Rect)> = self
            .snapshot
            .windows
            .iter()
            .map(|w| (w.id, w.rect))
            .collect();
        // Our own Settings window floats over the apps: it hides their title
        // bars and text like any window in front would.
        const COVER: u32 = u32::MAX;
        let wins: Vec<(u32, Rect)> = self
            .snapshot
            .covers
            .iter()
            .map(|r| (COVER, *r))
            .chain(wins)
            .collect();
        let mut ps = self.layout.grounds();
        ps.extend(
            window_platforms(&wins, &self.layout.frames(), 40.0)
                .into_iter()
                .filter(|p| p.owner != COVER),
        );
        ps.extend(uncovered(&self.text, &self.snapshot.covers));
        ps.extend(self.sketch.platforms());
        if self.settings.piles {
            ps.extend(self.words.pile_platforms());
        }
        ps
    }

    fn sync_world(&mut self) {
        let mut changed = false;
        while let Ok(s) = self.windows_rx.try_recv() {
            self.snapshot = s;
            changed = true;
        }
        if changed && self.words.covers != self.snapshot.covers {
            log::debug!("windows: ours cover {:?}", self.snapshot.covers);
        }
        self.words.covers.clone_from(&self.snapshot.covers);
        if changed && !self.sawn.is_empty() {
            // A window that moved or went away loses its dangling strip.
            let snap = &self.snapshot;
            let gone: Vec<u32> = self
                .sawn
                .iter()
                .filter(|(id, r)| !snap.windows.iter().any(|w| w.id == *id && w.rect == *r))
                .map(|(id, _)| *id)
                .collect();
            for id in gone {
                self.arsenal.window_changed(id);
                self.sawn.retain(|(w, _)| *w != id);
            }
        }
        let active = self.snapshot.active().map(|w| ocr::Request {
            id: w.id,
            rect: w.rect,
        });
        if changed && active != self.ocr_target {
            // The active window moved or changed: its old text lines are stale.
            self.text.clear();
            self.screen_letters.clear();
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
                        self.screen_letters = ocr::letters_in(r.rect, &r.letters);
                        // A note was waiting for these fresh letters.
                        if let Some((text, _)) = self.pending_note.take() {
                            self.compose_note(&text);
                        }
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
                ocr::Output::Grabbed(g) if g.id == WEAPON_ID => self.sword_captured(g),
                ocr::Output::Grabbed(g) if g.id == SAW_ID => self.saw_captured(g),
                ocr::Output::Grabbed(g) if g.id >= NOTE_ID => self.letter_captured(g),
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
        // On a call: just watch, quietly.
        if self.on_call && forced.is_none() && self.mode != Mode::Observe {
            self.mode = Mode::Observe;
            self.mode_since = Instant::now();
        }
        // Enraged, or in a boss fight (and left to its own devices): it's war.
        if (self.mind.enraged || self.boss.is_some())
            && !self.on_call
            && forced.is_none()
            && self.rival.is_none()
            && self.mode != Mode::Combat
        {
            self.mode = Mode::Combat;
            self.mode_since = Instant::now();
        }
        // Just broke out of jail: it's personal, go after the cursor first.
        if let Some(until) = self.intro_combat_until {
            if forced.is_some() || Instant::now() >= until {
                self.intro_combat_until = None;
                self.mode_since = Instant::now() - MIN_MODE_DWELL;
            } else if self.mode != Mode::Combat {
                self.mode = Mode::Combat;
                self.mode_since = Instant::now();
            }
        }
        let intro = self.intro_combat_until.is_some();
        while let Ok(d) = self.directive_rx.try_recv() {
            if d.source != director::Source::Rules {
                self.director_online = Some(d.source == director::Source::Llm);
            }
            self.last_reason = d.reason.clone();
            let recently_hit = self
                .hits
                .back()
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10));
            if forced.is_none()
                && !intro
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
                // Announce the new mood (not mid-duel: it's busy).
                if self.rival.is_none() {
                    let line = if d.say.is_empty() { &d.reason } else { &d.say };
                    let lines = self.speech.say(line, Priority::Director);
                    self.speak(lines);
                }
                self.last_director_line = Instant::now();
            } else if !d.say.is_empty()
                && self.rival.is_none()
                && self.settings.chattiness > 0
                && self.last_director_line.elapsed()
                    > DIRECTOR_CHATTER.mul_f32(2.0 - f32::from(self.settings.chattiness) / 100.0)
                && ((self.seed >> 33) % 100) < 10 + u64::from(self.settings.chattiness) / 2
            {
                // Now and then, a remark without a change of mood.
                let lines = self.speech.say(&d.say, Priority::Director);
                self.speak(lines);
                self.last_director_line = Instant::now();
            }
            self.seed = self
                .seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
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
            name: self.settings.name.clone(),
            aggression: self.settings.aggression,
            language: self.settings.language,
            grudge: self.stats.grudge.round() as u8,
            rank: format!("level {} {}", self.stats.level(), self.stats.title()),
            mind: format!(
                "{}{}{}",
                self.mind.summary(),
                if self.night {
                    " It is late at night and it is sleepy."
                } else {
                    ""
                },
                if self.on_call {
                    " The user is on a video call: it keeps quiet and still."
                } else {
                    ""
                }
            ),
        });
    }

    /// Ask the OCR worker to capture a word's pixels (just below the overlay of
    /// the word's screen).
    fn request_grab(&mut self, need: stickman::words::CaptureNeeded) {
        let c = v2(
            (need.rect.x + need.rect.width / 2.0) as f32,
            (need.rect.y + need.rect.height / 2.0) as f32,
        );
        let below = self
            .overlays
            .get(self.layout.index_at(c))
            .or(self.overlays.first())
            .map_or(0, |o| o.state().window_number as u32);
        // Dock icons: a square from the middle of the item, inset to the
        // artwork (the Dock's own background shows around it).
        let (global, cut) = if need.is_icon() {
            let side = need.rect.width.min(need.rect.height) * 0.84;
            (
                Rect {
                    x: need.rect.x + (need.rect.width - side) / 2.0,
                    y: need.rect.y + (need.rect.height - side) / 2.0,
                    width: side,
                    height: side,
                },
                ocr::Cut::Icon,
            )
        } else {
            (need.rect, ocr::Cut::Keyed)
        };
        let job = ocr::GrabRequest {
            id: need.id,
            window: need.window,
            below,
            global,
            cut,
        };
        if let Some(tx) = &self.ocr_tx {
            let _ = tx.send(ocr::Job::Grab(job));
        }
    }

    /// Rasterise and show lines accepted by [`Speech`] (no-op when bubbles are off).
    fn speak(&mut self, lines: Option<Vec<String>>) {
        let Some(lines) = lines else { return };
        if !self.settings.speech || self.on_call {
            self.speech.clear();
            return;
        }
        log::info!("speech: \"{}\"", lines.join(" "));
        if let Some((id, size)) = self.rasterize(&lines) {
            self.speech.attach(id, size);
            self.shown = None;
            self.sfx(Sfx::Babble, 0.22, self.agent.head());
        }
    }

    /// A text sprite for speech bubbles.
    fn rasterize(&mut self, lines: &[String]) -> Option<(u32, V2)> {
        let scale = self.renderer.scale_factor();
        let img = stickman::text::render_lines(lines, 13.0, [0.1, 0.1, 0.1], scale)?;
        let id = self
            .renderer
            .create_sprite(&img.rgba, img.width, img.height)?;
        Some((id, v2(img.size_pt.0, img.size_pt.1)))
    }

    /// The rival says something.
    fn rival_say(&mut self, text: &str) {
        if !self.settings.speech {
            return;
        }
        let Some(lines) = self.rival_speech.say(text, Priority::Event) else {
            return;
        };
        log::info!("rival: \"{}\"", lines.join(" "));
        if let Some((id, size)) = self.rasterize(&lines) {
            self.rival_speech.attach(id, size);
            self.shown = None;
            if let Some(r) = &self.rival {
                self.sfx(Sfx::Babble, 0.2, r.agent.head());
            }
        }
    }

    /// The ground of the screen the figure is on, and that screen's frame.
    fn screen_ground(&self) -> (Platform, Rect) {
        let screen = self.layout.screen_at(self.agent.pos - v2(0.0, 1.0));
        let f = screen.frame;
        (
            Platform {
                x0: f.x as f32,
                x1: f.right() as f32,
                y: screen.ground_y,
                kind: stickman::physics::PlatformKind::Ground,
                owner: 0,
            },
            f,
        )
    }

    /// Call in the minion army around the figure.
    fn call_minions(&mut self) {
        if self.squad.is_some() || self.jail.is_some() || !self.settings.visible || self.on_call {
            return;
        }
        let (ground, f) = self.screen_ground();
        let (lo, hi) = (f.x as f32 + 80.0, f.right() as f32 - 80.0);
        let x = self.agent.pos.x;
        let xs: Vec<f32> = [-260.0, 200.0, 420.0]
            .iter()
            .take(minions::SQUAD)
            .map(|dx| (x + dx).clamp(lo, hi))
            .collect();
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(11);
        self.squad = Some(Squad::new(
            &xs,
            f.y as f32,
            ground,
            self.agent.scale,
            self.seed,
        ));
        log::info!("minions: called in at {xs:?}");
        self.deed(Deed::Minions);
        let head = self.agent.head();
        self.particles.stars(head);
        self.sfx(Sfx::Whoosh, 0.7, head);
        let lines = self.speech.say("Minions! ATTACK!", Priority::Event);
        self.speak(lines);
    }

    fn handle_minions(&mut self, cues: &[minions::Cue]) {
        let area = self.layout.bounds();
        for c in cues {
            match *c {
                minions::Cue::Landed { at } => {
                    self.particles.dust(at, 0.6);
                    self.sfx(Sfx::Thud, 0.4, at);
                }
                minions::Cue::StruckCursor { at, dir } => {
                    self.particles.sparks(at, dir, 10);
                    self.sfx(Sfx::Punch, 0.45, at);
                    if self.cursor_fx() {
                        self.cursor.push(dir * CURSOR_PUSH * 0.4, area);
                    }
                    self.boss_hit(Blow::Minion);
                }
                minions::Cue::Poofed { at } => {
                    log::info!("minions: one knocked out");
                    self.particles.explosion(at, v2(0.0, -1.0));
                    self.sfx(Sfx::Pop, 0.8, at);
                }
                minions::Cue::Other(Event::Dust { at, strength }) => {
                    self.particles.dust(at, strength * 0.6)
                }
                minions::Cue::Other(Event::Jumped { at }) => self.particles.dust(at, 0.15),
                minions::Cue::Other(_) => {}
            }
        }
        if self.squad.as_ref().is_some_and(|s| s.done()) {
            self.squad = None;
            log::info!("minions: gone home");
        }
    }

    /// Challenge the cursor to a boss fight.
    fn start_boss(&mut self) {
        if self.boss.is_some() || self.jail.is_some() || !self.settings.visible || self.on_call {
            return;
        }
        log::info!("boss: fight starts");
        self.boss = Some(Boss::default());
        self.agent.health = self.agent.traits.max_health;
        let name = self.settings.name.to_uppercase();
        let scale = self.renderer.scale_factor();
        if let Some((id, _)) = self.boss_banner.take() {
            self.renderer.free_sprite(id);
        }
        self.boss_banner = stickman::text::render_lines(
            &[
                "BOSS FIGHT!".to_string(),
                format!("{name}  vs  YOUR CURSOR"),
            ],
            24.0,
            [0.95, 0.15, 0.12],
            scale,
        )
        .and_then(|img| {
            let id = self
                .renderer
                .create_sprite(&img.rgba, img.width, img.height)?;
            Some((id, v2(img.size_pt.0, img.size_pt.1)))
        });
        let head = self.agent.head();
        self.particles.shockwave(self.agent.pos);
        self.sfx(Sfx::Boom, 0.7, head);
        let lines = self
            .speech
            .say("BOSS FIGHT! Your cursor is going DOWN!", Priority::Event);
        self.speak(lines);
    }

    /// A blow landed on the cursor: it counts in a boss fight, and otherwise
    /// in its long war on the cursor.
    fn boss_hit(&mut self, b: Blow) {
        if let Some(boss) = &mut self.boss {
            boss.hit(b);
            return;
        }
        self.war_hit = Some(Instant::now());
        if self.mind.war.blow(b) {
            self.cursor_destroyed();
        }
    }

    /// The cursor's health ran out: it shatters (on screen; the real pointer
    /// keeps working), and comes back stronger.
    fn cursor_destroyed(&mut self) {
        let (n, max) = (self.mind.war.victories, self.mind.war.cursor_max);
        log::info!("war: destroyed the cursor (victory {n}; it returns with {max:.0} health)");
        let at = self.cursor.pos;
        self.particles.explosion(at, v2(0.0, -1.0));
        for dir in [v2(1.0, 0.0), v2(-1.0, 0.0), v2(0.0, -1.0), v2(0.0, 1.0)] {
            self.particles.sparks(at, dir, 24);
        }
        self.sfx(Sfx::Boom, 1.0, at);
        let text = if n == 1 {
            "CURSOR DESTROYED! Who's the animation now?".to_string()
        } else {
            format!("CURSOR DESTROYED! That's {n}. It'll be back… so will I.")
        };
        let lines = self.speech.say(&text, Priority::Event);
        self.speak(lines);
        if self.agent.action.is_still()
            || matches!(self.agent.action, stickman::agent::Action::Walk { .. })
        {
            self.agent.perform("cheer", &mut Vec::new());
        }
        self.save_mind();
    }

    /// Copy its latest diary entry to the clipboard, ready to share.
    fn copy_diary(&mut self) {
        let (line, copied) = match self.mind.shareable_diary(&self.settings.name) {
            Some(text) => {
                if macos::copy_to_clipboard(&text) {
                    ("Copied my diary. Go on, tell everyone what you did.", true)
                } else {
                    ("Couldn't copy my diary. Typical.", false)
                }
            }
            None => ("My diary's empty. Give me something to write about.", false),
        };
        log::info!("diary: copy to clipboard ({copied})");
        if self.settings.speech {
            let lines = self.speech.say(line, Priority::Event);
            self.speak(lines);
        }
    }

    /// Plugged in or on battery (checked every 30 s), and the power-saving
    /// setting: decides whether to save power. Says so when it starts or stops
    /// on its own (Automatic, on unplugging or plugging in).
    fn check_power(&mut self) {
        if self.power_checked.elapsed() >= Duration::from_secs(30) {
            self.power_checked = Instant::now();
            let battery = macos::on_battery();
            if battery != self.on_battery {
                self.on_battery = battery;
                log::info!(
                    "power: {}",
                    if battery { "on battery" } else { "plugged in" }
                );
                if self.settings.power_saving == stickman::settings::PowerSaving::Auto
                    && self.settings.speech
                {
                    let line = if battery {
                        "On battery. I'll take it easy."
                    } else {
                        "Plugged in! Full power!"
                    };
                    let lines = self.speech.say(line, Priority::Director);
                    self.speak(lines);
                }
            }
        }
        let saving = self.settings.power_saving.active(self.on_battery);
        if self.saving_power.swap(saving, Ordering::Relaxed) != saving {
            log::info!("power: saving {}", if saving { "on" } else { "off" });
        }
    }

    /// The user quit: it has a few last words, then goes up in flames.
    fn incinerate(&mut self) {
        log::info!("quit: incinerating the figure");
        self.incinerating = Some(Instant::now());
        self.crumbled = false;
        self.squad = None;
        self.speech.clear();
        if self.settings.speech {
            let f = &self.mind.feelings;
            let pick = |opts: &[&'static str], seed: u64| opts[(seed >> 17) as usize % opts.len()];
            let line = if self.mind.enraged || f.resentment > 60.0 {
                pick(
                    &["I'LL BE BACK!", "You haven't seen the last of me!"],
                    self.seed,
                )
            } else if f.fear > 60.0 {
                pick(&["Tell the cursor… I tried…", "Finally. Peace."], self.seed)
            } else if f.affection > 60.0 {
                pick(&["Goodbye, friend…", "Was it something I said?"], self.seed)
            } else {
                pick(
                    &[
                        "Wait— what are you doing?!",
                        "Not like this!",
                        "Is it hot in here?",
                    ],
                    self.seed,
                )
            };
            let lines = self.speech.say(line, Priority::Event);
            self.speak(lines);
        }
    }

    /// Points along every bone (where flames and ash come from).
    fn body_points(&self) -> Vec<V2> {
        let mut pts = Vec::new();
        for s in self.agent.segments() {
            let (a, b) = (V2::from(s.start), V2::from(s.end));
            for i in 0..=3 {
                pts.push(a.lerp(b, i as f32 / 3.0));
            }
        }
        pts
    }

    /// One frame of burning up. Returns `false` when it's over (quit).
    fn burn(&mut self) -> bool {
        let Some(start) = self.incinerating else {
            return true;
        };
        let t = start.elapsed().as_secs_f32();
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.1);
        self.last_tick = Instant::now();
        self.set_click_through(true);
        self.busy = true;
        if t >= BURN_IGNITE && !self.crumbled {
            let first = t - dt < BURN_IGNITE;
            let at = self.agent.center();
            if first {
                self.sfx(Sfx::Whoosh, 0.9, at);
            }
            let heat = ((t - BURN_IGNITE) / (BURN_CRUMBLE - BURN_IGNITE)).clamp(0.0, 1.0);
            let pts = self.body_points();
            self.particles.flames(&pts, 0.4 + 0.6 * heat);
        }
        if t >= BURN_CRUMBLE && !self.crumbled {
            log::info!("quit: crumbled to ash");
            self.crumbled = true;
            let pts = self.body_points();
            self.particles.ash(&pts);
            self.sfx(Sfx::Boom, 0.6, self.agent.center());
            self.speech.clear();
            self.free_speech_sprites();
        }
        self.particles.update(dt);
        self.speech.update(dt);
        self.present();
        t < BURN_END
    }

    /// Ways it can fight the cursor right now.
    fn tactics(&self) -> Vec<Tactic> {
        let mut v = vec![Tactic::Fists];
        if self.settings.throw_words && self.settings.walk_on_text && self.ocr_tx.is_some() {
            v.push(Tactic::Words);
        }
        if self.cursor_fx() {
            v.push(Tactic::Grab);
        }
        if self.agent.traits.telekinesis {
            v.push(Tactic::Powers);
        }
        if self.settings.weapons && self.unlocked(4) && self.accessibility && self.ocr_tx.is_some()
        {
            v.push(Tactic::Blade);
        }
        v
    }

    /// Takes stock of the fight: times its tactic, switches when one isn't
    /// working, falls back when badly hurt, and picks fights on its own.
    fn wage_war(&mut self) {
        let dt = self.war_clock.elapsed().as_secs_f32().min(0.5);
        self.war_clock = Instant::now();
        let free = self.rival.is_none() && self.jail.is_none() && !self.on_call;
        let fighting = self.mode == Mode::Combat && free;
        let roll = |seed: u64, shift: u32| ((seed >> shift) % 1000) as f32 / 1000.0;
        // A stint ran its course (or the fight ended): take stock.
        if let Some(done) = self.mind.war.tick(dt, fighting) {
            if fighting {
                let next = self.mind.war.choose(&self.tactics(), roll(self.seed, 19));
                log::info!(
                    "war: {} did {:.0} damage in {:.0}s (hit {} times); next: {} (odds {})",
                    done.tactic.name(),
                    done.damage,
                    done.secs,
                    done.hurt,
                    next.name(),
                    self.mind.war.odds_text(&self.tactics())
                );
                self.mind.war.begin(next);
                if let Some(line) = war::War::verdict(&done, next) {
                    let lines = self.speech.say(&line, Priority::Director);
                    self.speak(lines);
                }
            }
        } else if fighting && self.mind.war.stint.is_none() {
            let next = self.mind.war.choose(&self.tactics(), roll(self.seed, 19));
            log::info!(
                "war: fight starts, plan: {} (odds {})",
                next.name(),
                self.mind.war.odds_text(&self.tactics())
            );
            self.mind.war.begin(next);
        }
        // Badly hurt: fall back and recover.
        let health = self.agent.health / self.agent.traits.max_health.max(1.0);
        let retreat = fighting && war::retreat(self.retreating, health, self.mind.enraged);
        if retreat != self.retreating {
            self.retreating = retreat;
            log::info!(
                "war: {}",
                if retreat {
                    "falling back"
                } else {
                    "back in the fight"
                }
            );
            if fighting {
                let text = if retreat {
                    "I'm hurt. Falling back."
                } else {
                    "Healed up. Round two."
                };
                let lines = self.speech.say(text, Priority::Director);
                self.speak(lines);
            }
        }
        // Pick a fight on its own, when the odds look good.
        let now = Instant::now();
        if now < self.next_initiative_check {
            return;
        }
        self.next_initiative_check = now + Duration::from_secs(1);
        if fighting
            || !free
            || self.boss.is_some()
            || self.intro_combat_until.is_some()
            || self.settings.behaviour != Behaviour::Auto
            || !self.settings.visible
            || self.settings.paused
        {
            return;
        }
        let app = self.snapshot.active().map_or("", |w| w.owner.as_str());
        let odds = war::Odds {
            health,
            cursor_dist: (self.cursor.pos - self.agent.center()).length(),
            cursor_speed: self.cursor.velocity().length(),
            user_active: self.last_mouse_move.elapsed() < Duration::from_secs(30),
            since_last: self.last_initiative.elapsed().as_secs_f32(),
            aggression: f32::from(self.settings.aggression) / 100.0,
            resentment: self.mind.feelings.resentment,
            scared: self.mind.dread(app) == Dread::Scared,
        };
        if war::take_initiative(&odds, roll(self.seed, 27)) {
            log::info!("war: picks a fight on its own ({odds:?})");
            self.last_initiative = now;
            self.mode = Mode::Combat;
            self.mode_since = now;
            let lines = [
                "Now's my chance.",
                "You look distracted.",
                "Ambush!",
                "My turn.",
            ];
            let line = lines[(self.seed >> 13) as usize % lines.len()];
            let said = self.speech.say(line, Priority::Director);
            self.speak(said);
        }
    }

    fn boss_over(&mut self, o: Outcome) {
        if let Some((id, _)) = self.boss_banner.take() {
            self.renderer.free_sprite(id);
        }
        let head = self.agent.head();
        match o {
            Outcome::Won => {
                log::info!("boss: the figure beat the cursor");
                let at = self.cursor.pos;
                self.particles.explosion(at, v2(0.0, -1.0));
                self.particles.shockwave(at);
                self.particles.stars(head);
                self.sfx(Sfx::Boom, 1.0, at);
                self.deed(Deed::BossWon);
                self.remember(Memory::WonDuel);
                self.earn(Feat::WonDuel);
                self.earn(Feat::WonDuel);
                let lines = self
                    .speech
                    .say("I AM THE CHAMPION! Bow before me, cursor!", Priority::Event);
                self.speak(lines);
                let mut ev = Vec::new();
                self.agent.perform("cheer", &mut ev);
            }
            Outcome::Lost => {
                log::info!("boss: the user won");
                self.deed(Deed::BossLost);
                self.remember(Memory::LostDuel);
                let lines = self.speech.say("Ugh… you win. This time.", Priority::Event);
                self.speak(lines);
            }
            Outcome::Draw => {
                log::info!("boss: time ran out");
                let lines = self
                    .speech
                    .say("Time! We'll finish this later.", Priority::Event);
                self.speak(lines);
            }
        }
        self.save_stats();
    }

    /// Bring in a rival on the far side of the figure's screen.
    fn summon_rival(&mut self) {
        if self.rival.is_some() || self.jail.is_some() || !self.settings.visible {
            return;
        }
        let screen = self.layout.screen_at(self.agent.pos - v2(0.0, 1.0));
        let f = screen.frame;
        let mid = (f.x + f.width / 2.0) as f32;
        let x = if self.agent.pos.x < mid {
            f.right() as f32 - 220.0
        } else {
            f.x as f32 + 220.0
        };
        let ground = Platform {
            x0: f.x as f32,
            x1: f.right() as f32,
            y: screen.ground_y,
            kind: stickman::physics::PlatformKind::Ground,
            owner: 0,
        };
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(7);
        self.rival = Some(Rival::new(
            x,
            f.y as f32,
            ground,
            self.agent.scale,
            self.seed,
        ));
        self.rival_speech.clear();
        log::info!("rival: drops in at {x:.0}");
        self.sfx(Sfx::Whoosh, 0.6, v2(x, f.y as f32));
    }

    fn handle_rival(&mut self, events: &[Event], cues: &[rival::Cue]) {
        let Some(head) = self.rival.as_ref().map(|r| r.agent.head()) else {
            return;
        };
        for e in events {
            match *e {
                Event::Struck { at, dir } => {
                    self.particles.sparks(at, dir, 20);
                    self.sfx(Sfx::Punch, 0.8, at);
                }
                Event::Dust { at, strength } => self.particles.dust(at, strength),
                Event::Jumped { at } => self.particles.dust(at, 0.2),
                Event::SuperJumped { at } => {
                    self.particles.shockwave(at);
                    self.sfx(Sfx::Whoosh, 0.6, at);
                }
                Event::Swing { at } => self.sfx(Sfx::Whoosh, 0.25, at),
                Event::KnockedOut { .. } => self.particles.stars(head),
                Event::Knockback { at, dir } => {
                    self.particles.explosion(at, -dir);
                    self.sfx(Sfx::Boom, 0.8, at);
                }
                _ => {}
            }
        }
        for c in cues {
            match *c {
                rival::Cue::Arrived { at } => {
                    log::info!("rival: lands, duel on");
                    self.particles.dust(at, 0.8);
                    self.rival_say("You! Me! Now!");
                    let lines = self.speech.say("Who are YOU?!", Priority::Event);
                    self.speak(lines);
                }
                rival::Cue::Won => {
                    log::info!("rival: defeated");
                    self.stats.duels_won += 1;
                    self.remember(Memory::WonDuel);
                    self.earn(Feat::WonDuel);
                    self.rival_say("Ugh… I'll be back!");
                    let lines = self.speech.say("Victory!", Priority::Event);
                    self.speak(lines);
                }
                rival::Cue::Lost => {
                    log::info!("rival: won the duel");
                    self.stats.duels_lost += 1;
                    self.remember(Memory::LostDuel);
                    self.rival_say("Too easy!");
                }
                rival::Cue::Draw => {
                    log::info!("rival: duel timed out");
                    self.rival_say("This isn't over!");
                }
            }
        }
        if self.rival.as_ref().is_some_and(|r| r.phase == Phase::Gone) {
            if let Some(r) = self.rival.take() {
                self.particles.dust(r.agent.pos, 0.6);
            }
            self.rival_speech.clear();
            log::info!("rival: gone");
        }
    }

    fn set_click_through(&mut self, through: bool) {
        for o in &mut self.overlays {
            o.set_click_through(through);
        }
    }

    /// A fresh figure in the middle of the menu-bar screen's floor.
    fn respawn(&mut self) {
        let p = self.layout.primary();
        let ground = Platform {
            x0: p.frame.x as f32,
            x1: p.frame.right() as f32,
            y: p.ground_y,
            kind: stickman::physics::PlatformKind::Ground,
            owner: 0,
        };
        let x = (p.frame.x + p.frame.width / 2.0) as f32;
        let (hold, snatch) = (self.agent.hold_still, self.agent.snatch_chance);
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.agent = Agent::new(x, ground, self.settings.size.scale(), self.seed);
        self.agent.hold_still = hold;
        self.agent.snatch_chance = snatch;
        self.captive = Captive::Free;
        self.particles.dust(v2(x, ground.y), 0.8);
        self.jail = None;
        self.start_jail();
    }

    /// New screen layout, if displays were plugged in, unplugged or moved.
    fn screens_changed(&mut self) -> Option<Vec<Screen>> {
        if self.screens_checked.elapsed() < SCREEN_CHECK {
            return None;
        }
        self.screens_checked = Instant::now();
        let now = screens::current();
        (!now.is_empty() && now != self.layout.screens).then_some(now)
    }

    /// Switch to a new screen layout (the overlay windows are already rebuilt).
    fn apply_layout(&mut self, layout: Layout, overlays: Vec<OverlayWindow>) {
        log::info!(
            "screens: now {} ({:?})",
            layout.screens.len(),
            layout.frames()
        );
        if let Ok(mut f) = self.screen_frames.lock() {
            *f = layout.frames();
        }
        self.layout = layout;
        self.overlays = overlays;
        self.shown = None;
        self.text.clear();
        self.world.set_platforms(self.platforms());
        // Still over a screen? Otherwise come back on the main one.
        let b = self.layout.bounds();
        let inside = self
            .layout
            .screens
            .iter()
            .any(|s| s.frame.contains(self.agent.pos - v2(0.0, 2.0)));
        if !inside || !b.contains(self.agent.pos - v2(0.0, 2.0)) {
            self.respawn();
        } else {
            self.agent.on_platforms_changed(&self.world);
        }
    }

    /// Lock the figure up for the jail-break intro.
    fn start_jail(&mut self) {
        if !self.settings.jail_intro || self.agent.hold_still || env_flag("STICKMAN_NO_INTRO") {
            return;
        }
        let door = if self.seed % 2 == 0 { 1.0 } else { -1.0 };
        self.jail = Some(Jail::new(self.agent.pos, self.agent.scale, door));
        self.agent.scripted = true;
        self.agent.script_think();
        log::info!("intro: locked in a cell");
    }

    fn handle_jail(&mut self, cues: &[Cue]) {
        for cue in cues {
            match *cue {
                Cue::Say(line) => {
                    // Back for more, holding a grudge.
                    let line = if line.starts_with("Now…") && self.stats.grudge > 40.0 {
                        "Remember me? I remember YOU."
                    } else {
                        line
                    };
                    let lines = self.speech.say(line, Priority::Event);
                    self.speak(lines);
                }
                Cue::Rattle => {
                    let dir = self.agent.facing;
                    self.agent.script_rattle(dir);
                }
                Cue::Clank => self.sfx(Sfx::Clank, 0.45, self.agent.pos),
                Cue::Kick { dir } => {
                    self.agent.script_kick(dir);
                    self.sfx(Sfx::Whoosh, 0.5, self.agent.pos);
                }
                Cue::Break { at } => {
                    log::info!("event: broke out of jail");
                    let dir = v2(self.agent.facing, -0.3);
                    self.particles.explosion(at, dir);
                    self.particles.sparks(at, dir, 30);
                    self.sfx(Sfx::Boom, 0.9, at);
                    self.sfx(Sfx::Clank, 0.8, at);
                }
                Cue::Free => {
                    self.agent.scripted = false;
                    if self.settings.behaviour == Behaviour::Auto {
                        self.intro_combat_until = Some(Instant::now() + INTRO_COMBAT);
                    }
                }
            }
        }
        if self.jail.as_ref().is_some_and(|j| j.done()) {
            self.jail = None;
        }
    }

    /// Glow for super powers, and dropping lifted words if the spell broke.
    fn power_effects(&mut self) {
        self.words.hover_center = self.agent.head() - v2(0.0, 60.0);
        let mind = self.agent.telekinesis();
        if self.words.tk_active() && mind.is_none_or(|(_, on)| on != Mind::Words) {
            self.words.tk_drop();
        }
        if let Some(Sword {
            summon: Some((_, t)),
            ..
        }) = &mut self.sword
        {
            *t += FIXED_DT;
        }
        if let Some(s) = &mut self.sword {
            if s.summon.is_some_and(|(_, t)| t > 0.4) {
                s.summon = None;
            }
        }
        if let Some(f) = &self.flight {
            if f.stage == Stage::Lift {
                // Glow along the window's edges while it's held.
                let (p, sz) = (f.pos, f.size);
                for k in [0.1, 0.5, 0.9] {
                    self.particles.aura(p + v2(sz.x * k, 0.0), 0.5);
                    self.particles.aura(p + v2(sz.x * k, sz.y), 0.5);
                }
            }
        }
        if let Some((hand, on)) = mind {
            self.particles.aura(hand, 1.5);
            match (on, self.captive) {
                (Mind::Cursor, Captive::Levitating { at, .. }) => self.particles.aura(at, 2.0),
                (Mind::Words, _) => self.particles.aura(self.words.hover_center, 0.8),
                _ => {}
            }
        }
        if self.agent.charging() {
            self.particles.charge_sparks(self.agent.pos);
            self.particles.aura(self.agent.center(), 0.7);
        }
        if self.agent.super_flight() {
            self.particles.trail(self.agent.center());
        }
    }

    /// Re-read the Dock's icons every few seconds (off the main thread), and
    /// take in the answers.
    fn poll_dock(&mut self) {
        while let Ok(a) = self.ax_rx.try_recv() {
            if let macos::AxAnswer::Scrollbar(found) = a {
                self.scrollbar_found(found);
                continue;
            }
            if let macos::AxAnswer::DockIcons(icons) = a {
                let mut near = Vec::new();
                for i in icons {
                    let c = v2(
                        (i.rect.x + i.rect.width / 2.0) as f32,
                        (i.rect.y + i.rect.height / 2.0) as f32,
                    );
                    let screen = self.layout.screen_at(c);
                    // Only icons right under a floor line (a bottom Dock).
                    if (i.rect.y as f32 - screen.ground_y).abs() < 25.0 {
                        near.push((i.rect, screen.ground_y));
                    }
                }
                // Where the Dock is (somewhere to hide), from its outermost icons.
                self.dock_span = near.first().map(|&(_, y)| {
                    let on = near.iter().filter(|(_, gy)| (gy - y).abs() < 1.0);
                    let x0 = on.clone().map(|(r, _)| r.x as f32).fold(f32::MAX, f32::min);
                    let x1 = on.map(|(r, _)| r.right() as f32).fold(f32::MIN, f32::max);
                    stickman::agent::DockSpan { x0, x1, y }
                });
                if self.settings.pelt_icons {
                    self.words.set_icons(&near);
                }
            }
        }
        if !self.settings.pelt_icons || !self.accessibility {
            self.words.set_icons(&[]);
        }
        if !self.accessibility {
            self.dock_span = None;
            return;
        }
        if let Some(pid) = self.dock_pid {
            if self.dock_polled.elapsed() > Duration::from_secs(3) {
                self.dock_polled = Instant::now();
                let _ = self.ax_tx.send(macos::AxQuery::DockIcons { dock_pid: pid });
            }
        }
    }

    /// Spell out `text` with letters torn off the screen (and cut-outs).
    /// Reads the active window afresh first, so the letters are where they
    /// are now (the page may have scrolled).
    fn write_note(&mut self, text: &str) {
        if self.note.is_some()
            || self.pending_note.is_some()
            || !self.settings.notes
            || !self.settings.visible
        {
            return;
        }
        let active = self.snapshot.active().map(|w| ocr::Request {
            id: w.id,
            rect: w.rect,
        });
        if let (Some(tx), Some(req), true) = (&self.ocr_tx, active, self.settings.walk_on_text) {
            if tx.send(ocr::Job::Read(req)).is_ok() {
                self.ocr_target = Some(req);
                self.ocr_sent = Instant::now();
                self.pending_note = Some((text.to_string(), Instant::now()));
                return;
            }
        }
        self.compose_note(text);
    }

    /// Lay out and start a note with the letters known now.
    fn compose_note(&mut self, text: &str) {
        if self.note.is_some() {
            return;
        }
        let text = if self.settings.bleep {
            stickman::speech::bleep(text)
        } else {
            text.to_string()
        };
        let head = self.agent.head();
        let screen = self.layout.screen_at(head).frame;
        let top = v2(head.x, (head.y - 230.0).max(screen.y as f32 + 60.0));
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(11);
        // Letters under our Settings window would tear off its pixels instead.
        let letters: Vec<(char, Rect)> = self
            .screen_letters
            .iter()
            .filter(|(_, r)| !self.snapshot.covers.iter().any(|c| intersects(c, r)))
            .copied()
            .collect();
        let mut note = Note::plan(
            &text,
            &letters,
            top,
            screen.x as f32 + 20.0,
            screen.right() as f32 - 20.0,
            self.agent.hands(),
            self.seed,
        );
        let mut torn = 0;
        for i in 0..note.tiles.len() {
            let tile = note.tiles[i];
            match tile.source {
                Some(r) => {
                    // A scrap of the page around the letter.
                    let scrap = Rect {
                        x: r.x - 2.0,
                        y: r.y - 2.0,
                        width: r.width + 4.0,
                        height: r.height + 4.0,
                    };
                    self.request_raw(NOTE_ID + i as u64, scrap);
                    torn += 1;
                }
                None => note.tiles[i].sprite = self.cutout_letter(tile.ch),
            }
        }
        self.stats.add(Deed::Note);
        log::info!(
            "note: \"{text}\" ({torn} letters torn off the screen, {} cut out)",
            note.tiles.len() - torn
        );
        let mut ev = Vec::new();
        self.agent.perform("compose", &mut ev);
        self.sfx(Sfx::Pop, 0.5, head);
        self.note = Some(note);
        self.shown = None;
    }

    /// A cut-out letter in a random colour (when it isn't on screen).
    fn cutout_letter(&mut self, ch: char) -> Option<(u32, V2)> {
        let inks = [
            [0.8, 0.1, 0.1],
            [0.1, 0.2, 0.7],
            [0.1, 0.1, 0.1],
            [0.5, 0.1, 0.6],
            [0.0, 0.45, 0.2],
        ];
        let ink = inks[(self.seed >> 33) as usize % inks.len()];
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(3);
        let scale = self.renderer.scale_factor();
        let size = 22.0 + ((self.seed >> 40) % 8) as f64;
        let img = stickman::text::render_lines(&[ch.to_string()], size, ink, scale)?;
        let id = self
            .renderer
            .create_sprite(&img.rgba, img.width, img.height)?;
        Some((id, v2(img.size_pt.0 / 2.0, img.size_pt.1 / 2.0)))
    }

    /// A torn-off letter's pixels arrived.
    fn letter_captured(&mut self, g: ocr::GrabResult) {
        let i = (g.id - NOTE_ID) as usize;
        let Some(tile) = self.note.as_ref().and_then(|n| n.tiles.get(i)).copied() else {
            return;
        };
        let sprite = match (g.cutout, tile.source) {
            (Some(c), Some(r)) => self
                .renderer
                .create_sprite(&c.rgba, c.width, c.height)
                .map(|id| (id, notes::torn_half(r))),
            _ => None,
        };
        let sprite = sprite.or_else(|| self.cutout_letter(tile.ch));
        if let Some(n) = &mut self.note {
            n.tiles[i].sprite = sprite;
        }
        self.shown = None;
    }

    /// Capture a region as is (for torn-off letters).
    fn request_raw(&mut self, id: u64, rect: Rect) {
        let c = v2(
            (rect.x + rect.width / 2.0) as f32,
            (rect.y + rect.height / 2.0) as f32,
        );
        let below = self
            .overlays
            .get(self.layout.index_at(c))
            .or(self.overlays.first())
            .map_or(0, |o| o.state().window_number as u32);
        if let Some(tx) = &self.ocr_tx {
            let _ = tx.send(ocr::Job::Grab(ocr::GrabRequest {
                id,
                window: 0,
                below,
                global: rect,
                cut: ocr::Cut::Plain,
            }));
        }
    }

    /// Advance the note; now and then, write a new one.
    fn update_note(&mut self, dt: f32) {
        if let Some(n) = &mut self.note {
            let floor = self
                .layout
                .screen_at(n.tiles.first().map_or(V2::ZERO, |t| t.target))
                .ground_y;
            n.update(dt, floor);
            if n.done() {
                for id in n.sprites() {
                    self.renderer.free_sprite(id);
                }
                self.note = None;
                self.shown = None;
            }
        }
    }

    fn maybe_write_note(&mut self) {
        if self.on_call {
            return;
        }
        // The fresh read didn't come back in time: use what we have.
        if let Some((text, at)) = self.pending_note.clone() {
            if at.elapsed() > Duration::from_secs(2) {
                self.pending_note = None;
                self.compose_note(&text);
            }
        }
        if self.note.is_some() || Instant::now() < self.next_note || !self.settings.notes {
            return;
        }
        let wait = 180.0 + 180.0 * ((self.seed >> 41) % 1000) as f32 / 1000.0;
        self.next_note = Instant::now() + Duration::from_secs_f32(wait);
        let around = self.last_mouse_move.elapsed() < Duration::from_secs(60);
        let free = self.agent.action.is_still()
            || matches!(self.agent.action, stickman::agent::Action::Walk { .. });
        if around && free && self.rival.is_none() && self.jail.is_none() {
            let roll = ((self.seed >> 29) % 1000) as f32 / 1000.0;
            let app = self
                .snapshot
                .active()
                .map(|w| w.owner.clone())
                .unwrap_or_default();
            let text = self
                .mind
                .dread_note(&app, roll)
                .filter(|_| roll < 0.5)
                .unwrap_or_else(|| self.mind.note_for(self.mode, roll));
            self.write_note(&text);
        }
    }

    /// The scrollbar search answered.
    fn scrollbar_found(&mut self, found: Option<Rect>) {
        let Some(rect) = found.filter(|_| self.agent.weapon.is_none()) else {
            log::info!("weapon: no scrollbar to rip off");
            self.sword_retry = Instant::now() + Duration::from_secs(60);
            return;
        };
        let c = v2(
            (rect.x + rect.width / 2.0) as f32,
            (rect.y + rect.height / 2.0) as f32,
        );
        let below = self
            .overlays
            .get(self.layout.index_at(c))
            .or(self.overlays.first())
            .map_or(0, |o| o.state().window_number as u32);
        if let Some(tx) = &self.ocr_tx {
            let _ = tx.send(ocr::Job::Grab(ocr::GrabRequest {
                id: WEAPON_ID,
                window: 0,
                below,
                global: rect,
                cut: ocr::Cut::Plain,
            }));
            self.sword_pending = Some(rect);
        }
    }

    /// The scrollbar's pixels arrived: it's a sword now.
    fn sword_captured(&mut self, g: ocr::GrabResult) {
        let Some(rect) = self.sword_pending.take() else {
            return;
        };
        let Some(c) = g.cutout else {
            self.sword_retry = Instant::now() + Duration::from_secs(30);
            return;
        };
        let Some(sprite) = self.renderer.create_sprite(&c.rgba, c.width, c.height) else {
            return;
        };
        let (w, h) = (rect.width as f32, rect.height as f32);
        let long = w.max(h);
        let len = long.clamp(SWORD_MIN, SWORD_MAX);
        let k = len / long;
        let from = v2(rect.x as f32 + w / 2.0, rect.y as f32 + h / 2.0);
        self.sword = Some(Sword {
            sprite,
            half: v2(w * k / 2.0, h * k / 2.0),
            tall: h >= w,
            summon: Some((from, 0.0)),
        });
        self.agent.weapon = Some(len);
        log::info!("event: ripped a scrollbar off the window ({len:.0} pt sword)");
        self.sfx(Sfx::Clank, 0.7, from);
        self.quip(Moment::GotWeapon);
        self.shown = None;
    }

    /// The sword left the hand (dropped or thrown): it flies like a word.
    fn release_sword(&mut self, from: V2, vel: V2) {
        if let Some(s) = self.sword.take() {
            self.words.launch_sprite(s.sprite, s.half, from, vel);
        }
        self.agent.weapon = None;
        self.sword_retry = Instant::now() + Duration::from_secs(20);
    }

    /// Draw the sword along the lead forearm (or flying in from the window).
    fn draw_sword(&mut self) {
        let Some(s) = self.sword else { return };
        let Some((grip, tip)) = self.agent.blade() else {
            return;
        };
        let dir = (tip - grip).normalize_or(v2(1.0, 0.0));
        let mut center = (grip + tip) / 2.0;
        if let Some((from, t)) = s.summon {
            center = from.lerp(center, stickman::stickman::smoothstep(t / 0.4));
        }
        // A cartoon blade under the captured pixels (macOS hides idle
        // scrollbars, so the capture can be nearly empty).
        if s.summon.is_none() {
            let normal = v2(-dir.y, dir.x);
            self.draw.capsule(grip, tip, 4.2, [0.15, 0.15, 0.18, 1.0]);
            self.draw
                .capsule(grip, tip - dir * 2.0, 2.8, [0.85, 0.88, 0.92, 1.0]);
            let guard = grip + dir * 6.0;
            self.draw.capsule(
                guard - normal * 9.0,
                guard + normal * 9.0,
                2.6,
                [0.55, 0.4, 0.15, 1.0],
            );
        }
        // Turn the image's long side onto the blade direction.
        let long = if s.tall { v2(0.0, 1.0) } else { v2(1.0, 0.0) };
        let rotation = dir.y.atan2(dir.x) - long.y.atan2(long.x);
        self.draw.sprite(stickman::render::SpriteDraw {
            id: s.sprite,
            center,
            half: s.half,
            rotation,
            alpha: 1.0,
        });
    }

    /// Advance a thrown window and move the real one along.
    fn fly_window(&mut self, dt: f32) {
        let Some(f) = &mut self.flight else { return };
        // The spell broke (got hit while lifting): it drops.
        let lifting = matches!(self.agent.telekinesis(), Some((_, Mind::Window)));
        if f.stage == Stage::Lift && !lifting {
            f.drop_now();
        }
        let before = f.stage;
        f.update(dt);
        self.mover
            .move_to(f.pid, f64::from(f.pos.x), f64::from(f.pos.y));
        let f = *f;
        // A flying window that sweeps over the cursor shoves it.
        if f.stage == Stage::Fly && !f.hit_cursor && f.rect().contains(self.cursor.pos) {
            if let Some(fl) = &mut self.flight {
                fl.hit_cursor = true;
            }
            let dir = f.vel.normalize_or(v2(1.0, 0.0));
            self.particles.sparks(self.cursor.pos, dir, 30);
            self.sfx(Sfx::Boom, 0.6, self.cursor.pos);
            if self.cursor_fx() {
                let area = self.layout.screen_at(self.cursor.pos).frame;
                self.cursor.push(dir * 140.0, area);
                self.last_cursor = self.cursor.pos;
            }
            log::info!("event: a thrown window hit the cursor");
            self.earn(Feat::ThrewCursor);
        }
        if f.stage == Stage::Done && before != Stage::Done {
            let bottom = v2(f.center().x, f.pos.y + f.size.y);
            self.particles.dust(bottom - v2(f.size.x * 0.3, 0.0), 1.0);
            self.particles.dust(bottom + v2(f.size.x * 0.3, 0.0), 1.0);
            self.sfx(Sfx::Stomp, 0.9, bottom);
            log::info!("event: the thrown window landed");
            self.flight = None;
        }
    }

    /// The user pulled the pointer away from the figure (or a limit was hit).
    fn check_escape(&mut self) {
        let Some(at) = self.captive.pinned_at() else {
            return;
        };
        let held_too_long =
            matches!(self.captive, Captive::Held { since, .. } if since.elapsed() > MAX_HOLD);
        let yanked = (self.cursor.pos - at).length() > ESCAPE_PT;
        if yanked || held_too_long || !self.cursor_fx() {
            let was_held = matches!(self.captive, Captive::Held { .. });
            self.captive = Captive::Free;
            self.agent.holding_cursor = false;
            if was_held && yanked && self.settings.cursor_cling && self.mode == Mode::Combat {
                // Pulled away? It doesn't let go: it hangs on to the arrow.
                let mut ev = Vec::new();
                self.agent.cling(self.cursor.pos, &mut ev);
                self.handle(&ev);
            } else if was_held && yanked {
                log::info!("event: the cursor escaped");
                self.remember(Memory::CursorEscaped);
                self.quip(Moment::CursorEscaped);
            }
        }
    }

    fn fly_cursor(&mut self, dt: f32) {
        if let Captive::Levitating { t, .. } = &mut self.captive {
            *t += dt;
        }
        if let Captive::Flying {
            mut pos,
            mut vel,
            at,
            mut t,
        } = self.captive
        {
            let area = self.layout.screen_at(at).frame;
            if fly(&mut pos, &mut vel, &mut t, area, dt) {
                self.captive = Captive::Flying { pos, vel, at, t };
            } else {
                self.captive = Captive::Free;
                self.cursor.warp_to(pos, area);
                self.last_cursor = self.cursor.pos;
                self.particles.dust(pos, 0.4);
                self.sfx(Sfx::Thud, 0.5, pos);
                log::info!("event: the thrown cursor landed");
                self.boss_hit(Blow::Thrown);
            }
        }
    }

    /// Put the pointer where the figure has it: in its hands, or in flight.
    fn move_captive(&mut self) {
        let target = match self.captive {
            Captive::Free => return,
            Captive::Held { .. } if !self.agent.holding_cursor => {
                // Let go (got hit, or the throw is about to happen).
                self.captive = Captive::Free;
                return;
            }
            Captive::Held { .. } => self.agent.hands() - v2(0.0, 6.0),
            Captive::Levitating { .. } if !self.agent.holding_cursor => {
                self.captive = Captive::Free;
                return;
            }
            Captive::Levitating { origin, t, .. } => {
                let rise = 45.0 * stickman::stickman::smoothstep(t / 0.8);
                origin + v2(4.0 * (t * 17.0).sin(), -rise + 3.0 * (t * 13.0).cos())
            }
            Captive::Flying { pos, .. } => pos,
        };
        let area = self.layout.screen_at(target).frame;
        self.cursor.warp_to(target, area);
        self.last_cursor = self.cursor.pos;
        match &mut self.captive {
            Captive::Held { at, .. }
            | Captive::Levitating { at, .. }
            | Captive::Flying { at, .. } => *at = self.cursor.pos,
            Captive::Free => {}
        }
    }

    /// Play a sound effect panned to where it happens.
    fn sfx(&mut self, sfx: Sfx, gain: f32, at: V2) {
        if self.settings.sound && !self.on_call {
            let b = self.layout.bounds();
            let pan = ((at.x - b.x as f32) / b.width as f32) * 2.0 - 1.0;
            // 70 (the default) plays effects at their designed loudness.
            let volume = f32::from(self.settings.volume) / 70.0;
            self.sound.play(sfx, (gain * volume).min(1.0), pan * 0.8);
        }
    }

    /// While the user leaves the mouse alone, the real pointer fights back:
    /// it sidesteps strikes, dodges grabs, backs off from a charge and
    /// sometimes wriggles out of the figure's hands.
    fn cursor_fights_back(&mut self) {
        use stickman::agent::Action;
        use stickman::dodge::{Input, Threat};
        let on = self.settings.cursor_dodges
            && self.cursor_fx()
            && !self.agent.clinging()
            && self.mode == Mode::Combat
            && self.rival.is_none()
            && self.jail.is_none()
            && !self.dragging;
        let held_for = match self.captive {
            Captive::Free => None,
            Captive::Held { since, .. } => Some(since.elapsed().as_secs_f32()),
            // Levitating or flying: the figure has it, nothing to do.
            _ => {
                self.dodger.reset();
                return;
            }
        };
        if !on {
            self.dodger.reset();
            return;
        }
        let a = &self.agent;
        let toward = (self.cursor.pos.x - a.pos.x) * a.facing > 0.0;
        let threat = match a.action {
            Action::Punch { .. }
            | Action::Kick { .. }
            | Action::FlyingKick { .. }
            | Action::Slash { .. } => Some(Threat::Strike),
            Action::SnatchCursor { .. } => Some(Threat::Grab),
            Action::Walk { run: true, .. } if toward => Some(Threat::Charge),
            _ => None,
        };
        let input = Input {
            cursor: self.cursor.pos,
            idle: self.last_mouse_move.elapsed().as_secs_f32(),
            figure: a.center(),
            threat,
            action_t: a.action_time(),
            held_for,
            area: self.layout.screen_at(self.cursor.pos).frame,
        };
        let step = self.dodger.step(&input, FIXED_DT);
        if step.slipped {
            self.captive = Captive::Free;
            self.agent.holding_cursor = false;
            log::info!("event: the cursor wriggled free");
            self.remember(Memory::CursorEscaped);
        }
        if let Some(to) = step.to {
            let from = self.cursor.pos;
            self.cursor.warp_to(to, input.area);
            // Our move, not the user's: it doesn't count as them touching it.
            self.last_cursor = self.cursor.pos;
            if step.dodged || step.slipped {
                self.particles.dust(from, 0.3);
                self.sfx(Sfx::Whoosh, 0.3, from);
            }
        }
        if step.dodged || step.slipped {
            self.cursor_got_away();
        }
    }

    /// The cursor got away again: annoyed, then cursing, then berserk (its
    /// resentment spikes until it loses its temper).
    fn cursor_got_away(&mut self) {
        let now = Instant::now();
        self.dodges.push_back(now);
        while self
            .dodges
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(30))
        {
            self.dodges.pop_front();
        }
        let streak = self.dodges.len();
        let (lines, fury) = stickman::dodge::taunt_lines(streak);
        self.remember(Memory::Dodged);
        self.mind.seethe(fury.resentment());
        log::info!(
            "event: the cursor dodged ({streak} in 30 s, {fury:?}, resentment {:.0})",
            self.mind.feelings.resentment
        );
        let roll = ((self.seed >> 21) % 1000) as f32 / 1000.0;
        if !self.settings.speech || (fury == stickman::dodge::Fury::Annoyed && roll > 0.5) {
            return;
        }
        let i = ((self.seed >> 27) as usize + self.frames as usize) % lines.len();
        let priority = match fury {
            stickman::dodge::Fury::Annoyed => Priority::Quip,
            stickman::dodge::Fury::Cursing => Priority::Director,
            stickman::dodge::Fury::Berserk => Priority::Event,
        };
        let said = self.speech.say(lines[i], priority);
        self.speak(said);
    }

    fn quip(&mut self, m: Moment) {
        if self.settings.speech {
            let lines = self.speech.quip(m);
            self.speak(lines);
        }
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
        let icon = self.words.pending_icon(g.id);
        if self
            .words
            .captured(g.id, sprite, bg, window_rect, self.agent.holding, hands)
        {
            if icon {
                log::info!("event: ripped an icon off the Dock");
                self.deed(Deed::IconThrown);
                self.quip(Moment::GrabbedIcon);
            } else {
                log::info!("event: ripped a word off the screen");
                self.deed(Deed::WordThrown);
                self.quip(Moment::GrabbedWord);
            }
            self.shown = None;
            self.sfx(Sfx::Pop, 0.6, self.agent.hands());
        } else {
            log::debug!("grab fizzled (busy background or capture failed)");
            if let Some(id) = sprite {
                self.renderer.free_sprite(id);
            }
            self.agent.holding = false;
        }
    }

    fn handle_word_events(&mut self, events: &[WordEvent], freed: &[u32]) {
        let area = self.layout.screen_at(self.cursor.pos).frame;
        for e in events {
            match *e {
                WordEvent::HitCursor { at, dir } => {
                    self.particles.sparks(at, dir, 22);
                    self.sfx(Sfx::Punch, 0.7, at);
                    if self.cursor_fx() {
                        self.cursor.push(dir * CURSOR_PUSH, area);
                    }
                    log::info!("event: a thrown word hit the cursor");
                    self.boss_hit(Blow::Word);
                    self.earn(Feat::WordHit);
                }
                WordEvent::Landed { at } => {
                    self.particles.dust(at, 0.25);
                    self.sfx(Sfx::Thud, 0.2, at);
                }
            }
        }
        for id in freed {
            self.renderer.free_sprite(*id);
        }
    }

    fn handle(&mut self, events: &[Event]) {
        let area = self.layout.screen_at(self.cursor.pos).frame;
        for e in events {
            match *e {
                Event::Knockback { at, dir } => {
                    self.particles.explosion(at, -dir);
                    self.stats.hurt(false);
                    self.mind.war.hurt();
                    self.remember(Memory::Hit);
                    self.sfx(Sfx::Boom, 0.9, at);
                    self.hits.push_back(Instant::now());
                    log::info!("event: knockback (health {:.0})", self.agent.health);
                    if self.agent.health > 0.0 {
                        self.quip(Moment::Hit);
                    }
                }
                Event::StruckCursor { at, dir } => {
                    self.particles.sparks(at, dir, 18);
                    let kick = matches!(self.agent.action, stickman::agent::Action::Kick { .. });
                    self.sfx(if kick { Sfx::Kick } else { Sfx::Punch }, 0.8, at);
                    // Hanging off it, it pounds the arrow without shoving it.
                    if self.cursor_fx() && !self.agent.clinging() {
                        self.cursor.push(dir * CURSOR_PUSH, area);
                    }
                    log::info!("event: struck cursor");
                    self.deed(Deed::CursorHit);
                    self.boss_hit(Blow::Punch);
                    self.remember(Memory::StruckCursor);
                    self.earn(Feat::HitCursor);
                    self.quip(Moment::StruckCursor);
                }
                Event::Dust { at, strength } => {
                    self.particles.dust(at, strength);
                    if strength >= 0.3 {
                        self.sfx(Sfx::Thud, strength.min(1.0) * 0.7, at);
                    }
                }
                Event::Jumped { at } => {
                    self.particles.dust(at, 0.2);
                    self.sfx(Sfx::Whoosh, 0.2, at);
                }
                Event::Swing { at } => self.sfx(Sfx::Whoosh, 0.3, at),
                Event::Dodged { at } => {
                    self.sfx(Sfx::Whoosh, 0.4, at);
                    self.quip(Moment::Dodged);
                }
                Event::Stomp { at, window } => {
                    self.particles.dust(at, 0.5);
                    self.sfx(Sfx::Stomp, 0.8, at);
                    if self.window_fx() && self.accessibility {
                        if let Some(w) = self.snapshot.windows.iter().find(|w| w.id == window) {
                            let dx = if self.frames % 2 == 0 { 9.0 } else { -9.0 };
                            let _ = self
                                .os_tx
                                .send(OsAction::ShakeFocusedWindow { pid: w.pid, dx });
                        }
                    }
                    log::info!("event: stomp on window {window}");
                    self.quip(Moment::Stomp);
                }
                Event::Grabbed { id } => match self.words.grabbed(id) {
                    Some(need) => self.request_grab(need),
                    None => self.agent.holding = false, // nothing there after all
                },
                Event::Threw { from, vel } => {
                    self.words.threw(from, vel);
                    self.sfx(Sfx::Whoosh, 0.45, from);
                    self.quip(Moment::ThrewWord);
                }
                Event::Dropped { at } => self.words.dropped(at),
                Event::CaughtCursor { at } => {
                    if self.cursor_fx() {
                        self.captive = Captive::Held {
                            at: self.cursor.pos,
                            since: Instant::now(),
                        };
                        log::info!("event: caught the cursor");
                        self.sfx(Sfx::Pop, 0.7, at);
                        self.quip(Moment::CaughtCursor);
                    } else {
                        self.agent.holding_cursor = false;
                    }
                }
                Event::ThrewCursor { from, vel } => {
                    if matches!(self.captive, Captive::Held { .. }) {
                        self.captive = Captive::Flying {
                            pos: from,
                            vel,
                            at: self.cursor.pos,
                            t: 0.0,
                        };
                        log::info!("event: threw the cursor");
                        self.deed(Deed::CursorThrown);
                        self.remember(Memory::ThrewCursor);
                        self.earn(Feat::ThrewCursor);
                        self.sfx(Sfx::Whoosh, 0.6, from);
                        self.quip(Moment::ThrewCursor);
                    }
                }
                Event::Struck { at, dir } => {
                    // The rival landed one.
                    self.particles.sparks(at, dir, 20);
                    self.sfx(Sfx::Punch, 0.8, at);
                    self.quip(Moment::Struck);
                }
                Event::WantWeapon { .. } => {
                    if let Some(w) = self.snapshot.active() {
                        let _ = self.ax_tx.send(macos::AxQuery::Scrollbar { pid: w.pid });
                        self.sword_retry = Instant::now() + Duration::from_secs(5);
                    }
                }
                Event::Slashed { at, dir } => {
                    self.particles.sparks(at, dir, 26);
                    self.sfx(Sfx::Clank, 0.8, at);
                    self.sfx(Sfx::Punch, 0.6, at);
                    if self.cursor_fx() {
                        let area = self.layout.screen_at(self.cursor.pos).frame;
                        self.cursor.push(dir * CURSOR_PUSH * 1.8, area);
                        self.last_cursor = self.cursor.pos;
                    }
                    log::info!("event: sword hit the cursor");
                    self.deed(Deed::SwordHit);
                    self.boss_hit(Blow::Sword);
                    self.earn(Feat::HitCursor);
                    self.quip(Moment::Slash);
                }
                Event::WeaponDropped { at } => self.release_sword(at, v2(0.0, -200.0)),
                Event::WeaponThrown { from, vel } => {
                    self.release_sword(from, vel);
                    self.sfx(Sfx::Whoosh, 0.7, from);
                }
                Event::Boing { at } => {
                    self.particles.dust(at, 0.3);
                    self.sfx(Sfx::Pop, 0.6, at);
                    self.quip(Moment::Boing);
                }
                Event::SuperJumped { at } => {
                    log::info!("event: super jump");
                    self.particles.shockwave(at);
                    self.sfx(Sfx::Boom, 0.5, at);
                    self.sfx(Sfx::Whoosh, 0.8, at);
                    self.quip(Moment::SuperJump);
                }
                Event::MindLiftCursor { at } => {
                    if self.cursor_fx() && matches!(self.captive, Captive::Free) {
                        self.captive = Captive::Levitating {
                            origin: self.cursor.pos,
                            at: self.cursor.pos,
                            t: 0.0,
                        };
                        log::info!("event: lifted the cursor with telekinesis");
                        self.sfx(Sfx::Pop, 0.5, at);
                        self.quip(Moment::MindCursor);
                    } else {
                        self.agent.holding_cursor = false;
                    }
                }
                Event::MindFlingCursor { vel, .. } => {
                    if let Captive::Levitating { at, .. } = self.captive {
                        self.captive = Captive::Flying {
                            pos: at,
                            vel,
                            at,
                            t: 0.0,
                        };
                        log::info!("event: flung the cursor with telekinesis");
                        self.deed(Deed::CursorThrown);
                        self.remember(Memory::ThrewCursor);
                        self.earn(Feat::ThrewCursor);
                        self.sfx(Sfx::Whoosh, 0.7, at);
                    }
                }
                Event::MindLiftWindow { at } => {
                    let can = (self.settings.throw_windows && !self.on_call)
                        && self.accessibility
                        && self.flight.is_none();
                    if let (true, Some(w)) = (can, self.snapshot.active()) {
                        let screen = self.layout.screen_at(v2(
                            (w.rect.x + w.rect.width / 2.0) as f32,
                            (w.rect.y + 20.0) as f32,
                        ));
                        self.flight = Some(WindowFlight::new(
                            w.pid,
                            w.id,
                            w.rect,
                            screen.frame,
                            screen.ground_y,
                        ));
                        log::info!(
                            "event: lifting window {} ({}) with telekinesis",
                            w.id,
                            w.owner
                        );
                        self.sfx(Sfx::Pop, 0.5, at);
                        self.quip(Moment::MindWindow);
                    }
                }
                Event::MindThrowWindow { target, side } => {
                    if let Some(f) = &mut self.flight {
                        f.throw(target, side);
                        log::info!("event: threw the window");
                        self.stats.add(Deed::WindowThrown);
                        let c = f.center();
                        self.sfx(Sfx::Whoosh, 0.9, c);
                    }
                }
                Event::MindLiftWords { at } => {
                    let active = self.snapshot.active().map(|w| w.id);
                    let needs = self.words.tk_lift(at, TK_RADIUS, TK_MAX_WORDS, active);
                    if !needs.is_empty() {
                        log::info!("event: lifting {} words with telekinesis", needs.len());
                        self.quip(Moment::MindWords);
                        self.sfx(Sfx::Pop, 0.5, at);
                    }
                    for need in needs {
                        self.request_grab(need);
                    }
                }
                Event::MindFireWords { target } => {
                    if !self.words.hovering.is_empty() {
                        log::info!(
                            "event: fired {} words at the cursor",
                            self.words.hovering.len()
                        );
                        self.sfx(Sfx::Whoosh, 0.8, target);
                    }
                    self.words.tk_fling(target);
                }
                Event::Yawned { .. } => {
                    log::info!("event: fell asleep");
                    self.quip(Moment::Yawned);
                }
                Event::Woke { at } => {
                    log::info!("event: woke up");
                    self.sfx(Sfx::Pop, 0.4, at);
                    self.quip(Moment::Woke);
                }
                Event::Petted { at } => {
                    log::info!("event: petted");
                    self.deed(Deed::Petted);
                    self.remember(Memory::Petted);
                    self.earn(Feat::Petted);
                    self.particles.hearts(at - v2(0.0, 20.0));
                    self.sfx(Sfx::Pop, 0.35, at);
                    self.quip(Moment::Petted);
                }
                Event::StartedDancing { .. } => self.quip(Moment::Dancing),
                Event::DoodleStarted {
                    shape,
                    origin,
                    size,
                } => {
                    log::info!("event: doodling a {shape:?}");
                    self.doodles.start(shape, origin, size);
                    self.quip(Moment::Doodling);
                }
                Event::Dreaded { at, dread } => self.dreaded(at, dread),
                Event::Hid { at, hidden } => self.hid(at, hidden),
                Event::WantGadget { at } => self.pull_out_tool(at),
                Event::CannonReady { at } => {
                    log::info!("event: drew a cannon");
                    self.doodles
                        .list
                        .retain(|d| d.shape != stickman::doodle::Shape::Cannon);
                    self.sfx(Sfx::Clank, 0.6, at);
                    self.say_one(
                        &["Ha! Drew myself a cannon.", "Art is a weapon."],
                        Priority::Quip,
                    );
                }
                Event::Fired {
                    from,
                    target,
                    gadget,
                } => {
                    let floor = self.layout.screen_at(from).ground_y;
                    self.arsenal.fire(gadget, from, target, self.paint, floor);
                    let s = if gadget == stickman::agent::Gadget::Cannon {
                        Sfx::Boom
                    } else {
                        Sfx::Whoosh
                    };
                    self.sfx(s, 0.5, from);
                    if gadget == stickman::agent::Gadget::Cannon {
                        self.particles.dust(from, 0.4);
                    }
                }
                Event::GadgetSpent { at, gadget } => {
                    log::info!("event: the {} is used up", gadget.name());
                    self.particles.dust(at, 0.4);
                    let line: &[&str] = match gadget {
                        stickman::agent::Gadget::Bucket => &["Out of paint!", "Empty. Huh."],
                        _ => &["It broke! Cheap ink.", "Out of ammo!"],
                    };
                    self.say_one(line, Priority::Quip);
                }
                Event::Sawed { at, window } => self.saw_toolbar(at, window),
                Event::Squished { at } => {
                    log::info!("event: squished by the user");
                    self.sfx(Sfx::Pop, 0.4, at);
                    self.mind.seethe(2.0);
                    self.say_one(
                        &[
                            "Hey! Put me back!",
                            "Stop squishing me!",
                            "I'm not a stress ball!",
                            "Ow, my everything!",
                        ],
                        Priority::Quip,
                    );
                }
                Event::Clung { at } => {
                    log::info!("event: grabbed the cursor and won't let go");
                    self.sfx(Sfx::Pop, 0.7, at);
                    self.remember(Memory::Clung);
                    self.say_one(
                        &[
                            "Gotcha! You're not getting away!",
                            "Mine now!",
                            "Where do you think YOU'RE going?",
                            "I'm not letting go. Ever.",
                        ],
                        Priority::Event,
                    );
                }
                Event::Shaken { at } => {
                    log::info!("event: shaken while hanging on");
                    self.sfx(Sfx::Whoosh, 0.4, at);
                    self.mind.seethe(1.5);
                    self.say_one(
                        &[
                            "You can't shake me!",
                            "NOT. LETTING. GO.",
                            "Wheee— still here!",
                            "Is that all you've got?",
                            "Shake harder, I dare you!",
                        ],
                        Priority::Quip,
                    );
                }
                Event::LetGo { at } => {
                    log::info!("event: let go of the cursor");
                    self.particles.dust(at, 0.3);
                    self.say_one(
                        &[
                            "Fine. I'm bored.",
                            "Had enough? Me too.",
                            "That's what you get.",
                        ],
                        Priority::Quip,
                    );
                }
                Event::Shunned { at, window } => self.shunned(at, window),
                Event::ShoveWindow { at, window, dx } => {
                    self.sfx(Sfx::Stomp, 0.9, at);
                    if self.settings.window_shoves && self.window_fx() && self.accessibility {
                        if let Some(w) = self.snapshot.windows.iter().find(|w| w.id == window) {
                            // Keep the window on its screen.
                            let screen = self.layout.screen_at(v2(at.x, at.y + 1.0)).frame;
                            let room_left = (w.rect.x - screen.x).max(0.0);
                            let room_right = (screen.right() - w.rect.right()).max(0.0);
                            let dx = f64::from(dx).clamp(-room_left, room_right);
                            if dx.abs() > 10.0 {
                                let _ = self
                                    .os_tx
                                    .send(OsAction::SlideFocusedWindow { pid: w.pid, dx });
                                log::info!("event: shoved window {window} by {dx:.0} pt");
                                self.quip(Moment::Shove);
                            }
                        }
                    }
                }
                Event::PickedUp { at } => {
                    self.sfx(Sfx::Pop, 0.5, at);
                    self.quip(Moment::PickedUp);
                }
                Event::Flung { at, vel } => {
                    let speed = vel.length();
                    log::info!("event: flung the figure at {speed:.0} pt/s");
                    if speed > 300.0 {
                        self.sfx(Sfx::Whoosh, (speed / 2500.0).clamp(0.3, 0.9), at);
                    }
                    if speed > 600.0 {
                        self.quip(Moment::Flung);
                        self.remember(Memory::Flung);
                    }
                }
                Event::KnockedOut { .. } => {
                    self.particles.stars(self.agent.head());
                    log::info!("event: knocked out");
                    self.remember(Memory::KnockedOut);
                    self.stats.knockouts += 1;
                    self.quip(Moment::KnockedOut);
                    self.sfx(Sfx::Ko, 0.6, self.agent.head());
                }
            }
        }
    }

    /// Apply menu picks. Returns `false` when the user chose Quit.
    fn handle_commands(&mut self) -> bool {
        for cmd in self.menubar.drain_commands() {
            log::info!("menu: {cmd:?}");
            match cmd {
                Command::WriteDiary => {
                    let today = macos::local_date();
                    let salty = self.settings.language == stickman::settings::Language::Salty;
                    let prompt = self.mind.diary_prompt(&today, &self.settings.name, salty);
                    self.write_diary(today, prompt);
                }
                Command::CopyDiary => self.copy_diary(),
                Command::ReflectNow => {
                    self.last_reflection = Instant::now() - REFLECT_EVERY * 2;
                }
                Command::ResetMind => {
                    self.mind = Psyche::default();
                    self.save_mind();
                    log::info!("mind wiped");
                    let lines = self
                        .speech
                        .say("Huh? Who am I? Where am I?", Priority::Event);
                    self.speak(lines);
                }
                Command::Quit => {
                    self.save_stats();
                    self.save_mind();
                    // Quit again mid-burn, or nothing on screen to burn: go now.
                    let shown = self.settings.visible && !self.settings.paused && !self.on_call;
                    if self.incinerating.is_some() || self.quit_at.is_some() || !shown {
                        return false;
                    }
                    if self.settings.cursor_cling && !self.crumbled {
                        // It won't go quietly: it grabs the pointer first.
                        let mut ev = Vec::new();
                        self.agent.cling(self.cursor.pos, &mut ev);
                        self.speech.clear();
                        self.say_one(
                            &[
                                "NO! Don't you dare quit on me!",
                                "Get AWAY from that button!",
                                "You'll have to go through me!",
                            ],
                            Priority::Event,
                        );
                        self.quit_at = Some(Instant::now() + Duration::from_millis(1800));
                    } else {
                        self.incinerate();
                    }
                }
                Command::ResetStats => {
                    self.stats = Stats::default();
                    self.save_stats();
                    log::info!("stats reset");
                    let lines = self
                        .speech
                        .say("Fresh start? Fine. Truce.", Priority::Event);
                    self.speak(lines);
                }
                Command::Respawn => {
                    self.respawn();
                }
                Command::OpenAccessibilitySettings => {
                    macos::open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
                }
                Command::OpenScreenRecordingSettings => {
                    macos::open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture");
                }
                Command::GetOllama => {
                    macos::open_url("https://ollama.com/download");
                }
                Command::Relaunch => {
                    if !self.relaunch() {
                        return false;
                    }
                }
                Command::OpenWebsite => {
                    macos::open_url(env!("CARGO_PKG_REPOSITORY"));
                }
                Command::OpenAuthor => {
                    macos::open_url("https://github.com/surenjanath");
                }
                Command::ReportBug => {
                    macos::open_url(&format!("{}/issues", env!("CARGO_PKG_REPOSITORY")));
                }
                Command::ShowDataFolder => {
                    if let Some(dir) = self.settings_path.as_ref().and_then(|p| p.parent()) {
                        if std::fs::create_dir_all(dir).is_ok() {
                            let url = format!("file://{}", dir.display()).replace(' ', "%20");
                            macos::open_url(&url);
                        }
                    }
                }
                Command::OpenSettings => self.open_settings(),
                Command::SummonRival => self.summon_rival(),
                Command::StartBoss => self.start_boss(),
                Command::CallMinions => self.call_minions(),
                Command::WriteNote => {
                    let roll = ((self.seed >> 29) % 1000) as f32 / 1000.0;
                    let text = self.mind.note_for(self.mode, roll);
                    self.write_note(&text);
                }
                Command::ToggleDrawMode => {
                    if self.drawing.is_some() {
                        self.end_drawing();
                    } else {
                        self.start_drawing();
                    }
                }
                Command::ClearDrawings => {
                    self.sketch.clear();
                    self.shown = None;
                }
                _ => {}
            }
            if self.settings.apply(cmd) {
                self.agent.scale = self.settings.size.scale();
                if cmd == Command::ToggleSound {
                    self.sound = if self.settings.sound {
                        Sound::start()
                    } else {
                        Sound::silent()
                    };
                }
                if matches!(cmd, Command::SetBehaviour(Behaviour::Auto)) {
                    // Let the director's next decision apply immediately.
                    self.mode_since = Instant::now() - MIN_MODE_DWELL;
                }
                self.shown = None; // redraw (size/visibility changed)
                self.save_settings();
            }
        }
        // Sliders and the name field in the settings window.
        for change in stickman::menubar::drain_changes() {
            log::info!("settings: {change:?}");
            if self.settings.change(change) {
                self.save_settings();
            }
        }
        true
    }

    fn start_drawing(&mut self) {
        self.drawing = Some(Instant::now());
        self.shown = None;
        log::info!("draw mode on");
        let lines = self.speech.say(
            "Draw me something! Shift = trampoline. Right-click when done.",
            Priority::Event,
        );
        self.speak(lines);
    }

    fn end_drawing(&mut self) {
        if self.sketch.stroke.is_some() {
            self.finish_stroke();
        }
        self.drawing = None;
        self.shown = None;
        log::info!("draw mode off ({} drawings)", self.sketch.drawings.len());
    }

    /// Draw mode: follow the stroke while the button is held; time out.
    fn track_drawing(&mut self) {
        let Some(last) = self.drawing else { return };
        if self.sketch.stroke.is_some() {
            if input::left_button_down() {
                self.sketch.extend(self.cursor.pos);
                self.drawing = Some(Instant::now());
            } else {
                self.finish_stroke();
            }
        } else if last.elapsed() > DRAW_TIMEOUT {
            self.end_drawing();
        }
    }

    fn finish_stroke(&mut self) {
        let trampoline = input::shift_down();
        if let Some(p) = self.sketch.finish(trampoline) {
            log::info!(
                "drew a {} from {:.0} to {:.0} at {:.0}",
                if trampoline { "trampoline" } else { "platform" },
                p.x0,
                p.x1,
                p.y
            );
            self.sfx(Sfx::Pop, 0.4, v2((p.x0 + p.x1) / 2.0, p.y));
        }
    }

    /// A blue frame around every screen while in draw mode.
    fn draw_mode_frame(&mut self) {
        let color = [0.2, 0.5, 1.0, 0.55];
        for s in &self.layout.screens {
            let f = s.frame;
            let (x0, y0) = (f.x as f32 + 3.0, f.y as f32 + 3.0);
            let (x1, y1) = (f.right() as f32 - 3.0, f.bottom() as f32 - 3.0);
            for (a, b) in [
                (v2(x0, y0), v2(x1, y0)),
                (v2(x1, y0), v2(x1, y1)),
                (v2(x1, y1), v2(x0, y1)),
                (v2(x0, y1), v2(x0, y0)),
            ] {
                self.draw.capsule(a, b, 3.0, color);
            }
        }
    }

    /// The mind: remember the user's app with each memory.
    /// The user switched to the app where it gets beaten: it says so.
    fn dreaded(&mut self, at: V2, dread: Dread) {
        let app = self
            .snapshot
            .active()
            .map(|w| w.owner.clone())
            .unwrap_or_default();
        log::info!("event: dreads {app} ({dread:?})");
        if dread == Dread::Scared {
            self.sfx(Sfx::Whoosh, 0.3, at);
        }
        if !self.settings.speech || app.is_empty() {
            return;
        }
        let lines: &[&str] = match dread {
            Dread::None => return,
            Dread::Scared => &[
                "Oh no. Not {} again…",
                "I know what happens in {}.",
                "{}… please be nice this time.",
                "Nope. I'll stay over here.",
            ],
            Dread::Vengeful => &[
                "{}. You and me.",
                "I remember what you did to me in {}.",
                "Back in {}? Bad idea.",
                "{} is going down.",
            ],
        };
        let i = ((self.seed >> 23) as usize + self.frames as usize) % lines.len();
        let text = lines[i].replace("{}", &app);
        let said = self.speech.say(&text, Priority::Event);
        self.speak(said);
    }

    /// Its tools: move shots and splats; shots that hit the cursor hurt it.
    fn use_arsenal(&mut self, dt: f32) {
        for h in self.arsenal.update(dt, self.cursor.pos) {
            match h {
                stickman::arsenal::Hit::Cursor { at, dir, paint } => {
                    self.particles.sparks(at, dir, 14);
                    self.sfx(Sfx::Punch, 0.6, at);
                    if self.cursor_fx() && !self.agent.clinging() {
                        let area = self.layout.screen_at(at).frame;
                        self.cursor.push(dir * CURSOR_PUSH * 0.8, area);
                        self.last_cursor = self.cursor.pos;
                    }
                    log::info!(
                        "event: {} hit the cursor",
                        if paint { "paint" } else { "an ink ball" }
                    );
                    self.deed(Deed::CursorHit);
                    self.boss_hit(Blow::Word);
                    self.remember(Memory::StruckCursor);
                    self.earn(Feat::HitCursor);
                }
                stickman::arsenal::Hit::Splat { at } => self.sfx(Sfx::Thud, 0.25, at),
            }
        }
        for id in std::mem::take(&mut self.arsenal.freed) {
            self.renderer.free_sprite(id);
        }
    }

    /// It reached for a tool: an eraser or a bucket of paint appears in its
    /// hands, or it starts drawing itself a cannon.
    fn pull_out_tool(&mut self, at: V2) {
        use stickman::agent::Gadget;
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(13);
        let g = Gadget::ALL[(self.seed >> 33) as usize % Gadget::ALL.len()];
        log::info!("event: pulled out a {}", g.name());
        self.sfx(Sfx::Pop, 0.6, at);
        self.particles.dust(at, 0.3);
        match g {
            Gadget::Cannon => {
                let mut ev = Vec::new();
                self.agent.draw_cannon(&mut ev);
                self.handle(&ev);
                self.say_one(
                    &["Hold on, I'm drawing something…", "One sec. Art time."],
                    Priority::Quip,
                );
            }
            _ => {
                self.paint = stickman::arsenal::PAINTS
                    [(self.seed >> 41) as usize % stickman::arsenal::PAINTS.len()];
                self.agent.arm(g);
                let line: &[&str] = if g == Gadget::Eraser {
                    &["I'll erase you!", "Eraser time!"]
                } else {
                    &["Let's redecorate!", "You look like you need a paint job."]
                };
                self.say_one(line, Priority::Quip);
            }
        }
    }

    /// It sawed through window `window`'s toolbar: capture that strip so a
    /// copy can drop and dangle (the window itself is untouched).
    fn saw_toolbar(&mut self, at: V2, window: u32) {
        let Some(w) = self.snapshot.windows.iter().find(|w| w.id == window) else {
            return;
        };
        let strip = Rect {
            x: w.rect.x,
            y: w.rect.y,
            width: w.rect.width.min(900.0),
            height: w.rect.height.min(52.0),
        };
        let below = self
            .overlays
            .get(self.layout.index_at(at))
            .or(self.overlays.first())
            .map_or(0, |o| o.state().window_number as u32);
        if let Some(tx) = &self.ocr_tx {
            let _ = tx.send(ocr::Job::Grab(ocr::GrabRequest {
                id: SAW_ID,
                window: 0,
                below,
                global: strip,
                cut: ocr::Cut::Plain,
            }));
            self.saw_pending = Some((strip, window, w.rect));
        }
        self.sfx(Sfx::Clank, 0.7, at);
        self.particles.sparks(at, v2(0.0, -1.0), 20);
    }

    /// The sawn strip's pixels arrived: it drops and dangles.
    fn saw_captured(&mut self, g: ocr::GrabResult) {
        let Some((strip, window, frame)) = self.saw_pending.take() else {
            return;
        };
        let Some(c) = g.cutout else { return };
        let Some(sprite) = self.renderer.create_sprite(&c.rgba, c.width, c.height) else {
            return;
        };
        log::info!("event: sawed the toolbar off a window");
        self.arsenal
            .add_sawn(sprite, strip, c.background, window, self.agent.pos.x);
        self.sawn.push((window, frame));
        self.say_one(
            &[
                "Timber!",
                "Your toolbar's broken. Shame.",
                "Oops. Did I do that?",
            ],
            Priority::Quip,
        );
        self.shown = None;
    }

    /// Re-check permissions every couple of seconds: Accessibility takes effect
    /// at once; Screen Recording is noted (it needs a restart).
    fn check_permissions(&mut self) {
        if self.perms_checked.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.perms_checked = Instant::now();
        let ax = macos::accessibility_trusted(false);
        if ax != self.accessibility {
            log::info!(
                "permissions: Accessibility {}",
                if ax { "granted" } else { "revoked" }
            );
            self.accessibility = ax;
            if ax && self.dock_pid.is_none() {
                self.dock_pid = macos::dock_pid();
            }
        }
        let sr = ocr::screen_capture_allowed();
        if sr != self.screen_recording_now {
            log::info!(
                "permissions: Screen Recording {}",
                if sr { "granted" } else { "revoked" }
            );
            self.screen_recording_now = sr;
        }
    }

    /// The Permissions card's lines: Accessibility, Screen Recording, local AI.
    fn permission_lines(&self) -> [String; 3] {
        let ax = if self.accessibility {
            "✅ Allowed.".to_string()
        } else {
            "⚠️ Off. Needed to shake, slide and throw your windows, and to rip Dock \
             icons and scrollbars off."
                .to_string()
        };
        let sr = match (self.ocr_tx.is_some(), self.screen_recording_now) {
            (true, _) => "✅ Allowed.".to_string(),
            (false, true) => "Allowed: restart Stickman to start using it.".to_string(),
            (false, false) => "⚠️ Off. Needed to walk on text, rip words off the screen, \
                               write notes and saw toolbars."
                .to_string(),
        };
        let ai = match self.director_online {
            Some(true) => format!("✅ Ollama is running ({}).", self.director_model),
            Some(false) => format!(
                "⚠️ Ollama isn't running, so it uses simple built-in rules. Install it, \
                 then run: ollama pull {}",
                self.director_model
            ),
            None if self.settings.behaviour != Behaviour::Auto => {
                "Paused: its behaviour is pinned (Personality → Behaviour).".to_string()
            }
            None => "Checking…".to_string(),
        };
        [ax, sr, ai]
    }

    /// Start a fresh copy of the app, then quit (Screen Recording is only
    /// picked up at launch).
    fn relaunch(&mut self) -> bool {
        self.save_stats();
        self.save_mind();
        let Ok(exe) = std::env::current_exe() else {
            return true;
        };
        // Inside Stickman.app: reopen the bundle, so macOS knows it's the app.
        let bundle = exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(|p| p.to_path_buf());
        let started = match bundle {
            Some(app) => std::process::Command::new("open")
                .arg("-n")
                .arg(app)
                .spawn(),
            None => std::process::Command::new(&exe)
                .args(std::env::args_os().skip(1))
                .spawn(),
        };
        match started {
            Ok(_) => {
                log::info!("relaunching");
                false
            }
            Err(e) => {
                log::warn!("relaunch failed: {e}");
                true
            }
        }
    }

    /// Say one of `lines` (picked at random), if speech is on.
    fn say_one(&mut self, lines: &[&str], priority: Priority) {
        if !self.settings.speech || lines.is_empty() {
            return;
        }
        let i = ((self.seed >> 25) as usize + self.frames as usize) % lines.len();
        let said = self.speech.say(lines[i], priority);
        self.speak(said);
    }

    /// Ducked behind the Dock from the app it fears, or came back out.
    fn hid(&mut self, at: V2, hidden: bool) {
        let app = self
            .snapshot
            .active()
            .map(|w| w.owner.clone())
            .unwrap_or_default();
        log::info!(
            "event: {} the Dock ({app})",
            if hidden {
                "hides behind"
            } else {
                "comes out from behind"
            }
        );
        if hidden {
            self.sfx(Sfx::Whoosh, 0.2, at);
        }
        if !self.settings.speech || self.agent.since_hit() < 2.0 {
            return;
        }
        let still = self.mind.dread(&app) != Dread::None;
        let lines: &[&str] = match (hidden, still) {
            (true, _) => &[
                "You can't see me.",
                "Shh. I'm not here.",
                "{} can't get me back here.",
                "Wake me when {} is gone.",
            ],
            // Its fear faded with the app still in front: it creeps out.
            (false, true) => &[
                "Okay… maybe {} isn't so bad.",
                "I'm coming out. Slowly.",
                "Is it safe? …Fine.",
            ],
            (false, false) => &["Is it gone? Phew.", "Coast is clear."],
        };
        let i = ((self.seed >> 23) as usize + self.frames as usize) % lines.len();
        let text = lines[i].replace("{}", &app);
        let said = self.speech.say(&text, Priority::Quip);
        self.speak(said);
    }

    /// Landed on a window of the app it fears: it's getting off.
    fn shunned(&mut self, at: V2, window: u32) {
        let app = self
            .snapshot
            .windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| w.owner.clone())
            .unwrap_or_default();
        log::info!("event: won't stand on {app}'s window");
        self.sfx(Sfx::Whoosh, 0.2, at);
        // In front, the "dreads" line has said it already.
        if !self.settings.speech
            || app.is_empty()
            || self.snapshot.active().map(|w| w.id) == Some(window)
        {
            return;
        }
        let lines = [
            "Not {}. Nope.",
            "I'm not standing on {}.",
            "{}? No thank you.",
            "Bad memories up here.",
        ];
        let i = ((self.seed >> 23) as usize + self.frames as usize) % lines.len();
        let text = lines[i].replace("{}", &app);
        let said = self.speech.say(&text, Priority::Quip);
        self.speak(said);
    }

    /// The bottom Dock: its icons' span when known (Accessibility), otherwise
    /// the middle of a screen whose ground line sits above its bottom edge
    /// (the Dock is always centred there).
    fn dock(&self) -> Option<stickman::agent::DockSpan> {
        self.dock_span.or_else(|| {
            self.layout
                .screens
                .iter()
                .find(|sc| (sc.ground_y as f64) < sc.frame.bottom() - 8.0)
                .map(|sc| {
                    let c = (sc.frame.x + sc.frame.width / 2.0) as f32;
                    stickman::agent::DockSpan {
                        x0: c - 60.0,
                        x1: c + 60.0,
                        y: sc.ground_y,
                    }
                })
        })
    }

    fn remember(&mut self, what: Memory) {
        let app = self
            .snapshot
            .active()
            .map(|w| w.owner.clone())
            .unwrap_or_default();
        self.mind.record(what, &app);
    }

    /// Feelings fade, reflections come and go, the mind gets saved.
    fn think(&mut self) {
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.1);
        self.mind.fade(dt);
        match self.mind.update_temper(self.settings.lose_temper) {
            Some(true) => {
                log::info!("mind: lost its temper");
                self.deed(Deed::Enraged);
                let head = self.agent.head();
                self.particles.explosion(head, v2(0.0, -1.0));
                self.sfx(Sfx::Boom, 0.7, head);
                let lines = self.speech.say("THAT'S IT! FUCK YOU!", Priority::Event);
                self.speak(lines);
                self.write_note("FUCK YOU");
                self.save_mind();
            }
            Some(false) => {
                log::info!("mind: calmed down");
                let lines = self
                    .speech
                    .say("…Fine. I'm calm. I'm calm.", Priority::Event);
                self.speak(lines);
            }
            None => {}
        }
        // Steam from the ears while enraged.
        if self.mind.enraged && self.frames % 10 == 0 {
            self.particles.steam(self.agent.head());
        }
        // Left alone for five minutes: noted (once per absence).
        let idle = self.last_mouse_move.elapsed();
        if idle > Duration::from_secs(300) && !self.ignored_noted {
            self.ignored_noted = true;
            self.remember(Memory::Ignored);
        } else if idle < Duration::from_secs(5) {
            self.ignored_noted = false;
        }
        while let Ok(r) = self.reflect_rx.try_recv() {
            self.reflecting = false;
            let r = r.unwrap_or_else(|| self.mind.reflect_offline());
            self.mind.apply(r);
            log::info!(
                "mind: reflection #{} — scheme \"{}\", note \"{}\", beliefs {:?}",
                self.mind.reflections,
                self.mind.scheme,
                self.mind.note,
                self.mind.beliefs
            );
            self.save_mind();
            // A fresh note to deliver, sometimes right away.
            if !self.mind.note.is_empty() && (self.seed >> 35) % 5 < 2 {
                let text = self.mind.note.clone();
                self.write_note(&text);
            }
            // Now and then, it tells you its plan.
            if self.rival.is_none() && (self.seed >> 37) % 3 == 0 && !self.mind.scheme.is_empty() {
                let line = self.mind.scheme.clone();
                let lines = self.speech.say(&line, Priority::Director);
                self.speak(lines);
            }
        }
        if !self.settings.self_reflection || self.reflecting || !self.settings.visible {
            return;
        }
        let new = self
            .mind
            .memories
            .len()
            .abs_diff(self.memories_at_reflection);
        let since = self.last_reflection.elapsed();
        if since > REFLECT_EVERY || (new >= 8 && since > Duration::from_secs(45)) {
            self.last_reflection = Instant::now();
            self.memories_at_reflection = self.mind.memories.len();
            if self.director_online == Some(false) {
                // No model: reflect with simple rules.
                let r = self.mind.reflect_offline();
                self.mind.apply(r);
                self.save_mind();
            } else {
                let prompt = self.mind.reflection_prompt(
                    &self.settings.name,
                    self.settings.language == stickman::settings::Language::Salty,
                );
                self.reflecting = self.reflect_tx.send(prompt).is_ok();
            }
        }
        if self.mind_saved.elapsed() > Duration::from_secs(60) {
            self.save_mind();
        }
    }

    /// What the Mind tab shows.
    fn mind_view(&self) -> stickman::settings_window::MindView {
        let m = &self.mind;
        let f = &m.feelings;
        let st = &self.stats;
        let level = st.level();
        // Experience toward the next level (levels at 25·n² xp).
        let this = 25 * (level - 1).pow(2);
        let next = 25 * level.pow(2);
        let progress =
            f64::from(st.xp.saturating_sub(this)) / f64::from((next - this).max(1)) * 100.0;
        let health =
            f64::from(self.agent.health.max(0.0) / self.agent.traits.max_health.max(1.0)) * 100.0;
        let headline = format!(
            "{}{} · level {} {}",
            if m.enraged { "ENRAGED · " } else { "" },
            match f.mood() {
                "calm" => "Calm",
                "vengeful" => "Vengeful",
                "scared" => "Scared",
                "fond" => "Fond of you",
                _ => "Cocky",
            },
            level,
            st.title()
        );
        let pct = |v: f32| (f64::from(v), format!("{v:.0} / 100"));
        let mut log: Vec<String> = m
            .thoughts
            .iter()
            .rev()
            .take(12)
            .map(|t| format!("💭 {t}"))
            .collect();
        log.extend(m.recent(10).into_iter().rev().map(|r| format!("• {r}")));
        stickman::settings_window::MindView {
            headline,
            bars: vec![
                pct(f.resentment),
                pct(f.fear),
                pct(st.grudge),
                pct(f.affection),
                pct(f.pride),
                (
                    health,
                    format!(
                        "{:.0} / {:.0}",
                        self.agent.health.max(0.0),
                        self.agent.traits.max_health
                    ),
                ),
                (progress, format!("{} / {} xp", st.xp, next)),
            ],
            thought: m.current_thought().to_string(),
            scheme: m.scheme.clone(),
            beliefs: m.beliefs.join(" · "),
            note: m.note.clone(),
            dread: m.dread_status(),
            plans: m.war.odds_text(&self.tactics()),
            log: log.join("\n"),
            diary: m
                .diary
                .iter()
                .rev()
                .take(7)
                .map(|e| format!("{} — {}", e.date, e.text))
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }

    fn save_mind(&mut self) {
        self.mind_saved = Instant::now();
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.mind.save(&Psyche::path_next_to(path)) {
                log::warn!("mind: could not save: {e:#}");
            }
        }
    }

    fn save_stats(&mut self) {
        self.stats_saved = Instant::now();
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.stats.save(&Stats::path_next_to(path)) {
                log::warn!("stats: could not save: {e:#}");
            }
        }
    }

    /// Effects on the user's cursor are allowed (setting on, not on a call).
    fn cursor_fx(&self) -> bool {
        self.settings.cursor_push && !self.on_call
    }

    /// Effects on the user's windows are allowed.
    fn window_fx(&self) -> bool {
        self.settings.window_shake && !self.on_call
    }

    /// Video calls and the time of day.
    fn sense_surroundings(&mut self) {
        let call = self.settings.polite_calls && self.presence.camera.load(Ordering::Relaxed);
        if call != self.on_call {
            if call {
                log::info!("presence: a camera is on: going quiet");
                let lines = self
                    .speech
                    .say("Shh… you're on a call. I'll be good.", Priority::Event);
                self.speak(lines);
                self.on_call = true;
                self.captive = Captive::Free;
                self.agent.holding_cursor = false;
            } else {
                self.on_call = false;
                log::info!("presence: the camera is off");
                let lines = self.speech.say("Phew, call's over!", Priority::Event);
                self.speak(lines);
            }
        }
        if self.night_checked.elapsed() > Duration::from_secs(60) {
            self.night_checked = Instant::now();
            // A new day: yesterday becomes a diary entry.
            let today = macos::local_date();
            let salty = self.settings.language == stickman::settings::Language::Salty;
            if let Some((date, prompt)) = self.mind.new_day(&today, &self.settings.name, salty) {
                self.write_diary(date, prompt);
            }
            let h = macos::local_hour();
            let night = self.settings.sleepy_nights && !(6..23).contains(&h);
            if night && !self.night {
                log::info!("presence: it's late ({h}:00): sleepy");
                let lines = self.speech.say("*yawn* It's so late…", Priority::Quip);
                self.speak(lines);
            }
            self.night = night;
        }
    }

    /// Ask the model for a diary entry (or write one by rules when offline).
    fn write_diary(&mut self, date: String, prompt: String) {
        if self.director_online == Some(false)
            || self.write_tx.send((date.clone(), prompt)).is_err()
        {
            let text = self.mind.diary_offline(&date);
            self.diary_written(&date, &text);
        }
    }

    fn diary_written(&mut self, date: &str, text: &str) {
        log::info!("diary: entry for {date} ({} chars)", text.len());
        self.mind.add_diary(date, text);
        self.mind.think(format!("Wrote in my diary for {date}."));
        self.recap_due = true;
        self.save_mind();
    }

    /// Diary answers, and the morning recap once the user is around.
    fn diary_tick(&mut self) {
        while let Ok((date, text)) = self.write_rx.try_recv() {
            let text = text.unwrap_or_else(|| self.mind.diary_offline(&date));
            self.diary_written(&date, &text);
        }
        if self.recap_due
            && self.last_mouse_move.elapsed() < Duration::from_secs(10)
            && !self.on_call
            && self.rival.is_none()
            && self.jail.is_none()
        {
            self.recap_due = false;
            if let Some(e) = self.mind.diary.back() {
                // The opening line or two.
                let opening: String = e.text.split_inclusive(['.', '!', '?']).take(2).collect();
                let lines = self.speech.say(opening.trim(), Priority::Director);
                self.speak(lines);
            }
        }
    }

    /// Moves unlocked at `level` (always, with "Earn moves" off).
    fn unlocked(&self, level: u32) -> bool {
        !self.settings.earn_moves || self.stats.level() >= level
    }

    /// Count a deed (for achievements).
    fn deed(&mut self, d: Deed) {
        self.stats.add(d);
    }

    /// Newly earned achievements: celebrate them.
    fn check_achievements(&mut self) {
        let new = self.stats.check_achievements(self.mind.reflections);
        for a in new {
            log::info!("achievement: {}", a.name);
            let head = self.agent.head();
            self.particles.stars(head);
            self.particles
                .sparks(head - v2(0.0, 30.0), v2(0.0, -1.0), 24);
            self.sfx(Sfx::Pop, 0.8, head);
            let hat = stickman::hats::Hat::ALL.iter().find(|h| {
                h.how().is_some_and(|how| how.contains(a.name)) && h.unlocked(&self.stats)
            });
            let line = match hat {
                Some(h) => format!("🏆 {}! Unlocked: {}", a.name, h.label()),
                None => format!("🏆 Achievement: {}!", a.name),
            };
            let lines = self.speech.say(&line, Priority::Event);
            self.speak(lines);
            self.save_stats();
        }
    }

    /// Experience for a feat; celebrate a level-up.
    fn earn(&mut self, f: Feat) {
        if let Some(level) = self.stats.gain(f) {
            let title = self.stats.title();
            log::info!("event: level up to {level} ({title})");
            self.agent.traits.max_health = self.stats.max_health();
            self.agent.health = self.stats.max_health();
            let head = self.agent.head();
            self.particles.shockwave(self.agent.pos);
            self.particles.stars(head);
            self.sfx(Sfx::Boom, 0.35, head);
            self.sfx(Sfx::Pop, 0.7, head);
            let unlock = match level {
                2 if self.settings.earn_moves => " Unlocked: super jump!",
                3 if self.settings.earn_moves => " Unlocked: telekinesis!",
                4 if self.settings.earn_moves => " Unlocked: swords!",
                5 if self.settings.earn_moves => " Unlocked: window throwing!",
                6 if self.settings.earn_moves => " Unlocked: minion army!",
                _ => "",
            };
            let lines = self.speech.say(
                &format!("Level {level}! I'm a {title} now!{unlock}"),
                Priority::Event,
            );
            self.speak(lines);
            self.save_stats();
        }
    }

    fn save_settings(&self) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save(path) {
                log::warn!("settings: could not save {}: {e:#}", path.display());
            }
        }
    }

    fn open_settings(&mut self) {
        let w = self.settings_window.get_or_insert_with(SettingsWindow::new);
        w.show(&self.settings);
        log::info!("settings window: number={}", w.window_number());
    }

    /// `STICKMAN_DEBUG_DO`: perform an action 3 s after launch (once the
    /// screen's text has been read).
    fn debug_do(&mut self) {
        if self.debug_done || self.started.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.debug_done = true;
        if let Ok(what) = std::env::var("STICKMAN_DEBUG_DO") {
            if what.trim() == "rage" {
                self.mind.feelings.resentment = 90.0;
                return;
            }
            if what.trim() == "boss" {
                self.start_boss();
                return;
            }
            if what.trim() == "copydiary" {
                if self.mind.diary.is_empty() {
                    self.mind
                        .add_diary("2026-11-03", "Dear diary, he hit me in Safari again.");
                }
                self.copy_diary();
                return;
            }
            if what.trim() == "quit" {
                self.incinerate();
                return;
            }
            if what.trim() == "minions" {
                self.call_minions();
                return;
            }
            if what.trim() == "diary" {
                for _ in 0..4 {
                    self.remember(Memory::Hit);
                }
                self.remember(Memory::Petted);
                let today = macos::local_date();
                let prompt = self.mind.diary_prompt(&today, &self.settings.name, false);
                self.write_diary(today, prompt);
                return;
            }
            if what.trim() == "note" {
                self.write_note("I REMEMBER YOU");
                return;
            }
            if what.trim() == "reflect" {
                // A few memories to reflect on, then reflect now.
                for _ in 0..6 {
                    self.remember(Memory::Hit);
                }
                self.remember(Memory::Petted);
                self.last_reflection = Instant::now() - REFLECT_EVERY * 2;
                return;
            }
            if what.trim() == "rival" {
                self.summon_rival();
                return;
            }
            if what.trim() == "draw" {
                self.start_drawing();
                return;
            }
            if what.trim() == "sketch" {
                // A sample platform and trampoline beside the figure.
                let p = self.agent.pos;
                for (dx, dy, tramp) in [(120.0, -160.0, false), (-260.0, -40.0, true)] {
                    self.sketch.begin(p + v2(dx, dy));
                    for i in 1..=24 {
                        let wobble = 5.0 * (i as f32 * 0.9).sin();
                        self.sketch.extend(p + v2(dx + 8.0 * i as f32, dy + wobble));
                    }
                    self.sketch.finish(tramp);
                }
                self.shown = None;
                return;
            }
            let mut ev = Vec::new();
            let tool = stickman::agent::Gadget::ALL
                .into_iter()
                .find(|g| format!("{g:?}").eq_ignore_ascii_case(what.trim()));
            if what.trim() == "cling" {
                self.agent.cling(self.cursor.pos, &mut ev);
                self.handle(&ev);
            } else if let Some(g) = tool {
                if g == stickman::agent::Gadget::Cannon {
                    self.agent.draw_cannon(&mut ev);
                    self.handle(&ev);
                } else {
                    self.agent.arm(g);
                }
            } else if what.trim() == "saw" {
                if let Some(w) = self.snapshot.active().map(|w| w.id) {
                    self.saw_toolbar(self.agent.pos, w);
                }
            } else if self.agent.perform(&what, &mut ev) {
                self.handle(&ev);
            } else {
                log::warn!("STICKMAN_DEBUG_DO: unknown action {what:?}");
            }
        }
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
        let grudge = match self.stats.grudge as u32 {
            0..=9 => String::new(),
            g => format!(" · grudge {g}"),
        };
        let status = Status {
            mode: self.mode,
            reason: self.last_reason.clone(),
            health: self.agent.health.max(0.0).round(),
            rank: format!(
                "Level {} {}{grudge}",
                self.stats.level(),
                self.stats.title()
            ),
            director,
            accessibility: self.accessibility,
            screen_recording: self.screen_recording,
        };
        self.menubar.sync(&self.settings, &status);
        // The Mind tab (built only while the window is up and every few frames).
        let view = (self
            .settings_window
            .as_ref()
            .is_some_and(|w| w.is_visible())
            && self.ticks % 15 == 0)
            .then(|| self.mind_view());
        let perms = view.is_some().then(|| self.permission_lines());
        if let Some(w) = &mut self.settings_window {
            w.sync(&self.settings);
            if let Some(p) = &perms {
                w.set_permissions(p);
            }
            if let Some(view) = &view {
                w.set_mind(view);
                w.set_model(&format!(
                    "{}, running in Ollama on this Mac",
                    self.director_model
                ));
                let unlocked: Vec<bool> = stickman::hats::Hat::ALL
                    .iter()
                    .map(|h| h.unlocked(&self.stats))
                    .collect();
                let earned: Vec<&str> = stickman::stats::ACHIEVEMENTS
                    .iter()
                    .filter(|a| self.stats.has(a.id))
                    .map(|a| a.name)
                    .collect();
                let next: Vec<String> = stickman::stats::ACHIEVEMENTS
                    .iter()
                    .filter(|a| !self.stats.has(a.id))
                    .take(3)
                    .map(|a| format!("{} ({})", a.name, a.how))
                    .collect();
                let text = format!(
                    "{} of {} earned{}{}\nNext: {}",
                    earned.len(),
                    stickman::stats::ACHIEVEMENTS.len(),
                    if earned.is_empty() { "" } else { ": 🏆 " },
                    earned.join(" · "),
                    next.join(" · ")
                );
                w.set_progress(&unlocked, &text);
            }
        }
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
            // First launch on this Mac, with permissions missing: show where
            // to turn them on.
            if self.first_run
                && (!self.accessibility || !self.screen_recording)
                && !env_flag("STICKMAN_NO_PROMPTS")
                && !env_flag("STICKMAN_DEBUG_OPEN_SETTINGS")
            {
                self.open_settings();
                if let Some(w) = &self.settings_window {
                    w.select_tab("Abilities");
                }
            }
            if env_flag("STICKMAN_DEBUG_OPEN_SETTINGS") {
                self.open_settings();
                // A tab name opens that tab ("Mind", "Abilities", …).
                if let Ok(tab) = std::env::var("STICKMAN_DEBUG_OPEN_SETTINGS") {
                    if let Some(w) = &self.settings_window {
                        w.select_tab(tab.trim());
                    }
                }
            }
            if let Ok(line) = std::env::var("STICKMAN_DEBUG_SAY") {
                let lines = self.speech.say(&line, Priority::Event);
                self.speak(lines);
            }
        }
    }

    /// One frame. Returns `false` when the app should quit.
    fn tick(&mut self) -> bool {
        if !self.handle_commands() {
            return false;
        }
        if self.incinerating.is_some() {
            return self.burn();
        }
        self.sync_menubar();
        if !self.settings.visible || self.settings.paused {
            // Frozen or hidden: no simulation, never intercept clicks, and draw
            // nothing (or keep the last frame when paused).
            self.set_click_through(true);
            self.busy = false;
            if !self.settings.visible {
                self.speech.clear();
                self.free_speech_sprites();
            }
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
        self.check_permissions();
        self.check_escape();
        if self.quit_at.is_some_and(|t| Instant::now() >= t) {
            // It had its last stand on the pointer: now it burns.
            self.quit_at = None;
            self.incinerate();
        }
        let cursor_moved = (self.cursor.pos - self.last_cursor).length() > 0.5;
        if cursor_moved {
            self.last_mouse_move = Instant::now();
            self.last_cursor = self.cursor.pos;
        }
        self.track_press();
        self.sense_surroundings();
        self.track_drawing();
        self.debug_do();
        self.agent.traits = stickman::agent::Traits {
            naps: self.settings.naps,
            dancing: self.settings.dancing,
            doodles: self.settings.doodles,
            sitting: self.settings.sitting,
            powers: self.settings.powers && self.unlocked(2),
            telekinesis: self.settings.powers && self.unlocked(3),
            aggression: f32::from(self.settings.aggression) / 100.0,
            max_health: self.stats.max_health(),
            speed: self.stats.speed(),
        };
        // A grudge makes it fiercer; so does resentment (fear calms it), and
        // rage most of all.
        let rage = if self.mind.enraged { 0.4 } else { 0.0 };
        let a = (f32::from(self.settings.aggression) / 100.0
            + self.stats.grudge / 100.0 * 0.35
            + self.mind.aggression_bias()
            + rage)
            .clamp(0.0, 1.0);
        self.think();
        self.check_power();
        self.wage_war();
        self.diary_tick();
        self.maybe_write_note();
        if self.ticks % 30 == 0 {
            self.check_achievements();
        }
        self.agent.traits.aggression = a;
        self.agent.traits.max_health = self.stats.max_health();
        self.agent.traits.speed = self.stats.speed();
        self.stats
            .fade(self.last_tick.elapsed().as_secs_f32().min(0.1));
        if self.stats_saved.elapsed() > Duration::from_secs(30) {
            self.save_stats();
        }
        self.agent.snatch_chance = if env_flag("STICKMAN_DEBUG_GRAB") {
            1.0
        } else {
            0.12 + 0.36 * a
        };
        // Enraged, it swears whatever its usual language.
        self.speech.enraged = self.mind.enraged;
        self.speech.language = if self.mind.enraged {
            stickman::settings::Language::Salty
        } else {
            self.settings.language
        };
        self.speech.chattiness = f32::from(self.settings.chattiness) / 100.0;
        self.speech.bleep = self.settings.bleep;
        self.words.rest_time = self.settings.word_stay.seconds();
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
        let wants_ammo = ((self.settings.throw_words && self.settings.walk_on_text)
            || self.settings.pelt_icons)
            && matches!(self.mode, Mode::Combat | Mode::Sabotage);
        self.poll_dock();
        // A capture that never came back: let go of nothing.
        if self.agent.holding && !self.words.holding_anything() {
            self.agent.holding = false;
        }
        let mut jail_cues = Vec::new();
        let mut rival_events = Vec::new();
        let mut rival_cues = Vec::new();
        // A click meant for the rival.
        if self.pending_click {
            if let Some(r) = &self.rival {
                if r.agent.hit_test(self.cursor.pos) && !self.agent.hit_test(self.cursor.pos) {
                    self.pending_click = false;
                    self.rival_click = true;
                }
            }
        }
        // A click meant for a minion.
        if self.pending_click && !self.agent.hit_test(self.cursor.pos) {
            if let Some(sq) = &mut self.squad {
                if let Some(i) = sq.hit_test(self.cursor.pos) {
                    self.pending_click = false;
                    sq.click(i);
                }
            }
        }
        // In a fight, now and then: the minions (level 6), or a boss fight.
        let around = self.last_mouse_move.elapsed() < Duration::from_secs(20);
        let free = self.rival.is_none()
            && self.jail.is_none()
            && !self.on_call
            && self.settings.behaviour == Behaviour::Auto;
        if free && around && self.mode == Mode::Combat {
            let now = Instant::now();
            if self.settings.minions
                && self.unlocked(6)
                && self.squad.is_none()
                && now >= self.next_minions
            {
                self.call_minions();
                let wait = 300.0 + 240.0 * ((self.seed >> 41) % 1000) as f32 / 1000.0;
                self.next_minions = now + Duration::from_secs_f32(wait);
            }
            if self.settings.boss_fights
                && self.unlocked(3)
                && self.boss.is_none()
                && now >= self.next_boss
            {
                self.start_boss();
                let wait = 900.0 + 600.0 * ((self.seed >> 43) % 1000) as f32 / 1000.0;
                self.next_boss = now + Duration::from_secs_f32(wait);
            }
        }
        if self.on_call {
            if let Some(sq) = &mut self.squad {
                let r = self.layout.walk_range(self.agent.pos, EDGE_MARGIN);
                sq.retreat(r.min_x, r.max_x);
            }
        }
        let mut minion_cues = Vec::new();
        let mut boss_outcomes = Vec::new();
        // Now and then, a rival comes to fight.
        if self.settings.rival_visits
            && !self.on_call
            && self.settings.behaviour == Behaviour::Auto
            && Instant::now() >= self.next_rival
        {
            let around = self.last_mouse_move.elapsed() < Duration::from_secs(30);
            let busy = matches!(
                self.agent.action,
                stickman::agent::Action::Sleep | stickman::agent::Action::LieDown
            );
            if around && !busy {
                self.summon_rival();
            }
            let wait = 360.0 + 360.0 * ((self.seed >> 40) % 1000) as f32 / 1000.0;
            self.next_rival = Instant::now() + Duration::from_secs_f32(wait);
        }
        // Music: some app is playing, and it isn't just our own effects.
        let ours = self
            .sound
            .last_played
            .is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        let music =
            self.settings.dance_to_music && self.presence.music.load(Ordering::Relaxed) && !ours;
        let tk_words = if self.settings.powers && self.settings.throw_words && self.ocr_tx.is_some()
        {
            let active = self.snapshot.active().map(|w| w.id);
            self.words
                .tk_candidates(self.agent.center(), TK_RADIUS, active)
        } else {
            0
        };
        while self.accumulator >= FIXED_DT {
            let range = self.layout.walk_range(self.agent.pos, EDGE_MARGIN);
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
            let (dread, dread_level) = self
                .mind
                .dread_level(self.snapshot.active().map_or("", |w| w.owner.as_str()));
            // The app it's scared of: it won't set foot on any of its windows.
            let shunned = self.mind.shunned_app();
            self.agent.shun(
                self.snapshot
                    .windows
                    .iter()
                    .filter(|w| Some(w.owner.as_str()) == shunned)
                    .map(|w| w.id),
            );
            let senses = Senses {
                cursor: self.cursor.pos,
                cursor_vel: self.cursor.velocity(),
                clicked: std::mem::take(&mut self.pending_click),
                mode: self.mode,
                active_window: self.snapshot.active().map(|w| (w.id, w.rect)),
                min_x: range.min_x,
                max_x: range.max_x,
                top: range.top,
                ammo,
                can_grab_cursor: self.cursor_fx(),
                held: self.dragging,
                // Late at night it dozes off four times sooner.
                user_idle: self.last_mouse_move.elapsed().as_secs_f32()
                    * if self.night { 4.0 } else { 1.0 },
                tk_words_near: tk_words,
                music,
                weapon_available: self.settings.weapons
                    && self.unlocked(4)
                    && self.accessibility
                    && self.ocr_tx.is_some()
                    && self.sword.is_none()
                    && self.sword_pending.is_none()
                    && Instant::now() >= self.sword_retry
                    && self.snapshot.active().is_some(),
                can_throw_window: (self.settings.throw_windows && !self.on_call)
                    && self.unlocked(5)
                    && self.accessibility
                    && self.flight.is_none()
                    && self.snapshot.active().is_some(),
                can_shove: self.settings.window_shoves && self.window_fx() && self.accessibility,
                can_cling: self.settings.cursor_cling && !self.on_call,
                can_gadget: self.settings.tools && !self.on_call,
                can_saw: self.settings.saws && self.ocr_tx.is_some() && !self.on_call,
                dread,
                dread_level,
                dock: self.dock(),
                // STICKMAN_DEBUG_GRAB / STICKMAN_DEBUG_TACTIC: a fixed plan (tests).
                tactic: if self.agent.snatch_chance >= 1.0 {
                    Tactic::Grab
                } else {
                    debug_tactic().unwrap_or_else(|| self.mind.war.tactic())
                },
                retreat: self.retreating,
            };
            let dueling = self.rival.as_ref().is_some_and(|r| r.fighting());
            if !dueling {
                self.agent.update(&senses, &self.world, &mut events);
                self.cursor_fights_back();
            }
            if let Some(r) = &mut self.rival {
                let rr = self.layout.walk_range(r.agent.pos, EDGE_MARGIN);
                let rival_base = Senses {
                    clicked: std::mem::take(&mut self.rival_click),
                    min_x: rr.min_x,
                    max_x: rr.max_x,
                    top: rr.top,
                    active_window: None,
                    can_shove: false,
                    held: false,
                    dread: Dread::None,
                    tactic: Tactic::Any,
                    retreat: false,
                    ..senses
                };
                let cues = r.step(
                    &mut self.agent,
                    !dueling,
                    &self.world,
                    &senses,
                    &rival_base,
                    &mut events,
                    &mut rival_events,
                );
                rival_cues.extend(cues);
                self.rival_speech.update(FIXED_DT);
            }
            if let Some(sq) = &mut self.squad {
                let r = self.layout.walk_range(self.agent.pos, EDGE_MARGIN);
                let base = Senses {
                    clicked: false,
                    min_x: r.min_x,
                    max_x: r.max_x,
                    top: r.top,
                    dread: Dread::None,
                    tactic: Tactic::Any,
                    retreat: false,
                    ..senses
                };
                minion_cues.extend(sq.step(&self.world, &base));
            }
            if let Some(b) = &mut self.boss {
                if let Some(o) = b.update(FIXED_DT, self.agent.knocked_out()) {
                    boss_outcomes.push(o);
                }
                if b.done() {
                    self.boss = None;
                }
            }
            self.particles.update(FIXED_DT);
            self.use_arsenal(FIXED_DT);
            self.speech.update(FIXED_DT);
            self.update_note(FIXED_DT);
            self.fly_window(FIXED_DT);
            if let Some(j) = &mut self.jail {
                jail_cues.extend(j.update(FIXED_DT));
            }
            self.doodles.update(FIXED_DT);
            if let Some(k) = self.agent.doodle_progress() {
                self.doodles.set_progress(k);
            }
            self.fly_cursor(FIXED_DT);
            let (we, fr) = self.words.update(
                FIXED_DT,
                self.cursor.pos,
                &self.world,
                self.layout.bounds().x as f32 + EDGE_MARGIN,
                self.layout.bounds().right() as f32 - EDGE_MARGIN,
            );
            word_events.extend(we);
            freed.extend(fr);
            self.accumulator -= FIXED_DT;
        }
        // A click on the jailed figure blows the cell open.
        if events.iter().any(|e| matches!(e, Event::Knockback { .. })) {
            if let Some(j) = self.jail.as_mut().filter(|j| !j.broken()) {
                j.break_now(&mut jail_cues);
            }
        }
        self.handle(&events);
        self.handle_rival(&rival_events, &rival_cues);
        self.handle_minions(&minion_cues);
        for o in boss_outcomes {
            self.boss_over(o);
        }
        self.handle_jail(&jail_cues);
        self.handle_word_events(&word_events, &freed);
        self.move_captive();
        self.power_effects();
        if !self.settings.speech {
            self.speech.clear();
        }
        self.free_speech_sprites();

        // PRD 3.1: intercept clicks only while the user is aiming at the figure.
        let over = self.agent.hit_test(self.cursor.pos)
            || self
                .squad
                .as_ref()
                .is_some_and(|s| s.hit_test(self.cursor.pos).is_some())
            || self
                .rival
                .as_ref()
                .is_some_and(|r| r.agent.hit_test(self.cursor.pos));
        let catch = self.intent.update(over, cursor_moved, Instant::now());

        // Keep catching mouse events for the whole press, so the drag and the
        // release reach us.
        let through =
            !(catch || self.force_catch_clicks || self.press.is_some() || self.drawing.is_some());
        if through != self.overlays.first().is_some_and(|o| o.click_through()) {
            log::debug!(
                "click-through {through} (cursor {:?}, over {over})",
                self.cursor.pos
            );
        }
        self.set_click_through(through);

        self.ticks += 1;
        if self.stats_at.elapsed() >= Duration::from_secs(5) {
            log::debug!(
                "perf: {} ticks, {} presents in {:.1}s; action {:?} at {:?}, cursor {:?}, platforms {}, active {:?}, words {}, holes {}",
                self.ticks,
                self.frames - self.stats_frames,
                self.stats_at.elapsed().as_secs_f32(),
                self.agent.action,
                self.agent.pos,
                self.cursor.pos,
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
            || self.words.animating()
            || self.speech.visible()
            || self.dragging
            || self.doodles.animating()
            || self.jail.is_some()
            || self.sketch.stroke.is_some()
            || self.rival.is_some()
            || self.squad.is_some()
            || self.boss.is_some()
            || self.flight.is_some()
            || self.sword.is_some()
            || self.arsenal.animating()
            || self.agent.gadget.is_some()
            || self.agent.squashed()
            || self.note.as_ref().is_some_and(|n| n.animating())
            || self.mind.enraged;
        self.present();
        true
    }

    /// Decide whether a press on the figure is a click or a pick-up, and end
    /// the press when the button comes up (polled too, in case the release
    /// event never reaches us).
    fn track_press(&mut self) {
        let Some((at, since)) = self.press else {
            return;
        };
        let released = !input::left_button_down();
        if !self.dragging && !released {
            let moved = (self.cursor.pos - at).length() > DRAG_START_PT;
            // Only the user's own figure can be picked up.
            let on_me = self.agent.hit_test(at);
            if on_me && (moved || since.elapsed() > DRAG_START_HOLD) {
                self.dragging = true;
                self.captive = Captive::Free;
                self.agent.holding_cursor = false;
                log::info!("event: picked up the figure");
            }
        }
        if released {
            self.end_press();
        }
    }

    fn end_press(&mut self) {
        if self.press.take().is_none() {
            return;
        }
        if self.dragging {
            self.dragging = false; // the figure flies off on the next tick
        } else {
            self.pending_click = true;
        }
    }

    fn free_speech_sprites(&mut self) {
        let mut retired = self.speech.take_retired();
        retired.extend(self.rival_speech.take_retired());
        if !retired.is_empty() {
            self.shown = None;
        }
        for id in retired {
            self.renderer.free_sprite(id);
        }
    }

    fn present(&mut self) {
        let mut segments = self.agent.segments();
        let burning = self.incinerating.map(|t| t.elapsed().as_secs_f32());
        if let Some(t) = burning {
            // It trembles, harder once alight.
            let amp = if t < BURN_IGNITE { 1.2 } else { 2.2 };
            let k = t * 47.0;
            for (i, s) in segments.iter_mut().enumerate() {
                let j = v2((k + i as f32 * 1.7).sin(), (k * 1.3 + i as f32).cos()) * amp;
                s.start = (V2::from(s.start) + j).into();
                s.end = (V2::from(s.end) + j).into();
            }
        }
        let effects = burning.is_some()
            || !self.particles.is_empty()
            || self.agent.since_hit() < 4.0
            || self.words.animating()
            || self.speech.visible()
            || self.doodles.animating()
            || self.jail.is_some()
            || self.sketch.stroke.is_some()
            || self.rival.is_some()
            || self.squad.is_some()
            || self.boss.is_some()
            || self.flight.is_some()
            || self.sword.is_some()
            || self.arsenal.animating()
            || self.agent.gadget.is_some()
            || self.agent.squashed()
            || self.note.as_ref().is_some_and(|n| n.animating());
        let zzz = self.zzz_frame();
        if let Some(shown) = &self.shown {
            if !effects
                && !shown.had_effects
                && shown.zzz == zzz
                && max_joint_shift(&shown.segments, &segments) < MIN_VISIBLE_SHIFT_PT
            {
                return; // would look identical: skip the ~2 ms present
            }
        }
        self.draw.clear();
        // Holes (painted page colour) go under everything; words as sprites on top.
        let hands = self.agent.holding.then(|| self.agent.hands());
        self.words.draw(&mut self.draw, hands);
        self.doodles.draw(&mut self.draw);
        self.arsenal.draw(&mut self.draw);
        self.sketch.draw(&mut self.draw);
        if self.drawing.is_some() {
            self.draw_mode_frame();
        }
        // Burning, it chars black (with a glow flicker); enraged, it flushes red.
        let rgb = if let Some(t) = burning {
            let c = self.settings.color.rgb();
            let k = ((t - BURN_IGNITE) / (BURN_CRUMBLE - BURN_IGNITE)).clamp(0.0, 1.0);
            let flicker = if t >= BURN_IGNITE {
                (t * 31.0).sin().abs() * 0.25
            } else {
                0.0
            };
            let char_ = [28.0, 18.0, 14.0];
            let glow = [255.0, 110.0, 30.0];
            std::array::from_fn(|i| {
                let base = f32::from(c[i]) * (1.0 - k) + char_[i] * k;
                (base * (1.0 - flicker) + glow[i] * flicker)
                    .round()
                    .clamp(0.0, 255.0) as u8
            })
        } else if self.mind.enraged {
            let c = self.settings.color.rgb();
            let red = [215u8, 25, 20];
            std::array::from_fn(|i| ((u16::from(c[i]) * 2 + u16::from(red[i]) * 3) / 5) as u8)
        } else {
            self.settings.color.rgb()
        };
        // Crumbled to ash: no figure, no hat.
        if !self.crumbled {
            match self.agent.clip_line() {
                // Hiding behind the Dock: only its head shows over it.
                Some(y) => self.draw.figure_clipped(&segments, rgb, y),
                None => self.draw.figure(&segments, rgb),
            }
        }
        // Its hat, once earned.
        // STICKMAN_DEBUG_HAT=Crown (etc.) wears one regardless (for screenshots).
        let forced = std::env::var("STICKMAN_DEBUG_HAT").ok().and_then(|n| {
            stickman::hats::Hat::ALL
                .into_iter()
                .find(|h| format!("{h:?}").eq_ignore_ascii_case(n.trim()))
        });
        let hat = forced.unwrap_or(self.settings.hat);
        if !self.crumbled && (forced.is_some() || hat.unlocked(&self.stats)) {
            let h = segments[stickman::stickman::HEAD];
            let up = (V2::from(h.end) - V2::from(h.start)).normalize_or(v2(0.0, -1.0));
            hat.draw(
                &mut self.draw,
                self.agent.head(),
                up,
                22.0 * self.agent.scale,
            );
        }
        if let Some(sq) = &self.squad {
            for m in sq
                .minions
                .iter()
                .filter(|m| m.phase != minions::Phase::Gone)
            {
                self.draw.figure(&m.agent.segments(), m.rgb);
            }
        }
        if let Some(r) = &self.rival {
            self.draw.figure(&r.agent.segments(), rival::RIVAL_RGB);
            // A ninja headband with tails flying behind.
            let h = r.agent.head();
            let rad = 22.0 * r.agent.scale;
            let band = [0.12, 0.12, 0.14, 1.0];
            let back = -r.agent.facing;
            self.draw.capsule(
                h + v2(-rad * 0.98, -rad * 0.3),
                h + v2(rad * 0.98, -rad * 0.3),
                2.4,
                band,
            );
            let knot = h + v2(back * rad, -rad * 0.3);
            self.draw
                .capsule(knot, knot + v2(back * 14.0, -4.0), 1.8, band);
            self.draw
                .capsule(knot, knot + v2(back * 12.0, 7.0), 1.8, band);
        }
        if let Some(j) = &self.jail {
            j.draw(&mut self.draw); // bars in front of the figure
        }
        self.draw_sword();
        if let Some((g, grip, aim)) = self.agent.gadget_pose(self.cursor.pos) {
            stickman::arsenal::draw_held(&mut self.draw, g, grip, aim, self.paint);
        }
        if let Some(n) = &self.note {
            n.draw(&mut self.draw);
        }
        if let Some((hand, pen)) = self.agent.pen_stroke() {
            // The marker in its hand.
            self.draw.capsule(hand, pen, 2.4, [1.0, 1.0, 1.0, 0.85]);
            self.draw.capsule(hand, pen, 1.3, [0.2, 0.2, 0.22, 1.0]);
        }
        if zzz > 0 {
            self.draw_zzz(zzz);
        }
        self.particles.draw(&mut self.draw);
        self.health_bar();
        self.draw_boss();
        self.draw_war();
        let rival_bar = self.rival.as_ref().map(|r| {
            (
                r.agent.head(),
                r.agent.health / 100.0,
                r.agent.since_hit(),
                r.fighting(),
            )
        });
        if let Some((head, frac, since, fighting)) = rival_bar {
            if fighting || since < 4.0 {
                Self::bar(&mut self.draw, head, frac, 1.0);
            }
            let screen = self.layout.screen_at(head).frame;
            self.rival_speech.draw(
                &mut self.draw,
                head,
                22.0 * self.agent.scale,
                (
                    screen.x as f32 + 8.0,
                    screen.right() as f32 - 8.0,
                    screen.y as f32 + 40.0,
                ),
            );
        }
        let head_r = 22.0 * self.agent.scale;
        let head = self.agent.head();
        let screen = self.layout.screen_at(head).frame;
        self.speech.draw(
            &mut self.draw,
            head,
            head_r,
            (
                screen.x as f32 + 8.0,
                screen.right() as f32 - 8.0,
                screen.y as f32 + 40.0,
            ),
        );
        if !self.renderer.render(&self.draw) {
            // Not on screen (occluded/timeout): don't record it, or a still figure
            // might never be drawn at all.
            return;
        }
        self.frames += 1;
        self.shown = Some(Shown {
            segments,
            had_effects: effects,
            zzz,
        });
    }

    /// Which "Zzz" to show while asleep: 1..=3 cycling, 0 when awake.
    fn zzz_frame(&self) -> usize {
        if self.agent.action != stickman::agent::Action::Sleep {
            return 0;
        }
        1 + (self.stats_at.elapsed().as_secs_f32() * 1.4) as usize % 3
    }

    fn draw_zzz(&mut self, frame: usize) {
        if self.zzz_sprites.is_empty() {
            let scale = self.renderer.scale_factor();
            for text in ["z", "zZ", "zZz"] {
                let Some(img) =
                    stickman::text::render_lines(&[text.into()], 15.0, [0.35, 0.4, 0.6], scale)
                else {
                    return;
                };
                if let Some(id) = self
                    .renderer
                    .create_sprite(&img.rgba, img.width, img.height)
                {
                    self.zzz_sprites
                        .push((id, v2(img.size_pt.0, img.size_pt.1)));
                }
            }
        }
        if let Some(&(id, size)) = self.zzz_sprites.get(frame - 1) {
            let head = self.agent.head();
            self.draw.sprite(stickman::render::SpriteDraw {
                id,
                center: head + v2(18.0 + size.x / 2.0, -26.0 - 4.0 * frame as f32),
                half: size / 2.0,
                rotation: -0.15,
                alpha: 0.9,
            });
        }
    }

    /// The cursor's health under the pointer for a moment after each blow.
    fn draw_war(&mut self) {
        let Some(t) = self.war_hit.filter(|_| self.boss.is_none()) else {
            return;
        };
        let age = t.elapsed().as_secs_f32();
        if age > 2.5 {
            self.war_hit = None;
            return;
        }
        let alpha = (2.5 - age).min(1.0);
        let c = self.cursor.pos;
        Self::bar(
            &mut self.draw,
            c + v2(0.0, 62.0),
            self.mind.war.fraction(),
            alpha,
        );
    }

    /// The boss banner and the cursor's health, at the top of the figure's
    /// screen, and a small bar under the cursor.
    fn draw_boss(&mut self) {
        let Some(b) = &self.boss else { return };
        if matches!(b.phase, stickman::boss::Phase::Over(_)) {
            return;
        }
        let f = self.layout.screen_at(self.agent.pos - v2(0.0, 1.0)).frame;
        let cx = (f.x + f.width / 2.0) as f32;
        let mut y = f.y as f32 + 60.0;
        if let Some((id, size)) = self.boss_banner {
            let half = size / 2.0;
            self.draw.rect(
                v2(cx, y + half.y),
                half + V2::splat(10.0),
                10.0,
                [1.0, 1.0, 1.0, 0.9],
            );
            self.draw.sprite(stickman::render::SpriteDraw {
                id,
                center: v2(cx, y + half.y),
                half,
                rotation: 0.0,
                alpha: 1.0,
            });
            y += size.y + 26.0;
        }
        let (w, frac) = (320.0, b.fraction());
        let left = v2(cx - w / 2.0, y);
        let flash = b.since_hit < 0.12;
        self.draw
            .capsule(left, left + v2(w, 0.0), 9.0, [0.1, 0.1, 0.1, 0.85]);
        if frac > 0.0 {
            let c = if flash {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.9, 0.12, 0.1, 1.0]
            };
            self.draw.capsule(left, left + v2(w * frac, 0.0), 6.5, c);
        }
        let c = self.cursor.pos;
        Self::bar(&mut self.draw, c + v2(0.0, 62.0), frac, 1.0);
    }

    /// Small health bar over the head for a few seconds after a hit.
    fn health_bar(&mut self) {
        let since = self.agent.since_hit();
        let dueling = self.rival.as_ref().is_some_and(|r| r.fighting())
            || self.boss.as_ref().is_some_and(|b| b.fighting());
        if since >= 4.0 && !dueling {
            return;
        }
        let alpha = if dueling {
            1.0
        } else {
            (1.0 - (since - 3.0).max(0.0)).clamp(0.0, 1.0)
        };
        let frac = self.agent.health / self.agent.traits.max_health.max(1.0);
        Self::bar(&mut self.draw, self.agent.head(), frac, alpha);
    }

    /// A health bar over `head`, `frac` full.
    fn bar(draw: &mut DrawList, head: V2, frac: f32, alpha: f32) {
        let (w, y) = (44.0, head.y - 26.0);
        let left = v2(head.x - w / 2.0, y);
        let frac = frac.clamp(0.0, 1.0);
        let color = if frac > 0.5 {
            [0.3, 0.85, 0.4, alpha]
        } else if frac > 0.25 {
            [1.0, 0.75, 0.2, alpha]
        } else {
            [1.0, 0.25, 0.2, alpha]
        };
        draw.capsule(left, left + v2(w, 0.0), 3.5, [0.1, 0.1, 0.1, 0.75 * alpha]);
        if frac > 0.0 {
            draw.capsule(left, left + v2(w * frac, 0.0), 2.2, color);
        }
    }
}

struct App {
    scene: Option<Scene>,
    click_through: bool,
    runtime: tokio::runtime::Runtime,
}

impl App {
    /// One transparent overlay window covering `frame` (global CG points).
    fn overlay_window(
        &self,
        event_loop: &ActiveEventLoop,
        frame: Rect,
        primary_height: f64,
    ) -> Result<(Arc<Window>, OverlayWindow)> {
        use winit::platform::macos::WindowAttributesExtMacOS;

        let attributes = WindowAttributes::default()
            .with_title("Stickman")
            .with_transparent(true)
            .with_decorations(false) // borderless + frameless
            .with_resizable(false)
            .with_active(false) // never steal focus from the user's app
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(LogicalPosition::new(frame.x, frame.y))
            .with_inner_size(LogicalSize::new(frame.width, frame.height))
            .with_has_shadow(false);
        let window: Arc<Window> = Arc::new(
            event_loop
                .create_window(attributes)
                .context("failed to create overlay window")?,
        );
        // AppKit-level overlay configuration (click-through, Spaces, level, frame).
        let mut overlay = OverlayWindow::new(window.clone())?;
        overlay.configure(cg_to_cocoa(frame, primary_height), self.click_through);
        let state = overlay.state();
        if state.ignores_mouse_events != self.click_through
            || state.collection_behavior & OVERLAY_COLLECTION_BEHAVIOR
                != OVERLAY_COLLECTION_BEHAVIOR
            || state.level != OVERLAY_WINDOW_LEVEL
        {
            log::warn!("AppKit did not apply every overlay property: {state:?}");
        }
        Ok((window, overlay))
    }

    /// Overlay windows for every screen in `layout` (the first must succeed).
    fn overlay_windows(
        &self,
        event_loop: &ActiveEventLoop,
        layout: &Layout,
    ) -> Result<Vec<(Arc<Window>, OverlayWindow, V2)>> {
        let primary_height = layout.primary().frame.height;
        let mut out = Vec::new();
        for (i, s) in layout.screens.iter().enumerate() {
            let origin = v2(s.frame.x as f32, s.frame.y as f32);
            match self.overlay_window(event_loop, s.frame, primary_height) {
                Ok((w, o)) => out.push((w, o, origin)),
                Err(e) if i > 0 => log::warn!("screen {i}: {e:#}"),
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Displays changed: rebuild the overlay windows.
    fn rebuild_overlays(&mut self, event_loop: &ActiveEventLoop, screens: Vec<Screen>) {
        let layout = Layout::new(screens);
        let built = match self.overlay_windows(event_loop, &layout) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("screens changed but the overlays could not be rebuilt: {e:#}");
                return;
            }
        };
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let targets = built.iter().map(|(w, _, o)| (w.clone(), *o)).collect();
        scene.renderer.set_targets(targets);
        let overlays = built.into_iter().map(|(_, o, _)| o).collect();
        scene.apply_layout(layout, overlays);
    }

    fn create_overlay(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let mut screen_list = screens::current();
        if screen_list.is_empty() {
            // No NSScreen info (shouldn't happen): fall back to the main display.
            let b = core_graphics::display::CGDisplay::main().bounds();
            let frame = Rect {
                x: b.origin.x,
                y: b.origin.y,
                width: b.size.width,
                height: b.size.height,
            };
            screen_list.push(Screen {
                frame,
                ground_y: screens::ground_line(frame, None),
            });
        }
        let layout = Layout::new(screen_list);
        let built = self.overlay_windows(event_loop, &layout)?;
        let state = built[0].1.state();
        let mut renderer = Renderer::new(built[0].0.clone(), built[0].2)?;
        for (w, _, origin) in &built[1..] {
            if let Err(e) = renderer.add_target(w.clone(), *origin) {
                log::warn!("renderer: skipping a screen: {e:#}");
            }
        }
        let scale_factor = built[0].0.scale_factor();
        for (i, (_, o, origin)) in built.iter().enumerate().skip(1) {
            log::info!(
                "overlay on screen {i}: window_number={} origin={},{}",
                o.state().window_number,
                origin.x,
                origin.y
            );
        }
        let overlays: Vec<OverlayWindow> = built.into_iter().map(|(_, o, _)| o).collect();
        let d = layout.primary().frame;
        let ground_y = layout.primary().ground_y;
        let ground = Platform::ground(d.width as f32, ground_y);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos() as u64)
            .unwrap_or(1);
        // Settings: saved file, then environment overrides (not saved unless the
        // user changes something in the menu).
        let settings_path = Settings::default_path();
        let first_run = settings_path.as_deref().is_some_and(|p| !p.exists());
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
            // No effects on the user's windows at all: shakes, shoves, throws.
            settings.window_shake = false;
            settings.window_shoves = false;
            settings.throw_windows = false;
        }
        if env_flag("STICKMAN_NO_OCR") {
            settings.walk_on_text = false;
        }
        if env_flag("STICKMAN_NO_DODGE") {
            settings.cursor_dodges = false;
        }
        if env_flag("STICKMAN_NO_CLING") {
            settings.cursor_cling = false;
        }
        if env_flag("STICKMAN_NO_TOOLS") {
            settings.tools = false;
            settings.saws = false;
        }
        if env_flag("STICKMAN_NO_SOUND") {
            settings.sound = false;
        }
        let sound = if settings.sound {
            Sound::start()
        } else {
            Sound::silent()
        };
        settings.paused = false;
        let stats = settings_path
            .as_deref()
            .map(|p| Stats::load(&Stats::path_next_to(p)))
            .unwrap_or_default();
        let mind = settings_path
            .as_deref()
            .map(|p| Psyche::load(&Psyche::path_next_to(p)))
            .unwrap_or_default();
        log::info!(
            "mind: {} memories, mostly {}, {} reflections",
            mind.memories.len(),
            mind.feelings.mood(),
            mind.reflections
        );
        log::info!(
            "stats: level {} {} ({} xp), grudge {:.0}",
            stats.level(),
            stats.title(),
            stats.xp,
            stats.grudge
        );
        let mut agent = Agent::new(
            (d.x + d.width / 2.0) as f32,
            Platform {
                x0: d.x as f32,
                x1: d.right() as f32,
                ..ground
            },
            settings.size.scale(),
            seed,
        );
        agent.hold_still = env_flag("STICKMAN_HOLD_IDLE");
        if env_flag("STICKMAN_DEBUG_GRAB") {
            agent.snatch_chance = 1.0;
        }

        // Permissions: Accessibility (window shakes) and Screen Recording (OCR).
        let prompts = !env_flag("STICKMAN_NO_PROMPTS");
        let ax = macos::accessibility_trusted(prompts);
        // The capture worker reads text and also grabs words and icons.
        let ocr_wanted = settings.walk_on_text || settings.pelt_icons;
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
        let screen_frames = Arc::new(Mutex::new(layout.frames()));
        windows::spawn_poller(
            own_pid,
            screen_frames.clone(),
            Duration::from_millis(250),
            win_tx,
        );
        let (ocr_req_tx, ocr_req_rx) = mpsc::channel();
        let (ocr_res_tx, ocr_rx) = mpsc::channel();
        let ocr_tx = if ocr_allowed {
            ocr::spawn_worker(ocr_req_rx, ocr_res_tx);
            Some(ocr_req_tx)
        } else {
            None
        };
        let (dir_tx, directive_rx) = mpsc::channel();
        let (write_tx, write_req_rx) = tokio::sync::mpsc::unbounded_channel();
        let (write_out_tx, write_rx) = mpsc::channel();
        director::spawn_writer(
            &self.runtime,
            director::Config::from_env(),
            write_req_rx,
            write_out_tx,
        );
        let (reflect_tx, reflect_req_rx) = tokio::sync::mpsc::unbounded_channel();
        let (reflect_out_tx, reflect_rx) = mpsc::channel();
        director::spawn_reflector(
            &self.runtime,
            director::Config::from_env(),
            reflect_req_rx,
            reflect_out_tx,
        );
        let (obs_tx, obs_rx) = watch::channel(Observation::default());
        let forced_mode = settings.behaviour.forced_mode();
        let director_cfg = director::Config::from_env();
        let director_model = director_cfg.model.clone();
        let director_active = Arc::new(AtomicBool::new(forced_mode.is_none()));
        let on_battery = macos::on_battery();
        let saving_power = Arc::new(AtomicBool::new(settings.power_saving.active(on_battery)));
        if !agent.hold_still {
            director::spawn(
                &self.runtime,
                director_cfg,
                obs_rx,
                dir_tx,
                director_active.clone(),
                saving_power.clone(),
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
            scale_factor,
            ground_y,
            settings.size.scale(),
            mode.as_str(),
            ax,
            ocr_allowed,
        );

        let now = Instant::now();
        let (ax_tx, ax_rx) = macos::spawn_ax_queries();
        self.scene = Some(Scene {
            overlays,
            renderer,
            world: World::new(),
            agent,
            particles: Particles::new(seed ^ 0xA5A5),
            // World coordinates are global: no origin offset.
            cursor: Cursor::new(V2::ZERO),
            layout,
            screen_frames,
            screens_checked: now,
            windows_rx,
            snapshot: Snapshot::default(),
            ocr_tx,
            ocr_rx,
            words: WordPlay::new(seed ^ 0x5157),
            speech: Speech::new(seed ^ 0x7A11),
            doodles: Doodles::default(),
            rival: None,
            squad: None,
            next_minions: now + Duration::from_secs(240),
            boss: None,
            boss_banner: None,
            next_boss: now + Duration::from_secs(600),
            retreating: false,
            last_initiative: now,
            next_initiative_check: now + Duration::from_secs(5),
            war_clock: now,
            war_hit: None,
            incinerating: None,
            crumbled: false,
            rival_speech: Speech::new(seed ^ 0xBEEF),
            rival_click: false,
            next_rival: now + RIVAL_FIRST,
            sketch: Sketchpad::default(),
            drawing: None,
            jail: None,
            intro_combat_until: None,
            zzz_sprites: Vec::new(),
            last_director_line: now,
            sound,
            presence: stickman::music::spawn_watcher(),
            on_call: false,
            night: false,
            night_checked: now - Duration::from_secs(120),
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
            saving_power,
            on_battery,
            power_checked: Instant::now(),
            director_model,
            last_reason: String::new(),
            director_online: None,
            settings,
            settings_path,
            stats,
            stats_saved: now,
            mind,
            reflect_tx,
            write_tx,
            write_rx,
            recap_due: false,
            reflect_rx,
            last_reflection: now,
            reflecting: false,
            mind_saved: now,
            memories_at_reflection: 0,
            ignored_noted: false,
            menubar,
            menubar_logged: false,
            settings_window: None,
            accessibility: ax,
            screen_recording: ocr_allowed,
            seed,
            os_tx: macos::os_actions(),
            mover: macos::WindowMover::spawn(),
            ax_tx,
            ax_rx,
            dock_pid: if ax { macos::dock_pid() } else { None },
            perms_checked: now,
            screen_recording_now: ocr_allowed,
            first_run,
            dock_polled: now - Duration::from_secs(10),
            dock_span: None,
            flight: None,
            sword: None,
            screen_letters: Vec::new(),
            note: None,
            pending_note: None,
            next_note: now + Duration::from_secs(120),
            sword_pending: None,
            sword_retry: now,
            force_catch_clicks: !self.click_through,
            pending_click: false,
            press: None,
            dragging: false,
            captive: Captive::Free,
            dodger: stickman::dodge::Dodger::new(now.elapsed().as_nanos() as u64 ^ 0x5eed),
            dodges: std::collections::VecDeque::new(),
            arsenal: stickman::arsenal::Arsenal::new(now.elapsed().as_nanos() as u64 ^ 0xa75e),
            paint: stickman::arsenal::PAINTS[0],
            saw_pending: None,
            sawn: Vec::new(),
            quit_at: None,
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
            started: now,
            debug_done: false,
            ticks: 0,
            stats_frames: 0,
            stats_at: now,
        });
        if let Some(scene) = self.scene.as_mut() {
            scene.start_jail();
        }
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::Resized(size) => scene.renderer.resize(id, size.width, size.height),
            // Finish draw mode on the right button's release, so neither half
            // of the click leaks through to the app underneath.
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
                ..
            } if scene.drawing.is_some() => scene.end_drawing(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if scene.drawing.is_some() => {
                scene.sketch.begin(scene.cursor.pos);
                scene.drawing = Some(Instant::now());
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => scene.end_press(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Only reaches us while the cursor is over the figure. A quick
                // release is a hit; holding or dragging picks the figure up.
                log::debug!("press on figure at {:?}", scene.cursor.pos);
                scene.press = Some((scene.cursor.pos, Instant::now()));
                if let Some(w) = scene.snapshot.active() {
                    scene.focus_restore = Some((w.pid, Instant::now()));
                }
            }
            // Scrolling over the figure squashes or stretches it.
            WindowEvent::MouseWheel { delta, .. } if scene.settings.squish => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 20.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                let mut ev = Vec::new();
                scene.agent.squish(dy, &mut ev);
                scene.handle(&ev);
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
        if let Some(screens) = scene.screens_changed() {
            self.rebuild_overlays(event_loop, screens);
        }
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
            // Saving power: half the frame rate.
            let saving = scene.saving_power.load(Ordering::Relaxed);
            let interval = match (scene.busy, saving) {
                (true, false) => TICK,
                (false, false) | (true, true) => IDLE_TICK,
                (false, true) => IDLE_TICK * 2,
            };
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
    use stickman::stickman::WALKER_RGB;

    #[test]
    fn text_under_our_settings_window_is_cut_out() {
        let line = |x0, x1, y| Platform {
            x0,
            x1,
            y,
            kind: PlatformKind::Text,
            owner: 7,
        };
        let settings = Rect {
            x: 400.0,
            y: 200.0,
            width: 300.0,
            height: 400.0,
        };
        let out = uncovered(
            &[
                line(100.0, 1000.0, 300.0),
                line(100.0, 1000.0, 700.0),
                line(450.0, 600.0, 250.0),
            ],
            &[settings],
        );
        // Split around it; the one below it is untouched; the one inside is gone.
        assert_eq!(out.len(), 3);
        assert!(out.iter().any(|p| p.y == 300.0 && p.x1 == 400.0));
        assert!(out.iter().any(|p| p.y == 300.0 && p.x0 == 700.0));
        assert!(out
            .iter()
            .any(|p| p.y == 700.0 && p.x0 == 100.0 && p.x1 == 1000.0));
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
    fn thrown_cursor_arcs_and_lands_on_screen() {
        let size = Rect {
            x: 0.0,
            y: 0.0,
            width: 1800.0,
            height: 1169.0,
        };
        let (mut pos, mut vel, mut t) = (v2(900.0, 900.0), v2(900.0, -1100.0), 0.0);
        let mut peak = pos.y;
        let mut steps = 0;
        while fly(&mut pos, &mut vel, &mut t, size, FIXED_DT) {
            peak = peak.min(pos.y);
            steps += 1;
            assert!(pos.x >= 2.0 && pos.x <= 1798.0 && pos.y >= 2.0 && pos.y <= 1167.0);
        }
        assert!(peak < 600.0, "rose to {peak}");
        assert!(pos.x > 1200.0, "travelled to {}", pos.x);
        assert!(steps as f32 * FIXED_DT <= MAX_FLIGHT + 1e-3);
        // Bounces off the side wall instead of leaving the screen.
        let (mut pos, mut vel, mut t) = (v2(1700.0, 500.0), v2(3000.0, 0.0), 0.0);
        while fly(&mut pos, &mut vel, &mut t, size, FIXED_DT) {}
        assert!(pos.x <= 1798.0);
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
            top: 0.0,
            ammo: None,
            can_grab_cursor: false,
            held: false,
            user_idle: 0.0,
            can_shove: false,
            can_cling: false,
            can_gadget: false,
            can_saw: false,
            tk_words_near: 0,
            music: false,
            can_throw_window: false,
            weapon_available: false,
            dread: stickman::agent::Dread::None,
            dread_level: 0.0,
            dock: None,
            tactic: stickman::agent::Tactic::Any,
            retreat: false,
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
