use edge_nal::UdpBind;

use embedded_svc::wifi::asynch::Wifi;

use rs_matter::transport::network::btp::GattPeripheral;

use crate::netif::Netif;

/// A trait representing the radio of the device, which can operate either in BLE mode, or in Wifi mode.
pub trait Modem {
    type Gatt<'a>: GattPeripheral
    where
        Self: 'a;
    type Wifi<'a>: Wifi
    where
        Self: 'a;
    type Netif<'a>: Netif + UdpBind
    where
        Self: 'a;

    /// Setup the radio to operate in BLE mode and return a GATT peripheral configured as per the
    /// requirements of the `rs-matter` BTP implementation. Necessary during commissioning.
    fn gatt(&mut self) -> Self::Gatt<'_>;

    /// Setup the radio to operate in Wifi mode and return:
    /// - A `Wifi` trait instance, so that the stack can configure and operate the Wifi according to the
    ///   provisioned networks
    /// - A `Netif` + `UdpBind` instance, so that the Matter stack can listen to changes in the IP layer
    ///   running on top of the wifi radio, as well as bind to UDP sockets in this IP layer
    ///   when running in operational mode
    fn wifi_netif(&mut self) -> (Self::Wifi<'_>, Self::Netif<'_>);
}

impl<T> Modem for &mut T
where
    T: Modem,
{
    type Gatt<'a> = T::Gatt<'a> where Self: 'a;
    type Wifi<'a> = T::Wifi<'a> where Self: 'a;
    type Netif<'a> = T::Netif<'a> where Self: 'a;

    fn gatt(&mut self) -> Self::Gatt<'_> {
        T::gatt(*self)
    }

    fn wifi_netif(&mut self) -> (Self::Wifi<'_>, Self::Netif<'_>) {
        T::wifi_netif(*self)
    }
}
