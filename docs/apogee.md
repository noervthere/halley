# Apogee and Alt+Tab

`Mod+O` opens Apogee across every configured output. Each monitor lays out its
own attached windows and collapsed nodes, preserves their aspect ratios, and
animates them directly from their desktop presentation into the mosaic. The
old-Halley spatial packer keeps field-space reading order, weights slots by
window area, prevents tile overlap, and reserves the original upper core band.
`max-rows` is limited to the old supported range of 1 through 5.

Apogee replaces the desktop scene while it is visible. The only scene behind
its tiles is the wallpaper/background layer: normal windows, nodes, panels,
and other desktop layers never poke through the dimming backdrop.
Its tile title bands and Alt+Tab's cards use the shared
[`overlays:`](overlays.md) shape, palette, and border settings.

Arrow keys navigate spatially across monitor geometry. Enter or a tile click
selects a window, Escape or an outside click cancels, and `Mod+O` toggles the
overview. Selection is applied only after the closing animation finishes.
Collapsed selections restore through the normal one-action node policy.
Once closing begins Apogee stops consuming keyboard input, so a key pressed
during the fly-back belongs to the restored client instead of becoming stuck
in the overlay. Its compositor cursor is forced visible for the whole session,
including when the previously focused client had locked or hidden its cursor.

```rune
apogee:
  enabled true
  live-previews true
  preview-max-fps 30
  transition-ms 320
  gap 24.0
  max-rows 3
  background-dim 0.85
end
```

Tiles use reusable GPU-local offscreen textures. DMA-BUF client buffers remain
on the GPU through import and composition; there is no CPU readback. A client
commit only dirties that window's cached tile, commits are coalesced, and
preview frame callbacks are limited independently per output to
`preview-max-fps`. Once transitions settle and clients stop committing,
Apogee produces no continuing render frames.

The default four-finger swipe up opens Apogee interactively. Progress follows
net finger travel in both directions, so reversing a slow swipe returns through
the exact same geometry and opacity state without rebuilding the overlay or
flashing. Release commits at 40% progress or on an upward flick; otherwise the
remaining distance animates back to zero. Four-finger swipe down closes an
already-open overview after the configured threshold.

```rune
input:
  gestures:
    swipe-threshold-px 120.0
    swipe-up-4 "apogee-open"
    apogee-swipe-down-4 "apogee-close"
  end
end
```

`Alt+Tab` and `Alt+Shift+Tab` use the same MRU window set, including collapsed
nodes. The center card is selected, releasing Alt commits, and Escape cancels.
The old five-card rail is restored: preview-hugging chip chrome, an overlaid
app-icon/title band, monitor badge, and `NODE` pill. The selected preview is
refreshed live while neighboring cards keep frozen GPU stills, avoiding a
full-rail redraw or CPU copy. The carousel and Apogee are mutually exclusive.
Cluster/core presentation is reserved for the later cluster runtime and is not
synthesized here.
