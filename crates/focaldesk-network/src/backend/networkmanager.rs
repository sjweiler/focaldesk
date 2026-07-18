use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::watch;
use zbus::{
    Connection, proxy,
    zvariant::{ObjectPath, OwnedObjectPath},
};

use crate::backend::NetworkBackend;
use crate::model::{Connectivity, NetTransport, NetworkState, WifiInfo};

const NM_DEVICE_TYPE_WIFI: u32 = 2;

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

    #[zbus(property, name = "PrimaryConnection")]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;

    #[zbus(property, name = "DeviceType")]
    fn device_type(&self) -> zbus::Result<u32>;

    #[zbus(property, name = "Ip4Config")]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(property, name = "Ip6Config")]
    fn ip6_config(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wired",
    default_service = "org.freedesktop.NetworkManager"
)]
trait WiredDevice {
    #[zbus(property)]
    fn carrier(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait WirelessDevice {
    #[zbus(property, name = "ActiveAccessPoint")]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip4Config {
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.IP6Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip6Config {
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;
}

pub struct NetworkManagerBackend {
    conn: Connection,
}

#[derive(Default)]
struct DeviceDetails {
    interface_name: Option<String>,
    has_carrier: bool,
    has_ipv4: bool,
    has_ipv6: bool,
    has_default_route: bool,
    wifi: Option<WifiInfo>,
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

    /// NetworkManager's top-level `Connectivity`/`PrimaryConnectionType`
    /// properties say nothing about interface name, carrier, IP presence,
    /// or (for wifi) SSID/signal — those live on the primary active
    /// connection's device object(s), which we walk here.
    async fn primary_device_details(&self, primary_connection: &OwnedObjectPath) -> DeviceDetails {
        let mut details = DeviceDetails::default();

        if primary_connection.as_str() == "/" {
            return details;
        }

        let Ok(active) = self
            .active_connection_proxy(primary_connection.as_ref())
            .await
        else {
            return details;
        };
        let Ok(device_paths) = active.devices().await else {
            return details;
        };
        let Some(device_path) = device_paths.into_iter().next() else {
            return details;
        };

        let Ok(device) = self.device_proxy(device_path.as_ref()).await else {
            return details;
        };

        details.interface_name = device.interface().await.ok();
        let device_type = device.device_type().await.unwrap_or(0);

        if let Ok(wired) = self.wired_device_proxy(device_path.as_ref()).await {
            details.has_carrier = wired.carrier().await.unwrap_or(false);
        }

        if device_type == NM_DEVICE_TYPE_WIFI {
            // A wifi link being up counts as "carrier" for our purposes —
            // Device.Wired.Carrier doesn't apply to wireless devices.
            details.has_carrier = true;
            details.wifi = self.wifi_info(&device_path).await;
        }

        if let Ok(ip4_path) = device.ip4_config().await
            && ip4_path.as_str() != "/"
        {
            details.has_ipv4 = true;
            if let Ok(ip4) = self.ip4_config_proxy(ip4_path.as_ref()).await {
                details.has_default_route = ip4
                    .gateway()
                    .await
                    .map(|gw| !gw.is_empty())
                    .unwrap_or(false);
            }
        }

        if let Ok(ip6_path) = device.ip6_config().await
            && ip6_path.as_str() != "/"
        {
            details.has_ipv6 = true;
            if !details.has_default_route
                && let Ok(ip6) = self.ip6_config_proxy(ip6_path.as_ref()).await
            {
                details.has_default_route = ip6
                    .gateway()
                    .await
                    .map(|gw| !gw.is_empty())
                    .unwrap_or(false);
            }
        }

        details
    }

    async fn wifi_info(&self, device_path: &OwnedObjectPath) -> Option<WifiInfo> {
        let wireless = self
            .wireless_device_proxy(device_path.as_ref())
            .await
            .ok()?;
        let ap_path = wireless.active_access_point().await.ok()?;
        if ap_path.as_str() == "/" {
            return None;
        }

        let ap = self.access_point_proxy(ap_path.as_ref()).await.ok()?;
        let ssid_bytes = ap.ssid().await.unwrap_or_default();
        let ssid =
            (!ssid_bytes.is_empty()).then(|| String::from_utf8_lossy(&ssid_bytes).into_owned());
        let signal_percent = ap.strength().await.ok();

        Some(WifiInfo {
            ssid,
            signal_percent,
        })
    }

    async fn active_connection_proxy<'a>(
        &self,
        path: ObjectPath<'a>,
    ) -> zbus::Result<ActiveConnectionProxy<'a>> {
        ActiveConnectionProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await
    }

    async fn device_proxy<'a>(&self, path: ObjectPath<'a>) -> zbus::Result<DeviceProxy<'a>> {
        DeviceProxy::builder(&self.conn).path(path)?.build().await
    }

    async fn wired_device_proxy<'a>(
        &self,
        path: ObjectPath<'a>,
    ) -> zbus::Result<WiredDeviceProxy<'a>> {
        WiredDeviceProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await
    }

    async fn wireless_device_proxy<'a>(
        &self,
        path: ObjectPath<'a>,
    ) -> zbus::Result<WirelessDeviceProxy<'a>> {
        WirelessDeviceProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await
    }

    async fn access_point_proxy<'a>(
        &self,
        path: ObjectPath<'a>,
    ) -> zbus::Result<AccessPointProxy<'a>> {
        AccessPointProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await
    }

    async fn ip4_config_proxy<'a>(&self, path: ObjectPath<'a>) -> zbus::Result<Ip4ConfigProxy<'a>> {
        Ip4ConfigProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await
    }

    async fn ip6_config_proxy<'a>(&self, path: ObjectPath<'a>) -> zbus::Result<Ip6ConfigProxy<'a>> {
        Ip6ConfigProxy::builder(&self.conn)
            .path(path)?
            .build()
            .await
    }
}

#[async_trait]
impl NetworkBackend for NetworkManagerBackend {
    async fn current_state(&self) -> Result<NetworkState> {
        let proxy = NetworkManagerProxy::new(&self.conn).await?;
        let connectivity = proxy.connectivity().await?;
        let primary_connection_type = proxy.primary_connection_type().await.unwrap_or_default();
        let primary_connection = proxy.primary_connection().await.ok();

        let details = match &primary_connection {
            Some(path) => self.primary_device_details(path).await,
            None => DeviceDetails::default(),
        };

        Ok(NetworkState {
            connectivity: Self::map_nm_connectivity(connectivity),
            primary_transport: Self::map_primary_type(&primary_connection_type),
            interface_name: details.interface_name,
            has_carrier: details.has_carrier,
            has_ipv4: details.has_ipv4,
            has_ipv6: details.has_ipv6,
            has_default_route: details.has_default_route,
            vpn_active: primary_connection_type == "vpn",
            wifi: details.wifi,
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
