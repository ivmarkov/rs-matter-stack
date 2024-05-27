use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use rs_matter::error::Error;
use rs_matter::utils::buf::{BufferAccess, PooledBuffers};
use rs_matter::Matter;

use crate::network::{Embedding, Network};
use crate::wifi::WifiContext;
use crate::{Eth, MatterStack, WifiBle, MAX_WIFI_NETWORKS};

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
pub struct KvPersist<'a, T, const N: usize, M>
where
    M: RawMutex,
{
    store: T,
    buf: &'a PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
    matter: &'a Matter<'a>,
    wifi_networks: Option<&'a WifiContext<N, M>>,
}

impl<'a, T> KvPersist<'a, T, 0, NoopRawMutex>
where
    T: KvBlobStore,
{
    /// Create a new `KvPersist` instance for an Ethernet-only Matter stack.
    pub fn new_eth<E>(store: T, stack: &'a MatterStack<Eth<KvBlobBuf<E>>>) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(
            store,
            stack.network().embedding().buf(),
            stack.matter(),
            None,
        )
    }
}

impl<'a, T, M> KvPersist<'a, T, MAX_WIFI_NETWORKS, M>
where
    T: KvBlobStore,
    M: RawMutex,
{
    /// Create a new `KvPersist` instance for a WiFi/BLE Matter stack.
    pub fn new_wifi_ble<E>(store: T, stack: &'a MatterStack<WifiBle<M, KvBlobBuf<E>>>) -> Self
    where
        E: Embedding + 'static,
    {
        Self::wrap(
            store,
            stack.network().embedding().buf(),
            stack.matter(),
            Some(stack.network().wifi_context()),
        )
    }
}

impl<'a, T, const N: usize, M> KvPersist<'a, T, N, M>
where
    T: KvBlobStore,
    M: RawMutex,
{
    /// Create a new `KvPersist` instance.
    pub fn wrap(
        store: T,
        buf: &'a PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
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
        let mut buf = self.buf.get().await.unwrap();
        buf.resize_default(KV_BLOB_BUF_SIZE).unwrap();

        if let Some(data) = self.store.load("acls", &mut buf).await? {
            self.matter.load_acls(data)?;
        }

        if let Some(data) = self.store.load("fabrics", &mut buf).await? {
            self.matter.load_fabrics(data)?;
        }

        if let Some(wifi_networks) = self.wifi_networks {
            if let Some(data) = self.store.load("wifi", &mut buf).await? {
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

            let mut buf = self.buf.get().await.unwrap();
            buf.resize_default(KV_BLOB_BUF_SIZE).unwrap();

            if self.matter.is_changed() {
                if let Some(data) = self.matter.store_acls(&mut buf)? {
                    self.store.store("acls", data).await?;
                }

                if let Some(data) = self.matter.store_fabrics(&mut buf)? {
                    self.store.store("fabrics", data).await?;
                }
            }

            if let Some(wifi_networks) = self.wifi_networks {
                if wifi_networks.is_changed() {
                    if let Some(data) = wifi_networks.store(&mut buf)? {
                        self.store.store("wifi", data).await?;
                    }
                }
            }
        }
    }
}

impl<'a, T, const N: usize, M> Persist for KvPersist<'a, T, N, M>
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
    async fn load<'a>(&mut self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error>;
    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error>;
    async fn remove(&mut self, key: &str) -> Result<(), Error>;
}

impl<T> KvBlobStore for &mut T
where
    T: KvBlobStore,
{
    async fn load<'a>(&mut self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        T::load(self, key, buf).await
    }

    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
        T::store(self, key, value).await
    }

    async fn remove(&mut self, key: &str) -> Result<(), Error> {
        T::remove(self, key).await
    }
}

/// An implementation of the `KvBlobStore` trait that stores the BLOBs in a directory.
#[cfg(feature = "std")]
pub struct DirKvBlobStore(std::path::PathBuf);

#[cfg(feature = "std")]
impl DirKvBlobStore {
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
impl KvBlobStore for DirKvBlobStore {
    async fn load<'a>(&mut self, key: &str, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        DirKvStore::load(self, key, buf)
    }

    async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
        DirKvStore::store(self, key, value)
    }

    async fn remove(&mut self, key: &str) -> Result<(), Error> {
        DirKvStore::remove(self, key)
    }
}

const KV_BLOB_BUF_SIZE: usize = 4096;

/// A buffer for the `KvBlobStore` trait.
pub type KvBlobBuffer = heapless::Vec<u8, KV_BLOB_BUF_SIZE>;

/// An embedding of the buffer necessary for the `KvBlobStore` trait.
/// Allows the memory of this buffer to be statically allocated and cost-initialized.
///
/// Usage:
/// ```no_run
/// MatterStack<WifiBle<M, KvBlobBuf<E>>>::new();
/// ```
/// or:
/// ```no_run
/// MatterStack<Eth<KvBlobBuf<E>>>::new();
/// ```
///
/// ... where `E` can be a next-level, user-supplied embedding or just `()` if the user does not need to embed anything.
pub struct KvBlobBuf<E = ()> {
    buf: PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
    embedding: E,
}

impl<E> KvBlobBuf<E>
where
    E: Embedding,
{
    const fn new() -> Self {
        Self {
            buf: PooledBuffers::new(0),
            embedding: E::INIT,
        }
    }

    pub fn buf(&self) -> &PooledBuffers<1, NoopRawMutex, KvBlobBuffer> {
        &self.buf
    }

    pub fn embedding(&self) -> &E {
        &self.embedding
    }
}

impl<E> Embedding for KvBlobBuf<E>
where
    E: Embedding,
{
    const INIT: Self = Self::new();
}
