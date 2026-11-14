//! Typed appearance operations shared by Settings frontends.
//!
//! Preview is a projection only. Applying reloads the current configuration,
//! rejects stale appearance edits, and asks the compositor to persist the change.
use anyhow::{bail, Context, Result};
use focaldesk_config::{load_config_read_only, DockVisibility, FocalDeskConfig, RailBackdrop};
use focaldesk_themes::{manager::theme_from_document, ThemeDocument, ThemePackage};
use image::GenericImageView;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    pub theme: String,
    pub font_scale: f64,
    pub glow_strength: f64,
    pub output_focus_glow: bool,
    pub shader_chrome: bool,
    pub work_area_glass: bool,
    pub rail_backdrop: RailBackdrop,
    pub panel_darkness: Option<f64>,
    pub task_shelf_visibility: DockVisibility,
}

impl Appearance {
    pub fn from_config(config: &FocalDeskConfig) -> Self {
        Self {
            theme: config.appearance.theme.clone(),
            font_scale: config.appearance.font_scale,
            glow_strength: config.appearance.glow_strength,
            output_focus_glow: config.appearance.output_focus_glow,
            shader_chrome: config.appearance.shader_chrome,
            work_area_glass: config.appearance.work_area_glass,
            rail_backdrop: config.appearance.rail_backdrop,
            panel_darkness: config.appearance.panel_darkness,
            task_shelf_visibility: config.dock.visibility,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !self.font_scale.is_finite() || !(0.75..=2.0).contains(&self.font_scale) {
            bail!("Text size must be between 75% and 200%.");
        }
        if !self.glow_strength.is_finite() || !(0.0..=1.0).contains(&self.glow_strength) {
            bail!("Glow strength must be between 0 and 100%.");
        }
        if self
            .panel_darkness
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            bail!("Panel darkness must be between 0 and 100%, or use the theme default.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeChoice {
    pub id: String,
    pub name: String,
    pub source: String,
    pub accent: String,
    pub surface: String,
    pub text: String,
    pub dynamic_range: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallpaper_preview: Option<String>,
    pub wallpaper_hdr: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppearanceSnapshot {
    pub appearance: Appearance,
    pub themes: Vec<ThemeChoice>,
}

const PACKAGES: &[&str] = &[
    include_str!("../../../assets/themes/optional/eagle.fdtheme"),
    include_str!("../../../assets/themes/optional/mars-colony.fdtheme"),
    include_str!("../../../assets/themes/optional/classic.fdtheme"),
    include_str!("../../../assets/themes/optional/moonbase.fdtheme"),
    include_str!("../../../assets/themes/optional/venus_aerie.fdtheme"),
    include_str!("../../../assets/themes/optional/lunar_transport.fdtheme"),
    include_str!("../../../assets/themes/optional/deepspace.fdtheme"),
];

/// Validate portable collection assets once and reuse their documents.
pub fn collection_packages() -> Result<&'static [ThemePackage]> {
    static VALIDATED: std::sync::OnceLock<std::result::Result<Vec<ThemePackage>, String>> =
        std::sync::OnceLock::new();
    match VALIDATED.get_or_init(|| {
        PACKAGES
            .iter()
            .map(|source| ThemePackage::from_toml(source))
            .collect::<Result<Vec<_>>>()
            .map_err(|error| format!("{error:#}"))
    }) {
        Ok(packages) => Ok(packages),
        Err(error) => bail!("Invalid collection package: {error}"),
    }
}

fn themes_root() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("The user data directory is unavailable.")?
        .join("focaldesk/themes"))
}

fn css_color(color: [f32; 4]) -> String {
    format!(
        "rgb({} {} {})",
        (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn choice(id: String, source: &str, document: &ThemeDocument) -> Result<ThemeChoice> {
    let theme = theme_from_document(document)?;
    let (wallpaper_preview, wallpaper_hdr) = wallpaper_preview(document);
    Ok(ThemeChoice {
        id,
        name: document.name.clone(),
        source: source.into(),
        accent: css_color(theme.chrome.accent_color),
        surface: css_color(theme.dialog.panel_color),
        text: css_color(theme.text.normal),
        dynamic_range: match document.intent.dynamic_range {
            focaldesk_themes::ThemeDynamicRange::Sdr => "SDR",
            _ => "HDR",
        }
        .into(),
        wallpaper_preview,
        wallpaper_hdr,
    })
}

const WALLPAPER_PREVIEW_MAX_BYTES: u64 = 64 * 1024 * 1024;
const WALLPAPER_PREVIEW_SIZE: (u32, u32) = (960, 540);

pub(crate) fn wallpaper_preview(document: &ThemeDocument) -> (Option<String>, bool) {
    let Some(path) = document.wallpaper.path.as_deref() else {
        return (None, false);
    };
    if Path::new(path).is_absolute() {
        let file_path = Path::new(path);
        let Ok(metadata) = std::fs::metadata(file_path) else {
            return (None, path.to_ascii_lowercase().ends_with(".exr"));
        };
        if metadata.len() > WALLPAPER_PREVIEW_MAX_BYTES {
            return (None, path.to_ascii_lowercase().ends_with(".exr"));
        }
        let stamp = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos());
        let key = format!("{}:{}:{stamp}", file_path.display(), metadata.len());
        let is_hdr = file_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"));
        if let Ok(cache) = preview_cache().lock() {
            if let Some(preview) = cache.get(&key).cloned() {
                return (Some(preview), is_hdr);
            }
        }
        let Ok(bytes) = std::fs::read(file_path) else {
            return (None, is_hdr);
        };
        let preview = encode_wallpaper_preview(&bytes, is_hdr);
        if let (Some(preview), Ok(mut cache)) = (&preview, preview_cache().lock()) {
            cache.insert(key, preview.clone());
        }
        return (preview, is_hdr);
    }

    // Bundled packages keep their image bytes inside the portable package.
    for package in collection_packages().ok().into_iter().flatten() {
        if package.document.name != document.name
            || package.document.wallpaper.path.as_deref() != Some(path)
        {
            continue;
        }
        let Some(asset) = package.assets.iter().find(|asset| asset.path == path) else {
            continue;
        };
        let is_hdr = path.to_ascii_lowercase().ends_with(".exr");
        let key = format!("package:{}:{}", package.slug, asset.sha256);
        if let Ok(cache) = preview_cache().lock() {
            if let Some(preview) = cache.get(&key).cloned() {
                return (Some(preview), is_hdr);
            }
        }
        use base64::Engine as _;
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&asset.data_base64) else {
            return (None, is_hdr);
        };
        let preview = encode_wallpaper_preview(&bytes, is_hdr);
        if let (Some(preview), Ok(mut cache)) = (&preview, preview_cache().lock()) {
            cache.insert(key, preview.clone());
        }
        return (preview, is_hdr);
    }
    (None, path.to_ascii_lowercase().ends_with(".exr"))
}

fn preview_cache() -> &'static Mutex<std::collections::HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn encode_wallpaper_preview(bytes: &[u8], is_hdr: bool) -> Option<String> {
    if bytes.len() as u64 > WALLPAPER_PREVIEW_MAX_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(image::Limits::default());
    let image = reader.decode().ok()?;
    let (width, height) = image.dimensions();
    if width == 0
        || height == 0
        || width > 32_768
        || height > 32_768
        || u64::from(width) * u64::from(height) > 100_000_000
    {
        return None;
    }
    let preview = if is_hdr {
        let resized = image
            .resize_to_fill(
                WALLPAPER_PREVIEW_SIZE.0,
                WALLPAPER_PREVIEW_SIZE.1,
                image::imageops::FilterType::Triangle,
            )
            .to_rgba32f();
        let mut rgb = image::RgbImage::new(resized.width(), resized.height());
        for (source, target) in resized.pixels().zip(rgb.pixels_mut()) {
            for channel in 0..3 {
                let linear = source[channel].max(0.0);
                let mapped = linear / (1.0 + linear);
                let srgb = if mapped <= 0.003_130_8 {
                    mapped * 12.92
                } else {
                    1.055 * mapped.powf(1.0 / 2.4) - 0.055
                };
                target[channel] = (srgb.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        rgb
    } else {
        image
            .resize_to_fill(
                WALLPAPER_PREVIEW_SIZE.0,
                WALLPAPER_PREVIEW_SIZE.1,
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8()
    };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode_image(&image::DynamicImage::ImageRgb8(preview))
        .ok()?;
    use base64::Engine as _;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(jpeg)
    ))
}

pub fn snapshot() -> Result<AppearanceSnapshot> {
    let config = load_config_read_only();
    let default = ThemeDocument::load(std::path::Path::new(
        focaldesk_themes::SYSTEM_DEFAULT_THEME_PATH,
    ))
    .or_else(|_| ThemeDocument::from_toml(include_str!("../../../assets/themes/default.toml")))?;
    let mut themes = vec![choice("Default".into(), "System", &default)?];
    let root = themes_root()?;
    let packages = collection_packages()?;
    let mut installed_collection_slugs = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        let mut installed = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = entry.path().join("theme.toml");
            if let Ok(document) = ThemeDocument::load(&path) {
                let collection = packages.iter().find(|package| {
                    let canonical_document = root.join(&package.slug).join("theme.toml");
                    if path != canonical_document {
                        return false;
                    }
                    let mut expected = package.document.clone();
                    if let Some(asset) = expected.wallpaper.path.as_deref() {
                        expected.wallpaper.path = Some(
                            canonical_document
                                .parent()
                                .unwrap_or(&root)
                                .join(asset)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                    document == expected
                });
                let (source, id) = if let Some(package) = collection {
                    installed_collection_slugs.insert(package.slug.as_str());
                    ("FocalDesk collection", path.to_string_lossy().into_owned())
                } else {
                    ("Installed", path.to_string_lossy().into_owned())
                };
                installed.push(choice(id, source, &document)?);
            }
        }
        installed.sort_by(|a, b| a.name.cmp(&b.name));
        themes.extend(installed);
    }
    for package in packages {
        if !installed_collection_slugs.contains(package.slug.as_str()) {
            themes.push(choice(
                format!("package:{}", package.slug),
                "FocalDesk collection",
                &package.document,
            )?);
        }
    }
    // Preserve an existing custom selection rather than silently changing it.
    if !themes
        .iter()
        .any(|theme| theme.id == config.appearance.theme)
    {
        let document = ThemeDocument::load(std::path::Path::new(&config.appearance.theme))
            .context(
                "The selected theme is unavailable. Restore it using the existing Settings app.",
            )?;
        themes.push(choice(
            config.appearance.theme.clone(),
            "Current custom theme",
            &document,
        )?);
    }
    Ok(AppearanceSnapshot {
        appearance: Appearance::from_config(&config),
        themes,
    })
}

fn merge_appearance(
    mut latest: FocalDeskConfig,
    expected: &Appearance,
    requested: &Appearance,
    resolved_theme: String,
) -> Result<FocalDeskConfig> {
    requested.validate()?;
    if Appearance::from_config(&latest) != *expected {
        bail!("Appearance changed in another window. Reload before applying your changes.");
    }
    latest.appearance.theme = resolved_theme;
    latest.appearance.font_scale = requested.font_scale;
    latest.appearance.glow_strength = requested.glow_strength;
    latest.appearance.output_focus_glow = requested.output_focus_glow;
    latest.appearance.shader_chrome = requested.shader_chrome;
    latest.appearance.work_area_glass = requested.work_area_glass;
    latest.appearance.rail_backdrop = requested.rail_backdrop;
    latest.appearance.panel_darkness = requested.panel_darkness;
    latest.dock.visibility = requested.task_shelf_visibility;
    Ok(latest)
}

fn legacy_collection_package(
    selected: &str,
    root: &std::path::Path,
) -> Result<Option<&'static ThemePackage>> {
    let path = std::path::Path::new(selected);
    if !path.starts_with(root) || !path.is_file() {
        return Ok(None);
    }
    let document = ThemeDocument::load(path)?;
    if document.wallpaper.path.is_some() {
        return Ok(None);
    }
    for package in collection_packages()? {
        if path != root.join(&package.slug).join("theme.toml") {
            continue;
        }
        let mut original = package.document.clone();
        original.wallpaper.path = None;
        if document == original && !package.assets.is_empty() {
            return Ok(Some(package));
        }
    }
    Ok(None)
}

pub fn apply(expected: Appearance, requested: Appearance) -> Result<AppearanceSnapshot> {
    requested.validate()?;
    let current = snapshot()?;
    if !current
        .themes
        .iter()
        .any(|theme| theme.id == requested.theme)
    {
        bail!("Choose a theme from the available collection.");
    }
    // Check for stale edits before installing a packaged theme.
    let latest = load_config_read_only();
    merge_appearance(
        latest.clone(),
        &expected,
        &requested,
        requested.theme.clone(),
    )?;
    // Old collection installs omitted their wallpaper. Repair only an exact
    // unmodified legacy document at its canonical collection path, on Apply.
    let legacy_package = legacy_collection_package(&requested.theme, &themes_root()?)?;
    let resolved = if let Some(slug) = requested.theme.strip_prefix("package:") {
        let package = collection_packages()?
            .iter()
            .find(|package| package.slug == slug)
            .context("The selected theme package is unavailable.")?;
        package
            .install(&themes_root()?)?
            .document_path
            .to_string_lossy()
            .into_owned()
    } else if let Some(package) = legacy_package {
        package
            .install(&themes_root()?)?
            .document_path
            .to_string_lossy()
            .into_owned()
    } else {
        requested.theme.clone()
    };
    let merged = merge_appearance(load_config_read_only(), &expected, &requested, resolved)?;
    // No direct-save fallback: a compositor rejection must never be reported as success.
    focaldesk_ipc::send_desktop_config(merged).map_err(|error| {
        anyhow::anyhow!("Could not apply appearance. Check that FocalDesk is running: {error}")
    })?;
    snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_numbers_and_unknown_fields() {
        let mut appearance = Appearance::from_config(&FocalDeskConfig::default());
        for invalid in [f64::NAN, f64::INFINITY, 0.0, 2.1] {
            appearance.font_scale = invalid;
            assert!(appearance.validate().is_err());
        }
        appearance.font_scale = 1.0;
        appearance.glow_strength = -0.1;
        assert!(appearance.validate().is_err());
        let mut value =
            serde_json::to_value(Appearance::from_config(&FocalDeskConfig::default())).unwrap();
        value["command"] = "anything".into();
        assert!(serde_json::from_value::<Appearance>(value).is_err());
    }

    #[test]
    fn rejects_stale_edits_and_preserves_unrelated_settings() {
        let mut latest = FocalDeskConfig::default();
        latest.dock.visibility = focaldesk_config::DockVisibility::Autohide;
        latest.appearance.work_area_glass = false;
        let expected = Appearance::from_config(&latest);
        let mut requested = expected.clone();
        requested.font_scale = 1.25;
        let merged =
            merge_appearance(latest.clone(), &expected, &requested, "Default".into()).unwrap();
        assert_eq!(merged.appearance.font_scale, 1.25);
        assert!(!merged.appearance.work_area_glass);
        assert_eq!(
            merged.dock.visibility,
            focaldesk_config::DockVisibility::Autohide
        );
        latest.appearance.font_scale = 1.5;
        assert!(merge_appearance(latest, &expected, &requested, "Default".into()).is_err());
    }

    #[test]
    fn projects_every_packaged_theme_without_installing_it() {
        for source in PACKAGES {
            let package = ThemePackage::from_toml(source).unwrap();
            let preview = choice(
                format!("package:{}", package.slug),
                "Collection",
                &package.document,
            )
            .unwrap();
            assert!(
                package.document.wallpaper.path.is_some(),
                "{} must include its wallpaper",
                package.slug
            );
            assert_eq!(
                package.assets.len(),
                1,
                "{} must ship a portable wallpaper asset",
                package.slug
            );
            assert_eq!(preview.name, package.document.name);
            assert!(preview.accent.starts_with("rgb("));
        }
    }

    #[test]
    fn every_collection_wallpaper_installs_and_fully_decodes() {
        let root = tempfile::tempdir().unwrap();
        for package in collection_packages().unwrap() {
            let installed = package.install(root.path()).unwrap();
            let document = ThemeDocument::load(&installed.document_path).unwrap();
            let wallpaper = std::path::Path::new(document.wallpaper.path.as_ref().unwrap());
            assert!(wallpaper.starts_with(&installed.directory));
            let decoded = image::open(wallpaper).unwrap();
            assert!(
                decoded.width() > 0 && decoded.height() > 0,
                "{}",
                package.slug
            );
        }
    }

    #[test]
    fn default_wallpaper_reference_is_shipped_and_decodes() {
        let document =
            ThemeDocument::from_toml(include_str!("../../../assets/themes/default.toml")).unwrap();
        let wallpaper = std::path::Path::new(document.wallpaper.path.as_ref().unwrap());
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/wallpaper")
            .join(wallpaper.file_name().unwrap());
        let decoded = image::open(source).unwrap();
        assert!(decoded.width() > 0 && decoded.height() > 0);
        let package = ThemePackage::from_toml(include_str!(
            "../../../assets/themes/focaldesk-theme.fdtheme"
        ))
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let installed = package.install(root.path()).unwrap();
        let document = ThemeDocument::load(&installed.document_path).unwrap();
        image::open(document.wallpaper.path.unwrap()).unwrap();
    }

    #[test]
    fn repairing_legacy_collection_keeps_custom_documents_untouched() {
        let root = tempfile::tempdir().unwrap();
        let package = ThemePackage::from_toml(PACKAGES[2]).unwrap();
        let installed = package.install(root.path()).unwrap();
        let mut legacy = ThemeDocument::load(&installed.document_path).unwrap();
        legacy.wallpaper.path = None;
        legacy.save(&installed.document_path).unwrap();
        let selected = installed.document_path.to_str().unwrap();
        let repair = legacy_collection_package(selected, root.path())
            .unwrap()
            .unwrap();
        let repaired = repair.install(root.path()).unwrap();
        let document = ThemeDocument::load(&repaired.document_path).unwrap();
        assert!(std::path::Path::new(document.wallpaper.path.as_ref().unwrap()).is_file());
        assert!(legacy_collection_package(selected, root.path())
            .unwrap()
            .is_none());
        legacy.name = "My customized theme".into();
        legacy.save(&installed.document_path).unwrap();
        assert!(legacy_collection_package(selected, root.path())
            .unwrap()
            .is_none());
    }

    #[test]
    fn visual_controls_validate_merge_and_reject_stale_shelf_edits() {
        let mut latest = FocalDeskConfig::default();
        let expected = Appearance::from_config(&latest);
        let mut requested = expected.clone();
        requested.shader_chrome = false;
        requested.work_area_glass = false;
        requested.rail_backdrop = RailBackdrop::SmokedGlass;
        requested.panel_darkness = Some(0.62);
        requested.task_shelf_visibility = DockVisibility::Autohide;
        let merged = merge_appearance(
            latest.clone(),
            &expected,
            &requested,
            requested.theme.clone(),
        )
        .unwrap();
        assert_eq!(Appearance::from_config(&merged), requested);
        let saved = Appearance::from_config(&merged);
        requested.panel_darkness = None;
        let inherited =
            merge_appearance(merged, &saved, &requested, requested.theme.clone()).unwrap();
        assert_eq!(inherited.appearance.panel_darkness, None);
        for invalid in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            requested.panel_darkness = Some(invalid);
            assert!(requested.validate().is_err());
        }
        requested.panel_darkness = None;
        latest.dock.visibility = DockVisibility::Autohide;
        assert!(merge_appearance(latest, &expected, &requested, requested.theme.clone()).is_err());
    }
}
