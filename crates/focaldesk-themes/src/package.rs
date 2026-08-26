use crate::ThemeDocument;
use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub const THEME_PACKAGE_VERSION: u32 = 1;
pub const MAX_THEME_ASSET_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThemePackage {
    pub package_version: u32,
    pub slug: String,
    pub document: ThemeDocument,
    #[serde(default)]
    pub assets: Vec<ThemePackageAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemePackageAsset {
    pub path: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTheme {
    pub slug: String,
    pub directory: PathBuf,
    pub document_path: PathBuf,
}

impl ThemePackage {
    pub fn from_document(document: &ThemeDocument) -> anyhow::Result<Self> {
        document.validate()?;
        let slug = theme_slug(&document.name)?;
        let mut packaged_document = document.clone();
        let mut assets = Vec::new();
        if let Some(source) = document.wallpaper.path.as_deref() {
            let source = Path::new(source);
            let extension = supported_wallpaper_extension(source)?;
            let bytes = std::fs::read(source)
                .with_context(|| format!("read wallpaper asset {}", source.display()))?;
            if bytes.len() > MAX_THEME_ASSET_BYTES {
                bail!("wallpaper exceeds the 64 MiB package limit");
            }
            let asset_path = format!("wallpaper.{extension}");
            packaged_document.wallpaper.path = Some(asset_path.clone());
            assets.push(ThemePackageAsset::new(
                asset_path,
                wallpaper_media_type(&extension).to_string(),
                &bytes,
            ));
        }
        let package = Self {
            package_version: THEME_PACKAGE_VERSION,
            slug,
            document: packaged_document,
            assets,
        };
        package.validate()?;
        Ok(package)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.package_version != THEME_PACKAGE_VERSION {
            bail!("unsupported theme package version {}", self.package_version);
        }
        validate_slug(&self.slug)?;
        self.document.validate()?;
        if self.assets.len() > 1 {
            bail!("theme packages currently support one wallpaper asset");
        }
        for asset in &self.assets {
            validate_asset_path(&asset.path)?;
            let bytes = asset.decode()?;
            if asset.size != bytes.len() as u64 {
                bail!("asset size does not match its manifest");
            }
            let digest = format!("{:x}", Sha256::digest(&bytes));
            if digest != asset.sha256 {
                bail!("asset checksum does not match its manifest");
            }
            let (width, height) = image::ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .context("detect wallpaper image format")?
                .into_dimensions()
                .context("read wallpaper image dimensions")?;
            if width == 0
                || height == 0
                || width > 32_768
                || height > 32_768
                || u64::from(width) * u64::from(height) > 100_000_000
            {
                bail!("wallpaper image dimensions exceed package limits");
            }
        }
        match (&self.document.wallpaper.path, self.assets.first()) {
            (Some(path), Some(asset)) if path == &asset.path => {}
            (None, None) => {}
            (Some(_), None) => bail!("package wallpaper asset is missing"),
            (None, Some(_)) => bail!("package contains an unreferenced asset"),
            (Some(_), Some(_)) => bail!("package wallpaper reference does not match its asset"),
        }
        Ok(())
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        self.validate()?;
        toml::to_string_pretty(self).context("serialize theme package")
    }

    pub fn from_toml(source: &str) -> anyhow::Result<Self> {
        let package: Self = toml::from_str(source).context("parse theme package")?;
        package.validate()?;
        Ok(package)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, self.to_toml()?)
            .with_context(|| format!("write theme package {}", path.display()))
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read theme package {}", path.display()))?;
        Self::from_toml(&source)
    }

    pub fn install(&self, themes_root: &Path) -> anyhow::Result<InstalledTheme> {
        self.validate()?;
        std::fs::create_dir_all(themes_root).context("create themes directory")?;
        let directory = themes_root.join(&self.slug);
        refuse_symlink(&directory)?;
        std::fs::create_dir_all(&directory).context("create installed theme directory")?;

        let mut document = self.document.clone();
        if let Some(asset) = self.assets.first() {
            let destination = directory.join(&asset.path);
            refuse_symlink(&destination)?;
            std::fs::write(&destination, asset.decode()?)
                .context("write installed wallpaper asset")?;
            document.wallpaper.path = Some(destination.to_string_lossy().into_owned());
        }
        let document_path = directory.join("theme.toml");
        refuse_symlink(&document_path)?;
        document.save(&document_path)?;
        Ok(InstalledTheme {
            slug: self.slug.clone(),
            directory,
            document_path,
        })
    }

    pub fn uninstall(themes_root: &Path, slug: &str) -> anyhow::Result<()> {
        validate_slug(slug)?;
        let directory = themes_root.join(slug);
        refuse_symlink(&directory)?;
        if directory.exists() {
            std::fs::remove_dir_all(&directory).context("remove installed theme")?;
        }
        Ok(())
    }
}

impl ThemePackageAsset {
    fn new(path: String, media_type: String, bytes: &[u8]) -> Self {
        Self {
            path,
            media_type,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            data_base64: BASE64.encode(bytes),
        }
    }

    fn decode(&self) -> anyhow::Result<Vec<u8>> {
        if self.data_base64.len() > MAX_THEME_ASSET_BYTES * 2 {
            bail!("encoded asset exceeds package limits");
        }
        let bytes = BASE64
            .decode(&self.data_base64)
            .context("decode package asset")?;
        if bytes.len() > MAX_THEME_ASSET_BYTES {
            bail!("decoded asset exceeds the 64 MiB package limit");
        }
        Ok(bytes)
    }
}

pub fn theme_slug(name: &str) -> anyhow::Result<String> {
    let slug = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    validate_slug(&slug)?;
    Ok(slug)
}

fn validate_slug(slug: &str) -> anyhow::Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("theme package slug is invalid");
    }
    Ok(())
}

fn validate_asset_path(path: &str) -> anyhow::Result<()> {
    let candidate = Path::new(path);
    if candidate.components().count() != 1
        || candidate.file_name().and_then(|name| name.to_str()) != Some(path)
        || !path.starts_with("wallpaper.")
    {
        bail!("unsafe theme asset path");
    }
    supported_wallpaper_extension(candidate)?;
    Ok(())
}

fn supported_wallpaper_extension(path: &Path) -> anyhow::Result<String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| anyhow::anyhow!("wallpaper must have a supported image extension"))?;
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" => Ok(if extension == "jpeg" {
            "jpg".to_string()
        } else {
            extension
        }),
        _ => bail!("wallpaper must be PNG, JPEG, or WebP"),
    }
}

fn wallpaper_media_type(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

fn refuse_symlink(path: &Path) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("refusing to install through a symbolic link");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ThemeColor, ThemePaint, ThemePaintIntent, ThemeWallpaper};

    fn document() -> ThemeDocument {
        ThemeDocument::new(
            "Polar Aurora",
            ThemePaintIntent::new(ThemePaint::solid(ThemeColor::srgb(0.2, 0.4, 0.8, 1.0))),
        )
    }

    #[test]
    fn package_round_trip_installs_wallpaper_and_document() {
        let source = tempfile::tempdir().unwrap();
        let wallpaper = source.path().join("sky.png");
        image::RgbaImage::new(1, 1).save(&wallpaper).unwrap();
        let mut document = document();
        document.wallpaper = ThemeWallpaper {
            path: Some(wallpaper.to_string_lossy().into_owned()),
            ..ThemeWallpaper::default()
        };
        let package = ThemePackage::from_document(&document).unwrap();
        let decoded = ThemePackage::from_toml(&package.to_toml().unwrap()).unwrap();
        let install_root = tempfile::tempdir().unwrap();
        let installed = decoded.install(install_root.path()).unwrap();
        assert_eq!(installed.slug, "polar-aurora");
        assert!(installed.directory.join("wallpaper.png").is_file());
        let installed_document = ThemeDocument::load(&installed.document_path).unwrap();
        assert!(Path::new(installed_document.wallpaper.path.as_ref().unwrap()).is_absolute());
    }

    #[test]
    fn package_rejects_traversal_and_checksum_tampering() {
        let mut package = ThemePackage::from_document(&document()).unwrap();
        package.document.wallpaper.path = Some("../escape.png".to_string());
        package.assets.push(ThemePackageAsset::new(
            "../escape.png".to_string(),
            "image/png".to_string(),
            b"bad",
        ));
        assert!(package.validate().is_err());

        let mut package = ThemePackage::from_document(&document()).unwrap();
        package.assets.push(ThemePackageAsset::new(
            "wallpaper.png".to_string(),
            "image/png".to_string(),
            b"data",
        ));
        package.document.wallpaper.path = Some("wallpaper.png".to_string());
        package.assets[0].sha256 = "00".repeat(32);
        assert!(package.validate().is_err());
    }

    #[test]
    fn slug_generation_never_contains_path_components() {
        assert_eq!(theme_slug("  Polar Aurora  ").unwrap(), "polar-aurora");
        assert_eq!(theme_slug("../../Escape").unwrap(), "escape");
        assert!(theme_slug("---").is_err());
    }
}
