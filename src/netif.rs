use core::fmt;
use core::net::{Ipv4Addr, Ipv6Addr};

/// Async trait for accessing the `EspNetif` network interface (netif) of a driver.
///
/// Allows sharing the network interface between multiple tasks, where one task
/// may be waiting for the network interface to be ready, while the other might
/// be mutably operating on the L2 driver below the netif, or on the netif itself.
pub trait Netif {
    /// Return the active configuration of the network interface, if any
    fn get_conf(&self) -> Option<NetifConf>;

    /// Wait until the network interface configuration changes.
    async fn wait_conf_change(&self);
}

impl<T> Netif for &T
where
    T: Netif,
{
    fn get_conf(&self) -> Option<NetifConf> {
        (**self).get_conf()
    }

    async fn wait_conf_change(&self) {
        (**self).wait_conf_change().await
    }
}

impl<T> Netif for &mut T
where
    T: Netif,
{
    fn get_conf(&self) -> Option<NetifConf> {
        (**self).get_conf()
    }

    async fn wait_conf_change(&self) {
        (**self).wait_conf_change().await
    }
}

/// The current IP configuration of a network interface (if the netif is configured and up)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetifConf {
    /// Ipv4 address
    pub ipv4: Ipv4Addr,
    // Ipv6 address
    pub ipv6: Ipv6Addr,
    // Interface index
    pub interface: u32,
    // MAC address
    pub mac: [u8; 6],
}

impl fmt::Display for NetifConf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "IPv4: {}, IPv6: {}, Interface: {}, MAC: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.ipv4,
            self.ipv6,
            self.interface,
            self.mac[0],
            self.mac[1],
            self.mac[2],
            self.mac[3],
            self.mac[4],
            self.mac[5]
        )
    }
}
