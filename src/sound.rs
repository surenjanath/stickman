//! Sound effects, synthesised in code (no audio files): punches, whooshes,
//! explosions, thuds, pops, a babble for speech and a "wah-wah" for knockouts.
//!
//! The main thread only sends [`Sfx`] requests over a channel. A dedicated
//! `stickman-sound` thread owns the CoreAudio stream (via `cpal`), renders each
//! effect once per variant and mixes active voices in the audio callback. The
//! stream is paused after a couple of quiet seconds so an idle figure costs
//! nothing.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// A sound effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sfx {
    /// Fist connects.
    Punch,
    /// Foot connects (deeper).
    Kick,
    /// A fast limb or body moving through the air.
    Whoosh,
    /// The user's click: knockback explosion.
    Boom,
    /// Landing or bouncing.
    Thud,
    /// Ripping a word off the screen / catching the cursor.
    Pop,
    /// Stomping on a window.
    Stomp,
    /// Talking (a little babble).
    Babble,
    /// Knocked out.
    Ko,
}

impl Sfx {
    pub const ALL: [Sfx; 9] = [
        Sfx::Punch,
        Sfx::Kick,
        Sfx::Whoosh,
        Sfx::Boom,
        Sfx::Thud,
        Sfx::Pop,
        Sfx::Stomp,
        Sfx::Babble,
        Sfx::Ko,
    ];
}

/// Variants rendered per effect (slightly different pitch and noise), so
/// repeated hits don't sound like a loop.
const VARIANTS: u32 = 3;
const MAX_VOICES: usize = 12;
const MASTER: f32 = 0.55;
/// Pause the stream after this much silence.
const IDLE_PAUSE: Duration = Duration::from_secs(2);

struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// One-pole low-pass.
struct Lowpass {
    y: f32,
    k: f32,
}

impl Lowpass {
    fn new(cutoff: f32, rate: f32) -> Lowpass {
        Lowpass {
            y: 0.0,
            k: 1.0 - (-TAU * cutoff / rate).exp(),
        }
    }

    fn set(&mut self, cutoff: f32, rate: f32) {
        self.k = 1.0 - (-TAU * cutoff / rate).exp();
    }

    fn run(&mut self, x: f32) -> f32 {
        self.y += (x - self.y) * self.k;
        self.y
    }
}

/// Exponential decay envelope with a short linear attack.
fn env(t: f32, attack: f32, decay: f32) -> f32 {
    if t < attack {
        t / attack
    } else {
        (-(t - attack) / decay).exp()
    }
}

/// A sine whose frequency glides exponentially from `f0` to `f1` over `len` s.
struct Glide {
    phase: f32,
}

impl Glide {
    fn at(&mut self, t: f32, f0: f32, f1: f32, len: f32, rate: f32) -> f32 {
        let f = f0 * (f1 / f0).powf((t / len).min(1.0));
        self.phase = (self.phase + f / rate).fract();
        (self.phase * TAU).sin()
    }
}

/// Render `sfx` (mono, at `rate` Hz). `variant` nudges pitch and noise.
pub fn synth(sfx: Sfx, rate: u32, variant: u32) -> Vec<f32> {
    let r = rate as f32;
    let pitch = 1.0 + 0.07 * (variant as f32 - 1.0);
    let mut noise = Noise(0x9E37_79B9 ^ (variant + 1).wrapping_mul(0x85EB_CA6B) ^ sfx as u32);
    let len = match sfx {
        Sfx::Punch => 0.11,
        Sfx::Kick => 0.16,
        Sfx::Whoosh => 0.22,
        Sfx::Boom => 0.6,
        Sfx::Thud => 0.12,
        Sfx::Pop => 0.08,
        Sfx::Stomp => 0.2,
        Sfx::Babble => 0.36,
        Sfx::Ko => 0.75,
    };
    let n = (len * r) as usize;
    let mut out = Vec::with_capacity(n);
    let mut lp = Lowpass::new(2000.0, r);
    let mut lp2 = Lowpass::new(400.0, r);
    let mut tone = Glide { phase: 0.0 };
    let mut tone2 = Glide { phase: 0.0 };
    for i in 0..n {
        let t = i as f32 / r;
        let s = match sfx {
            Sfx::Punch => {
                let body = tone.at(t, 190.0 * pitch, 70.0, 0.08, r) * env(t, 0.002, 0.04);
                let smack = lp.run(noise.next()) * env(t, 0.001, 0.018) * 1.6;
                0.9 * body + smack
            }
            Sfx::Kick => {
                let body = tone.at(t, 140.0 * pitch, 48.0, 0.12, r) * env(t, 0.003, 0.06);
                lp.set(1200.0, r);
                let smack = lp.run(noise.next()) * env(t, 0.001, 0.03) * 1.4;
                body + smack
            }
            Sfx::Whoosh => {
                // Band-passed noise whose centre sweeps up then down.
                let u = t / len;
                let centre = 500.0 + 2200.0 * pitch * (u * std::f32::consts::PI).sin();
                lp.set(centre, r);
                lp2.set(centre * 0.35, r);
                let x = noise.next();
                let band = lp.run(x) - lp2.run(x);
                band * (u * std::f32::consts::PI).sin().powi(2) * 2.2
            }
            Sfx::Boom => {
                lp.set(900.0 * (-t / 0.2).exp() + 120.0, r);
                let rumble = lp.run(noise.next()) * env(t, 0.004, 0.22) * 2.2;
                let body = tone.at(t, 90.0 * pitch, 32.0, 0.4, r) * env(t, 0.003, 0.18);
                rumble + body
            }
            Sfx::Thud => {
                let body = tone.at(t, 110.0 * pitch, 45.0, 0.1, r) * env(t, 0.002, 0.04);
                lp.set(500.0, r);
                body + lp.run(noise.next()) * env(t, 0.001, 0.02)
            }
            Sfx::Pop => tone.at(t, 520.0 * pitch, 1500.0, 0.05, r) * env(t, 0.002, 0.025),
            Sfx::Stomp => {
                let body = tone.at(t, 85.0 * pitch, 38.0, 0.15, r) * env(t, 0.003, 0.07);
                lp.set(700.0, r);
                let crunch = lp.run(noise.next()) * env(t, 0.001, 0.05) * 1.5;
                1.1 * body + crunch
            }
            Sfx::Babble => {
                // Four quick syllables of a squarish voice at wandering pitches.
                let syl = 0.09;
                let k = (t / syl) as u32;
                let local = t - k as f32 * syl;
                let notes = [1.0, 1.26, 0.89, 1.12, 1.33, 0.94];
                let f = 330.0 * pitch * notes[((k + variant * 2) as usize) % notes.len()];
                let s = tone.at(t, f, f, 1.0, r);
                let square = (s * 3.0).clamp(-1.0, 1.0);
                lp.set(1800.0, r);
                lp.run(square) * (local / syl * std::f32::consts::PI).sin().max(0.0) * 0.55
            }
            Sfx::Ko => {
                // Descending "wah-wah" with vibrato.
                let vib = 1.0 + 0.04 * (t * TAU * 7.0).sin();
                let f = 520.0 * pitch * (0.25f32).powf(t / len) * vib;
                let s = tone2.at(t, f, f, 1.0, r);
                let wah = 0.5 + 0.5 * (t * TAU * 3.2).sin();
                lp.set(400.0 + 2000.0 * wah, r);
                lp.run((s * 2.0).clamp(-1.0, 1.0)) * env(t, 0.02, 0.45) * 0.8
            }
        };
        out.push(s);
    }
    // Normalise each effect to the same peak; loudness is set by the caller's gain.
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        for s in &mut out {
            *s *= 0.9 / peak;
        }
    }
    // Short fade-out: no clicks if the envelope hadn't fully decayed.
    let fade = ((0.004 * r) as usize).min(out.len());
    let len = out.len();
    for (i, s) in out[len - fade..].iter_mut().enumerate() {
        *s *= 1.0 - i as f32 / fade as f32;
    }
    out
}

struct Voice {
    samples: Arc<[f32]>,
    pos: usize,
    /// Left/right gains.
    gain: [f32; 2],
}

#[derive(Default)]
struct Mixer {
    voices: Vec<Voice>,
}

impl Mixer {
    /// Mix into interleaved `out` with `channels` channels.
    fn fill(&mut self, out: &mut [f32], channels: usize) {
        out.fill(0.0);
        let channels = channels.max(1);
        for v in &mut self.voices {
            for frame in out.chunks_mut(channels) {
                let Some(&s) = v.samples.get(v.pos) else {
                    break;
                };
                v.pos += 1;
                if channels == 1 {
                    frame[0] += s * (v.gain[0] + v.gain[1]) * 0.5;
                } else {
                    frame[0] += s * v.gain[0];
                    frame[1] += s * v.gain[1];
                }
            }
        }
        self.voices.retain(|v| v.pos < v.samples.len());
        for s in out.iter_mut() {
            *s = (*s * MASTER).tanh(); // soft clip when many effects stack
        }
    }
}

/// Constant-power pan: `pan` -1 (left) .. 1 (right).
pub fn pan_gains(gain: f32, pan: f32) -> [f32; 2] {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    [gain * a.cos(), gain * a.sin()]
}

struct Request {
    sfx: Sfx,
    gain: f32,
    pan: f32,
}

/// Handle to the sound thread. Cheap to call from the main loop.
pub struct Sound {
    tx: Option<Sender<Request>>,
    variant: u32,
    last: HashMap<Sfx, Instant>,
}

impl Sound {
    /// A player that does nothing (sound off or no audio device).
    pub fn silent() -> Sound {
        Sound {
            tx: None,
            variant: 0,
            last: HashMap::new(),
        }
    }

    /// Start the sound thread. Falls back to silence if there's no output device.
    pub fn start() -> Sound {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("stickman-sound".into())
            .spawn(move || run(rx, ready_tx));
        let ok = spawned.is_ok()
            && ready_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap_or(false);
        if !ok {
            log::warn!("sound: no audio output; staying silent");
            return Sound::silent();
        }
        Sound {
            tx: Some(tx),
            variant: 0,
            last: HashMap::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.tx.is_some()
    }

    /// Play `sfx` at `gain` (0..1), panned by `pan` (-1 left .. 1 right).
    /// The same effect retriggers at most every 40 ms.
    pub fn play(&mut self, sfx: Sfx, gain: f32, pan: f32) {
        let Some(tx) = &self.tx else { return };
        let now = Instant::now();
        if self
            .last
            .get(&sfx)
            .is_some_and(|t| now.duration_since(*t) < Duration::from_millis(40))
        {
            return;
        }
        self.last.insert(sfx, now);
        self.variant = self.variant.wrapping_add(1);
        if tx.send(Request { sfx, gain, pan }).is_err() {
            self.tx = None;
        }
    }
}

/// The sound thread: owns the stream, renders effects, feeds the mixer.
fn run(rx: Receiver<Request>, ready: Sender<bool>) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        let _ = ready.send(false);
        return;
    };
    let Ok(supported) = device.default_output_config() else {
        let _ = ready.send(false);
        return;
    };
    if supported.sample_format() != cpal::SampleFormat::F32 {
        log::warn!(
            "sound: output format {:?} unsupported",
            supported.sample_format()
        );
        let _ = ready.send(false);
        return;
    }
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate.0;
    let channels = config.channels as usize;
    let mixer = Arc::new(Mutex::new(Mixer::default()));
    let cb_mixer = mixer.clone();
    let stream = device.build_output_stream(
        &config,
        move |out: &mut [f32], _| match cb_mixer.try_lock() {
            Ok(mut m) => m.fill(out, channels),
            // Never block the audio thread: a skipped buffer is just silence.
            Err(_) => out.fill(0.0),
        },
        |e| log::warn!("sound: stream error: {e}"),
        None,
    );
    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            log::warn!("sound: could not open output: {e}");
            let _ = ready.send(false);
            return;
        }
    };
    let _ = stream.pause();
    let _ = ready.send(true);
    log::info!("sound: {rate} Hz, {channels} ch");

    let mut cache: HashMap<(Sfx, u32), Arc<[f32]>> = HashMap::new();
    let mut variant = 0u32;
    let mut playing = false;
    let mut quiet_since = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(req) => {
                variant = (variant + 1) % VARIANTS;
                let samples = cache
                    .entry((req.sfx, variant))
                    .or_insert_with(|| synth(req.sfx, rate, variant).into())
                    .clone();
                if let Ok(mut m) = mixer.lock() {
                    if m.voices.len() >= MAX_VOICES {
                        m.voices.remove(0);
                    }
                    m.voices.push(Voice {
                        samples,
                        pos: 0,
                        gain: pan_gains(req.gain.clamp(0.0, 1.0), req.pan),
                    });
                }
                if !playing {
                    playing = stream.play().is_ok();
                }
                quiet_since = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                let busy = mixer.lock().map(|m| !m.voices.is_empty()).unwrap_or(false);
                if busy {
                    quiet_since = Instant::now();
                } else if playing && quiet_since.elapsed() > IDLE_PAUSE {
                    playing = stream.pause().is_err();
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_is_audible_bounded_and_click_free() {
        for sfx in Sfx::ALL {
            for v in 0..VARIANTS {
                let s = synth(sfx, 48_000, v);
                assert!(s.len() > 48_000 / 20, "{sfx:?} too short");
                let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
                assert!((0.85..=0.91).contains(&peak), "{sfx:?} peak {peak}");
                assert!(s.iter().all(|x| x.is_finite()));
                assert!(s.last().unwrap().abs() < 0.01, "{sfx:?} ends with a click");
                let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
                assert!(rms > 0.05, "{sfx:?} nearly silent (rms {rms})");
            }
        }
    }

    #[test]
    fn variants_differ() {
        assert_ne!(synth(Sfx::Punch, 44_100, 0), synth(Sfx::Punch, 44_100, 1));
    }

    #[test]
    fn mixer_mixes_pans_and_retires_voices() {
        let mut m = Mixer::default();
        m.voices.push(Voice {
            samples: vec![0.5; 4].into(),
            pos: 0,
            gain: pan_gains(1.0, -1.0),
        });
        let mut out = [0.0f32; 12]; // 6 stereo frames
        m.fill(&mut out, 2);
        assert!(out[0] > 0.2 && out[1].abs() < 1e-6, "hard left: {out:?}");
        assert_eq!(out[8], 0.0, "voice ended after 4 frames");
        assert!(m.voices.is_empty());
        // Many loud voices are soft-clipped, never above 1.
        for _ in 0..10 {
            m.voices.push(Voice {
                samples: vec![1.0; 4].into(),
                pos: 0,
                gain: [1.0, 1.0],
            });
        }
        m.fill(&mut out, 2);
        assert!(out.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn pan_is_constant_power() {
        for p in [-1.0, -0.3, 0.0, 0.6, 1.0] {
            let [l, r] = pan_gains(1.0, p);
            assert!((l * l + r * r - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn silent_player_ignores_requests() {
        let mut s = Sound::silent();
        s.play(Sfx::Boom, 1.0, 0.0);
        assert!(!s.is_running());
    }
}
