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

  titlebars:
    enabled true
    button-position "left"
    show-buttons true
    show-icons false
    show-title true
    radius 8
    height 32

    colour-focused "#38d1eb"
    colour-unfocused "#474d59"
    foreground-colour-focused "#101418"
    foreground-colour-unfocused "#f4f5f7"
    button-hover-colour "#ffffff"
    button-pressed-colour "#101418"
  end
end
```

`size` and `radius` are output pixels and follow camera zoom. `radius` describes
the client-content corner; the outer border radius is increased by `size`.
Setting `radius` to `0` restores square corners. Setting `size` to `0` hides the
border while retaining rounded client content.

An enabled titlebar supplies the top edge of a server-decorated window. The
border then paints only its left, right, and bottom edges. `titlebars.radius`
rounds only the titlebar's top corners; `border.radius` continues to round only
the client body's bottom corners. The requested titlebar `height` is clamped to
1-96 pixels and is raised internally when enabled buttons, the application
icon, or the global font need more room.

Buttons are ordered close/maximize/minimize on the left and
minimize/maximize/close on the right. Hover and pressed colors tint both the
button glyph and a translucent backplate. The maximize glyph does not change
when the window is field-maximized.

Field-maximized windows retain their rounded border. Entering true fullscreen
removes compositor chrome immediately, while the client content and geometry
continue animating. The fullscreen surface is square and eligible for direct
scanout from its first presented frame.

Popups, override-redirect X11 surfaces, layer-shell surfaces, and isolated
window capture sources are not decorated.

## Future custom button SVGs

Halley's built-in button artwork and future user-provided artwork are alpha
masks. Source RGB colors are ignored; Halley applies normal, hover, pressed,
and disabled colors. Author each button on a square canvas with a finite square
`viewBox`, a transparent background, one monochrome silhouette, and comfortable
padding around every edge. Convert text and strokes to paths and export as
plain SVG when practical.

Button files must not contain scripts, animation, external references,
embedded bitmap images, fonts, filters, gradients, or masks. Inkscape metadata
is harmless and may remain. Custom file-path settings are not exposed yet;
when they are added, invalid assets will fall back to the matching bundled
button and emit a precise warning.
