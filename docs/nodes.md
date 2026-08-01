# Nodes, decay, and the focus ring

Every managed XDG toplevel and normal Xwayland window has one stable node ID
for its lifetime. An active node is a normal window in the compositor space. A
collapsed node remains protocol-alive but is removed from the window/input
space and represented by a compositor-rendered marker at the same field
position.

Collapse preserves visual stack depth. The captured window flies and shrinks
into its final node position at the same layer it occupied: a back window
drops behind every window that was above it, a middle window stays between its
neighbors, and a front window drops in front. The emerging marker shares that
depth instead of jumping to a global node overlay.

Click a collapsed marker once to restore and focus its window. This is one
atomic action: Halley does not first center the camera, leave the marker
collapsed, and require a second click. `$var.mod+n` runs the same state toggle
for the focused window.

A client's minimize request is the one-way form of that action: it collapses
the window into its existing node and never restores an already-collapsed
node. Clicking the node (or using `toggle-state`) restores it. XWayland's
`_NET_WM_STATE_HIDDEN` is kept in sync so X11 clients can suspend while
collapsed and resume when restored.

Collapsing the focused window preserves that node as Halley's logical focus,
so node-aware commands such as `close-focused` still target it. The hidden
client's Wayland keyboard focus is cleared before unmapping, so it cannot keep
receiving typed input while collapsed. In hover-focus mode, hovering a marker
makes that node the command target; the default Mod+Q then closes that node.

A plain left press is resolved as a click or grab when the pointer is released
or moves. Releasing before moving 8 screen pixels performs the single-click
restore. Moving at least 8 pixels keeps the window collapsed and grabs its
marker. Mod+left grabs immediately, without restoring even if the button is
released without movement. Both forms preserve the exact point where the
marker was grabbed and can carry it between outputs without centering it under
the pointer.

## Restoration and centering

Centering is optional:

```rune
node:
  click-collapsed-pan "never"
end
```

The accepted values are:

- `never` restores in place and does not move the camera. This is the default.
- `if-offscreen` centers only when the marker is outside the current output.
- `always` centers every restored node.

When centering is selected, camera motion and window restoration begin in the
same action. Moving or restoring a node does not change its stable ID.

## Automatic decay

An unfocused active window becomes a node after its eligibility timer expires:

```rune
decay:
  enabled true
  outside-delay-seconds 180
  inside-delay-seconds 1800
end
```

There is no active-window count cap. Focused windows, fullscreen or
fullscreen-pending windows, field-maximized windows, and windows in an
interactive move/resize grab are hard-protected from decay. Changing between
protected, inside-ring, and outside-ring status starts a fresh timer; stale
time from an earlier status is never reused.

Each output has its own camera-centered ellipse. Keep these as repeatable,
standalone top-level blocks; they are not nested inside the hardware `output:`
blocks:

```rune
focus-ring:
  output "DP-1"
  radius-x 820.0
  radius-y 420.0
  offset-x 0.0
  offset-y 0.0
end

focus-ring:
  output "DP-2"
  radius-x 700.0
  radius-y 360.0
  offset-x 0.0
  offset-y 20.0
end
```

A window uses the shorter outside delay only when at least 90% of its footprint
is outside the ellipse for its owning output. Moving it to another output or
changing that output's ring starts a fresh eligibility timer. Editing one
output does not reset timers on the others. An output without a matching block
uses the default 820×420 ring; an old unkeyed block remains accepted only as a
migration fallback.

The ring is normally hidden. Saving a changed focus-ring configuration previews
it briefly; `debug.show-focus-ring true` keeps it visible:

```rune
debug:
  show-focus-ring false
end
```

## Appearance

```rune
font:
  family "monospace"
  size 11
end

node:
  show-labels "hover"
  show-app-icons "always"
  shape "squircle"
  label-shape "squircle"
  icon-size 0.72
  opacity 1.0
  background-colour "auto"
  border-colour-hover "use-window-active"
  border-colour-inactive "use-window-inactive"
end
```

Shapes accept `square` or `squircle`. `shape` and `label-shape` are the only
supported keys; the redundant `node-shape` and `node-label-shape` spellings
were removed. Labels use dedicated rectangle shaders and the shared Cosmic
Text renderer, including configured font family/weight suffixes, measured
centering, contrast-aware text, edge flipping, and the old hover
slide/grow/fade. See [Fonts](fonts.md) for global typography behavior.

Display policies accept `off`, `hover`, or `always`. Real application icons
are resolved from desktop entries and icon themes in a background worker.
The marker stays blank while an icon is loading or unavailable, so a cold
first collapse never flashes a temporary letter before the real icon appears.

With `show-labels "hover"`, only an explicit pointer hover reveals the label;
logical or keyboard focus still highlights the marker but does not open its
label. The old-Halley back-loaded slide/grow/fade appears first, then 1500 ms
of uninterrupted hover replaces it with a live, aspect-fitted window preview.
Leaving the marker, changing hover targets, pressing it, or beginning a node
grab cancels the dwell and closes the hover UI. A grab keeps labels and
previews suppressed until later pointer motion deliberately targets a node
again.

## Cluster bloom joining

Rest the pointer on a collapsed cluster core to open its member bloom. While
the bloom is open, its core is temporarily fixed in place. Mod+left-drag a
normal Field window against the core: the window docks at the same non-overlap
distance used by `field.gap` instead of pushing the core away.

Hold the window there for `clusters.join-dwell-ms`. When the dwell completes,
the core's original border changes to the focused colour and thickens to five
pixels without changing its fill or icon. A light wash of that same colour
marks the dragged window; releasing then adds the window to that cluster.
Moving away, closing the bloom, changing outputs, cancelling the grab, or
releasing before the affordance appears cancels the join. Closed and closing
blooms never accept windows.

Clicking or grabbing another window leaves the bloom open, matching old
Halley. A plain click on the empty Field, clicking or dragging the bloomed core,
activating a cluster, or an explicit keyboard action closes it. Typing into a
different focused window after its drag has ended also closes the abandoned
bloom without changing that window's focus.

The legacy `clusters.join-distance-px` key remains parseable so existing
configurations continue to load, but it no longer affects this interaction.
Contact is determined from the actual window and core bounds plus `field.gap`.

## Landmark non-overlap

Collapsed nodes are the only landmarks. Active windows may overlap other
active windows freely, but nodes remain clear of both nodes and active
windows:

```rune
field:
  gap 20.0
end

placement:
  landmarks:
    strategy "nearest-free"
    normal-blocker "relocate"
  end
end
```

Collapse starts at the window center and slides to the nearest legal location.
A new or restored active window keeps its placement and relocates blocking
nodes. An interactively dragged window is authoritative and pushes unpinned
nodes; `halleyctl node move` remains a discrete legal-placement operation.
Marker collision is screen-constant across camera zoom; transient labels and
shadows never reserve space.

The same `field.gap` insets field-maximized windows from the usable output
work area. See [Field behavior and maximize](field.md).

## Rigid and physics movement

```rune
physics:
  enabled true
  damping 0.45
end
```

With physics disabled, a dragged window transfers displacement directly: after
contact, moving the window one field unit moves the contacted node or movable
node chain one field unit along the contact normal. There is no slide animation
on interactive displacement.

With physics enabled, grabbed windows and nodes are kinematic authorities and
impart bounded old-Halley momentum to the objects they contact. Pushed objects
use frame-rate-independent damping and continue settling after release; the
grabbed object itself does not fling. A grabbed node slides around an active
window in rigid mode and can bump that window in physics mode. Active windows
still never collide with other active windows. Pinned nodes remain fixed.

Pointer reports only update the latest drag target and sampled authority
velocity. Physics advances once per rendered frame using real elapsed time, so
high-polling mice do not multiply damping. Releasing an active window flushes
its final target and holds that window fixed for 350 ms while displaced nodes
settle, matching old Halley's drop behavior.

## `halleyctl node`

The original old-Halley command surface remains intact, with explicit
collapse, restore, and toggle controls added for complete remote state control:

```text
halleyctl node list [--output OUTPUT] [--json]
halleyctl node info [SELECTOR] [--output OUTPUT] [--json]
halleyctl node focus [SELECTOR] [--output OUTPUT]
halleyctl node move left|right|up|down [SELECTOR] [--output OUTPUT]
halleyctl node collapse [SELECTOR] [--output OUTPUT]
halleyctl node restore [SELECTOR] [--output OUTPUT]
halleyctl node toggle [SELECTOR] [--output OUTPUT]
halleyctl node close [SELECTOR] [--output OUTPUT]
```

Selectors are `focused`, `latest`, a bare numeric ID, `id:NUMBER`,
`title:TEXT`, and `app:TEXT`.
Title and app matching are case-insensitive substrings and return an error when
ambiguous. With no selector, commands use the focused node and otherwise fall
back to the latest node. `--output` validates and limits selection to one
connector.

`list` groups nodes by output and marks the focused node with `*`, the latest
node with `+`, and other nodes with `-`. Each text entry includes its state,
application ID, role, protocol family, modal/parent relationships, child-popup
count, focus/latest flags, field position, and size. `info` prints the same
fields for one node. This makes the default output useful for beginners while
leaving `--json` stable for scripts.

`focus` restores a collapsed node or focuses an active one. `move` requests an
80-field-unit shift and then resolves the nearest legal landmark/window
destination. `collapse` and `restore` explicitly set the selected node state
and are idempotent; `toggle` inverts it. `close` sends the appropriate XDG or
X11 close request without restoring first. `--json` is available for `list`
and `info`.

This interface uses IPC protocol version 10. Keep `halleyctl` and the compositor
from the same build because postcard enum variants are positional on the wire.

Offscreen active windows and collapsed nodes are also available through
[Bearings](bearings.md), including the old
`halleyctl bearings show|hide|toggle|status` controls.

## Lifecycle boundaries

Null-buffer unmaps, remaps, metadata commits, activation, fullscreen requests,
and destruction remain valid while a toplevel is collapsed. A client does not
need to be mapped in the render space for Halley to process those protocol
events. Layer-shell surfaces, popups, and X11 override-redirect windows are not
nodes.
