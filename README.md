# Halley

Halley is a spatial Wayland compositor built around independent per-monitor
cameras, overlapping active windows, and collapsed node landmarks.

Current compositor-owned navigation includes:

- nodes, decay, focus rings, landmarks, and old-Halley contact physics;
- [Bearings](docs/bearings.md) for offscreen node navigation;
- [Apogee and Alt+Tab](docs/apogee.md) with live compositor previews;
- output-local fullscreen, including decoration maximize buttons;
- native screenshots, screencasting, and an XDG desktop portal backend.

The canonical configuration is [examples/halley.rune](examples/halley.rune).
It is also the bootstrap template used when no user configuration exists.
Configuration, keybind, node, animation, and font changes live-reload as one
validated snapshot.

Build the workspace with:

```sh
cargo build --release --workspace
```

Run `target/release/halley --winit` for a nested development session or
`target/release/halley --session` for a real TTY session. `halleyctl` exposes
output, node, and Bearings controls; `halleyctl --help` lists the current
surface.

References:

- [Keybind triggers and actions](docs/keybinds.md)
- [Nodes, decay, focus rings, landmarks, and physics](docs/nodes.md)
- [Bearings](docs/bearings.md)
- [Apogee and Alt+Tab](docs/apogee.md)
- [Fonts](docs/fonts.md)
- [Animations](docs/animations.md)
