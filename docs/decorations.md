# Window decorations

Halley draws one compositor-owned primary border around each managed top-level
window. The border and client content share a concentric rounded shape:

```rune
decorations:
  border:
    size 3
    radius 8
    colour-focused "#38d1eb"
    colour-unfocused "#474d59"
  end
end
```

`size` and `radius` are output pixels and follow camera zoom. `radius` describes
the client-content corner; the outer border radius is increased by `size`.
Setting `radius` to `0` restores square corners. Setting `size` to `0` hides the
border while retaining rounded client content.

Field-maximized windows retain their rounded border. Native fullscreen
transitions smoothly remove it so the final fullscreen surface is square and
eligible for direct scanout.

Popups, override-redirect X11 surfaces, layer-shell surfaces, and isolated
window capture sources are not decorated.
