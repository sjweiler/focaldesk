use focaldesk_themes::{ThemeDocument, ThemePackage};
use std::path::Path;

#[test]
fn packaged_system_default_is_a_valid_theme_document() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/default.toml");
    let document = ThemeDocument::load(&path).expect("packaged default theme should be valid");

    assert_eq!(document.name, "Default");
    assert_eq!(
        document.wallpaper.path.as_deref(),
        Some("/usr/share/focaldesk/wallpaper/focaldesk_wallpaper_hdr1400.exr")
    );
}

#[test]
fn optional_legacy_themes_are_valid_portable_packages() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/optional");
    for (slug, name) in [
        ("eagle", "Eagle"),
        ("moonbase", "Moonbase"),
        ("classic", "Classic"),
    ] {
        let package = ThemePackage::load(&root.join(format!("{slug}.fdtheme")))
            .unwrap_or_else(|error| panic!("{slug} package should be valid: {error:#}"));
        assert_eq!(package.slug, slug);
        assert_eq!(package.document.name, name);
    }
}
