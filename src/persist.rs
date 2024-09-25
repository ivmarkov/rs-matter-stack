use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use rs_matter::error::Error;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::storage::pooled::{BufferAccess, PooledBuffers};
use rs_matter::Matter;

use crate::network::{Embedding, Network};
use crate::MatterStack;

#[cfg(feature = "std")]
pub use file::DirKvBlobStore;

/// A perist API that needs to be implemented by the network impl which is used in the Matter stack.
pub trait NetworkPersist {
    /// Reset all networks, removing all stored data from the memory
    async fn reset(&mut self) -> Result<(), Error>;

    /// Load the networks from the provided data BLOB
    async fn load(&mut self, data: &[u8]) -> Result<(), Error>;

    /// Save the networks as BLOB using the provided data buffer
    ///
    /// Return a sub-slice of the buffer which contains the data to be written
    /// to the non-volatile storage.
    ///
    /// If the networks have not changed since the last call to this method,
    /// this method should return `None`.
    async fn store<'a>(&mut self, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error>;

    /// Wait until the networks have changed.
    ///
    /// This method might return even if the networks have not changed,
    /// so the ultimate litmus test whether the networks did indeed change is trying to call
    /// `store` and then inspecting if it returned something.
    async fn wait_state_changed(&self);
}

impl<T> NetworkPersist for &mut T
where
    T: NetworkPersist,
{
    async fn reset(&mut self) -> Result<(), Error> {
        T::reset(self).await
    }

    async fn load(&mut self, data: &[u8]) -> Result<(), Error> {
        T::load(self, data).await
    }

    async fn store<'a>(&mut self, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        T::store(self, buf).await
    }

    async fn wait_state_changed(&self) {
        T::wait_state_changed(self).await
    }
}

/// A no-op implementation of the `NetworksPersist` trait.
///
/// Useful when the Matter stack is configured for Ethernet, as in that case
/// there is no network state that needs to be saved
impl NetworkPersist for () {
    async fn reset(&mut self) -> Result<(), Error> {
        Ok(())
    }

    async fn load(&mut self, _data: &[u8]) -> Result<(), Error> {
        Ok(())
    }

    async fn store<'a>(&mut self, _buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        Ok(None)
    }

    async fn wait_state_changed(&self) {}
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

        self.store.remove("fabrics").await?;
        self.store.remove("networks").await?;

        self.networks.reset().await?;

        Ok(())
    }

    /// Load the Matter stack from the non-volatile storage.
    pub async fn load(&mut self) -> Result<(), Error> {
        let mut buf = self.buf.get().await.unwrap();
        buf.resize_default(KV_BLOB_BUF_SIZE).unwrap();

        if let Some(data) = self.store.load("fabrics", &mut buf).await? {
            self.matter.load_fabrics(data)?;
        }

        if let Some(data) = self.store.load("networks", &mut buf).await? {
            self.networks.load(data).await?;
        }

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
                if let Some(data) = self.matter.store_fabrics(&mut buf)? {
                    self.store.store("fabrics", data).await?;
                }
            }

            if let Some(data) = self.networks.store(&mut buf).await? {
                self.store.store("networks", data).await?;
            }
        }
    }
}

impl<'a, T, C> Persist for KvPersist<'a, T, C>
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
    use rs_matter::error::Error;

    use super::KvBlobStore;

    extern crate std;

    /// An implementation of the `KvBlobStore` trait that stores the BLOBs in a directory.
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

    impl Default for DirKvBlobStore {
        fn default() -> Self {
            Self::new_default()
        }
    }

    impl KvBlobStore for DirKvBlobStore {
        async fn load<'a>(
            &mut self,
            key: &str,
            buf: &'a mut [u8],
        ) -> Result<Option<&'a [u8]>, Error> {
            DirKvBlobStore::load(self, key, buf)
        }

        async fn store(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
            DirKvBlobStore::store(self, key, value)
        }

        async fn remove(&mut self, key: &str) -> Result<(), Error> {
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
