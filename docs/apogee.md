# Apogee and Alt+Tab

`Mod+O` opens Apogee across every configured output. Each monitor lays out its
own attached windows and collapsed nodes, preserves their aspect ratios, and
animates them directly from their desktop presentation into the mosaic.

Arrow keys navigate spatially across monitor geometry. Enter or a tile click
selects a window, Escape or an outside click cancels, and `Mod+O` toggles the
overview. Selection is applied only after the closing animation finishes.
Collapsed selections restore through the normal one-action node policy.

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

Live tiles sample the compositor's existing Wayland surface textures. DMA-BUF
clients therefore need no second full-resolution copy; SHM clients use the
normal import path. Commits are coalesced and preview frame callbacks are
limited per output to `preview-max-fps`. Once transitions settle and clients
stop committing, Apogee produces no continuing render frames.

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
The carousel and Apogee are mutually exclusive. Cluster/core presentation is
reserved for the later cluster runtime and is not synthesized here.
