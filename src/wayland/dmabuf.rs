use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use smithay::reexports::wayland_server::{DisplayHandle, GlobalDispatch};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufGlobalData, DmabufHandler, DmabufState,
};

use crate::backend::dmabuf::DmabufCapabilities;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Advertisement {
    None,
    Version3,
    Version4(libc::dev_t),
}

fn advertisement(capabilities: &DmabufCapabilities) -> Advertisement {
    if capabilities.is_empty() {
        Advertisement::None
    } else if let Some(main_device) = capabilities.main_device() {
        Advertisement::Version4(main_device)
    } else {
        Advertisement::Version3
    }
}

/// Advertises the strongest truthful linux-dmabuf global for one renderer.
///
/// Version 4 needs a device-backed feedback table. Renderers whose EGL
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
        Advertisement::Version4(main_device) => main_device,
    };

    match DmabufFeedbackBuilder::new(main_device, formats.iter().copied()).build() {
        Ok(feedback) => {
            eventline::debug!(
                "DMA-BUF: advertising version 4 with {} renderer formats",
                formats.len()
            );
            Some(state.create_global_with_default_feedback::<D>(display_handle, &feedback))
        }
        Err(err) => {
            eventline::warn!(
                "DMA-BUF: failed to build version 4 feedback, falling back to version 3: {err}"
            );
            Some(state.create_global::<D>(display_handle, formats))
        }
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
    fn selects_version_four_when_feedback_has_a_device() {
        assert_eq!(
            advertisement(&capabilities(Some(42))),
            Advertisement::Version4(42)
        );
    }
}
