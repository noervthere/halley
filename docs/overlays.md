# Compositor overlays

The `overlays:` section is the shared style contract for compositor-owned UI:
Apogee title bands, the Alt+Tab rail, Bearings chips, the screenshot picker,
configuration notices, and the exit confirmation. It deliberately does not
restyle client window decorations or node labels; those retain their existing
sections.

All overlay text uses the exact family and size from the global
[`font:`](fonts.md) section. Overlays do not apply private small, normal, or
large font tiers.

```rune
overlays:
  background-colour "auto"
  text-colour "auto"
  error-colour "#fb4934"
  radius 8
  borders true

  notifications:
    position "top-center"
    success-duration-ms 4000
    error-duration-ms 9000
  end
end
```

`background-colour`, `text-colour`, and `error-colour` accept `auto`, `light`,
`dark`, or a `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa` colour. American `color`
spellings are accepted as aliases. `radius` is the overlay content-corner
radius in pixels; `0` is square.
`borders false` removes overlay borders. Overlay borders always use the
configured focused decoration border colour; there is no secondary/dual
overlay border source.
Apogee and Alt+Tab title bands, monitor badges, and `NODE` badges remain
borderless regardless of this setting, matching old Halley's label chrome.
Window preview textures use `decorations.border.radius`; the one surrounding
overlay border uses `overlays.radius`. Both settings use the same content-radius
semantics, so setting both to `8` produces matching curves.

Notification positions are `top-left`, `top-center`, `top-right`,
`bottom-left`, `bottom-center`, and `bottom-right`. Durations are positive
milliseconds. The renderer builds every card at its final pixel dimensions,
so changing its radius or output scale does not stretch a small blurred texture.

## Configuration lifecycle

On the first successful compositor load, including an explicit
`halley -c PATH` or `halley --config PATH`, Halley briefly shows:

```text
Configuration successfully loaded from /absolute/path/to/halley.rune
```

If any live reload is invalid, every rejected reload shows:

```text
Current configuration was unable to load properly. Run `halleyctl config verify` to see why.
```

The last valid configuration remains active. Correcting the file dismisses an
active error notice. File replacement, deletion, recreation, and ordinary
in-place edits are all observed.

Use the terminal verifier for the complete structured diagnostic:

```text
halleyctl config verify
halleyctl config verify -c PATH
halleyctl config verify --config PATH
```

Without `-c`, `halleyctl` asks the running compositor which file it selected.
If no compositor is reachable, it verifies the normal default configuration.
Verification is read-only: success exits 0, a rejected configuration exits 1,
and command or path-discovery errors exit 2.

## Exit confirmation

The `quit` keybind action and `halleyctl quit` open the same compositor-owned
confirmation. Enter stops Halley; Escape cancels. Other keyboard and pointer
input is held away from clients while the modal is active, but the focused
window beneath it is not changed.
