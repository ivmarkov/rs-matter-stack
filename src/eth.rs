use core::future::Future;
use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::select3;
use log::info;

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
use crate::persist::Persist;
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

pub type EthMatterStack<'a, E> = MatterStack<'a, Eth<E>>;

/// A specialization of the `MatterStack` for Ethernet.
impl<'a, E> MatterStack<'a, Eth<E>>
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
            true,
            self.matter().rand(),
        )
    }

    /// Resets the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs

        Ok(())
    }

    /// Run the Matter stack for Ethernet network.
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `netif` - a user-provided `Netif` implementation
    /// - `dev_comm` - the commissioning data
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<'d, I, P, H, U>(
        &self,
        netif: I,
        persist: P,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        I: Netif + UdpBind,
        P: Persist,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        // TODO persist.load().await?;

        self.matter().reset_transport()?;

        if !self.is_commissioned().await? {
            self.matter()
                .enable_basic_commissioning(DiscoveryCapabilities::IP, 0)
                .await?; // TODO
        }

        let mut net_task = pin!(self.run_oper_net(
            netif,
            core::future::pending(),
            Option::<(NoNetwork, NoNetwork)>::None
        ));
        let mut handler_task = pin!(self.run_handlers(persist, handler));
        let mut user_task = pin!(user);

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }
}

/// The type of the handler for the root (Endpoint 0) of the Matter Node
/// when configured for Ethernet network.
pub type EthRootEndpointHandler<'a> = RootEndpointHandler<
    'a,
    HandlerCompat<nw_commissioning::EthNwCommCluster>,
    HandlerCompat<ethernet_nw_diagnostics::EthNwDiagCluster>,
>;
