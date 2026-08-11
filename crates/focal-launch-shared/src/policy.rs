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

pub fn chrome_command_args(use_x11: bool, profile_dir: &str) -> Vec<String> {
    let ozone = if use_x11 { "x11" } else { "wayland" };

    vec![
        format!("--ozone-platform={ozone}"),
        // Chrome's upstream Wayland color-management implementation is gated
        // by this feature. It is normally enabled by default, but variations
        // and remote kill switches can turn it off for an existing profile.
        // FocalDesk depends on wp_color_management_v1 for Display P3 buffers,
        // so make that dependency explicit for compositor-launched browsers.
        "--enable-features=WaylandWpColorManagerV1".into(),
        // Keep Chromium's raster target wide-gamut. On Linux/Wayland Chromium
        // can negotiate a P3 surface while still flattening ICC-tagged images
        // through an sRGB raster target, which makes the wide-gamut.com W test
        // disappear. FocalDesk consumes the tagged P3 buffer and performs the
        // final P3 -> output ICC transform, including on sRGB outputs.
        "--force-color-profile=display-p3-d65".into(),
        "--disable-features=Vulkan".into(),
        format!("--user-data-dir={profile_dir}"),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--new-window".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::chrome_command_args;

    #[test]
    fn chrome_wayland_launch_enables_wp_color_management() {
        let args = chrome_command_args(false, "/tmp/focaldesk-chrome-test");
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
}
