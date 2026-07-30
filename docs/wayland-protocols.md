# Wayland protocol support

Halley advertises `ext_background_effect_manager_v1` version 1 with the blur
capability. A committed `set_blur_region` is clipped to the requesting
surface, preserves ordered `wl_region` additions and subtractions, and is
drawn immediately behind that surface at its current layer or window stack
depth. Layer-shell roots and their popups use output-local coordinates;
ordinary toplevel roots follow the same camera, opening, fullscreen, and field
maximize presentation as their window.

All blur effects on one output share one persistent output-sized texture pool.
Each stack depth still performs its own framebuffer capture, so an upper
translucent surface includes lower windows and panels without including
content above itself. No full texture chain is allocated per requesting
surface. The nested winit backend explicitly runs the same framebuffer-effect
sequence as the DRM damage tracker.

Halley also advertises `ext_data_control_manager_v1` version 1. Clipboard and
primary-selection managers use the existing seat selections shared with
`wl_data_device` and `zwp_primary_selection`; Halley does not copy or retain
clipboard payloads. The source client's file descriptor is transferred
directly to the receiver by Smithay's selection implementation.

Halley advertises `ext_idle_notifier_v1` version 2 and reports activity from
the compositor's physical keyboard, pointer, gesture, touch, and tablet input
path before modal routing. Idle managers therefore continue receiving correct
idle/resume edges while compositor-owned interfaces such as screenshot and
portal source selectors consume the input instead of forwarding it to a
client surface.

Halley advertises `ext_session_lock_manager_v1` version 1. A lock request
immediately replaces every powered output with an opaque black scene; the
`locked` event is sent only after that generation has actually been submitted
on winit or page-flipped on every TTY output. Powered-off outputs are already
secure and do not delay confirmation. Lock surfaces are configured to each
output's logical size and are the only client surfaces rendered or given
keyboard, pointer, and touch focus until the owning, confirmed lock object
unlocks. Compositor bindings and ordinary client input are bypassed, and
screenshot plus screencast reads fail while locked. If the locker crashes or
destroys its surfaces, Halley deliberately remains locked with black outputs;
a second or unconfirmed lock object cannot unlock the session.

Halley advertises `wp_presentation` version 2 using `CLOCK_MONOTONIC`. On the
TTY backend, feedback is retained with the submitted DRM frame and completed
from its page-flip sequence and timestamp. Kernel monotonic timestamps carry
the `vsync`, `hw_clock`, and `hw_completion` flags, while zero-copy is reported
per surface from the DRM render-element state. The nested winit backend
completes feedback after host submission with monotonic time, fixed-refresh
metadata, and the `vsync` flag. Feedback is taken only for surface elements
actually included in the submitted frame, so compositor textures and hidden
or collapsed windows do not receive false presentation events.

On the TTY backend, Halley conditionally advertises
`wp_linux_drm_syncobj_manager_v1` version 1 only when the primary DRM device
supports syncobj eventfd notification. Support is demand-driven: advertising
the global does not allocate timelines, install event sources, or add waits.
Those resources are created only when an opting-in client imports a timeline
and commits a DMA-BUF with acquire and release points. The acquire point
blocks only that surface transaction without stalling the compositor event
loop; Smithay signals the release point when the compositor drops its final
reference to the buffer. The nested winit backend never advertises this
hardware protocol, and ordinary implicit-sync clients retain the existing
commit and render path.

These globals are intended for shells, tools, and latency-sensitive clients.
They are independent: data-control, idle notification, presentation timing,
and blur do not require one another, and clients that do not bind them follow
Halley's existing rendering, clipboard, and input paths.
