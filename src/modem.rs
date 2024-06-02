use edge_nal::UdpBind;

use embedded_svc::wifi::{asynch::Wifi, AccessPointInfo, Capability, Configuration};

use enumset::EnumSet;

use rs_matter::transport::network::btp::GattPeripheral;

use crate::netif::Netif;

#[cfg(feature = "alloc")]
extern crate alloc;

/// A trait representing the radio of the device, which can operate either in BLE mode, or in Wifi mode.
pub trait Modem {
    type BleDevice<'a>: GattPeripheral
    where
        Self: 'a;

    type WifiDevice<'a>: WifiDevice
    where
        Self: 'a;

    /// Setup the radio to operate in BLE mode and return a GATT peripheral configured as per the
    /// requirements of the `rs-matter` BTP implementation. Necessary during commissioning.
    async fn ble(&mut self) -> Self::BleDevice<'_>;

    /// Setup the radio to operate in Wifi mode and return a Wifi device that can be further split
    /// into an "L2" portion controlling the Wifi connection aspects of the network stack, and an
    /// "L3" portion controlling the IP connection aspects of the network stack.
    /// Necessary during Matter operational mode.
    async fn wifi(&mut self) -> Self::WifiDevice<'_>;
}

impl<T> Modem for &mut T
where
    T: Modem,
{
    type BleDevice<'a> = T::BleDevice<'a> where Self: 'a;
    type WifiDevice<'a> = T::WifiDevice<'a> where Self: 'a;

    async fn ble(&mut self) -> Self::BleDevice<'_> {
        T::ble(*self).await
    }

    async fn wifi(&mut self) -> Self::WifiDevice<'_> {
        T::wifi(*self).await
    }
}

// A trait represeting the Wifi device, which can be split into an L2 and L3 portion.
pub trait WifiDevice {
    type L2<'a>: Wifi
    where
        Self: 'a;

    type L3<'a>: Netif + UdpBind
    where
        Self: 'a;

    async fn split(&mut self) -> (Self::L2<'_>, Self::L3<'_>);
}

impl<T> WifiDevice for &mut T
where
    T: WifiDevice,
{
    type L2<'a> = T::L2<'a> where Self: 'a;
    type L3<'a> = T::L3<'a> where Self: 'a;

    async fn split(&mut self) -> (Self::L2<'_>, Self::L3<'_>) {
        T::split(*self).await
    }
}

/// A dummy modem implementation that can be used for testing purposes.
///
/// The "dummy" aspects of this implementation are related to the Wifi network:
/// - The L2 (Wifi) network is simulated by a dummy network interface that does not actually connect to Wifi networks.
/// - The L3 (IP) network is not simulated and is expected to be user-provided.
/// - The BLE network is not simulated and is expected to be user-provided as well,
///   because - without a functioning BLE stack - the device cannot even be commissioned.
///
/// On Linux, the BlueR-based BLE gatt peripheral can be used.
pub struct DummyModem<N, B> {
    netif_factory: N,
    bt_factory: B,
}

impl<N, B> DummyModem<N, B> {
    /// Create a new `DummyModem` with the given configuration, UDP `bind` stack and BLE peripheral factory
    pub const fn new(netif_factory: N, bt_factory: B) -> Self {
        Self {
            netif_factory,
            bt_factory,
        }
    }
}

impl<N, B, I, G> Modem for DummyModem<N, B>
where
    N: FnMut() -> I,
    B: FnMut() -> G,
    I: Netif + UdpBind + 'static,
    G: GattPeripheral,
{
    type BleDevice<'t> = G where Self: 't;

    type WifiDevice<'t> = DummyWifiDevice<I> where Self: 't;

    async fn ble(&mut self) -> Self::BleDevice<'_> {
        (self.bt_factory)()
    }

    async fn wifi(&mut self) -> Self::WifiDevice<'_> {
        DummyWifiDevice((self.netif_factory)())
    }
}

/// A dummy Wifi device.
///
/// The "dummy" aspects of this implementation are related to the Wifi network:
/// - The L2 (Wifi) network is simulated by a dummy network interface that does not actually connect to Wifi networks.
/// - The L3 (IP) network is simulated by a `DummyNetif` instance that uses a hard-coded `NetifConf` and assumes the
///   netif is always up
pub struct DummyWifiDevice<N>(N);

impl<N> WifiDevice for DummyWifiDevice<N>
where
    N: Netif + UdpBind,
{
    type L2<'t> = DummyL2 where Self: 't;

    type L3<'t> = &'t N where Self: 't;

    async fn split(&mut self) -> (Self::L2<'_>, Self::L3<'_>) {
        (DummyL2::new(), &self.0)
    }
}

/// A dummy L2 Wifi device that does not actually connect to Wifi networks
/// - yet - happily returns `Ok(())` to everything.
pub struct DummyL2 {
    conf: Configuration,
    started: bool,
    connected: bool,
}

impl DummyL2 {
    const fn new() -> Self {
        Self {
            conf: Configuration::None,
            started: false,
            connected: false,
        }
    }
}

impl Wifi for DummyL2 {
    type Error = core::convert::Infallible;

    async fn get_capabilities(&self) -> Result<EnumSet<Capability>, Self::Error> {
        Ok(EnumSet::empty())
    }

    async fn get_configuration(&self) -> Result<Configuration, Self::Error> {
        Ok(self.conf.clone())
    }

    async fn set_configuration(&mut self, conf: &Configuration) -> Result<(), Self::Error> {
        self.conf = conf.clone();

        Ok(())
    }

    async fn start(&mut self) -> Result<(), Self::Error> {
        self.started = true;

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Self::Error> {
        self.started = false;

        Ok(())
    }

    async fn connect(&mut self) -> Result<(), Self::Error> {
        self.connected = true;

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), Self::Error> {
        self.connected = false;

        Ok(())
    }

    async fn is_started(&self) -> Result<bool, Self::Error> {
        Ok(self.started)
    }

    async fn is_connected(&self) -> Result<bool, Self::Error> {
        Ok(self.connected)
    }

    async fn scan_n<const N: usize>(
        &mut self,
    ) -> Result<(heapless::Vec<AccessPointInfo, N>, usize), Self::Error> {
        Ok((heapless::Vec::new(), 0))
    }

    #[cfg(feature = "alloc")]
    async fn scan(&mut self) -> Result<alloc::vec::Vec<AccessPointInfo>, Self::Error> {
        Ok(alloc::vec::Vec::new())
    }
}

/// An instantiation of `DummyModem` for Linux specifically,
/// that uses the `UnixNetif` instance and the BlueR GATT peripheral from `rs-matter`.
#[cfg(all(feature = "std", target_os = "linux"))]
pub type DummyLinuxModem = DummyModem<
    fn() -> crate::netif::UnixNetif,
    fn() -> rs_matter::transport::network::btp::BuiltinGattPeripheral,
>;

#[cfg(all(feature = "std", target_os = "linux"))]
impl Default for DummyLinuxModem {
    fn default() -> Self {
        Self::new(crate::netif::UnixNetif::new_default, || {
            rs_matter::transport::network::btp::BuiltinGattPeripheral::new(None)
        })
    }
}
