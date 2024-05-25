use core::fmt;
use core::net::{Ipv4Addr, Ipv6Addr};
use core::pin::pin;

use alloc::sync::Arc;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};

use rs_matter::utils::notification::Notification;

use crate::error::Error;

const TIMEOUT_PERIOD_SECS: u8 = 5;

/// Async trait for accessing the `EspNetif` network interface (netif) of a driver.
///
/// Allows sharing the network interface between multiple tasks, where one task
/// may be waiting for the network interface to be ready, while the other might
/// be mutably operating on the L2 driver below the netif, or on the netif itself.
pub trait Netif {
    fn get_conf(&self) -> Option<NetifConf>;

    async fn wait_conf_change(&self);

    // /// Waits until the network interface is available and then
    // /// calls the provided closure with a reference to the network interface.
    // async fn with_netif<F, R>(&self, f: F) -> R
    // where
    //     F: FnOnce(&EspNetif) -> R;

    // /// Waits until a certain condition `f` becomes `Some` for a network interface
    // /// and then returns the result.
    // ///
    // /// The condition is checked every 5 seconds and every time a new `IpEvent` is
    // /// generated on the ESP IDF system event loop.
    // ///
    // /// The main use case of this method is to wait and listen the netif for changes
    // /// (netif up/down, IP address changes, etc.)
    // async fn wait<F, R>(&self, sysloop: EspSystemEventLoop, mut f: F) -> Result<R, Error>
    // where
    //     F: FnMut(&EspNetif) -> Result<Option<R>, Error>,
    // {
    //     let notification = Arc::new(Notification::<EspRawMutex>::new());

    //     let _subscription = {
    //         let notification = notification.clone();

    //         sysloop.subscribe::<IpEvent, _>(move |_| {
    //             notification.notify();
    //         })
    //     }?;

    //     loop {
    //         if let Some(result) = self.with_netif(&mut f).await? {
    //             break Ok(result);
    //         }

    //         let mut events = pin!(notification.wait());
    //         let mut timer = pin!(Timer::after(Duration::from_secs(TIMEOUT_PERIOD_SECS as _)));

    //         select(&mut events, &mut timer).await;
    //     }
    // }
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
