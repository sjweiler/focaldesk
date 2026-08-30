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

    // In HDR, let Chrome follow the compositor's wp_color preferred
    // Display-P3/extended-sRGB description. Forcing HDR10 here can make an
    // 8-bit AB24 raster buffer carry pixels encoded for a different transfer
    // function, visibly corrupting wide-gamut images. SDR keeps the explicit
    // P3 target used for wide-gamut CSS and images.
    if !hdr_output_active {
        args.insert(2, "--force-color-profile=display-p3-d65".into());
    }

    args
}

#[cfg(test)]
mod tests {
    use super::{chrome_command_args, chrome_hdr_mode_active};

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
    fn chrome_hdr_launch_follows_wayland_preferred_color() {
        let args = chrome_command_args(false, "/tmp/focaldesk-chrome-test", true);
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("--force-color-profile="))
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
