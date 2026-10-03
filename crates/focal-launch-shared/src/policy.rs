// focal_launch/src/policy.rs

pub fn is_chrome_like(app: &str) -> bool {
    let lower = app.to_ascii_lowercase();
    lower.contains("chrome")
        || lower.contains("chromium")
        || lower.contains("google-chrome")
        || lower.contains("brave")
        || lower.contains("edge")
}

pub fn is_browser_like(app: &str) -> bool {
    let lower = app.to_ascii_lowercase();
    is_chrome_like(app) || lower.contains("firefox") || lower.contains("librewolf")
}

/// Whether an application is explicitly opted into compositor Auto HDR.
/// Entries are comma/semicolon separated and matched case-insensitively
/// against an executable name, Wayland app-id, or X11 WM_CLASS. `*` is
/// accepted for diagnostics but is intentionally never the default.
pub fn auto_hdr_app_enabled(app: &str) -> bool {
    let Ok(configured) = std::env::var("FOCALDESK_AUTO_HDR_APPS") else {
        return false;
    };
    auto_hdr_app_matches(&configured, app)
}

fn auto_hdr_app_matches(configured: &str, app: &str) -> bool {
    let app = app.trim().to_ascii_lowercase();
    let basename = app.rsplit('/').next().unwrap_or(&app);
    configured
        .split([',', ';'])
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry == "*" || entry == app || entry == basename)
}

fn finite_env_f32(name: &str) -> Option<f32> {
    std::env::var(name)
        .ok()?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

pub fn auto_hdr_sdr_nits() -> f32 {
    finite_env_f32("FOCALDESK_AUTO_HDR_SDR_NITS")
        .unwrap_or(100.0)
        .clamp(40.0, 400.0)
}

pub fn auto_hdr_target_nits() -> Option<f32> {
    finite_env_f32("FOCALDESK_AUTO_HDR_TARGET_NITS").map(|value| value.clamp(100.0, 10_000.0))
}

pub fn auto_hdr_gamut_wideness() -> f32 {
    finite_env_f32("FOCALDESK_AUTO_HDR_GAMUT_WIDENESS")
        .unwrap_or(0.0)
        .clamp(0.0, 0.35)
}

fn env_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_falsey(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// Whether Chromium must follow the compositor's HDR output description.
///
/// Normal HDR sessions are selected by `FOCALDESK_HDR_RENDER`; they do not use
/// the exclusive-HDR state file. Treating only exclusive HDR as active leaves
/// Chromium forced to Display P3 while the compositor is producing BT.2020/PQ.
pub fn chrome_hdr_mode_active(
    exclusive_hdr_active: bool,
    hdr_render: Option<&str>,
    hdr_kms: Option<&str>,
) -> bool {
    exclusive_hdr_active || (env_truthy(hdr_render) && !env_falsey(hdr_kms))
}

pub fn chrome_command_args(
    use_x11: bool,
    profile_dir: &str,
    hdr_output_active: bool,
) -> Vec<String> {
    let ozone = if use_x11 { "x11" } else { "wayland" };

    let mut args = vec![
        format!("--ozone-platform={ozone}"),
        // Chrome's upstream Wayland color-management implementation is gated
        // by this feature. It is normally enabled by default, but variations
        // and remote kill switches can turn it off for an existing profile.
        // FocalDesk depends on wp_color_management_v1 for Display P3 buffers,
        // so make that dependency explicit for compositor-launched browsers.
        "--enable-features=WaylandWpColorManagerV1".into(),
        "--disable-features=Vulkan".into(),
        format!("--user-data-dir={profile_dir}"),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--new-window".into(),
    ];

    // Current desktop Chromium can select a PQ surface while continuing to
    // allocate an 8-bit AB24 Wayland buffer, visibly quantizing gradients.
    // Keep browser composition in Display-P3/sRGB on both SDR and HDR outputs;
    // FocalDesk promotes that scene in FP16 before its final BT.2020/PQ encode.
    // Keep this input in the policy API so native browser HDR can be restored
    // once Chromium reliably selects AB30 or FP16.
    let _ = hdr_output_active;
    args.insert(2, "--force-color-profile=display-p3-d65".into());

    args
}

#[cfg(test)]
mod tests {
    use super::{auto_hdr_app_matches, chrome_command_args, chrome_hdr_mode_active};

    #[test]
    fn auto_hdr_allowlist_is_exact_case_insensitive_and_off_by_default() {
        assert!(!auto_hdr_app_matches("", "game.exe"));
        assert!(auto_hdr_app_matches(
            "other, GAME.EXE;steam_app_123",
            "/games/game.exe"
        ));
        assert!(auto_hdr_app_matches("steam_app_123", "STEAM_APP_123"));
        assert!(!auto_hdr_app_matches("game", "game.exe"));
        assert!(auto_hdr_app_matches("*", "anything"));
    }

    #[test]
    fn chrome_wayland_launch_enables_wp_color_management() {
        let args = chrome_command_args(false, "/tmp/focaldesk-chrome-test", false);
        assert!(
            args.iter()
                .any(|arg| arg == "--enable-features=WaylandWpColorManagerV1")
        );
        assert!(
            args.iter()
                .any(|arg| arg == "--force-color-profile=display-p3-d65")
        );
        assert!(args.iter().any(|arg| arg == "--ozone-platform=wayland"));
    }

    #[test]
    fn chrome_hdr_launch_avoids_eight_bit_pq_surfaces() {
        let args = chrome_command_args(false, "/tmp/focaldesk-chrome-test", true);
        assert!(
            args.iter()
                .any(|arg| arg == "--force-color-profile=display-p3-d65")
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("--force-raster-color-profile="))
        );
        assert!(
            args.iter()
                .any(|arg| arg == "--enable-features=WaylandWpColorManagerV1")
        );
    }

    #[test]
    fn normal_hdr_render_session_uses_hdr_chrome_profile() {
        assert!(chrome_hdr_mode_active(false, Some("1"), None));
        assert!(chrome_hdr_mode_active(false, Some(" true "), Some("1")));
        assert!(!chrome_hdr_mode_active(false, Some("1"), Some("off")));
        assert!(!chrome_hdr_mode_active(false, None, None));
    }

    #[test]
    fn verified_exclusive_hdr_overrides_environment_defaults() {
        assert!(chrome_hdr_mode_active(true, None, Some("0")));
    }
}
