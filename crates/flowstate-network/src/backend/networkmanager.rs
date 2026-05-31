use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::watch;
use zbus::{Connection, proxy};

use crate::backend::NetworkBackend;
use crate::model::{Connectivity, NetTransport, NetworkState};

#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn primary_connection_type(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn active_connections(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;
}

pub struct NetworkManagerBackend {
    conn: Connection,
}

impl NetworkManagerBackend {
    pub async fn new() -> Result<Self> {
        let conn = Connection::system().await.context("connect system bus")?;
        Ok(Self { conn })
    }

    fn map_nm_connectivity(value: u32) -> Connectivity {
        match value {
            1 => Connectivity::Disconnected, // NM_CONNECTIVITY_NONE
            2 => Connectivity::LocalOnly,    // NM_CONNECTIVITY_PORTAL
            3 => Connectivity::SiteOnly,     // NM_CONNECTIVITY_LIMITED
            4 => Connectivity::Internet,     // NM_CONNECTIVITY_FULL
            _ => Connectivity::Unknown,
        }
    }

    fn map_primary_type(s: &str) -> Option<NetTransport> {
        match s {
            "802-3-ethernet" => Some(NetTransport::Ethernet),
            "802-11-wireless" => Some(NetTransport::Wifi),
            "vpn" => Some(NetTransport::Vpn),
            "gsm" | "cdma" => Some(NetTransport::Cellular),
            "" => None,
            _ => Some(NetTransport::Unknown),
        }
    }
}

#[async_trait]
impl NetworkBackend for NetworkManagerBackend {
    async fn current_state(&self) -> Result<NetworkState> {
        let proxy = NetworkManagerProxy::new(&self.conn).await?;
        let connectivity = proxy.connectivity().await?;
        let primary_connection_type = proxy.primary_connection_type().await.unwrap_or_default();

        Ok(NetworkState {
            connectivity: Self::map_nm_connectivity(connectivity),
            primary_transport: Self::map_primary_type(&primary_connection_type),
            // Fill these in later by walking active device objects.
            interface_name: None,
            has_carrier: false,
            has_ipv4: false,
            has_ipv6: false,
            has_default_route: false,
            vpn_active: primary_connection_type == "vpn",
            wifi: None,
        })
    }

    async fn watch(&self, tx: watch::Sender<NetworkState>) -> Result<()> {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));

        loop {
            interval.tick().await;
            if let Ok(state) = self.current_state().await {
                let _ = tx.send(state);
            }
        }
    }

    fn name(&self) -> &'static str {
        "networkmanager"
    }
}
