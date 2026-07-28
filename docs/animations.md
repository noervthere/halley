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
    epsilon 0.0001
  end
end
```

Spring motion accepts `damping-ratio`, `stiffness`, and `epsilon`. Lower
damping ratios allow overshoot, higher stiffness moves faster, and epsilon
controls how close to the target the motion must get before it is complete.

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

Either type can use any easing curve through `duration-ms` and `curve`, or
select `motion "spring"` and use the same spring fields. Changing the type
never selects a different curve or duration implicitly.
