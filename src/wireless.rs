use core::future::Future;
use core::marker::PhantomData;
use core::pin::pin;

use diag::thread::ThreadNwDiagCluster;
use diag::wifi::WifiNwDiagCluster;
use edge_nal::UdpBind;

use embassy_futures::select::{select, select3, select4};
use embassy_sync::blocking_mutex::raw::RawMutex;

use log::info;

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
use traits::{
    ConcurrencyMode, DisconnectedController, Thread, ThreadData, Wifi, WifiData, Wireless,
    WirelessConfig, WirelessData, NC,
};

use crate::netif::{Netif, NetifRun};
use crate::network::{Embedding, Network};
use crate::persist::Persist;
use crate::utils::futures::IntoFaillble;
use crate::wireless::mgmt::WirelessManager;
use crate::wireless::store::NetworkContext;
use crate::wireless::traits::{Ble, Controller, NetworkCredentials};
use crate::MatterStack;

use self::proxy::ControllerProxy;

#[cfg(all(feature = "os", target_os = "linux"))]
pub use bluez::*;

pub mod comm;
pub mod diag;
pub mod mgmt;
pub mod proxy;
pub mod store;
pub mod svc;
pub mod traits;

const MAX_WIRELESS_NETWORKS: usize = 2;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
///
/// The supported commissioning is either concurrent or non-concurrent (as per the Matter Core spec),
/// where one over the other is decided compile-time with the concrete `WirelessConfig` type.
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
pub type WifiMatterStack<'a, M, E> = MatterStack<'a, WirelessBle<M, Wifi, E>>;

/// A type alias for a Matter stack running over Thread (and BLE, during commissioning).
pub type ThreadMatterStack<'a, M, E> = MatterStack<'a, WirelessBle<M, Thread, E>>;

/// A type alias for a Matter stack running over Wifi (and BLE, during commissioning).
///
/// Unlike `WifiMatterStack`, this type alias runs the commissioning in a non-concurrent mode,
/// where the device runs either BLE or Wifi, but not both at the same time.
///
/// This is useful for devices which share a single wireless radio for both BLE and Wifi
/// and do not have BLE+Wifi co-exist drivers, or just to save memory by only having one of
/// the stacks active at any point in time.
///
/// Note that Alexa does not (yet) work with non-concurrent commissioning.
pub type WifiNCMatterStack<'a, M, E> = MatterStack<'a, WirelessBle<M, Wifi<NC>, E>>;

/// A type alias for a Matter stack running over Thread (and BLE, during commissioning).
///
/// Unlike `ThreadMatterStack`, this type alias runs the commissioning in a non-concurrent mode,
/// where the device runs either BLE or Thread, but not both at the same time.
///
/// This is useful for devices which share a single wireless radio for both BLE and Thread
/// and do not have BLE+Thread co-exist drivers, or just to save memory by only having one of
/// the stacks active at any point in time.
///
/// Note that Alexa does not (yet) work with non-concurrent commissioning.
pub type ThreadNCMatterStack<'a, M, E> = MatterStack<'a, WirelessBle<M, Thread<NC>, E>>;

impl<'a, M, T, E> MatterStack<'a, WirelessBle<M, T, E>>
where
    M: RawMutex + Send + Sync + 'static,
    T: WirelessConfig,
    <T::Data as WirelessData>::NetworkCredentials: Clone + for<'t> FromTLV<'t> + ToTLV,
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

    pub async fn run<'d, W, B, P, H, U>(
        &'static self,
        wireless: W,
        ble: B,
        persist: P,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        B: Ble,
        W: Wireless<Data = T::Data>,
        <W::Data as WirelessData>::ScanResult: Clone,
        <W::Data as WirelessData>::Stats: Default,
        P: Persist,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_net(wireless, ble));
        let mut handler_task = pin!(self.run_handlers(persist, handler));
        let mut user_task = pin!(user);

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }

    async fn run_net<'d, W, B>(&'static self, mut wireless: W, mut ble: B) -> Result<(), Error>
    where
        W: Wireless<Data = T::Data>,
        B: Ble,
        <W::Data as WirelessData>::ScanResult: Clone,
        <W::Data as WirelessData>::Stats: Default,
    {
        if T::CONCURRENT {
            let (netif, mut controller) = wireless.start().await?;

            let mut mgr = WirelessManager::new(&self.network.network_context.controller_proxy);

            info!("Wireless driver started");

            loop {
                let commissioned = self.is_commissioned().await?;

                if !commissioned {
                    self.matter()
                        .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                        .await?; // TODO

                    let btp = Btp::new(ble.start().await?, &self.network.btp_context);

                    info!("BLE driver started");

                    let mut netif_task = pin!(netif.run());
                    let mut net_task = pin!(self.run_comm_net(&netif, &btp));
                    let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                    let mut proxy_task = pin!(self
                        .network
                        .network_context
                        .controller_proxy
                        .process_with(&mut controller));

                    select4(
                        &mut netif_task,
                        &mut net_task,
                        &mut mgr_task,
                        &mut proxy_task,
                    )
                    .coalesce()
                    .await?;
                } else {
                    self.matter().disable_commissioning()?;

                    let mut netif_task = pin!(netif.run());
                    let mut net_task = pin!(self.run_oper_net(
                        &netif,
                        core::future::pending(),
                        Option::<(NoNetwork, NoNetwork)>::None
                    ));
                    let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                    let mut proxy_task = pin!(self
                        .network
                        .network_context
                        .controller_proxy
                        .process_with(&mut controller));

                    select4(
                        &mut netif_task,
                        &mut net_task,
                        &mut mgr_task,
                        &mut proxy_task,
                    )
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

                let (netif, mut controller) = wireless.start().await?;

                let mut mgr = WirelessManager::new(&self.network.network_context.controller_proxy);

                info!("Wireless driver started");

                self.matter().disable_commissioning()?;

                let mut netif_task = pin!(netif.run());
                let mut net_task = pin!(self.run_oper_net(
                    &netif,
                    core::future::pending(),
                    Option::<(NoNetwork, NoNetwork)>::None
                ));
                let mut mgr_task = pin!(mgr.run(&self.network.network_context));
                let mut proxy_task = pin!(self
                    .network
                    .network_context
                    .controller_proxy
                    .process_with(&mut controller));

                select4(
                    &mut netif_task,
                    &mut net_task,
                    &mut mgr_task,
                    &mut proxy_task,
                )
                .coalesce()
                .await?;
            }
        }
    }

    async fn run_comm_net<'d, N, B>(
        &self,
        mut netif: N,
        btp: &Btp<&'static BtpContext<M>, M, B>,
    ) -> Result<(), Error>
    where
        N: Netif + UdpBind,
        B: GattPeripheral,
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

impl<M, C, E> MatterStack<'_, WirelessBle<M, Wifi<C>, E>>
where
    M: RawMutex + Send + Sync + 'static,
    C: ConcurrencyMode,
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
            C::CONCURRENT,
            self.matter().rand(),
        )
    }
}

impl<M, C, E> MatterStack<'_, WirelessBle<M, Thread<C>, E>>
where
    M: RawMutex + Send + Sync + 'static,
    C: ConcurrencyMode,
    E: Embedding + 'static,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Thread network.
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0, OperNwType::Thread)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub fn root_handler<P>(
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
            C::CONCURRENT,
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

/// A dummy wireless interface which returns a wireless controller
/// which is always disconnected, and a network interface which is
/// pre-existing.
///
/// Useful for testing the Matter stack without a real wireless interface.
pub struct DummyWireless<T, N>(PhantomData<T>, N);

impl<T, N> DummyWireless<T, N> {
    /// Creates a new instance of the `DummyWireless` type
    /// with the supplied network interface.
    pub const fn new(netif: N) -> Self {
        Self(PhantomData, netif)
    }
}

impl<T, N> Wireless for DummyWireless<T, N>
where
    T: WirelessData,
    T::Stats: Default,
    N: Netif + NetifRun + UdpBind,
{
    type Data = T;

    type Netif<'a>
        = &'a mut N
    where
        Self: 'a;

    type Controller<'a>
        = DisconnectedController<T>
    where
        Self: 'a;

    async fn start(&mut self) -> Result<(Self::Netif<'_>, Self::Controller<'_>), Error> {
        Ok((&mut self.1, DisconnectedController::new()))
    }
}

#[cfg(all(feature = "os", target_os = "linux"))]
mod bluez {
    use rs_matter::error::Error;
    use rs_matter::transport::network::btp::BuiltinGattPeripheral;

    use crate::wireless::traits::Ble;

    /// A `Ble` trait implementation for the BlueZ GATT peripheral
    /// which is built-in in `rs-matter`.
    pub struct BuiltinBle<'a>(Option<&'a str>);

    impl<'a> BuiltinBle<'a> {
        pub const fn new(adapter: Option<&'a str>) -> Self {
            Self(adapter)
        }
    }

    impl<'a> Ble for BuiltinBle<'a> {
        type Peripheral<'t>
            = BuiltinGattPeripheral
        where
            Self: 't;

        async fn start(&mut self) -> Result<Self::Peripheral<'_>, Error> {
            Ok(BuiltinGattPeripheral::new(self.0))
        }
    }
}
