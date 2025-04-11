use core::future::Future;
use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::select3;

use rs_matter::data_model::objects::{
    AsyncHandler, AsyncMetadata, Dataver, Endpoint, HandlerCompat,
};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::ethernet_nw_diagnostics::EthNwDiagCluster;
use rs_matter::data_model::sdm::nw_commissioning::EthNwCommCluster;
use rs_matter::data_model::sdm::{ethernet_nw_diagnostics, nw_commissioning};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;

use crate::netif::Netif;
use crate::network::{Embedding, Network};
use crate::persist::{KvBlobStore, SharedKvBlobStore};
use crate::private::Sealed;
use crate::MatterStack;

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
    async fn run<N, U>(&mut self, netif: N, udp: U) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind;
}

impl<T> EthernetTask for &mut T
where
    T: EthernetTask,
{
    async fn run<N, U>(&mut self, netif: N, udp: U) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind,
    {
        (*self).run(netif, udp).await
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
pub struct PreexistingEthernet<N, U> {
    netif: N,
    udp: U,
}

impl<N, U> PreexistingEthernet<N, U> {
    /// Create a new `PreexistingEthernet` instance with the given network interface and UDP stack.
    pub const fn new(netif: N, udp: U) -> Self {
        Self { netif, udp }
    }
}

impl<N, U> Ethernet for PreexistingEthernet<N, U>
where
    N: Netif,
    U: UdpBind,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: EthernetTask,
    {
        task.run(&mut self.netif, &mut self.udp).await
    }
}

/// A specialization of the `MatterStack` for Ethernet.
impl<E> MatterStack<'_, Eth<E>>
where
    E: Embedding + 'static,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0, OperNwType::Ethernet)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub fn root_handler(&self) -> EthRootEndpointHandler<'_> {
        handler(
            0,
            HandlerCompat(EthNwCommCluster::new(Dataver::new_rand(
                self.matter().rand(),
            ))),
            ethernet_nw_diagnostics::ID,
            HandlerCompat(EthNwDiagCluster::new(Dataver::new_rand(
                self.matter().rand(),
            ))),
            &true,
            self.matter().rand(),
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
    pub async fn run_preex<N, U, S, H, X>(
        &self,
        netif: N,
        udp: U,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        N: Netif,
        U: UdpBind,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: Future<Output = Result<(), Error>>,
    {
        self.run(PreexistingEthernet::new(netif, udp), store, handler, user)
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
        X: Future<Output = Result<(), Error>>,
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

        let mut net_task = pin!(self.run_ethernet(ethernet));
        let mut handler_task = pin!(self.run_handlers(&persist, handler));
        let mut user_task = pin!(user);

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }

    async fn run_ethernet<N>(&self, mut ethernet: N) -> Result<(), Error>
    where
        N: Ethernet,
    {
        #[allow(non_local_definitions)]
        impl<E> EthernetTask for MatterStackEthernetTask<'_, E>
        where
            E: Embedding + 'static,
        {
            async fn run<N, U>(&mut self, netif: N, udp: U) -> Result<(), Error>
            where
                N: Netif,
                U: UdpBind,
            {
                info!("Ethernet driver started");

                self.0
                    .run_oper_net(
                        netif,
                        udp,
                        core::future::pending(),
                        Option::<(NoNetwork, NoNetwork)>::None,
                    )
                    .await
            }
        }

        Ethernet::run(&mut ethernet, MatterStackEthernetTask(self)).await
    }
}

struct MatterStackEthernetTask<'a, E>(&'a MatterStack<'a, Eth<E>>)
where
    E: Embedding + 'static;

/// The type of the handler for the root (Endpoint 0) of the Matter Node
/// when configured for Ethernet network.
pub type EthRootEndpointHandler<'a> = RootEndpointHandler<
    'a,
    HandlerCompat<nw_commissioning::EthNwCommCluster>,
    HandlerCompat<ethernet_nw_diagnostics::EthNwDiagCluster>,
>;
