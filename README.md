# StykAnimate Desktop Overlay

A native macOS desktop companion: a 2D stick figure that lives on top of your
desktop, walks along the Dock, and (in upcoming phases) climbs your windows, reacts
to your mouse, fights the cursor, and takes orders from a **local** LLM "director"
running in Ollama.

Written in Rust for Apple Silicon: `winit` + AppKit (`objc`) for the overlay window,
`wgpu` (Metal) for rendering, `rapier2d` for physics, and `tokio` + `reqwest` for the
AI director.

> Status: **Phase 1 complete, rendering pass 1 complete.** The overlay window, the
> renderer and the walking/idle stick figure work and are covered by tests. Combat,
> window collision, OCR and the AI director are in progress. See [Roadmap](#roadmap).

---

## What it does today

| Feature | Details |
|---|---|
| Full-screen transparent overlay | Borderless window covering the main display, including the menu-bar strip. Everything except the figure is fully transparent. |
| Click-through | `NSWindow.ignoresMouseEvents = YES`: every click goes to the app underneath. |
| Always on top, every Space | Window level `NSStatusWindowLevel` (25); collection behaviour `canJoinAllSpaces \| stationary \| ignoresCycle \| fullScreenAuxiliary`, so it survives Mission Control swipes and floats over full-screen apps. |
| Never steals focus | Accessory activation policy: no Dock icon, no Cmd-Tab entry, never becomes the active app. |
| Stick figure | StykAnimate's default "Walker" rig (10 bones), drawn with SDF-shaded capsules and a head ring in one instanced draw call. Anti-aliased at any Retina scale, with a light outline so it reads on dark wallpapers. |
| Walk | Procedural gait: planted feet that never slide, two-bone IK legs (knees always bend forward), swing-foot lift, hip bob, counter-swinging arms. |
| Idle and behaviour | Breathing idle pose, random walk/idle timers, stops and turns around at screen edges with blended (non-popping) transitions. |
| Ground | Walks on top of the Dock (`NSScreen.visibleFrame`), or on the screen bottom when the Dock is hidden or on the side. |
| Low CPU | 60 fps while moving, 20 fps wakeups while idle, and frames that wouldn't visibly change are skipped. Idle ≈ 2.5–4% of one core (PRD budget: 5%). |

## Requirements

- macOS on Apple Silicon (`aarch64-apple-darwin`). Developed on an M1 Max.
- Rust 1.85+ (`rustup target add aarch64-apple-darwin` if needed).
- For upcoming phases:
  - [Ollama](https://ollama.com) running locally (AI director).
  - Accessibility permission (active-window polling).
  - Screen Recording permission (OCR of on-screen text).

## Build and run

```sh
cargo run --release      # release is strongly recommended for wgpu and physics
```

The overlay has no Dock icon and ignores the mouse, so **quit it with Ctrl+C** in
the terminal that launched it.

### Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG=debug` | Verbose logging (env_logger). The startup line reports window number, frame, level, collection behaviour and ground line. |
| `STYK_CLICK_THROUGH=0` | Debug only: the overlay *catches* mouse events, which blocks clicks to the whole screen. |
| `STYK_HOLD_IDLE=1` | Keep the figure standing idle. Used to measure idle CPU. |

## Testing

```sh
cargo test --release     # unit tests + live GUI tests (PRD CPU budget enforced)
cargo test               # same, but the idle-CPU budget is report-only in debug
cargo clippy --all-targets -- -D warnings
```

**Unit tests (24).** Rig fidelity to StykAnimate, forward kinematics, mirroring, IK
accuracy and knee direction, *planted feet don't slide*, feet always on the ground,
gait speed, edge turnarounds, *no joint ever jumps between frames*, frame-skip rates,
coordinate conversions, ground-line detection, and env parsing.

**Live GUI tests (13).** These are in `tests/overlay.rs` with a custom harness.
They launch the real binary and inspect it from outside through the window server
and AppKit:

- window level, bounds and stacking above all normal windows
- clicks pass through: AppKit hit-test at several points, plus a control run with
  click-through off proving the test can fail
- the figure is visible, under 1% of the screen, stands on the ground line within
  4 pt, and actually walks
- no focus stealing, no Dock icon
- CPU while walking and while idle, startup time
- clean exit on SIGINT, with the window removed

The GUI tests need a logged-in desktop session. They briefly show overlays, and
during one control test (about 1 s) the screen ignores clicks. Set
`STYK_SKIP_GUI_TESTS=1` to skip them (e.g. on CI). Capturing the overlay needs Screen
Recording permission for your terminal.

## Architecture

```text
main thread (winit / AppKit)                     background (tokio runtime)
┌──────────────────────────────────────┐        ┌──────────────────────────────┐
│ ApplicationHandler                   │        │ (upcoming)                   │
│  ├─ about_to_wait: frame timer       │        │  AI director → Ollama        │
│  │   60 fps moving / 20 fps idle     │◄──mpsc─┤  active-window poller (AX)   │
│  ├─ Scene::frame                     │        │  OCR worker (text colliders) │
│  │   Figure::update  (behaviour)     │        └──────────────────────────────┘
│  │   Figure::segments (pose → FK)    │
│  │   skip if nothing moved ≥ 0.25 pt │
│  │   Renderer::render (wgpu/Metal)   │
│  └─ macos::OverlayWindow (objc)      │
└──────────────────────────────────────┘
```

| Path | Role |
|---|---|
| `src/main.rs` | App setup, overlay window attributes, frame pacing, coordinate helpers. |
| `src/macos.rs` | Safe wrappers over the Objective-C calls: `setIgnoresMouseEvents:`, `setCollectionBehavior:`, `setLevel:`, `setFrame:display:`, `NSScreen.visibleFrame`, and AppKit read-back. |
| `src/stickman.rs` | Walker rig, idle pose, procedural IK walk, forward kinematics, `Figure` state machine. |
| `src/render.rs` | wgpu surface (`Bgra8Unorm`, `PostMultiplied` alpha), instanced pipeline, per-frame upload and present. |
| `src/stickman.wgsl` | Vertex/fragment shader: SDF capsule/ring strokes, premultiplied output. |
| `tests/overlay.rs` | Live GUI test harness. |
| `docs/STYKANIMATE_ANALYSIS.md` | What was extracted from the StykAnimate repository, and how it maps here. |
| `prd.md`, `claude.md` | Product requirements and engineering rules. |

Design rules (from `claude.md`):
- **Main thread:** rendering stays on the main thread; LLM, OCR and window scraping
  run on tokio tasks.
- **Unsafe code:** every `unsafe` Objective-C call is wrapped in a safe API with its
  invariants documented.
- **Physics:** a fixed-timestep physics step drives the visuals.
- **Errors:** `anyhow` for application errors, and no `unwrap()` in production paths.

### Performance notes

- **No `request_redraw()`:** on macOS, `window.request_redraw()` marks the full-screen
  NSView dirty and runs an AppKit display pass. That alone cost ~14% CPU at 60 fps.
  Frames are rendered straight from the timer instead.
- **Fixed per-frame cost:** each Metal present costs ~2 ms of CPU (command submit,
  completion handlers, WindowServer IPC), whatever the surface size. Idle therefore
  renders at a low rate and skips frames where nothing visibly moved.

## Roadmap

| Phase | Scope | PRD |
|---|---|---|
| 1 ✅ | Transparent, click-through, all-Spaces overlay; dependencies; tests | 3.1 |
| 2 ✅ | wgpu renderer, Walker rig, IK walk, idle, behaviour, frame pacing | 3.1, 4, 5 |
| 3 | Global cursor tracking, velocity spikes → dodge/defend, attacks on the cursor, click → knockback impulse + GPU particle burst, dynamic click-through over the figure | 3.1, 3.3 |
| 4 | rapier2d world: gravity, falling, landing; active-window edges as static colliders (Accessibility API), so the figure walks on window title bars | 3.2, 3.3 |
| 5 | AI director: debounced Ollama calls with state (app name, mouse velocity, health/frustration) → `IDLE` / `OBSERVE` / `SABOTAGE` / `COMBAT` | 3.4 |
| 6 | OCR of the active window → text-line colliders the figure can walk on | 3.2 |

## Credits

- The stick-figure rig (segment lengths, rest angles, thicknesses, colour) comes
  from [StykAnimate](https://github.com/filippocalippo/StykAnimate-App) (`src/demo.mjs`,
  `defaultFigure()`) by Filippo Calippo, licensed GPL-3.0. Keep that in mind when
  choosing a license for redistribution.
- All animation (walk, idle, behaviour), rendering and OS integration in this
  repository is original.
