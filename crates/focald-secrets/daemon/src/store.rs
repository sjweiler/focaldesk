//! Encrypted on-disk secret store.
//!
//! Format: `b"FSDB1" || 12-byte nonce || ChaCha20-Poly1305(ciphertext of JSON)`.
//! The 32-byte master key is provisioned externally (PAM at login, or a
//! 0600 key file in $XDG_RUNTIME_DIR for bootstrap). Writes are atomic
//! (tempfile + fsync + rename). Secret values are zeroized on drop.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

const MAGIC: &[u8; 5] = b"FSDB1";

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize, Deserialize)]
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
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Default)]
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
            .mode(0o600)
            .open(&lock_path)?;
        // The daemon and migration tools must never mutate the JSON store at
        // the same time. Non-blocking failure is safer than a lost update.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("secrets store is already in use: {}", path.display()),
            ));
        }
        let key = chacha20poly1305::Key::from_slice(key_bytes).to_owned();
        let db = match std::fs::read(path) {
            Ok(raw) => Self::decrypt(&key, &raw).map_err(std::io::Error::other)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Db {
                next_id: 1,
                items: Vec::new(),
            },
            Err(e) => return Err(e),
        };
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

    pub fn save(&self) -> std::io::Result<()> {
        let mut plain = serde_json::to_vec(&self.db)?;
        let cipher = ChaCha20Poly1305::new(&self.key);
        let mut nonce_bytes = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, plain.as_slice())
            .map_err(|e| std::io::Error::other(format!("encrypt: {e}")))?;
        plain.zeroize();

        let mut buf = Vec::with_capacity(17 + ct.len());
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&nonce_bytes);
        buf.extend_from_slice(&ct);

        if let Some(dir) = self.path.parent() {
            if !dir.exists() {
                std::fs::create_dir_all(dir)?;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
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

    /// Create an item; if `replace`, first delete items with identical attributes.
    /// Returns (new_id, replaced_ids).
    pub fn create(
        &mut self,
        label: String,
        attributes: BTreeMap<String, String>,
        secret: Vec<u8>,
        content_type: String,
        item_type: String,
        replace: bool,
    ) -> (u64, Vec<u64>) {
        let mut replaced = Vec::new();
        if replace {
            replaced = self
                .db
                .items
                .iter()
                .filter(|i| i.attributes == attributes)
                .map(|i| i.id)
                .collect();
            self.db.items.retain(|i| !replaced.contains(&i.id));
        }
        let id = self.db.next_id;
        self.db.next_id += 1;
        let t = now_secs();
        self.db.items.push(Item {
            id,
            label,
            attributes,
            secret,
            content_type,
            item_type,
            created: t,
            modified: t,
        });
        (id, replaced)
    }

    pub fn set_secret(&mut self, id: u64, secret: Vec<u8>, content_type: String) -> bool {
        if let Some(i) = self.db.items.iter_mut().find(|i| i.id == id) {
            i.secret.zeroize();
            i.secret = secret;
            i.content_type = content_type;
            i.modified = now_secs();
            true
        } else {
            false
        }
    }

    pub fn set_attributes(&mut self, id: u64, attributes: BTreeMap<String, String>) -> bool {
        if let Some(i) = self.db.items.iter_mut().find(|i| i.id == id) {
            i.attributes = attributes;
            i.modified = now_secs();
            true
        } else {
            false
        }
    }

    pub fn set_label(&mut self, id: u64, label: String) -> bool {
        if let Some(i) = self.db.items.iter_mut().find(|i| i.id == id) {
            i.label = label;
            i.modified = now_secs();
            true
        } else {
            false
        }
    }

    pub fn delete(&mut self, id: u64) -> bool {
        let before = self.db.items.len();
        self.db.items.retain(|i| i.id != id);
        self.db.items.len() != before
    }
}

/// Load the 32-byte master key. Preference order:
///  1. $FOCALD_SECRETS_KEYFILE
///  2. $XDG_RUNTIME_DIR/focaldesk/secrets.key
///
/// If the file is absent it is generated (0600) with a warning — production
/// deployments should provision it from PAM at login instead.
pub fn load_master_key() -> std::io::Result<[u8; 32]> {
    let path = std::env::var_os("FOCALD_SECRETS_KEYFILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(|r| PathBuf::from(r).join("focaldesk/secrets.key"))
        })
        .ok_or_else(|| {
            std::io::Error::other("neither FOCALD_SECRETS_KEYFILE nor XDG_RUNTIME_DIR set")
        })?;

    match std::fs::read(&path) {
        Ok(raw) if raw.len() == 32 => {
            let mut k = [0u8; 32];
            k.copy_from_slice(&raw);
            Ok(k)
        }
        Ok(_) => Err(std::io::Error::other(format!(
            "{}: key file must be exactly 32 bytes",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!(
                "master key not found; generating ephemeral session key at {} \
                 (provision from PAM for persistence across key loss)",
                path.display()
            );
            let mut k = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut k);
            if let Some(dir) = path.parent() {
                if !dir.exists() {
                    std::fs::create_dir_all(dir)?;
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            f.write_all(&k)?;
            f.sync_all()?;
            Ok(k)
        }
        Err(e) => Err(e),
    }
}
