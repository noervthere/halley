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

These globals are intended for shells and tools such as DMS. They are
independent: data-control support does not require blur, and a client that
does not bind either protocol follows Halley's existing rendering and
clipboard paths.
