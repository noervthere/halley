use crate::ModifierKey;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GestureScope {
    #[default]
    EmptyField,
    Global,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollPanMode {
    Off,
    #[default]
    EmptyField,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GestureModifier {
    #[default]
    Keybind,
    Disabled,
    Explicit(ModifierKey),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GestureSettings {
    pub enabled: bool,
    pub client_passthrough: bool,
    pub touch_passthrough: bool,
    pub pinch_to_zoom: bool,
    pub pinch_scope: GestureScope,
    pub compositor_scope: GestureScope,
    pub modifier: GestureModifier,
    pub scroll_pan: ScrollPanMode,
    pub pan_fingers: u32,
    pub pan_momentum: bool,
    pub pan_decay_rate: f32,
    pub flick_min_px_per_s: f32,
    pub swipe_threshold_px: f32,
    pub apogee_open_fingers: u32,
    pub apogee_close_fingers: u32,
}

impl Default for GestureSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            client_passthrough: true,
            touch_passthrough: true,
            pinch_to_zoom: true,
            pinch_scope: GestureScope::EmptyField,
            compositor_scope: GestureScope::EmptyField,
            modifier: GestureModifier::Keybind,
            scroll_pan: ScrollPanMode::EmptyField,
            pan_fingers: 3,
            pan_momentum: true,
            pan_decay_rate: 6.0,
            flick_min_px_per_s: 200.0,
            swipe_threshold_px: 120.0,
            apogee_open_fingers: 4,
            apogee_close_fingers: 4,
        }
    }
}
