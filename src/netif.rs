use core::fmt;
use core::net::{Ipv4Addr, Ipv6Addr};

use edge_nal::UdpBind;
use rs_matter::error::Error;

#[cfg(all(unix, feature = "std", not(target_os = "espidf")))]
pub use unix::UnixNetif;

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

#[cfg(all(unix, feature = "std", not(target_os = "espidf")))]
mod unix {
    use alloc::string::String;

    use embassy_time::{Duration, Timer};
    use nix::{
        net::if_::InterfaceFlags,
        sys::socket::{SockaddrIn6, SockaddrStorage},
    };

    use super::{Netif, NetifConf};

    /// UnixNetif works on any Unix-like OS
    ///
    /// It is a simple implementation of the `Netif` trait that uses polling instead of actual notifications
    /// to detect changes in the network interface configuration.
    pub struct UnixNetif(String);

    impl UnixNetif {
        /// Create a new `UnixNetif`. The implementation will try
        /// to find and use a suitable interface automatically.
        pub fn new_default() -> Self {
            Self(default_if().unwrap())
        }

        /// Create a new `UnixNetif` for the given interface name
        pub const fn new(if_name: String) -> Self {
            Self(if_name)
        }

        pub fn get_conf(&self) -> Option<NetifConf> {
            get_if_conf(&self.0)
        }
    }

    impl Default for UnixNetif {
        fn default() -> Self {
            Self::new_default()
        }
    }

    impl Netif for UnixNetif {
        async fn get_conf(&self) -> Result<Option<NetifConf>, rs_matter::error::Error> {
            Ok(UnixNetif::get_conf(self))
        }

        async fn wait_conf_change(&self) -> Result<(), rs_matter::error::Error> {
            // Just poll every two seconds
            Timer::after(Duration::from_secs(2)).await;

            Ok(())
        }
    }

    impl edge_nal::UdpBind for UnixNetif {
        type Error = std::io::Error;

        type Socket<'a> = edge_nal_std::UdpSocket where Self: 'a;

        async fn bind(&self, addr: core::net::SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
            edge_nal_std::Stack::new().bind(addr).await
        }
    }

    fn default_if() -> Option<String> {
        // A quick and dirty way to get a network interface that has a link-local IPv6 address assigned as well as a non-loopback IPv4
        // Most likely, this is the interface we need
        // (as opposed to all the docker and libvirt interfaces that might be assigned on the machine and which seem by default to be IPv4 only)
        nix::ifaddrs::getifaddrs()
            .unwrap()
            .filter(|ia| {
                // Only take interfaces which are up, which are not PTP or loopback
                // and which have IPv4 IP assigned
                ia.flags
                    .contains(InterfaceFlags::IFF_UP | InterfaceFlags::IFF_BROADCAST)
                    && !ia
                        .flags
                        .intersects(InterfaceFlags::IFF_LOOPBACK | InterfaceFlags::IFF_POINTOPOINT)
                    && ia
                        .address
                        .and_then(|addr| addr.as_sockaddr_in().map(|_| ()))
                        .is_some()
            })
            .map(|ia| ia.interface_name)
            .find(|ifname| {
                // Only take interfaces which have an IPv4 address
                nix::ifaddrs::getifaddrs()
                    .unwrap()
                    .filter(|ia2| &ia2.interface_name == ifname)
                    .any(|ia2| {
                        ia2.address
                            .and_then(|addr| addr.as_sockaddr_in6().map(SockaddrIn6::ip))
                            .filter(|ip| ip.octets()[..2] == [0xfe, 0x80])
                            .is_some()
                    })
            })
    }

    fn get_if_conf(if_name: &str) -> Option<NetifConf> {
        extract_if_conf(
            nix::ifaddrs::getifaddrs()
                .unwrap()
                .filter(|ia| ia.interface_name == if_name)
                .filter_map(|ia| ia.address),
        )
    }

    fn extract_if_conf(addrs: impl Iterator<Item = SockaddrStorage>) -> Option<NetifConf> {
        let mut ipv4 = None;
        let mut ipv6 = None;
        let mut interface = None;
        let mut mac = None;

        for addr in addrs {
            if let Some(addr_ipv4) = addr.as_sockaddr_in() {
                ipv4 = Some(addr_ipv4.ip().into());
            } else if let Some(addr_ipv6) = addr.as_sockaddr_in6() {
                ipv6 = Some(addr_ipv6.ip());
            } else if let Some(addr_link) = addr.as_link_addr() {
                if mac.is_none() {
                    mac = addr_link.addr();
                }

                if interface.is_none() {
                    interface = Some(addr_link.ifindex() as _);
                }
            }
        }

        Some(NetifConf {
            ipv4: ipv4?,
            ipv6: ipv6?,
            interface: interface?,
            mac: mac?,
        })
    }
}
