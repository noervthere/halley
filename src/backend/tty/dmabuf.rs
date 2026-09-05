use std::io;

use smithay::backend::allocator::format::FormatSet;
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::DrmNode;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;

use super::TtyDrmOutput;
use crate::backend::dmabuf::{SurfaceDmabufFeedback, scanout_formats};

pub fn frame_flags() -> FrameFlags {
    // Enable hardware direct scanout across the primary plane (when format and
    // modifiers strictly match) and the hardware cursor plane for zero-latency
    // pointer motion without CPU stalls.
    //
    // Note: We deliberately omit ALLOW_PRIMARY_PLANE_SCANOUT_ANY and
    // ALLOW_OVERLAY_PLANE_SCANOUT:
    // - ALLOW_PRIMARY_PLANE_SCANOUT_ANY bypasses format/modifier matching in Smithay.
    //   On AMD (amdgpu / RADV) and Intel, scanning out a client buffer before
    //   dmabuf scanout feedback negotiation finishes hands non-display DCC
    //   (Delta Color Compression) compressed buffers directly to the KMS display
    //   hardware, causing green / rainbow lines to glitch on screen for a short timeline.
    // - ALLOW_OVERLAY_PLANE_SCANOUT on AMD DC triggers underflow or modifier
    //   tiling corruptions when subsurfaces/popups are assigned to hardware overlay planes.
    FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT | FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT
}

pub fn frame_flags_for_scene(has_framebuffer_effect: bool) -> FrameFlags {
    let mut flags = frame_flags();
    if has_framebuffer_effect {
        // Direct scan-out of a window skips the backdrop blur behind it, so
        // the surface flickers between the client buffer and the composed
        // frosted scene. Disable primary-plane scan-out while any
        // framebuffer effect is in the output list.
        flags.remove(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT);
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
    fn direct_scanout_policy_uses_hardware_planes() {
        let flags = frame_flags();
        assert!(flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT));
        assert!(!flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY));
        assert!(flags.contains(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT));
        assert!(!flags.contains(FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT));
    }

    #[test]
    fn framebuffer_effects_disable_primary_and_overlay_scanout() {
        let flags = frame_flags_for_scene(true);
        assert!(!flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT));
        assert!(!flags.contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY));
        assert!(!flags.contains(FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT));
        assert!(flags.contains(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT));
        assert!(frame_flags_for_scene(false).contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT));
    }
}
