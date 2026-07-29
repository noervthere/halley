use smithay::reexports::input::{
    AccelProfile, Device, DeviceCapability, DeviceConfigError, DeviceConfigResult, ScrollMethod,
    SendEventsMode,
};

#[derive(Default)]
pub struct PhysicalMouseDevices {
    devices: Vec<Device>,
}

impl PhysicalMouseDevices {
    pub fn added(&mut self, mut device: Device, input: &halley_config::Input) {
        if !is_ordinary_mouse(&device) {
            return;
        }
        apply(&mut device, input);
        eventline::debug!("input: managing mouse {:?}", device.name());
        self.devices.push(device);
    }

    pub fn removed(&mut self, device: &Device) {
        self.devices.retain(|candidate| candidate != device);
    }

    pub fn reload(&mut self, input: &halley_config::Input) {
        for device in &mut self.devices {
            apply(device, input);
        }
    }
}

fn apply(device: &mut Device, input: &halley_config::Input) {
    let configured = input.settings_for_device(&device.name());
    let resolved = ResolvedSettings::from_device(device, &configured);
    let name = device.name().into_owned();

    apply_result(
        &name,
        "enabled",
        device.config_send_events_set_mode(resolved.send_events),
    );
    apply_result(
        &name,
        "natural-scroll",
        device.config_scroll_set_natural_scroll_enabled(resolved.natural_scroll),
    );
    apply_result(
        &name,
        "accel-speed",
        device.config_accel_set_speed(resolved.accel_speed),
    );
    if let Some(profile) = resolved.accel_profile {
        apply_result(
            &name,
            "accel-profile",
            device.config_accel_set_profile(profile),
        );
    }
    if let Some(method) = resolved.scroll_method {
        apply_result(
            &name,
            "scroll-method",
            device.config_scroll_set_method(method),
        );
    }
    apply_result(
        &name,
        "scroll-button",
        device.config_scroll_set_button(resolved.scroll_button),
    );
    apply_result(
        &name,
        "left-handed",
        device.config_left_handed_set(resolved.left_handed),
    );
    apply_result(
        &name,
        "middle-emulation",
        device.config_middle_emulation_set_enabled(resolved.middle_emulation),
    );
}

fn apply_result(name: &str, setting: &str, result: DeviceConfigResult) {
    match result {
        Ok(()) | Err(DeviceConfigError::Unsupported) => {}
        Err(DeviceConfigError::Invalid) => {
            eventline::warn!("input: mouse {name:?} rejected {setting}")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DeviceDefaults {
    natural_scroll: bool,
    accel_speed: f64,
    accel_profile: Option<AccelProfile>,
    scroll_method: Option<ScrollMethod>,
    scroll_button: u32,
    left_handed: bool,
    middle_emulation: bool,
}

impl DeviceDefaults {
    fn read(device: &Device) -> Self {
        Self {
            natural_scroll: device.config_scroll_default_natural_scroll_enabled(),
            accel_speed: device.config_accel_default_speed(),
            accel_profile: device.config_accel_default_profile(),
            scroll_method: device.config_scroll_default_method(),
            scroll_button: device.config_scroll_default_button(),
            left_handed: device.config_left_handed_default(),
            middle_emulation: device.config_middle_emulation_default_enabled(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedSettings {
    send_events: SendEventsMode,
    natural_scroll: bool,
    accel_speed: f64,
    accel_profile: Option<AccelProfile>,
    scroll_method: Option<ScrollMethod>,
    scroll_button: u32,
    left_handed: bool,
    middle_emulation: bool,
}

impl ResolvedSettings {
    fn from_device(device: &Device, configured: &halley_config::MouseSettings) -> Self {
        Self::resolve(DeviceDefaults::read(device), configured)
    }

    fn resolve(defaults: DeviceDefaults, configured: &halley_config::MouseSettings) -> Self {
        Self {
            send_events: if configured.enabled.unwrap_or(true) {
                SendEventsMode::ENABLED
            } else {
                SendEventsMode::DISABLED
            },
            natural_scroll: configured.natural_scroll.unwrap_or(defaults.natural_scroll),
            accel_speed: configured.accel_speed.unwrap_or(defaults.accel_speed),
            accel_profile: configured
                .accel_profile
                .map(map_accel_profile)
                .or(defaults.accel_profile),
            scroll_method: configured
                .scroll_method
                .map(map_scroll_method)
                .or(defaults.scroll_method),
            scroll_button: configured.scroll_button.unwrap_or(defaults.scroll_button),
            left_handed: configured.left_handed.unwrap_or(defaults.left_handed),
            middle_emulation: configured
                .middle_emulation
                .unwrap_or(defaults.middle_emulation),
        }
    }
}

fn map_accel_profile(profile: halley_config::AccelProfile) -> AccelProfile {
    match profile {
        halley_config::AccelProfile::Adaptive => AccelProfile::Adaptive,
        halley_config::AccelProfile::Flat => AccelProfile::Flat,
    }
}

fn map_scroll_method(method: halley_config::ScrollMethod) -> ScrollMethod {
    match method {
        halley_config::ScrollMethod::None => ScrollMethod::NoScroll,
        halley_config::ScrollMethod::TwoFinger => ScrollMethod::TwoFinger,
        halley_config::ScrollMethod::Edge => ScrollMethod::Edge,
        halley_config::ScrollMethod::OnButtonDown => ScrollMethod::OnButtonDown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeviceFacts {
    pointer: bool,
    touch: bool,
    tablet_tool: bool,
    tablet_pad: bool,
    tap_fingers: u32,
    trackball: bool,
    trackpoint: bool,
}

fn facts_are_ordinary_mouse(facts: DeviceFacts) -> bool {
    facts.pointer
        && !facts.touch
        && !facts.tablet_tool
        && !facts.tablet_pad
        && facts.tap_fingers == 0
        && !facts.trackball
        && !facts.trackpoint
}

fn is_ordinary_mouse(device: &Device) -> bool {
    let (trackball, trackpoint) = unsafe { device.udev_device() }.map_or((false, false), |udev| {
        (
            udev.property_value("ID_INPUT_TRACKBALL").is_some(),
            udev.property_value("ID_INPUT_POINTINGSTICK").is_some(),
        )
    });
    facts_are_ordinary_mouse(DeviceFacts {
        pointer: device.has_capability(DeviceCapability::Pointer),
        touch: device.has_capability(DeviceCapability::Touch),
        tablet_tool: device.has_capability(DeviceCapability::TabletTool),
        tablet_pad: device.has_capability(DeviceCapability::TabletPad),
        tap_fingers: device.config_tap_finger_count(),
        trackball,
        trackpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_facts() -> DeviceFacts {
        DeviceFacts {
            pointer: true,
            touch: false,
            tablet_tool: false,
            tablet_pad: false,
            tap_fingers: 0,
            trackball: false,
            trackpoint: false,
        }
    }

    #[test]
    fn classifies_only_ordinary_mice() {
        assert!(facts_are_ordinary_mouse(mouse_facts()));
        for facts in [
            DeviceFacts {
                pointer: false,
                ..mouse_facts()
            },
            DeviceFacts {
                tap_fingers: 2,
                ..mouse_facts()
            },
            DeviceFacts {
                trackball: true,
                ..mouse_facts()
            },
            DeviceFacts {
                trackpoint: true,
                ..mouse_facts()
            },
            DeviceFacts {
                touch: true,
                ..mouse_facts()
            },
            DeviceFacts {
                tablet_tool: true,
                ..mouse_facts()
            },
            DeviceFacts {
                tablet_pad: true,
                ..mouse_facts()
            },
        ] {
            assert!(!facts_are_ordinary_mouse(facts));
        }
    }

    #[test]
    fn omitted_values_restore_every_device_default() {
        let defaults = DeviceDefaults {
            natural_scroll: true,
            accel_speed: -0.3,
            accel_profile: Some(AccelProfile::Adaptive),
            scroll_method: Some(ScrollMethod::OnButtonDown),
            scroll_button: 276,
            left_handed: true,
            middle_emulation: true,
        };

        assert_eq!(
            ResolvedSettings::resolve(defaults, &halley_config::MouseSettings::default()),
            ResolvedSettings {
                send_events: SendEventsMode::ENABLED,
                natural_scroll: defaults.natural_scroll,
                accel_speed: defaults.accel_speed,
                accel_profile: defaults.accel_profile,
                scroll_method: defaults.scroll_method,
                scroll_button: defaults.scroll_button,
                left_handed: defaults.left_handed,
                middle_emulation: defaults.middle_emulation,
            }
        );
    }

    #[test]
    fn configured_values_override_each_device_default() {
        let defaults = DeviceDefaults {
            natural_scroll: false,
            accel_speed: 0.0,
            accel_profile: Some(AccelProfile::Adaptive),
            scroll_method: Some(ScrollMethod::NoScroll),
            scroll_button: 0,
            left_handed: false,
            middle_emulation: false,
        };
        let configured = halley_config::MouseSettings {
            enabled: Some(false),
            natural_scroll: Some(true),
            accel_speed: Some(0.5),
            accel_profile: Some(halley_config::AccelProfile::Flat),
            scroll_method: Some(halley_config::ScrollMethod::OnButtonDown),
            scroll_button: Some(274),
            left_handed: Some(true),
            middle_emulation: Some(true),
        };

        assert_eq!(
            ResolvedSettings::resolve(defaults, &configured),
            ResolvedSettings {
                send_events: SendEventsMode::DISABLED,
                natural_scroll: true,
                accel_speed: 0.5,
                accel_profile: Some(AccelProfile::Flat),
                scroll_method: Some(ScrollMethod::OnButtonDown),
                scroll_button: 274,
                left_handed: true,
                middle_emulation: true,
            }
        );
    }
}
