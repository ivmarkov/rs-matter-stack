use core::future::Future;
use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;

use log::info;

use rs_matter::data_model::objects::{
    AsyncHandler, AsyncMetadata, Dataver, Endpoint, HandlerCompat,
};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::thread_nw_diagnostics::{self, ThreadNwDiagCluster};
use rs_matter::data_model::sdm::wifi_nw_diagnostics;
use rs_matter::data_model::sdm::wifi_nw_diagnostics::{
    WiFiSecurity, WiFiVersion, WifiNwDiagCluster, WifiNwDiagData,
};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::tlv::{FromTLV, ToTLV};
use rs_matter::transport::network::btp::{Btp, BtpContext, GattPeripheral};
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;

use crate::netif::Netif;
use crate::network::{Embedding, Network};
use crate::persist::Persist;
use crate::wireless::mgmt::WirelessManager;
use crate::wireless::store::NetworkContext;
use crate::wireless::traits::{
    Ble, NetworkCredentials, ThreadCredentials, WifiCredentials, WirelessController, WirelessStats,
};
use crate::MatterStack;

pub mod comm;
pub mod mgmt;
pub mod store;
pub mod traits;

const MAX_WIRELESS_NETWORKS: usize = 2;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
///
/// The supported commissioning is of the non-concurrent type (as per the Matter Core spec),
/// where the device - at any point in time - either runs Bluetooth or Wifi/Thread, but not both.
/// This is done to save memory and to avoid the usage of BLE+Wifi/Thread co-exist drivers on
/// devices which share a single wireless radio for both BLE and Wifi/Thread.
pub struct WirelessBle<M, T, E = ()>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'a> FromTLV<'a> + ToTLV,
{
    btp_context: BtpContext<M>,
    network_context: NetworkContext<MAX_WIRELESS_NETWORKS, M, T>,
    embedding: E,
}

impl<M, T, E> WirelessBle<M, T, E>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding,
{
    const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            network_context: NetworkContext::new(),
            embedding: E::INIT,
        }
    }

    pub fn network_context(&self) -> &NetworkContext<MAX_WIRELESS_NETWORKS, M, T> {
        &self.network_context
    }
}

impl<M, T, E> Network for WirelessBle<M, T, E>
where
    M: RawMutex + 'static,
    T: NetworkCredentials + Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding + 'static,
{
    const INIT: Self = Self::new();

    type PersistContext<'a> = &'a NetworkContext<MAX_WIRELESS_NETWORKS, M, T>;

    type Embedding = E;

    fn persist_context(&self) -> Self::PersistContext<'_> {
        &self.network_context
    }

    fn embedding(&self) -> &Self::Embedding {
        &self.embedding
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            btp_context <- BtpContext::init(),
            network_context <- NetworkContext::init(),
            embedding <- E::init(),
        })
    }
}

pub type WifiBleMatterStack<'a, M, E> = MatterStack<'a, WirelessBle<M, WifiCredentials, E>>;
pub type ThreadBleMatterStack<'a, M, E> = MatterStack<'a, WirelessBle<M, ThreadCredentials, E>>;

impl<'a, M, T, E> MatterStack<'a, WirelessBle<M, T, E>>
where
    M: RawMutex + Send + Sync + 'static,
    T: NetworkCredentials + Clone + for<'t> FromTLV<'t> + ToTLV,
    E: Embedding + 'static,
{
    /// Resets the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs
        // TODO self.network.btp_gatt_context.reset()?;
        // TODO self.network.btp_context.reset();
        self.network.network_context.reset();

        Ok(())
    }

    /// A utility method to run the Matter stack in Operating mode (as per the Matter Core spec) over Wifi.
    ///
    /// This method assumes that the Matter instance is already commissioned and therefore
    /// does not take a `CommissioningData` parameter.
    ///
    /// It is just a composition of the `MatterStack::run_with_netif` method, and the `WifiManager::run` method,
    /// where the former takes care of running the main Matter loop, while the latter takes care of ensuring
    /// that the Matter instance stays connected to the Wifi network.
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `wifi` - a user-provided `Wifi` implementation
    /// - `netif` - a user-provided `Netif` implementation
    /// - `dev_comm` - the commissioning data and discovery capabilities
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn operate<H, P, W, I, U>(
        &self,
        persist: P,
        wireless: W,
        netif: I,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        W: WirelessController<NetworkCredentials = T>,
        I: Netif + UdpBind,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Running Matter on the operational network");

        let mut user = pin!(user);

        let mut main = pin!(self.run_with_netif(persist, netif, handler, &mut user));

        let mut mgr = WirelessManager::new(wireless);

        let mut wireless = pin!(mgr.run(self.network.network_context()));

        select(&mut wireless, &mut main).coalesce().await
    }

    /// A utility method to run the Matter stack in Commissioning mode (as per the Matter Core spec) over BLE.
    /// Essentially an instantiation of `MatterStack::run_with_transport` with the BLE transport.
    ///
    /// Note: make sure to call `MatterStack::reset` before calling this method, as all fabrics and ACLs, as well as all
    /// transport state should be reset.
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `gatt` - a user-provided `GattPeripheral` implementation
    /// - `dev_comm` - the commissioning data
    /// - `handler` - a user-provided DM handler implementation
    pub async fn commission<'d, H, P, G>(
        &'static self,
        persist: P,
        gatt: G,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        G: GattPeripheral,
    {
        info!("Running Matter in commissioning mode (BLE)");

        self.matter()
            .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
            .await?; // TODO

        let btp = Btp::new(gatt, &self.network.btp_context);

        let mut ble = pin!(async {
            btp.run(
                "BT",
                self.matter().dev_det(),
                self.matter().dev_comm().discriminator,
            )
            .await
            .map_err(Into::into)
        });

        let mut main = pin!(self.run_with_transport(&btp, &btp, persist, &handler));

        select(&mut ble, &mut main).coalesce().await
    }

    /// An all-in-one "run everything" method that automatically
    /// places the Matter stack either in Commissioning or in Operating mode, depending
    /// on the state of the device as persisted in the NVS storage.
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `modem` - a user-provided `Modem` implementation
    /// - `dev_comm` - the commissioning data
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<'d, P, B, W, N, H, U>(
        &'static self,
        mut persist: P,
        mut ble: B,
        mut wireless: W,
        mut netif: N,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        P: Persist,
        B: Ble,
        W: WirelessController<NetworkCredentials = T>,
        N: Netif + UdpBind,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let mut user = pin!(user);

        loop {
            // TODO persist.load().await?;

            if !self.is_commissioned().await? {
                let gatt = ble.peripheral().await;

                info!("BLE driver initialized");

                let mut main = pin!(self.commission(&mut persist, gatt, &handler));
                let mut wait_network_connect = pin!(async {
                    self.network.network_context.wait_network_connect().await;
                    Ok::<(), Error>(())
                });

                select(&mut main, &mut wait_network_connect)
                    .coalesce()
                    .await?;
            }

            info!("Wireless driver initialized");

            self.operate(&mut persist, &mut wireless, &mut netif, &handler, &mut user)
                .await?;
        }
    }
}

impl<'a, M, E> MatterStack<'a, WirelessBle<M, WifiCredentials, E>>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0, OperNwType::Wifi)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub fn root_handler<W, S>(&self, controller: W, _stats: S) -> WifiBleRootEndpointHandler<'_, M>
    where
        W: WirelessController<NetworkCredentials = WifiCredentials>,
        S: WirelessStats,
    {
        let supports_concurrent_connection = controller.supports_concurrent_connection();

        handler(
            0,
            HandlerCompat(comm::WirelessNwCommCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.network_context,
            )),
            wifi_nw_diagnostics::ID,
            HandlerCompat(WifiNwDiagCluster::new(
                Dataver::new_rand(self.matter().rand()),
                // TODO: Update with actual information
                WifiNwDiagData {
                    bssid: [0; 6],
                    security_type: WiFiSecurity::Unspecified,
                    wifi_version: WiFiVersion::B,
                    channel_number: 20,
                    rssi: 0,
                },
            )),
            supports_concurrent_connection,
            self.matter().rand(),
        )
    }
}

impl<'a, M, E> MatterStack<'a, WirelessBle<M, ThreadCredentials, E>>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Thread network.
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0, OperNwType::Thread)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub fn root_handler<W, S>(
        &self,
        controller: W,
        _stats: S,
    ) -> ThreadBleRootEndpointHandler<'_, M>
    where
        W: WirelessController<NetworkCredentials = WifiCredentials>,
        S: WirelessStats,
    {
        let supports_concurrent_connection = controller.supports_concurrent_connection();

        handler(
            0,
            HandlerCompat(comm::WirelessNwCommCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.network_context,
            )),
            thread_nw_diagnostics::ID,
            HandlerCompat(ThreadNwDiagCluster::new(
                Dataver::new_rand(self.matter().rand()),
                // TODO: Update with actual information
                todo!(),
            )),
            supports_concurrent_connection,
            self.matter().rand(),
        )
    }
}

pub type WifiBleRootEndpointHandler<'a, M> = RootEndpointHandler<
    'a,
    HandlerCompat<comm::WirelessNwCommCluster<'a, MAX_WIRELESS_NETWORKS, M, WifiCredentials>>,
    HandlerCompat<WifiNwDiagCluster>,
>;

pub type ThreadBleRootEndpointHandler<'a, M> = RootEndpointHandler<
    'a,
    HandlerCompat<comm::WirelessNwCommCluster<'a, MAX_WIRELESS_NETWORKS, M, ThreadCredentials>>,
    HandlerCompat<ThreadNwDiagCluster<'a>>,
>;
