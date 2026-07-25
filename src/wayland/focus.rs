use smithay::desktop::{layer_map_for_output, LayerSurface};
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
pub fn current(wayland: &WaylandState) -> Option<KeyboardFocus> {
    for layer in [Layer::Overlay, Layer::Top] {
        if let Some(surface) =
            first_interactive(wayland, layer, KeyboardInteractivity::Exclusive, None)
        {
            return Some(KeyboardFocus::ExclusiveLayer(surface));
        }
        if let Some(surface) = selected_on_demand(wayland, layer) {
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
        !wayland.unmapped_layers.contains(layer.wl_surface())
            && layer.cached_state().keyboard_interactivity == KeyboardInteractivity::OnDemand
    });
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
