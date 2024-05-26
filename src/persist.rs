use rs_matter::Matter;

use crate::error::Error;

/// A persistent storage manager for the Matter stack.
pub trait Persist<N> {
    /// Reset the persist instance, removing all stored data from the non-volatile storage.
    async fn reset(&mut self, network: &N, matter: &Matter<'_>) -> Result<(), Error>;

    /// Run the persist instance, listening for changes in the Matter stack's state
    /// and persisting these, as well as the network state, to the non-volatile storage.
    async fn run(&mut self, network: &N, matter: &Matter<'_>) -> Result<(), Error>;
}

impl<T, N> Persist<N> for &mut T
where
    T: Persist<N>,
{
    async fn reset(&mut self, network: &N, matter: &Matter<'_>) -> Result<(), Error> {
        T::reset(self, network, matter).await
    }

    async fn run(&mut self, network: &N, matter: &Matter<'_>) -> Result<(), Error> {
        T::run(self, network, matter).await
    }
}
