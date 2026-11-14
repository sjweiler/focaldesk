//! Validated input settings, applied by the compositor and persisted by Rust.
use super::*;
use focaldesk_ipc::{InputConfiguration, InputLayout, InputShortcut};

fn validate_input(input: &InputSettings) -> Result<(), String> {
    if !input.pointer_speed.is_finite() || !(0.25..=2.0).contains(&input.pointer_speed) {
        return Err("Pointer speed must be between 0.25 and 2.0.".into());
    }
    if !(100..=2000).contains(&input.keyboard_repeat_delay_ms)
        || !(1..=100).contains(&input.keyboard_repeat_rate)
    {
        return Err(
            "Repeat delay must be 100–2000 ms and repeat rate 1–100 keys per second.".into(),
        );
    }
    if !matches!(input.workspace_swipe_fingers, 0 | 3 | 4)
        || !input.workspace_swipe_distance.is_finite()
        || !(40.0..=400.0).contains(&input.workspace_swipe_distance)
    {
        return Err("Choose disabled, three-finger, or four-finger swipes with a 40–400 pixel activation distance.".into());
    }
    if input.accessibility != input.accessibility.sanitized() {
        return Err("Keyboard accessibility delays are outside their supported range.".into());
    }
    for (name, value, options) in [
        ("layout", &input.keyboard_layout, false),
        ("variant", &input.keyboard_variant, false),
        ("model", &input.keyboard_model, false),
        ("options", &input.keyboard_options, true),
    ] {
        if value.len() > 256
            || !value.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ',') || (options && c == ':')
            })
        {
            return Err(format!(
                "The keyboard {name} contains unsupported characters."
            ));
        }
    }
    if input.keyboard_layout.is_empty() {
        return Err("Choose a keyboard layout.".into());
    }
    if input.keybindings.len() > 256
        || input
            .keybindings
            .iter()
            .any(|(action, binding)| action.len() > 64 || binding.len() > 128)
    {
        return Err("Too many shortcut overrides or an oversized shortcut.".into());
    }
    // Build the keymap before touching the active keyboard. xkbcommon requires
    // NUL-free names; the grammar above also keeps file paths out of the API.
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(
        &context,
        "",
        &input.keyboard_model,
        &input.keyboard_layout,
        &input.keyboard_variant,
        (!input.keyboard_options.is_empty()).then(|| input.keyboard_options.clone()),
        xkb::COMPILE_NO_FLAGS,
    )
    .ok_or("The keyboard layout, variant, or model could not be compiled.")?;
    Ok(())
}

fn validate_devices(
    previous: &InputSettings,
    requested: &InputSettings,
    devices: &[InputDeviceSnapshot],
) -> Result<Vec<String>, String> {
    if requested.devices.len() > 256 {
        return Err("Too many input device overrides.".into());
    }
    if previous
        .devices
        .keys()
        .any(|id| !requested.devices.contains_key(id))
    {
        return Err("Use the device controls to change overrides; removing device preferences is not supported here.".into());
    }
    let mut changed = Vec::new();
    for (id, settings) in &requested.devices {
        if previous.devices.get(id) == Some(settings) {
            continue;
        }
        let device = devices
            .iter()
            .find(|device| device.id == *id)
            .ok_or("An input device disconnected. Reload the device list before applying.")?;
        let old = previous.devices.get(id).cloned().unwrap_or_default();
        if device.acceleration_profile.as_deref() == Some("custom") {
            return Err("This device uses a custom acceleration curve. Change it in the existing Settings app before editing it here.".into());
        }
        if settings.enabled != old.enabled && !device.can_disable {
            return Err(
                "This device cannot be disabled; keyboard-capable devices must remain available."
                    .into(),
            );
        }
        let unsupported = (settings.tap_to_click.is_some() && device.tap_finger_count == 0)
            || (settings.disable_while_typing.is_some() && !device.supports_disable_while_typing)
            || (settings.acceleration_speed.is_some() && !device.supports_acceleration)
            || (settings.acceleration_profile == InputAccelerationProfile::Adaptive
                && !device.supports_adaptive_acceleration)
            || (settings.acceleration_profile == InputAccelerationProfile::Flat
                && !device.supports_flat_acceleration)
            || (settings.left_handed.is_some() && !device.supports_left_handed)
            || (settings.middle_button_emulation.is_some()
                && !device.supports_middle_button_emulation)
            || match settings.scroll_method {
                InputScrollMethod::TwoFinger => !device.supports_two_finger_scroll,
                InputScrollMethod::Edge => !device.supports_edge_scroll,
                InputScrollMethod::OnButtonDown => !device.supports_button_scroll,
                InputScrollMethod::Disabled => !device.supports_disabled_scroll,
                InputScrollMethod::Default => false,
            };
        if unsupported {
            return Err(format!(
                "{} does not support one of the requested controls.",
                device.name
            ));
        }
        if settings
            .acceleration_speed
            .is_some_and(|speed| !speed.is_finite() || !(-1.0..=1.0).contains(&speed))
        {
            return Err("Device acceleration speed must be between -1 and 1.".into());
        }
        changed.push(id.clone());
    }
    Ok(changed)
}

fn restore_settings(device: &InputDeviceSnapshot) -> InputDeviceSettings {
    InputDeviceSettings {
        enabled: device.can_disable.then_some(device.enabled),
        tap_to_click: (device.tap_finger_count > 0).then_some(device.tap_to_click),
        disable_while_typing: device
            .supports_disable_while_typing
            .then_some(device.disable_while_typing),
        acceleration_profile: match device.acceleration_profile.as_deref() {
            Some("adaptive") => InputAccelerationProfile::Adaptive,
            Some("flat") => InputAccelerationProfile::Flat,
            _ => InputAccelerationProfile::Default,
        },
        acceleration_speed: device
            .supports_acceleration
            .then_some(device.acceleration_speed),
        scroll_method: match device.scroll_method.as_deref() {
            Some("two_finger") => InputScrollMethod::TwoFinger,
            Some("edge") => InputScrollMethod::Edge,
            Some("on_button_down") => InputScrollMethod::OnButtonDown,
            Some("disabled") => InputScrollMethod::Disabled,
            _ => InputScrollMethod::Default,
        },
        left_handed: device.supports_left_handed.then_some(device.left_handed),
        middle_button_emulation: device
            .supports_middle_button_emulation
            .then_some(device.middle_button_emulation),
    }
}

fn keybinds(
    input: &InputSettings,
    capabilities: focaldesk_shortcuts::ShortcutCapabilities,
) -> Result<Keybinds, String> {
    let mut bindings = Keybinds::with_defaults(capabilities);
    let warnings = bindings.apply_overrides(
        input
            .keybindings
            .iter()
            .map(|(action, shortcut)| (action.as_str(), shortcut.as_str())),
    );
    if !warnings.is_empty() {
        return Err(warnings.join("; "));
    }
    Ok(bindings)
}

impl DesktopState {
    pub(super) fn input_configuration(&self) -> InputConfiguration {
        let mut shortcuts = focaldesk_shortcuts::EDITABLE_SHORTCUTS
            .iter()
            .map(|(action, label, default)| InputShortcut {
                action: (*action).into(),
                label: (*label).into(),
                default: (*default).into(),
            })
            .collect::<Vec<_>>();
        for action in self.input_settings.keybindings.keys() {
            if !shortcuts.iter().any(|shortcut| shortcut.action == *action) {
                shortcuts.push(InputShortcut {
                    action: action.clone(),
                    label: action.replace('_', " "),
                    default: String::new(),
                });
            }
        }
        InputConfiguration {
            input: self.input_settings.clone(),
            saved_input: load_settings().input,
            devices: self.input_device_snapshots(),
            shortcuts,
            layouts: [
                ("us", "English (US)"),
                ("gb", "English (UK)"),
                ("de", "German"),
                ("fr", "French"),
                ("es", "Spanish"),
                ("it", "Italian"),
                ("pt", "Portuguese"),
                ("br", "Portuguese (Brazil)"),
                ("se", "Swedish"),
                ("jp", "Japanese"),
            ]
            .into_iter()
            .map(|(id, label)| InputLayout {
                id: id.into(),
                label: label.into(),
            })
            .collect(),
            hardware_devices: self.backend_kind == CompositorBackend::Drm,
        }
    }

    fn configure_keyboard(&mut self, input: &InputSettings) -> Result<(), String> {
        let keyboard = self
            .seat
            .get_keyboard()
            .ok_or("No compositor keyboard is available.")?;
        keyboard
            .set_xkb_config(
                self,
                XkbConfig {
                    layout: &input.keyboard_layout,
                    variant: &input.keyboard_variant,
                    model: &input.keyboard_model,
                    options: (!input.keyboard_options.is_empty())
                        .then(|| input.keyboard_options.clone()),
                    ..Default::default()
                },
            )
            .map_err(|error| format!("Could not apply the keyboard layout: {error}"))
    }

    pub(super) fn set_input_configuration(
        &mut self,
        expected: InputSettings,
        expected_saved: InputSettings,
        input: InputSettings,
    ) -> Result<InputConfiguration, String> {
        if self.input_settings != expected {
            return Err(
                "Active input settings changed in another client. Reload before applying.".into(),
            );
        }
        if load_settings().input != expected_saved {
            return Err(
                "Saved input settings changed in another client. Reload before applying.".into(),
            );
        }
        validate_input(&input)?;
        if input.keyboard_options != expected.keyboard_options
            && !matches!(
                input.keyboard_options.as_str(),
                "" | "ctrl:nocaps" | "ctrl:swap_lalt_lctl"
            )
        {
            return Err("Choose one of the supported modifier presets; existing custom options can be kept unchanged.".into());
        }
        let prepared = keybinds(
            &input,
            crate::backend::common::shortcut_capabilities(self.backend_kind),
        )?;
        let devices = self.input_device_snapshots();
        let changed = validate_devices(&expected, &input, &devices)?;
        self.configure_keyboard(&input)?;
        let apply = (|| {
            for id in &changed {
                let device = self
                    .libinput_devices
                    .get_mut(id)
                    .ok_or("An input device disconnected while applying.")?;
                apply_libinput_device_settings(device, &input.devices[id])?;
            }
            focaldesk_settings_core::save_input_settings(&expected_saved, &input)
                .map_err(|error| error.to_string())
        })();
        if let Err(message) = apply {
            for id in &changed {
                if let (Some(device), Some(previous)) = (
                    self.libinput_devices.get_mut(id),
                    devices.iter().find(|device| device.id == *id),
                ) {
                    if let Err(error) =
                        apply_libinput_device_settings(device, &restore_settings(previous))
                    {
                        flog_warn!("Could not restore input device after rejected update: {error}");
                    }
                }
            }
            if let Err(error) = self.configure_keyboard(&expected) {
                flog_warn!("Could not restore keyboard after rejected input update: {error}");
            }
            return Err(message);
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.change_repeat_info(
                input.keyboard_repeat_rate as i32,
                input.keyboard_repeat_delay_ms as i32,
            );
        }
        let releases = self.keyboard_accessibility.reconfigure(input.accessibility);
        self.keybinds = prepared;
        self.input_settings = input;
        for release in releases {
            self.handle_key_event(release.keycode, release.state);
        }
        self.mark_all_outputs_full_damage(DamageSource::Unknown);
        Ok(self.input_configuration())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_ranges_and_compiles_layout_before_applying() {
        let mut input = focaldesk_settings_core::default_settings().input;
        assert!(validate_input(&input).is_ok());
        input.keyboard_layout = "not_a_real_layout".into();
        assert!(validate_input(&input).is_err());
        input.keyboard_layout = "us\0malformed".into();
        assert!(validate_input(&input).is_err());
        input.keyboard_layout = "us".into();
        input.keyboard_repeat_rate = 0;
        assert!(validate_input(&input).is_err());
        input.keyboard_repeat_rate = 25;
        input.pointer_speed = f32::NAN;
        assert!(validate_input(&input).is_err());
    }
    #[test]
    fn rejects_shortcut_conflicts_alias_collisions_and_invalid_parts() {
        let mut input = focaldesk_settings_core::default_settings().input;
        input
            .keybindings
            .insert("launch_terminal".into(), "Ctrl+Alt+T".into());
        assert!(keybinds(&input, focaldesk_shortcuts::ShortcutCapabilities::DESKTOP).is_ok());
        input
            .keybindings
            .insert("launch_browser".into(), "Control+Alt+t".into());
        assert!(keybinds(&input, focaldesk_shortcuts::ShortcutCapabilities::DESKTOP).is_err());
        input.keybindings.clear();
        input
            .keybindings
            .insert("launch_terminal".into(), "Ctrl++T".into());
        assert!(keybinds(&input, focaldesk_shortcuts::ShortcutCapabilities::DESKTOP).is_err());
        input.keybindings.clear();
        input
            .keybindings
            .insert("run_arbitrary_command".into(), "Ctrl+Alt+T".into());
        assert!(keybinds(&input, focaldesk_shortcuts::ShortcutCapabilities::DESKTOP).is_err());
    }
    #[test]
    fn device_enable_changes_require_capabilities_and_rollback_restores_enabled_state() {
        let mut device = InputDeviceSnapshot {
            id: "pointer".into(),
            name: "Pointer".into(),
            sysname: "event1".into(),
            vendor_id: 0,
            product_id: 0,
            touchpad: false,
            enabled: true,
            can_disable: true,
            tap_finger_count: 0,
            tap_to_click: false,
            supports_disable_while_typing: false,
            disable_while_typing: false,
            supports_acceleration: false,
            supports_adaptive_acceleration: false,
            supports_flat_acceleration: false,
            acceleration_profile: None,
            acceleration_speed: 0.0,
            supports_two_finger_scroll: false,
            supports_edge_scroll: false,
            supports_button_scroll: false,
            supports_disabled_scroll: false,
            scroll_method: None,
            supports_left_handed: false,
            left_handed: false,
            supports_middle_button_emulation: false,
            middle_button_emulation: false,
        };
        let previous = focaldesk_settings_core::default_settings().input;
        let mut requested = previous.clone();
        requested.devices.insert(
            device.id.clone(),
            InputDeviceSettings {
                enabled: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(
            validate_devices(&previous, &requested, &[device.clone()]).unwrap(),
            vec!["pointer"]
        );
        assert_eq!(restore_settings(&device).enabled, Some(true));
        device.can_disable = false;
        assert!(validate_devices(&previous, &requested, &[device.clone()])
            .unwrap_err()
            .contains("keyboard"));
        assert_eq!(restore_settings(&device).enabled, None);
        device.can_disable = true;
        device.enabled = false;
        assert_eq!(restore_settings(&device).enabled, Some(false));
    }
    #[test]
    fn leaves_disconnected_preferences_unchanged_but_rejects_edits_without_capabilities() {
        let mut previous = focaldesk_settings_core::default_settings().input;
        previous
            .devices
            .insert("disconnected".into(), InputDeviceSettings::default());
        assert!(validate_devices(&previous, &previous, &[])
            .unwrap()
            .is_empty());
        let mut requested = previous.clone();
        requested
            .devices
            .get_mut("disconnected")
            .unwrap()
            .tap_to_click = Some(true);
        assert!(validate_devices(&previous, &requested, &[]).is_err());
    }
}
