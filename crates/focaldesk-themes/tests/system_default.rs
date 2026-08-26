use focaldesk_themes::ThemeDocument;
use std::path::Path;

#[test]
fn packaged_system_default_is_a_valid_theme_document() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/default.toml");
    let document = ThemeDocument::load(&path).expect("packaged default theme should be valid");

    assert_eq!(document.name, "Default");
    assert_eq!(
        document.wallpaper.path.as_deref(),
        Some("/usr/share/focaldesk/wallpaper/focaldesk_wallpaper.png")
    );
}
