//! The network state store for the wireless module.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::RawMutex;

use log::info;

use rs_matter::data_model::sdm::general_commissioning::ConcurrentConnectionPolicy;
use rs_matter::data_model::sdm::nw_commissioning::NetworkCommissioningStatus;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, TLVElement, TLVTag, ToTLV};
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::storage::WriteBuf;
use rs_matter::utils::sync::blocking::Mutex;
use rs_matter::utils::sync::Notification;

use crate::persist::{MatterStackKey, NetworkPersist};
use crate::private::Sealed;

use super::proxy::ControllerProxy;
use super::traits::WirelessData;
use super::NetworkCredentials;

pub(crate) struct NetworkStatus<I> {
    pub(crate) network_id: I,
    pub(crate) status: NetworkCommissioningStatus,
    pub(crate) value: i32,
}

pub(crate) struct NetworkState<const N: usize, T>
where
    T: NetworkCredentials,
{
    pub(crate) networks: rs_matter::utils::storage::Vec<T, N>,
    pub(crate) connected_once: bool,
    pub(crate) connect_requested: Option<T::NetworkId>,
    pub(crate) status: Option<NetworkStatus<T::NetworkId>>,
    pub(crate) changed: bool,
}

impl<const N: usize, T> NetworkState<N, T>
where
    T: NetworkCredentials + Clone,
{
    const fn new() -> Self {
        Self {
            networks: rs_matter::utils::storage::Vec::new(),
            connected_once: false,
            connect_requested: None,
            status: None,
            changed: false,
        }
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            networks <- rs_matter::utils::storage::Vec::init(),
            connected_once: false,
            connect_requested: None,
            status: None,
            changed: false,
        })
    }

    pub(crate) fn get_next_network(&mut self, last_network_id: Option<&T::NetworkId>) -> Option<T> {
        // Return the requested network with priority
        if let Some(network_id) = self.connect_requested.take() {
            let creds = self
                .networks
                .iter()
                .find(|creds| creds.network_id() == network_id);

            if let Some(creds) = creds {
                info!(
                    "Trying with requested network first - ID: {}",
                    creds.network_id()
                );

                return Some(creds.clone());
            }
        }

        if let Some(last_network_id) = last_network_id {
            info!(
                "Looking for network after the one with ID: {}",
                last_network_id
            );

            // Return the network positioned after the last one used

            let mut networks = self.networks.iter();

            for network in &mut networks {
                if network.network_id() == *last_network_id {
                    break;
                }
            }

            let creds = networks.next();
            if let Some(creds) = creds {
                info!("Trying with next network - ID: {}", creds.network_id());

                return Some(creds.clone());
            }
        }

        // Wrap over
        info!("Wrapping over");

        self.networks.first().cloned()
    }

    fn reset(&mut self) {
        self.networks.clear();
        self.connected_once = false;
        self.connect_requested = None;
        self.status = None;
        self.changed = false;
    }

    fn load(&mut self, data: &[u8]) -> Result<(), Error>
    where
        T: for<'a> FromTLV<'a>,
    {
        let root = TLVElement::new(data);

        let iter = root.array()?.iter();

        self.networks.clear();

        for creds in iter {
            let creds = creds?;

            self.networks
                .push_init(T::init_from_tlv(creds), || ErrorCode::NoSpace.into())?;
        }

        self.changed = false;

        Ok(())
    }

    fn store(&mut self, buf: &mut [u8]) -> Result<usize, Error>
    where
        T: ToTLV,
    {
        let mut wb = WriteBuf::new(buf);

        self.networks.to_tlv(&TLVTag::Anonymous, &mut wb)?;

        self.changed = false;

        Ok(wb.get_tail())
    }
}

/// The `'static` state of the Wifi module.
/// Isolated as a separate struct to allow for `const fn` construction
/// and static allocation.
pub struct NetworkContext<const N: usize, M, T>
where
    M: RawMutex,
    T: WirelessData,
{
    pub(crate) state: Mutex<M, RefCell<NetworkState<N, T::NetworkCredentials>>>,
    pub(crate) state_changed: Notification<M>,
    pub(crate) controller_proxy: ControllerProxy<M, T>,
    pub(crate) concurrent_connection: Mutex<M, Cell<bool>>,
    pub(crate) network_connect_requested: Notification<M>,
}

impl<const N: usize, M, T> NetworkContext<N, M, T>
where
    M: RawMutex,
    T: WirelessData,
    T::NetworkCredentials: Clone,
{
    /// Create a new instance.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(NetworkState::new())),
            state_changed: Notification::new(),
            controller_proxy: ControllerProxy::new(),
            concurrent_connection: Mutex::new(Cell::new(false)),
            network_connect_requested: Notification::new(),
        }
    }

    /// Return an in-place initializer for the struct.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            state <- Mutex::init(RefCell::init(NetworkState::init())),
            state_changed: Notification::new(),
            controller_proxy <- ControllerProxy::init(),
            concurrent_connection <- Mutex::init(Cell::new(false)),
            network_connect_requested: Notification::new(),
        })
    }

    /// Reset the state.
    pub fn reset(&self) {
        self.state.lock(|state| state.borrow_mut().reset());
    }

    /// Load the state from a byte slice.
    pub fn load(&self, data: &[u8]) -> Result<(), Error>
    where
        T::NetworkCredentials: for<'a> FromTLV<'a>,
    {
        self.state.lock(|state| state.borrow_mut().load(data))
    }

    /// Store the state into a byte slice.
    pub fn store(&self, buf: &mut [u8]) -> Result<usize, Error>
    where
        T::NetworkCredentials: ToTLV,
    {
        self.state.lock(|state| state.borrow_mut().store(buf))
    }

    /// Return `true` if the state has changed.
    pub fn changed(&self) -> bool {
        self.state.lock(|state| state.borrow().changed)
    }

    pub fn is_network_activated(&self) -> bool {
        self.state
            .lock(|state| state.borrow().connect_requested.is_some())
    }

    /// Wait until signalled by the Matter stack that a network connect request is issued during commissioning.
    ///
    /// Typically, this is a signal that the BLE/BTP transport should be teared down and
    /// the Wifi transport should be brought up for the Matter non-concurrent connections' case.
    pub async fn wait_network_activated(&self) {
        loop {
            if self
                .state
                .lock(|state| state.borrow().connect_requested.is_some())
            {
                break;
            }

            self.network_connect_requested.wait().await;
        }
    }

    pub async fn wait_state_changed(&self) {
        loop {
            if self.state.lock(|state| state.borrow().changed) {
                break;
            }

            self.state_changed.wait().await;
        }
    }
}

impl<const N: usize, M, T> Default for NetworkContext<N, M, T>
where
    M: RawMutex,
    T: WirelessData,
    T::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, M, T> Sealed for &NetworkContext<N, M, T>
where
    M: RawMutex,
    T: WirelessData,
    T::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
{
}

impl<const N: usize, M, T> NetworkPersist for &NetworkContext<N, M, T>
where
    M: RawMutex,
    T: WirelessData,
    T::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
{
    const KEY: crate::persist::MatterStackKey = if T::WIFI {
        MatterStackKey::WifiNetworks
    } else {
        MatterStackKey::ThreadNetworks
    };

    fn reset(&self) -> Result<(), Error> {
        NetworkContext::reset(self);

        Ok(())
    }

    fn load(&self, data: &[u8]) -> Result<(), Error> {
        NetworkContext::load(self, data)
    }

    fn store(&self, buf: &mut [u8]) -> Result<usize, Error> {
        NetworkContext::store(self, buf)
    }

    fn changed(&self) -> bool {
        NetworkContext::changed(self)
    }

    async fn wait_state_changed(&self) {
        NetworkContext::wait_state_changed(self).await;
    }
}

impl<const N: usize, M, T> ConcurrentConnectionPolicy for NetworkContext<N, M, T>
where
    M: RawMutex,
    T: WirelessData,
    T::NetworkCredentials: Clone + for<'a> FromTLV<'a> + ToTLV,
{
    fn concurrent_connection_supported(&self) -> bool {
        self.concurrent_connection.lock(|state| state.get())
    }
}
