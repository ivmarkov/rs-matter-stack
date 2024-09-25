
use edge_nal::UdpBind;

use rs_matter::transport::network::btp::GattPeripheral;

use crate::netif::Netif;
use crate::wireless::{WirelessController, WirelessStats};

#[cfg(feature = "alloc")]
extern crate alloc;

/// A trait representing the Wifi/Thread radio of the device
pub trait WirelessModem {
    type WirelessDevice<'a>: WirelessDevice
    where
        Self: 'a;

    /// Setup the radio to operate in Wifi/Thread mode and return a Wireless device that can be further split
    /// into an "L2" portion controlling the Wifi/Thread connection aspects of the network stack, and an
    /// "L3" portion controlling the IP connection aspects of the network stack.
    /// Necessary during Matter operational mode.
    async fn wireless(&mut self) -> Self::WirelessDevice<'_>;
}

impl<T> WirelessModem for &mut T
where
    T: WirelessModem,
{
    type WirelessDevice<'a> = T::WirelessDevice<'a> where Self: 'a;

    async fn wireless(&mut self) -> Self::WirelessDevice<'_> {
        T::wireless(*self).await
    }
}

// A trait represeting the Wireless device, which can be split into an L2 and L3 portion.
pub trait WirelessDevice {
    type Controller<'a>: WirelessController
    where
        Self: 'a;

    type Stats<'a>: WirelessStats
    where
        Self: 'a;

    type Netif<'a>: Netif + UdpBind
    where
        Self: 'a;

    async fn split(&mut self) -> (Self::Controller<'_>, Self::Stats<'_>, Self::Netif<'_>);
}

impl<T> WirelessDevice for &mut T
where
    T: WirelessDevice,
{
    type Controller<'a> = T::Controller<'a> where Self: 'a;

    type Stats<'a> = T::Stats<'a> where Self: 'a;

    type Netif<'a> = T::Netif<'a> where Self: 'a;

    async fn split(&mut self) -> (Self::Controller<'_>, Self::Stats<'_>, Self::Netif<'_>) {
        T::split(*self).await
    }
}

// /// A dummy modem implementation that can be used for testing purposes.
// ///
// /// The "dummy" aspects of this implementation are related to the Wifi network:
// /// - The L2 (Wifi/Thread) network is simulated by a dummy network interface that does not actually connect to Wifi/Thread networks.
// /// - The L3 (IP) network is not simulated and is expected to be user-provided.
// /// - The BLE network is not simulated and is expected to be user-provided as well,
// ///   because - without a functioning BLE stack - the device cannot even be commissioned.
// ///
// /// On Linux, the BlueR-based BLE gatt peripheral can be used.
// pub struct DummyModem<N, B> {
//     netif_factory: N,
//     bt_factory: B,
// }

// impl<N, B> DummyModem<N, B> {
//     /// Create a new `DummyModem` with the given configuration, UDP `bind` stack and BLE peripheral factory
//     pub const fn new(netif_factory: N, bt_factory: B) -> Self {
//         Self {
//             netif_factory,
//             bt_factory,
//         }
//     }
// }

// impl<N, B, I, G> WirelessModem for DummyModem<N, B>
// where
//     N: FnMut() -> I,
//     B: FnMut() -> G,
//     I: Netif + UdpBind + 'static,
//     G: GattPeripheral,
// {
//     type WirelessDevice<'t> = DummyWirelessDevice<T, I> where Self: 't;

//     async fn wireless(&mut self) -> Self::WirelessDevice<'_> {
//         DummyWirelessDevice((self.netif_factory)(), PhantomData)
//     }
// }

// impl<N, B, I, G> BleModem for DummyModem<N, B>
// where
//     N: FnMut() -> I,
//     B: FnMut() -> G,
//     I: Netif + UdpBind + 'static,
//     G: GattPeripheral,
// {
//     type BleDevice<'t> = G where Self: 't;

//     async fn ble(&mut self) -> Self::BleDevice<'_> {
//         (self.bt_factory)()
//     }
// }

// /// A dummy wireless device. TODO
// ///
// /// The "dummy" aspects of this implementation are related to the Wifi network:
// /// - The L2 (Wifi/Thread) network is using a disconnected controller and statistics
// /// - The L3 (IP) network is simulated by a `DummyNetif` instance that uses a hard-coded `NetifConf` and assumes the
// ///   netif is always up
// pub struct DummyWirelessDevice<T, S, R, N>(PhantomData<fn() -> T>, PhantomData<fn() -> S>, R, N);

// impl<'a, R, N> DummyWirelessDevice<WifiCredentials, WiFiInterfaceScanResult<'a>, R, N>
// where
//     R: Clone,
//     N: Netif + UdpBind,
// {
//     pub const fn new_wifi(stats: R, netif: N) -> Self {
//         Self(PhantomData, PhantomData, stats, netif)
//     }
// }

// impl<'a, R, N> DummyWirelessDevice<ThreadCredentials, ThreadInterfaceScanResult<'a>, R, N>
// where
//     R: Clone,
//     N: Netif + UdpBind,
// {
//     pub const fn new_thread(stats: R, netif: N) -> Self {
//         Self(PhantomData, PhantomData, stats, netif)
//     }
// }

// impl<T, S, R, N> WirelessDevice for DummyWirelessDevice<T, S, R, N>
// where
//     T: NetworkCredentials,
//     R: Clone,
//     N: Netif + UdpBind,
// {
//     type Controller<'t> = DisconnectedController<T, R> where Self: 't;

//     type Stats<'t> = DisconnectedStats<R> where Self: 't;

//     type Netif<'t> = &'t N where Self: 't;

//     async fn split(&mut self) -> (Self::Controller<'_>, Self::Stats<'_>, Self::Netif<'_>) {
//         (DisconnectedController::new(), DisconnectedStats::new(self.2.clone()), &self.3)
//     }
// }

// /// A dummy Controller wireless device that does not actually connect to Wifi/Thread networks
// /// - yet - happily pretends to.
// pub struct DummyController<T>(PhantomData<fn() -> T>);

// impl<T> DummyController<T> {
//     const fn new() -> Self {
//         Self(PhantomData)
//     }
// }

// impl<T> WirelessController<T> for DummyController<T> 
// where 
//     T: NetworkCredentials,
// {
//     async fn scan<F>(&mut self, _network_id: Option<&[u8]>, mut callback: F) -> Result<(), Error>
//     where 
//         F: FnMut(Option<&T::ScanResult>),
//     {
//         callback(None);

//         Ok(())
//     }

//     async fn connect(&mut self, _creds: &T) -> Result<(), Error> {
//         Ok(())
//     }

//     async fn connected_network(&mut self) -> Result<Option<&str>, Error> {
//         Ok(Some("DummyNetwork")) // TODO
//     }
// }

/// An instantiation of `DummyModem` for Linux specifically,
/// that uses the `UnixNetif` instance and the BlueR GATT peripheral from `rs-matter`.
#[cfg(all(feature = "os", feature = "nix", target_os = "linux"))]
pub type DummyLinuxModem = DummyModem<
    fn() -> crate::netif::UnixNetif,
    fn() -> rs_matter::transport::network::btp::BuiltinGattPeripheral,
>;

#[cfg(all(feature = "os", feature = "nix", target_os = "linux"))]
impl Default for DummyLinuxModem {
    fn default() -> Self {
        Self::new(crate::netif::UnixNetif::new_default, || {
            rs_matter::transport::network::btp::BuiltinGattPeripheral::new(None)
        })
    }
}
