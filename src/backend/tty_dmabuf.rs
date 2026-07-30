use std::io;

use smithay::backend::allocator::format::FormatSet;
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::DrmNode;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;

use super::dmabuf::{SurfaceDmabufFeedback, scanout_formats};
use super::tty::TtyDrmOutput;

pub fn frame_flags(vrr_active: bool) -> FrameFlags {
    let mut flags = FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT;
    if !vrr_active {
        flags |= FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY;
    }
    flags
}

pub fn surface_feedback(
    output: &TtyDrmOutput,
    renderer_formats: FormatSet,
    render_node: DrmNode,
    scanout_node: DrmNode,
) -> Result<SurfaceDmabufFeedback, io::Error> {
    let primary_plane_formats =
        output.with_compositor(|compositor| compositor.surface().plane_info().formats.clone());
    let primary_scanout_formats = scanout_formats(&renderer_formats, &primary_plane_formats);
    let builder = DmabufFeedbackBuilder::new(render_node.dev_id(), renderer_formats);
    let scanout = builder
        .clone()
        .add_preference_tranche(
            scanout_node.dev_id(),
            Some(TrancheFlags::Scanout),
            primary_scanout_formats,
        )
        .build()?;

    // Both nodes belong to the same GPU in this backend, so scan-out-friendly
    // allocations are also the preferred render allocations.
    Ok(SurfaceDmabufFeedback {
        render: scanout.clone(),
        scanout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_refresh_allows_primary_and_cursor_plane_scanout() {
        let flags = frame_flags(false);
        assert!(flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY));
        assert!(flags.contains(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT));
        assert!(!flags.contains(FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT));
    }

    #[test]
    fn vrr_excludes_primary_but_keeps_cursor_plane_scanout() {
        let flags = frame_flags(true);
        assert!(!flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY));
        assert!(flags.contains(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT));
        assert!(!flags.contains(FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT));
    }
}
