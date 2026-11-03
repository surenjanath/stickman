# StykAnimate Desktop Overlay

A native macOS desktop companion: a stick figure that lives on top of your desktop.
It walks along the Dock, climbs onto your windows, stands on lines of text, dodges
your mouse, fights your cursor, gets knocked flying when you click it, and stomps
on whatever window you're working in. A **local** LLM running in Ollama decides
its mood.

Written in Rust for Apple Silicon:
- **Overlay window:** `winit` + AppKit (`objc`).
- **Rendering:** `wgpu` (Metal).
- **Collision world:** `rapier2d`.
- **AI director:** `tokio` + `reqwest`, talking to Ollama.
- **Text detection:** Apple's Vision framework.

![Every pose of the figure](docs/poses.png)

## What it does

| | |
|---|---|
| **Overlay** | Borderless, fully transparent window over the main display, always on top (`NSStatusWindowLevel`), on every Space, including over full-screen apps. No Dock icon, never steals focus. |
| **Click-through** | Every click passes to the app underneath, except when you deliberately aim at the figure (the cursor moves onto it and hovers for 120 ms). Clicks meant for your app aren't stolen when the figure wanders under a parked cursor. |
| **Walking** | Procedural gait with planted feet (no sliding), two-bone IK knees, hip bob and counter-swinging arms. Running uses a longer stride, high knees and pumping arms. |
| **Climbing** | Window title bars are one-way platforms (occluded parts removed), as are lines of text found by OCR in the active window. The figure jumps up onto them, walks along them, rides them when you drag the window, and falls when it walks off the edge or you close the window. |
| **Combat** | Click the figure: it's knocked flying with an explosion, spins, bounces and gets back up, and a small health bar appears. Clicks land mid-air too (juggling). Enough hits knock it out, with stars. Swipe fast at it and it dodges. In combat mode it chases the cursor and punches or kicks it, physically shoving your pointer. |
| **Sabotage** | It climbs onto your active window and stomps on it, shaking the window via Accessibility. |
| **AI director** | Every 4 s a local Ollama model (`llama3.2:1b` by default, ~200–300 ms) picks `IDLE` / `OBSERVE` / `SABOTAGE` / `COMBAT` from what you're doing: active app, mouse speed, hits, health, frustration, idle time. Modes last at least 12 s, except immediate retaliation after a hit. If Ollama is down, a built-in director takes over. |
| **Efficiency** | 60 Hz simulation when something moves, 30 Hz otherwise; frames that wouldn't visibly change are skipped. Idle ≈ 2–3% of one core (PRD budget 5%), walking ≈ 13–16%. |

## Requirements

- macOS on Apple Silicon (`aarch64-apple-darwin`). Developed on an M1 Max.
- Rust 1.85+.
- Optional: [Ollama](https://ollama.com) running locally with a small model, e.g.
  `ollama pull llama3.2:1b`. Without it, the built-in director is used.
- Permissions, granted to the app or terminal that launches it (System Settings →
  Privacy & Security):
  - **Accessibility:** stomps shake windows.
  - **Screen Recording:** OCR, so the figure can walk on text.

  Both are optional; the figure works without them. macOS asks on first launch.

## Run

```sh
cargo run --release
```

There's no Dock icon and the window ignores the mouse, so **quit with Ctrl+C** in
the launching terminal.

### Environment variables

| Variable | Effect |
|---|---|
| `STYK_MODE=IDLE\|OBSERVE\|SABOTAGE\|COMBAT` | Force a mode (director off). |
| `STYK_OLLAMA_MODEL` | Ollama model (default `llama3.2:1b`). |
| `STYK_OLLAMA_URL` | Ollama URL (default `http://localhost:11434`). |
| `STYK_DIRECTOR_INTERVAL_S` | Seconds between director decisions (default 4). |
| `STYK_NO_CURSOR_PUSH=1` | Punches don't move your pointer. |
| `STYK_NO_WINDOW_SHAKE=1` | Stomps don't move your windows. |
| `STYK_NO_OCR=1` | Don't read screen text (no walking on text). |
| `STYK_NO_PROMPTS=1` | Never show macOS permission dialogs. |
| `STYK_HOLD_IDLE=1` | Stand still (used to measure idle CPU). |
| `STYK_CLICK_THROUGH=0` | Debug: the overlay catches every click (blocks the screen). |
| `RUST_LOG=debug` | Verbose logs: director decisions, OCR, perf stats, click-through toggles. |

## Testing

```sh
cargo test --release                       # everything (PRD CPU budget enforced)
cargo test --release --test overlay -- ocr # one GUI test by name
STYK_INTERACTIVE_TESTS=1 cargo test --release --test overlay -- click  # hands off the mouse
cargo clippy --all-targets -- -D warnings
cargo run --release --example pose_sheet -- docs/poses.png   # re-render the pose sheet
```

**Unit tests (72).**
- **Collision:** one-way landings, no tunnelling, window-edge occlusion clipping.
- **Walking and running:** feet never slide; feet always on the ground.
- **Poses:** IK accuracy and knee direction; hands and arms never cross the head.
- **Combat:** knockback, juggling into a KO, getting back up, chasing and hitting
  the cursor, dodging fast swipes.
- **Sabotage:** climbing onto a window and stomping it; riding a dragged window.
- **Smoothness:** no limb ever jumps between frames, across 16 mode/seed runs.
- **Director:** prompt and schema building, response parsing, fallback heuristics.
- **Input:** velocity reacts within 50 ms.
- **Particles, coordinates and click intent.**

There are also live checks against a real Ollama, and Vision OCR on rendered text.

**Live GUI tests (`tests/overlay.rs`).** These launch the real binary and inspect it
through the window server and AppKit:
- **Window:** stacking, bounds and transparency.
- **Click-through:** an AppKit hit test, plus a control run proving the test can fail.
- **Figure:** drawn on the ground line and walking.
- **Focus:** never steals focus, no Dock icon.
- **CPU:** idle and walking.
- **AI director:** makes a decision with the live model.
- **OCR:** finds text platforms on the active window.
- **Exit:** clean shutdown on SIGINT.
- **Click (opt-in):** clicking the figure knocks it back, bursts particles and
  returns focus to your app. It drives the real pointer, so it only runs with
  `STYK_INTERACTIVE_TESTS=1`.

The GUI tests run overlays with cursor pushes, window shakes and permission
prompts disabled. Set `STYK_SKIP_GUI_TESTS=1` to skip them (e.g. on CI).

## Architecture

```text
main thread (winit / AppKit)                         background
┌───────────────────────────────────────────┐   ┌───────────────────────────────┐
│ about_to_wait: tick at 60/30 Hz           │   │ styk-windows: CGWindowList    │
│  ├ Cursor::sample (pos, velocity)         │◄──┤   every 250 ms → Snapshot     │
│  ├ platforms: ground + window tops + text │◄──┤ styk-ocr: Vision on the       │
│  │   → physics::World (rapier2d)          │   │   settled active window       │
│  ├ director decisions → Mode              │◄──┤ tokio: director ↔ Ollama      │
│  ├ Agent::update ×N (fixed 60 Hz steps)   │   │                               │
│  ├ events → particles, cursor push,       │──►│ styk-os: Accessibility shakes │
│  │          window shake                  │   └───────────────────────────────┘
│  ├ click intent → setIgnoresMouseEvents   │
│  └ present (skip if nothing changed)      │
│      DrawList → wgpu/Metal, 1 draw call   │
└───────────────────────────────────────────┘
```

| Module | Role |
|---|---|
| `src/main.rs` | App wiring: window, threads, tick loop, events, click intent, focus restore, director dwell. |
| `src/agent.rs` | The figure's brain and body: reflexes, modes, actions, fixed-step physics, pose blending. |
| `src/poses.rs` | Action poses (stance, punch, kick, jump, flail, tumble, dodge, stomp, taunt, think, run). |
| `src/stickman.rs` | StykAnimate Walker rig, forward kinematics, leg IK, walk/run gaits, idle pose. |
| `src/physics.rs` | rapier2d platform world: one-way landing and support queries, window-edge clipping. |
| `src/particles.rs` | Explosions, sparks, dust, shockwave rings, KO stars. |
| `src/render.rs`, `src/stickman.wgsl` | SDF capsule/ring/disc instancing over a transparent Metal surface. |
| `src/input.rs` | Global cursor position and velocity, cursor push. |
| `src/windows.rs` | Window-server polling for app windows. |
| `src/ocr.rs` | Window capture and Vision text-line detection, turned into text platforms. |
| `src/director.rs` | Ollama client, prompt, structured output, fallback director. |
| `src/macos.rs` | AppKit and Accessibility wrappers (overlay setup, focus, window shake). |
| `tests/overlay.rs` | Live GUI test harness. |
| `examples/pose_sheet.rs` | Renders `docs/poses.png`. |

Engineering rules (from `claude.md`):
- **Threads:** heavy work stays off the main thread.
- **Unsafe code:** every `unsafe` block documents its invariants.
- **Physics:** fixed-timestep physics drives the visuals.
- **Memory:** captures are released immediately.
- **Errors:** `anyhow` and graceful degradation when permissions are denied.

### Notable decisions

- **Vision instead of Tesseract (PRD 4):**
  - **Install:** Vision ships with macOS, so nothing to install.
  - **Speed:** it runs on the Neural Engine.
  - **Privacy:** only text-line boxes are used, never the text itself.
- **Custom character controller over rapier queries:** platforms are rapier
  colliders. The figure moves with its own fixed-step integrator using rapier ray
  casts, which gives precise one-way platforms and planted feet.
- **No `request_redraw()`:** on macOS it runs a full-screen AppKit display pass per
  frame (~14% CPU). Frames are drawn straight from the timer instead.
- **Continuous pose blending:** target angles are unwrapped over time, so blends
  never snap the wrong way round.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 1 | Transparent click-through all-Spaces overlay | ✅ |
| 2 | wgpu renderer, rig, IK walk, idle | ✅ |
| 3 | Cursor tracking, dodge, attacks on the cursor, click knockback, particles, dynamic click-through | ✅ |
| 4 | Window title bars and text as platforms, jumping, falling, riding windows | ✅ |
| 5 | AI director (Ollama) with fallback | ✅ |
| 6 | OCR text traversal (Vision) | ✅ |
| Next | Multi-display, speech bubbles, more combat moves (grabs, throws), sound (out of scope for V1) | — |

## Credits

- The stick-figure rig (segment lengths, rest angles, thicknesses, colour) comes
  from [StykAnimate](https://github.com/filippocalippo/StykAnimate-App) (`src/demo.mjs`,
  `defaultFigure()`) by Filippo Calippo, licensed GPL-3.0. Keep that in mind when
  choosing a license for redistribution.
- All animation, behaviour, rendering and OS integration here is original.
