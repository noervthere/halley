# Clusters

A cluster is a persistent named workspace represented by one core in the Field.
Opening the core presents its members in either tiling or stacking layout. A
cluster remains valid when it has no members: closing the final member leaves
its name, slot, layout, and core available for later use.

Opening or selecting an empty cluster briefly shows a centered `name · layout`
label so the otherwise blank workspace remains identifiable. A populated
cluster reveals its windows without showing that activation label. Explicitly
changing a cluster's layout still flashes its updated name and layout.

A collapsed core's hover label prefers the right side, then searches the other
sides and corners for space not occupied by a visible window, ordinary node, or
another core. If every nearby position is obstructed, it uses the on-screen
position with the least overlap.

During zoom-out, unpinned collapsed cores and ordinary nodes reflow together
when their screen-constant collision footprints would overlap each other or an
active window. Pinned landmarks remain fixed.

## Startup clusters

Startup clusters are declared inside `autostart`. Each `cluster` block has a
required name and a compact `members` array containing only commands to run:

```rune
autostart:
  cluster:
    name "Work"
    members ["foot" "firefox"]
    layout "tiling"
    output "DP-1"
  end

  cluster:
    name "Inbox"
    members []
  end
end
```

Fields:

- `name` is required and must be non-empty.
- `members` is required. It is an array of command strings and may be empty.
- `layout` is optional: `"tiling"` or `"stacking"`. When omitted, the cluster
  configuration's default layout is used.
- `output` selects the connector where the cluster initially lives (for
  example, `"DP-1"`). It is optional; omission uses the primary output. An
  unavailable named output is skipped with a warning. The core may be moved to
  another output after startup like any other collapsed cluster core.

At most ten clusters can occupy an output. Startup cores are arranged as a
centered row near the top of each output, in declaration order.

### Launch attribution

On a real TTY session, Halley launches every member command with an XDG
activation token and an inherited private launch identifier. Native Wayland
clients can be attributed through the activation token or their process
ancestry; XWayland clients are attributed through PID ancestry. An attributed
window joins its declared cluster directly, even while that cluster is
collapsed, without claiming an unrelated “next window.”

A command may use shell syntax just like `autostart.once` and configured spawn
keybinds. Startup launch attribution is intentionally short-lived: commands
should start their initial application windows promptly.

Startup declarations are session initialization, not live-reload actions.
Restart the real TTY compositor session after changing them. Nested
`halley --winit` sessions create the declared cores for safe visual testing but
do not launch their member commands. Ordinary `once` and `on-reload` semantics
remain unchanged.
