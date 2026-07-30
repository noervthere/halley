# Halley architecture

Halley uses balanced subsystem boundaries: modules are large enough to keep a
complete policy visible, but platform, presentation, shell UI, and protocol
state do not share implementation files.

## Dependency direction

- `session` owns compositor policy and coordinates subsystems.
- `backend` owns only TTY/winit, output, DMA-BUF, frame submission, DPMS, and
  VRR mechanics.
- `render` owns scene composition, persistent GPU resources, effects,
  decorations, textures, text, nodes, and shell UI rendering.
- `overlay`, `capture`, `bearings`, `focus_cycle`, and `apogee` own
  renderer-independent state. Their GLES presentation lives below `render`.
- `wayland` and `xwayland` own protocol transactions. XWayland managed-window
  lifecycle, override-redirect policy, and presentation policy are separate.
- `animation` owns pure timelines. Rendering consumes animation output but
  animation code does not know about GLES or backends.

One `RenderState` owns persistent GPU caches. A frame crosses the
session/render boundary through six contexts: frame, desktop, cursor, overlays,
visual configuration, and mutable render resources.

Pointer confinement and locking are a protected boundary. Their lifecycle and
motion algorithms remain in `session::pointer::constraints`; presentation
adapters may be called by that subsystem, but rendering and refactors must not
own or duplicate constraint policy.

## Maintainability rubric

Scores use a ten-point scale and are reviewed against production structure,
dependency direction, interface size, strict linting, and regression coverage.

| Category | Weight | Score | Evidence |
| --- | ---: | ---: | --- |
| Ownership and cohesion | 30% | 9.0 | Backend/render split; overlays and XWayland policies have dedicated modules |
| Dependency direction | 20% | 8.8 | Renderer-independent state points toward presentation, never GLES implementation |
| Interface coupling | 20% | 8.5 | Six frame contexts and one persistent render-resource aggregate |
| Local complexity | 15% | 8.0 | Central scene is under 300 production lines; nodes and XWM are decomposed |
| Regression safety | 15% | 9.2 | Workspace tests, strict clippy, formatting, build, and nested smoke gate |
| **Weighted total** | **100%** | **8.7** | No category is below 8 |

The remaining large orchestration files are `session/input.rs`,
`wayland/fullscreen.rs`, and the TTY session/backend drivers. They are cohesive
event loops or transaction managers rather than mixed-domain modules; future
feature work should extract policy objects from them instead of adding more
branches in place.

## Change gates

Before merging architectural or rendering changes:

1. `cargo fmt --all -- --check`
2. `cargo test --workspace`
3. `cargo build -p halley`
4. `cargo clippy --workspace --all-targets -- -D warnings`

Configuration syntax and IPC enum ordering are compatibility surfaces. IPC
remains at version 10 unless a deliberate wire-format change is made.
