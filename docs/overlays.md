# Compositor overlays

The `overlays:` section is the shared style contract for compositor-owned UI:
Apogee title bands, the Alt+Tab rail, Bearings chips, configuration notices,
and the exit confirmation. It deliberately does not restyle client window
decorations or node labels; those retain their existing sections.

```rune
overlays:
  background-colour "auto"
  text-colour "auto"
  error-colour "#fb4934"
  shape "square"
  borders true
  border-source "primary"

  notifications:
    position "top-center"
    success-duration-ms 4000
    error-duration-ms 9000
  end
end
```

`background-colour`, `text-colour`, and `error-colour` accept `auto`, `light`,
`dark`, or a `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa` colour. American `color`
spellings are accepted as aliases. `shape` is `square` or `rounded`.
`borders false` removes overlay borders. `border-source` is `primary` or
`secondary`; until a secondary decoration palette is configured, `secondary`
falls back to the primary focused decoration colour.

Notification positions are `top-left`, `top-center`, `top-right`,
`bottom-left`, `bottom-center`, and `bottom-right`. Durations are positive
milliseconds. The renderer builds every card at its final pixel dimensions,
so changing shape or output scale does not stretch a small blurred texture.

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
