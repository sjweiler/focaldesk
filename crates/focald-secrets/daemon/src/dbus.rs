//! org.freedesktop.secrets provider (Secret Service API 0.2).
//!
//! Object model:
//!   /org/freedesktop/secrets                       Service
//!   /org/freedesktop/secrets/collection/default    Collection (the only one)
//!   /org/freedesktop/secrets/aliases/default       same Collection (alias object)
//!   /org/freedesktop/secrets/collection/default/N  Item
//!   /org/freedesktop/secrets/session/sN            Session
//!
//! Lock model: the store key lives for the session (provisioned at login), so
//! everything reports unlocked and Unlock is a no-op returning no prompt ("/").
//! Prompts are never required; all prompt return values are "/" per spec.

use crate::ipc::KEY_ATTR;
use crate::shared::{ClientSession, Shared};
use crate::sscrypto::{SessionCipher, ALG_DH, ALG_PLAIN};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{interface, Connection};
use zeroize::{Zeroize, Zeroizing};

pub const SERVICE_PATH: &str = "/org/freedesktop/secrets";
pub const COLLECTION_PATH: &str = "/org/freedesktop/secrets/collection/default";
pub const ALIAS_PATH: &str = "/org/freedesktop/secrets/aliases/default";
const NO_PROMPT: &str = "/";
const MAX_SECRET_BYTES: usize = 1024 * 1024;
const MAX_ATTRIBUTES: usize = 128;
const MAX_METADATA_BYTES: usize = 4096;
const MAX_SESSIONS: usize = 256;
const MAX_SESSIONS_PER_CALLER: usize = 32;
const SESSION_OPEN_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_SESSION_OPENS_PER_WINDOW: usize = 64;

fn admit_session_open(
    attempts: &mut std::collections::VecDeque<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    while attempts
        .front()
        .is_some_and(|attempt| now.duration_since(*attempt) >= SESSION_OPEN_WINDOW)
    {
        attempts.pop_front();
    }
    if attempts.len() >= MAX_SESSION_OPENS_PER_WINDOW {
        return false;
    }
    attempts.push_back(now);
    true
}

/// The wire Secret struct: (session, parameters, value, content_type).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct WireSecret(pub OwnedObjectPath, pub Vec<u8>, pub Vec<u8>, pub String);

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.freedesktop.Secret.Error")]
pub enum SsError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSession(String),
    NoSuchObject(String),
    Failed(String),
}

fn item_path(id: u64) -> OwnedObjectPath {
    ObjectPath::try_from(format!("{COLLECTION_PATH}/{id}"))
        .expect("valid path")
        .into()
}

fn session_path(id: u64) -> OwnedObjectPath {
    ObjectPath::try_from(format!("{SERVICE_PATH}/session/s{id}"))
        .expect("valid path")
        .into()
}

fn obj(p: &str) -> OwnedObjectPath {
    ObjectPath::try_from(p).expect("valid path").into()
}

/// Parse a session id out of its object path.
fn session_id(path: &ObjectPath<'_>) -> Option<u64> {
    path.as_str()
        .strip_prefix("/org/freedesktop/secrets/session/s")?
        .parse()
        .ok()
}

fn item_id(path: &ObjectPath<'_>) -> Option<u64> {
    path.as_str()
        .strip_prefix("/org/freedesktop/secrets/collection/default/")?
        .parse()
        .ok()
}

/// Extract a{ss} attributes out of a D-Bus Value.
fn attrs_from_value(v: &Value<'_>) -> Result<BTreeMap<String, String>, SsError> {
    let map: HashMap<String, String> = v
        .try_clone()
        .map_err(|e| SsError::Failed(e.to_string()))?
        .try_into()
        .map_err(|e: zbus::zvariant::Error| SsError::Failed(format!("bad attributes: {e}")))?;
    let attributes: BTreeMap<String, String> = map.into_iter().collect();
    validate_public_attributes(&attributes)?;
    Ok(attributes)
}

fn validate_public_attributes(attributes: &BTreeMap<String, String>) -> Result<(), SsError> {
    if attributes.contains_key(KEY_ATTR) {
        return Err(SsError::Failed(format!(
            "{KEY_ATTR} is reserved for the ACL-protected native broker"
        )));
    }
    if attributes.len() > MAX_ATTRIBUTES
        || attributes
            .iter()
            .any(|(key, value)| key.len() > MAX_METADATA_BYTES || value.len() > MAX_METADATA_BYTES)
    {
        return Err(SsError::Failed(
            "item attributes exceed broker limits".into(),
        ));
    }
    Ok(())
}

fn is_public_item(item: &crate::store::Item) -> bool {
    !item.attributes.contains_key(KEY_ATTR)
}

fn caller(header: &Header<'_>) -> Result<String, SsError> {
    header
        .sender()
        .map(|name| name.as_str().to_owned())
        .ok_or_else(|| SsError::Failed("D-Bus method call has no sender".into()))
}

async fn encrypt_for_session(
    shared: &Shared,
    owner: &str,
    session: &ObjectPath<'_>,
    plaintext: &[u8],
    content_type: &str,
) -> Result<WireSecret, SsError> {
    let sid = session_id(session)
        .ok_or_else(|| SsError::NoSession(format!("unknown session {session}")))?;
    let sessions = shared.sessions.lock().await;
    let state = sessions
        .get(&sid)
        .ok_or_else(|| SsError::NoSession(format!("unknown session {session}")))?;
    if state.owner != owner {
        return Err(SsError::NoSession(format!(
            "session {session} belongs to another caller"
        )));
    }
    let (params, value) = state.cipher.encrypt(plaintext);
    Ok(WireSecret(
        obj(session.as_str()),
        params,
        value,
        content_type.to_string(),
    ))
}

async fn decrypt_from_session(
    shared: &Shared,
    owner: &str,
    secret: &WireSecret,
) -> Result<Zeroizing<Vec<u8>>, SsError> {
    let sid = session_id(&secret.0.as_ref())
        .ok_or_else(|| SsError::NoSession(format!("unknown session {}", secret.0)))?;
    let sessions = shared.sessions.lock().await;
    let state = sessions
        .get(&sid)
        .ok_or_else(|| SsError::NoSession(format!("unknown session {}", secret.0)))?;
    if state.owner != owner {
        return Err(SsError::NoSession(format!(
            "session {} belongs to another caller",
            secret.0
        )));
    }
    state
        .cipher
        .decrypt(&secret.1, &secret.2)
        .map(Zeroizing::new)
        .map_err(SsError::Failed)
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct ServiceIface {
    pub shared: Shared,
}

#[interface(name = "org.freedesktop.Secret.Service")]
impl ServiceIface {
    async fn open_session(
        &self,
        algorithm: String,
        input: Value<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(OwnedValue, OwnedObjectPath), SsError> {
        let owner = caller(&header)?;
        {
            let now = std::time::Instant::now();
            let mut attempts = self.shared.session_open_times.lock().await;
            if !admit_session_open(&mut attempts, now) {
                return Err(SsError::Failed(
                    "secret-service session creation is temporarily rate-limited".into(),
                ));
            }
        }
        {
            let sessions = self.shared.sessions.lock().await;
            if sessions.len() >= MAX_SESSIONS
                || sessions
                    .values()
                    .filter(|state| state.owner == owner)
                    .count()
                    >= MAX_SESSIONS_PER_CALLER
            {
                return Err(SsError::Failed(
                    "too many open secret-service sessions".into(),
                ));
            }
        }
        let (cipher, output): (SessionCipher, Value<'_>) = match algorithm.as_str() {
            ALG_PLAIN => (SessionCipher::plain(), Value::from("")),
            ALG_DH => {
                let client_pub: Vec<u8> = input
                    .try_clone()
                    .map_err(|e| SsError::Failed(e.to_string()))?
                    .try_into()
                    .map_err(|e: zbus::zvariant::Error| {
                        SsError::Failed(format!("dh input must be a byte array: {e}"))
                    })?;
                let (cipher, our_pub) = SessionCipher::dh(&client_pub).map_err(SsError::Failed)?;
                (cipher, Value::from(our_pub))
            }
            other => {
                // Spec: unsupported algorithm => org.freedesktop.DBus.Error.NotSupported
                return Err(SsError::ZBus(zbus::Error::from(
                    zbus::fdo::Error::NotSupported(format!("algorithm {other} not supported")),
                )));
            }
        };

        let sid = self.shared.next_session.fetch_add(1, Ordering::SeqCst);
        self.shared
            .sessions
            .lock()
            .await
            .insert(sid, ClientSession { cipher, owner });
        let path = session_path(sid);
        if let Err(error) = conn
            .object_server()
            .at(
                &path,
                SessionIface {
                    shared: self.shared.clone(),
                    id: sid,
                },
            )
            .await
        {
            self.shared.sessions.lock().await.remove(&sid);
            return Err(error.into());
        }
        log::debug!("dbus: opened session s{sid} ({algorithm})");
        let out = OwnedValue::try_from(output).map_err(|e| SsError::Failed(e.to_string()))?;
        Ok((out, path))
    }

    /// Single-collection model: any create request resolves to the default
    /// collection (no prompt). Third-party apps get a working default store.
    async fn create_collection(
        &self,
        _properties: HashMap<String, Value<'_>>,
        _alias: String,
    ) -> Result<(OwnedObjectPath, OwnedObjectPath), SsError> {
        Ok((obj(COLLECTION_PATH), obj(NO_PROMPT)))
    }

    async fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), SsError> {
        let attrs: BTreeMap<String, String> = attributes.into_iter().collect();
        validate_public_attributes(&attrs)?;
        let store = self.shared.store.lock().await;
        let unlocked = store
            .search(&attrs)
            .into_iter()
            .filter(|id| store.get(*id).is_some_and(is_public_item))
            .map(item_path)
            .collect();
        Ok((unlocked, Vec::new()))
    }

    async fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
    ) -> Result<(Vec<OwnedObjectPath>, OwnedObjectPath), SsError> {
        // Everything we serve is always unlocked.
        Ok((objects, obj(NO_PROMPT)))
    }

    async fn lock(
        &self,
        _objects: Vec<OwnedObjectPath>,
    ) -> Result<(Vec<OwnedObjectPath>, OwnedObjectPath), SsError> {
        // Locking is not supported in the session-key model; report nothing locked.
        Ok((Vec::new(), obj(NO_PROMPT)))
    }

    async fn get_secrets(
        &self,
        items: Vec<OwnedObjectPath>,
        session: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<HashMap<OwnedObjectPath, WireSecret>, SsError> {
        let owner = caller(&header)?;
        let mut out = HashMap::new();
        for path in items {
            let Some(id) = item_id(&path.as_ref()) else {
                continue;
            };
            let (secret, ct) = {
                let store = self.shared.store.lock().await;
                match store.get(id) {
                    Some(i) if is_public_item(i) => {
                        (Zeroizing::new(i.secret.clone()), i.content_type.clone())
                    }
                    _ => continue,
                }
            };
            let wire =
                encrypt_for_session(&self.shared, &owner, &session.as_ref(), &secret, &ct).await?;
            out.insert(path, wire);
        }
        Ok(out)
    }

    async fn read_alias(&self, name: String) -> Result<OwnedObjectPath, SsError> {
        if name == "default" || name == "login" || name == "session" {
            Ok(obj(COLLECTION_PATH))
        } else {
            Ok(obj(NO_PROMPT))
        }
    }

    async fn set_alias(&self, _name: String, _collection: OwnedObjectPath) -> Result<(), SsError> {
        Ok(())
    }

    #[zbus(property)]
    async fn collections(&self) -> Vec<OwnedObjectPath> {
        vec![obj(COLLECTION_PATH)]
    }

    #[zbus(signal)]
    async fn collection_created(
        emitter: &SignalEmitter<'_>,
        collection: OwnedObjectPath,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn collection_deleted(
        emitter: &SignalEmitter<'_>,
        collection: OwnedObjectPath,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn collection_changed(
        emitter: &SignalEmitter<'_>,
        collection: OwnedObjectPath,
    ) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

pub struct CollectionIface {
    pub shared: Shared,
}

#[interface(name = "org.freedesktop.Secret.Collection")]
impl CollectionIface {
    async fn delete(&self) -> Result<OwnedObjectPath, SsError> {
        Err(SsError::Failed(
            "the default collection cannot be deleted".into(),
        ))
    }

    async fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> Result<Vec<OwnedObjectPath>, SsError> {
        let attrs: BTreeMap<String, String> = attributes.into_iter().collect();
        validate_public_attributes(&attrs)?;
        let store = self.shared.store.lock().await;
        Ok(store
            .search(&attrs)
            .into_iter()
            .filter(|id| store.get(*id).is_some_and(is_public_item))
            .map(item_path)
            .collect())
    }

    async fn create_item(
        &self,
        properties: HashMap<String, Value<'_>>,
        mut secret: WireSecret,
        replace: bool,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(OwnedObjectPath, OwnedObjectPath), SsError> {
        let label = properties
            .get("org.freedesktop.Secret.Item.Label")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default();
        let attributes = match properties.get("org.freedesktop.Secret.Item.Attributes") {
            Some(v) => attrs_from_value(v)?,
            None => BTreeMap::new(),
        };
        let item_type = properties
            .get("org.freedesktop.Secret.Item.Type")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_else(|| "org.freedesktop.Secret.Generic".into());
        let owner = caller(&header)?;
        let plaintext = decrypt_from_session(&self.shared, &owner, &secret).await;
        secret.2.zeroize();
        let mut plaintext = plaintext?;
        if plaintext.len() > MAX_SECRET_BYTES
            || label.len() > MAX_METADATA_BYTES
            || item_type.len() > MAX_METADATA_BYTES
            || secret.3.len() > MAX_METADATA_BYTES
        {
            return Err(SsError::Failed("item exceeds broker limits".into()));
        }

        let (id, replaced) = {
            let mut store = self.shared.store.lock().await;
            store
                .transaction(move |transaction| {
                    // mem::take moves the buffer into the candidate store,
                    // which zeroizes it if persistence fails.
                    transaction.create(
                        label,
                        attributes,
                        std::mem::take(&mut *plaintext),
                        secret.3.clone(),
                        item_type,
                        replace,
                    )
                })
                .map_err(|e| SsError::Failed(format!("persist failed: {e}")))?
        };
        sync_items(conn, &self.shared).await?;

        for rid in replaced {
            let _ = Self::item_deleted(&emitter, item_path(rid)).await;
        }
        let path = item_path(id);
        let _ = Self::item_created(&emitter, path.clone()).await;
        log::info!("dbus: item {id} created (replace={replace})");
        Ok((path, obj(NO_PROMPT)))
    }

    #[zbus(property)]
    async fn items(&self) -> Vec<OwnedObjectPath> {
        let store = self.shared.store.lock().await;
        store
            .items()
            .iter()
            .filter(|item| is_public_item(item))
            .map(|i| item_path(i.id))
            .collect()
    }

    #[zbus(property)]
    async fn label(&self) -> String {
        "Focaldesk".into()
    }

    #[zbus(property)]
    async fn set_label(&self, _v: String) {}

    #[zbus(property)]
    async fn locked(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn created(&self) -> u64 {
        0
    }

    #[zbus(property)]
    async fn modified(&self) -> u64 {
        let store = self.shared.store.lock().await;
        store.items().iter().map(|i| i.modified).max().unwrap_or(0)
    }

    #[zbus(signal)]
    async fn item_created(emitter: &SignalEmitter<'_>, item: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn item_deleted(emitter: &SignalEmitter<'_>, item: OwnedObjectPath) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn item_changed(emitter: &SignalEmitter<'_>, item: OwnedObjectPath) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------------
// Item
// ---------------------------------------------------------------------------

pub struct ItemIface {
    pub shared: Shared,
    pub id: u64,
}

#[interface(name = "org.freedesktop.Secret.Item")]
impl ItemIface {
    async fn delete(
        &self,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, SsError> {
        {
            let mut store = self.shared.store.lock().await;
            if store.get(self.id).is_none() {
                return Err(SsError::NoSuchObject(format!("item {}", self.id)));
            }
            store
                .transaction(|transaction| {
                    transaction.delete(self.id);
                    Ok(())
                })
                .map_err(|e| SsError::Failed(format!("persist failed: {e}")))?;
        }
        sync_items(conn, &self.shared).await?;
        log::info!("dbus: item {} deleted", self.id);
        Ok(obj(NO_PROMPT))
    }

    /// NB: the 1-tuple return is deliberate — zbus flattens a top-level struct
    /// return into multiple out-arguments; wrapping keeps the D-Bus signature
    /// `(oayays)` as the spec (and python-secretstorage) requires.
    async fn get_secret(
        &self,
        session: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(WireSecret,), SsError> {
        let owner = caller(&header)?;
        let (secret, ct) = {
            let store = self.shared.store.lock().await;
            let item = store
                .get(self.id)
                .ok_or_else(|| SsError::NoSuchObject(format!("item {}", self.id)))?;
            (
                Zeroizing::new(item.secret.clone()),
                item.content_type.clone(),
            )
        };
        Ok((encrypt_for_session(&self.shared, &owner, &session.as_ref(), &secret, &ct).await?,))
    }

    async fn set_secret(
        &self,
        mut secret: WireSecret,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), SsError> {
        let owner = caller(&header)?;
        let plaintext = decrypt_from_session(&self.shared, &owner, &secret).await;
        secret.2.zeroize();
        let mut plaintext = plaintext?;
        if plaintext.len() > MAX_SECRET_BYTES || secret.3.len() > MAX_METADATA_BYTES {
            return Err(SsError::Failed("secret exceeds broker limits".into()));
        }
        let mut store = self.shared.store.lock().await;
        if store.get(self.id).is_none() {
            return Err(SsError::NoSuchObject(format!("item {}", self.id)));
        }
        store
            .transaction(move |transaction| {
                transaction.set_secret(self.id, std::mem::take(&mut *plaintext), secret.3.clone());
                Ok(())
            })
            .map_err(|e| SsError::Failed(format!("persist failed: {e}")))
    }

    #[zbus(property)]
    async fn locked(&self) -> bool {
        false
    }

    #[zbus(property)]
    async fn attributes(&self) -> HashMap<String, String> {
        let store = self.shared.store.lock().await;
        store
            .get(self.id)
            .map(|i| {
                i.attributes
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn set_attributes(&self, attrs: HashMap<String, String>) -> zbus::fdo::Result<()> {
        let attrs: BTreeMap<String, String> = attrs.into_iter().collect();
        validate_public_attributes(&attrs)
            .map_err(|error| zbus::fdo::Error::Failed(format!("{error:?}")))?;
        let mut store = self.shared.store.lock().await;
        if store.get(self.id).is_some() {
            store
                .transaction(move |transaction| {
                    transaction.set_attributes(self.id, attrs);
                    Ok(())
                })
                .map_err(|e| zbus::fdo::Error::Failed(format!("persist failed: {e}")))?;
        }
        Ok(())
    }

    #[zbus(property)]
    async fn label(&self) -> String {
        let store = self.shared.store.lock().await;
        store
            .get(self.id)
            .map(|i| i.label.clone())
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn set_label(&self, v: String) -> zbus::fdo::Result<()> {
        if v.len() > MAX_METADATA_BYTES {
            return Err(zbus::fdo::Error::Failed(
                "item label exceeds broker limits".into(),
            ));
        }
        let mut store = self.shared.store.lock().await;
        if store.get(self.id).is_some() {
            store
                .transaction(move |transaction| {
                    transaction.set_label(self.id, v);
                    Ok(())
                })
                .map_err(|error| zbus::fdo::Error::Failed(format!("persist failed: {error}")))?;
        }
        Ok(())
    }

    #[zbus(property)]
    async fn created(&self) -> u64 {
        let store = self.shared.store.lock().await;
        store.get(self.id).map(|i| i.created).unwrap_or(0)
    }

    #[zbus(property)]
    async fn modified(&self) -> u64 {
        let store = self.shared.store.lock().await;
        store.get(self.id).map(|i| i.modified).unwrap_or(0)
    }

    #[zbus(property)]
    async fn r#type(&self) -> String {
        let store = self.shared.store.lock().await;
        store
            .get(self.id)
            .map(|i| i.item_type.clone())
            .unwrap_or_else(|| "org.freedesktop.Secret.Generic".into())
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

pub struct SessionIface {
    pub shared: Shared,
    pub id: u64,
}

#[interface(name = "org.freedesktop.Secret.Session")]
impl SessionIface {
    async fn close(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), SsError> {
        let owner = caller(&header)?;
        let mut sessions = self.shared.sessions.lock().await;
        if sessions
            .get(&self.id)
            .is_none_or(|session| session.owner != owner)
        {
            return Err(SsError::NoSession(format!(
                "session {} belongs to another caller",
                session_path(self.id)
            )));
        }
        sessions.remove(&self.id);
        drop(sessions);
        let path = session_path(self.id);
        let _ = conn.object_server().remove::<SessionIface, _>(&path).await;
        log::debug!("dbus: session s{} closed", self.id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

/// Diff store items against registered D-Bus objects; add/remove as needed.
pub async fn sync_items(conn: &Connection, shared: &Shared) -> zbus::Result<()> {
    let current: HashSet<u64> = {
        let store = shared.store.lock().await;
        store
            .items()
            .iter()
            .filter(|item| is_public_item(item))
            .map(|i| i.id)
            .collect()
    };
    let mut reg = shared.registered_items.lock().await;
    let server = conn.object_server();
    for id in current.difference(&reg) {
        server
            .at(
                item_path(*id),
                ItemIface {
                    shared: shared.clone(),
                    id: *id,
                },
            )
            .await?;
    }
    for id in reg.difference(&current) {
        let _ = server.remove::<ItemIface, _>(item_path(*id)).await;
    }
    *reg = current;
    Ok(())
}

pub async fn start(shared: Shared) -> zbus::Result<Connection> {
    let conn = zbus::connection::Builder::session()?
        .serve_at(
            SERVICE_PATH,
            ServiceIface {
                shared: shared.clone(),
            },
        )?
        .serve_at(
            COLLECTION_PATH,
            CollectionIface {
                shared: shared.clone(),
            },
        )?
        .serve_at(
            ALIAS_PATH,
            CollectionIface {
                shared: shared.clone(),
            },
        )?
        .name("org.freedesktop.secrets")?
        .build()
        .await?;
    sync_items(&conn, &shared).await?;
    start_session_reaper(&conn, shared);
    log::info!("dbus: serving org.freedesktop.secrets");
    Ok(conn)
}

fn start_session_reaper(conn: &Connection, shared: Shared) {
    let conn = conn.clone();
    tokio::spawn(async move {
        let proxy = match zbus::fdo::DBusProxy::new(&conn).await {
            Ok(proxy) => proxy,
            Err(error) => {
                log::warn!("dbus: cannot monitor disconnected secret sessions: {error}");
                return;
            }
        };
        let mut changes = match proxy.receive_name_owner_changed().await {
            Ok(changes) => changes,
            Err(error) => {
                log::warn!("dbus: cannot subscribe to owner changes: {error}");
                return;
            }
        };
        while let Some(change) = changes.next().await {
            let Ok(arguments) = change.args() else {
                continue;
            };
            if arguments.new_owner().as_ref().is_some()
                || !arguments.name().as_str().starts_with(':')
            {
                continue;
            }
            let owner = arguments.name().as_str();
            let stale: Vec<u64> = {
                let mut sessions = shared.sessions.lock().await;
                let stale = sessions
                    .iter()
                    .filter_map(|(id, session)| (session.owner == owner).then_some(*id))
                    .collect::<Vec<_>>();
                sessions.retain(|_, session| session.owner != owner);
                stale
            };
            for id in stale {
                let _ = conn
                    .object_server()
                    .remove::<SessionIface, _>(session_path(id))
                    .await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        admit_session_open, is_public_item, validate_public_attributes,
        MAX_SESSION_OPENS_PER_WINDOW, SESSION_OPEN_WINDOW,
    };
    use crate::ipc::KEY_ATTR;
    use crate::store::Item;
    use std::collections::BTreeMap;

    fn item(attributes: BTreeMap<String, String>) -> Item {
        Item {
            id: 1,
            label: String::new(),
            attributes,
            secret: Vec::new(),
            content_type: "text/plain".into(),
            item_type: "org.freedesktop.Secret.Generic".into(),
            created: 0,
            modified: 0,
        }
    }

    #[test]
    fn native_key_attribute_is_never_public() {
        let mut attributes = BTreeMap::new();
        attributes.insert(KEY_ATTR.into(), "ai/token".into());
        assert!(validate_public_attributes(&attributes).is_err());
        assert!(!is_public_item(&item(attributes)));
    }

    #[test]
    fn ordinary_secret_service_item_is_public() {
        let mut attributes = BTreeMap::new();
        attributes.insert("service".into(), "example".into());
        assert!(validate_public_attributes(&attributes).is_ok());
        assert!(is_public_item(&item(attributes)));
    }

    #[test]
    fn session_open_rate_limit_recovers_after_window() {
        let now = std::time::Instant::now();
        let mut attempts = std::collections::VecDeque::new();
        for _ in 0..MAX_SESSION_OPENS_PER_WINDOW {
            assert!(admit_session_open(&mut attempts, now));
        }
        assert!(!admit_session_open(&mut attempts, now));
        assert!(admit_session_open(&mut attempts, now + SESSION_OPEN_WINDOW));
    }
}
