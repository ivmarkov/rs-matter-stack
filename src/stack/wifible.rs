use core::borrow::Borrow;
use core::cell::RefCell;
use core::pin::pin;

use edge_nal::{Multicast, Readable, UdpBind, UdpSplit};

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;

use embedded_svc::wifi::asynch::Wifi;

use log::info;

use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata, Endpoint, HandlerCompat};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::failsafe::FailSafe;
use rs_matter::data_model::sdm::wifi_nw_diagnostics;
use rs_matter::data_model::sdm::wifi_nw_diagnostics::{
    WiFiSecurity, WiFiVersion, WifiNwDiagCluster, WifiNwDiagData,
};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::{Btp, BtpContext, GattPeripheral};
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use crate::error::Error;
use crate::netif::Netif;
use crate::persist::{NetworkContext, Persist};
use crate::wifi::mgmt::WifiManager;
use crate::wifi::{comm, WifiContext};
use crate::{MatterStack, Network};

pub const MAX_WIFI_NETWORKS: usize = 2;

pub trait Modem {
    type Gatt<'a>: GattPeripheral
    where
        Self: 'a;
    type Wifi<'a>: Wifi
    where
        Self: 'a;
    type Netif<'a>: Netif + UdpBind
    where
        Self: 'a;

    fn gatt(&mut self) -> Self::Gatt<'_>;
    fn wifi_netif(&mut self) -> (Self::Wifi<'_>, Self::Netif<'_>);
}

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over WiFi when operating.
///
/// The supported commissioning is of the non-concurrent type (as per the Matter Core spec),
/// where the device - at any point in time - either runs Bluetooth or Wifi, but not both.
/// This is done to save memory and to avoid the usage of the ESP IDF Co-exist driver.
///
/// The BLE implementation used is the ESP IDF Bluedroid stack (not NimBLE).
pub struct WifiBle<M: RawMutex> {
    btp_context: BtpContext<M>,
    wifi_context: WifiContext<MAX_WIFI_NETWORKS, M>,
}

impl<M: RawMutex> WifiBle<M> {
    const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            wifi_context: WifiContext::new(),
        }
    }
}

impl<M: RawMutex> Network for WifiBle<M> {
    const INIT: Self = Self::new();

    type Mutex = M;

    fn network_context(&self) -> NetworkContext<'_, { MAX_WIFI_NETWORKS }, Self::Mutex> {
        NetworkContext::Wifi(&self.wifi_context)
    }
}

pub type WifiBleMatterStack<'a, M> = MatterStack<'a, WifiBle<M>>;

impl<'a, M> MatterStack<'a, WifiBle<M>>
where
    M: RawMutex + Send + Sync,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub const fn root_metadata() -> Endpoint<'static> {
        root_endpoint::endpoint(0, OperNwType::Wifi)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub fn root_handler(&self) -> WifiBleRootEndpointHandler<'_, M> {
        handler(
            0,
            self.matter(),
            comm::WifiNwCommCluster::new(*self.matter().borrow(), &self.network.wifi_context),
            wifi_nw_diagnostics::ID,
            HandlerCompat(WifiNwDiagCluster::new(
                *self.matter().borrow(),
                // TODO: Update with actual information
                WifiNwDiagData {
                    bssid: [0; 6],
                    security_type: WiFiSecurity::Unspecified,
                    wifi_version: WiFiVersion::B,
                    channel_number: 20,
                    rssi: 0,
                },
            )),
        )
    }

    /// Resets the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs
        // TODO self.network.btp_gatt_context.reset()?;
        // TODO self.network.btp_context.reset();
        self.network.wifi_context.reset();

        Ok(())
    }

    /// Return information whether the Matter instance is already commissioned.
    ///
    /// User might need to reach out to this method only when it needs finer-grained control
    /// and utilizes the `commission` and `operate` methods rather than the all-in-one `run` loop.
    pub async fn is_commissioned(&self) -> Result<bool, Error> {
        // TODO
        Ok(false)
    }

    /// A utility method to run the Matter stack in Operating mode (as per the Matter Core spec) over Wifi.
    ///
    /// This method assumes that the Matter instance is already commissioned and therefore
    /// does not take a `CommissioningData` parameter.
    ///
    /// It is just a composition of the `MatterStack::run_with_netif` method, and the `WifiManager::run` method,
    /// where the former takes care of running the main Matter loop, while the latter takes care of ensuring
    /// that the Matter instance stays connected to the Wifi network.
    pub async fn operate<H, P, W, I>(
        &self,
        persist: P,
        wifi: W,
        netif: I,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        W: Wifi,
        I: Netif + UdpBind,
        for<'s> I::Socket<'s>: UdpSplit,
        for<'s> I::Socket<'s>: Multicast,
        for<'s, 'r> <I::Socket<'s> as UdpSplit>::Receive<'r>: Readable,
    {
        info!("Running Matter in operating mode (Wifi)");

        let mut mgr = WifiManager::new(wifi, &self.network.wifi_context);

        let mut main = pin!(self.run_with_netif(persist, netif, None, handler));
        let mut wifi = pin!(mgr.run());

        select(&mut wifi, &mut main).coalesce().await
    }

    /// A utility method to run the Matter stack in Commissioning mode (as per the Matter Core spec) over BLE.
    /// Essentially an instantiation of `MatterStack::run_with_transport` with the BLE transport.
    ///
    /// Note: make sure to call `MatterStack::reset` before calling this method, as all fabrics and ACLs, as well as all
    /// transport state should be reset.
    pub async fn commission<'d, H, P, G>(
        &'static self,
        persist: P,
        gatt: G,
        dev_comm: CommissioningData,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        G: GattPeripheral,
    {
        info!("Running Matter in commissioning mode (BLE)");

        let btp = Btp::new(gatt, &self.network.btp_context);

        let mut ble = pin!(async {
            btp.run("BT", self.matter().dev_det(), &dev_comm)
                .await
                .map_err(Into::into)
        });

        let mut main = pin!(self.run_with_transport(
            &btp,
            &btp,
            persist,
            Some((
                dev_comm.clone(),
                DiscoveryCapabilities::new(false, true, false)
            )),
            &handler
        ));

        select(&mut ble, &mut main).coalesce().await
    }

    /// An all-in-one "run everything" method that automatically
    /// places the Matter stack either in Commissioning or in Operating mode, depending
    /// on the state of the device as persisted in the NVS storage.
    pub async fn run<'d, P, E, H>(
        &'static self,
        mut persist: P,
        mut modem: E,
        dev_comm: CommissioningData,
        handler: H,
    ) -> Result<(), Error>
    where
        P: Persist,
        E: Modem,
        for<'n, 's> <E::Netif<'n> as UdpBind>::Socket<'s>: UdpSplit,
        for<'n, 's> <E::Netif<'n> as UdpBind>::Socket<'s>: Multicast,
        for<'n, 's, 'r> <<E::Netif<'n> as UdpBind>::Socket<'s> as UdpSplit>::Receive<'r>: Readable,
        H: AsyncHandler + AsyncMetadata,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        loop {
            if !self.is_commissioned().await? {
                // Reset to factory defaults everything, as we'll do commissioning all over
                self.reset()?;

                let gatt = modem.gatt();

                info!("BLE driver initialized");

                let mut main =
                    pin!(self.commission(&mut persist, gatt, dev_comm.clone(), &handler));
                let mut wait_network_connect =
                    pin!(self.network.wifi_context.wait_network_connect());

                select(&mut main, &mut wait_network_connect)
                    .coalesce()
                    .await?;
            }

            // As per spec, we need to indicate the expectation of a re-arm with a CASE session
            // even if the current session is a PASE one (this is specific for non-concurrent commissiioning flows)
            let failsafe: &RefCell<FailSafe> = self.matter().borrow();
            failsafe.borrow_mut().expect_case_rearm()?;

            let (wifi, netif) = modem.wifi_netif();

            info!("Wifi driver initialized");

            self.operate(&mut persist, wifi, netif, &handler).await?;
        }
    }
}

pub type WifiBleRootEndpointHandler<'a, M> = RootEndpointHandler<
    'a,
    comm::WifiNwCommCluster<'a, MAX_WIFI_NETWORKS, M>,
    HandlerCompat<WifiNwDiagCluster>,
>;
