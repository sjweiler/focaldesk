//! One-time migration from GNOME Keyring through the Secret Service API.
//!
//! The importer only accepts a service owned by `gnome-keyring-daemon`, uses a
//! plain Secret Service session on the same-user D-Bus transport, never deletes
//! source items, and tags copies with their source object path for idempotency.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{Connection, Proxy};
use zeroize::{Zeroize, Zeroizing};

use crate::store::Store;

const SERVICE_NAME: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const SESSION_IFACE: &str = "org.freedesktop.Secret.Session";
const DBUS_NAME: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_IFACE: &str = "org.freedesktop.DBus";
const NO_PROMPT: &str = "/";
const SOURCE_ATTR: &str = "focald:import-source";

#[derive(Debug, Deserialize, Type)]
struct WireSecret(OwnedObjectPath, Vec<u8>, Vec<u8>, String);

pub struct ImportedItem {
    pub source_path: String,
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub secret: Zeroizing<Vec<u8>>,
    pub content_type: String,
    pub item_type: String,
}

#[derive(Default)]
pub struct Collected {
    pub items: Vec<ImportedItem>,
    pub collections: usize,
    pub locked: usize,
    pub failed: usize,
}

impl Collected {
    pub fn complete(&self) -> bool {
        self.locked == 0 && self.failed == 0
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ImportSummary {
    pub imported: usize,
    pub skipped_existing: usize,
    pub locked: usize,
    pub failed: usize,
}

/// Start the configured Secret Service if needed and verify that its owner is
/// GNOME Keyring. This prevents accidentally re-importing Focaldesk's own
/// service or copying from an unrelated provider.
pub async fn ensure_gnome_owner(connection: &Connection, activate: bool) -> Result<()> {
    let dbus = Proxy::new(connection, DBUS_NAME, DBUS_PATH, DBUS_IFACE).await?;
    let mut owner: Result<String, zbus::Error> = dbus.call("GetNameOwner", &(SERVICE_NAME,)).await;
    if owner.is_err() && activate {
        let _: u32 = dbus
            .call("StartServiceByName", &(SERVICE_NAME, 0_u32))
            .await?;
        owner = dbus.call("GetNameOwner", &(SERVICE_NAME,)).await;
    }
    let owner = owner.context("no Secret Service is available")?;
    let pid: u32 = dbus
        .call("GetConnectionUnixProcessID", &(owner.as_str(),))
        .await
        .context("resolve Secret Service owner process")?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .with_context(|| format!("resolve Secret Service owner pid {pid}"))?;
    let is_gnome = exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.trim_end_matches(" (deleted)") == "gnome-keyring-daemon");
    if !is_gnome {
        bail!(
            "{SERVICE_NAME} is owned by {}, not gnome-keyring-daemon",
            exe.display()
        );
    }
    Ok(())
}

pub async fn collect(connection: &Connection, include_secrets: bool) -> Result<Collected> {
    let service = Proxy::new(connection, SERVICE_NAME, SERVICE_PATH, SERVICE_IFACE).await?;
    let collections: Vec<OwnedObjectPath> = service
        .get_property("Collections")
        .await
        .context("list GNOME Keyring collections")?;

    let session = if include_secrets {
        let input = Value::from("");
        let (_output, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", input))
            .await
            .context("open GNOME Keyring migration session")?;
        Some(session)
    } else {
        None
    };

    let mut collected = Collected {
        collections: collections.len(),
        ..Collected::default()
    };
    for collection_path in collections {
        let collection = Proxy::new(
            connection,
            SERVICE_NAME,
            collection_path.as_str(),
            COLLECTION_IFACE,
        )
        .await?;
        if collection
            .get_property::<bool>("Locked")
            .await
            .unwrap_or(true)
            && !try_unlock(&service, &collection_path).await
        {
            collected.locked += 1;
            continue;
        }
        let items: Vec<OwnedObjectPath> = match collection.get_property("Items").await {
            Ok(items) => items,
            Err(error) => {
                eprintln!(
                    "warning: cannot list GNOME collection {}: {error}",
                    collection_path
                );
                collected.failed += 1;
                continue;
            }
        };
        for item_path in items {
            match collect_item(connection, &item_path, session.as_ref()).await {
                Ok(Some(item)) => collected.items.push(item),
                Ok(None) => collected.locked += 1,
                Err(error) => {
                    eprintln!("warning: cannot import GNOME item {item_path}: {error:#}");
                    collected.failed += 1;
                }
            }
        }
    }

    if let Some(session) = session {
        if let Ok(proxy) =
            Proxy::new(connection, SERVICE_NAME, session.as_str(), SESSION_IFACE).await
        {
            let _: Result<(), _> = proxy.call("Close", &()).await;
        }
    }
    Ok(collected)
}

async fn try_unlock(service: &Proxy<'_>, path: &OwnedObjectPath) -> bool {
    let result: Result<(Vec<OwnedObjectPath>, OwnedObjectPath), zbus::Error> =
        service.call("Unlock", &(vec![path.clone()],)).await;
    matches!(
        result,
        Ok((ref unlocked, ref prompt))
            if unlocked.iter().any(|candidate| candidate == path)
                && prompt.as_str() == NO_PROMPT
    )
}

async fn collect_item(
    connection: &Connection,
    path: &OwnedObjectPath,
    session: Option<&OwnedObjectPath>,
) -> Result<Option<ImportedItem>> {
    let item = Proxy::new(connection, SERVICE_NAME, path.as_str(), ITEM_IFACE).await?;
    if item.get_property::<bool>("Locked").await.unwrap_or(true) {
        return Ok(None);
    }
    let label: String = item.get_property("Label").await.unwrap_or_default();
    let attributes: HashMap<String, String> =
        item.get_property("Attributes").await.unwrap_or_default();
    let item_type: String = item
        .get_property("Type")
        .await
        .unwrap_or_else(|_| "org.freedesktop.Secret.Generic".into());

    let Some(session) = session else {
        return Ok(Some(ImportedItem {
            source_path: path.to_string(),
            label,
            attributes: attributes.into_iter().collect(),
            secret: Zeroizing::new(Vec::new()),
            content_type: String::new(),
            item_type,
        }));
    };
    let (wire,): (WireSecret,) = item
        .call("GetSecret", &(session.clone(),))
        .await
        .context("read item secret")?;
    let WireSecret(wire_session, parameters, secret, content_type) = wire;
    if wire_session != *session || !parameters.is_empty() {
        let mut secret = secret;
        secret.zeroize();
        bail!("GNOME Keyring returned an invalid plain-session secret");
    }
    Ok(Some(ImportedItem {
        source_path: path.to_string(),
        label,
        attributes: attributes.into_iter().collect(),
        secret: Zeroizing::new(secret),
        content_type,
        item_type,
    }))
}

pub fn apply(store: &mut Store, collected: Collected) -> Result<ImportSummary> {
    let mut summary = ImportSummary {
        locked: collected.locked,
        failed: collected.failed,
        ..ImportSummary::default()
    };
    for mut item in collected.items {
        let source = format!("gnome-keyring:{}", item.source_path);
        if store
            .items()
            .iter()
            .any(|existing| existing.attributes.get(SOURCE_ATTR) == Some(&source))
        {
            summary.skipped_existing += 1;
            continue;
        }
        item.attributes.insert(SOURCE_ATTR.into(), source);
        store.create(
            item.label,
            item.attributes,
            std::mem::take(&mut *item.secret),
            item.content_type,
            item.item_type,
            false,
        );
        summary.imported += 1;
    }
    if summary.imported > 0 {
        store.save().context("save imported credentials")?;
    }
    Ok(summary)
}

pub fn data_file() -> Result<PathBuf> {
    std::env::var_os("FOCALD_SECRETS_DB")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .map(|dir| PathBuf::from(dir).join("focaldesk/secrets.db"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/share/focaldesk/secrets.db"))
        })
        .ok_or_else(|| anyhow!("cannot determine Focaldesk data directory"))
}

pub fn marker_file() -> Result<PathBuf> {
    Ok(data_file()?
        .parent()
        .context("credential database has no parent directory")?
        .join("migrations/gnome-keyring-v1.json"))
}

pub fn write_marker(path: &Path, summary: &ImportSummary) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .context("migration marker has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, summary)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{apply, Collected, ImportedItem};
    use crate::store::Store;
    use std::collections::BTreeMap;
    use zeroize::Zeroizing;

    #[test]
    fn apply_is_idempotent_without_heuristic_mapping() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secrets.db");
        let mut store = Store::open(&path, &[7; 32]).unwrap();
        let item = || ImportedItem {
            source_path: "/org/freedesktop/secrets/collection/login/1".into(),
            label: "OpenAI API key".into(),
            attributes: BTreeMap::new(),
            secret: Zeroizing::new(b"test-key".to_vec()),
            content_type: "text/plain".into(),
            item_type: "org.freedesktop.Secret.Generic".into(),
        };
        let first = apply(
            &mut store,
            Collected {
                items: vec![item()],
                ..Collected::default()
            },
        )
        .unwrap();
        let second = apply(
            &mut store,
            Collected {
                items: vec![item()],
                ..Collected::default()
            },
        )
        .unwrap();

        assert_eq!(first.imported, 1);
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped_existing, 1);
        assert!(!store.items()[0].attributes.contains_key("focald:key"));
    }
}
