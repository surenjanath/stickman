# Product Requirements Document: Stickman AI Desktop Overlay

## 1. Executive Summary

Stickman is a native desktop overlay application that deploys an autonomous, interactive 2D stickman onto the user's screen. Operating as an advanced, context-aware desktop companion, the agent dynamically interacts with the operating system environment. It walks on active application windows, scales on-screen text, engages in physics-based combat with the user's mouse cursor, and alters its behavior based on decisions made by a localized AI engine.

## 2. Target Platform & Hardware Target

* **Operating System:** macOS (V1 Focus).
* **Hardware:** Optimized for Apple Silicon (M1 Max and newer) to ensure low-overhead, high-framerate rendering during GPU-accelerated particle effects and real-time screen polling.
* **AI Infrastructure:** Local LLM execution via Ollama (no cloud dependencies).

## 3. Core Features & Functional Requirements

**3.1. Transparent Overlay System**

* The application must render a borderless, transparent, frameless window spanning the entire desktop workspace.
* The window must default to a click-through state (ignoring standard mouse events) to allow uninterrupted user interaction with underlying applications.
* The window must dynamically toggle its click-through state to intercept specific mouse clicks when the cursor intersects with the stickman's rigid body collider or when triggering particle explosions.

**3.2. Context-Aware OS Collision**

* **Window Boundaries:** The system must poll macOS accessibility APIs to retrieve the dimensions and positions of the currently active application window. These coordinates must be dynamically translated into static 2D physics colliders.
* **Text Traversal:** The application must capture localized screenshots of the active window and process them through a lightweight OCR engine to generate bounding boxes for distinct text blocks, allowing the agent to walk on lines of text.

**3.3. Physics and Combat Engine**

* The stickman must be governed by a 2D rigid body physics engine handling gravity, momentum, and friction.
* **Mouse Tracking:** The application must monitor global mouse coordinates to calculate cursor velocity and trajectory.
* **Combat Mechanics:** Spikes in mouse velocity directed at the agent must trigger defensive or evasive animation states. User clicks on the agent must apply localized physics impulses (knockback) and spawn GPU-rendered particle explosions.

**3.4. Local AI Decision Engine (The Director)**

* The stickman's macro-behavior must be dictated by a local LLM operating on a background thread.
* The system will periodically send a JSON payload to the Ollama endpoint containing current state data (e.g., active application name, average mouse velocity, agent health/frustration metrics).
* The LLM will return a structured response commanding the agent to transition between predefined behavioral states (e.g., `OBSERVE`, `SABOTAGE_WORK`, `FRENZY_ATTACK`, `IDLE`).

## 4. Technical Stack & Architecture

* **Core Language:** Rust
* **Window Management:** `winit` or `tao` for transparent overlay generation.
* **macOS Interop:** `objc`, `core-graphics`, and `accessibility-sys` for active window polling, global cursor tracking, and toggling `NSWindow` event properties.
* **Rendering Pipeline:** `wgpu` for hardware-accelerated 2D sprite rendering (stickman assets) and customized particle emission (explosions).
* **Physics Engine:** `rapier2d` for managing the agent's kinematic state, window edge colliders, and impulse forces.
* **OCR Integration:** `tesseract-sys` for high-speed localized text bounding box generation.
* **AI Networking:** `reqwest` for asynchronous local HTTP polling to the Ollama API.

## 5. Performance Constraints & Success Metrics

* **Framerate:** The render loop must sustain a strict 60+ FPS during combat sequences and particle generation.
* **Resource Utilization:** CPU utilization must remain under 5% during idle observation states. OCR and window polling must be aggressively optimized or throttled to prevent blocking the main render loop.
* **Latency:** The delay between a mouse velocity spike and the corresponding animation state change must not exceed 50 milliseconds.
* **Collision Accuracy:** Generated static colliders for OS windows and text blocks must align with visual UI elements within a 5-pixel margin of error.

## 6. Out of Scope (Version 1)

* Cross-platform compatibility (Windows/Linux support).
* Audio playback and sound effects.
* Complex multi-agent interactions (spawning multiple stickmen).
* Integration with cloud-based AI models (e.g., OpenAI, Anthropic).