use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::{select, select3};

use rs_matter::data_model::networks::NetChangeNotif;
use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata, Endpoint};
use rs_matter::data_model::root_endpoint::{self, with_eth, with_sys, EthHandler, SysHandler};
use rs_matter::data_model::sdm::gen_comm::CommPolicy;
use rs_matter::data_model::sdm::gen_diag::{GenDiag, NetifDiag};
use rs_matter::data_model::sdm::net_comm::NetworkType;
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;

use crate::network::{Embedding, Network};
use crate::persist::{KvBlobStore, SharedKvBlobStore};
use crate::private::Sealed;
use crate::{MatterStack, UserTask};

/// An implementation of the `Network` trait for Ethernet.
///
/// Note that "Ethernet" - in the context of this crate - means
/// not just the Ethernet transport, but also any other IP-based transport
/// (like Wifi or Thread), where the Matter stack would not be concerned
/// with the management of the network transport (as in re-connecting to the
/// network on lost signal, managing network credentials and so on).
///
/// The expectation is nevertheless that for production use-cases
/// the `Eth` network would really only be used for Ethernet.
pub struct Eth<E = ()> {
    embedding: E,
}

impl<E> Sealed for Eth<E> {}

impl<E> Network for Eth<E>
where
    E: Embedding + 'static,
{
    const INIT: Self = Self { embedding: E::INIT };

    type PersistContext<'a> = ();

    type Embedding = E;

    fn persist_context(&self) -> Self::PersistContext<'_> {}

    fn embedding(&self) -> &Self::Embedding {
        &self.embedding
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            embedding <- E::init(),
        })
    }
}

pub type EthMatterStack<'a, E = ()> = MatterStack<'a, Eth<E>>;

/// A trait representing a task that needs access to the operational Ethernet interface
/// (Netif and UDP stack) to perform its work.
pub trait EthernetTask {
    /// Run the task with the given network interface and UDP stack
    async fn run<U, N>(&mut self, udp: U, netif: N) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif;
}

impl<T> EthernetTask for &mut T
where
    T: EthernetTask,
{
    async fn run<U, N>(&mut self, udp: U, netif: N) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
    {
        (*self).run(udp, netif).await
    }
}

/// A trait for running a task within a context where the ethernet interface is initialized and operable
pub trait Ethernet {
    /// Setup Ethernet and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: EthernetTask;
}

impl<T> Ethernet for &mut T
where
    T: Ethernet,
{
    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: EthernetTask,
    {
        (*self).run(task).await
    }
}

/// A utility type for running an ethernet task with a pre-existing ethernet interface
/// rather than bringing up / tearing down the ethernet interface for the task.
pub struct PreexistingEthernet<U, N> {
    udp: U,
    netif: N,
}

impl<N, U> PreexistingEthernet<U, N> {
    /// Create a new `PreexistingEthernet` instance with the given network interface and UDP stack.
    pub const fn new(udp: U, netif: N) -> Self {
        Self { udp, netif }
    }
}

impl<U, N> Ethernet for PreexistingEthernet<U, N>
where
    U: UdpBind,
    N: NetifDiag + NetChangeNotif,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: EthernetTask,
    {
        task.run(&mut self.udp, &self.netif).await
    }
}

/// A specialization of the `MatterStack` for Ethernet.
impl<E> MatterStack<'_, Eth<E>>
where
    E: Embedding + 'static,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub const fn root_endpoint() -> Endpoint<'static> {
        root_endpoint::root_endpoint(NetworkType::Ethernet)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    fn root_handler<'a, H>(
        &self,
        gen_diag: &'a dyn GenDiag,
        comm_policy: &'a dyn CommPolicy,
        netif_diag: &'a dyn NetifDiag,
        handler: H,
    ) -> EthHandler<'a, SysHandler<'a, H>> {
        with_eth(
            gen_diag,
            netif_diag,
            self.matter().rand(),
            with_sys(comm_policy, self.matter().rand(), handler),
        )
    }

    /// Reset the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs

        Ok(())
    }

    /// Run the Matter stack for a pre-existing Ethernet network.
    ///
    /// Parameters:
    /// - `netif` - a user-provided `Netif` implementation for the Ethernet network
    /// - `udp` - a user-provided `UdpBind` implementation
    /// - `persist` - a user-provided `Persist` implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run_preex<U, N, S, H, X>(
        &self,
        udp: U,
        netif: N,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        N: NetifDiag + NetChangeNotif,
        U: UdpBind,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: UserTask,
    {
        self.run(PreexistingEthernet::new(udp, netif), store, handler, user)
            .await
    }

    /// Run the Matter stack for an Ethernet network.
    ///
    /// Parameters:
    /// - `ethernet` - a user-provided `Ethernet` implementation
    /// - `persist` - a user-provided `Persist` implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<N, S, H, X>(
        &self,
        ethernet: N,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        N: Ethernet,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: UserTask,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        if !self.is_commissioned().await? {
            self.matter()
                .enable_basic_commissioning(DiscoveryCapabilities::IP, 0)
                .await?; // TODO
        }

        let persist = self.create_persist(store);

        let mut net_task = pin!(self.run_ethernet(ethernet, handler, user));
        let mut persist_task = pin!(self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    async fn run_ethernet<N, H, X>(&self, mut ethernet: N, handler: H, user: X) -> Result<(), Error>
    where
        N: Ethernet,
        H: AsyncHandler + AsyncMetadata,
        X: UserTask,
    {
        Ethernet::run(&mut ethernet, MatterStackEthernetTask(self, handler, user)).await
    }
}

struct MatterStackEthernetTask<'a, E, H, X>(&'a MatterStack<'a, Eth<E>>, H, X)
where
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask;

impl<E, H, X> EthernetTask for MatterStackEthernetTask<'_, E, H, X>
where
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<U, C>(&mut self, udp: U, netif: C) -> Result<(), Error>
    where
        U: UdpBind,
        C: NetifDiag + NetChangeNotif,
    {
        info!("Ethernet driver started");

        let mut net_task = pin!(self.0.run_oper_net(
            &udp,
            &netif,
            core::future::pending(),
            Option::<(NoNetwork, NoNetwork)>::None,
        ));

        let handler = self.0.root_handler(&(), &true, &netif, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        let mut user_task = pin!(self.2.run(&udp, &netif));

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }
}
