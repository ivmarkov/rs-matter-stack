use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::{select, select3, select4};
use embassy_sync::blocking_mutex::raw::RawMutex;

use rs_matter::dm::clusters::gen_comm::CommPolicy;
use rs_matter::dm::clusters::gen_diag::GenDiag;
use rs_matter::dm::clusters::net_comm::{NetCtl, NetCtlStatus, NetworkType};
use rs_matter::dm::clusters::wifi_diag::WifiDiag;
use rs_matter::dm::endpoints::{self, with_sys, with_wifi, SysHandler, WifiHandler};
use rs_matter::dm::networks::wireless::{
    self, NetCtlWithStatusImpl, NoopWirelessNetCtl, WirelessMgr,
};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::{clusters::gen_diag::NetifDiag, AsyncHandler};
use rs_matter::dm::{AsyncMetadata, Endpoint};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::GattPeripheral;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::select::Coalesce;

use crate::network::Embedding;
use crate::persist::{KvBlobStore, SharedKvBlobStore};
use crate::wireless::MatterStackWirelessTask;
use crate::UserTask;

use super::{Gatt, GattTask, PreexistingWireless, WirelessMatterStack};

/// A type alias for a Matter stack running over Wifi (and BLE, during commissioning).
pub type WifiMatterStack<'a, M, E = ()> = WirelessMatterStack<'a, M, wireless::Wifi, E>;

impl<M, E> WirelessMatterStack<'_, M, wireless::Wifi, E>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
{
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
    pub async fn run_preex<U, N, C, G, S, H, X>(
        &'static self,
        udp: U,
        netif: N,
        net_ctl: C,
        gatt: G,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
        G: GattPeripheral,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: UserTask,
    {
        self.run_coex(
            PreexistingWireless::new(udp, netif, net_ctl, gatt),
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
        wifi: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: WifiCoex,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let persist = self.create_persist(store);

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_wifi_coex(wifi, handler, user));
        let mut persist_task = pin!(self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
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
        wifi: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: Wifi + Gatt,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let persist = self.create_persist(store);

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_wifi(wifi, handler, user));
        let mut persist_task = pin!(self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    async fn run_wifi_coex<W, H, U>(
        &'static self,
        mut wifi: W,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: WifiCoex,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        wifi.run(MatterStackWirelessTask(self, handler, user)).await
    }

    async fn run_wifi<W, H, U>(
        &'static self,
        mut wifi: W,
        handler: H,
        mut user: U,
    ) -> Result<(), Error>
    where
        W: Wifi + Gatt,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                self.matter()
                    .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                    .await?; // TODO

                Gatt::run(
                    &mut wifi,
                    MatterStackWirelessTask(self, &handler, &mut user),
                )
                .await?;
            }

            if commissioned {
                self.matter().disable_commissioning()?;
            }

            Wifi::run(
                &mut wifi,
                MatterStackWirelessTask(self, &handler, &mut user),
            )
            .await?;
        }
    }

    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub const fn root_endpoint() -> Endpoint<'static> {
        endpoints::root_endpoint(NetworkType::Wifi)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    fn root_handler<'a, C, H>(
        &'a self,
        gen_diag: &'a dyn GenDiag,
        netif_diag: &'a dyn NetifDiag,
        net_ctl: &'a C,
        comm_policy: &'a dyn CommPolicy,
        handler: H,
    ) -> WifiHandler<'a, &'a C, SysHandler<'a, H>>
    where
        C: NetCtl + NetCtlStatus + WifiDiag,
    {
        with_wifi(
            gen_diag,
            netif_diag,
            net_ctl,
            &self.network.networks,
            self.matter().rand(),
            with_sys(comm_policy, self.matter().rand(), handler),
        )
    }
}

/// A trait representing a task that needs access to the operational wireless interface (Wifi or Thread)
/// (Netif, UDP stack and Wireless controller) to perform its work.
pub trait WifiTask {
    /// Run the task with the given network interface, UDP stack and wireless controller
    async fn run<U, N, C>(&mut self, udp: U, netif: N, net_ctl: C) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif;
}

impl<T> WifiTask for &mut T
where
    T: WifiTask,
{
    async fn run<U, N, C>(&mut self, udp: U, netif: N, net_ctl: C) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
    {
        T::run(*self, udp, netif, net_ctl).await
    }
}

/// A trait for running a task within a context where the wireless interface is initialized and operable
pub trait Wifi {
    /// Setup the radio to operate in wireless (Wifi or Thread) mode
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: WifiTask;
}

impl<T> Wifi for &mut T
where
    T: Wifi,
{
    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: WifiTask,
    {
        T::run(self, task).await
    }
}

/// A trait representing a task that needs access to the operational wireless interface (Wifi or Thread)
/// as well as to the commissioning BTP GATT peripheral.
///
/// Typically, tasks performing the Matter concurrent commissioning workflow will implement this trait.
pub trait WifiCoexTask {
    /// Run the task with the given network interface, UDP stack and wireless controller
    async fn run<U, N, C, G>(&mut self, udp: U, netif: N, net_ctl: C, gatt: G) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
        G: GattPeripheral;
}

impl<T> WifiCoexTask for &mut T
where
    T: WifiCoexTask,
{
    async fn run<U, N, C, G>(&mut self, udp: U, netif: N, net_ctl: C, gatt: G) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
        G: GattPeripheral,
    {
        T::run(*self, udp, netif, net_ctl, gatt).await
    }
}

/// A trait for running a task within a context where both the wireless interface (Thread or Wifi)
/// is initialized and operable, as well as the BLE GATT peripheral is also operable.
///
/// Typically, tasks performing the Matter concurrent commissioning workflow will ran by implementations
/// of this trait.
pub trait WifiCoex {
    /// Setup the radio to operate in wireless coexist mode (Wifi or Thread + BLE)
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: WifiCoexTask;
}

impl<T> WifiCoex for &mut T
where
    T: WifiCoex,
{
    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: WifiCoexTask,
    {
        T::run(self, task).await
    }
}

impl<U, N, C, P> Wifi for PreexistingWireless<U, N, C, P>
where
    U: UdpBind,
    N: NetifDiag + NetChangeNotif,
    C: NetCtl + WifiDiag + NetChangeNotif,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: WifiTask,
    {
        task.run(&mut self.udp, &self.netif, &self.net_ctl).await
    }
}

impl<U, N, C, P> WifiCoex for PreexistingWireless<U, N, C, P>
where
    U: UdpBind,
    N: NetifDiag + NetChangeNotif,
    C: NetCtl + WifiDiag + NetChangeNotif,
    P: GattPeripheral,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: WifiCoexTask,
    {
        task.run(&mut self.udp, &self.netif, &self.net_ctl, &mut self.gatt)
            .await
    }
}

impl<M, E, H, U> GattTask for MatterStackWirelessTask<'static, M, wireless::Wifi, E, H, U>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
{
    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral,
    {
        let net_ctl = NetCtlWithStatusImpl::new(
            &self.0.network.net_state,
            NoopWirelessNetCtl::new(NetworkType::Wifi),
        );

        let mut btp_task = pin!(self.0.run_btp(peripheral));

        let handler = self.0.root_handler(&(), &(), &net_ctl, &false, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        select(&mut btp_task, &mut handler_task).coalesce().await
    }
}

impl<M, E, H, X> WifiTask for MatterStackWirelessTask<'static, M, wireless::Wifi, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<U, N, C>(&mut self, udp: U, netif: N, net_ctl: C) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
    {
        info!("Wifi driver started");

        let mut buf = self.0.network.creds_buf.lock().await;

        let mut mgr = WirelessMgr::new(&self.0.network.networks, &net_ctl, &mut buf);

        let stack = &mut self.0;

        let mut net_task = pin!(stack.run_oper_net(
            &udp,
            &netif,
            core::future::pending(),
            Option::<(NoNetwork, NoNetwork)>::None
        ));
        let mut mgr_task = pin!(mgr.run());

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self
            .0
            .root_handler(&(), &netif, &net_ctl_s, &false, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        let mut user_task = pin!(self.2.run(&udp, &netif));

        select4(
            &mut net_task,
            &mut mgr_task,
            &mut handler_task,
            &mut user_task,
        )
        .coalesce()
        .await
    }
}

impl<M, E, H, X> WifiCoexTask for MatterStackWirelessTask<'static, M, wireless::Wifi, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<U, N, C, G>(&mut self, udp: U, netif: N, net_ctl: C, gatt: G) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
        G: GattPeripheral,
    {
        info!("Wifi and BLE drivers started");

        let stack = &mut self.0;

        let mut net_task = pin!(stack.run_net_coex(&udp, &netif, &net_ctl, gatt));

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self.0.root_handler(&(), &netif, &net_ctl_s, &true, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        let mut user_task = pin!(self.2.run(&udp, &netif));

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }
}
