//!  Utilities related to working with network interfaces.

use std::net::SocketAddr;

use pnet::datalink;
use pnet::datalink::NetworkInterface;

pub trait FromNetworkInterfaceName {
    fn from_net_iface_name(iface_name: &str) -> Option<NetworkInterface>;
}

impl FromNetworkInterfaceName for NetworkInterface {
    fn from_net_iface_name(iface_name: &str) -> Option<NetworkInterface> {
        datalink::interfaces()
            .into_iter()
            .find(|iface| iface.name == iface_name)
    }
}

pub trait IntoNetworkInterface {
    fn into_network_interface(self) -> Option<NetworkInterface>;
}

impl<T> IntoNetworkInterface for T
where
    T: AsRef<str>,
{
    fn into_network_interface(self) -> Option<NetworkInterface> {
        NetworkInterface::from_net_iface_name(self.as_ref())
    }
}

pub trait ToSocketAddr {
    fn to_socket_addr(self) -> Option<SocketAddr>;
}

impl ToSocketAddr for NetworkInterface {
    fn to_socket_addr(self) -> Option<SocketAddr> {
        let ip_addr = self.ips.iter().find(|ip| ip.is_ipv4())?.ip();
        Some(SocketAddr::new(ip_addr, 0))
    }
}
