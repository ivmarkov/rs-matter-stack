//! Types and traits for wireless network commissioning and operation

use core::fmt::{self, Display};
use core::marker::PhantomData;

use edge_nal::UdpBind;
use rs_matter::data_model::sdm::nw_commissioning::{
    AddThreadNetworkRequest, AddWifiNetworkRequest, ThreadInterfaceScanResult,
    WiFiInterfaceScanResult,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, OctetsOwned, ToTLV};
use rs_matter::transport::network::btp::GattPeripheral;

use crate::netif::Netif;

/// A trait representing the credentials of a wireless network (Wifi or Thread).
pub trait NetworkCredentials:
    for<'a> TryFrom<&'a AddWifiNetworkRequest<'a>, Error = Error>
    + for<'a> TryFrom<&'a AddThreadNetworkRequest<'a>, Error = Error>
    + Clone
    + 'static
{
    /// The ID of the network (SSID for Wifi and Extended PAN ID for Thread)
    type NetworkId: Display
        + Clone
        + PartialEq
        + AsRef<[u8]>
        + for<'a> TryFrom<&'a [u8], Error = Error>
        + 'static;

    /// Return `true` if these credentials are for a Wifi network
    fn is_wifi() -> bool;

    /// Return the network ID
    fn network_id(&self) -> &Self::NetworkId;
}

/// Concrete Network ID type for Wifi networks
#[derive(Debug, Clone, PartialEq, FromTLV, ToTLV)]
pub struct WifiSsid(pub heapless::String<32>);

impl TryFrom<&[u8]> for WifiSsid {
    type Error = Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let str = core::str::from_utf8(value).map_err(|_| ErrorCode::InvalidData)?;
        let ssid = heapless::String::try_from(str).map_err(|_| ErrorCode::NoSpace)?;

        Ok(Self(ssid))
    }
}

impl AsRef<[u8]> for WifiSsid {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Display for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SSID::{}", self.0)
    }
}

/// A struct implementing the `NetworkCredentials` trait for Wifi networks.
#[derive(Debug, Clone, ToTLV, FromTLV)]
pub struct WifiCredentials {
    pub ssid: WifiSsid,
    pub password: heapless::String<64>,
}

impl TryFrom<&AddWifiNetworkRequest<'_>> for WifiCredentials {
    type Error = Error;

    fn try_from(value: &AddWifiNetworkRequest) -> Result<Self, Self::Error> {
        let ssid = WifiSsid::try_from(value.ssid.0)?;

        let password =
            core::str::from_utf8(value.credentials.0).map_err(|_| ErrorCode::InvalidData)?;
        let password = heapless::String::try_from(password).map_err(|_| ErrorCode::InvalidData)?;

        Ok(Self { ssid, password })
    }
}

impl TryFrom<&AddThreadNetworkRequest<'_>> for WifiCredentials {
    type Error = Error;

    fn try_from(_value: &AddThreadNetworkRequest) -> Result<Self, Self::Error> {
        Err(ErrorCode::InvalidCommand.into())
    }
}

impl NetworkCredentials for WifiCredentials {
    type NetworkId = WifiSsid;

    fn is_wifi() -> bool {
        true
    }

    fn network_id(&self) -> &Self::NetworkId {
        &self.ssid
    }
}

/// Concrete Network ID type for Thread networks
#[derive(Debug, Clone, PartialEq, FromTLV, ToTLV)]
pub struct ThreadId(pub OctetsOwned<8>);

impl TryFrom<&[u8]> for ThreadId {
    type Error = Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() != 8 {
            Err(ErrorCode::InvalidData)?;
        }

        let mut octets = OctetsOwned::new();
        octets.vec.extend_from_slice(value).unwrap();

        Ok(Self(octets))
    }
}

impl AsRef<[u8]> for ThreadId {
    fn as_ref(&self) -> &[u8] {
        &self.0.vec
    }
}

impl Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "EPAN ID::{:x}",
            u64::from_le_bytes(self.0.vec.clone().into_array().unwrap())
        )
    }
}

/// A struct implementing the `NetworkCredentials` trait for Thread networks.
#[derive(Debug, Clone, ToTLV, FromTLV)]
pub struct ThreadCredentials {
    pub op_dataset: rs_matter::utils::storage::Vec<u8, 256>,
}

impl TryFrom<&AddWifiNetworkRequest<'_>> for ThreadCredentials {
    type Error = Error;

    fn try_from(_value: &AddWifiNetworkRequest) -> Result<Self, Self::Error> {
        Err(ErrorCode::InvalidCommand.into())
    }
}

impl TryFrom<&AddThreadNetworkRequest<'_>> for ThreadCredentials {
    type Error = Error;

    fn try_from(value: &AddThreadNetworkRequest) -> Result<Self, Self::Error> {
        let op_dataset = rs_matter::utils::storage::Vec::try_from(value.op_dataset.0)
            .map_err(|_| ErrorCode::NoSpace)?;

        Ok(Self { op_dataset })
    }
}

impl NetworkCredentials for ThreadCredentials {
    type NetworkId = ThreadId;

    fn is_wifi() -> bool {
        false
    }

    fn network_id(&self) -> &Self::NetworkId {
        todo!()
    }
}

/// A trait representing a wireless controller for either Wifi or Thread networks.
pub trait Controller: WirelessDTOs {
    /// Scan for available networks
    async fn scan<F>(
        &mut self,
        network_id: Option<&<Self::NetworkCredentials as NetworkCredentials>::NetworkId>,
        callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&Self::ScanResult>);

    /// Connect to a network
    async fn connect(&mut self, creds: &Self::NetworkCredentials) -> Result<(), Error>;

    /// Return the network ID of the currently connected network, if any
    async fn connected_network(
        &mut self,
    ) -> Result<Option<<Self::NetworkCredentials as NetworkCredentials>::NetworkId>, Error>;

    /// Return the current statistics of the wireless interface
    async fn stats(&mut self) -> Result<Self::Stats, Error>;
}

impl<T> Controller for &mut T
where
    T: Controller,
{
    async fn scan<F>(
        &mut self,
        network_id: Option<&<Self::NetworkCredentials as NetworkCredentials>::NetworkId>,
        callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&Self::ScanResult>),
    {
        T::scan(*self, network_id, callback).await
    }

    async fn connect(&mut self, creds: &Self::NetworkCredentials) -> Result<(), Error> {
        T::connect(*self, creds).await
    }

    async fn connected_network(
        &mut self,
    ) -> Result<Option<<Self::NetworkCredentials as NetworkCredentials>::NetworkId>, Error> {
        T::connected_network(*self).await
    }

    async fn stats(&mut self) -> Result<Self::Stats, Error> {
        T::stats(*self).await
    }
}

/// A no-op controller.
///
/// Useful for simulating non-concurrent wireless connectivity in tests and examples.
pub struct DisconnectedController<T, S, A>(PhantomData<fn() -> T>, PhantomData<fn() -> S>, A);

impl<T, S, A> DisconnectedController<T, S, A> {
    /// Create a new disconnected controller
    pub const fn new(stats: A) -> Self {
        Self(PhantomData, PhantomData, stats)
    }
}

impl<'a, A> DisconnectedController<WifiCredentials, WiFiInterfaceScanResult<'a>, A> {
    /// Create a new disconnected controller for Wifi networks
    pub const fn new_wifi(stats: A) -> Self {
        Self::new(stats)
    }
}

impl<'a, A> DisconnectedController<ThreadCredentials, ThreadInterfaceScanResult<'a>, A> {
    /// Create a new disconnected controller for Thread networks
    pub const fn new_thread(stats: A) -> Self {
        Self::new(stats)
    }
}

impl<T, S, A> WirelessDTOs for DisconnectedController<T, S, A>
where
    T: NetworkCredentials,
    S: Clone,
    A: Clone,
{
    type NetworkCredentials = T;
    type ScanResult = S;
    type Stats = A;

    fn supports_concurrent_connection(&self) -> bool {
        // Disconnected controllers obvious do not support concurrent connections
        false
    }
}

impl<T, S, A> Controller for DisconnectedController<T, S, A>
where
    T: NetworkCredentials,
    S: Clone,
    A: Clone,
{
    async fn scan<F>(
        &mut self,
        _network_id: Option<&T::NetworkId>,
        _callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&S>),
    {
        // Scan requests should not arrive in non-concurrent commissioning workflow
        Err(ErrorCode::InvalidCommand.into())
    }

    async fn connect(&mut self, _creds: &T) -> Result<(), Error> {
        // In non-concurrent commissioning workflow, we pretend to connect to the network
        // but this is of course not possible to do for real.
        Ok(())
    }

    async fn connected_network(&mut self) -> Result<Option<T::NetworkId>, Error> {
        Ok(None)
    }

    async fn stats(&mut self) -> Result<A, Error> {
        Ok(self.2.clone())
    }
}

/// A trait representing all DTOs required for wireless network commissioning and operation.
pub trait WirelessDTOs {
    /// The type of the network credentials (e.g. WifiCredentials or ThreadCredentials)
    type NetworkCredentials: NetworkCredentials + Clone;

    /// The type of the scan result (e.g. WiFiInter faceScanResult or ThreadInterfaceScanResult)
    type ScanResult: Clone;

    /// The type of the statistics (they are different for Wifi vs Thread)
    type Stats;

    /// Return `true` if this wireless interface can support simultaneous connections
    /// to the operational network (Wifi or Thread) on one hand, and the BLE commissioning network
    /// on the other.
    // TODO: Make it async
    fn supports_concurrent_connection(&self) -> bool;
}

/// A factory trait for constructing the wireless controller and its network interface
pub trait Wireless: WirelessDTOs {
    /// The type of the controller
    type Controller<'a>: Controller<
        NetworkCredentials = Self::NetworkCredentials,
        ScanResult = Self::ScanResult,
        Stats = Self::Stats,
    >
    where
        Self: 'a;

    /// The type of the network interface
    type Netif<'a>: Netif + UdpBind
    where
        Self: 'a;

    /// Setup the radio to operate in wireless (Wifi or Thread) mode and return the wireless controller
    /// and the network interface.
    async fn start(&mut self) -> Result<(Self::Controller<'_>, Self::Netif<'_>), Error>;
}

impl<T> WirelessDTOs for &mut T
where
    T: WirelessDTOs,
{
    type NetworkCredentials = T::NetworkCredentials;
    type ScanResult = T::ScanResult;
    type Stats = T::Stats;

    fn supports_concurrent_connection(&self) -> bool {
        T::supports_concurrent_connection(*self)
    }
}

impl<T> Wireless for &mut T
where
    T: Wireless,
{
    type Controller<'a> = T::Controller<'a> where Self: 'a;
    type Netif<'a> = T::Netif<'a> where Self: 'a;

    async fn start(&mut self) -> Result<(Self::Controller<'_>, Self::Netif<'_>), Error> {
        T::start(self).await
    }
}

/// A factory trait for constructing the BTP Gatt peripheral for the device
pub trait Ble {
    type Peripheral<'a>: GattPeripheral
    where
        Self: 'a;

    /// Setup the radio to operate in BLE mode and return a GATT peripheral configured as per the
    /// requirements of the `rs-matter` BTP implementation. Necessary during commissioning.
    async fn start(&mut self) -> Result<Self::Peripheral<'_>, Error>;
}

impl<T> Ble for &mut T
where
    T: Ble,
{
    type Peripheral<'a> = T::Peripheral<'a> where Self: 'a;

    async fn start(&mut self) -> Result<Self::Peripheral<'_>, Error> {
        T::start(*self).await
    }
}
