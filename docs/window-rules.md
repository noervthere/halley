# Window rules

Window rules apply one shared policy to managed native Wayland and XWayland
windows. X11 override-redirect menus and tooltips are not managed windows and
are deliberately excluded.

```rune
rules:
  rule:
    app-id ["firefox", r"org\.mozilla\..*"]
    title r"Picture.*"
    width 720
    height 520
    opacity 0.90
    blur true
    spawn-placement "cursor"
    cluster-participation "float"
  end
end
```

Rules are evaluated from top to bottom and the first matching rule wins.
Strings match exactly and case-sensitively. `r"..."` values are regular
expressions. An array is an OR-list. When both `app-id` and `title` are
present, both fields must match.

For native Wayland, `app-id` is the XDG toplevel app ID. For XWayland it is
the X11 window class. `title` uses the corresponding toplevel/window title.

## Effects

- `width` and `height` set the requested initial size and must appear together.
  Halley enforces a safe minimum of 96 by 72 logical pixels.
- `opacity` accepts `0.0` through `1.0` and applies to content, popups,
  decorations, shadows, and transitions as one visual policy.
- `blur` explicitly enables or disables compositor window blur for the match.
  It takes precedence over the global automatic window-blur decision; an
  explicit client background-effect request is still honored.
- `spawn-placement` controls only initial placement. Existing windows are never
  moved by a rule reload or identity change.

Placement values are:

- `viewport-center`: center in the current output camera view;
- `cursor`: center under the pointer;
- `center`: center on the parent window, falling back to the viewport;
- `adjacent`: use the first free right, left, below, or above position beside
  the parent or focused window, then fall back to the viewport;
- `app`: center on the parent, otherwise try adjacent to the focused window,
  then fall back to the viewport.

Omitting `spawn-placement` uses the same viewport-centered placement as
`viewport-center`.

`cluster-participation` accepts `layout` or `float`. Halley parses, validates,
and retains this value now so the configuration is stable for the upcoming
cluster runtime. It does not yet change window presentation. The old
`overlap-policy` field is accepted only as a migration aid and has no runtime
effect; new configurations should omit it.

Live reload re-resolves visual fields such as opacity and blur for mapped
windows. Initial size and placement remain initial-map decisions.
