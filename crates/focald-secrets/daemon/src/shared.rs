//! State shared between the native IPC surface and the D-Bus surface.

use crate::acl::Acl;
use crate::sscrypto::SessionCipher;
use crate::store::Store;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct ClientSession {
    pub cipher: SessionCipher,
    pub owner: String,
}

pub struct SharedInner {
    pub store: Mutex<Store>,
    pub acl: Mutex<Acl>,
    pub sessions: Mutex<HashMap<u64, ClientSession>>,
    pub next_session: AtomicU64,
    pub session_open_times: Mutex<VecDeque<std::time::Instant>>,
    pub dbus: Mutex<Option<zbus::Connection>>,
    /// Item ids currently registered on the D-Bus object server.
    pub registered_items: Mutex<HashSet<u64>>,
}

#[derive(Clone)]
pub struct Shared(pub Arc<SharedInner>);

impl std::ops::Deref for Shared {
    type Target = SharedInner;
    fn deref(&self) -> &SharedInner {
        &self.0
    }
}

impl Shared {
    pub fn new(store: Store, acl: Acl) -> Self {
        Shared(Arc::new(SharedInner {
            store: Mutex::new(store),
            acl: Mutex::new(acl),
            sessions: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
            session_open_times: Mutex::new(VecDeque::new()),
            dbus: Mutex::new(None),
            registered_items: Mutex::new(HashSet::new()),
        }))
    }

    pub async fn dbus_conn(&self) -> Option<zbus::Connection> {
        self.dbus.lock().await.clone()
    }

    /// After a mutation from the native surface, refresh D-Bus item objects.
    pub async fn notify_dbus_items_changed(&self) {
        if let Some(conn) = self.dbus_conn().await {
            if let Err(e) = crate::dbus::sync_items(&conn, self).await {
                log::warn!("dbus: item sync failed: {e}");
            }
        }
    }
}
