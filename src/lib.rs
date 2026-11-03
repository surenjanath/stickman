//! Stickman: an AI-directed stick figure that lives on the
//! macOS desktop. The binary (`src/main.rs`) wires these modules into the winit
//! event loop.

pub mod agent;
pub mod director;
pub mod geom;
pub mod input;
pub mod macos;
pub mod menubar;
pub mod ocr;
pub mod particles;
pub mod physics;
pub mod poses;
pub mod render;
pub mod settings;
pub mod speech;
pub mod stickman;
pub mod text;
pub mod windows;
pub mod words;
