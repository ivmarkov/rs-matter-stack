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

pub mod comm;
pub mod mgmt;

#[derive(Debug, Clone, ToTLV, FromTLV)]
struct WifiCredentials {
    ssid: heapless::String<32>,
    password: heapless::String<64>,
}

struct WifiStatus {
    ssid: heapless::String<32>,
    status: NetworkCommissioningStatus,
    value: i32,
}

struct WifiState<const N: usize> {
    networks: rs_matter::utils::storage::Vec<WifiCredentials, N>,
    connected_once: bool,
    connect_requested: Option<heapless::String<32>>,
    status: Option<WifiStatus>,
    changed: bool,
}

impl<const N: usize> WifiState<N> {
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

    pub(crate) fn get_next_network(&mut self, last_ssid: Option<&str>) -> Option<WifiCredentials> {
        // Return the requested network with priority
        if let Some(ssid) = self.connect_requested.take() {
            let creds = self.networks.iter().find(|creds| creds.ssid == ssid);

            if let Some(creds) = creds {
                info!("Trying with requested network first - SSID: {}", creds.ssid);

                return Some(creds.clone());
            }
        }

        if let Some(last_ssid) = last_ssid {
            info!("Looking for network after the one with SSID: {}", last_ssid);

            // Return the network positioned after the last one used

            let mut networks = self.networks.iter();

            for network in &mut networks {
                if network.ssid.as_str() == last_ssid {
                    break;
                }
            }

            let creds = networks.next();
            if let Some(creds) = creds {
                info!("Trying with next network - SSID: {}", creds.ssid);

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

    fn load(&mut self, data: &[u8]) -> Result<(), Error> {
        let root = TLVElement::new(data);

        let iter = root.array()?.iter();

        self.networks.clear();

        for creds in iter {
            let creds = creds?;

            self.networks
                .push_init(WifiCredentials::init_from_tlv(creds), || {
                    ErrorCode::NoSpace.into()
                })?;
        }

        self.changed = false;

        Ok(())
    }

    fn store<'m>(&mut self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error> {
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
pub struct WifiContext<const N: usize, M>
where
    M: RawMutex,
{
    state: Mutex<M, RefCell<WifiState<N>>>,
    state_changed: Notification<M>,
    network_connect_requested: Notification<M>,
}

impl<const N: usize, M> WifiContext<N, M>
where
    M: RawMutex,
{
    /// Create a new instance.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(WifiState::new())),
            state_changed: Notification::new(),
            network_connect_requested: Notification::new(),
        }
    }

    pub fn init() -> impl Init<Self> {
        init!(Self {
            state <- Mutex::init(RefCell::init(WifiState::init())),
            state_changed: Notification::new(),
            network_connect_requested: Notification::new(),
        })
    }

    /// Reset the state.
    pub fn reset(&self) {
        self.state.lock(|state| state.borrow_mut().reset());
    }

    /// Load the state from a byte slice.
    pub fn load(&self, data: &[u8]) -> Result<(), Error> {
        self.state.lock(|state| state.borrow_mut().load(data))
    }

    /// Store the state into a byte slice.
    pub fn store<'m>(&self, buf: &'m mut [u8]) -> Result<Option<&'m [u8]>, Error> {
        self.state.lock(|state| state.borrow_mut().store(buf))
    }

    pub fn is_changed(&self) -> bool {
        self.state.lock(|state| state.borrow().changed)
    }

    pub fn is_network_connect_requested(&self) -> bool {
        self.state
            .lock(|state| state.borrow().connect_requested.is_some())
    }

    /// Wait until signalled by the Matter stack that a network connect request is issued during commissioning.
    ///
    /// Typically, this is a signal that the BLE/BTP transport should be teared down and
    /// the Wifi transport should be brought up.
    pub async fn wait_network_connect(&self) {
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
            "Giving BLE/BTP extra 4 seconds for any outstanding messages before switching to Wifi"
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

impl<const N: usize, M> Default for WifiContext<N, M>
where
    M: RawMutex,
{
    fn default() -> Self {
        Self::new()
    }
}
