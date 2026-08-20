/// Backend-independent settings retained by a running compositor session.
///
/// Configuration parsing remains in `halley-config`; this aggregate owns the
/// applied values and their reload lifecycle. Keeping them together prevents
/// the session coordinator from exposing each setting as unrelated mutable
/// state.
pub struct RuntimeSettings {
    pub overlays: halley_config::Overlays,
    pub apogee: halley_config::Apogee,
    pub input: halley_config::Input,
    pub decorations: halley_config::Decorations,
    pub font: halley_config::Font,
    pub effects: halley_config::Effects,
    pub background: halley_config::Background,
    pub field: halley_config::Field,
    pub zoom: halley_config::Zoom,
    pub screenshot: halley_config::Screenshot,
    pub debug: halley_config::Debug,
    pub layer_rules: Vec<halley_config::LayerRule>,
}

impl RuntimeSettings {
    pub fn new(config: &halley_config::RuntimeConfig, applied_input: halley_config::Input) -> Self {
        Self {
            overlays: config.overlays,
            apogee: config.apogee,
            input: applied_input,
            decorations: config.decorations,
            font: config.font.clone(),
            effects: config.effects,
            background: config.background.clone(),
            field: config.field,
            zoom: config.field.zoom,
            screenshot: config.screenshot.clone(),
            layer_rules: config.layer_rules.clone(),
        }
    }

    pub fn visuals_changed(&self, config: &halley_config::RuntimeConfig) -> bool {
        self.decorations != config.decorations
            || self.font != config.font
            || self.effects != config.effects
            || self.background != config.background
            || self.zoom != config.field.zoom
            || self.overlays != config.overlays
            || self.layer_rules != config.layer_rules
    }

    /// Replaces settings that do not need protocol-specific application.
    /// Input is applied separately so a failed XKB reload can retain the last
    /// working keyboard configuration.
    pub fn reload_non_input(&mut self, config: &halley_config::RuntimeConfig) {
        self.apogee = config.apogee;
        self.decorations = config.decorations;
        self.font = config.font.clone();
        self.effects = config.effects;
        self.background = config.background.clone();
        self.field = config.field;
        self.overlays = config.overlays;
        self.zoom = config.field.zoom;
        self.screenshot = config.screenshot.clone();
        self.layer_rules.clone_from(&config.layer_rules);
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeSettings;

    #[test]
    fn visual_change_detection_ignores_non_visual_settings() {
        let config = halley_config::RuntimeConfig::default();
        let mut settings = RuntimeSettings::new(&config, config.input.clone());
        let mut changed = config.clone();
        changed.screenshot.directory = "different".into();

        assert!(!settings.visuals_changed(&changed));

        changed.decorations.border_width_px += 1;
        assert!(settings.visuals_changed(&changed));

        settings.reload_non_input(&changed);
        assert!(!settings.visuals_changed(&changed));
    }
}
