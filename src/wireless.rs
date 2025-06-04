use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::{select, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;

use rs_matter::data_model::networks::wireless::{
    NetCtlState, WirelessMgr, WirelessNetwork, WirelessNetworks, MAX_CREDS_SIZE,
};
use rs_matter::data_model::networks::NetChangeNotif;
use rs_matter::data_model::sdm::gen_diag::NetifDiag;
use rs_matter::data_model::sdm::net_comm::NetCtl;
use rs_matter::data_model::sdm::wifi_diag::WirelessDiag;
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::{Btp, BtpContext, GattPeripheral};
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, zeroed, Init};
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::sync::{blocking, IfMutex};

use crate::network::{Embedding, Network};
use crate::private::Sealed;
use crate::MatterStack;

pub use gatt::*;
pub use thread::*;
pub use wifi::*;

mod gatt;
mod thread;
mod wifi;

const MAX_WIRELESS_NETWORKS: usize = 2;

/// A type alias for a Matter stack running over either Wifi or Thread (and BLE, during commissioning).
pub type WirelessMatterStack<'a, M, T, E = ()> = MatterStack<'a, WirelessBle<M, T, E>>;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
///
/// The supported commissioning is either concurrent or non-concurrent (as per the Matter Core spec),
/// where one over the other is decided at runtime with the concrete wireless implementation
/// (`WirelessCoex` or `Wireless` + `Gatt`).
///
/// Non-concurrent commissioning means that the device - at any point in time - either runs Bluetooth
/// or Wifi/Thread, but not both.
///
/// This is done to save memory and to avoid the usage of BLE+Wifi/Thread co-exist drivers on
/// devices which share a single wireless radio for both BLE and Wifi/Thread.
pub struct WirelessBle<M, T, E = ()>
where
    M: RawMutex,
    T: WirelessNetwork,
{
    btp_context: BtpContext<M>,
    networks: WirelessNetworks<MAX_WIRELESS_NETWORKS, M, T>,
    net_state: blocking::Mutex<M, RefCell<NetCtlState>>,
    creds_buf: IfMutex<M, [u8; MAX_CREDS_SIZE]>,
    embedding: E,
}

impl<M, T, E> WirelessBle<M, T, E>
where
    M: RawMutex,
    T: WirelessNetwork,
    E: Embedding,
{
    /// Creates a new instance of the `WirelessBle` network type.
    pub const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            networks: WirelessNetworks::new(),
            net_state: NetCtlState::new_with_mutex(),
            creds_buf: IfMutex::new([0; MAX_CREDS_SIZE]),
            embedding: E::INIT,
        }
    }

    /// Return an in-place initializer for the `WirelessBle` network type.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            btp_context <- BtpContext::init(),
            networks <- WirelessNetworks::init(),
            net_state <- NetCtlState::init_with_mutex(),
            creds_buf <- IfMutex::init(zeroed()),
            embedding <- E::init(),
        })
    }

    /// Return a reference to the networks storage.
    pub fn networks(&self) -> &WirelessNetworks<MAX_WIRELESS_NETWORKS, M, T> {
        &self.networks
    }
}

impl<M, T, E> Default for WirelessBle<M, T, E>
where
    M: RawMutex,
    T: WirelessNetwork,
    E: Embedding,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<M, T, E> Sealed for WirelessBle<M, T, E>
where
    M: RawMutex,
    T: WirelessNetwork,
    E: Embedding,
{
}

impl<M, T, E> Network for WirelessBle<M, T, E>
where
    M: RawMutex + 'static,
    T: WirelessNetwork,
    E: Embedding + 'static,
{
    const INIT: Self = Self::new();

    type PersistContext<'a> = &'a WirelessNetworks<MAX_WIRELESS_NETWORKS, M, T>;

    type Embedding = E;

    fn persist_context(&self) -> Self::PersistContext<'_> {
        &self.networks
    }

    fn embedding(&self) -> &Self::Embedding {
        &self.embedding
    }

    fn init() -> impl Init<Self> {
        WirelessBle::init()
    }
}

impl<M, T, E> MatterStack<'_, WirelessBle<M, T, E>>
where
    M: RawMutex + Send + Sync + 'static,
    T: WirelessNetwork,
    E: Embedding + 'static,
{
    /// Reset the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs
        // TODO self.network.btp_gatt_context.reset()?;
        // TODO self.network.btp_context.reset();
        self.network.networks.reset();

        Ok(())
    }

    async fn run_net_coex<U, N, C, G>(
        &'static self,
        udp: U,
        netif: N,
        net_ctl: C,
        mut gatt: G,
    ) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WirelessDiag + NetChangeNotif,
        G: GattPeripheral,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                self.matter()
                    .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                    .await?; // TODO

                // // Return the requested network with priority
                // if let Some(network_id) = self.connect_requested.take() {
                //     let network = self
                //         .networks
                //         .iter()
                //         .find(|network| network.id() == network_id);

                //     if let Some(network) = network {
                //         info!(
                //             "Trying with requested network first - ID: {}",
                //             network.display()
                //         );

                //         f(network)?;
                //         return Ok(true);
                //     }
                // }

                let mut buf = self.network.creds_buf.lock().await;

                let mut mgr = WirelessMgr::new(&self.network.networks, &net_ctl, &mut buf);

                let mut net_task = pin!(self.run_btp_coex(&udp, &netif, &mut gatt));
                let mut mgr_task = pin!(mgr.run());

                select(&mut net_task, &mut mgr_task).coalesce().await?;
            } else {
                info!("Running in commissioned mode (wireless only)");

                let mut buf = self.network.creds_buf.lock().await;

                let mut mgr = WirelessMgr::new(&self.network.networks, &net_ctl, &mut buf);

                self.matter().disable_commissioning()?;

                let mut net_task = pin!(self.run_oper_net(
                    &udp,
                    &netif,
                    core::future::pending(),
                    Option::<(NoNetwork, NoNetwork)>::None
                ));
                let mut mgr_task = pin!(mgr.run());

                select(&mut net_task, &mut mgr_task).coalesce().await?;
            }
        }
    }

    async fn run_btp_coex<U, N, P>(
        &'static self,
        mut udp: U,
        netif: N,
        peripheral: P,
    ) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        P: GattPeripheral,
    {
        info!("Running in concurrent commissioning mode (BLE and Wireless)");

        let btp = Btp::new(peripheral, &self.network.btp_context);

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        // TODO: Run till commissioning is complete
        let mut net_task = pin!(self.run_oper_net(
            &mut udp,
            &netif,
            core::future::pending(),
            Some((&btp, &btp))
        ));

        select(&mut btp_task, &mut net_task).coalesce().await
    }

    async fn run_btp<P>(&'static self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral,
    {
        let btp = Btp::new(peripheral, &self.network.btp_context);

        info!("BLE driver started");

        info!("Running in non-concurrent commissioning mode (BLE only)");

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        let mut net_task = pin!(self.run_transport_net(&btp, &btp));
        let mut oper_net_act_task = pin!(async {
            NetCtlState::wait_prov_ready(&self.network.net_state, &btp).await;

            // TODO: Workaround for a bug in the `esp-wifi` BLE stack:
            // ====================== PANIC ======================
            // panicked at /home/ivan/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/esp-wifi-0.12.0/src/ble/npl.rs:914:9:
            // timed eventq_get not yet supported - go implement it!
            embassy_time::Timer::after(embassy_time::Duration::from_secs(2)).await;

            Ok(())
        });

        select3(&mut btp_task, &mut net_task, &mut oper_net_act_task)
            .coalesce()
            .await
    }
}

/// A utility type for running a wireless task with a pre-existing wireless interface
/// rather than bringing up / tearing down the wireless interface for the task.
///
/// This utility can only be used with hardware that implements wireless coexist mode
/// (i.e. the Thread/Wifi interface as well as the BLE GATT peripheral are available at the same time).
pub struct PreexistingWireless<U, N, C, G> {
    pub(crate) udp: U,
    pub(crate) netif: N,
    pub(crate) net_ctl: C,
    pub(crate) gatt: G,
}

impl<U, N, C, G> PreexistingWireless<U, N, C, G> {
    /// Create a new `PreexistingWireless` instance with the given UDP stack,
    /// network interface, network controller and GATT peripheral.
    pub const fn new(udp: U, netif: N, net_ctl: C, gatt: G) -> Self {
        Self {
            udp,
            netif,
            net_ctl,
            gatt,
        }
    }
}

pub(crate) struct MatterStackWirelessTask<'a, M, T, E, H, U>(
    &'a MatterStack<'a, WirelessBle<M, T, E>>,
    H,
    U,
)
where
    M: RawMutex + Send + Sync + 'static,
    T: WirelessNetwork,
    E: Embedding + 'static;
