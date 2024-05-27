use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use rs_matter::error::Error;
use rs_matter::Matter;

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

/// A persistent storage implementation that relies on a BLOB key-value storage
/// represented by the `KvBlobStore` trait.
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
    T: KvBlobStore,
{
    /// Create a new `KvPersist` instance for an Ethernet-only Matter stack.
    pub fn new_eth<E>(store: T, buf: &'b mut [u8], stack: &'a MatterStack<Eth<E>>) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(store, buf, stack.matter(), None)
    }
}

impl<'a, 'b, T, M> KvPersist<'a, 'b, T, MAX_WIFI_NETWORKS, M>
where
    T: KvBlobStore,
    M: RawMutex,
{
    /// Create a new `KvPersist` instance for a WiFi/BLE Matter stack.
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
    T: KvBlobStore,
    M: RawMutex,
{
    /// Create a new `KvPersist` instance.
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

    /// Reset the persist instance, removing all stored data from the non-volatile storage
    /// as well as removing all ACLs, fabrics and Wifi networks from the MAtter stack.
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

    /// Load the Matter stack from the non-volatile storage.
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

    /// Run the persist instance, listening for changes in the Matter stack's state.
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
    T: KvBlobStore,
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

/// A trait representing a key-value BLOB storage.
pub trait KvBlobStore {
    async fn load<'a>(&self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error>;
    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error>;
    async fn remove(&self, key: &str) -> Result<(), Error>;
}

/// An implementation of the `KvBlobStore` trait that stores the BLOBs in a directory.
#[cfg(feature = "std")]
pub struct DirKvStore(std::path::PathBuf);

#[cfg(feature = "std")]
impl DirKvStore {
    /// Create a new `DirKvStore` instance.
    pub const fn new(path: std::path::PathBuf) -> Self {
        Self(path)
    }

    /// Load a BLOB with the specified key from the directory.
    pub fn load<'b>(&self, key: &str, buf: &'b mut [u8]) -> Result<Option<&'b [u8]>, Error> {
        use log::info;
        use std::io::Read;

        let path = self.key_path(key);

        match std::fs::File::open(path) {
            Ok(mut file) => {
                let mut offset = 0;

                loop {
                    if offset == buf.len() {
                        Err(rs_matter::error::ErrorCode::NoSpace)?;
                    }

                    let len = file.read(&mut buf[offset..])?;

                    if len == 0 {
                        break;
                    }

                    offset += len;
                }

                let data = &buf[..offset];

                info!("Key {}: loaded {} bytes {:?}", key, data.len(), data);

                Ok(Some(data))
            }
            Err(_) => Ok(None),
        }
    }

    /// Store a BLOB with the specified key in the directory.
    fn store(&self, key: &str, data: &[u8]) -> Result<(), Error> {
        use log::info;
        use std::io::Write;

        std::fs::create_dir_all(&self.0)?;

        let path = self.key_path(key);

        let mut file = std::fs::File::create(path)?;

        file.write_all(data)?;

        info!("Key {}: stored {} bytes {:?}", key, data.len(), data);

        Ok(())
    }

    /// Remove a BLOB with the specified key from the directory.
    /// If the BLOB does not exist, this method does nothing.
    fn remove(&self, key: &str) -> Result<(), Error> {
        use log::info;

        let path = self.key_path(key);

        if std::fs::remove_file(path).is_ok() {
            info!("Key {}: removed", key);
        }

        Ok(())
    }

    fn key_path(&self, key: &str) -> std::path::PathBuf {
        self.0.join(key)
    }
}

#[cfg(feature = "std")]
impl KvBlobStore for DirKvStore {
    async fn load<'a>(&self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        DirKvStore::load(self, key, buf)
    }

    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
        DirKvStore::store(self, key, value)
    }

    async fn remove(&self, key: &str) -> Result<(), Error> {
        DirKvStore::remove(self, key)
    }
}
