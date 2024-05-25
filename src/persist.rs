use embassy_sync::blocking_mutex::raw::RawMutex;

use rs_matter::Matter;

use crate::{error::Error, wifi::WifiContext};

/// Represents the network type currently in use.
pub enum NetworkContext<'a, const N: usize, M>
where
    M: RawMutex,
{
    /// The Matter stack uses an Ethernet network for operating
    /// or in general, a network that is not managed by the stack
    /// and therefore does not need to be stored in the NVS.
    Eth,
    /// The Matter stack uses Wifi for operating.
    Wifi(&'a WifiContext<N, M>),
}

impl<'a, const N: usize, M> NetworkContext<'a, N, M>
where
    M: RawMutex,
{
    pub const fn key(&self) -> Option<&str> {
        match self {
            Self::Eth => None,
            Self::Wifi(_) => Some("wifi"),
        }
    }
}

impl<'a, const N: usize, M> Clone for NetworkContext<'a, N, M>
where
    M: RawMutex,
{
    fn clone(&self) -> Self {
        match self {
            Self::Eth => Self::Eth,
            Self::Wifi(wifi) => Self::Wifi(wifi),
        }
    }
}

/// A persistent storage manager for the Matter stack.
pub trait Persist {
    /// Reset the persist instance, removing all stored data from the non-volatile storage.
    async fn reset(&mut self) -> Result<(), Error>;

    /// Run the persist instance, listening for changes in the Matter stack's state
    /// and persisting these, as well as the network state, to the non-volatile storage.
    async fn run<const N: usize, M>(
        &mut self,
        matter: &Matter<'_>,
        network: NetworkContext<'_, N, M>,
    ) -> Result<(), Error>
    where
        M: RawMutex;
}

impl<T> Persist for &mut T
where
    T: Persist,
{
    async fn reset(&mut self) -> Result<(), Error> {
        (*self).reset().await
    }

    async fn run<const N: usize, M>(
        &mut self,
        matter: &Matter<'_>,
        network: NetworkContext<'_, N, M>,
    ) -> Result<(), Error>
    where
        M: RawMutex,
    {
        (*self).run(matter, network).await
    }
}
