//! Plays every sound effect in turn, panned left to right.
//!
//! cargo run --release --example sfx_demo

use std::time::Duration;

use stickman::sound::{Sfx, Sound};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut sound = Sound::start();
    if !sound.is_running() {
        eprintln!("no audio output device");
        return;
    }
    let n = Sfx::ALL.len() as f32;
    for (i, sfx) in Sfx::ALL.into_iter().enumerate() {
        println!("{sfx:?}");
        sound.play(sfx, 0.35, -0.8 + 1.6 * i as f32 / (n - 1.0));
        std::thread::sleep(Duration::from_millis(650));
    }
    std::thread::sleep(Duration::from_millis(500));
}
