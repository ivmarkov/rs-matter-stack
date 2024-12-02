use core::fmt::{self, Display};

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use rs_matter::error::Error;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::storage::pooled::{BufferAccess, PooledBuffers};
use rs_matter::Matter;

use crate::network::{Embedding, Network};
use crate::private::Sealed;
use crate::MatterStack;

#[cfg(feature = "std")]
pub use file::DirKvBlobStore;

/// A perist API that needs to be implemented by the network impl which is used in the Matter stack.
///
/// The trait is sealed and has only two implementations:
/// - `()` - which is used with the `Eth` network
/// - `&NetworkContext` - which is used with the `WirelessBle` network.
pub trait NetworkPersist: Sealed {
    const KEY: Key;

    /// Reset all networks, removing all stored data from the memory
    fn reset(&mut self) -> Result<(), Error>;

    /// Load the networks from the provided data BLOB
    fn load(&mut self, data: &[u8]) -> Result<(), Error>;

    /// Save the networks as BLOB using the provided data buffer
    ///
    /// Return the length of the data written in the provided buffer.
    fn store(&mut self, buf: &mut [u8]) -> Result<usize, Error>;

    /// Check if the networks have changed.
    ///
    /// This method should return `Ok(true)` if the networks have changed since the last call to `changed`.
    fn changed(&mut self) -> Result<bool, Error>;

    /// Wait until the networks have changed.
    ///
    /// This method might return even if the networks have not changed,
    /// so the ultimate litmus test whether the networks did indeed change is trying to call
    /// `store` and then inspecting if it returned something.
    async fn wait_state_changed(&mut self);
}

/// A no-op implementation of the `NetworksPersist` trait.
///
/// Used when the Matter stack is configured for Ethernet, as in that case
/// there is no network state that needs to be saved
impl NetworkPersist for () {
    const KEY: Key = Key::EthNetworks; // Does not matter really as Ethernet networks do not have a persistence story

    fn reset(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn load(&mut self, _data: &[u8]) -> Result<(), Error> {
        Ok(())
    }

    fn store(&mut self, _buf: &mut [u8]) -> Result<usize, Error> {
        Ok(0)
    }

    fn changed(&mut self) -> Result<bool, Error> {
        Ok(false)
    }

    async fn wait_state_changed(&mut self) {}
}

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
#[derive(Debug)]
pub struct DummyPersist;

impl Persist for DummyPersist {
    async fn reset(&mut self) -> Result<(), Error> {
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
pub struct KvPersist<'a, T, C> {
    store: T,
    buf: &'a PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
    matter: &'a Matter<'a>,
    networks: C,
}

impl<'a, T, C> KvPersist<'a, T, C>
where
    T: KvBlobStore,
    C: NetworkPersist,
{
    /// Create a new `KvPersist` instance.
    pub fn wrap(
        store: T,
        buf: &'a PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
        matter: &'a Matter<'a>,
        networks: C,
    ) -> Self {
        Self {
            store,
            buf,
            matter,
            networks,
        }
    }

    /// Reset the persist instance, removing all stored data from the non-volatile storage
    /// as well as removing all ACLs, fabrics and Wifi networks from the MAtter stack.
    pub async fn reset(&mut self) -> Result<(), Error> {
        // TODO: Reset fabrics

        let mut buf = self.buf.get().await.unwrap();
        buf.resize_default(KV_BLOB_BUF_SIZE).unwrap();

        self.store.remove(Key::Fabrics, &mut buf).await?;
        self.store.remove(Key::EthNetworks, &mut buf).await?;
        self.store.remove(Key::WifiNetworks, &mut buf).await?;
        self.store.remove(Key::ThreadNetworks, &mut buf).await?;

        self.networks.reset()?;

        Ok(())
    }

    /// Load the Matter stack from the non-volatile storage.
    pub async fn load(&mut self) -> Result<(), Error> {
        let mut buf = self.buf.get().await.unwrap();
        buf.resize_default(KV_BLOB_BUF_SIZE).unwrap();

        self.store
            .load(Key::Fabrics, &mut buf, |data| {
                if let Some(data) = data {
                    self.matter.load_fabrics(data)?;
                }

                Ok(())
            })
            .await?;

        self.store
            .load(C::KEY, &mut buf, |data| {
                if let Some(data) = data {
                    self.networks.load(data)?;
                }

                Ok(())
            })
            .await?;

        Ok(())
    }

    /// Run the persist instance, listening for changes in the Matter stack's state.
    pub async fn run(&mut self) -> Result<(), Error> {
        loop {
            let wait_fabrics = self.matter.wait_fabrics_changed();
            let wait_wifi = self.networks.wait_state_changed();

            select(wait_fabrics, wait_wifi).await;

            let mut buf = self.buf.get().await.unwrap();
            buf.resize_default(KV_BLOB_BUF_SIZE).unwrap();

            if self.matter.fabrics_changed() {
                self.store
                    .store(Key::Fabrics, &mut buf, |buf| {
                        self.matter
                            .store_fabrics(buf)
                            .map(|data| data.map(|data| data.len()).unwrap_or(0))
                    })
                    .await?;
            }

            self.store
                .store(C::KEY, &mut buf, |buf| self.networks.store(buf))
                .await?;
        }
    }
}

impl<T, C> Persist for KvPersist<'_, T, C>
where
    T: KvBlobStore,
    C: NetworkPersist,
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

/// The keys the `KvBlobStore` trait uses to store the BLOBs.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum Key {
    Fabrics = 0,
    EthNetworks = 1,
    WifiNetworks = 2,
    ThreadNetworks = 3,
}

impl From<Key> for u8 {
    fn from(key: Key) -> u8 {
        key as u8
    }
}

impl AsRef<str> for Key {
    fn as_ref(&self) -> &str {
        match self {
            Key::Fabrics => "fabrics",
            Key::EthNetworks => "eth-net",
            Key::WifiNetworks => "wifi-net",
            Key::ThreadNetworks => "thread-net",
        }
    }
}

impl Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_ref())
    }
}

/// A trait representing a key-value BLOB storage.
pub trait KvBlobStore {
    async fn load<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>;

    async fn store<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>;

    async fn remove(&mut self, key: Key, buf: &mut [u8]) -> Result<(), Error>;
}

impl<T> KvBlobStore for &mut T
where
    T: KvBlobStore,
{
    async fn load<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        T::load(self, key, buf, cb).await
    }

    async fn store<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        T::store(self, key, buf, cb).await
    }

    async fn remove(&mut self, key: Key, buf: &mut [u8]) -> Result<(), Error> {
        T::remove(self, key, buf).await
    }
}

/// Create a new `KvPersist` instance for a Matter stack.
pub fn new_kv<'a, T, N, E>(
    store: T,
    stack: &'a MatterStack<N>,
) -> KvPersist<'a, T, N::PersistContext<'a>>
where
    T: KvBlobStore,
    N: Network<Embedding = KvBlobBuf<E>>,
    E: Embedding + 'static,
{
    KvPersist::wrap(
        store,
        stack.network().embedding().buf(),
        stack.matter(),
        stack.network().persist_context(),
    )
}

#[cfg(feature = "std")]
mod file {
    use std::io::{Read, Write};

    use log::debug;

    use rs_matter::error::Error;

    use super::{Key, KvBlobStore};

    extern crate std;

    /// An implementation of the `KvBlobStore` trait that stores the BLOBs in a directory.
    ///
    /// The BLOBs are stored in files named after the keys in the specified directory.
    ///
    /// The implementation has its own public API, so that the user is able to load and store
    /// additional BLOBs, unrelated to the Matter-specific `KvBlobStore`.
    ///
    /// NOTE: The keys should be valid file names!
    #[derive(Debug, Clone)]
    pub struct DirKvBlobStore(std::path::PathBuf);

    impl DirKvBlobStore {
        /// Create a new `DirKvStore` instance, which will persist
        /// its settings in `<tmp-dir>/rs-matter`.
        pub fn new_default() -> Self {
            Self(std::env::temp_dir().join("rs-matter"))
        }

        /// Create a new `DirKvStore` instance.
        pub const fn new(path: std::path::PathBuf) -> Self {
            Self(path)
        }

        /// Load a BLOB with the specified key from the directory.
        pub fn load<'a, K>(&self, key: K, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error>
        where
            K: AsRef<str>,
        {
            let path = self.key_path(&key);

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

                    debug!("Key {}: loaded {}B ({:?})", key.as_ref(), data.len(), data);

                    Ok(Some(data))
                }
                Err(_) => Ok(None),
            }
        }

        /// Store a BLOB with the specified key in the directory.
        pub fn store<K>(&self, key: K, data: &[u8]) -> Result<(), Error>
        where
            K: AsRef<str>,
        {
            let path = self.key_path(&key);

            std::fs::create_dir_all(path.parent().unwrap())?;

            let mut file = std::fs::File::create(path)?;

            file.write_all(data)?;

            debug!("Key {}: stored {}B ({:?})", key.as_ref(), data.len(), data);

            Ok(())
        }

        /// Remove a BLOB with the specified key from the directory.
        /// If the BLOB does not exist, this method does nothing.
        pub fn remove<K>(&self, key: K) -> Result<(), Error>
        where
            K: AsRef<str>,
        {
            let path = self.key_path(&key);

            if std::fs::remove_file(path).is_ok() {
                debug!("Key {}: removed", key.as_ref());
            }

            Ok(())
        }

        fn key_path<K>(&self, key: &K) -> std::path::PathBuf
        where
            K: AsRef<str>,
        {
            self.0.join(key.as_ref())
        }
    }

    impl Default for DirKvBlobStore {
        fn default() -> Self {
            Self::new_default()
        }
    }

    impl KvBlobStore for DirKvBlobStore {
        async fn load<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
        where
            F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
        {
            let data = DirKvBlobStore::load(self, key, buf)?;

            cb(data)
        }

        async fn store<F>(&mut self, key: Key, buf: &mut [u8], cb: F) -> Result<(), Error>
        where
            F: FnOnce(&mut [u8]) -> Result<usize, Error>,
        {
            let data_len = cb(buf)?;

            DirKvBlobStore::store(self, key, &buf[..data_len])
        }

        async fn remove(&mut self, key: Key, _buf: &mut [u8]) -> Result<(), Error> {
            DirKvBlobStore::remove(self, key)
        }
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
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    const fn new() -> Self {
        Self {
            buf: PooledBuffers::new(0),
            embedding: E::INIT,
        }
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            buf <- PooledBuffers::init(0),
            embedding <- E::init(),
        })
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

    fn init() -> impl Init<Self> {
        KvBlobBuf::init()
    }
}
