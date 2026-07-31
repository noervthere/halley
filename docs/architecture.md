# Halley architecture

Halley uses balanced subsystem boundaries: modules are large enough to keep a
complete policy visible, but platform, presentation, shell UI, and protocol
state do not share implementation files.

## Dependency direction

- `session` owns compositor policy and coordinates subsystems.
- `backend` owns only TTY/winit, output, DMA-BUF, frame submission, DPMS, and
  VRR mechanics. TTY output and DMA-BUF policy live below `backend::tty`.
- `presentation` owns camera transforms plus shared window and field-maximize
  presentation state used by rendering and input mapping.
- `render` owns scene composition and dedicated persistent GPU resources for
  backgrounds, effects, decorations, textures, text, nodes, and shell UI.
- `window` owns backend-neutral managed-window rules and initial routing.
  Wayland and XWayland supply identities and apply the resolved policy without
  maintaining separate rule engines.
- `shell` owns renderer-independent overlay, bearings, focus-cycle, and Apogee
  state. Their GLES presentation lives below `render`.
- `capture` owns screenshot selection, a separate portal source-chooser state
  machine, pixel capture, and screencast buffers. Screenshot and portal chrome
  have independent render modules and share only capture icon assets.
- `wayland` and `xwayland` own protocol transactions. XWayland control
  properties, managed state, configure constraints, lifecycle,
  override-redirect policy, and presentation policy are separate.
- `animation` owns pure timelines. Rendering consumes animation output but
  animation code does not know about GLES or backends.
- `halley-cli` separates command parsing, help, response presentation,
  configuration diagnostics, and IPC transport. Node parsing has its own
  command module; the executable entry point only dispatches typed actions.

One `RenderState` owns persistent GPU caches. A frame crosses the
session/render boundary through six contexts: frame, desktop, cursor, overlays,
visual configuration, and mutable render resources.

Pointer confinement and locking are a protected boundary. Their lifecycle and
motion algorithms remain in `session::pointer::constraints`; presentation
adapters may be called by that subsystem, but rendering and refactors must not
own or duplicate constraint policy.

## Quality rubric

Scores use a ten-point scale and are gated separately; an average cannot hide
a category below 8.

| Category | Score | Evidence |
| --- | ---: | --- |
| Cohesion | 9.1 | Shell, presentation, capture, backend TTY, XWayland control/state/configure policy, window rules/routing, and background rendering have dedicated ownership boundaries |
| Coupling | 8.7 | Both backends share one scene request; rule consumers share one resolved policy; Smithay retains XWM event ownership while Halley adds only a property-policy boundary |
| Maintainability | 8.9 | Typed/validated config, documented protocol invariants, bounded diagnostics, focused pure tests, nested X11 checks, workspace regression tests, formatting, build, and strict clippy gates |

The remaining large orchestration files are `session/input.rs`,
`wayland/fullscreen.rs`, `render/scene.rs`, and the TTY session/backend
drivers. They are cohesive event loops, transaction managers, or scene
assemblers rather than mixed-domain feature modules. Future feature work should
extract policy objects and render elements from them instead of adding more
branches in place.

## Change gates

Before merging architectural or rendering changes:

1. `cargo fmt --all -- --check`
2. `cargo test --workspace`
3. `cargo build -p halley`
4. `cargo clippy --workspace --all-targets -- -D warnings`

Configuration syntax and IPC enum ordering are compatibility surfaces. IPC
remains at version 10 unless a deliberate wire-format change is made.
