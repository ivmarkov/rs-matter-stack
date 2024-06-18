use core::borrow::Borrow;
use core::future::Future;
use core::pin::pin;

use edge_nal::UdpBind;

use log::info;

use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata, Endpoint, HandlerCompat};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::ethernet_nw_diagnostics::EthNwDiagCluster;
use rs_matter::data_model::sdm::nw_commissioning::EthNwCommCluster;
use rs_matter::data_model::sdm::{ethernet_nw_diagnostics, nw_commissioning};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::CommissioningData;

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
pub struct Eth<E = ()>(E);

impl<E> Network for Eth<E>
where
    E: Embedding + 'static,
{
    const INIT: Self = Self(E::INIT);

    type Embedding = E;

    fn embedding(&self) -> &Self::Embedding {
        &self.0
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
            self.matter(),
            HandlerCompat(EthNwCommCluster::new(*self.matter().borrow())),
            ethernet_nw_diagnostics::ID,
            HandlerCompat(EthNwDiagCluster::new(*self.matter().borrow())),
            true,
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
    pub async fn run<'d, H, P, I, U>(
        &self,
        persist: P,
        netif: I,
        dev_comm: CommissioningData,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        I: Netif + UdpBind,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let mut user = pin!(user);

        self.run_with_netif(
            persist,
            netif,
            Some((dev_comm, DiscoveryCapabilities::new(true, false, false))),
            handler,
            &mut user,
        )
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
