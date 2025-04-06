use core::future::Future;
use core::pin::pin;

use diag::thread::ThreadNwDiagCluster;
use diag::wifi::WifiNwDiagCluster;

use edge_nal::UdpBind;

use embassy_futures::select::{select, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Timer};

use log::{info, warn};

use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata, Dataver, Endpoint};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::thread_nw_diagnostics;
use rs_matter::data_model::sdm::wifi_nw_diagnostics;
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::tlv::{FromTLV, ToTLV};
use rs_matter::transport::network::btp::{Btp, BtpContext, GattPeripheral};
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;

pub use traits::*;

use crate::netif::Netif;
use crate::network::{Embedding, Network};
use crate::persist::{KvBlobStore, SharedKvBlobStore};
use crate::private::Sealed;
use crate::wireless::mgmt::WirelessManager;
use crate::wireless::store::NetworkContext;
use crate::MatterStack;

use self::proxy::ControllerProxy;

pub mod comm;
pub mod diag;
pub mod mgmt;
pub mod proxy;
pub mod store;
pub mod svc;
mod traits;

const MAX_WIRELESS_NETWORKS: usize = 2;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
///
/// The supported commissioning is either concurrent or non-concurrent (as per the Matter Core spec),
/// where one over the other is decided at runtime with the concrete wireless implementation
/// (`WirelessCoex` or `Wireless` + `Gatt`).
///
/// Non-concurrent commissioning means that the device - at any point in time - either runs Bluetooth
/// or Wifi/Thread, but not both.
///
/// This is done to save memory and to avoid the usage of BLE+Wifi/Thread co-exist drivers on
/// devices which share a single wireless radio for both BLE and Wifi/Thread.
pub struct WirelessBle<M, T, E = ()>
where
    M: RawMutex,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
{
    btp_context: BtpContext<M>,
    network_context: NetworkContext<MAX_WIRELESS_NETWORKS, M, T::Data>,
    embedding: E,
}

impl<M, T, E> Default for WirelessBle<M, T, E>
where
    M: RawMutex,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<M, T, E> WirelessBle<M, T, E>
where
    M: RawMutex,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
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
    pub fn network_context(&self) -> &NetworkContext<MAX_WIRELESS_NETWORKS, M, T::Data> {
        &self.network_context
    }
}

impl<M, T, E> Sealed for WirelessBle<M, T, E>
where
    M: RawMutex,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding,
{
}

impl<M, T, E> Network for WirelessBle<M, T, E>
where
    M: RawMutex + 'static,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
    E: Embedding + 'static,
{
    const INIT: Self = Self::new();

    type PersistContext<'a> = &'a NetworkContext<MAX_WIRELESS_NETWORKS, M, T::Data>;

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

/// A type alias for a Matter stack running over Wifi (and BLE, during commissioning).
pub type WifiMatterStack<'a, M, E = ()> = MatterStack<'a, WirelessBle<M, Wifi, E>>;

/// A type alias for a Matter stack running over Thread (and BLE, during commissioning).
pub type ThreadMatterStack<'a, M, E = ()> = MatterStack<'a, WirelessBle<M, Thread, E>>;

impl<M, T, E> MatterStack<'_, WirelessBle<M, T, E>>
where
    M: RawMutex + Send + Sync + 'static,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
    E: Embedding + 'static,
{
    /// Reset the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs
        // TODO self.network.btp_gatt_context.reset()?;
        // TODO self.network.btp_context.reset();
        self.network.network_context.reset();

        Ok(())
    }

    /// Run the Matter stack for an already pre-existing wireless network where the BLE and the operational network can co-exist.
    ///
    /// Parameters:
    /// - `netif` - a user-provided `Netif` implementation
    /// - `udp` - a user-provided `UdpBind` implementation
    /// - `controller` - a user-provided `Controller` implementation
    /// - `gatt` - a user-provided `GattPeripheral` implementation
    /// - `store` - a `SharedKvBlobStore` implementation wrapping a user-provided `KvBlobStore`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    #[allow(clippy::too_many_arguments)]
    pub async fn run_preex<N, U, C, G, S, H, X>(
        &'static self,
        netif: N,
        udp: U,
        controller: C,
        gatt: G,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind,
        C: Controller<Data = T::Data>,
        G: GattPeripheral,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: Future<Output = Result<(), Error>>,
    {
        self.run_coex(
            PreexistingWireless::new(netif, udp, controller, gatt),
            store,
            handler,
            user,
        )
        .await
    }

    /// Run the Matter stack for a wireless network where the BLE and the operational network can co-exist.
    ///
    /// Parameters:
    /// - `wireless` - a user-provided `Wireless` implementation
    /// - `store` - a `SharedKvBlobStore` implementation wrapping a user-provided `KvBlobStore`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run_coex<W, S, H, U>(
        &'static self,
        wireless: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: WirelessCoex<Data = T::Data>,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        self.network
            .network_context
            .concurrent_connection
            .lock(|s| s.set(true));

        let persist = self.create_persist(store);

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_wireless_coex(wireless));
        let mut handler_task = pin!(self.run_handlers(&persist, handler));
        let mut user_task = pin!(user);

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }

    /// Run the Matter stack for a wireless network where the BLE and the operational network cannot co-exist.
    ///
    /// Parameters:
    /// - `wireless` - a user-provided `Wireless` + `Gatt` implementation
    /// - `store` - a `SharedKvBlobStore` implementation wrapping a user-provided `KvBlobStore`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<W, S, H, U>(
        &'static self,
        wireless: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: Wireless<Data = T::Data> + Gatt,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        self.network
            .network_context
            .concurrent_connection
            .lock(|s| s.set(false));

        let persist = self.create_persist(store);

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_wireless(wireless));
        let mut handler_task = pin!(self.run_handlers(&persist, handler));
        let mut user_task = pin!(user);

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }

    async fn run_wireless_coex<W>(&'static self, mut wireless: W) -> Result<(), Error>
    where
        W: WirelessCoex<Data = T::Data>,
    {
        #[allow(non_local_definitions)]
        impl<M, T, E> WirelessCoexTask for MatterStackWirelessTask<'static, M, T, E>
        where
            M: RawMutex + Send + Sync + 'static,
            T: WirelessConfig,
            <T::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
            E: Embedding + 'static,
        {
            type Data = T::Data;

            async fn run<N, U, C, G>(
                &mut self,
                netif: N,
                udp: U,
                controller: C,
                gatt: G,
            ) -> Result<(), Error>
            where
                N: Netif,
                U: UdpBind,
                C: Controller<Data = Self::Data>,
                G: GattPeripheral,
            {
                info!("Wireless driver started");

                let stack = &mut self.0;

                stack.run_net_coex(&netif, &udp, controller, gatt).await
            }
        }

        wireless.run(MatterStackWirelessTask(self)).await
    }

    async fn run_wireless<W>(&'static self, mut wireless: W) -> Result<(), Error>
    where
        W: Wireless<Data = T::Data> + Gatt,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                self.matter()
                    .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                    .await?; // TODO

                #[allow(non_local_definitions)]
                impl<M, T, E> GattTask for MatterStackWirelessTask<'static, M, T, E>
                where
                    M: RawMutex + Send + Sync + 'static,
                    T: WirelessConfig,
                    <T::Data as WirelessData>::NetworkCredentials:
                        Clone + for<'t> FromTLV<'t> + ToTLV,
                    E: Embedding + 'static,
                {
                    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
                    where
                        P: GattPeripheral,
                    {
                        let btp = Btp::new(peripheral, &self.0.network.btp_context);

                        info!("BLE driver started");

                        self.0.run_btp(&btp).await
                    }
                }

                Gatt::run(&mut wireless, MatterStackWirelessTask(self)).await?;
            }

            if commissioned {
                self.matter().disable_commissioning()?;
            }

            #[allow(non_local_definitions)]
            impl<M, T, E> WirelessTask for MatterStackWirelessTask<'static, M, T, E>
            where
                M: RawMutex + Send + Sync + 'static,
                T: WirelessConfig,
                <T::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
                E: Embedding + 'static,
            {
                type Data = T::Data;

                async fn run<N, U, C>(
                    &mut self,
                    netif: N,
                    udp: U,
                    mut controller: C,
                ) -> Result<(), Error>
                where
                    N: Netif,
                    U: UdpBind,
                    C: Controller<Data = Self::Data>,
                {
                    info!("Wireless driver started");

                    let mut mgr =
                        WirelessManager::new(&self.0.network.network_context.controller_proxy);

                    let stack = &mut self.0;

                    let mut net_task = pin!(stack.run_oper_net(
                        &netif,
                        &udp,
                        core::future::pending(),
                        Option::<(NoNetwork, NoNetwork)>::None
                    ));
                    let mut mgr_task = pin!(mgr.run(&stack.network.network_context));
                    let mut proxy_task = pin!(stack
                        .network
                        .network_context
                        .controller_proxy
                        .process_with(&mut controller)?);

                    select3(&mut net_task, &mut mgr_task, &mut proxy_task)
                        .coalesce()
                        .await
                }
            }

            Wireless::run(&mut wireless, MatterStackWirelessTask(self)).await?;
        }
    }

    async fn run_net_coex<C, N, U, G>(
        &'static self,
        netif: N,
        udp: U,
        mut controller: C,
        gatt: G,
    ) -> Result<(), Error>
    where
        C: Controller<Data = T::Data>,
        N: Netif,
        U: UdpBind,
        G: GattPeripheral,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                self.matter()
                    .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                    .await?; // TODO

                let btp = Btp::new(&gatt, &self.network.btp_context);

                info!("BLE driver started");

                let mut mgr = WirelessManager::new(&self.network.network_context.controller_proxy);

                let mut net_task = pin!(self.run_btp_coex(&netif, &udp, &btp));
                let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                let mut proxy_task = pin!(self
                    .network
                    .network_context
                    .controller_proxy
                    .process_with(&mut controller)?);

                select3(&mut net_task, &mut mgr_task, &mut proxy_task)
                    .coalesce()
                    .await?;
            } else {
                let mut mgr = WirelessManager::new(&self.network.network_context.controller_proxy);

                self.matter().disable_commissioning()?;

                let mut net_task = pin!(self.run_oper_net(
                    &netif,
                    &udp,
                    core::future::pending(),
                    Option::<(NoNetwork, NoNetwork)>::None
                ));
                let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                let mut proxy_task = pin!(self
                    .network
                    .network_context
                    .controller_proxy
                    .process_with(&mut controller)?);

                select3(&mut net_task, &mut mgr_task, &mut proxy_task)
                    .coalesce()
                    .await?;
            }
        }
    }

    async fn run_btp_coex<N, U, B>(
        &self,
        mut netif: N,
        mut udp: U,
        btp: &Btp<&'static BtpContext<M>, M, B>,
    ) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind,
        B: GattPeripheral,
    {
        info!("Running Matter in concurrent commissioning mode (BLE and Wireless)");

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        // TODO: Run till commissioning is complete
        let mut net_task = pin!(self.run_oper_net(
            &mut netif,
            &mut udp,
            core::future::pending(),
            Some((btp, btp))
        ));

        select(&mut btp_task, &mut net_task).coalesce().await
    }

    async fn run_btp<B>(&'static self, btp: &Btp<&'static BtpContext<M>, M, B>) -> Result<(), Error>
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

        let mut oper_net_act_task = pin!(async {
            const WAIT_SECS: u64 = 4;

            self.network.network_context.wait_network_activated().await;

            warn!(
                "Giving BLE/BTP extra {WAIT_SECS} seconds for any outstanding messages before switching to the operational network"
            );

            Timer::after(Duration::from_secs(WAIT_SECS)).await;

            Ok(())
        });

        select3(&mut btp_task, &mut net_task, &mut oper_net_act_task)
            .coalesce()
            .await
    }
}

struct MatterStackWirelessTask<'a, M, T, E>(&'a MatterStack<'a, WirelessBle<M, T, E>>)
where
    M: RawMutex + Send + Sync + 'static,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
    E: Embedding + 'static;

impl<M, E> MatterStack<'_, WirelessBle<M, Wifi, E>>
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
    pub fn root_handler(&self) -> WifiRootEndpointHandler<'_, M, &ControllerProxy<M, WifiData>> {
        handler(
            0,
            comm::WirelessNwCommCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.network_context,
                &self.network.network_context.controller_proxy,
            ),
            wifi_nw_diagnostics::ID,
            WifiNwDiagCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.network_context.controller_proxy,
            ),
            &self.network.network_context,
            self.matter().rand(),
        )
    }
}

impl<M, E> MatterStack<'_, WirelessBle<M, Thread, E>>
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
    pub fn root_handler(
        &self,
    ) -> ThreadRootEndpointHandler<'_, M, &ControllerProxy<M, ThreadData>> {
        handler(
            0,
            comm::WirelessNwCommCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.network_context,
                &self.network.network_context.controller_proxy,
            ),
            thread_nw_diagnostics::ID,
            ThreadNwDiagCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.network_context.controller_proxy,
            ),
            &self.network.network_context,
            self.matter().rand(),
        )
    }
}

/// The root endpoint handler for a Wifi network.
pub type WifiRootEndpointHandler<'a, M, T> = RootEndpointHandler<
    'a,
    comm::WirelessNwCommCluster<'a, MAX_WIRELESS_NETWORKS, M, T>,
    WifiNwDiagCluster<M, T>,
>;

/// The root endpoint handler for a Thread network.
pub type ThreadRootEndpointHandler<'a, M, T> = RootEndpointHandler<
    'a,
    comm::WirelessNwCommCluster<'a, MAX_WIRELESS_NETWORKS, M, T>,
    ThreadNwDiagCluster<M, T>,
>;
