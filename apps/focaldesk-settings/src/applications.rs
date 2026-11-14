//! Application preferences and XDG associations. No application is launched here.
use crate::preferences::{read_document, reload_message, write_document};
use anyhow::{bail, Context, Result};
use focaldesk_settings_core::{default_settings, AppSettings};
use gio::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Serialize)]
pub struct ApplicationsSnapshot {
    pub apps: AppSettings,
    pub defaults: AppSettings,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationsChange {
    pub expected: AppSettings,
    pub requested: AppSettings,
    pub rule_origins: Vec<Option<usize>>,
    pub startup_origins: Vec<Option<usize>>,
}
#[derive(Serialize)]
pub struct ApplicationsResult {
    pub snapshot: ApplicationsSnapshot,
    pub message: String,
}
#[derive(Clone, Serialize)]
pub struct RegisteredApplication {
    pub id: String,
    pub name: String,
    pub description: String,
    pub executable: String,
    pub recommended: bool,
}
#[derive(Serialize)]
pub struct AssociationSnapshot {
    pub mime_type: String,
    pub description: String,
    pub current: Option<String>,
    pub version: String,
    pub applications: Vec<RegisteredApplication>,
}
#[derive(Serialize)]
pub struct AssociationResult {
    pub snapshot: AssociationSnapshot,
    pub message: String,
}
#[derive(Default)]
pub struct ApplicationsService {
    operation: Mutex<()>,
}

fn apps(document: &Value) -> Result<AppSettings> {
    document
        .get("apps")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("Application settings are malformed; nothing was changed.")
        .map(|value| value.unwrap_or_else(|| default_settings().apps))
}
fn valid_command(value: &str, empty: bool) -> bool {
    (empty || !value.is_empty())
        && value.trim() == value
        && value.chars().count() <= 1024
        && !value.chars().any(char::is_control)
}
fn valid_origins(origins: &[Option<usize>], new_len: usize, old_len: usize) -> bool {
    let existing = origins.iter().flatten().copied().collect::<Vec<_>>();
    origins.len() == new_len
        && existing.iter().all(|index| *index < old_len)
        && existing.iter().collect::<BTreeSet<_>>().len() == existing.len()
}
fn validate(change: &ApplicationsChange) -> Result<()> {
    let requested = &change.requested;
    let expected = &change.expected;
    for (label, value, previous, empty) in [
        ("Terminal", &requested.terminal, &expected.terminal, false),
        ("Browser", &requested.browser, &expected.browser, false),
        (
            "File manager",
            &requested.file_manager,
            &expected.file_manager,
            false,
        ),
        ("Email", &requested.email, &expected.email, true),
    ] {
        if value != previous && !valid_command(value, empty) {
            bail!("{label}: enter an executable name or path without control characters or surrounding whitespace.");
        }
    }
    if requested.window_rules.len() > 64 || requested.startup_apps.len() > 64 {
        bail!("Use at most 64 window rules and 64 startup applications.");
    }
    if !valid_origins(
        &change.rule_origins,
        requested.window_rules.len(),
        expected.window_rules.len(),
    ) || !valid_origins(
        &change.startup_origins,
        requested.startup_apps.len(),
        expected.startup_apps.len(),
    ) {
        bail!("Invalid application row identity. Reload before applying.");
    }
    for (rule, origin) in requested.window_rules.iter().zip(&change.rule_origins) {
        if origin.and_then(|index| expected.window_rules.get(index)) == Some(rule) {
            continue;
        }
        if rule.clone().sanitized().as_ref() != Some(rule)
            || rule.workspace.is_some_and(|workspace| workspace > 99)
            || [&rule.app_id, &rule.title_contains]
                .iter()
                .any(|value| value.chars().any(char::is_control))
            || rule
                .output
                .as_ref()
                .is_some_and(|output| output.chars().any(char::is_control))
        {
            bail!("Each window rule needs an application ID or title, valid text, and an optional workspace from 1 to 99.");
        }
    }
    for (startup, origin) in requested.startup_apps.iter().zip(&change.startup_origins) {
        if origin.and_then(|index| expected.startup_apps.get(index)) == Some(startup) {
            continue;
        }
        if startup.clone().sanitized().as_ref() != Some(startup)
            || !valid_command(&startup.command, false)
            || startup.name.chars().any(char::is_control)
            || startup
                .arguments
                .iter()
                .any(|value| value.chars().any(char::is_control))
        {
            bail!("Each startup entry needs an executable and at most 32 nonempty arguments. Use one argument per entry, without surrounding whitespace or control characters.");
        }
    }
    Ok(())
}
fn merge_rows<T: Serialize>(
    old: Option<&Value>,
    rows: &[T],
    origins: &[Option<usize>],
) -> Result<Value> {
    let old = old.and_then(Value::as_array);
    rows.iter()
        .zip(origins)
        .map(|(row, origin)| {
            let mut value = origin
                .and_then(|index| old.and_then(|old| old.get(index)))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            value
                .as_object_mut()
                .context("Application row is malformed")?
                .extend(
                    serde_json::to_value(row)?
                        .as_object()
                        .context("Application row is malformed")?
                        .clone(),
                );
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}
fn save_at(path: &Path, change: &ApplicationsChange) -> Result<AppSettings> {
    validate(change)?;
    let mut document = read_document(path)?;
    if apps(&document)? != change.expected {
        bail!("Application settings changed in another client. Reload before applying.");
    }
    let target = document
        .as_object_mut()
        .unwrap()
        .entry("apps")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("Applications settings are malformed")?;
    let rules = merge_rows(
        target.get("window_rules"),
        &change.requested.window_rules,
        &change.rule_origins,
    )?;
    let startup = merge_rows(
        target.get("startup_apps"),
        &change.requested.startup_apps,
        &change.startup_origins,
    )?;
    target.extend(
        serde_json::to_value(&change.requested)?
            .as_object()
            .unwrap()
            .clone(),
    );
    target.insert("window_rules".into(), rules);
    target.insert("startup_apps".into(), startup);
    write_document(path, &document)?;
    Ok(change.requested.clone())
}
fn mime(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_lowercase();
    let Some((category, subtype)) = value.split_once('/') else {
        bail!("Enter a MIME type such as application/pdf or x-scheme-handler/https.");
    };
    let token = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    if value.len() > 255 || !token(category) || !token(subtype) {
        bail!("Invalid MIME type or URL scheme.");
    }
    if category == "x-scheme-handler"
        && (!subtype.starts_with(|ch: char| ch.is_ascii_alphabetic())
            || !subtype
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b)))
    {
        bail!("Invalid URL scheme.");
    }
    Ok(value)
}
fn preference_paths() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(config) = dirs::config_dir() {
        directories.push(config);
    }
    directories.extend(std::env::split_paths(
        &std::env::var_os("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into()),
    ));
    if let Some(data) = dirs::data_dir() {
        directories.push(data.join("applications"));
    }
    directories.extend(
        std::env::split_paths(
            &std::env::var_os("XDG_DATA_DIRS")
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
        )
        .map(|path| path.join("applications")),
    );
    let desktops = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .map(|name| name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    directories
        .into_iter()
        .flat_map(|directory| {
            desktops
                .iter()
                .map(|name| directory.join(format!("{name}-mimeapps.list")))
                .chain(std::iter::once(directory.join("mimeapps.list")))
                .collect::<Vec<_>>()
        })
        .collect()
}
fn association_version() -> Result<String> {
    let paths = preference_paths();
    if paths.len() > 128 {
        bail!("Too many XDG preference locations.");
    }
    let mut hash = Sha256::new();
    for path in paths {
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        if fs::metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
            bail!("A MIME preferences path is not a regular file.");
        }
        match fs::File::open(&path) {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    bail!("A MIME preferences path is not a regular file.");
                }
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 1024 * 1024 {
                    bail!("A MIME preferences file exceeds 1 MiB.");
                }
                hash.update([1]);
                hash.update(bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hash.update([2]),
            Err(error) => return Err(error.into()),
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn association_snapshot(mime_type: &str) -> Result<AssociationSnapshot> {
    let mime_type = mime(mime_type)?;
    let version = association_version()?;
    let scheme = mime_type.starts_with("x-scheme-handler/");
    let current = gio::AppInfo::default_for_type(&mime_type, scheme)
        .and_then(|app| app.id())
        .map(|id| id.to_string());
    let recommended = gio::AppInfo::all_for_type(&mime_type)
        .into_iter()
        .filter_map(|app| app.id())
        .map(|id| id.to_string())
        .collect::<BTreeSet<_>>();
    let mut applications = gio::AppInfo::all()
        .into_iter()
        .filter_map(|app| {
            let id = app.id()?.to_string();
            if !id.ends_with(".desktop") || (!app.should_show() && current.as_deref() != Some(&id))
            {
                return None;
            }
            if !recommended.contains(&id)
                && !(if scheme {
                    app.supports_uris()
                } else {
                    app.supports_files() || app.supports_uris()
                })
            {
                return None;
            }
            Some(RegisteredApplication {
                recommended: recommended.contains(&id),
                id,
                name: app.display_name().to_string(),
                description: app
                    .description()
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                executable: app.executable().to_string_lossy().into_owned(),
            })
        })
        .collect::<Vec<_>>();
    applications.sort_by(|a, b| {
        b.recommended
            .cmp(&a.recommended)
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    applications.dedup_by(|a, b| a.id == b.id);
    if association_version()? != version {
        bail!("File associations changed while loading. Refresh before continuing.");
    }
    Ok(AssociationSnapshot {
        description: gio::content_type_get_description(&mime_type).to_string(),
        mime_type,
        current,
        version,
        applications,
    })
}
impl ApplicationsService {
    pub fn snapshot(&self) -> Result<ApplicationsSnapshot> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| anyhow::anyhow!("Application controls unavailable"))?;
        Ok(ApplicationsSnapshot {
            apps: apps(&read_document(&focaldesk_settings_core::settings_path())?)?,
            defaults: default_settings().apps,
        })
    }
    pub fn apply(&self, change: ApplicationsChange) -> Result<ApplicationsResult> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| anyhow::anyhow!("Application controls unavailable"))?;
        let apps = save_at(&focaldesk_settings_core::settings_path(), &change)?;
        Ok(ApplicationsResult {
            snapshot: ApplicationsSnapshot {
                apps,
                defaults: default_settings().apps,
            },
            message: format!(
                "{} Window rules affect new windows; startup entries run at the next sign-in.",
                reload_message()
            ),
        })
    }
    pub fn association(&self, mime_type: String) -> Result<AssociationSnapshot> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| anyhow::anyhow!("Application controls unavailable"))?;
        association_snapshot(&mime_type)
    }
    pub fn set_association(
        &self,
        mime_type: String,
        expected: Option<String>,
        version: String,
        desktop_id: String,
    ) -> Result<AssociationResult> {
        let _guard = self
            .operation
            .lock()
            .map_err(|_| anyhow::anyhow!("Application controls unavailable"))?;
        let current = association_snapshot(&mime_type)?;
        if current.current != expected || current.version != version {
            bail!("File associations changed in another client. Reload before applying.");
        }
        if !current.applications.iter().any(|app| app.id == desktop_id) {
            bail!("Choose an installed application from the available list.");
        }
        let app = gio::DesktopAppInfo::new(&desktop_id)
            .context("This application disappeared. Reload the application list.")?;
        if !app.filename().is_some_and(|path| path.is_file()) {
            bail!("This application is no longer installed. Reload the application list.");
        }
        app.set_as_default_for_type(&current.mime_type)
            .context("Could not save the file association")?;
        let snapshot = association_snapshot(&current.mime_type)?;
        let message = if snapshot.current.as_deref() == Some(&desktop_id) {
            "Default application updated for this file or link type."
        } else {
            "Preference saved, but another desktop-specific preference remains active. Check your MIME preference overrides."
        };
        Ok(AssociationResult {
            snapshot,
            message: message.into(),
        })
    }
    pub fn content_types(&self) -> Vec<String> {
        let mut values = gio::content_types_get_registered()
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        values.sort();
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_mime_types_and_rejects_ambiguous_row_origins() {
        for value in [
            "application/pdf",
            "text/plain",
            "application/vnd.example+json",
            "x-scheme-handler/https",
        ] {
            assert!(mime(value).is_ok());
        }
        for value in [
            "https://example.com",
            "text/*",
            "text/plain\nrun",
            "x-scheme-handler/a_bad",
        ] {
            assert!(mime(value).is_err());
        }
        assert!(!valid_origins(&[Some(0), Some(0)], 2, 1));
    }
    #[test]
    fn apps_transaction_preserves_unknown_row_fields_through_edits_and_reordering() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-applications-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        let mut expected = default_settings().apps;
        expected.window_rules = vec![
            focaldesk_settings_core::WindowRule {
                app_id: "first".into(),
                ..Default::default()
            },
            focaldesk_settings_core::WindowRule {
                app_id: "second".into(),
                ..Default::default()
            },
        ];
        let mut document =
            serde_json::json!({"apps":expected,"displays":{"keep":true},"privacy":{"keep":42}});
        document["apps"]["future"] = true.into();
        document["apps"]["window_rules"][0]["future_rule"] = "first metadata".into();
        document["apps"]["window_rules"][1]["future_rule"] = "second metadata".into();
        fs::write(&path, document.to_string()).unwrap();
        let mut requested = expected.clone();
        requested.window_rules.swap(0, 1);
        requested.window_rules[0].workspace = Some(3);
        let change = ApplicationsChange {
            expected: expected.clone(),
            requested,
            rule_origins: vec![Some(1), Some(0)],
            startup_origins: vec![],
        };
        save_at(&path, &change).unwrap();
        let saved = read_document(&path).unwrap();
        assert_eq!(
            saved["apps"]["window_rules"][0]["future_rule"],
            "second metadata"
        );
        assert_eq!(saved["apps"]["future"], true);
        assert_eq!(saved["privacy"], document["privacy"]);
        assert_eq!(saved["displays"], document["displays"]);
        let bytes = fs::read(&path).unwrap();
        assert!(save_at(&path, &change).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let mut invalid = ApplicationsChange {
            expected: expected.clone(),
            requested: expected,
            rule_origins: vec![Some(0), Some(1)],
            startup_origins: vec![],
        };
        invalid
            .requested
            .startup_apps
            .push(focaldesk_settings_core::StartupApp::default());
        invalid.startup_origins.push(None);
        assert!(validate(&invalid).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
