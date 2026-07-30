# Rune keybind trigger reference

Halley keybinds are entries in the `keybinds:` section of
`~/.config/halley/halley.rune`:

```rune
keybinds:
  mod "super"

  "Print" "grim ~/Pictures/screenshot.png"
  "$var.mod+Page_Up" "some-command"
  "$var.mod+click-forward" "some-other-command"
  "$var.mod+scroll-up" "zoom-in"
end
```

The last `+`-separated part is the trigger. Every earlier part is a modifier.
Trigger and modifier names are case-insensitive. Modifiers are matched exactly:
a bind without `shift`, for example, will not fire while Shift is held.

## Modifiers

| Rune name | Meaning |
|---|---|
| `$var.mod` | The key selected by `mod "super"`, `"alt"`, `"ctrl"`, or `"shift"` |
| `super`, `logo`, `mod4` | Super/Windows/Command-style logo modifier |
| `alt` | Alt |
| `ctrl`, `control` | Control |
| `shift` | Shift |

The configured `$var.mod` is remapped to Alt while running nested under
Winit, so development keybinds do not steal the host compositor's Super key.

## Keyboard triggers

Halley accepts every keysym name recognized by the system XKB library, not
only the names in these tables. The spelling shown below is the conventional
XKB spelling; matching is case-insensitive. This rule also covers uncommon
international, vendor, and legacy keysyms without requiring Halley to carry a
stale copy of XKB's thousands-entry catalog.

### Printable keys

| Physical label or group | Rune trigger |
|---|---|
| Letters | `a` through `z` |
| Number row | `0` through `9` |
| Space | `space` |
| Backtick, tilde | `grave`, `asciitilde` |
| Exclamation, at, number sign | `exclam`, `at`, `numbersign` |
| Dollar, percent, caret | `dollar`, `percent`, `asciicircum` |
| Ampersand, asterisk | `ampersand`, `asterisk` |
| Parentheses | `parenleft`, `parenright` |
| Minus, underscore | `minus`, `underscore` |
| Equals, plus | `equal`, `plus` |
| Square brackets | `bracketleft`, `bracketright` |
| Braces | `braceleft`, `braceright` |
| Backslash, pipe | `backslash`, `bar` |
| Semicolon, colon | `semicolon`, `colon` |
| Apostrophe, quotation mark | `apostrophe`, `quotedbl` |
| Comma, less-than | `comma`, `less` |
| Period, greater-than | `period`, `greater` |
| Slash, question mark | `slash`, `question` |

Shifted symbols still require `shift` in the chord because modifiers match
exactly. For example, use `"shift+plus"` for the `+` symbol. To bind the
physical equals key regardless of layout or produced symbol, use its
`keycode-N` form instead.

### Navigation and editing

| Physical key | Rune trigger |
|---|---|
| Escape | `Escape` |
| Tab | `Tab` |
| Shift+Tab symbol | `ISO_Left_Tab` |
| Enter | `Return` |
| Backspace | `BackSpace` |
| Insert | `Insert` |
| Delete | `Delete` |
| Home | `Home` |
| End | `End` |
| Page Up | `Page_Up` |
| Page Down | `Page_Down` |
| Arrow up | `Up` |
| Arrow down | `Down` |
| Arrow left | `Left` |
| Arrow right | `Right` |
| Menu/application | `Menu` |
| Help | `Help` |
| Undo, redo | `Undo`, `Redo` |
| Find, cancel | `Find`, `Cancel` |

### Function, system, and lock keys

| Physical key or group | Rune trigger |
|---|---|
| Function keys | `F1` through `F35` |
| Print Screen | `Print` |
| System Request | `Sys_Req` |
| Pause | `Pause` |
| Break | `Break` |
| Caps Lock | `Caps_Lock` |
| Num Lock | `Num_Lock` |
| Scroll Lock | `Scroll_Lock` |
| Clear | `Clear` |

### Modifier keys as triggers

| Physical key | Rune trigger |
|---|---|
| Left/right Shift | `Shift_L`, `Shift_R` |
| Left/right Control | `Control_L`, `Control_R` |
| Left/right Alt | `Alt_L`, `Alt_R` |
| Left/right Super | `Super_L`, `Super_R` |
| AltGr | `ISO_Level3_Shift` |

For Shift, Control, Alt, and Super, the modifier contributed by the trigger
itself is ignored for exact-modifier matching, so a bare `"Shift_L"` binding
works as expected. Other held modifiers still have to appear in the chord.

### Numeric keypad

| Physical key or group | Rune trigger |
|---|---|
| Keypad digits | `KP_0` through `KP_9` |
| Keypad decimal/separator | `KP_Decimal`, `KP_Separator` |
| Keypad add/subtract | `KP_Add`, `KP_Subtract` |
| Keypad multiply/divide | `KP_Multiply`, `KP_Divide` |
| Keypad equals | `KP_Equal` |
| Keypad enter | `KP_Enter` |
| Keypad navigation | `KP_Home`, `KP_End`, `KP_Left`, `KP_Right`, `KP_Up`, `KP_Down` |
| Keypad page keys | `KP_Page_Up`, `KP_Page_Down` |
| Keypad insert/delete | `KP_Insert`, `KP_Delete` |

### Common media and hardware keys

| Physical key or group | Rune trigger |
|---|---|
| Mute, volume down/up | `XF86AudioMute`, `XF86AudioLowerVolume`, `XF86AudioRaiseVolume` |
| Play, pause, stop | `XF86AudioPlay`, `XF86AudioPause`, `XF86AudioStop` |
| Previous/next track | `XF86AudioPrev`, `XF86AudioNext` |
| Microphone mute | `XF86AudioMicMute` |
| Brightness down/up | `XF86MonBrightnessDown`, `XF86MonBrightnessUp` |
| Keyboard brightness down/up | `XF86KbdBrightnessDown`, `XF86KbdBrightnessUp` |
| Power, sleep, wake | `XF86PowerOff`, `XF86Sleep`, `XF86WakeUp` |
| Display switch | `XF86Display` |
| Calculator, mail, browser home | `XF86Calculator`, `XF86Mail`, `XF86HomePage` |
| Search | `XF86Search` |
| Wi-Fi, Bluetooth | `XF86WLAN`, `XF86Bluetooth` |
| Touchpad toggle/on/off buttons | `XF86TouchpadToggle`, `XF86TouchpadOn`, `XF86TouchpadOff` |

These are keyboard buttons. Touchpad scroll gestures are not configurable
triggers yet.

## Mouse buttons and wheel

| Input | Rune trigger | Linux evdev code matched |
|---|---|---|
| Left button | `click-left` | 272 (`BTN_LEFT`) |
| Right button | `click-right` | 273 (`BTN_RIGHT`) |
| Middle button | `click-middle` | 274 (`BTN_MIDDLE`) |
| Back/side button | `click-back` | 275 or 278 (`BTN_SIDE`/`BTN_BACK`) |
| Forward/extra button | `click-forward` | 276 or 277 (`BTN_EXTRA`/`BTN_FORWARD`) |
| Wheel up | `scroll-up` | Physical wheel vertical negative |
| Wheel down | `scroll-down` | Physical wheel vertical positive |
| Wheel left | `scroll-left` | Physical wheel horizontal negative |
| Wheel right | `scroll-right` | Physical wheel horizontal positive |

Mouse-button bindings run before Halley's built-in Mod+left move and Mod+right
resize behavior. Mod+left moves active windows and collapsed nodes; collapsed
nodes can also be grabbed without Mod after crossing the 8 px click/drag
threshold. Those built-ins remain the fallback when no exact configured chord
matches. A bare left drag on the desktop background is always reserved for
panning.

Wheel binds apply only to a physical mouse wheel. High-resolution wheel input
is accumulated to one action per complete notch. Touchpad/finger scrolling
continues to the focused client and is not treated as a keybind.

## Raw Linux input codes

Raw codes cover hardware that has no useful XKB name:

| Form | Meaning | Example |
|---|---|---|
| `keycode-N` | Exact decimal Linux evdev keyboard code | `"keycode-99" "some-command"` |
| `button-N` | Exact decimal Linux evdev mouse-button code | `"$var.mod+button-279" "some-command"` |

Use the Linux evdev number as reported by tools such as `wev` or
`libinput debug-events`. Do not add XKB's internal offset; Halley adds it when
resolving `keycode-N`.

## Actions

The built-in action strings are `quit`, `close-focused`, `toggle-fullscreen`,
`toggle-state`, `apogee`, `bearings-show`, `bearings-toggle`, `cycle-focus`,
`cycle-focus-backward`, `open-terminal`, `zoom-in`, `zoom-out`, `zoom-reset`,
and `screenshot`. `Alt+Tab` and `Alt+Shift+Tab` open and navigate the focus
carousel; releasing Alt commits and Escape cancels. `Mod+O` opens or closes
the multi-monitor Apogee overview. Apogee stops trapping keys as soon as its
close transition begins.
The interactive screenshot menu and its area, screen, and window selectors
force the compositor cursor visible even if a client or inactivity policy had
hidden it.
`quit` opens Halley's modal exit confirmation instead of stopping the
compositor immediately. Enter confirms and Escape cancels while preserving
the focused client beneath it. Its appearance is configured in
[Compositor overlays](overlays.md).
See [Apogee and Alt+Tab](apogee.md) for navigation, gestures, and preview
performance.
Holding the default `Mod+Z` `bearings-show` binding exposes offscreen nodes and
hides them on the initiating key's release, even if Mod was released first.
`Mod+Shift+Z` toggles the same UI persistently. See
[Bearings](bearings.md) for layout, click behavior, configuration, and
`halleyctl` controls.
`toggle-state` collapses the focused window to a node or restores
it in one action; its default binding is `$var.mod+n`. Optional
`node.click-collapsed-pan` camera movement begins in that same action and never
creates a center-first second-click interaction. Any other action string is
launched as a command line.

`toggle-fullscreen` and a client decoration's maximize button both enter the
same Halley fullscreen presentation. The maximize button toggles that
presentation off on its second press, including reversing an exit transition
when pressed again. It never takes ownership of or cancels a fullscreen entered
with `Mod+F`, a client fullscreen request, or an initial fullscreen hint.
Top layer-shell
surfaces are suppressed per fullscreen output, independent of pointer or
keyboard focus on another monitor. Fullscreen also owns that output's complete
camera: zoom and pan ease to the window center and native 1.0 scale on the same
transition, every window stacked above it uses that camera, and camera input
remains locked until the pre-fullscreen monitor view has been restored.
For X11 windows, entering or leaving fullscreen preserves the window's current
stack slot; a window already above the fullscreen target stays above until the
user explicitly raises the fullscreen window.
