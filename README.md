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
- output-local fullscreen, separate from ordinary decoration maximize;
- native screenshots, screencasting, and an XDG desktop portal backend.

The canonical configuration is [examples/halley.rune](examples/halley.rune).
It is also the bootstrap template used when no user configuration exists.
Configuration, keybind, node, animation, and font changes live-reload as one
validated snapshot.

Build the workspace with:

```sh
cargo build --release --workspace
```

The default compositor includes D-Bus accessibility, systemd session
integration, native XWayland, and the nested winit backend. Distributions can
select a different user manager or a smaller compositor with Cargo features;
see [building and packaging](docs/packaging.md) for the supported combinations
and resource install paths.

Run `target/release/halley --winit` for a nested development session or
`target/release/halley --session` for a real TTY session. Pass `-c PATH` (or
`--config PATH`) to select a configuration explicitly. `halleyctl` exposes
output, DPMS, node, Bearings, configuration verification, and quit controls;
`halleyctl --help` lists the current surface. `halley-lift` is the bundled
search and action launcher; the default `Mod+D` binding toggles it.

Third-party launchers, panels, and automation should use the supported
[`halley-api` Rust SDK](docs/api.md). It exposes typed commands and queries,
structured errors, capability negotiation, and sequenced state subscriptions;
the internal postcard IPC codec is not the external compatibility boundary.

On the TTY backend, `halleyctl dpms off|on|toggle [-o OUTPUT]` controls
connector power. Omitting `--output` applies the command to every active
output. Keyboard and pointer input wake the selected sleeping output, or every
output when the whole layout is asleep. The nested winit backend rejects DPMS
commands.

References:

- [Keybind triggers and actions](docs/keybinds.md)
- [Compositor API for external tools](docs/api.md)
- [Field behavior, maximize, zoom, and close succession](docs/field.md)
- [Nodes, decay, focus rings, landmarks, and physics](docs/nodes.md)
- [Bearings](docs/bearings.md)
- [Apogee and Alt+Tab](docs/apogee.md)
- [Compositor overlays and configuration notices](docs/overlays.md)
- [Compositor backgrounds](docs/backgrounds.md)
- [Managed-window rules](docs/window-rules.md)
- [Screen sharing and the desktop portal](docs/portal.md)
- [Wayland protocol support](docs/wayland-protocols.md)
- [XWayland policy and conformance baseline](docs/xwayland.md)
- [Fonts](docs/fonts.md)
- [Animations](docs/animations.md)
- [Window decorations](docs/decorations.md)
- [Building, session managers, and packaging](docs/packaging.md)
