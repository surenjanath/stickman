# StykAnimate reference repo — what maps to the Rust overlay

Source: https://github.com/filippocalippo/StykAnimate-App (shallow clone, Nov 2026).

## Key finding: no sprite sheets and no Idle/Walk/Attack state machine

StykAnimate is a **Pivot-style skeletal stick-figure editor** (Electron + browser, JS/Python).
It has no sprite-sheet assets, no fixed frame grid, and no gameplay state machine. A figure is a
tree of segments (bones) and an animation is a list of keyframes holding one angle per segment.
"Sprites" in that repo are user-imported PNGs attached to a bone (`docs/SPRITE-IMPORT.md`,
`docs/SPRITE-PLACEMENT.md`), not animation sheets.

What it does contain, and what this project can use:

| Item | Source | Value |
|---|---|---|
| Default stick figure rig ("Walker") | `src/demo.mjs` → `defaultFigure()` | 10 segments, table below |
| Walk cycle | `src/demo.mjs` → `createDemo()` | 24 procedural keyframes |
| Document timing | `src/demo.mjs` → `emptyDocument()` | `fps: 24`, canvas 960×540 |
| Keyframe hold | `createFrame(objects, duration_raw)` | walk uses `duration_raw = 2` ticks |
| Forward kinematics | `src/animation-origin.mjs` (~L57–66) | absolute angles, y-down |
| Idle / Attack | — | **not present**; must be authored (see below) |

## Rig: "Walker" (`defaultFigure`)

Root (segment parent `0`) is the hip. Color `rgb(43,43,43)`. Angles are radians, **absolute**
(not relative to the parent), in a **y-down** screen space, so `-π/2` points up.

| id | parent | length | rest angle | thickness | kind | role (inferred) |
|---:|---:|---:|---:|---:|---|---|
| 1 | 0 | 78 | -π/2 | 11 | line | torso (hip → neck) |
| 2 | 1 | 44 | -π/2 | 8 | circle | head |
| 3 | 1 | 53 | 1.05 | 9 | line | upper arm A |
| 4 | 3 | 48 | 1.32 | 8 | line | forearm A |
| 5 | 1 | 53 | 2.02 | 9 | line | upper arm B |
| 6 | 5 | 48 | 1.65 | 8 | line | forearm B |
| 7 | 0 | 66 | 1.12 | 11 | line | thigh A |
| 8 | 7 | 63 | 1.52 | 10 | line | shin A |
| 9 | 0 | 66 | 2.00 | 11 | line | thigh B |
| 10 | 9 | 63 | 1.68 | 10 | line | shin B |

Forward kinematics (`bend = 0` case):

```text
end(seg) = start(seg) + length * scale * (cos(angle), sin(angle))
start(seg) = end(parent)   // or the root (hip) position when parent == 0
```

A circle segment draws a circle along its segment (the head), so the figure is about
78 + 44 + 129 ≈ 250 px tall at scale 1.

## Walk cycle (`createDemo`)

24 keyframes, each held `duration_raw = 2` ticks at 24 fps:
**48 ticks = 2.0 s per loop, 12 distinct poses per second.**

For keyframe `i` in `0..24`:

```text
phase = (i + 3) / 24 * 2π
swing = sin(phase)
lift  = cos(2 * phase)
root  = (450 + 9 * swing, 292 - 3 * lift)          // hip bob, demo canvas coords

angles[1]  = -π/2 + 0.055                           // torso lean
angles[2]  = -π/2 + 0.055                           // head
angles[3]  =  π/2 - 0.68 * swing                    // upper arm A
angles[4]  =  π/2 - 0.43 * swing - 0.4              // forearm A
angles[5]  =  π/2 + 0.68 * swing                    // upper arm B
angles[6]  =  π/2 + 0.43 * swing - 0.4              // forearm B
angles[7]  =  π/2 + 0.51 * swing                    // thigh A
angles[8]  =  π/2 + 0.86 * max(0, -swing)           // shin A (knee bends on back-swing)
angles[9]  =  π/2 - 0.51 * swing                    // thigh B
angles[10] =  π/2 + 0.86 * max(0,  swing)           // shin B
```

The demo walks in place (root x oscillates ±9 px). Moved across the screen it looks
wrong: the planted foot slides back and forth, and the knee bends backwards on the
front leg. **The overlay doesn't use it.** `src/stickman.rs` has its own procedural
gait on the same rig (see below).

## What the overlay implements

See the README for the full feature list. Relative to the source repository:
- **Rig:** StykAnimate's Walker, used verbatim.
- **Motion:** our own. A planted-foot IK gait (walk and run), 15+ action poses
  (rendered in `docs/poses.png`), and continuous pose blending.
- **State machine:** our own. It has the PRD's four macro modes (from the AI
  director) and the actions below. The source has no state machine at all.

  | Action | Triggered by |
  |---|---|
  | Stand / Think | idle, observe |
  | Walk / Run | wander, follow, chase |
  | Crouch → Air → Land | jumps onto windows/text, leaps at the cursor |
  | Fall | walked off an edge, platform vanished |
  | Punch / Kick | combat, cursor in reach (aimed at the cursor) |
  | Dodge | fast swipe toward the figure outside combat |
  | Tumble → GetUp / KO | user clicked the figure |
  | Stomp | sabotage, standing on the active window |
  | Taunt | combat at range, or a window out of reach |

## Mapping to the wgpu pipeline (original Phase 1 proposal)


The repo doesn't dictate a renderer, so this is a recommendation:

1. **Render the skeleton directly; no sprite sheet.** Each frame, run FK on the pose
   (10 bones), then draw one instanced quad per bone with an SDF capsule/circle fragment
   shader. That is 10 instances, a single draw call, and resolution-independent output on
   Retina, with any number of states and no texture memory.
2. **Pose interpolation:** sample the state's keyframes at 24 fps ticks and lerp angles
   (shortest arc) between keys at display rate (60–120 Hz). This keeps motion smooth while
   preserving the source timing.
3. **Physics coupling** (claude.md rule 3): rapier2d owns one capsule rigid body
   (~30 × 250 px). Its position drives the root and its rotation adds to all angles; the
   pose is visual only.
4. **Surface:** use `CompositeAlphaMode::PostMultiplied` (Metal layer, non-opaque) and clear
   to `(0,0,0,0)` so the overlay stays transparent. winit 0.30 and wgpu 30 share
   `raw-window-handle` 0.6.2, so the surface can be created from the existing `Window`.

If sprite sheets are still wanted (e.g. imported art): bake each state's keyframes with
StykAnimate's PNG-frames / APNG / WebM-alpha export and pack them into one atlas, with a
cell size of the figure's bounding box plus thickness padding.

## States the overlay needs but the repo lacks

| State | Status | Suggested source |
|---|---|---|
| Idle | implemented (procedural) | `stickman::idle_pose`. |
| Walk | implemented (procedural IK gait) | `stickman::walk_pose`; the demo walk above is reference only. |
| Attack | **missing** | Author in StykAnimate (wind-up → strike → recover, with short `duration_raw` on the strike), export `.stk`, and port the keyframe angles. |

Suggested state machine (ours, not from the repo), driven by the AI Director (`IDLE`,
`OBSERVE`, `SABOTAGE`, `COMBAT`) and by mouse-velocity spikes (PRD 3.3, ≤ 50 ms reaction):

```text
Idle ⇄ Walk          (Director target / wander timer)
* → Attack           (cursor velocity spike toward agent, or COMBAT)
Attack → Idle|Walk   (when the clip ends)
* → Knockback        (click impulse; returns when the body settles)
```
