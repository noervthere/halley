use smithay::backend::allocator::format::FormatSet;

/// Renderer facts needed to advertise one linux-dmabuf global.
///
/// Device discovery belongs to the concrete backend; protocol version and
/// fallback policy are decided by the shared Wayland layer.
#[derive(Clone, Debug)]
pub struct DmabufCapabilities {
    main_device: Option<libc::dev_t>,
    formats: FormatSet,
}

impl DmabufCapabilities {
    pub fn new(main_device: Option<libc::dev_t>, formats: FormatSet) -> Self {
        Self {
            main_device,
            formats,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.formats.iter().next().is_none()
    }

    pub fn main_device(&self) -> Option<libc::dev_t> {
        self.main_device
    }

    pub fn formats(&self) -> &FormatSet {
        &self.formats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_capabilities_are_detectable_without_device_information() {
        assert!(DmabufCapabilities::new(None, FormatSet::default()).is_empty());
    }
}
