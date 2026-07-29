pub mod animations;
pub mod bootstrap;
pub mod chord;
pub mod cursor;
pub mod decorations;
pub mod input;
pub mod keybinds;
pub mod output;
pub mod parse;
pub mod runtime;
pub mod screenshot;
pub mod terminal;
pub mod zoom;

pub use animations::{
    AnimationCurve, AnimationMotion, Animations, EasingMotion, FullscreenAnimation, SpringMotion,
    WindowCloseAnimation, WindowCloseAnimationType, WindowOpenAnimation, WindowOpenAnimationType,
    load_animations, parse_animations,
};
pub use bootstrap::{
    DEFAULT_CONFIG, bootstrap_default_config, bootstrap_default_config_at, config_path,
};
pub use chord::parse_chord;
pub use cursor::{Cursor, parse_cursor};
pub use decorations::{BorderColor, Decorations, load_decorations, parse_decorations};
pub use input::{
    AccelProfile, ClickMethod, DeviceKind, DeviceOverride, DeviceSettings, FocusMode,
    GestureModifier, GestureScope, GestureSettings, Input, InputParseError, KeyboardConfig,
    MouseSettings, ScrollMethod, ScrollPanMode, TapButtonMap, parse_input,
};
pub use keybinds::{Action, DefaultTerminal, Keybind, Keybinds, ModifierKey, Modifiers};
pub use output::{
    OutputConfig, OutputParseError, Vrr, load_outputs, parse_outputs, parse_outputs_checked,
};
pub use parse::{ParseError, parse_keybinds};
pub use runtime::{
    RuntimeConfig, RuntimeConfigError, load_runtime_config_at, parse_runtime_config,
};
pub use screenshot::{Screenshot, parse_screenshot};
pub use terminal::{
    TERMINAL_PRIORITY, resolve_default_terminal, resolve_default_terminal_from_path,
};
pub use zoom::{Zoom, load_zoom, parse_zoom};
