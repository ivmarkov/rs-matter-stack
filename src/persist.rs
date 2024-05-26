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

/// As the name suggests, this is a dummy implementation of the `Persist` trait
/// that does not persist anything.
pub struct DummyPersist;

impl<N> Persist<N> for DummyPersist {
    async fn reset(&mut self, _: &N, _: &Matter<'_>) -> Result<(), Error> {
        Ok(())
    }

    async fn run(&mut self, _: &N, _: &Matter<'_>) -> Result<(), Error> {
        Ok(())
    }
}

impl Default for DummyPersist {
    fn default() -> Self {
        Self
    }
}

// #[cfg(feature = "std")]
// pub struct FilePersist<'a>(rs_matter::persist::Psm<'a>);

// #[cfg(feature = "std")]
// impl<'a> FilePersist<'a> {
//     pub fn new(matter: &'a Matter<'a>, dir: std::path::PathBuf) -> Result<Self, Error> {
//         Ok(Self(rs_matter::persist::Psm::new(matter, dir)?))
//     }
// }

// impl
