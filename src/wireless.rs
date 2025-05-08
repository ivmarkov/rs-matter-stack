use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::{select, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;

use rs_matter::data_model::networks::wireless::{
    ConnectNetCtl, WirelessMgr, WirelessNetwork, WirelessNetworks,
};
use rs_matter::data_model::networks::NetChangeNotif;
use rs_matter::data_model::sdm::gen_diag::NetifDiag;
use rs_matter::data_model::sdm::net_comm::NetCtl;
use rs_matter::data_model::sdm::wifi_diag::WirelessDiag;
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::{Btp, BtpContext, GattPeripheral};
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;

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
    embedding: E,
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
            embedding: E::INIT,
        }
    }

    /// Return an in-place initializer for the `WirelessBle` network type.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            btp_context <- BtpContext::init(),
            networks <- WirelessNetworks::init(),
            embedding <- E::init(),
        })
    }

    /// Return a reference to the BTP context.
    pub fn network_context(&self) -> &WirelessNetworks<MAX_WIRELESS_NETWORKS, M, T> {
        &self.networks
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
        gatt: G,
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

                let btp = Btp::new(&gatt, &self.network.btp_context);

                info!("Running in concurrent commissioning mode");

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

                let mut mgr = WirelessMgr::new(&self.network.networks, &net_ctl, &mut []); // TODO

                let mut net_task = pin!(self.run_btp_coex(&udp, &netif, &btp));
                let mut mgr_task = pin!(mgr.run());

                select(&mut net_task, &mut mgr_task).coalesce().await?;
            } else {
                info!("Running in commissioned mode (wireless only)");

                let mut mgr = WirelessMgr::new(&self.network.networks, &net_ctl, &mut []); // TODO

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

    async fn run_btp_coex<U, N, B>(
        &self,
        mut udp: U,
        netif: N,
        btp: &Btp<&'static BtpContext<M>, M, B>,
    ) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
        B: GattPeripheral,
    {
        info!("Running in concurrent commissioning mode (BLE and Wireless)");

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        // TODO: Run till commissioning is complete
        let mut net_task =
            pin!(self.run_oper_net(&mut udp, &netif, core::future::pending(), Some((btp, btp))));

        select(&mut btp_task, &mut net_task).coalesce().await
    }

    async fn run_btp<B, M2, N>(
        &'static self,
        btp: &Btp<&'static BtpContext<M>, M, B>,
        net_ctl: &ConnectNetCtl<M2, N>,
    ) -> Result<(), Error>
    where
        B: GattPeripheral,
        M2: RawMutex,
        N: NetCtl,
    {
        info!("Running in non-concurrent commissioning mode (BLE only)");

        let mut btp_task = pin!(btp.run(
            "BT",
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        ));

        let mut net_task = pin!(self.run_transport_net(btp, btp));
        let mut oper_net_act_task = pin!(async {
            net_ctl.wait_prov_ready(btp).await;
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
