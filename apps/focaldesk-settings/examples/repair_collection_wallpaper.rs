//! Audit all installed wallpapers and repair exact legacy collection installs.
//! Defaults to a read-only plan; --all includes every installed theme.
use anyhow::{bail, Context, Result};
use focaldesk_themes::ThemeDocument;
use std::{io::Write, path::PathBuf, process::Command};

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().collect();
    let apply = arguments.iter().any(|argument| argument == "--apply");
    let all = arguments.iter().any(|argument| argument == "--all");
    let selected = focaldesk_config::load_config_read_only().appearance.theme;
    let root = dirs::data_dir()
        .context("No data directory")?
        .join("focaldesk/themes");
    let mut paths: Vec<PathBuf> = if all {
        std::fs::read_dir(&root)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path().join("theme.toml")))
            .filter(|path| path.is_file())
            .collect()
    } else {
        vec![PathBuf::from(&selected)]
    };
    paths.sort();
    let packages = focaldesk_settings::appearance::collection_packages()?;
    let mut repairs = Vec::new();
    let mut unresolved = Vec::new();
    for path in paths {
        let document = ThemeDocument::load(&path)?;
        let image = document.wallpaper.path.as_ref().map(|image| {
            let image = PathBuf::from(image);
            if image.is_absolute() {
                image
            } else {
                path.parent().unwrap().join(image)
            }
        });
        if let Some(image) = image.as_ref() {
            if let Ok(decoded) = image::open(image) {
                let (width, height) = (decoded.width(), decoded.height());
                if width > 0 && height > 0 {
                    println!(
                        "VALID: {} — {}x{} — {}",
                        document.name,
                        width,
                        height,
                        image.display()
                    );
                    continue;
                }
            }
        }
        let mut legacy = document.clone();
        legacy.wallpaper.path = None;
        let package = packages.iter().find(|package| {
            let mut original = package.document.clone();
            original.wallpaper.path = None;
            path == root.join(&package.slug).join("theme.toml") && legacy == original
        });
        if let Some(package) = package {
            println!(
                "REPAIR: {} — install its bundled wallpaper; preserve colors and preferences",
                document.name
            );
            repairs.push((path, document, package));
        } else {
            println!("UNRESOLVED: {} — missing/unreadable wallpaper in a customized theme; left untouched", document.name);
            unresolved.push(document.name);
        }
    }
    println!(
        "{} repairs planned; {} customized themes need attention",
        repairs.len(),
        unresolved.len()
    );
    if !apply {
        return Ok(());
    }
    let affects_selected = repairs
        .iter()
        .any(|(path, _, _)| path == std::path::Path::new(&selected));
    let cli = "/usr/local/bin/focaldesk-cli";
    if affects_selected {
        let status = Command::new(cli).arg("desktop-snapshot").output()?;
        if !status.status.success() {
            bail!(
                "Cannot query compositor: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
    }
    for (path, original, package) in repairs {
        if focaldesk_config::load_config_read_only().appearance.theme != selected
            || ThemeDocument::load(&path)? != original
        {
            bail!("Theme changed while preparing repair; reload before retrying");
        }
        let backup = path.with_file_name(format!(
            "theme.before-wallpaper-repair-{}.toml",
            std::process::id()
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?
            .write_all(&std::fs::read(&path)?)?;
        let installed = package.install(&root)?;
        let repaired = ThemeDocument::load(&installed.document_path)?;
        let image = repaired
            .wallpaper
            .path
            .as_ref()
            .context("Repaired document has no wallpaper")?;
        image::open(image).context("decode repaired wallpaper")?;
        println!(
            "REPAIRED: {}; original document backup: {}",
            repaired.name,
            backup.display()
        );
    }
    if affects_selected {
        let result = Command::new(cli).arg("reload-settings").output()?;
        if !result.status.success() {
            bail!(
                "Wallpapers repaired on disk, but live theme reload failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        println!("Current theme reloaded; monitor modes unchanged");
    }
    if !unresolved.is_empty() {
        bail!(
            "Customized themes left unchanged: {}",
            unresolved.join(", ")
        );
    }
    Ok(())
}
