use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use rs_matter::{error::Error, Matter};

use crate::{network::Embedding, wifi::WifiContext, Eth, MatterStack, WifiBle, MAX_WIFI_NETWORKS};

/// A persistent storage manager for the Matter stack.
pub trait Persist {
    /// Reset the persist instance, removing all stored data from the non-volatile storage.
    async fn reset(&mut self) -> Result<(), Error>;

    /// Does an initial load of the Matter stack from the non-volatile storage.
    async fn load(&mut self) -> Result<(), Error>;

    /// Run the persist instance, listening for changes in the Matter stack's state
    /// and persisting these, as well as the network state, to the non-volatile storage.
    async fn run(&mut self) -> Result<(), Error>;
}

impl<T> Persist for &mut T
where
    T: Persist,
{
    async fn reset(&mut self) -> Result<(), Error> {
        T::reset(self).await
    }

    async fn load(&mut self) -> Result<(), Error> {
        T::load(self).await
    }

    async fn run(&mut self) -> Result<(), Error> {
        T::run(self).await
    }
}

/// As the name suggests, this is a dummy implementation of the `Persist` trait
/// that does not persist anything.
pub struct DummyPersist;

impl Persist for DummyPersist {
    async fn reset(&mut self) -> Result<(), Error> {
        // TODO
        Ok(())
    }

    async fn load(&mut self) -> Result<(), Error> {
        Ok(())
    }

    async fn run(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Default for DummyPersist {
    fn default() -> Self {
        Self
    }
}

pub trait KvStore {
    async fn load<'a>(&self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error>;
    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error>;

    async fn remove<'a>(&self, key: &str) -> Result<(), Error>;
}

pub struct KvPersist<'a, 'b, T, const N: usize, M>
where
    M: RawMutex,
{
    store: T,
    buf: &'b mut [u8],
    matter: &'a Matter<'a>,
    wifi_networks: Option<&'a WifiContext<N, M>>,
}

impl<'a, 'b, T> KvPersist<'a, 'b, T, 0, NoopRawMutex>
where
    T: KvStore,
{
    pub fn new_eth<E>(store: T, buf: &'b mut [u8], stack: &'a MatterStack<Eth<E>>) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(store, buf, stack.matter(), None)
    }
}

impl<'a, 'b, T, M> KvPersist<'a, 'b, T, MAX_WIFI_NETWORKS, M>
where
    T: KvStore,
    M: RawMutex,
{
    pub fn new_wifi_ble<E>(
        store: T,
        buf: &'b mut [u8],
        stack: &'a MatterStack<WifiBle<M, E>>,
    ) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(
            store,
            buf,
            stack.matter(),
            Some(stack.network().wifi_context()),
        )
    }
}

impl<'a, 'b, T, const N: usize, M> KvPersist<'a, 'b, T, N, M>
where
    T: KvStore,
    M: RawMutex,
{
    pub fn wrap(
        store: T,
        buf: &'b mut [u8],
        matter: &'a Matter<'a>,
        wifi_networks: Option<&'a WifiContext<N, M>>,
    ) -> Self {
        Self {
            store,
            buf,
            matter,
            wifi_networks,
        }
    }

    pub async fn reset(&mut self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs

        self.store.remove("acls").await?;
        self.store.remove("fabrics").await?;
        self.store.remove("wifi").await?;

        if let Some(wifi_networks) = self.wifi_networks {
            wifi_networks.reset();
        }

        Ok(())
    }

    pub async fn load(&mut self) -> Result<(), Error> {
        if let Some(data) = self.store.load("acls", self.buf).await? {
            self.matter.load_acls(data)?;
        }

        if let Some(data) = self.store.load("fabrics", self.buf).await? {
            self.matter.load_fabrics(data)?;
        }

        if let Some(wifi_networks) = self.wifi_networks {
            if let Some(data) = self.store.load("wifi", self.buf).await? {
                wifi_networks.load(data)?;
            }
        }

        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        loop {
            let wait_matter = self.matter.wait_changed();
            let wait_wifi = async {
                if let Some(wifi_networks) = self.wifi_networks {
                    wifi_networks.wait_state_changed().await;
                } else {
                    core::future::pending::<()>().await;
                }
            };

            select(wait_matter, wait_wifi).await;

            if self.matter.is_changed() {
                if let Some(data) = self.matter.store_acls(self.buf)? {
                    self.store.store("acls", data).await?;
                }

                if let Some(data) = self.matter.store_fabrics(self.buf)? {
                    self.store.store("fabrics", data).await?;
                }
            }

            if let Some(wifi_networks) = self.wifi_networks {
                if wifi_networks.is_changed() {
                    if let Some(data) = wifi_networks.store(self.buf)? {
                        self.store.store("wifi", data).await?;
                    }
                }
            }
        }
    }
}

impl<'a, 'b, T, const N: usize, M> Persist for KvPersist<'a, 'b, T, N, M>
where
    T: KvStore,
    M: RawMutex,
{
    async fn reset(&mut self) -> Result<(), Error> {
        KvPersist::reset(self).await
    }

    async fn load(&mut self) -> Result<(), Error> {
        KvPersist::load(self).await
    }

    async fn run(&mut self) -> Result<(), Error> {
        KvPersist::run(self).await
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
