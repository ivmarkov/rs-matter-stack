use edge_nal::UdpBind;

use embedded_svc::wifi::asynch::Wifi;

use rs_matter::transport::network::btp::GattPeripheral;

use crate::netif::Netif;

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
