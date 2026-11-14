//! Display operations are owned by the compositor, including trial expiry.
use focaldesk_config::{load_config_read_only, FocalDeskConfig};
use focaldesk_ipc::{send_desktop_request, DisplayConfiguration, IpcRequest, IpcResponse};
use focaldesk_settings_core::OutputConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutPreferences {
    pub topbar_on_all_outputs: bool,
    pub sidebar_on_all_outputs: bool,
    pub remember_focused_output: bool,
}
impl LayoutPreferences {
    fn from_config(config: &FocalDeskConfig) -> Self {
        Self {
            topbar_on_all_outputs: config.displays.topbar_on_all_outputs,
            sidebar_on_all_outputs: config.displays.sidebar_on_all_outputs,
            remember_focused_output: config.displays.remember_focused_output,
        }
    }
}
pub fn layout_preferences() -> LayoutPreferences {
    LayoutPreferences::from_config(&load_config_read_only())
}
fn merge_layout(
    mut latest: FocalDeskConfig,
    expected: &LayoutPreferences,
    requested: &LayoutPreferences,
) -> Result<FocalDeskConfig, String> {
    if LayoutPreferences::from_config(&latest) != *expected {
        return Err(
            "Display layout preferences changed in another client. Reload before applying.".into(),
        );
    }
    latest.displays.topbar_on_all_outputs = requested.topbar_on_all_outputs;
    latest.displays.sidebar_on_all_outputs = requested.sidebar_on_all_outputs;
    latest.displays.remember_focused_output = requested.remember_focused_output;
    Ok(latest)
}
pub fn apply_layout(
    expected: LayoutPreferences,
    requested: LayoutPreferences,
) -> Result<LayoutPreferences, String> {
    if snapshot()?.preview.is_some() {
        return Err("Finish the current display trial before changing layout preferences.".into());
    }
    focaldesk_ipc::send_desktop_config(merge_layout(
        load_config_read_only(),
        &expected,
        &requested,
    )?)?;
    Ok(layout_preferences())
}

fn display_connection_error(message: String) -> String {
    if message.contains("IPC denied") {
        format!("The compositor denied this Settings executable. A release compositor requires a root-owned installed Settings preview or an approved service. Launch the installed preview; a binary run directly from /tmp may be rejected. Details: {message}")
    } else if message.contains("Connection reset by peer") || message.contains("Broken pipe") {
        format!("The compositor closed the connection before display information could be loaded. It may have rejected this Settings executable. A release compositor requires a root-owned installed preview or an approved service. Details: {message}")
    } else {
        message
    }
}
fn request(request: IpcRequest) -> Result<DisplayConfiguration, String> {
    match send_desktop_request(&request).map_err(display_connection_error)? {
        IpcResponse::DisplayConfiguration { configuration } => Ok(configuration),
        IpcResponse::Error { message } => Err(display_connection_error(message)),
        other => Err(format!(
            "Unexpected display response: {other:?}. Use the compositor from this checkout."
        )),
    }
}
pub fn snapshot() -> Result<DisplayConfiguration, String> {
    request(IpcRequest::GetDisplayConfiguration)
}
pub fn preview(
    expected: Vec<OutputConfig>,
    outputs: Vec<OutputConfig>,
) -> Result<DisplayConfiguration, String> {
    request(IpcRequest::PreviewDisplays { expected, outputs })
}
pub fn confirm(preview_id: u64) -> Result<DisplayConfiguration, String> {
    request(IpcRequest::ConfirmDisplays { preview_id })
}
pub fn revert(preview_id: u64) -> Result<DisplayConfiguration, String> {
    request(IpcRequest::RevertDisplays { preview_id })
}
pub fn calibration_pattern(
    preview_id: u64,
    connector: String,
    pattern: focaldesk_settings_core::HdrCalibrationPattern,
) -> Result<(), String> {
    match send_desktop_request(&IpcRequest::SetDisplayTrialCalibrationPattern {
        preview_id,
        connector,
        pattern,
    })? {
        IpcResponse::Ok => Ok(()),
        IpcResponse::Error { message } => Err(message),
        other => Err(format!("Unexpected calibration response: {other:?}")),
    }
}
pub fn hdr_presets() -> Vec<(&'static str, focaldesk_settings_core::HdrAppearance)> {
    use focaldesk_settings_core::HdrAppearance;
    vec![
        ("Neutral", HdrAppearance::default()),
        (
            "Bright desktop",
            HdrAppearance {
                reference_white_nits: 250.0,
                midtone_gamma: 0.90,
                ..Default::default()
            },
        ),
        (
            "Vivid",
            HdrAppearance {
                saturation: 1.10,
                midtone_gamma: 1.10,
                ..Default::default()
            },
        ),
    ]
}

pub fn exclusive_hdr() -> focaldesk_settings_core::ExclusiveHdrState {
    focaldesk_settings_core::load_exclusive_hdr_state()
}
pub fn restart_exclusive_hdr(
    connector: String,
    expected: focaldesk_settings_core::ExclusiveHdrState,
    enabled: bool,
) -> Result<String, String> {
    use focaldesk_settings_core::{ExclusiveHdrPhase, ExclusiveHdrState};
    let configuration = snapshot()?;
    if configuration.preview.is_some() || !configuration.hardware_modes {
        return Err(
            "Finish display trials and use a physical DRM session before restarting HDR.".into(),
        );
    }
    let output = configuration
        .outputs
        .iter()
        .find(|output| output.config.connector == connector)
        .ok_or("Display disconnected. Reload before continuing.")?;
    if enabled && (!output.config.enabled || !output.hdr_supported) {
        return Err("Choose an enabled HDR-capable display.".into());
    }
    if exclusive_hdr() != expected {
        return Err("Exclusive HDR state changed. Reload before continuing.".into());
    }
    if matches!(
        expected.phase,
        ExclusiveHdrPhase::Starting | ExclusiveHdrPhase::Verifying
    ) {
        return Err("Exclusive HDR verification is already in progress.".into());
    }
    let mut next = ExclusiveHdrState {
        connector: Some(connector),
        phase: if enabled {
            ExclusiveHdrPhase::Requested
        } else {
            ExclusiveHdrPhase::Disabled
        },
        reason: None,
        session_id: None,
    };
    focaldesk_settings_core::save_exclusive_hdr_state(&next).map_err(|e| e.to_string())?;
    match send_desktop_request(&IpcRequest::ExecuteDesktopAction {
        action: focaldesk_ipc::DesktopAction::Logout,
    }) {
        Ok(IpcResponse::Ok) => Ok(
            "HDR session preference saved; logout requested. Sign in again to verify it.".into(),
        ),
        response => {
            let message = format!("HDR session preference saved, but logout was not acknowledged ({response:?}). Log out manually to activate it.");
            next.reason = Some(message.clone());
            focaldesk_settings_core::save_exclusive_hdr_state(&next)
                .map_err(|e| format!("{message} Could not save restart status: {e}"))?;
            Ok(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_errors_explain_installation_without_claiming_no_session() {
        let reset = display_connection_error("Connection reset by peer (os error 104)".into());
        assert!(reset.contains("root-owned installed preview"));
        assert!(reset.contains("may have rejected"));
        let denial = display_connection_error(
            "desktop IPC denied peer pid=1 exe=/tmp/focaldesk-settings-next".into(),
        );
        assert!(denial.contains("denied this Settings executable"));
        assert_eq!(
            display_connection_error("Monitor disconnected".into()),
            "Monitor disconnected"
        );
    }

    #[test]
    fn layout_merge_preserves_other_config_and_rejects_stale_preferences() {
        let mut config = FocalDeskConfig::default();
        config.appearance.font_scale = 1.25;
        config.dock.visibility = focaldesk_config::DockVisibility::Autohide;
        let expected = LayoutPreferences::from_config(&config);
        let requested = LayoutPreferences {
            topbar_on_all_outputs: !expected.topbar_on_all_outputs,
            sidebar_on_all_outputs: !expected.sidebar_on_all_outputs,
            remember_focused_output: !expected.remember_focused_output,
        };
        let merged = merge_layout(config.clone(), &expected, &requested).unwrap();
        assert!(LayoutPreferences::from_config(&merged) == requested);
        assert_eq!(merged.appearance.font_scale, 1.25);
        assert_eq!(merged.dock.visibility, config.dock.visibility);
        assert!(merge_layout(merged, &expected, &requested).is_err());
    }
}
