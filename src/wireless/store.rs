//! The network state store for the wireless module.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Timer};

use log::{info, warn};

use rs_matter::data_model::sdm::nw_commissioning::NetworkCommissioningStatus;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, TLVElement, TLVTag, ToTLV};
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::storage::WriteBuf;
use rs_matter::utils::sync::blocking::Mutex;
use rs_matter::utils::sync::Notification;

use crate::persist::NetworkPersist;
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
                .find(|creds| creds.network_id() == &network_id);

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
                if network.network_id() == last_network_id {
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

    fn store<'m>(&mut self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error>
    where
        T: ToTLV,
    {
        if !self.changed {
            return Ok(None);
        }

        let mut wb = WriteBuf::new(buf);

        self.networks.to_tlv(&TLVTag::Anonymous, &mut wb)?;

        self.changed = false;

        let len = wb.get_tail();

        Ok(Some(&buf[..len]))
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
            network_connect_requested: Notification::new(),
        }
    }

    /// Return an in-place initializer for the struct.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            state <- Mutex::init(RefCell::init(NetworkState::init())),
            state_changed: Notification::new(),
            controller_proxy <- ControllerProxy::init(),
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
    pub fn store<'m>(&self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error>
    where
        T::NetworkCredentials: ToTLV,
    {
        self.state.lock(|state| state.borrow_mut().store(buf))
    }

    /// Return `true` if the state has changed.
    pub fn is_changed(&self) -> bool {
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

        warn!(
            "Giving BLE/BTP extra 4 seconds for any outstanding messages before switching to the operational network"
        );

        Timer::after(Duration::from_secs(4)).await;
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
    async fn reset(&mut self) -> Result<(), Error> {
        NetworkContext::reset(self);

        Ok(())
    }

    async fn load(&mut self, data: &[u8]) -> Result<(), Error> {
        NetworkContext::load(self, data)
    }

    async fn store<'a>(&mut self, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        NetworkContext::store(self, buf)
    }

    async fn wait_state_changed(&self) {
        NetworkContext::wait_state_changed(self).await;
    }
}
