//! Types and traits for wireless network commissioning and operation

use core::fmt::{self, Display};
use core::marker::PhantomData;

use rs_matter::data_model::sdm::nw_commissioning::{
    AddThreadNetworkRequest, AddWifiNetworkRequest, ThreadInterfaceScanResult,
    WiFiInterfaceScanResult,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, OctetsOwned, ToTLV};
use rs_matter::transport::network::btp::GattPeripheral;

/// A trait representing the credentials of a wireless network (Wifi or Thread).
pub trait NetworkCredentials:
    for<'a> TryFrom<&'a AddWifiNetworkRequest<'a>, Error = Error>
    + for<'a> TryFrom<&'a AddThreadNetworkRequest<'a>, Error = Error>
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

/// A trait representing a wireless interface statistics.
pub trait WirelessStats {
    /// The type of the statistics (they are different for Wifi vs Thread)
    type Stats;

    fn stats(&self) -> Self::Stats;
}

impl<T> WirelessStats for &mut T
where
    T: WirelessStats,
{
    type Stats = T::Stats;

    fn stats(&self) -> Self::Stats {
        T::stats(*self)
    }
}

impl<T> WirelessStats for &T
where
    T: WirelessStats,
{
    type Stats = T::Stats;

    fn stats(&self) -> Self::Stats {
        T::stats(*self)
    }
}

/// A trait representing a wireless controller for either Wifi or Thread networks.
pub trait WirelessController {
    /// The type of the network credentials (e.g. WifiCredentials or ThreadCredentials)
    type NetworkCredentials: NetworkCredentials;

    /// The type of the scan result (e.g. WiFiInterfaceScanResult or ThreadInterfaceScanResult)
    type ScanResult;

    /// Return `true` if this wireless controller can support simultaneous connections
    /// to the operational network (Wifi or Thread) on one hand, and the BLE commissioning network
    /// on the other.
    fn supports_concurrent_connection(&self) -> bool;

    /// Scan for available networks
    async fn scan<F>(&mut self, network_id: Option<&[u8]>, callback: F) -> Result<(), Error>
    where
        F: FnMut(Option<&Self::ScanResult>);

    /// Connect to a network
    async fn connect(&mut self, creds: &Self::NetworkCredentials) -> Result<(), Error>;

    /// Return the network ID of the currently connected network, if any
    async fn connected_network(
        &mut self,
    ) -> Result<Option<&<Self::NetworkCredentials as NetworkCredentials>::NetworkId>, Error>;
}

impl<T> WirelessController for &mut T
where
    T: WirelessController,
{
    type NetworkCredentials = T::NetworkCredentials;
    type ScanResult = T::ScanResult;

    fn supports_concurrent_connection(&self) -> bool {
        T::supports_concurrent_connection(*self)
    }

    async fn scan<F>(&mut self, network_id: Option<&[u8]>, callback: F) -> Result<(), Error>
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
    ) -> Result<Option<&<Self::NetworkCredentials as NetworkCredentials>::NetworkId>, Error> {
        T::connected_network(*self).await
    }
}

/// A no-op controller for a wireless interface which is not started yet.
///
/// Useful when the commissioning mode is non-concurrent.
pub struct DisconnectedController<T, S>(PhantomData<fn() -> T>, PhantomData<fn() -> S>);

impl<T, S> Default for DisconnectedController<T, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, S> DisconnectedController<T, S> {
    /// Create a new disconnected controller
    pub const fn new() -> Self {
        Self(PhantomData, PhantomData)
    }
}

impl<'a> DisconnectedController<WifiCredentials, WiFiInterfaceScanResult<'a>> {
    /// Create a new disconnected controller for Wifi networks
    pub const fn new_wifi() -> Self {
        Self::new()
    }
}

impl<'a> DisconnectedController<ThreadCredentials, ThreadInterfaceScanResult<'a>> {
    /// Create a new disconnected controller for Thread networks
    pub const fn new_thread() -> Self {
        Self::new()
    }
}

impl<T, S> WirelessController for DisconnectedController<T, S>
where
    T: NetworkCredentials,
{
    type NetworkCredentials = T;
    type ScanResult = S;

    fn supports_concurrent_connection(&self) -> bool {
        // Disconnected controllers obvious do not support concurrent connections
        false
    }

    async fn scan<F>(&mut self, _network_id: Option<&[u8]>, _callback: F) -> Result<(), Error>
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

    async fn connected_network(&mut self) -> Result<Option<&T::NetworkId>, Error> {
        Ok(None)
    }
}

/// Statistics for a wireless interface which is not started yet.
///
/// Useful when the commissioning mode is non-concurrent.
pub struct DisconnectedStats<T>(T);

impl<T> DisconnectedStats<T> {
    /// Create a new disconnected stats
    pub const fn new(stats: T) -> Self {
        Self(stats)
    }
}

impl<T> WirelessStats for DisconnectedStats<T>
where
    T: Clone,
{
    type Stats = T;

    fn stats(&self) -> Self::Stats {
        self.0.clone()
    }
}

/// A trait representing the BLE radioof the device
pub trait Ble {
    type Paripheral<'a>: GattPeripheral
    where
        Self: 'a;

    /// Setup the radio to operate in BLE mode and return a GATT peripheral configured as per the
    /// requirements of the `rs-matter` BTP implementation. Necessary during commissioning.
    async fn peripheral(&mut self) -> Self::Paripheral<'_>;
}

impl<T> Ble for &mut T
where
    T: Ble,
{
    type Paripheral<'a> = T::Paripheral<'a> where Self: 'a;

    async fn peripheral(&mut self) -> Self::Paripheral<'_> {
        T::peripheral(*self).await
    }
}
