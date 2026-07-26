use smithay::desktop::{LayerSurface, Space, Window, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer};

use super::WaylandState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyboardFocus {
    ExclusiveLayer(WlSurface),
    OnDemandLayer(WlSurface),
    Window(WlSurface),
}

impl KeyboardFocus {
    pub fn surface(&self) -> WlSurface {
        match self {
            Self::ExclusiveLayer(surface)
            | Self::OnDemandLayer(surface)
            | Self::Window(surface) => surface.clone(),
        }
    }

    pub fn bypasses_shortcuts(&self) -> bool {
        matches!(self, Self::ExclusiveLayer(_))
    }
}

/// Resolves actual seat focus without conflating it with Halley's persistent
/// focused-window state. Higher visual layers win; on-demand lower layers
/// selected by a click still precede the remembered window, while exclusive
/// Bottom/Background surfaces only win when no window is focused.
pub fn current(
    wayland: &WaylandState,
    fullscreen: &super::fullscreen::FullscreenManager,
    now: std::time::Duration,
) -> Option<KeyboardFocus> {
    if let Some(surface) = first_interactive(
        wayland,
        Layer::Overlay,
        KeyboardInteractivity::Exclusive,
        None,
    ) {
        return Some(KeyboardFocus::ExclusiveLayer(surface));
    }
    if let Some(surface) = selected_on_demand(wayland, Layer::Overlay) {
        return Some(KeyboardFocus::OnDemandLayer(surface));
    }
    if !fullscreen.covers_any_top(wayland, wayland.focused_window.as_ref(), now) {
        if let Some(surface) =
            first_interactive(wayland, Layer::Top, KeyboardInteractivity::Exclusive, None)
        {
            return Some(KeyboardFocus::ExclusiveLayer(surface));
        }
        if let Some(surface) = selected_on_demand(wayland, Layer::Top) {
            return Some(KeyboardFocus::OnDemandLayer(surface));
        }
    }

    for layer in [Layer::Bottom, Layer::Background] {
        if let Some(surface) = selected_on_demand(wayland, layer) {
            return Some(KeyboardFocus::OnDemandLayer(surface));
        }
    }

    if let Some(surface) = wayland.focused_window.clone() {
        return Some(KeyboardFocus::Window(surface));
    }

    for layer in [Layer::Bottom, Layer::Background] {
        if let Some(surface) =
            first_interactive(wayland, layer, KeyboardInteractivity::Exclusive, None)
        {
            return Some(KeyboardFocus::ExclusiveLayer(surface));
        }
    }

    None
}

pub fn select_layer(wayland: &mut WaylandState, selected: Option<LayerSurface>) {
    wayland.focused_layer = selected.filter(|layer| {
        retains_on_demand_focus(
            layer.layer_surface().alive(),
            wayland.unmapped_layers.contains(layer.wl_surface()),
            layer.cached_state().keyboard_interactivity,
        )
    });
}

/// Drops click-selected layer focus after the client makes that layer
/// ineligible. Protocol requests are dispatched in batches, so this cleanup
/// runs immediately before the batch's single seat-focus reconciliation.
pub fn refresh_selected_layer(wayland: &mut WaylandState) {
    let retain = wayland.focused_layer.as_ref().is_some_and(|layer| {
        retains_on_demand_focus(
            layer.layer_surface().alive(),
            wayland.unmapped_layers.contains(layer.wl_surface()),
            layer.cached_state().keyboard_interactivity,
        )
    });
    if !retain {
        wayland.focused_layer = None;
    }
}

/// Records the output chosen by click-to-focus. Resolution is deliberately
/// deferred until placement so unplugging an output cannot leave a stale
/// Smithay handle as the spawn target.
pub fn select_output(wayland: &mut WaylandState, output: &Output) {
    wayland.focused_output = Some(output.name());
}

/// Returns the click-focused output while it remains mapped, otherwise the
/// first mapped output. The fallback preserves startup behavior before the
/// first click and makes hot-unplug self-healing.
pub fn selected_output(wayland: &WaylandState) -> Option<&Output> {
    selected_output_in(&wayland.space, wayland.focused_output.as_deref())
}

fn selected_output_in<'a>(
    space: &'a Space<Window>,
    focused_output: Option<&str>,
) -> Option<&'a Output> {
    focused_output
        .and_then(|name| space.outputs().find(|output| output.name() == name))
        .or_else(|| space.outputs().next())
}

pub fn forget_layer(wayland: &mut WaylandState, surface: &WlSurface) {
    if wayland
        .focused_layer
        .as_ref()
        .is_some_and(|layer| layer.wl_surface() == surface)
    {
        wayland.focused_layer = None;
    }
}

fn selected_on_demand(wayland: &WaylandState, layer_kind: Layer) -> Option<WlSurface> {
    let selected = wayland.focused_layer.as_ref()?;
    if selected.layer() != layer_kind {
        return None;
    }
    first_interactive(
        wayland,
        layer_kind,
        KeyboardInteractivity::OnDemand,
        Some(selected),
    )
}

fn retains_on_demand_focus(
    alive: bool,
    unmapped: bool,
    interactivity: KeyboardInteractivity,
) -> bool {
    alive && !unmapped && interactivity == KeyboardInteractivity::OnDemand
}

fn first_interactive(
    wayland: &WaylandState,
    layer_kind: Layer,
    interactivity: KeyboardInteractivity,
    selected: Option<&LayerSurface>,
) -> Option<WlSurface> {
    wayland.space.outputs().find_map(|output| {
        let map = layer_map_for_output(output);
        map.layers_on(layer_kind).rev().find_map(|layer| {
            if wayland.unmapped_layers.contains(layer.wl_surface())
                || selected.is_some_and(|selected| selected != layer)
                || layer.cached_state().keyboard_interactivity != interactivity
            {
                return None;
            }
            Some(layer.wl_surface().clone())
        })
    })
}

#[cfg(test)]
mod tests {
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    use smithay::utils::{Physical, Size, Transform};

    use super::*;

    fn output(name: &str, size: Size<i32, Physical>) -> Output {
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "halley-next".into(),
                model: "test".into(),
                serial_number: "test".into(),
            },
        );
        let mode = Mode {
            size,
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output
    }

    #[test]
    fn clicked_output_wins_and_missing_output_falls_back() {
        let left = output("DP-1", Size::from((2560, 1440)));
        let right = output("DP-2", Size::from((1920, 1200)));
        let mut space = Space::<Window>::default();
        space.map_output(&left, (0, 0));
        space.map_output(&right, (2560, 0));

        assert_eq!(
            selected_output_in(&space, Some("DP-2")).map(Output::name),
            Some("DP-2".to_string())
        );
        assert_eq!(
            selected_output_in(&space, Some("unplugged")).map(Output::name),
            Some("DP-1".to_string())
        );
    }

    #[test]
    fn on_demand_focus_requires_a_live_mapped_interactive_layer() {
        let cases = [
            (true, false, KeyboardInteractivity::OnDemand, true),
            (false, false, KeyboardInteractivity::OnDemand, false),
            (true, true, KeyboardInteractivity::OnDemand, false),
            (true, false, KeyboardInteractivity::None, false),
            (true, false, KeyboardInteractivity::Exclusive, false),
        ];

        for (alive, unmapped, interactivity, expected) in cases {
            assert_eq!(
                retains_on_demand_focus(alive, unmapped, interactivity),
                expected
            );
        }
    }
}
