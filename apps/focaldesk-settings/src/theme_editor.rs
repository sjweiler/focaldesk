//! Native editor bridge. Documents, color math, persistence and IPC stay in Rust.
use anyhow::{bail, Context, Result};
use focaldesk_ipc::{
    send_desktop_request, IpcRequest, IpcResponse, ThemeEditorCommand,
    THEME_EDITOR_PROTOCOL_VERSION,
};
use focaldesk_themes::{ThemeColor, ThemeColorSpace, ThemeDocument, ThemePackage, ThemePaint};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
pub struct FileVersion {
    pub path: String,
    pub hash: String,
}
#[derive(Serialize)]
pub struct EditorDocument {
    pub document: ThemeDocument,
    pub source: Option<FileVersion>,
    pub toml: String,
    pub preview: Projection,
}
#[derive(Serialize)]
pub struct Projection {
    pub wallpaper: Option<String>,
    pub wallpaper_tint: Option<String>,
    pub paint: String,
    pub surface: String,
    pub text: String,
    pub accent: String,
    pub contrast: Vec<Contrast>,
}
#[derive(Serialize)]
pub struct Contrast {
    pub surface: String,
    pub state: String,
    pub ratio: f32,
    pub required: f32,
}
#[derive(Serialize)]
pub struct RuntimeStatus {
    pub preview_active: bool,
    pub applied_revision: u64,
    pub gradient_rendering: bool,
    pub semantic_rendering: bool,
    pub contrast_issue_count: usize,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileAction {
    Describe {
        document: Box<ThemeDocument>,
    },
    New,
    Open {
        path: String,
    },
    Parse {
        source: String,
    },
    Save {
        path: String,
        expected_hash: Option<String>,
        document: Box<ThemeDocument>,
    },
    Export {
        path: String,
        expected_hash: Option<String>,
        document: Box<ThemeDocument>,
    },
    Install {
        path: String,
        expected_hash: String,
        overwrite: bool,
    },
}
pub fn default_document() -> Result<EditorDocument> {
    let mut document =
        ThemeDocument::from_toml(include_str!("../../../assets/themes/default.toml"))?;
    document.name = "Untitled Theme".into();
    document.wallpaper = Default::default();
    describe(document, None)
}
fn read(path: &Path, limit: u64) -> Result<String> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() {
        bail!("Choose a regular theme file rather than a symbolic link.");
    }
    if !kind.is_file() {
        bail!("Choose a regular theme file.");
    }
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        bail!("Choose a regular file.");
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Theme file exceeds its size limit.");
    }
    Ok(String::from_utf8(bytes)?)
}
fn hash(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
pub fn file_hash(path: &Path) -> Result<String> {
    Ok(hash(&read(path, 100 * 1024 * 1024)?))
}
fn path_checked(path: &str, extension: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    if !path.is_absolute() || path.extension().and_then(|x| x.to_str()) != Some(extension) {
        bail!("Choose an absolute path ending in .{extension}.");
    }
    Ok(path)
}
fn validate(document: &ThemeDocument) -> Result<()> {
    document.validate()?;
    if document.name.len() > 200 || document.name.chars().any(char::is_control) {
        bail!("Use a theme name of 1–200 characters without control characters.");
    }
    let layout = &document.semantic.layout;
    for value in [
        layout.bar_height,
        layout.dock_width,
        layout.padding,
        layout.gap,
        layout.icon_size,
    ] {
        if !(0.0..=512.0).contains(&value) {
            bail!("Layout dimensions must be between 0 and 512 pixels.");
        }
    }
    if !(-10.0..=30.0).contains(&document.semantic.typography.letter_spacing) {
        bail!("Letter spacing must be between -10 and 30 pixels.");
    }
    Ok(())
}
fn css(color: ThemeColor) -> String {
    let color = color.mapped_for_srgb_preview();
    let encode = |v: f32| {
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    format!(
        "rgb({} {} {} / {})",
        (encode(color.r.clamp(0.0, 1.0)) * 255.0).round(),
        (encode(color.g.clamp(0.0, 1.0)) * 255.0).round(),
        (encode(color.b.clamp(0.0, 1.0)) * 255.0).round(),
        color.a
    )
}
pub fn project(document: &ThemeDocument) -> Result<Projection> {
    validate(document)?;
    let paint = document.intent.mapped_for_sdr_preview();
    let colors = (0..=16)
        .map(|i| {
            format!(
                "{} {}%",
                css(paint.sample_ramp(i as f32 / 16.0).unwrap()),
                i as f32 * 100.0 / 16.0
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let paint = match paint {
        ThemePaint::Solid { color } => css(color),
        ThemePaint::LinearGradient { angle, .. } => format!("linear-gradient({angle}deg,{colors})"),
        ThemePaint::RadialGradient { center, radius, .. } => format!(
            "radial-gradient(ellipse {}% {}% at {}% {}%,{colors})",
            radius * 100.0,
            radius * 100.0,
            center.0 * 100.0,
            center.1 * 100.0
        ),
    };
    let semantic = &document.semantic;
    let contrast = semantic
        .contrast_issues()
        .into_iter()
        .map(|issue| Contrast {
            surface: issue.surface.into(),
            state: format!("{:?}", issue.state).to_lowercase(),
            ratio: issue.ratio,
            required: issue.required_ratio,
        })
        .collect();
    Ok(Projection {
        wallpaper: crate::appearance::wallpaper_preview(document).0,
        wallpaper_tint: document.wallpaper.tint.map(css),
        paint,
        surface: css(semantic.surfaces.popup.normal),
        text: css(semantic.typography.primary),
        accent: css(semantic.surfaces.active_button.normal),
        contrast,
    })
}
fn describe(document: ThemeDocument, source: Option<FileVersion>) -> Result<EditorDocument> {
    let preview = project(&document)?;
    let toml = document.to_toml()?;
    Ok(EditorDocument {
        document,
        source,
        toml,
        preview,
    })
}
fn atomic_save(path: &Path, expected: Option<&str>, source: &str) -> Result<()> {
    let current = if path.exists() {
        Some(read(path, 100 * 1024 * 1024)?)
    } else {
        None
    };
    if current.as_deref().map(hash).as_deref() != expected {
        bail!("The destination changed or already exists. Reopen it before overwriting.");
    }
    let parent = path.parent().context("Missing destination directory")?;
    let temporary = parent.join(format!(
        ".theme-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        if expected.is_none() {
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        } else {
            fs::rename(&temporary, path)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub fn file_action(action: FileAction) -> Result<EditorDocument> {
    match action {
        FileAction::Describe { document } => describe(*document, None),
        FileAction::New => default_document(),
        FileAction::Parse { source } => {
            if source.len() > 1024 * 1024 {
                bail!("Theme source exceeds 1 MiB.");
            }
            describe(ThemeDocument::from_toml(&source)?, None)
        }
        FileAction::Open { path } => {
            let path = PathBuf::from(path);
            let package = path.extension().and_then(|s| s.to_str()) == Some("fdtheme");
            let source = read(
                &path,
                if package {
                    100 * 1024 * 1024
                } else {
                    1024 * 1024
                },
            )?;
            let mut document = if package {
                ThemePackage::from_toml(&source)?.document
            } else {
                ThemeDocument::from_toml(&source)?
            };
            if !package {
                if let Some(wallpaper) = &mut document.wallpaper.path {
                    let value = PathBuf::from(&*wallpaper);
                    if value.is_relative() {
                        *wallpaper = path
                            .parent()
                            .unwrap()
                            .join(value)
                            .to_string_lossy()
                            .into_owned();
                    }
                }
            }
            describe(
                document,
                Some(FileVersion {
                    path: path.to_string_lossy().into_owned(),
                    hash: hash(&source),
                }),
            )
        }
        FileAction::Save {
            path,
            expected_hash,
            document,
        } => {
            validate(&document)?;
            let path = path_checked(&path, "toml")?;
            let source = document.to_toml()?;
            atomic_save(&path, expected_hash.as_deref(), &source)?;
            describe(
                *document,
                Some(FileVersion {
                    path: path.to_string_lossy().into_owned(),
                    hash: hash(&source),
                }),
            )
        }
        FileAction::Export {
            path,
            expected_hash,
            document,
        } => {
            validate(&document)?;
            let path = path_checked(&path, "fdtheme")?;
            let package = ThemePackage::from_document(&document)?;
            atomic_save(&path, expected_hash.as_deref(), &package.to_toml()?)?;
            describe(*document, None)
        }
        FileAction::Install {
            path,
            expected_hash,
            overwrite,
        } => {
            let source = read(&path_checked(&path, "fdtheme")?, 100 * 1024 * 1024)?;
            if hash(&source) != expected_hash {
                bail!("The package changed. Open it again before installing.");
            }
            let package = ThemePackage::from_toml(&source)?;
            let root = dirs::data_dir()
                .context("The user data directory is unavailable")?
                .join("focaldesk/themes");
            if root.join(&package.slug).exists() && !overwrite {
                bail!("A theme with this package name is already installed. Confirm replacement to continue.");
            }
            let installed = package.install(&root)?;
            file_action(FileAction::Open {
                path: installed.document_path.to_string_lossy().into_owned(),
            })
        }
    }
}
pub fn runtime(command: Option<ThemeEditorCommand>) -> Result<RuntimeStatus> {
    let request = match command {
        Some(command) => IpcRequest::ThemeEditor {
            protocol_version: THEME_EDITOR_PROTOCOL_VERSION,
            command: Box::new(command),
        },
        None => IpcRequest::GetThemeEditorStatus {
            protocol_version: THEME_EDITOR_PROTOCOL_VERSION,
        },
    };
    match send_desktop_request(&request).map_err(anyhow::Error::msg)? {
        IpcResponse::ThemeEditorStatus {protocol_version,preview_active,applied_revision,gradient_rendering,semantic_rendering,contrast_issue_count,..} if protocol_version==THEME_EDITOR_PROTOCOL_VERSION=>Ok(RuntimeStatus {preview_active,applied_revision,gradient_rendering,semantic_rendering,contrast_issue_count}),
        IpcResponse::Error {message}=>bail!(message),
        _=>bail!("The running compositor does not support this theme editor. Rebuild and restart FocalDesk."),
    }
}
pub fn convert_color(color: ThemeColor, space: ThemeColorSpace) -> Result<ThemeColor> {
    if !color.components().iter().all(|v| v.is_finite()) || !(0.0..=1.0).contains(&color.a) {
        bail!("Invalid theme color.");
    }
    Ok(color.converted_to(space))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_loads_wallpaper_and_clears_it_when_removed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preview.png");
        image::RgbImage::from_pixel(2, 2, image::Rgb([30, 80, 140]))
            .save(&path).unwrap();
        let mut document = default_document().unwrap().document;
        document.wallpaper.path = Some(path.to_string_lossy().into_owned());
        assert!(project(&document).unwrap().wallpaper.unwrap().starts_with("data:image/jpeg;base64,"));
        document.wallpaper.path = None;
        assert!(project(&document).unwrap().wallpaper.is_none());
    }

    #[test]
    fn radial_projection_uses_valid_percentage_radii_without_mutating_the_document() {
        let mut document = default_document().unwrap().document;
        let color = ThemeColor::display_p3(1.1, -0.05, 0.2, 0.6);
        document.intent.paint = ThemePaint::RadialGradient {
            center: (0.5, 0.5),
            radius: 0.75,
            interpolation: Default::default(),
            stops: vec![
                focaldesk_themes::GradientStop {
                    position: 0.0,
                    color,
                },
                focaldesk_themes::GradientStop {
                    position: 1.0,
                    color,
                },
            ],
        };
        let before = document.clone();
        assert!(project(&document)
            .unwrap()
            .paint
            .starts_with("radial-gradient(ellipse 75% 75% at 50% 50%"));
        assert_eq!(document, before);
    }
    #[test]
    fn file_roundtrip_stale_writes_and_validation_preserve_sources() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-theme-editor-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("test.toml");
        let document = default_document().unwrap().document;
        let source = document.to_toml().unwrap();
        atomic_save(&path, None, &source).unwrap();
        assert!(atomic_save(&path, None, &source).is_err());
        let expected = hash(&source);
        fs::write(&path, "external edit").unwrap();
        assert!(atomic_save(&path, Some(&expected), &source).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "external edit");
        let mut invalid = document.clone();
        invalid.semantic.layout.bar_height = -1.0;
        assert!(project(&invalid).is_err());
        assert!(file_action(FileAction::Parse { source }).is_ok());
        assert!(path_checked("/tmp/example.sh", "toml").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
