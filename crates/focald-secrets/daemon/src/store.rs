//! Encrypted on-disk secret store.
//!
//! Format: `b"FSDB1" || 12-byte nonce || ChaCha20-Poly1305(ciphertext of JSON)`.
//! The 32-byte master key is provisioned externally (normally as a private
//! systemd service credential). Writes are atomic
//! (tempfile + fsync + rename). Secret values are zeroized on drop.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 5] = b"FSDB1";
const MAX_DB_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_STORE_ITEMS: usize = 4096;

fn validate_private_file(
    file: &std::fs::File,
    path: &Path,
    systemd_credential: bool,
) -> std::io::Result<std::fs::Metadata> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let expected_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || (!systemd_credential && (metadata.uid() != expected_uid || metadata.mode() & 0o077 != 0))
    {
        return Err(std::io::Error::other(format!(
            "{} must be {}",
            path.display(),
            if systemd_credential {
                "a regular systemd credential file".to_string()
            } else {
                format!("a regular file owned by uid {expected_uid} with no group/other access")
            }
        )));
    }
    Ok(metadata)
}

fn read_private_file_with_owner(
    path: &Path,
    maximum: u64,
    systemd_credential: bool,
) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = validate_private_file(&file, path, systemd_credential)?;
    if metadata.len() > maximum {
        return Err(std::io::Error::other(format!(
            "{} exceeds the {} byte size limit",
            path.display(),
            maximum
        )));
    }
    let mut data = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    std::io::Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 != metadata.len() {
        return Err(std::io::Error::other(format!(
            "{} changed size while being read",
            path.display()
        )));
    }
    Ok(data)
}

fn read_private_file(path: &Path, maximum: u64) -> std::io::Result<Zeroizing<Vec<u8>>> {
    read_private_file_with_owner(path, maximum, false)
}

/// Copy a legacy encrypted store into a new systemd-managed state directory.
///
/// The source is deliberately retained as a recovery copy. Migration only
/// runs when the destination is absent, and publishes through an atomic rename
/// so an interrupted first start cannot leave a partial database.
pub fn migrate_legacy_store(source: &Path, destination: &Path) -> std::io::Result<bool> {
    if destination.exists() {
        return Ok(false);
    }
    let encrypted = match read_private_file(source, MAX_DB_BYTES) {
        Ok(encrypted) => encrypted,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let directory = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("secrets database has no parent directory"))?;
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    let temporary = destination.with_extension(format!("migrate.{:016x}", rand::random::<u64>()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&encrypted)?;
        file.sync_all()?;
        std::fs::rename(&temporary, destination)?;
        std::fs::File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map(|()| true)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    #[serde(with = "b64")]
    pub secret: Vec<u8>,
    pub content_type: String,
    #[serde(default = "default_item_type")]
    pub item_type: String,
    pub created: u64,
    pub modified: u64,
}

fn default_item_type() -> String {
    "org.freedesktop.Secret.Generic".to_string()
}

impl Drop for Item {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

/// base64 (de)serialization for the secret bytes so the JSON stays sane.
mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    use zeroize::{Zeroize, Zeroizing};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(v));
        s.serialize_str(&encoded)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let mut encoded = String::deserialize(d)?;
        let result = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .map_err(serde::de::Error::custom);
        encoded.zeroize();
        result
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Db {
    next_id: u64,
    items: Vec<Item>,
}

pub struct Store {
    path: PathBuf,
    key: chacha20poly1305::Key,
    db: Db,
    _lock: std::fs::File,
}

pub struct StoreTransaction<'a> {
    db: &'a mut Db,
}

impl StoreTransaction<'_> {
    #[allow(dead_code)]
    pub fn items(&self) -> &[Item] {
        &self.db.items
    }

    pub fn search(&self, attrs: &BTreeMap<String, String>) -> Vec<u64> {
        self.db
            .items
            .iter()
            .filter(|item| {
                attrs
                    .iter()
                    .all(|(key, value)| item.attributes.get(key) == Some(value))
            })
            .map(|item| item.id)
            .collect()
    }

    pub fn create(
        &mut self,
        label: String,
        attributes: BTreeMap<String, String>,
        mut secret: Vec<u8>,
        content_type: String,
        item_type: String,
        replace: bool,
    ) -> std::io::Result<(u64, Vec<u64>)> {
        let replaced = if replace {
            self.db
                .items
                .iter()
                .filter(|item| item.attributes == attributes)
                .map(|item| item.id)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if self.db.items.len() - replaced.len() >= MAX_STORE_ITEMS {
            secret.zeroize();
            return Err(std::io::Error::other(format!(
                "secrets store item limit ({MAX_STORE_ITEMS}) reached"
            )));
        }
        if !replaced.is_empty() {
            self.db.items.retain(|item| !replaced.contains(&item.id));
        }
        let id = self.db.next_id;
        self.db.next_id = match self.db.next_id.checked_add(1) {
            Some(next_id) => next_id,
            None => {
                secret.zeroize();
                return Err(std::io::Error::other("secrets store item id exhausted"));
            }
        };
        let timestamp = now_secs();
        self.db.items.push(Item {
            id,
            label,
            attributes,
            secret,
            content_type,
            item_type,
            created: timestamp,
            modified: timestamp,
        });
        Ok((id, replaced))
    }

    pub fn set_secret(&mut self, id: u64, secret: Vec<u8>, content_type: String) -> bool {
        if let Some(item) = self.db.items.iter_mut().find(|item| item.id == id) {
            item.secret.zeroize();
            item.secret = secret;
            item.content_type = content_type;
            item.modified = now_secs();
            true
        } else {
            false
        }
    }

    pub fn set_attributes(&mut self, id: u64, attributes: BTreeMap<String, String>) -> bool {
        if let Some(item) = self.db.items.iter_mut().find(|item| item.id == id) {
            item.attributes = attributes;
            item.modified = now_secs();
            true
        } else {
            false
        }
    }

    pub fn set_label(&mut self, id: u64, label: String) -> bool {
        if let Some(item) = self.db.items.iter_mut().find(|item| item.id == id) {
            item.label = label;
            item.modified = now_secs();
            true
        } else {
            false
        }
    }

    pub fn delete(&mut self, id: u64) -> bool {
        let before = self.db.items.len();
        self.db.items.retain(|item| item.id != id);
        self.db.items.len() != before
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.key.as_mut_slice().zeroize();
    }
}

impl Store {
    pub fn open(path: &Path, key_bytes: &[u8; 32]) -> std::io::Result<Self> {
        let lock_path = path.with_extension("lock");
        if let Some(dir) = lock_path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&lock_path)?;
        validate_private_file(&lock, &lock_path, false)?;
        // The daemon and migration tools must never mutate the JSON store at
        // the same time. Non-blocking failure is safer than a lost update.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("secrets store is already in use: {}", path.display()),
            ));
        }
        let key = chacha20poly1305::Key::from_slice(key_bytes).to_owned();
        let db = match read_private_file(path, MAX_DB_BYTES) {
            Ok(raw) => Self::decrypt(&key, &raw).map_err(std::io::Error::other)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Db {
                next_id: 1,
                items: Vec::new(),
            },
            Err(e) => return Err(e),
        };
        if db.items.len() > MAX_STORE_ITEMS {
            return Err(std::io::Error::other(format!(
                "secrets store contains {} items; limit is {MAX_STORE_ITEMS}",
                db.items.len()
            )));
        }
        Ok(Store {
            path: path.to_owned(),
            key,
            db,
            _lock: lock,
        })
    }

    fn decrypt(key: &chacha20poly1305::Key, raw: &[u8]) -> Result<Db, String> {
        if raw.len() < MAGIC.len() + 12 || &raw[..MAGIC.len()] != MAGIC {
            return Err("secrets.db: bad magic (not a focald-secrets store)".into());
        }
        let nonce = Nonce::from_slice(&raw[5..17]);
        let cipher = ChaCha20Poly1305::new(key);
        let mut plain = cipher
            .decrypt(nonce, &raw[17..])
            .map_err(|_| "secrets.db: decryption failed (wrong key or corrupt file)".to_string())?;
        let db = serde_json::from_slice(&plain).map_err(|e| e.to_string());
        plain.zeroize();
        db
    }

    fn persist_db(&self, db: &Db) -> std::io::Result<()> {
        let mut plain = serde_json::to_vec(db)?;
        let encrypted_len = MAGIC.len() + 12 + plain.len() + 16;
        if encrypted_len > MAX_DB_BYTES as usize {
            plain.zeroize();
            return Err(std::io::Error::other(format!(
                "secrets store exceeds the {MAX_DB_BYTES} byte encrypted size limit"
            )));
        }
        let cipher = ChaCha20Poly1305::new(&self.key);
        let mut nonce_bytes = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, plain.as_slice())
            .map_err(|e| std::io::Error::other(format!("encrypt: {e}")))?;
        plain.zeroize();
        drop(plain);

        if let Some(dir) = self.path.parent() {
            if !dir.exists() {
                std::fs::create_dir_all(dir)?;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let tmp = self
            .path
            .with_extension(format!("tmp.{:016x}", rand::random::<u64>()));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .mode(0o600)
                .open(&tmp)?;
            // Stream the fixed header and ciphertext directly instead of
            // retaining a second full encrypted-store buffer in locked memory.
            f.write_all(MAGIC)?;
            f.write_all(&nonce_bytes)?;
            f.write_all(&ct)?;
            f.sync_all()?;
        }
        if let Err(error) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        if let Some(dir) = self.path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.persist_db(&self.db)
    }

    /// Apply a mutation to a private candidate database, persist it atomically,
    /// and publish it in memory only after persistence succeeds. Failed
    /// mutations therefore cannot leak into a later successful save.
    pub fn transaction<T>(
        &mut self,
        mutate: impl FnOnce(&mut StoreTransaction<'_>) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let mut candidate = self.db.clone();
        let result = {
            let mut transaction = StoreTransaction { db: &mut candidate };
            mutate(&mut transaction)?
        };
        self.persist_db(&candidate)?;
        self.db = candidate;
        Ok(result)
    }

    pub fn items(&self) -> &[Item] {
        &self.db.items
    }

    pub fn get(&self, id: u64) -> Option<&Item> {
        self.db.items.iter().find(|i| i.id == id)
    }

    /// Exact-match search: item must contain every requested attribute pair.
    pub fn search(&self, attrs: &BTreeMap<String, String>) -> Vec<u64> {
        self.db
            .items
            .iter()
            .filter(|i| attrs.iter().all(|(k, v)| i.attributes.get(k) == Some(v)))
            .map(|i| i.id)
            .collect()
    }
}

/// Validate and deserialize an encrypted store image without opening a path.
/// Used by fuzzing and recovery tooling to exercise the production parser.
pub fn validate_encrypted_store(key_bytes: &[u8; 32], raw: &[u8]) -> Result<(), String> {
    let key = chacha20poly1305::Key::from_slice(key_bytes).to_owned();
    let db = Store::decrypt(&key, raw)?;
    if db.items.len() > MAX_STORE_ITEMS {
        return Err(format!(
            "secrets store contains {} items; limit is {MAX_STORE_ITEMS}",
            db.items.len()
        ));
    }
    Ok(())
}

/// Load the 32-byte master key. Preference order:
///  1. $FOCALD_SECRETS_KEYFILE (explicit testing/recovery override)
///  2. $CREDENTIALS_DIRECTORY/focald.master (production)
///  3. the legacy runtime handoff, but only with
///     $FOCALD_SECRETS_ALLOW_LEGACY_HANDOFF=1
///
/// The key must be provisioned by PAM/systemd or by an explicit operator
/// override. The daemon never invents a replacement: doing so when an
/// encrypted database exists would create an invalid session state.
pub fn load_master_key() -> std::io::Result<Zeroizing<[u8; 32]>> {
    let path = master_key_path()?;
    // The credential is in a PID-1-created private mount whose ownership and
    // mode presentation varies with systemd's mount/idmap implementation.
    // Relax ordinary ownership/mode checks only for that exact source;
    // O_NOFOLLOW, regular-file type, and exact size remain enforced.
    let systemd_credential = std::env::var_os("FOCALD_SECRETS_KEYFILE").is_none()
        && std::env::var_os("CREDENTIALS_DIRECTORY").is_some();

    match read_private_file_with_owner(&path, 32, systemd_credential) {
        Ok(raw) if raw.len() == 32 => {
            let mut k = Zeroizing::new([0u8; 32]);
            k.copy_from_slice(&raw);
            Ok(k)
        }
        Ok(_) => Err(std::io::Error::other(format!(
            "{}: key file must be exactly 32 bytes",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "master-key credential is absent at {}; unlock through the PAM-managed system service",
                path.display()
            ),
        )),
        Err(error) => Err(error),
    }
}

pub fn master_key_path() -> std::io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("FOCALD_SECRETS_KEYFILE") {
        return Ok(PathBuf::from(path));
    }
    if let Some(directory) = std::env::var_os("CREDENTIALS_DIRECTORY") {
        return Ok(PathBuf::from(directory).join("focald.master"));
    }
    if std::env::var_os("FOCALD_SECRETS_ALLOW_LEGACY_HANDOFF").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        return std::env::var_os("XDG_RUNTIME_DIR")
            .map(|runtime| PathBuf::from(runtime).join("focaldesk/secrets.key"))
            .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is not set"));
    }
    Err(std::io::Error::other(
        "no systemd service credential; FOCALD_SECRETS_KEYFILE is only for explicit testing/recovery",
    ))
}

/// Remove an explicitly enabled legacy runtime key after opening the database.
/// systemd credentials and operator-owned overrides are never removed here.
pub fn consume_runtime_master_key() -> std::io::Result<bool> {
    if std::env::var_os("FOCALD_SECRETS_KEYFILE").is_some()
        || std::env::var_os("CREDENTIALS_DIRECTORY").is_some()
        || std::env::var_os("FOCALD_SECRETS_ALLOW_LEGACY_HANDOFF").as_deref()
            != Some(std::ffi::OsStr::new("1"))
    {
        return Ok(false);
    }
    let path = master_key_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{migrate_legacy_store, read_private_file, Store, MAX_STORE_ITEMS};
    use std::os::unix::fs::{symlink, OpenOptionsExt};

    #[test]
    fn encrypted_store_roundtrips_with_private_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secrets.db");
        let key = [7u8; 32];
        {
            let mut store = Store::open(&path, &key).unwrap();
            store
                .transaction(|transaction| {
                    transaction.create(
                        "test".into(),
                        Default::default(),
                        b"secret".to_vec(),
                        "text/plain".into(),
                        "org.freedesktop.Secret.Generic".into(),
                        false,
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let store = Store::open(&path, &key).unwrap();
        assert_eq!(store.items()[0].secret, b"secret");
    }

    #[test]
    fn private_read_rejects_open_permissions_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let open_path = directory.path().join("open");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&open_path)
            .unwrap();
        assert!(read_private_file(&open_path, 32).is_err());

        let link_path = directory.path().join("link");
        symlink("/etc/passwd", &link_path).unwrap();
        assert!(read_private_file(&link_path, 1024).is_err());
    }

    #[test]
    fn legacy_store_migration_is_atomic_and_non_destructive() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("legacy/secrets.db");
        let destination = directory.path().join("state/secrets.db");
        let key = [9u8; 32];
        {
            let mut store = Store::open(&source, &key).unwrap();
            store
                .transaction(|transaction| {
                    transaction.create(
                        "migrated".into(),
                        Default::default(),
                        b"secret".to_vec(),
                        "text/plain".into(),
                        "org.freedesktop.Secret.Generic".into(),
                        false,
                    )?;
                    Ok(())
                })
                .unwrap();
        }

        assert!(migrate_legacy_store(&source, &destination).unwrap());
        assert!(source.exists());
        assert_eq!(
            Store::open(&destination, &key).unwrap().items()[0].label,
            "migrated"
        );
        assert!(!migrate_legacy_store(&source, &destination).unwrap());
    }

    #[test]
    fn failed_persistence_rolls_back_in_memory() {
        let directory = tempfile::tempdir().unwrap();
        let store_directory = directory.path().join("active");
        let moved_directory = directory.path().join("moved");
        let path = store_directory.join("secrets.db");
        let key = [11u8; 32];
        let mut store = Store::open(&path, &key).unwrap();
        store
            .transaction(|transaction| {
                transaction.create(
                    "original".into(),
                    Default::default(),
                    b"secret".to_vec(),
                    "text/plain".into(),
                    "org.freedesktop.Secret.Generic".into(),
                    false,
                )?;
                Ok(())
            })
            .unwrap();

        std::fs::rename(&store_directory, &moved_directory).unwrap();
        std::fs::File::create(&store_directory).unwrap();
        assert!(store
            .transaction(|transaction| {
                transaction.set_label(1, "must-not-stick".into());
                Ok(())
            })
            .is_err());
        assert_eq!(store.get(1).unwrap().label, "original");

        std::fs::remove_file(&store_directory).unwrap();
        std::fs::rename(&moved_directory, &store_directory).unwrap();
        drop(store);
        assert_eq!(
            Store::open(&path, &key).unwrap().get(1).unwrap().label,
            "original"
        );
    }

    #[test]
    fn item_limit_is_enforced_before_commit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secrets.db");
        let mut store = Store::open(&path, &[13; 32]).unwrap();
        store
            .transaction(|transaction| {
                for index in 0..MAX_STORE_ITEMS {
                    transaction.create(
                        format!("item-{index}"),
                        Default::default(),
                        Vec::new(),
                        "text/plain".into(),
                        "org.freedesktop.Secret.Generic".into(),
                        false,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        assert!(store
            .transaction(|transaction| {
                transaction.create(
                    "one-too-many".into(),
                    Default::default(),
                    Vec::new(),
                    "text/plain".into(),
                    "org.freedesktop.Secret.Generic".into(),
                    false,
                )?;
                Ok(())
            })
            .is_err());
        assert_eq!(store.items().len(), MAX_STORE_ITEMS);
    }
}
