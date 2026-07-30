use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::element::utils::select_dmabuf_feedback;
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::send_dmabuf_feedback_surface_tree;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use smithay::reexports::wayland_server::{DisplayHandle, GlobalDispatch};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufGlobalData, DmabufHandler, DmabufState,
};

use crate::backend::dmabuf::{DmabufCapabilities, SurfaceDmabufFeedback};
use crate::wayland::{WaylandState, window_is_on_output};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Advertisement {
    None,
    Version3,
    Version5(libc::dev_t),
}

fn advertisement(capabilities: &DmabufCapabilities) -> Advertisement {
    if capabilities.is_empty() {
        Advertisement::None
    } else if let Some(main_device) = capabilities.main_device() {
        Advertisement::Version5(main_device)
    } else {
        Advertisement::Version3
    }
}

/// Advertises the strongest truthful linux-dmabuf global for one renderer.
///
/// Feedback-capable version 5 needs a device-backed table. Renderers whose EGL
/// device cannot be resolved still support the version 3 format list.
pub fn create_global<D>(
    state: &mut DmabufState,
    display_handle: &DisplayHandle,
    capabilities: &DmabufCapabilities,
) -> Option<DmabufGlobal>
where
    D: DmabufHandler + GlobalDispatch<ZwpLinuxDmabufV1, DmabufGlobalData> + 'static,
{
    let formats: Vec<_> = capabilities.formats().iter().copied().collect();
    let main_device = match advertisement(capabilities) {
        Advertisement::None => {
            eventline::warn!(
                "DMA-BUF: renderer reported no importable formats; global not advertised"
            );
            return None;
        }
        Advertisement::Version3 => {
            eventline::debug!(
                "DMA-BUF: render device unavailable; advertising version 3 with {} formats",
                formats.len()
            );
            return Some(state.create_global::<D>(display_handle, formats));
        }
        Advertisement::Version5(main_device) => main_device,
    };

    match DmabufFeedbackBuilder::new(main_device, formats.iter().copied()).build() {
        Ok(feedback) => {
            eventline::debug!(
                "DMA-BUF: advertising version 5 with {} renderer formats",
                formats.len()
            );
            Some(state.create_global_with_default_feedback::<D>(display_handle, &feedback))
        }
        Err(err) => {
            eventline::warn!(
                "DMA-BUF: failed to build version 5 feedback, falling back to version 3: {err}"
            );
            Some(state.create_global::<D>(display_handle, formats))
        }
    }
}

pub fn send_output_feedback(
    wayland: &WaylandState,
    output: &Output,
    primary_output: &Output,
    session_lock: &crate::wayland::session_lock::State,
    feedback: &SurfaceDmabufFeedback,
    element_states: &RenderElementStates,
) {
    if session_lock.active() {
        for surface in session_lock.surfaces_for_output(output) {
            send_dmabuf_feedback_surface_tree(
                surface.wl_surface(),
                output,
                |_, _| Some(output.clone()),
                |surface, _| {
                    select_dmabuf_feedback(
                        surface,
                        element_states,
                        &feedback.render,
                        &feedback.scanout,
                    )
                },
            );
        }
        return;
    }

    wayland
        .space
        .elements()
        .filter(|window| window_is_on_output(window, output, primary_output))
        .for_each(|window| {
            window.send_dmabuf_feedback(
                output,
                |_, _| Some(output.clone()),
                |surface, _| {
                    select_dmabuf_feedback(
                        surface,
                        element_states,
                        &feedback.render,
                        &feedback.scanout,
                    )
                },
            );
        });

    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_dmabuf_feedback(
            output,
            |_, _| Some(output.clone()),
            |surface, _| {
                select_dmabuf_feedback(surface, element_states, &feedback.render, &feedback.scanout)
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use smithay::backend::allocator::{Format, Fourcc, Modifier};

    use super::*;

    fn capabilities(main_device: Option<libc::dev_t>) -> DmabufCapabilities {
        let formats = [Format {
            code: Fourcc::Argb8888,
            modifier: Modifier::Linear,
        }]
        .into_iter()
        .collect();
        DmabufCapabilities::new(main_device, formats)
    }

    #[test]
    fn omits_global_without_renderer_formats() {
        assert_eq!(
            advertisement(&DmabufCapabilities::new(None, Default::default())),
            Advertisement::None
        );
    }

    #[test]
    fn falls_back_to_version_three_without_a_device() {
        assert_eq!(advertisement(&capabilities(None)), Advertisement::Version3);
    }

    #[test]
    fn selects_version_five_when_feedback_has_a_device() {
        assert_eq!(
            advertisement(&capabilities(Some(42))),
            Advertisement::Version5(42)
        );
    }
}
