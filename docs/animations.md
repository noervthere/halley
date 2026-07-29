# Animation configuration

`animations.enabled` is the master switch. Each animation also has its own
`enabled` field, so an individual effect can be disabled without changing the
others.

Fullscreen defaults to a critically damped spring:

```rune
animations:
  fullscreen:
    enabled true
    motion "spring"
    damping-ratio 1.0
    stiffness 800.0
  end
end
```

Spring motion accepts `damping-ratio` and `stiffness`. Lower damping ratios
allow overshoot and higher stiffness moves faster. Halley keeps the settling
threshold internal.

An animation can use duration-based easing instead:

```rune
animations:
  fullscreen:
    enabled true
    motion "easing"
    duration-ms 250
    curve "ease-out-cubic"
  end
end
```

Available curves are `linear`, `ease-out-quad`, `ease-out-cubic`,
`ease-out-expo`, and `elastic`. The elastic curve overshoots before settling.

Window opening separates visual style from motion:

- `type "center-out"` scales the window outward from its final center.
- `type "fade"` keeps the final geometry and animates opacity.
- `type "launch"` moves from a meaningful origin to the final geometry along
  a restrained arc. It starts at 80% scale and low opacity, briefly reaches
  102%, then settles at full size.

The launch origin is the activating launcher's visual center when the client
uses `xdg-activation`. Otherwise Halley uses the cursor on the target output,
the focused window center, then the output center. Travel is capped so a new
window cannot fly across an entire display. A 220 ms `ease-out-cubic` motion is
the recommended starting point:

```rune
animations:
  window-open:
    enabled true
    type "launch"
    duration-ms 220
    curve "ease-out-cubic"
  end
end
```

Every type can use any easing curve through `duration-ms` and `curve`, or
select `motion "spring"` and use the same spring fields. Changing the type
never selects a different curve or duration implicitly.
