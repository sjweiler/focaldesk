#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetTransport {
    Ethernet,
    Wifi,
    Vpn,
    Cellular,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    Unknown,
    Disconnected,
    Connecting,
    LinkOnly,
    LocalOnly,
    SiteOnly,
    Internet,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WifiInfo {
    pub ssid: Option<String>,
    pub signal_percent: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkState {
    pub primary_transport: Option<NetTransport>,
    pub connectivity: Connectivity,
    pub interface_name: Option<String>,
    pub has_carrier: bool,
    pub has_ipv4: bool,
    pub has_ipv6: bool,
    pub has_default_route: bool,
    pub vpn_active: bool,
    pub wifi: Option<WifiInfo>,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self {
            primary_transport: None,
            connectivity: Connectivity::Unknown,
            interface_name: None,
            has_carrier: false,
            has_ipv4: false,
            has_ipv6: false,
            has_default_route: false,
            vpn_active: false,
            wifi: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkIcon {
    Offline,
    Connecting,
    Limited,
    Ethernet,
    Wifi0,
    Wifi1,
    Wifi2,
    Wifi3,
    Wifi4,
    EthernetVpn,
    WifiVpn0,
    WifiVpn1,
    WifiVpn2,
    WifiVpn3,
    WifiVpn4,
}

pub fn map_icon(state: &NetworkState) -> NetworkIcon {
    let vpn = state.vpn_active;

    let wifi_icon = match state.wifi.as_ref().and_then(|w| w.signal_percent) {
        Some(0..=12) => NetworkIcon::Wifi0,
        Some(13..=37) => NetworkIcon::Wifi1,
        Some(38..=62) => NetworkIcon::Wifi2,
        Some(63..=87) => NetworkIcon::Wifi3,
        Some(_) => NetworkIcon::Wifi4,
        None => NetworkIcon::Wifi0,
    };

    let wifi_vpn_icon = match state.wifi.as_ref().and_then(|w| w.signal_percent) {
        Some(0..=12) => NetworkIcon::WifiVpn0,
        Some(13..=37) => NetworkIcon::WifiVpn1,
        Some(38..=62) => NetworkIcon::WifiVpn2,
        Some(63..=87) => NetworkIcon::WifiVpn3,
        Some(_) => NetworkIcon::WifiVpn4,
        None => NetworkIcon::WifiVpn0,
    };

    match state.connectivity {
        Connectivity::Unknown | Connectivity::Disconnected => NetworkIcon::Offline,
        Connectivity::Connecting | Connectivity::LinkOnly => NetworkIcon::Connecting,
        Connectivity::LocalOnly | Connectivity::SiteOnly => NetworkIcon::Limited,
        Connectivity::Internet => match state.primary_transport {
            Some(NetTransport::Ethernet) => {
                if vpn {
                    NetworkIcon::EthernetVpn
                } else {
                    NetworkIcon::Ethernet
                }
            }
            Some(NetTransport::Wifi) => {
                if vpn {
                    wifi_vpn_icon
                } else {
                    wifi_icon
                }
            }
            _ => NetworkIcon::Ethernet,
        },
    }
}
