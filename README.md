# Halley

Halley is a spatial Wayland compositor built around independent per-monitor
cameras, overlapping active windows, and collapsed node landmarks.

Current compositor-owned navigation includes:

- nodes, decay, focus rings, landmarks, and old-Halley contact physics;
- [Bearings](docs/bearings.md) for offscreen node navigation;
- [Apogee and Alt+Tab](docs/apogee.md) with live compositor previews;
- [field maximize and close succession](docs/field.md), independently per
  monitor;
- shared [compositor overlays](docs/overlays.md), including configuration
  notices and exit confirmation;
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
`target/release/halley --session` for a real TTY session. Pass `-c PATH` (or
`--config PATH`) to select a configuration explicitly. `halleyctl` exposes
output, DPMS, node, Bearings, configuration verification, and quit controls;
`halleyctl --help` lists the current surface.

On the TTY backend, `halleyctl dpms off|on|toggle [-o OUTPUT]` controls
connector power. Omitting `--output` applies the command to every active
output. Keyboard and pointer input wake the selected sleeping output, or every
output when the whole layout is asleep. The nested winit backend rejects DPMS
commands.

References:

- [Keybind triggers and actions](docs/keybinds.md)
- [Field behavior, maximize, zoom, and close succession](docs/field.md)
- [Nodes, decay, focus rings, landmarks, and physics](docs/nodes.md)
- [Bearings](docs/bearings.md)
- [Apogee and Alt+Tab](docs/apogee.md)
- [Compositor overlays and configuration notices](docs/overlays.md)
- [Wayland protocol support](docs/wayland-protocols.md)
- [Fonts](docs/fonts.md)
- [Animations](docs/animations.md)
- [Window decorations](docs/decorations.md)
