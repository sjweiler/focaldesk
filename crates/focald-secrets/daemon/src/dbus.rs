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

use crate::shared::Shared;
use crate::sscrypto::{SessionCipher, ALG_DH, ALG_PLAIN};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{interface, Connection};
use zeroize::Zeroizing;

pub const SERVICE_PATH: &str = "/org/freedesktop/secrets";
pub const COLLECTION_PATH: &str = "/org/freedesktop/secrets/collection/default";
pub const ALIAS_PATH: &str = "/org/freedesktop/secrets/aliases/default";
const NO_PROMPT: &str = "/";

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
    Ok(map.into_iter().collect())
}

async fn encrypt_for_session(
    shared: &Shared,
    session: &ObjectPath<'_>,
    plaintext: &[u8],
    content_type: &str,
) -> Result<WireSecret, SsError> {
    let sid = session_id(session)
        .ok_or_else(|| SsError::NoSession(format!("unknown session {session}")))?;
    let sessions = shared.sessions.lock().await;
    let cipher = sessions
        .get(&sid)
        .ok_or_else(|| SsError::NoSession(format!("unknown session {session}")))?;
    let (params, value) = cipher.encrypt(plaintext);
    Ok(WireSecret(
        obj(session.as_str()),
        params,
        value,
        content_type.to_string(),
    ))
}

async fn decrypt_from_session(
    shared: &Shared,
    secret: &WireSecret,
) -> Result<Zeroizing<Vec<u8>>, SsError> {
    let sid = session_id(&secret.0.as_ref())
        .ok_or_else(|| SsError::NoSession(format!("unknown session {}", secret.0)))?;
    let sessions = shared.sessions.lock().await;
    let cipher = sessions
        .get(&sid)
        .ok_or_else(|| SsError::NoSession(format!("unknown session {}", secret.0)))?;
    cipher
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
    ) -> Result<(OwnedValue, OwnedObjectPath), SsError> {
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
        self.shared.sessions.lock().await.insert(sid, cipher);
        let path = session_path(sid);
        conn.object_server()
            .at(
                &path,
                SessionIface {
                    shared: self.shared.clone(),
                    id: sid,
                },
            )
            .await?;
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
        let store = self.shared.store.lock().await;
        let unlocked = store.search(&attrs).into_iter().map(item_path).collect();
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
    ) -> Result<HashMap<OwnedObjectPath, WireSecret>, SsError> {
        let mut out = HashMap::new();
        for path in items {
            let Some(id) = item_id(&path.as_ref()) else {
                continue;
            };
            let (secret, ct) = {
                let store = self.shared.store.lock().await;
                match store.get(id) {
                    Some(i) => (Zeroizing::new(i.secret.clone()), i.content_type.clone()),
                    None => continue,
                }
            };
            let wire = encrypt_for_session(&self.shared, &session.as_ref(), &secret, &ct).await?;
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
        let store = self.shared.store.lock().await;
        Ok(store.search(&attrs).into_iter().map(item_path).collect())
    }

    async fn create_item(
        &self,
        properties: HashMap<String, Value<'_>>,
        secret: WireSecret,
        replace: bool,
        #[zbus(connection)] conn: &Connection,
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
        let mut plaintext = decrypt_from_session(&self.shared, &secret).await?;

        let (id, replaced) = {
            let mut store = self.shared.store.lock().await;
            // mem::take moves the buffer into the store (which zeroizes it on
            // drop); the emptied Zeroizing shell scrubs nothing but is sound.
            let r = store.create(
                label,
                attributes,
                std::mem::take(&mut *plaintext),
                secret.3.clone(),
                item_type,
                replace,
            );
            store
                .save()
                .map_err(|e| SsError::Failed(format!("persist failed: {e}")))?;
            r
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
        store.items().iter().map(|i| item_path(i.id)).collect()
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
            if !store.delete(self.id) {
                return Err(SsError::NoSuchObject(format!("item {}", self.id)));
            }
            store
                .save()
                .map_err(|e| SsError::Failed(format!("persist failed: {e}")))?;
        }
        sync_items(conn, &self.shared).await?;
        log::info!("dbus: item {} deleted", self.id);
        Ok(obj(NO_PROMPT))
    }

    /// NB: the 1-tuple return is deliberate — zbus flattens a top-level struct
    /// return into multiple out-arguments; wrapping keeps the D-Bus signature
    /// `(oayays)` as the spec (and python-secretstorage) requires.
    async fn get_secret(&self, session: OwnedObjectPath) -> Result<(WireSecret,), SsError> {
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
        Ok((encrypt_for_session(&self.shared, &session.as_ref(), &secret, &ct).await?,))
    }

    async fn set_secret(&self, secret: WireSecret) -> Result<(), SsError> {
        let mut plaintext = decrypt_from_session(&self.shared, &secret).await?;
        let mut store = self.shared.store.lock().await;
        if !store.set_secret(self.id, std::mem::take(&mut *plaintext), secret.3.clone()) {
            return Err(SsError::NoSuchObject(format!("item {}", self.id)));
        }
        store
            .save()
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
    #[zbus(property)]
    async fn set_attributes(&self, attrs: HashMap<String, String>) {
        let mut store = self.shared.store.lock().await;
        if store.set_attributes(self.id, attrs.into_iter().collect()) {
            if let Err(e) = store.save() {
                log::error!("dbus: persist after set_attributes failed: {e}");
            }
        }
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
    async fn set_label(&self, v: String) {
        let mut store = self.shared.store.lock().await;
        if store.set_label(self.id, v) {
            let _ = store.save();
        }
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
    async fn close(&self, #[zbus(connection)] conn: &Connection) -> Result<(), SsError> {
        self.shared.sessions.lock().await.remove(&self.id);
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
        store.items().iter().map(|i| i.id).collect()
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
    log::info!("dbus: serving org.freedesktop.secrets");
    Ok(conn)
}
