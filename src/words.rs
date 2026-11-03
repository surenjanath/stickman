//! Ripping words off the screen.
//!
//! The figure grabs a word (or a single letter) from the active window — any app,
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
/// Thrown words rest on the ground this long before fading away.
const REST_TIME: f32 = 3.0;
const FADE_TIME: f32 = 0.8;

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
    pub resting: f32,
    pub age: f32,
}

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
    pending_throw: Option<(V2, V2)>,
    rng: u64,
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
    pub fn new(seed: u64) -> WordPlay {
        WordPlay {
            rng: seed | 1,
            next_id: 1,
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
        !self.projectiles.is_empty()
            || self.carried.is_some()
            || self.holes.iter().any(|h| h.age > HOLE_LIFETIME)
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
        // The chosen target may be gone or renumbered: choose again.
        self.target = None;
    }

    pub fn clear_words(&mut self) {
        self.words.clear();
        self.target = None;
    }

    /// Ammo for the figure: a word (sometimes just one letter) on the active
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
            if Some(*win) == active && *age < 10.0 {
                return Some(*a);
            }
            self.target = None;
        }
        let active = active?;
        let on_line = |w: &ScreenWord| {
            ground.is_some_and(|g| g.kind == PlatformKind::Text && (g.y - w.line_y).abs() < 2.0)
        };
        let best = self
            .words
            .iter()
            .filter(|w| {
                w.window == active && !self.holes.iter().any(|h| overlaps(&h.rect, &w.rect) > 0.3)
            })
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
        // Sometimes just pluck out a single letter.
        let rect = if best.chars > 2 && self.rand() < 0.35 {
            let i = (self.rand() * f32::from(best.chars)) as u16;
            crate::ocr::letter_box(best.rect, best.chars, i)
        } else {
            best.rect
        };
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
        let need = CaptureNeeded { id, rect, window };
        self.pending = Some(need);
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
        let Some(need) = self.pending.filter(|p| p.id == id) else {
            return false;
        };
        self.pending = None;
        let Some(sprite) = sprite else { return false };
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
        for h in &mut self.holes {
            h.age += dt;
        }
        self.holes.retain(|h| h.age < HOLE_LIFETIME + HOLE_FADE);
        for p in &mut self.projectiles {
            p.age += dt;
            if p.resting > 0.0 || (p.vel.length() < 1.0 && p.age > 0.5) {
                p.resting += dt;
                continue;
            }
            p.vel.y += PROJECTILE_GRAVITY * dt;
            let bottom = |pos: V2| pos + v2(0.0, p.half.y.min(p.half.x));
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
        self.projectiles.retain(|p| {
            let keep = p.resting < REST_TIME + FADE_TIME && p.age < 20.0;
            if !keep {
                freed.push(p.sprite);
            }
            keep
        });
        (events, freed)
    }

    /// Holes first (under everything), then carried and flying words as sprites.
    pub fn draw(&self, out: &mut DrawList, hands: Option<V2>) {
        for h in &self.holes {
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
        for p in &self.projectiles {
            let alpha = if p.resting > REST_TIME {
                1.0 - (p.resting - REST_TIME) / FADE_TIME
            } else {
                1.0
            };
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
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
