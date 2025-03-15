//! Types and traits for wireless network commissioning and operation

use core::fmt::{self, Debug, Display};
use core::marker::PhantomData;

use edge_nal::UdpBind;
use rs_matter::data_model::sdm::nw_commissioning::{
    AddThreadNetworkRequest, AddWifiNetworkRequest, WiFiSecurity, WifiBand,
};
use rs_matter::data_model::sdm::wifi_nw_diagnostics::WifiNwDiagData;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, OctetsOwned, ToTLV};
use rs_matter::transport::network::btp::GattPeripheral;
use rs_matter::utils::storage::Vec;

use crate::netif::Netif;
use crate::private::Sealed;

/// A trait representing the credentials of a wireless network (Wifi or Thread).
///
/// The trait is sealed and has only two implementations: `WifiCredentials` and `ThreadCredentials`.
pub trait NetworkCredentials:
    Sealed
    + for<'a> TryFrom<&'a AddWifiNetworkRequest<'a>, Error = Error>
    + for<'a> TryFrom<&'a AddThreadNetworkRequest<'a>, Error = Error>
    + Clone
    + Debug
    + 'static
{
    /// The ID of the network (SSID for Wifi and Extended PAN ID for Thread)
    type NetworkId: Display
        + Clone
        + Debug
        + PartialEq
        + AsRef<[u8]>
        + for<'a> TryFrom<&'a [u8], Error = Error>
        + 'static;

    /// Return the network ID
    fn network_id(&self) -> Self::NetworkId;
}

/// Concrete Network ID type for Wifi networks
#[derive(Debug, Clone, PartialEq, FromTLV, ToTLV)]
pub struct WifiSsid(pub OctetsOwned<32>);

impl TryFrom<&[u8]> for WifiSsid {
    type Error = Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let ssid = Vec::try_from(value).map_err(|_| ErrorCode::NoSpace)?;

        Ok(Self(OctetsOwned { vec: ssid }))
    }
}

impl AsRef<[u8]> for WifiSsid {
    fn as_ref(&self) -> &[u8] {
        self.0.vec.as_slice()
    }
}

impl Display for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SSID::{:?}", core::str::from_utf8(self.0.vec.as_slice()))
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

impl Sealed for WifiCredentials {}

impl NetworkCredentials for WifiCredentials {
    type NetworkId = WifiSsid;

    fn network_id(&self) -> Self::NetworkId {
        self.ssid.clone()
    }
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
pub struct WifiScanResult {
    pub security: WiFiSecurity,
    pub ssid: WifiSsid,
    pub bssid: OctetsOwned<6>,
    pub channel: u16,
    pub band: Option<WifiBand>,
    pub rssi: Option<i8>,
}

/// Concrete Network ID type for Thread networks
#[derive(Debug, Clone, PartialEq, FromTLV, ToTLV)]
pub struct ThreadId(pub OctetsOwned<8>);

impl TryFrom<&[u8]> for ThreadId {
    type Error = Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() > 8 {
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
            "EPAN ID::0x{:08x}",
            u64::from_be_bytes(self.0.vec.clone().into_array().unwrap())
        )
    }
}

/// A struct implementing the `NetworkCredentials` trait for Thread networks.
#[derive(Debug, Clone, ToTLV, FromTLV)]
pub struct ThreadCredentials {
    pub op_dataset: rs_matter::utils::storage::Vec<u8, 256>,
}

impl ThreadCredentials {
    const EMPTY_ID: ThreadId = ThreadId(OctetsOwned { vec: Vec::new() });

    // Adopted from here:
    // https://gist.github.com/agners/0338576e0003318b63ec1ea75adc90f9
    fn ext_pan_id(&self) -> Option<ThreadId> {
        const EXT_PAN_ID: u8 = 2;

        let mut slice = self.op_dataset.as_slice();

        while !slice.is_empty() {
            if slice.len() < 2 {
                break;
            }

            let tlv_type = slice[0];
            let tlv_len = slice[1] as usize;

            let rest = &slice[2..];
            if tlv_len > rest.len() {
                break;
            }

            if tlv_type == EXT_PAN_ID {
                return Some(rest[..tlv_len].try_into().unwrap());
            }

            slice = rest;
        }

        None
    }
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

impl Sealed for ThreadCredentials {}

impl NetworkCredentials for ThreadCredentials {
    type NetworkId = ThreadId;

    fn network_id(&self) -> Self::NetworkId {
        self.ext_pan_id().unwrap_or(Self::EMPTY_ID)
    }
}

#[derive(Debug, Clone, FromTLV, ToTLV)]
pub struct ThreadScanResult {
    pub pan_id: u16,
    pub extended_pan_id: u64,
    pub network_name: heapless::String<32>, // TODO: Enough
    pub channel: u16,
    pub version: u8,
    pub extended_address: Vec<u8, 16>,
    pub rssi: i8,
    pub lqi: u8,
}

/// A trait representing a wireless controller for either Wifi or Thread networks.
pub trait Controller {
    /// The type of the wireless data (WifiData or ThreadData)
    type Data: WirelessData;

    /// Scan for available networks
    async fn scan<F>(
        &mut self,
        network_id: Option<
            &<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
        >,
        callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>) -> Result<(), Error>;

    /// Connect to a network
    async fn connect(
        &mut self,
        creds: &<Self::Data as WirelessData>::NetworkCredentials,
    ) -> Result<(), Error>;

    /// Return the network ID of the currently connected network, if any
    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId>,
        Error,
    >;

    /// Return the current statistics of the wireless interface
    async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error>;
}

impl<T> Controller for &mut T
where
    T: Controller,
{
    type Data = T::Data;

    async fn scan<F>(
        &mut self,
        network_id: Option<
            &<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
        >,
        callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>) -> Result<(), Error>,
    {
        T::scan(*self, network_id, callback).await
    }

    async fn connect(
        &mut self,
        creds: &<Self::Data as WirelessData>::NetworkCredentials,
    ) -> Result<(), Error> {
        T::connect(*self, creds).await
    }

    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId>,
        Error,
    > {
        T::connected_network(*self).await
    }

    async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
        T::stats(*self).await
    }
}

/// A no-op controller.
///
/// Useful for simulating non-concurrent wireless connectivity in tests and examples.
pub struct DisconnectedController<T>(PhantomData<T>)
where
    T: WirelessData,
    T::Stats: Default;

impl<T> Default for DisconnectedController<T>
where
    T: WirelessData,
    T::Stats: Default,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T> DisconnectedController<T>
where
    T: WirelessData,
    T::Stats: Default,
{
    /// Create a new disconnected controller
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Controller for DisconnectedController<T>
where
    T: WirelessData,
    T::ScanResult: Clone,
    T::Stats: Default,
{
    type Data = T;

    async fn scan<F>(
        &mut self,
        _network_id: Option<
            &<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
        >,
        _callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>) -> Result<(), Error>,
    {
        // Scan requests should not arrive in non-concurrent commissioning workflow
        Err(ErrorCode::InvalidCommand.into())
    }

    async fn connect(
        &mut self,
        _creds: &<Self::Data as WirelessData>::NetworkCredentials,
    ) -> Result<(), Error> {
        // In non-concurrent commissioning workflow, we pretend to connect to the network
        // but this is of course not possible to do for real.
        Ok(())
    }

    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId>,
        Error,
    > {
        Ok(None)
    }

    async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
        Ok(Default::default())
    }
}

/// A trait representing all DTOs required for wireless network commissioning and operation.
///
/// The trait is sealed and has only two implementations: `WifiData` and `ThreadData`.
pub trait WirelessData: Sealed + Debug + 'static {
    /// The type of the network credentials (e.g. WifiCredentials or ThreadCredentials)
    type NetworkCredentials: NetworkCredentials + Clone;

    /// The type of the scan result (e.g. WiFiInterfaceScanResult or ThreadInterfaceScanResult)
    type ScanResult: Debug + Clone;

    /// The type of the statistics (they are different for Wifi vs Thread)
    type Stats: Debug + Default;

    // Whether this wireless data is for Wifi networks (`true`) or Thread networks (`false`)
    const WIFI: bool;
}

/// A struct implementing the `WirelessData` trait for Wifi networks.
#[derive(Debug)]
pub struct WifiData;

impl Sealed for WifiData {}

impl WirelessData for WifiData {
    type NetworkCredentials = WifiCredentials;
    type ScanResult = WifiScanResult;
    type Stats = Option<WifiNwDiagData>;

    const WIFI: bool = true;
}

/// A struct implementing the `WirelessData` trait for Thread networks.
#[derive(Debug)]
pub struct ThreadData;

impl Sealed for ThreadData {}

impl WirelessData for ThreadData {
    type NetworkCredentials = ThreadCredentials;
    type ScanResult = ThreadScanResult;
    type Stats = ();

    const WIFI: bool = false;
}

/// A trait representing a wireless configuration, i.e. what data (Wifi or Thread) and
/// whether the wireless network should be used in concurrent commissioning mode or not.
///
/// The trait is sealed and has only two implementations: `Wifi<T>` and `Thread<T>`.
pub trait WirelessConfig: Sealed + 'static {
    /// The type of the wireless data (WifiData or ThreadData)
    type Data: WirelessData;

    /// Whether this wireless configuration supports concurrent commisioning
    /// (i.e. both BLE and Wifi/Thread radios active at the same time)
    const CONCURRENT: bool;
}

/// A marker trait for wireless configurations that will do concurrent commissioning
///
/// The trait is sealed and has only two implementations: `Wifi<()>` and `Thread<()>`.
pub trait ConcurrentWirelessConfig: WirelessConfig {}

/// A struct representing a Wifi wireless configuration
#[derive(Debug)]
pub struct Wifi<T = ()>(T);

impl<T: 'static> Sealed for Wifi<T> {}

impl<T: 'static> WirelessConfig for Wifi<T>
where
    T: ConcurrencyMode,
{
    type Data = WifiData;

    const CONCURRENT: bool = T::CONCURRENT;
}

impl ConcurrentWirelessConfig for Wifi {}

/// A struct representing a Thread wireless configuration
#[derive(Debug)]
pub struct Thread<T = ()>(T);

impl<T: 'static> Sealed for Thread<T> {}

impl<T: 'static> WirelessConfig for Thread<T>
where
    T: ConcurrencyMode,
{
    type Data = ThreadData;

    const CONCURRENT: bool = T::CONCURRENT;
}

impl ConcurrentWirelessConfig for Thread {}

/// A marker trait representing whether the wireless configuration
/// indicates whether concurrent or non-concurrent commissioning will be used.
///
/// The trait is sealed and has only two implementations:
/// - `()` which indicates a wireless configuration for concurrent provisioning
/// - `NC` which indicates a wireless configuration for non-concurrent provisioning
pub trait ConcurrencyMode: Sealed + 'static {
    const CONCURRENT: bool;
}

impl Sealed for () {}

impl ConcurrencyMode for () {
    const CONCURRENT: bool = true;
}

#[derive(Debug)]
pub struct NC;

impl Sealed for NC {}

impl ConcurrencyMode for NC {
    const CONCURRENT: bool = false;
}

/// A trait representing a task that needs access to the wireless interface
/// (Netif, UDP stack and Wireless controller) to perform its work.
pub trait WirelessTask {
    type Data: WirelessData;

    /// Run the task with the given network interface, UDP stack and wireless controller
    async fn run<N, U, C>(&mut self, netif: N, udp: U, controller: C) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind,
        C: Controller<Data = Self::Data>;
}

impl<T> WirelessTask for &mut T
where
    T: WirelessTask,
{
    type Data = T::Data;

    async fn run<N, U, C>(&mut self, netif: N, udp: U, controller: C) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind,
        C: Controller<Data = Self::Data>,
    {
        T::run(*self, netif, udp, controller).await
    }
}

/// A trait for running a task within a context where the wireless interface is initialized and operable
pub trait Wireless {
    /// The type of the wireless data (WifiData or ThreadData)
    type Data: WirelessData;

    /// Setup the radio to operate in wireless (Wifi or Thread) mode
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: WirelessTask<Data = Self::Data>;
}

impl<T> Wireless for &mut T
where
    T: Wireless,
{
    type Data = T::Data;

    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: WirelessTask<Data = Self::Data>,
    {
        T::run(self, task).await
    }
}

/// A utility type for running a wireless task with a pre-existing wireless interface
/// rather than bringing up / tearing down the wireless interface for the task.
pub struct PreexistingWireless<N, U, C> {
    netif: N,
    udp: U,
    controller: C,
}

impl<N, U, C> PreexistingWireless<N, U, C> {
    /// Create a new `PreexistingWireless` instance with the given network interface, UDP stack
    /// and wireless controller
    pub const fn new(netif: N, udp: U, controller: C) -> Self {
        Self {
            netif,
            udp,
            controller,
        }
    }
}

impl<N, U, C> Wireless for PreexistingWireless<N, U, C>
where
    N: Netif,
    U: UdpBind,
    C: Controller,
{
    type Data = C::Data;

    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: WirelessTask<Data = Self::Data>,
    {
        task.run(&mut self.netif, &mut self.udp, &mut self.controller)
            .await
    }
}

/// A trait representing a task that needs access to the BLE peripheral to perform its work
pub trait BleTask {
    /// Run the task with the given GATT peripheral
    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral;
}

impl<T> BleTask for &mut T
where
    T: BleTask,
{
    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral,
    {
        T::run(*self, peripheral).await
    }
}

/// A trait for running a task within a context where the BLE peripheral is initialized and operable
/// (e.g. in a commissioning workflow)
pub trait Ble {
    /// Setup the radio to operate in BLE mode and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: BleTask;
}

impl<T> Ble for &mut T
where
    T: Ble,
{
    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: BleTask,
    {
        T::run(self, task).await
    }
}

/// A utility type for running a BLE task with a pre-existing BLE peripheral
/// rather than bringing up / tearing down the BLE peripheral for the task.
pub struct PreexistingBle<P> {
    peripheral: P,
}

impl<P> PreexistingBle<P> {
    /// Create a new `PreexistingBle` instance with the given GATT peripheral
    pub const fn new(peripheral: P) -> Self {
        Self { peripheral }
    }
}

impl<P> Ble for PreexistingBle<P>
where
    P: GattPeripheral,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: BleTask,
    {
        task.run(&mut self.peripheral).await
    }
}
