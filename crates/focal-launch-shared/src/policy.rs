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
    is_chrome_like(app)
        || lower.contains("firefox")
        || lower.contains("librewolf")
}

pub fn chrome_command_args(use_x11: bool, profile_dir: &str) -> Vec<String> {
    let ozone = if use_x11 { "x11" } else { "wayland" };

    vec![
        format!("--ozone-platform={ozone}"),
        "--disable-features=Vulkan".into(),
        format!("--user-data-dir={profile_dir}"),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--new-window".into(),
    ]
}
