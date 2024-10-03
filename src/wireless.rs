use core::future::Future;
use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::{select, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;

use log::info;

use proxy::ControllerProxy;
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
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;
use traits::{Wireless, WirelessDTOs};

use crate::netif::Netif;
use crate::network::{Embedding, Network};
use crate::persist::Persist;
use crate::utils::futures::IntoFaillble;
use crate::wireless::mgmt::WirelessManager;
use crate::wireless::store::NetworkContext;
use crate::wireless::traits::{
    Ble, Controller, NetworkCredentials, ThreadCredentials, WifiCredentials,
};
use crate::MatterStack;

pub mod comm;
pub mod mgmt;
pub mod proxy;
pub mod store;
pub mod svc;
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

impl<M, T, E> Default for WirelessBle<M, T, E>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<M, T, E> WirelessBle<M, T, E>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding,
{
    /// Creates a new instance of the `WirelessBle` network type.
    pub const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            network_context: NetworkContext::new(),
            embedding: E::INIT,
        }
    }

    /// Return an in-place initializer for the `WirelessBle` network type.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            btp_context <- BtpContext::init(),
            network_context <- NetworkContext::init(),
            embedding <- E::init(),
        })
    }

    /// Return a reference to the BTP context.
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
        WirelessBle::init()
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

    pub async fn run<'d, B, W, P, H, U>(
        &'static self,
        ble: B,
        wireless: W,
        proxy: &ControllerProxy<W>,
        persist: P,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        B: Ble,
        W: Wireless<NetworkCredentials = T>,
        W::ScanResult: Clone,
        W::Stats: Default,
        P: Persist,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_net(ble, wireless, proxy));
        let mut handler_task = pin!(self.run_handlers(persist, handler));
        let mut user_task = pin!(user);

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }

    async fn run_net<'d, B, W>(
        &'static self,
        mut ble: B,
        mut wireless: W,
        proxy: &ControllerProxy<W>,
    ) -> Result<(), Error>
    where
        B: Ble,
        W: Wireless<NetworkCredentials = T>,
        W::ScanResult: Clone,
        W::Stats: Default,
    {
        if wireless.supports_concurrent_connection() {
            let (mut controller, mut netif) = wireless.start().await?;

            let mut mgr = WirelessManager::new(proxy);

            info!("Wireless driver started");

            loop {
                let commissioned = self.is_commissioned().await?;

                if !commissioned {
                    self.matter()
                        .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                        .await?; // TODO

                    let btp = Btp::new(ble.start().await?, &self.network.btp_context);

                    info!("BLE driver started");

                    let mut net_task = pin!(self.run_comm_net(&btp, &mut netif));
                    let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                    let mut proxy_task = pin!(proxy.process_with(&mut controller));

                    select3(&mut net_task, &mut mgr_task, &mut proxy_task)
                        .coalesce()
                        .await?;
                } else {
                    self.matter().disable_commissioning()?;

                    let mut net_task = pin!(self.run_oper_net(
                        &mut netif,
                        core::future::pending(),
                        Option::<(NoNetwork, NoNetwork)>::None
                    ));
                    let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                    let mut proxy_task = pin!(proxy.process_with(&mut controller));

                    select3(&mut net_task, &mut mgr_task, &mut proxy_task)
                        .coalesce()
                        .await?;
                }
            }
        } else {
            loop {
                let commissioned = self.is_commissioned().await?;

                if !commissioned {
                    self.matter()
                        .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                        .await?; // TODO

                    let btp = Btp::new(ble.start().await?, &self.network.btp_context);

                    info!("BLE driver started");

                    self.run_nc_comm_net(&btp).await?;
                }

                let (mut controller, mut netif) = wireless.start().await?;

                let mut mgr = WirelessManager::new(proxy);

                info!("Wireless driver started");

                self.matter().disable_commissioning()?;

                let mut net_task = pin!(self.run_oper_net(
                    &mut netif,
                    core::future::pending(),
                    Option::<(NoNetwork, NoNetwork)>::None
                ));
                let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                let mut proxy_task = pin!(proxy.process_with(&mut controller));

                select3(&mut net_task, &mut mgr_task, &mut proxy_task)
                    .coalesce()
                    .await?;
            }
        }
    }

    async fn run_comm_net<'d, B, N>(
        &self,
        btp: &Btp<&'static BtpContext<M>, M, B>,
        mut netif: N,
    ) -> Result<(), Error>
    where
        B: GattPeripheral,
        N: Netif + UdpBind,
    {
        info!("Running Matter in concurrent commissioning mode (BLE and Wireless)");

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        // TODO: Run till commissioning is complete
        let mut net_task =
            pin!(self.run_oper_net(&mut netif, core::future::pending(), Some((btp, btp))));

        select(&mut btp_task, &mut net_task).coalesce().await
    }

    async fn run_nc_comm_net<'d, B>(
        &'static self,
        btp: &Btp<&'static BtpContext<M>, M, B>,
    ) -> Result<(), Error>
    where
        B: GattPeripheral,
    {
        info!("Running Matter in non-concurrent commissioning mode (BLE only)");

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        let mut net_task = pin!(self.run_transport_net(btp, btp));

        let mut oper_net_act_task = pin!(self
            .network
            .network_context
            .wait_network_activated()
            .into_fallible());

        select3(&mut btp_task, &mut net_task, &mut oper_net_act_task)
            .coalesce()
            .await
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
    pub fn root_handler<W>(
        &self,
        controller_proxy: &ControllerProxy<W>,
    ) -> WifiBleRootEndpointHandler<'_, M>
    where
        W: WirelessDTOs<NetworkCredentials = WifiCredentials>,
        W::ScanResult: Clone,
        W::Stats: Default,
    {
        let supports_concurrent_connection = controller_proxy.supports_concurrent_connection();

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
    pub fn root_handler<P, W>(
        &self,
        controller_proxy: &ControllerProxy<W>,
    ) -> ThreadBleRootEndpointHandler<'_, M>
    where
        W: Wireless<NetworkCredentials = WifiCredentials>,
        W::ScanResult: Clone,
        W::Stats: Default,
    {
        let supports_concurrent_connection = controller_proxy.supports_concurrent_connection();

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

/// The root endpoint handler for a BLE+Wifi network.
pub type WifiBleRootEndpointHandler<'a, M> = RootEndpointHandler<
    'a,
    HandlerCompat<comm::WirelessNwCommCluster<'a, MAX_WIRELESS_NETWORKS, M, WifiCredentials>>,
    HandlerCompat<WifiNwDiagCluster>,
>;

/// The root endpoint handler for a BLE+Thread network.
pub type ThreadBleRootEndpointHandler<'a, M> = RootEndpointHandler<
    'a,
    HandlerCompat<comm::WirelessNwCommCluster<'a, MAX_WIRELESS_NETWORKS, M, ThreadCredentials>>,
    HandlerCompat<ThreadNwDiagCluster<'a>>,
>;