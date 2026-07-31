# Field behavior and maximize

The `field:` section owns the shared spatial policy for window clearance,
camera zoom, close succession, and Halley's field-maximized state:

```rune
field:
  gap 20.0
  close-restore-focus true
  close-restore-nodes false
  close-restore-pan "if-offscreen"

  zoom:
    enabled true
    step 1.10
    min 0.35
    smooth-rate 12.5
  end
end
```

`gap` is measured in logical output pixels. It is both the inset between a
field-maximized window border and the usable monitor work area and the
clearance reserved between collapsed landmarks and active windows. Active
windows can still overlap each other.

Camera zoom is always smooth and never magnifies beyond native scale 1.0.
`min` is clamped from 0.05 through 1.0, `step` from 1.001 through 8.0, and
`smooth-rate` from 0.1 through 120.0. The old top-level `zoom:` section and
`field.gap-px` remain compatibility inputs through the 0.1 series. A canonical
`field.zoom` or `field.gap` value wins independently when both spellings are
present. `field.zoom.max`, `smooth`, `filter`, and `sharpen` are deliberately
unsupported and produce a configuration verification error.

## Field maximize

The default `$var.mod+m` binding runs `maximize-focused`. The aliases
`maximize_focused`, `toggle-maximize`, and `toggle_maximize` select the same
action. Pressing the action again reverses it.

Field maximize is separate from fullscreen:

- it fills the layer-shell non-exclusive work area, inset by `field.gap`, so
  panels remain visible;
- it preserves the target's stack slot and every bystander's stack slot;
- windows and nodes remain visible in front or behind according to that
  existing order;
- it keeps the output camera center fixed, eases its scale to native 1.0, and
  locks pan and zoom only on that monitor until exit;
- it sets the client's Wayland/X11 maximized state, so decoration buttons and
  titlebar double-clicks remain synchronized with the compositor;
- its presentation transform is shared by rendering, hit testing, popups,
  screenshots, and screencasts.

Each output has an independent maximize session. Selecting a different window
on the same output retargets without restoring and reapplying the camera.
Mod+left drag ends field maximize and continues with the same cursor-relative
grab point. Resize is blocked while maximized. Collapsing, minimizing, closing,
unmapping, or entering fullscreen ends field maximize cleanly.

An active cluster workspace is the exception to the normal visible-stack rule:
maximizing a cluster member gives that member an exclusive presentation above
the desktop while the rest of the cluster and field are covered. Exiting
maximize smoothly returns it to the cluster's current tile without changing its
persistent stack slot. Cluster fullscreen uses the same exclusive/restore
behavior while retaining fullscreen's panel policy.

The transition crossfades the outgoing and incoming window textures, using
the same configurable spring/easing controls as fullscreen. Its default keeps
the original ease-in-out cubic feel:

```rune
animations:
  maximize:
    enabled true
    motion "easing"
    duration-ms 240
    curve "ease-in-out-cubic"
  end
end
```

For spring motion, set `motion "spring"` with `damping-ratio` and `stiffness`.
See [Animations](animations.md) for every curve and motion knob.

## Focus after close

When `close-restore-focus` is true, closing the focused window selects the
most recently focused surviving window on the same output, then falls back to
the global MRU window. An active successor is focused normally. A collapsed
successor remains collapsed and becomes Halley's logical node focus by default.
Set `close-restore-nodes` to true to restore and focus that node in the same
close action. When `close-restore-focus` is false, Halley clears focus instead
of selecting a successor, regardless of `close-restore-nodes`.

`close-restore-pan` controls the camera when an active successor is focused or
a collapsed successor is restored:

- `"never"` changes focus without moving the camera.
- `"if-offscreen"` is the default. A fully offscreen successor is moved into
  view by the minimum required pan; a partly clipped window does not move.
- `"always"` centers the successor.

Restoration and camera motion begin together. Closing a field-maximized target
first restores its exact pre-maximize camera, then applies the successor
policy. Closing a window stacked above a maximized window leaves that
maximize session intact.
