use anyhow::Result;
use async_trait::async_trait;
use futures::stream::TryStreamExt;
use tokio::sync::watch;

use crate::backend::NetworkBackend;
use crate::model::{Connectivity, NetTransport, NetworkState};

use rtnetlink::RouteMessageBuilder;
use rtnetlink::packet_route::{AddressFamily, link::LinkAttribute, route::RouteAttribute};

/// Fallback backend for when NetworkManager isn't running. Deliberately
/// leaves [`NetworkState::wifi`](crate::model::NetworkState::wifi) unset:
/// SSID/signal strength live in nl80211, a separate netlink family from the
/// route/link tables this backend already talks to, and pulling it in is
/// out of scope for a fallback path — NetworkManager (the primary backend,
/// see [`crate::factory::auto_backend`]) covers wifi info instead.
pub struct RtnetlinkBackend {
    handle: rtnetlink::Handle,
}

impl RtnetlinkBackend {
    pub fn new() -> Result<Self> {
        let (conn, handle, _) = rtnetlink::new_connection()?;
        tokio::spawn(conn);
        Ok(Self { handle })
    }
}

#[async_trait]
impl NetworkBackend for RtnetlinkBackend {
    async fn current_state(&self) -> Result<NetworkState> {
        let mut primary_ifindex: Option<u32> = None;

        let mut routes_v4 = self
            .handle
            .route()
            .get(RouteMessageBuilder::<std::net::Ipv4Addr>::new().build())
            .execute();
        while let Some(route) = routes_v4.try_next().await? {
            if route.header.address_family != AddressFamily::Inet {
                continue;
            }

            if route.header.destination_prefix_length == 0 {
                for attr in route.attributes {
                    if let RouteAttribute::Oif(index) = attr {
                        primary_ifindex = Some(index);
                        break;
                    }
                }

                if primary_ifindex.is_some() {
                    break;
                }
            }
        }

        if primary_ifindex.is_none() {
            let mut routes_v6 = self
                .handle
                .route()
                .get(RouteMessageBuilder::<std::net::Ipv6Addr>::new().build())
                .execute();
            while let Some(route) = routes_v6.try_next().await? {
                if route.header.address_family != AddressFamily::Inet6 {
                    continue;
                }

                if route.header.destination_prefix_length == 0 {
                    for attr in route.attributes {
                        if let RouteAttribute::Oif(index) = attr {
                            primary_ifindex = Some(index);
                            break;
                        }
                    }

                    if primary_ifindex.is_some() {
                        break;
                    }
                }
            }
        }

        let mut iface_name = None;
        let mut has_carrier = false;
        let mut transport = NetTransport::Unknown;

        if let Some(ifindex) = primary_ifindex {
            let mut links = self.handle.link().get().execute();

            while let Some(msg) = links.try_next().await? {
                if msg.header.index != ifindex {
                    continue;
                }

                has_carrier = true;

                for attr in msg.attributes {
                    if let LinkAttribute::IfName(n) = attr {
                        if n.starts_with("wl") || n.starts_with("wlan") {
                            transport = NetTransport::Wifi;
                        } else if n.starts_with("en")
                            || n.starts_with("eth")
                            || n.starts_with("eno")
                            || n.starts_with("ens")
                            || n.starts_with("enp")
                        {
                            transport = NetTransport::Ethernet;
                        } else if n.starts_with("tun")
                            || n.starts_with("tap")
                            || n.starts_with("wg")
                            || n.starts_with("vpn")
                        {
                            transport = NetTransport::Vpn;
                        }

                        iface_name = Some(n);
                    }
                }

                break;
            }
        }

        let mut has_ipv4 = false;
        let mut has_ipv6 = false;

        if let Some(ifindex) = primary_ifindex {
            let mut addrs = self.handle.address().get().execute();

            while let Some(msg) = addrs.try_next().await? {
                if msg.header.index != ifindex {
                    continue;
                }

                match msg.header.family {
                    AddressFamily::Inet => has_ipv4 = true,
                    AddressFamily::Inet6 => has_ipv6 = true,
                    _ => {}
                }
            }
        }

        let has_default_route = primary_ifindex.is_some();

        let connectivity = if !has_carrier {
            Connectivity::Disconnected
        } else if !has_ipv4 && !has_ipv6 {
            Connectivity::LinkOnly
        } else if !has_default_route {
            Connectivity::LocalOnly
        } else {
            Connectivity::Internet
        };

        Ok(NetworkState {
            primary_transport: Some(transport),
            connectivity,
            interface_name: iface_name,
            has_carrier,
            has_ipv4,
            has_ipv6,
            has_default_route,
            vpn_active: matches!(transport, NetTransport::Vpn),
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
        "rtnetlink"
    }
}
