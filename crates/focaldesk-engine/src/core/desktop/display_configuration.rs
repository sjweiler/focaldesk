//! Compositor-owned display trials. No frontend lifetime or timer is trusted.
use super::*;
use focaldesk_ipc::{
    DisplayConfiguration, DisplayMode, DisplayOutput, DisplayPreviewPhase, DisplayPreviewStatus,
};

const TRIAL_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) struct DisplayTrial {
    id: u64,
    previous: Vec<OutputConfig>,
    requested: Vec<OutputConfig>,
    persisted: Vec<OutputConfig>,
    deadline: Instant,
    phase: DisplayPreviewPhase,
    backend_pending: bool,
    retry_after: Option<Instant>,
    icc_versions: Vec<(String, [u8; 16])>,
}

impl DisplayTrial {
    fn target(&self) -> &[OutputConfig] {
        if self.phase == DisplayPreviewPhase::Reverting {
            &self.previous
        } else {
            &self.requested
        }
    }
    fn status(&self, now: Instant) -> DisplayPreviewStatus {
        DisplayPreviewStatus {
            id: self.id,
            phase: self.phase,
            remaining_ms: self.deadline.saturating_duration_since(now).as_millis() as u64,
        }
    }
    fn confirmable(&self, id: u64, now: Instant) -> Result<(), String> {
        if self.id != id {
            return Err("This display trial is no longer current.".into());
        }
        if self.phase != DisplayPreviewPhase::Confirming
            || self.backend_pending
            || now >= self.deadline
        {
            return Err("The display trial is not ready to confirm or has expired.".into());
        }
        Ok(())
    }
}

fn validate_icc_path(path: &str) -> Result<[u8; 16], String> {
    if !std::path::Path::new(path).is_absolute()
        || path.len() > 4096
        || path.chars().any(char::is_control)
    {
        return Err("Choose an absolute local ICC profile path.".into());
    }
    let profile = crate::core::icc::load_display_profile_from_path(std::path::Path::new(path))
        .map_err(|error| format!("Invalid or unavailable RGB display ICC profile: {error:?}"))?;
    Ok(md5::compute(&profile.bytes).0)
}

pub(super) fn validate_trial(
    current: &[DisplayOutput],
    expected: &[OutputConfig],
    requested: &[OutputConfig],
    hardware: bool,
) -> Result<(), String> {
    let actual = current
        .iter()
        .map(|output| output.config.clone())
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(
            "Displays changed. Reload their current configuration before trying again.".into(),
        );
    }
    if current.is_empty() || current.len() > 16 || requested.len() != current.len() {
        return Err("The trial must include every connected display (up to 16).".into());
    }
    let mut seen = HashSet::new();
    let mut primary = 0;
    for output in requested {
        if !seen.insert(&output.connector) {
            return Err("A display occurs more than once.".into());
        }
        let original = current
            .iter()
            .find(|current| current.config.connector == output.connector)
            .ok_or_else(|| format!("Unknown display: {}", output.connector))?;
        if !hardware && output.enabled != original.config.enabled {
            return Err("Output enable/disable requires a DRM desktop session.".into());
        }
        if !output.enabled && output.primary {
            return Err("A disabled display cannot be primary.".into());
        }
        if !output.scale.is_finite()
            || !(1.0..=4.0).contains(&output.scale)
            || !(-32768..=32768).contains(&output.x)
            || !(-32768..=32768).contains(&output.y)
        {
            return Err("Display scale or position is outside the supported range.".into());
        }
        let mode = DisplayMode {
            width: output.width,
            height: output.height,
            refresh_mhz: output.refresh_mhz,
        };
        if !original.modes.contains(&mode) {
            return Err(format!(
                "The selected mode is not advertised by {}.",
                output.connector
            ));
        }
        if output.enabled && output.hdr_requested && !original.hdr_supported {
            return Err(format!(
                "{} does not advertise HDR support.",
                output.connector
            ));
        }
        if output.hdr_enabled {
            return Err(
                "HDR active state is reported by the compositor, not requested by Settings.".into(),
            );
        }
        if output.hdr_appearance != original.config.hdr_appearance {
            output.hdr_appearance.validate().map_err(str::to_string)?;
            if !hardware || !output.enabled || !output.hdr_requested || !original.hdr_supported {
                return Err(
                    "Enable HDR on this display before testing HDR appearance changes.".into(),
                );
            }
        }
        if output.icc_profile_path != original.config.icc_profile_path {
            if !output.enabled {
                return Err("Enable the display before testing an ICC change.".into());
            }
            if let Some(path) = &output.icc_profile_path {
                validate_icc_path(path)?;
            }
        }
        if !hardware
            && (output.width != original.config.width
                || output.height != original.config.height
                || output.refresh_mhz != original.config.refresh_mhz
                || output.hdr_requested != original.config.hdr_requested)
        {
            return Err(
                "Hardware modes and HDR must be configured in a DRM desktop session.".into(),
            );
        }
        if output.primary {
            if output.role != OutputRole::Normal {
                return Err("The primary display must be an application display.".into());
            }
            primary += 1;
        }
    }
    if primary != 1 {
        return Err("Choose exactly one primary display.".into());
    }
    // A disconnected arrangement can strand the pointer between screens. Require
    // every application display to touch or overlap the primary-connected group.
    let rect = |output: &OutputConfig| {
        let rotated = matches!(
            output.transform,
            DisplayTransform::Rotate90 | DisplayTransform::Rotate270
        );
        let (w, h) = if rotated {
            (output.height, output.width)
        } else {
            (output.width, output.height)
        };
        (
            i64::from(output.x),
            i64::from(output.y),
            (f64::from(w) / f64::from(output.scale)).round() as i64,
            (f64::from(h) / f64::from(output.scale)).round() as i64,
        )
    };
    let mut connected = vec![requested.iter().position(|output| output.primary).unwrap()];
    loop {
        let next = (0..requested.len()).find(|index| {
            requested[*index].enabled
                && !connected.contains(index)
                && connected.iter().any(|other| {
                    let (ax, ay, aw, ah) = rect(&requested[*index]);
                    let (bx, by, bw, bh) = rect(&requested[*other]);
                    ax <= bx + bw && bx <= ax + aw && ay <= by + bh && by <= ay + ah
                })
        });
        match next {
            Some(index) => connected.push(index),
            None => break,
        }
    }
    if connected.len() != requested.iter().filter(|output| output.enabled).count() {
        return Err("Arrange displays so their edges touch, without gaps.".into());
    }
    Ok(())
}

impl DesktopState {
    pub(super) fn display_configuration(&self) -> DisplayConfiguration {
        // Mode inventory is produced by the DRM backend. Read only bounded,
        // connector-specific capabilities; all current values come from live state.
        let inventory_path =
            focaldesk_settings_core::settings_path().with_file_name("displays.json");
        let inventory = std::fs::metadata(&inventory_path)
            .ok()
            .filter(|meta| meta.len() <= 1024 * 1024)
            .and_then(|_| std::fs::read(inventory_path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let mut outputs = self
            .outputs
            .iter()
            .filter_map(|(id, output)| {
                let mode = output.handle.current_mode()?;
                let connector = output.handle.name();
                let current = DisplayMode {
                    width: mode.size.w,
                    height: mode.size.h,
                    refresh_mhz: mode.refresh,
                };
                let mut modes = output
                    .handle
                    .modes()
                    .into_iter()
                    .map(|mode| DisplayMode {
                        width: mode.size.w,
                        height: mode.size.h,
                        refresh_mhz: mode.refresh,
                    })
                    .collect::<Vec<_>>();
                if self.backend_kind == CompositorBackend::Drm {
                    if let Some(values) = inventory
                        .as_ref()
                        .and_then(|value| value.as_array())
                        .and_then(|values| {
                            values
                                .iter()
                                .find(|value| value["name"].as_str() == Some(connector.as_str()))
                        })
                        .and_then(|saved| saved["available_modes"].as_array())
                    {
                        modes.extend(values.iter().take(256).filter_map(|value| {
                            serde_json::from_value::<DisplayMode>(value.clone()).ok()
                        }));
                    }
                }
                modes.push(current.clone());
                modes.retain(|mode| mode.width > 0 && mode.height > 0 && mode.refresh_mhz > 0);
                modes.sort_by_key(|mode| (mode.width, mode.height, mode.refresh_mhz));
                modes.dedup();
                let transform = match output.handle.current_transform() {
                    Transform::_90 => DisplayTransform::Rotate90,
                    Transform::_180 => DisplayTransform::Rotate180,
                    Transform::_270 => DisplayTransform::Rotate270,
                    _ => DisplayTransform::Normal,
                };
                Some(DisplayOutput {
                    config: OutputConfig {
                        connector,
                        enabled: true,
                        x: output.logical_origin.x,
                        y: output.logical_origin.y,
                        width: current.width,
                        height: current.height,
                        refresh_mhz: current.refresh_mhz,
                        scale: output.scale_factor as f32,
                        transform,
                        role: output.role,
                        primary: *id == self.primary_output,
                        color_profile: output.color_profile_override,
                        icc_profile_path: output.icc_profile_path.clone(),
                        hdr_requested: output.hdr_requested,
                        hdr_enabled: false,
                        hdr_appearance: output.hdr_appearance,
                    },
                    make: output.monitor_make.clone(),
                    model: output.monitor_model.clone(),
                    modes,
                    hdr_supported: output.hdr_supported,
                    hdr_active: output.hdr_enabled,
                    wide_gamut_active: output.color_description.primaries
                        != crate::core::color::default_output_color_description().primaries,
                    icc_lut_fallback_active: output.icc_lut_fallback_active,
                })
            })
            .collect::<Vec<_>>();
        // Connected but disabled connectors have no wl_output. Backend-probed
        // capability inventory keeps them selectable without trusting stale JSON.
        let persisted = load_settings().displays.outputs;
        for inactive in self.connected_drm_outputs.values().flatten() {
            if outputs
                .iter()
                .any(|output| output.config.connector == inactive.config.connector)
            {
                continue;
            }
            let mut inactive = inactive.clone();
            if let Some(config) = self
                .trial_display_outputs()
                .unwrap_or_default()
                .iter()
                .find(|config| config.connector == inactive.config.connector)
            {
                inactive.config = config.clone();
            } else if let Some(config) = persisted
                .iter()
                .find(|config| config.connector == inactive.config.connector)
            {
                let monitor_icc = inactive.config.icc_profile_path.clone();
                let monitor_hdr = inactive.config.hdr_requested;
                let monitor_appearance = inactive.config.hdr_appearance;
                inactive.config = config.clone();
                inactive.config.icc_profile_path = monitor_icc;
                inactive.config.hdr_requested = monitor_hdr;
                inactive.config.hdr_appearance = monitor_appearance;
            }
            inactive.config.enabled = false;
            inactive.config.primary = false;
            inactive.config.hdr_enabled = false;
            if !inactive.modes.iter().any(|mode| {
                mode.width == inactive.config.width
                    && mode.height == inactive.config.height
                    && mode.refresh_mhz == inactive.config.refresh_mhz
            }) {
                if let Some(mode) = inactive.modes.first() {
                    inactive.config.width = mode.width;
                    inactive.config.height = mode.height;
                    inactive.config.refresh_mhz = mode.refresh_mhz;
                }
            }
            outputs.push(inactive);
        }
        outputs.sort_by(|left, right| left.config.connector.cmp(&right.config.connector));
        DisplayConfiguration {
            outputs,
            preview: self
                .display_trial
                .as_ref()
                .map(|trial| trial.status(Instant::now())),
            hardware_modes: self.backend_kind == CompositorBackend::Drm,
        }
    }

    pub(super) fn preview_displays(
        &mut self,
        expected: Vec<OutputConfig>,
        requested: Vec<OutputConfig>,
    ) -> Result<DisplayConfiguration, String> {
        self.process_display_trial_timer();
        if self.display_trial.is_some() {
            return Err("Confirm or revert the current display trial first.".into());
        }
        let current = self.display_configuration();
        validate_trial(
            &current.outputs,
            &expected,
            &requested,
            current.hardware_modes,
        )?;
        self.display_trial_sequence = self
            .display_trial_sequence
            .checked_add(1)
            .ok_or("Display trial sequence exhausted.")?;
        self.display_trial = Some(DisplayTrial {
            id: self.display_trial_sequence,
            previous: expected,
            requested: requested.clone(),
            persisted: load_settings().displays.outputs,
            deadline: Instant::now() + TRIAL_TIMEOUT,
            phase: DisplayPreviewPhase::Applying,
            backend_pending: current.hardware_modes,
            retry_after: None,
            icc_versions: requested
                .iter()
                .filter(|config| {
                    current.outputs.iter().any(|output| {
                        output.config.connector == config.connector
                            && output.config.icc_profile_path != config.icc_profile_path
                    })
                })
                .filter_map(|config| config.icc_profile_path.as_ref())
                .map(|path| validate_icc_path(path).map(|version| (path.clone(), version)))
                .collect::<Result<Vec<_>, _>>()?,
        });
        if current.hardware_modes {
            self.display_reconfigure_requested = true;
        } else if let Err(message) = self.apply_display_configs(requested) {
            self.revert_display_trial();
            return Err(message);
        }
        self.process_display_trial_timer();
        Ok(self.display_configuration())
    }

    pub(super) fn confirm_displays(&mut self, id: u64) -> Result<DisplayConfiguration, String> {
        self.process_display_trial_timer();
        let trial = self
            .display_trial
            .as_ref()
            .ok_or("No display trial is active.")?;
        trial.confirmable(id, Instant::now())?;
        if load_settings().displays.outputs != trial.persisted {
            self.revert_display_trial();
            return Err(
                "Saved display settings changed in another client; restoring the previous layout."
                    .into(),
            );
        }
        if trial
            .icc_versions
            .iter()
            .any(|(path, version)| validate_icc_path(path) != Ok(*version))
        {
            self.revert_display_trial();
            return Err("The ICC profile changed or became unavailable during the trial; restoring the previous configuration.".into());
        }
        if let Err(message) = trial.confirmable(id, Instant::now()) {
            self.revert_display_trial();
            return Err(message);
        }
        if let Err(error) = focaldesk_settings_core::save_display_outputs(&trial.requested) {
            self.revert_display_trial();
            return Err(format!(
                "Could not save confirmed displays; restoring previous layout: {error}"
            ));
        }
        self.clear_display_calibration_patterns();
        self.display_trial = None;
        Ok(self.display_configuration())
    }

    pub(super) fn revert_displays(&mut self, id: u64) -> Result<DisplayConfiguration, String> {
        if self
            .display_trial
            .as_ref()
            .is_none_or(|trial| trial.id != id)
        {
            return Err("This display trial is no longer current.".into());
        }
        self.revert_display_trial();
        Ok(self.display_configuration())
    }

    pub(super) fn revert_display_trial(&mut self) {
        self.clear_display_calibration_patterns();
        let Some(trial) = self.display_trial.as_mut() else {
            return;
        };
        if trial.phase == DisplayPreviewPhase::Reverting {
            return;
        }
        trial.phase = DisplayPreviewPhase::Reverting;
        trial.backend_pending = self.backend_kind == CompositorBackend::Drm;
        let previous = trial.previous.clone();
        if self.backend_kind == CompositorBackend::Drm {
            self.display_reconfigure_requested = true;
        } else if let Err(error) = self.apply_display_configs(previous) {
            flog_warn!("Display rollback failed: {error}");
        }
        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    pub(crate) fn process_display_trial_timer(&mut self) {
        let Some(trial) = self.display_trial.as_ref() else {
            return;
        };
        let phase = trial.phase;
        if phase == DisplayPreviewPhase::Reverting
            && trial
                .retry_after
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            if let Some(trial) = self.display_trial.as_mut() {
                trial.retry_after = None;
            }
            self.display_reconfigure_requested = true;
            return;
        }
        if phase != DisplayPreviewPhase::Reverting && Instant::now() >= trial.deadline {
            flog_warn!("Display trial expired; restoring previous display configuration");
            self.revert_display_trial();
            return;
        }
        if trial.backend_pending {
            return;
        }
        let target = trial.target().to_vec();
        let live = self.display_configuration();
        let topology_matches = live.outputs.len() == target.len()
            && live.outputs.iter().all(|output| {
                target
                    .iter()
                    .any(|config| config.connector == output.config.connector)
            });
        if phase != DisplayPreviewPhase::Reverting && !topology_matches {
            self.revert_display_trial();
            return;
        }
        let matches = live
            .outputs
            .iter()
            .any(|output| output.config.enabled && output.config.role == OutputRole::Normal)
            && live.outputs.iter().all(|output| {
                let Some(config) = target
                    .iter()
                    .find(|config| config.connector == output.config.connector)
                else {
                    return phase == DisplayPreviewPhase::Reverting;
                };
                output.config.enabled == config.enabled
                    && output.config.role == config.role
                    && (!config.enabled
                        || (output.config.x == config.x
                            && output.config.y == config.y
                            && output.config.width == config.width
                            && output.config.height == config.height
                            && (i64::from(output.config.refresh_mhz)
                                - i64::from(config.refresh_mhz))
                            .abs()
                                <= 1_000
                            && (output.config.scale - config.scale).abs() < 0.001
                            && output.config.transform == config.transform
                            && output.config.color_profile == config.color_profile
                            && output.config.icc_profile_path == config.icc_profile_path
                            && output.config.hdr_appearance == config.hdr_appearance
                            && output.config.hdr_requested == config.hdr_requested
                            && (phase == DisplayPreviewPhase::Reverting
                                || output.config.primary == config.primary)
                            && (phase == DisplayPreviewPhase::Reverting
                                || !config.hdr_requested
                                || output.hdr_active)))
            });
        if !matches && phase == DisplayPreviewPhase::Confirming {
            self.revert_display_trial();
            return;
        }
        if matches {
            if phase == DisplayPreviewPhase::Reverting {
                self.display_trial = None;
                self.display_reconfigure_requested = false;
            } else if let Some(trial) = self.display_trial.as_mut() {
                trial.phase = DisplayPreviewPhase::Confirming;
            }
        }
    }

    fn clear_display_calibration_patterns(&mut self) {
        for output in self.outputs.values_mut() {
            output.hdr_calibration_pattern = HdrCalibrationPattern::Off;
        }
        self.mark_all_outputs_full_damage(DamageSource::Unknown);
    }

    pub(super) fn display_trial_calibration_pattern(
        &mut self,
        id: u64,
        connector: &str,
        pattern: HdrCalibrationPattern,
    ) -> Result<(), String> {
        self.process_display_trial_timer();
        let trial = self
            .display_trial
            .as_ref()
            .ok_or("Start a display trial before showing calibration patterns.")?;
        trial.confirmable(id, Instant::now())?;
        if !trial
            .requested
            .iter()
            .any(|output| output.connector == connector && output.enabled && output.hdr_requested)
        {
            return Err("Choose an enabled HDR display in this trial.".into());
        }
        self.set_hdr_calibration_pattern(connector, pattern)
    }

    /// Backends use this transient override during both trial and rollback.
    pub(crate) fn trial_display_outputs(&self) -> Option<&[OutputConfig]> {
        self.display_trial.as_ref().map(DisplayTrial::target)
    }
    #[cfg(feature = "drm")]
    pub(crate) fn display_trial_reconfigured(&mut self, success: bool) {
        if let Some(trial) = self.display_trial.as_mut() {
            if success {
                trial.backend_pending = false;
            } else if trial.phase != DisplayPreviewPhase::Reverting {
                self.revert_display_trial();
            } else {
                // A transient connector/driver failure must not leave rollback
                // permanently pending, or trigger rebuilds on every frame.
                trial.backend_pending = true;
                trial.retry_after = Some(Instant::now() + Duration::from_millis(500));
            }
        }
        self.process_display_trial_timer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn display(connector: &str, x: i32, primary: bool) -> DisplayOutput {
        DisplayOutput {
            config: OutputConfig {
                connector: connector.into(),
                enabled: true,
                x,
                y: 0,
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                scale: 1.0,
                transform: DisplayTransform::Normal,
                role: OutputRole::Normal,
                primary,
                color_profile: DisplayColorProfile::Auto,
                icc_profile_path: None,
                hdr_requested: false,
                hdr_enabled: false,
                hdr_appearance: HdrAppearance::default(),
            },
            make: "Fixture".into(),
            model: connector.into(),
            modes: vec![DisplayMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            }],
            hdr_supported: false,
            hdr_active: false,
            wide_gamut_active: false,
            icc_lut_fallback_active: false,
        }
    }
    #[test]
    fn validates_modes_hdr_duplicates_primary_and_connected_topology_before_mutation() {
        let current = vec![display("DP-1", 0, true), display("DP-2", 1920, false)];
        let expected = current
            .iter()
            .map(|output| output.config.clone())
            .collect::<Vec<_>>();
        assert!(validate_trial(&current, &expected, &expected, true).is_ok());
        for invalid in 0..7 {
            let mut requested = expected.clone();
            match invalid {
                0 => requested[0].width = 9999,
                1 => requested[1].connector = "DP-1".into(),
                2 => requested[0].hdr_requested = true,
                3 => requested[0].scale = f32::NAN,
                4 => requested[1].primary = true,
                5 => requested[1].x = 9000,
                _ => requested[0].icc_profile_path = Some("/arbitrary/file".into()),
            }
            assert!(
                validate_trial(&current, &expected, &requested, true).is_err(),
                "case {invalid}"
            );
        }
        let mut stale = expected.clone();
        stale[0].scale = 1.25;
        assert!(validate_trial(&current, &stale, &expected, true).is_err());
    }
    #[test]
    fn deadline_and_current_trial_identity_gate_confirmation() {
        let now = Instant::now();
        let previous = vec![display("DP-1", 0, true).config];
        let mut requested = previous.clone();
        requested[0].scale = 1.25;
        let mut trial = DisplayTrial {
            id: 7,
            previous,
            requested,
            persisted: vec![],
            deadline: now + TRIAL_TIMEOUT,
            phase: DisplayPreviewPhase::Applying,
            backend_pending: true,
            retry_after: None,
            icc_versions: vec![],
        };
        assert!(trial.confirmable(7, now).is_err());
        trial.backend_pending = false;
        trial.phase = DisplayPreviewPhase::Confirming;
        assert!(trial.confirmable(7, now).is_ok());
        assert!(trial.confirmable(6, now).is_err());
        assert!(trial.confirmable(7, now + TRIAL_TIMEOUT).is_err());
        assert_eq!(trial.target()[0].scale, 1.25);
        trial.phase = DisplayPreviewPhase::Reverting;
        assert_eq!(trial.target()[0].scale, 1.0);
        assert!(trial.confirmable(7, now).is_err());
    }

    #[test]
    fn disable_reenable_and_roles_require_an_enabled_primary_desktop() {
        let mut current = vec![display("DP-1", 0, true), display("DP-2", 1920, false)];
        let expected = current
            .iter()
            .map(|output| output.config.clone())
            .collect::<Vec<_>>();
        let mut requested = expected.clone();
        requested[1].enabled = false;
        requested[1].x = 20000; // Disabled outputs do not bridge or constrain topology.
        assert!(validate_trial(&current, &expected, &requested, true).is_ok());
        assert!(validate_trial(&current, &expected, &requested, false).is_err());
        requested[0].enabled = false;
        requested[0].primary = false;
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
        requested = expected.clone();
        requested[0].role = OutputRole::ControlPanel;
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
        requested[0].primary = false;
        requested[1].primary = true;
        assert!(validate_trial(&current, &expected, &requested, true).is_ok());
        requested[1].role = OutputRole::ControlPanel;
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
        current[1].config.enabled = false;
        let expected = current
            .iter()
            .map(|output| output.config.clone())
            .collect::<Vec<_>>();
        requested = expected.clone();
        requested[1].enabled = true;
        assert!(validate_trial(&current, &expected, &requested, true).is_ok());
        requested[1].width = 9999;
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
    }

    #[test]
    fn icc_trials_require_valid_local_rgb_profiles_and_detect_file_changes() {
        let path = std::env::temp_dir().join(format!("display-trial-{}.icc", std::process::id()));
        let mut profile = lcms2::Profile::new_srgb();
        std::fs::write(&path, profile.icc().unwrap()).unwrap();
        let version = validate_icc_path(path.to_str().unwrap()).unwrap();
        let current = vec![display("DP-1", 0, true)];
        let expected = vec![current[0].config.clone()];
        let mut requested = expected.clone();
        requested[0].icc_profile_path = Some(path.display().to_string());
        assert!(validate_trial(&current, &expected, &requested, false).is_ok());
        profile.set_version(2.1);
        std::fs::write(&path, profile.icc().unwrap()).unwrap();
        assert_ne!(validate_icc_path(path.to_str().unwrap()).unwrap(), version);
        std::fs::write(&path, b"not an ICC profile").unwrap();
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
        assert!(validate_icc_path("relative.icc").is_err());
        assert!(validate_icc_path(std::env::temp_dir().to_str().unwrap()).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn hdr_drafts_use_shared_bounds_and_require_enabled_hdr_hardware() {
        let mut current = vec![display("DP-1", 0, true)];
        current[0].hdr_supported = true;
        let expected = vec![current[0].config.clone()];
        let mut requested = expected.clone();
        requested[0].hdr_requested = true;
        requested[0].hdr_appearance.peak_nits = 1000.0;
        assert!(validate_trial(&current, &expected, &requested, true).is_ok());
        assert!(validate_trial(&current, &expected, &requested, false).is_err());
        requested[0].hdr_appearance.full_frame_peak_nits = 1100.0;
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
        requested[0].hdr_appearance.full_frame_peak_nits = 300.0;
        requested[0].hdr_requested = false;
        assert!(validate_trial(&current, &expected, &requested, true).is_err());
    }
}
