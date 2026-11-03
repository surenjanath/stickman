# StykAnimate AI Desktop Overlay - Agent Instructions

## Project Overview
This is a native macOS desktop overlay written in Rust. It renders an autonomous 2D stickman that interacts with OS windows, text, and the user's cursor. It uses a transparent, click-through window, real-time physics, and a local LLM decision engine.

## Tech Stack
* **Language:** Rust (Target: `aarch64-apple-darwin`)
* **Window/Graphics:** `winit` (window management), `wgpu` (hardware rendering)
* **Physics:** `rapier2d` (rigid body dynamics, colliders)
* **OS Interop:** `core-graphics`, `accessibility-sys`, `objc` (macOS APIs)
* **AI/Async:** `reqwest`, `tokio` (polling local Ollama instances)
* **OCR:** `tesseract-sys` (text bounding box detection)

## Build & Run Commands
* **Run application:** `cargo run --release` (Release mode is required for `wgpu` and physics performance)
* **Build:** `cargo build`
* **Linting:** `cargo clippy -- -D warnings`
* **Format:** `cargo fmt`
* **Test:** `cargo test`

## Architectural Rules & Code Style
1. **Thread Separation:** The rendering loop (`winit` / `wgpu`) MUST run on the main thread. All LLM polling, OCR processing, and heavy OS-level window scraping must be offloaded to asynchronous `tokio` tasks or separate OS threads. Never block the main render loop.
2. **macOS API Safety:** When using `unsafe` blocks for Objective-C message passing (`msg_send!`) or CoreGraphics FFI, always wrap them in safe, idiomatic Rust abstractions. Thoroughly comment the invariants of all `unsafe` blocks.
3. **Physics Synchronization:** The `rapier2d` physics step should run at a fixed timestep (e.g., 60Hz). Sprites and visual coordinates must interpolate based on the physics engine's state, not the other way around.
4. **Memory Management:** Ensure aggressive memory cleanup for OCR screenshots. Do not leak `CGImage` or `CGContext` references when scraping the screen.
5. **Error Handling:** Use the `anyhow` crate for application-level errors and `thiserror` for library-level abstractions. Do not use `.unwrap()` in production logic; handle failures gracefully, especially for macOS accessibility permissions which the user may deny.

## AI Director Logic (Ollama)
* The agent's macro-decisions are handled by a local Ollama instance. 
* API calls to `http://localhost:11434/api/generate` should be debounced (e.g., every 3-5 seconds).
* JSON payloads sent to the LLM should include: `mouse_velocity_x`, `mouse_velocity_y`, `active_app_name`, and `agent_health`.
* Expect a structured JSON response containing the target state: `IDLE`, `SABOTAGE`, `COMBAT`, or `OBSERVE`. Parse this using `serde_json` and push it to the state machine via an `mpsc` channel.