//! Ripping words off the screen.
//!
//! The figure grabs a whole word from the active window — any app,
//! including web pages — and throws it at the cursor. The word's pixels are
//! captured and keyed out, so the real glyphs fly; where it was, the overlay
//! paints a patch of the page's own background colour, so the word looks
//! *missing*. Nothing in the real app changes: the "hole" heals as soon as the
//! window moves, scrolls, loses focus, or after a while.
//!
//! This module is the bookkeeping (pure, testable). `main.rs` wires it to the OCR
//! worker (captures) and the renderer (sprites).

use crate::agent::{Ammo, PROJECTILE_GRAVITY};
use crate::geom::{v2, Rect, V2};
use crate::physics::{Platform, PlatformKind, World};
use crate::render::{DrawList, SpriteDraw};

/// How long a word stays missing before it "grows back".
pub const HOLE_LIFETIME: f32 = 45.0;
const HOLE_FADE: f32 = 1.5;
/// Default time thrown words rest where they land before fading away.
pub const DEFAULT_REST: f32 = 5.0;
const FADE_TIME: f32 = 0.8;
/// At most this many words lie around; the oldest fades when there are more.
pub const MAX_RESTING: usize = 40;

/// A word currently visible in the active window (overlay points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenWord {
    pub id: u64,
    pub rect: Rect,
    pub chars: u16,
    pub window: u32,
    /// Top of its text line (where the figure stands to grab it).
    pub line_y: f32,
}

/// The `window` of Dock icons (they belong to no app window).
pub const ICON_WINDOW: u32 = u32::MAX;

/// Where a word was ripped out: painted over with the page colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hole {
    pub rect: Rect,
    pub color: [f32; 3],
    pub window: u32,
    /// The window's frame when the word was taken; if it changes, the hole goes.
    pub window_rect: Rect,
    pub age: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projectile {
    pub sprite: u32,
    pub pos: V2,
    pub vel: V2,
    pub rot: f32,
    pub spin: f32,
    pub half: V2,
    pub hit: bool,
    /// Seconds lying still where it landed (0 while flying).
    pub resting: f32,
    /// Seconds into fading out, once it started.
    pub fade: Option<f32>,
    /// Seconds in flight since it last lay still.
    pub age: f32,
}

/// A word held up by telekinesis, orbiting the figure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hover {
    pub sprite: u32,
    pub half: V2,
    /// Where it was ripped from (it floats up from there).
    pub from: V2,
    pub t: f32,
    pub slot: usize,
    pub pos: V2,
}

/// Seconds for a lifted word to float from the page into orbit.
const HOVER_RISE: f32 = 0.7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Carried {
    pub sprite: u32,
    pub half: V2,
}

/// Something worth showing or logging.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WordEvent {
    /// A thrown word struck the cursor.
    HitCursor {
        at: V2,
        dir: V2,
    },
    Landed {
        at: V2,
    },
}

/// A capture the main loop must request from the OCR worker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureNeeded {
    pub id: u64,
    pub rect: Rect,
    pub window: u32,
}

impl CaptureNeeded {
    /// A Dock icon rather than a word.
    pub fn is_icon(&self) -> bool {
        self.window == ICON_WINDOW
    }
}

#[derive(Default)]
pub struct WordPlay {
    words: Vec<ScreenWord>,
    next_id: u64,
    pub holes: Vec<Hole>,
    pub projectiles: Vec<Projectile>,
    pub carried: Option<Carried>,
    /// Chosen ammo (stable across ticks): (ammo, exact rect to rip, window, age).
    target: Option<(Ammo, Rect, u32, f32)>,
    pending: Option<CaptureNeeded>,
    /// Seconds the pending capture has been waiting.
    pending_age: f32,
    pending_throw: Option<(V2, V2)>,
    rng: u64,
    /// How long landed words stay before fading; `None` keeps them for good.
    pub rest_time: Option<f32>,
    /// Dock icons the figure can rip up (when it stands on the Dock).
    icons: Vec<ScreenWord>,
    /// Icons thrown lately (rect, seconds ago): it picks others for a while.
    used_icons: Vec<(Rect, f32)>,
    /// Telekinesis: captures still on their way, and words in orbit.
    tk_pending: Vec<CaptureNeeded>,
    pub hovering: Vec<Hover>,
    /// What lifted words orbit (above the figure's head); set every tick.
    pub hover_center: V2,
    /// Volley in progress: (target, seconds until the next word goes).
    volley: Option<(V2, f32)>,
    /// Our own windows over the page (Settings): words under them are hidden.
    pub covers: Vec<Rect>,
}

fn overlaps(a: &Rect, b: &Rect) -> f64 {
    let x = (a.right().min(b.right()) - a.x.max(b.x)).max(0.0);
    let y = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0);
    let inter = x * y;
    let small = (a.width * a.height).min(b.width * b.height).max(1e-6);
    inter / small
}

fn center(r: &Rect) -> V2 {
    v2((r.x + r.width / 2.0) as f32, (r.y + r.height / 2.0) as f32)
}

impl WordPlay {
    /// Under one of our own windows (any overlap: the capture would show it).
    fn covered(&self, r: &Rect) -> bool {
        self.covers.iter().any(|c| overlaps(c, r) > 0.0)
    }

    pub fn new(seed: u64) -> WordPlay {
        WordPlay {
            rng: seed | 1,
            next_id: 1,
            rest_time: Some(DEFAULT_REST),
            ..WordPlay::default()
        }
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn words(&self) -> &[ScreenWord] {
        &self.words
    }

    /// Something is moving or carried: render every frame.
    pub fn animating(&self) -> bool {
        // Words lying still don't need frames; flying or fading ones do.
        self.projectiles
            .iter()
            .any(|p| p.resting == 0.0 || p.fade.is_some())
            || self.carried.is_some()
            || !self.hovering.is_empty()
            || self.holes.iter().any(|h| h.age > HOLE_LIFETIME)
    }

    /// Words telekinesis could lift: on the active window, within `radius` of
    /// `near`, not already taken.
    pub fn tk_candidates(&self, near: V2, radius: f32, active: Option<u32>) -> usize {
        self.tk_words(near, radius, active).count()
    }

    fn tk_words(
        &self,
        near: V2,
        radius: f32,
        active: Option<u32>,
    ) -> impl Iterator<Item = &ScreenWord> {
        let target = self.target.map(|t| t.0.id);
        self.words.iter().filter(move |w| {
            Some(w.window) == active
                && !self.covered(&w.rect)
                && Some(w.id) != target
                && (center(&w.rect) - near).length() < radius
                && !self.holes.iter().any(|h| overlaps(&h.rect, &w.rect) > 0.3)
        })
    }

    /// Lift up to `max` words near `near` with telekinesis. Returns the
    /// captures to request; each arrives through [`captured`](Self::captured).
    pub fn tk_lift(
        &mut self,
        near: V2,
        radius: f32,
        max: usize,
        active: Option<u32>,
    ) -> Vec<CaptureNeeded> {
        let mut picked: Vec<ScreenWord> = self.tk_words(near, radius, active).copied().collect();
        picked.sort_by(|a, b| {
            (center(&a.rect) - near)
                .length()
                .total_cmp(&(center(&b.rect) - near).length())
        });
        picked.truncate(max);
        let needs: Vec<CaptureNeeded> = picked
            .iter()
            .map(|w| CaptureNeeded {
                id: w.id,
                rect: w.rect,
                window: w.window,
            })
            .collect();
        self.words.retain(|w| !picked.iter().any(|p| p.id == w.id));
        self.tk_pending.extend(needs.iter().copied());
        needs
    }

    /// Fire every lifted word at `target`, one after another.
    pub fn tk_fling(&mut self, target: V2) {
        self.volley = Some((target, 0.0));
        self.tk_pending.clear(); // late captures just drop
    }

    /// Telekinesis broke (the figure got hit): lifted words fall.
    pub fn tk_drop(&mut self) {
        self.tk_pending.clear();
        self.volley = None;
        for h in std::mem::take(&mut self.hovering) {
            self.launch(
                Carried {
                    sprite: h.sprite,
                    half: h.half,
                },
                h.pos,
                v2(0.0, 0.0),
            );
        }
    }

    /// Carrying something, or waiting for its pixels.
    pub fn holding_anything(&self) -> bool {
        self.carried.is_some() || self.pending.is_some()
    }

    /// The capture on its way for `id` is a Dock icon.
    pub fn pending_icon(&self, id: u64) -> bool {
        self.pending.is_some_and(|p| p.id == id && p.is_icon())
    }

    pub fn tk_active(&self) -> bool {
        !self.hovering.is_empty() || !self.tk_pending.is_empty()
    }

    /// Orbit position for slot `i` of `n` at time `t`.
    fn orbit(&self, i: usize, n: usize, t: f32) -> V2 {
        let a = i as f32 / n.max(1) as f32 * std::f32::consts::TAU + t * 1.6;
        self.hover_center
            + v2(
                a.cos() * 85.0,
                a.sin() * 30.0 + (t * 3.0 + i as f32).sin() * 4.0,
            )
    }

    /// New OCR words for `window` (overlay rects). `text` are the text-line
    /// platforms, used to know which line each word sits on. Holes in this
    /// window whose word is no longer where it was (the page scrolled) heal.
    pub fn set_words(&mut self, window: u32, words: &[(Rect, u16)], text: &[Platform]) {
        self.holes.retain(|h| {
            h.window != window || words.iter().any(|(r, _)| overlaps(r, &h.rect) > 0.5)
        });
        let mut out = Vec::with_capacity(words.len());
        for (rect, chars) in words {
            let c = center(rect);
            let line = text
                .iter()
                .filter(|p| p.kind == PlatformKind::Text && p.owner == window && p.contains_x(c.x))
                .filter(|p| (f64::from(p.y) - rect.y).abs() < rect.height * 0.8)
                .min_by(|a, b| {
                    (f64::from(a.y) - rect.y)
                        .abs()
                        .total_cmp(&(f64::from(b.y) - rect.y).abs())
                });
            if let Some(line) = line {
                out.push(ScreenWord {
                    id: self.next_id,
                    rect: *rect,
                    chars: *chars,
                    window,
                    line_y: line.y,
                });
                self.next_id += 1;
            }
        }
        self.words = out;
        // A word target may be gone or renumbered: choose again.
        if self.target.is_some_and(|t| t.2 != ICON_WINDOW) {
            self.target = None;
        }
    }

    /// Dock icons (global rects) and the floor line each sits under. Icons
    /// that didn't move keep their ids, so a chosen target stays valid.
    pub fn set_icons(&mut self, icons: &[(Rect, f32)]) {
        let mut out = Vec::with_capacity(icons.len());
        for (rect, floor) in icons {
            if self.used_icons.iter().any(|(r, _)| r == rect) {
                continue; // thrown lately: give the others a turn
            }
            let id = match self.icons.iter().find(|i| i.rect == *rect) {
                Some(i) => i.id,
                None => {
                    self.next_id += 1;
                    self.next_id
                }
            };
            out.push(ScreenWord {
                id,
                rect: *rect,
                chars: 0,
                window: ICON_WINDOW,
                line_y: *floor,
            });
        }
        self.icons = out;
        if self
            .target
            .is_some_and(|t| t.2 == ICON_WINDOW && !self.icons.iter().any(|i| i.id == t.0.id))
        {
            log::debug!("icons: target gone after a Dock re-read");
            self.target = None;
        }
    }

    pub fn clear_words(&mut self) {
        self.words.clear();
        // An icon target stays: icons aren't words.
        if self.target.is_some_and(|t| t.2 != ICON_WINDOW) {
            self.target = None;
        }
    }

    /// Ammo for the figure: a whole word on the active
    /// window, not already ripped out, preferring the line it stands on, then the
    /// nearest. Stays on the same target until it's taken or goes stale.
    pub fn ammo(
        &mut self,
        dt: f32,
        figure: V2,
        ground: Option<Platform>,
        active: Option<u32>,
    ) -> Option<Ammo> {
        if self.pending.is_some() || self.carried.is_some() {
            return None;
        }
        if let Some((a, _, win, age)) = &mut self.target {
            *age += dt;
            let hidden = self.covers.iter().any(|c| {
                overlaps(
                    c,
                    &Rect {
                        x: f64::from(a.at.x) - 1.0,
                        y: f64::from(a.at.y) - 1.0,
                        width: 2.0,
                        height: 2.0,
                    },
                ) > 0.0
            });
            if (*win == ICON_WINDOW || Some(*win) == active) && *age < 10.0 && !hidden {
                return Some(*a);
            }
            self.target = None;
        }
        // Words on the line underfoot (or Dock icons under the floor it
        // stands on) first, then the nearest.
        let on_line = |w: &ScreenWord| {
            ground.is_some_and(|g| {
                matches!(g.kind, PlatformKind::Text | PlatformKind::Ground)
                    && (g.y - w.line_y).abs() < 2.0
                    && g.contains_x(center(&w.rect).x)
            })
        };
        let best = self
            .words
            .iter()
            .filter(|w| {
                Some(w.window) == active
                    && !self.covered(&w.rect)
                    && !self.holes.iter().any(|h| overlaps(&h.rect, &w.rect) > 0.3)
            })
            .chain(self.icons.iter().filter(|w| !self.covered(&w.rect)))
            .min_by(|a, b| {
                let score = |w: &ScreenWord| {
                    let d = (center(&w.rect) - figure).length();
                    if on_line(w) {
                        d - 10_000.0
                    } else {
                        d
                    }
                };
                score(a).total_cmp(&score(b))
            })
            .copied()?;
        // Always whole words: they read better in flight than single letters.
        let rect = best.rect;
        let ammo = Ammo {
            id: best.id,
            at: center(&rect),
            line_y: best.line_y,
        };
        self.target = Some((ammo, rect, best.window, 0.0));
        Some(ammo)
    }

    /// The figure grabbed word `id`: returns the capture to request.
    pub fn grabbed(&mut self, id: u64) -> Option<CaptureNeeded> {
        let (ammo, rect, window, _) = self.target.take()?;
        if ammo.id != id {
            return None;
        }
        self.words.retain(|w| w.id != id);
        if self.icons.iter().any(|w| w.id == id) {
            self.used_icons.push((rect, 0.0));
        }
        self.icons.retain(|w| w.id != id);
        let need = CaptureNeeded { id, rect, window };
        self.pending = Some(need);
        self.pending_age = 0.0;
        Some(need)
    }

    /// The capture for `id` arrived. `sprite` is the uploaded cutout (or `None` if
    /// the capture failed: the grab fizzles), `background` the page colour.
    /// `window_rect` is the window's current frame, `holding`/`hands` the figure's
    /// state. Returns false if the grab fizzled (the figure should let go).
    pub fn captured(
        &mut self,
        id: u64,
        sprite: Option<u32>,
        background: [u8; 3],
        window_rect: Rect,
        holding: bool,
        hands: V2,
    ) -> bool {
        if let Some(i) = self.tk_pending.iter().position(|p| p.id == id) {
            let need = self.tk_pending.remove(i);
            let Some(sprite) = sprite else { return false };
            self.holes.push(Hole {
                rect: need.rect,
                color: background.map(|c| f32::from(c) / 255.0),
                window: need.window,
                window_rect,
                age: 0.0,
            });
            let pad = crate::ocr::GRAB_PAD_Y as f32;
            let from = center(&need.rect);
            self.hovering.push(Hover {
                sprite,
                half: v2(
                    need.rect.width as f32 / 2.0,
                    need.rect.height as f32 / 2.0 + pad,
                ),
                from,
                t: 0.0,
                slot: self.hovering.len(),
                pos: from,
            });
            return true;
        }
        let Some(need) = self.pending.filter(|p| p.id == id) else {
            return false;
        };
        self.pending = None;
        let Some(sprite) = sprite else { return false };
        if need.is_icon() {
            // A copy of the icon: the Dock keeps its own.
            let half = v2(need.rect.width as f32 / 2.0, need.rect.height as f32 / 2.0);
            let carried = Carried { sprite, half };
            if let Some((from, vel)) = self.pending_throw.take() {
                self.launch(carried, from, vel);
            } else if holding {
                self.carried = Some(carried);
            } else {
                self.launch(carried, hands, v2(0.0, -80.0));
            }
            return true;
        }
        self.holes.push(Hole {
            rect: need.rect,
            color: background.map(|c| f32::from(c) / 255.0),
            window: need.window,
            window_rect,
            age: 0.0,
        });
        // The capture is padded vertically into the line spacing.
        let pad = crate::ocr::GRAB_PAD_Y as f32;
        let half = v2(
            need.rect.width as f32 / 2.0,
            need.rect.height as f32 / 2.0 + pad,
        );
        let carried = Carried { sprite, half };
        if let Some((from, vel)) = self.pending_throw.take() {
            self.launch(carried, from, vel);
        } else if holding {
            self.carried = Some(carried);
        } else {
            self.launch(carried, hands, v2(0.0, -80.0)); // dropped while capturing
        }
        true
    }

    /// Throw any sprite (a dropped or thrown weapon) like a word.
    pub fn launch_sprite(&mut self, sprite: u32, half: V2, from: V2, vel: V2) {
        self.launch(Carried { sprite, half }, from, vel);
    }

    fn launch(&mut self, c: Carried, from: V2, vel: V2) {
        let spin = (8.0 + 6.0 * self.rand()) * if vel.x >= 0.0 { 1.0 } else { -1.0 };
        self.projectiles.push(Projectile {
            sprite: c.sprite,
            pos: from,
            vel,
            rot: 0.0,
            spin,
            half: c.half,
            hit: false,
            resting: 0.0,
            fade: None,
            age: 0.0,
        });
    }

    pub fn threw(&mut self, from: V2, vel: V2) {
        match self.carried.take() {
            Some(c) => self.launch(c, from, vel),
            None if self.pending.is_some() => self.pending_throw = Some((from, vel)),
            None => {}
        }
    }

    pub fn dropped(&mut self, at: V2) {
        if let Some(c) = self.carried.take() {
            self.launch(c, at, v2(0.0, -120.0));
        }
    }

    /// Holes only make sense over their window, unmoved and in front. Returns
    /// whether any hole changed (so the frame must be redrawn).
    pub fn validate_holes(&mut self, active: Option<(u32, Rect)>) -> bool {
        let before = self.holes.len();
        self.holes
            .retain(|h| active.is_some_and(|(id, r)| id == h.window && r == h.window_rect));
        before != self.holes.len()
    }

    /// Advance flying words by `dt`. Returns events and sprites to free.
    pub fn update(
        &mut self,
        dt: f32,
        cursor: V2,
        world: &World,
        min_x: f32,
        max_x: f32,
    ) -> (Vec<WordEvent>, Vec<u32>) {
        let mut events = Vec::new();
        let mut freed = Vec::new();
        for (_, age) in &mut self.used_icons {
            *age += dt;
        }
        self.used_icons.retain(|(_, age)| *age < 30.0);
        // Give up on a capture that never arrives (worker busy or missing).
        if self.pending.is_some() {
            self.pending_age += dt;
            if self.pending_age > 3.0 {
                self.pending = None;
                self.pending_throw = None;
            }
        }
        for h in &mut self.holes {
            h.age += dt;
        }
        self.holes.retain(|h| h.age < HOLE_LIFETIME + HOLE_FADE);
        // Lifted words float up from the page into orbit.
        let n = self.hovering.len();
        for i in 0..n {
            let h = self.hovering[i];
            let t = h.t + dt;
            let goal = self.orbit(h.slot, n, t);
            let k = crate::stickman::smoothstep(t / HOVER_RISE);
            self.hovering[i].t = t;
            self.hovering[i].pos = h.from.lerp(goal, k);
        }
        // The volley: one word every 0.09 s, straight-ish at the target.
        if let Some((target, wait)) = &mut self.volley {
            *wait -= dt;
            if *wait <= 0.0 {
                *wait = 0.09;
                let target = *target;
                if let Some(h) = (!self.hovering.is_empty()).then(|| self.hovering.remove(0)) {
                    let d = target - h.pos;
                    // Fast and flat: flight time grows gently with distance.
                    let t = (d.length() / 1800.0).clamp(0.15, 0.45);
                    let vel = v2(d.x / t, d.y / t - 0.5 * PROJECTILE_GRAVITY * t);
                    self.launch(
                        Carried {
                            sprite: h.sprite,
                            half: h.half,
                        },
                        h.pos,
                        vel,
                    );
                } else {
                    self.volley = None;
                }
            }
        }
        let rest_time = self.rest_time;
        for p in &mut self.projectiles {
            p.age += dt;
            if let Some(f) = &mut p.fade {
                *f += dt;
            }
            let bottom = |pos: V2| pos + v2(0.0, p.half.y.min(p.half.x));
            if p.resting > 0.0 || (p.vel.length() < 1.0 && p.age > 0.5) {
                p.resting += dt;
                if p.fade.is_none() && rest_time.is_some_and(|r| p.resting > r) {
                    p.fade = Some(0.0);
                }
                // The window it lay on moved or closed: fall again.
                if world.support(bottom(p.pos)).is_none() {
                    p.resting = 0.0;
                    p.age = 0.0;
                    p.vel = v2(0.0, 1.0);
                }
                continue;
            }
            p.vel.y += PROJECTILE_GRAVITY * dt;
            let next = p.pos + p.vel * dt;
            if !p.hit && (next - cursor).length() < p.half.x.max(p.half.y) + 14.0 {
                p.hit = true;
                let dir = p.vel.normalize_or(v2(1.0, 0.0));
                events.push(WordEvent::HitCursor { at: cursor, dir });
                p.vel = -p.vel * 0.25 + v2(0.0, -150.0);
                p.spin = -p.spin * 1.5;
            }
            if let Some(i) = world.landing(bottom(p.pos), bottom(next)) {
                let y = world.platforms()[i].y - p.half.y.min(p.half.x);
                let impact = p.vel.y;
                p.pos = v2(next.x, y);
                p.vel = v2(p.vel.x * 0.55, -impact * 0.3);
                p.spin *= 0.5;
                if p.vel.y.abs() < 80.0 {
                    p.vel = V2::ZERO;
                    p.spin = 0.0;
                    // Settle flat (squares on any side), so they stack.
                    p.rot = settle(p.rot, p.half);
                    p.resting = f32::EPSILON;
                    events.push(WordEvent::Landed { at: p.pos });
                }
            } else {
                p.pos = next;
            }
            if p.pos.x < min_x || p.pos.x > max_x {
                p.pos.x = p.pos.x.clamp(min_x, max_x);
                p.vel.x = -p.vel.x * 0.5;
            }
            p.rot += p.spin * dt;
        }
        // Too many lying around: the oldest starts fading.
        let resting = self
            .projectiles
            .iter()
            .filter(|p| p.resting > 0.0 && p.fade.is_none())
            .count();
        if resting > MAX_RESTING {
            if let Some(p) = self
                .projectiles
                .iter_mut()
                .filter(|p| p.resting > 0.0 && p.fade.is_none())
                .max_by(|a, b| a.resting.total_cmp(&b.resting))
            {
                p.fade = Some(0.0);
            }
        }
        self.projectiles.retain(|p| {
            let faded = p.fade.is_some_and(|f| f >= FADE_TIME);
            let lost = p.resting == 0.0 && p.age > 20.0; // flew off somewhere
            let keep = !faded && !lost;
            if !keep {
                freed.push(p.sprite);
            }
            keep
        });
        (events, freed)
    }

    /// The tops of words and icons lying still: platforms, so later throws
    /// land on them and piles grow (and the figure can climb them).
    pub fn pile_platforms(&self) -> Vec<Platform> {
        self.projectiles
            .iter()
            .filter(|p| p.resting > 0.0 && p.fade.is_none())
            .map(|p| {
                let (hw, hh) = if upright_sideways(p.rot) {
                    (p.half.y, p.half.x)
                } else {
                    (p.half.x, p.half.y)
                };
                Platform {
                    x0: p.pos.x - hw * 0.9,
                    x1: p.pos.x + hw * 0.9,
                    y: p.pos.y - hh,
                    kind: PlatformKind::Pile,
                    owner: 0,
                }
            })
            .collect()
    }

    /// Holes first (under everything), then carried and flying words as sprites.
    pub fn draw(&self, out: &mut DrawList, hands: Option<V2>) {
        // Our Settings window is in front of the page: no patches on it.
        for h in self.holes.iter().filter(|h| !self.covered(&h.rect)) {
            let alpha = if h.age > HOLE_LIFETIME {
                1.0 - (h.age - HOLE_LIFETIME) / HOLE_FADE
            } else {
                1.0
            };
            let c = center(&h.rect);
            let half = v2(
                h.rect.width as f32 / 2.0 + 1.5,
                h.rect.height as f32 / 2.0 + 1.0,
            );
            out.rect(
                c,
                half,
                2.0,
                [h.color[0], h.color[1], h.color[2], alpha.clamp(0.0, 1.0)],
            );
        }
        if let (Some(c), Some(hands)) = (self.carried, hands) {
            out.sprite(SpriteDraw {
                id: c.sprite,
                center: hands - v2(0.0, c.half.y + 2.0),
                half: c.half,
                rotation: 0.0,
                alpha: 1.0,
            });
        }
        for h in &self.hovering {
            // A faint aura behind each lifted word.
            out.rect(h.pos, h.half + V2::splat(5.0), 8.0, [0.65, 0.4, 1.0, 0.22]);
            out.sprite(SpriteDraw {
                id: h.sprite,
                center: h.pos,
                half: h.half,
                rotation: 0.12 * (h.t * 2.0 + h.slot as f32).sin(),
                alpha: 1.0,
            });
        }
        for p in &self.projectiles {
            let alpha = p.fade.map_or(1.0, |f| 1.0 - f / FADE_TIME);
            out.sprite(SpriteDraw {
                id: p.sprite,
                center: p.pos,
                half: p.half,
                rotation: p.rot,
                alpha: alpha.clamp(0.0, 1.0),
            });
        }
    }

    /// Sprites still in use (to free on shutdown/reset).
    pub fn sprites(&self) -> Vec<u32> {
        self.carried
            .iter()
            .map(|c| c.sprite)
            .chain(self.projectiles.iter().map(|p| p.sprite))
            .chain(self.hovering.iter().map(|h| h.sprite))
            .collect()
    }
}

/// The nearest resting angle: flat for words, any quarter turn for squares.
fn settle(rot: f32, half: V2) -> f32 {
    let q = if (half.x - half.y).abs() < 2.0 {
        std::f32::consts::FRAC_PI_2
    } else {
        std::f32::consts::PI
    };
    (rot / q).round() * q
}

/// Lying on its side (a quarter turn).
fn upright_sideways(rot: f32) -> bool {
    ((rot / std::f32::consts::FRAC_PI_2).round() as i64).rem_euclid(2) == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landed_words_pile_up() {
        let ground = Platform::ground(1800.0, 1000.0);
        let mut w = World::new();
        let mut play = WordPlay::new(1);
        play.rest_time = None;
        let half = v2(30.0, 10.0);
        play.launch_sprite(1, half, v2(500.0, 700.0), v2(0.0, 0.0));
        let step = |play: &mut WordPlay, w: &mut World| {
            let mut ps = vec![ground];
            ps.extend(play.pile_platforms());
            w.set_platforms(ps);
            play.update(1.0 / 60.0, v2(-999.0, -999.0), w, 0.0, 1800.0);
        };
        for _ in 0..180 {
            step(&mut play, &mut w);
        }
        assert_eq!(play.pile_platforms().len(), 1);
        let first = play.projectiles[0].pos.y;
        assert!((first - 990.0).abs() < 1.0, "on the ground: {first}");
        // A second word dropped on top lands on the first.
        play.launch_sprite(2, half, v2(505.0, 600.0), v2(0.0, 0.0));
        for _ in 0..180 {
            step(&mut play, &mut w);
        }
        let second = play.projectiles[1].pos.y;
        assert!(
            (second - (first - 20.0)).abs() < 1.5,
            "stacked: {second} on {first}"
        );
        assert!(play
            .pile_platforms()
            .iter()
            .all(|p| p.kind == PlatformKind::Pile));
    }

    const WIN: u32 = 7;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn line(x0: f32, x1: f32, y: f32) -> Platform {
        Platform {
            x0,
            x1,
            y,
            kind: PlatformKind::Text,
            owner: WIN,
        }
    }

    fn play_with_words() -> WordPlay {
        let mut p = WordPlay::new(3);
        let text = [line(100.0, 900.0, 400.0), line(100.0, 900.0, 440.0)];
        p.set_words(
            WIN,
            &[
                (r(120.0, 401.0, 60.0, 16.0), 5),
                (r(200.0, 401.0, 40.0, 16.0), 3),
                (r(120.0, 441.0, 80.0, 16.0), 6),
            ],
            &text,
        );
        p
    }

    #[test]
    fn words_get_their_line() {
        let p = play_with_words();
        assert_eq!(p.words().len(), 3);
        assert_eq!(p.words()[0].line_y, 400.0);
        assert_eq!(p.words()[2].line_y, 440.0);
    }

    #[test]
    fn ammo_prefers_the_line_underfoot_and_stays_stable() {
        let mut p = play_with_words();
        let g = Some(line(100.0, 900.0, 440.0));
        let a = p.ammo(0.016, v2(130.0, 400.0), g, Some(WIN)).unwrap();
        assert_eq!(a.line_y, 440.0, "should pick the word on its own line");
        let b = p.ammo(0.016, v2(800.0, 100.0), g, Some(WIN)).unwrap();
        assert_eq!(a, b, "target must not flicker between ticks");
        assert!(
            p.ammo(0.016, v2(0.0, 0.0), g, Some(99)).is_none(),
            "other window active"
        );
    }

    #[test]
    fn grab_capture_and_throw_flow() {
        let mut p = play_with_words();
        let a = p.ammo(0.016, v2(130.0, 400.0), None, Some(WIN)).unwrap();
        let need = p.grabbed(a.id).expect("capture requested");
        assert_eq!(need.window, WIN);
        assert!(
            p.ammo(0.016, v2(0.0, 0.0), None, Some(WIN)).is_none(),
            "busy while capturing"
        );
        assert!(p.captured(
            a.id,
            Some(42),
            [250, 250, 250],
            r(0.0, 0.0, 1000.0, 800.0),
            true,
            v2(0.0, 0.0)
        ));
        assert_eq!(p.holes.len(), 1);
        assert_eq!(p.carried.map(|c| c.sprite), Some(42));
        assert!(
            !p.words().iter().any(|w| w.id == a.id),
            "taken word is gone"
        );
        p.threw(v2(100.0, 300.0), v2(300.0, -200.0));
        assert!(p.carried.is_none());
        assert_eq!(p.projectiles.len(), 1);
        // The ripped-out spot is never chosen again.
        let next = p.ammo(0.016, v2(130.0, 400.0), None, Some(WIN)).unwrap();
        assert_ne!(next.id, a.id);
    }

    #[test]
    fn failed_capture_fizzles() {
        let mut p = play_with_words();
        let a = p.ammo(0.016, v2(130.0, 400.0), None, Some(WIN)).unwrap();
        p.grabbed(a.id).unwrap();
        assert!(!p.captured(a.id, None, [0; 3], r(0.0, 0.0, 1.0, 1.0), true, V2::ZERO));
        assert!(p.holes.is_empty() && p.carried.is_none());
    }

    #[test]
    fn throw_before_capture_arrives_still_flies() {
        let mut p = play_with_words();
        let a = p.ammo(0.016, v2(130.0, 400.0), None, Some(WIN)).unwrap();
        p.grabbed(a.id).unwrap();
        p.threw(v2(1.0, 2.0), v2(10.0, 0.0));
        p.captured(
            a.id,
            Some(5),
            [255; 3],
            r(0.0, 0.0, 1.0, 1.0),
            false,
            V2::ZERO,
        );
        assert_eq!(p.projectiles.len(), 1);
        assert_eq!(p.projectiles[0].pos, v2(1.0, 2.0));
    }

    #[test]
    fn holes_heal_when_the_window_moves_scrolls_or_loses_focus() {
        let win_rect = r(0.0, 0.0, 1000.0, 800.0);
        let mut p = play_with_words();
        let a = p.ammo(0.016, v2(130.0, 400.0), None, Some(WIN)).unwrap();
        p.grabbed(a.id).unwrap();
        p.captured(a.id, Some(1), [255; 3], win_rect, true, V2::ZERO);
        let hole = p.holes[0].rect;
        assert!(!p.validate_holes(Some((WIN, win_rect))));
        // Scroll: the next OCR pass no longer finds a word at the hole.
        let mut q = WordPlay::new(1);
        q.holes = p.holes.clone();
        q.set_words(
            WIN,
            &[(r(500.0, 600.0, 40.0, 16.0), 3)],
            &[line(0.0, 1000.0, 600.0)],
        );
        assert!(q.holes.is_empty(), "page scrolled: hole should heal");
        // Still there after a re-read: keep it.
        let mut k = WordPlay::new(1);
        k.holes = p.holes.clone();
        k.set_words(WIN, &[(hole, 4)], &[line(0.0, 1000.0, hole.y as f32)]);
        assert_eq!(k.holes.len(), 1);
        // Moved window / other app in front.
        assert!(p.validate_holes(Some((WIN, r(10.0, 0.0, 1000.0, 800.0)))));
        assert!(p.holes.is_empty());
    }

    #[test]
    fn thrown_word_hits_the_cursor_lands_and_fades() {
        let mut w = World::new();
        w.set_platforms(vec![Platform::ground(2000.0, 1000.0)]);
        let mut p = WordPlay::new(9);
        let from = v2(200.0, 900.0);
        let cursor = v2(700.0, 600.0);
        p.launch(
            Carried {
                sprite: 11,
                half: v2(20.0, 8.0),
            },
            from,
            crate::agent::lob_velocity(from, cursor),
        );
        let mut hit = false;
        let mut landed = false;
        let mut freed = vec![];
        for _ in 0..(12.0 * 60.0) as usize {
            let (ev, f) = p.update(1.0 / 60.0, cursor, &w, 0.0, 2000.0);
            hit |= ev.iter().any(|e| matches!(e, WordEvent::HitCursor { .. }));
            landed |= ev.iter().any(|e| matches!(e, WordEvent::Landed { .. }));
            freed.extend(f);
        }
        assert!(hit, "the lob should hit the cursor");
        assert!(landed, "then fall and land");
        assert_eq!(freed, [11], "and finally free its sprite");
        assert!(p.projectiles.is_empty());
    }

    fn drop_word(p: &mut WordPlay, sprite: u32, at: V2) {
        p.launch(
            Carried {
                sprite,
                half: v2(20.0, 8.0),
            },
            at,
            V2::ZERO,
        );
    }

    #[test]
    fn words_can_stay_forever_and_fall_when_their_window_goes() {
        let ground = Platform::ground(2000.0, 1000.0);
        let window = Platform {
            x0: 100.0,
            x1: 600.0,
            y: 500.0,
            kind: PlatformKind::Window,
            owner: 3,
        };
        let mut w = World::new();
        w.set_platforms(vec![ground, window]);
        let mut p = WordPlay::new(1);
        p.rest_time = None;
        drop_word(&mut p, 5, v2(300.0, 450.0));
        let step = |p: &mut WordPlay, w: &World, secs: f32| {
            let mut freed = vec![];
            for _ in 0..(secs * 60.0) as usize {
                freed.extend(p.update(1.0 / 60.0, v2(-999.0, -999.0), w, 0.0, 2000.0).1);
            }
            freed
        };
        assert!(step(&mut p, &w, 120.0).is_empty(), "faded despite Forever");
        assert_eq!(p.projectiles.len(), 1);
        assert!(
            (p.projectiles[0].pos.y - 492.0).abs() < 1.0,
            "resting on the window"
        );
        assert!(!p.animating(), "a word lying still needs no frames");
        // The window closes: the word drops to the ground and stays there.
        w.set_platforms(vec![ground]);
        step(&mut p, &w, 3.0);
        assert!(
            (p.projectiles[0].pos.y - 992.0).abs() < 1.0,
            "{:?}",
            p.projectiles[0].pos
        );
        // A timed setting fades it.
        p.rest_time = Some(15.0);
        let freed = step(&mut p, &w, 17.0);
        assert_eq!(freed, [5]);
    }

    #[test]
    fn only_so_many_words_lie_around() {
        let mut w = World::new();
        w.set_platforms(vec![Platform::ground(4000.0, 1000.0)]);
        let mut p = WordPlay::new(1);
        p.rest_time = None;
        for i in 0..(MAX_RESTING as u32 + 5) {
            drop_word(&mut p, i, v2(50.0 + 60.0 * i as f32, 980.0));
        }
        let mut freed = vec![];
        for _ in 0..(10 * 60) {
            freed.extend(p.update(1.0 / 60.0, v2(-999.0, -999.0), &w, 0.0, 4000.0).1);
        }
        assert_eq!(p.projectiles.len(), MAX_RESTING);
        assert_eq!(freed.len(), 5);
    }

    #[test]
    fn telekinesis_lifts_a_group_of_words_and_fires_them_at_the_cursor() {
        let mut w = World::new();
        w.set_platforms(vec![Platform::ground(2000.0, 1000.0)]);
        let mut p = play_with_words();
        let near = v2(300.0, 300.0);
        let available = p.tk_candidates(near, 2000.0, Some(WIN));
        assert!(available >= 3, "{available}");
        let needs = p.tk_lift(near, 2000.0, 3, Some(WIN));
        assert_eq!(needs.len(), 3);
        assert_eq!(p.tk_candidates(near, 2000.0, Some(WIN)), available - 3);
        let wr = r(0.0, 0.0, 1000.0, 800.0);
        for (i, n) in needs.iter().enumerate() {
            assert!(p.captured(n.id, Some(20 + i as u32), [255; 3], wr, false, V2::ZERO));
        }
        assert_eq!(p.hovering.len(), 3);
        assert_eq!(p.holes.len(), 3, "each leaves a hole");
        p.hover_center = v2(600.0, 500.0);
        for _ in 0..60 {
            p.update(1.0 / 60.0, v2(-999.0, -999.0), &w, 0.0, 2000.0);
        }
        for h in &p.hovering {
            assert!(
                (h.pos - p.hover_center).length() < 100.0,
                "not in orbit: {:?}",
                h.pos
            );
        }
        let cursor = v2(1400.0, 300.0);
        p.tk_fling(cursor);
        let mut hits = 0;
        for _ in 0..120 {
            let (ev, _) = p.update(1.0 / 60.0, cursor, &w, 0.0, 2000.0);
            hits += ev
                .iter()
                .filter(|e| matches!(e, WordEvent::HitCursor { .. }))
                .count();
        }
        assert!(p.hovering.is_empty());
        assert!(hits >= 2, "only {hits} of 3 words hit the cursor");
    }

    #[test]
    fn broken_telekinesis_drops_the_words() {
        let mut p = play_with_words();
        let needs = p.tk_lift(v2(300.0, 300.0), 2000.0, 2, Some(WIN));
        let wr = r(0.0, 0.0, 1000.0, 800.0);
        p.captured(needs[0].id, Some(1), [255; 3], wr, false, V2::ZERO);
        p.tk_drop();
        assert!(p.hovering.is_empty() && !p.tk_active());
        assert_eq!(p.projectiles.len(), 1, "falls instead of vanishing");
        // The other capture arriving late is not lifted.
        assert!(!p.captured(needs[1].id, Some(2), [255; 3], wr, false, V2::ZERO));
    }

    #[test]
    fn dock_icons_underfoot_become_ammo_without_holes() {
        let mut p = WordPlay::new(4);
        let ground = Platform::ground(1800.0, 1100.0);
        let icon = r(700.0, 1102.0, 40.0, 40.0);
        p.set_icons(&[(icon, 1100.0), (r(760.0, 1102.0, 40.0, 40.0), 1100.0)]);
        let a = p
            .ammo(0.016, v2(705.0, 1100.0), Some(ground), None)
            .expect("an icon");
        assert!((a.at - v2(720.0, 1122.0)).length() < 1.0);
        assert_eq!(a.line_y, 1100.0);
        // Re-reading the Dock keeps the id while the icon stays put.
        p.set_icons(&[(icon, 1100.0), (r(760.0, 1102.0, 40.0, 40.0), 1100.0)]);
        let need = p.grabbed(a.id).expect("capture needed");
        assert!(need.is_icon());
        assert!(p.captured(a.id, Some(9), [0; 3], r(0.0, 0.0, 1.0, 1.0), true, V2::ZERO));
        assert!(p.holes.is_empty(), "no hole in the Dock");
        assert!(p.carried.is_some());
        // Words on the line underfoot still beat icons further away.
        let mut q = play_with_words();
        q.set_icons(&[(icon, 1100.0)]);
        let line = Platform {
            x0: 100.0,
            x1: 900.0,
            y: 400.0,
            kind: PlatformKind::Text,
            owner: WIN,
        };
        let a = q
            .ammo(0.016, v2(150.0, 400.0), Some(line), Some(WIN))
            .unwrap();
        assert_eq!(a.line_y, 400.0, "picked the word underfoot");
    }

    #[test]
    fn holes_expire_and_draw_under_sprites() {
        let mut p = play_with_words();
        p.holes.push(Hole {
            rect: r(0.0, 0.0, 10.0, 10.0),
            color: [1.0; 3],
            window: WIN,
            window_rect: r(0.0, 0.0, 1.0, 1.0),
            age: 0.0,
        });
        p.carried = Some(Carried {
            sprite: 3,
            half: v2(5.0, 5.0),
        });
        let mut dl = DrawList::default();
        p.draw(&mut dl, Some(v2(0.0, 0.0)));
        assert_eq!(dl.len(), 2);
        let w = World::new();
        p.update(HOLE_LIFETIME + HOLE_FADE + 0.1, V2::ZERO, &w, 0.0, 100.0);
        assert!(p.holes.is_empty());
    }
}
