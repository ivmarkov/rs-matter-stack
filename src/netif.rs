use core::fmt;
use core::net::{Ipv4Addr, Ipv6Addr};

use edge_nal::UdpBind;
use rs_matter::error::Error;

/// Async trait for accessing the network interface (netif) of a driver.
///
/// Allows sharing the network interface between multiple tasks, where one task
/// may be waiting for the network interface to be ready, while the other might
/// be mutably operating on the L2 driver below the netif, or on the netif itself.
pub trait Netif {
    /// Return the active configuration of the network interface, if any
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error>;

    /// Wait until the network interface configuration changes.
    async fn wait_conf_change(&self) -> Result<(), Error>;
}

impl<T> Netif for &T
where
    T: Netif,
{
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        (**self).get_conf().await
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
        (**self).wait_conf_change().await
    }
}

impl<T> Netif for &mut T
where
    T: Netif,
{
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        (**self).get_conf().await
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
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

impl NetifConf {
    pub const fn new() -> Self {
        Self {
            ipv4: Ipv4Addr::UNSPECIFIED,
            ipv6: Ipv6Addr::UNSPECIFIED,
            interface: 0,
            mac: [0; 6],
        }
    }
}

impl Default for NetifConf {
    fn default() -> Self {
        Self::new()
    }
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

/// This is a `Netif` implementation that does not really track any changes of an underlying network interface,
/// and therefore assumes the network interface is always up.
///
/// Furthermore, it always reports fixed IPs and interface ID
/// (by default `Ipv4Addr::UNSPECIFIED` / `Ipv6Addr::UNSPECIFIED` and 0).
///
/// Useful for demoing purposes
pub struct DummyNetif<U> {
    conf: Option<NetifConf>,
    bind: U,
}

impl<U> DummyNetif<U> {
    /// Create a new `DummyNetif` with the given IP configuration and MAC address
    pub const fn new(conf: Option<NetifConf>, bind: U) -> Self {
        Self { conf, bind }
    }
}

#[cfg(feature = "std")]
impl Default for DummyNetif<edge_nal_std::Stack> {
    fn default() -> Self {
        Self {
            conf: Some(NetifConf::default()),
            bind: edge_nal_std::Stack::new(),
        }
    }
}

impl<U> Netif for DummyNetif<U> {
    async fn get_conf(&self) -> Result<Option<NetifConf>, Error> {
        Ok(self.conf.clone())
    }

    async fn wait_conf_change(&self) -> Result<(), Error> {
        // DummyNetif does not track any changes
        core::future::pending().await
    }
}

impl<U> edge_nal::UdpBind for DummyNetif<U>
where
    U: UdpBind,
{
    type Error = U::Error;

    type Socket<'a> = U::Socket<'a> where Self: 'a;

    async fn bind(&self, addr: core::net::SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        self.bind.bind(addr).await
    }
}
