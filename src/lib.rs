//! Stickman: an AI-directed stick figure that lives on the
//! macOS desktop. The binary (`src/main.rs`) wires these modules into the winit
//! event loop.

pub mod agent;
pub mod director;
pub mod doodle;
pub mod draw;
pub mod geom;
pub mod hats;
pub mod input;
pub mod jail;
pub mod macos;
pub mod menubar;
pub mod mind;
pub mod music;
pub mod note;
pub mod ocr;
pub mod particles;
pub mod physics;
pub mod poses;
pub mod render;
pub mod rival;
pub mod screens;
pub mod settings;
pub mod settings_window;
pub mod sound;
pub mod speech;
pub mod stats;
pub mod stickman;
pub mod text;
pub mod windows;
pub mod winfly;
pub mod words;
