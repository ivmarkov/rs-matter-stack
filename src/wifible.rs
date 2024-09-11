use core::future::Future;
use core::pin::pin;

use edge_nal::UdpBind;

use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::RawMutex;

use embedded_svc::wifi::asynch::Wifi;

use log::info;

use rs_matter::data_model::objects::{
    AsyncHandler, AsyncMetadata, Dataver, Endpoint, HandlerCompat,
};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::root_endpoint::{handler, OperNwType, RootEndpointHandler};
use rs_matter::data_model::sdm::wifi_nw_diagnostics;
use rs_matter::data_model::sdm::wifi_nw_diagnostics::{
    WiFiSecurity, WiFiVersion, WifiNwDiagCluster, WifiNwDiagData,
};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::{Btp, BtpContext, GattPeripheral};
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use crate::modem::{Modem, WifiDevice};
use crate::netif::Netif;
use crate::network::{Embedding, Network};
use crate::persist::Persist;
use crate::wifi::mgmt::WifiManager;
use crate::wifi::{comm, WifiContext};
use crate::MatterStack;

pub const MAX_WIFI_NETWORKS: usize = 2;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over WiFi when operating.
///
/// The supported commissioning is of the non-concurrent type (as per the Matter Core spec),
/// where the device - at any point in time - either runs Bluetooth or Wifi, but not both.
/// This is done to save memory and to avoid the usage of BLE+Wifi co-exist drivers on
/// devices which share a single wireless radio for both BLE and Wifi.
pub struct WifiBle<M: RawMutex, E = ()> {
    btp_context: BtpContext<M>,
    wifi_context: WifiContext<MAX_WIFI_NETWORKS, M>,
    embedding: E,
}

impl<M: RawMutex, E> WifiBle<M, E>
where
    E: Embedding,
{
    const fn new() -> Self {
        Self {
            btp_context: BtpContext::new(),
            wifi_context: WifiContext::new(),
            embedding: E::INIT,
        }
    }

    pub fn wifi_context(&self) -> &WifiContext<MAX_WIFI_NETWORKS, M> {
        &self.wifi_context
    }
}

impl<M: RawMutex, E> Network for WifiBle<M, E>
where
    E: Embedding + 'static,
{
    const INIT: Self = Self::new();

    type Embedding = E;

    fn embedding(&self) -> &Self::Embedding {
        &self.embedding
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            btp_context <- BtpContext::init(),
            wifi_context <- WifiContext::init(),
            embedding <- E::init(),
        })
    }
}

pub type WifiBleMatterStack<'a, M, E> = MatterStack<'a, WifiBle<M, E>>;

impl<'a, M, E> MatterStack<'a, WifiBle<M, E>>
where
    M: RawMutex + Send + Sync,
    E: Embedding + 'static,
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
            HandlerCompat(comm::WifiNwCommCluster::new(
                Dataver::new_rand(self.matter().rand()),
                &self.network.wifi_context,
            )),
            wifi_nw_diagnostics::ID,
            HandlerCompat(WifiNwDiagCluster::new(
                Dataver::new_rand(self.matter().rand()),
                // TODO: Update with actual information
                WifiNwDiagData {
                    bssid: [0; 6],
                    security_type: WiFiSecurity::Unspecified,
                    wifi_version: WiFiVersion::B,
                    channel_number: 20,
                    rssi: 0,
                },
            )),
            false,
            self.matter().rand(),
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
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `wifi` - a user-provided `Wifi` implementation
    /// - `netif` - a user-provided `Netif` implementation
    /// - `dev_comm` - the commissioning data and discovery capabilities
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn operate<H, P, W, I, U>(
        &self,
        persist: P,
        wifi: W,
        netif: I,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        W: Wifi,
        I: Netif + UdpBind,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Running Matter in operating mode (Wifi)");

        let mut user = pin!(user);

        let mut mgr = WifiManager::new(wifi, &self.network.wifi_context);

        let mut main = pin!(self.run_with_netif(persist, netif, None, handler, &mut user));
        let mut wifi = pin!(mgr.run());

        select(&mut wifi, &mut main).coalesce().await
    }

    /// A utility method to run the Matter stack in Commissioning mode (as per the Matter Core spec) over BLE.
    /// Essentially an instantiation of `MatterStack::run_with_transport` with the BLE transport.
    ///
    /// Note: make sure to call `MatterStack::reset` before calling this method, as all fabrics and ACLs, as well as all
    /// transport state should be reset.
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `gatt` - a user-provided `GattPeripheral` implementation
    /// - `dev_comm` - the commissioning data
    /// - `handler` - a user-provided DM handler implementation
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
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `modem` - a user-provided `Modem` implementation
    /// - `dev_comm` - the commissioning data
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<'d, P, O, H, U>(
        &'static self,
        mut persist: P,
        mut modem: O,
        dev_comm: CommissioningData,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        P: Persist,
        O: Modem,
        H: AsyncHandler + AsyncMetadata,
        U: Future<Output = Result<(), Error>>,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let mut user = pin!(user);

        loop {
            if !self.is_commissioned().await? {
                // Reset to factory defaults everything, as we'll do commissioning all over
                self.reset()?;

                let gatt = modem.ble().await;

                info!("BLE driver initialized");

                let mut main =
                    pin!(self.commission(&mut persist, gatt, dev_comm.clone(), &handler));
                let mut wait_network_connect = pin!(async {
                    self.network.wifi_context.wait_network_connect().await;
                    Ok(())
                });

                select(&mut main, &mut wait_network_connect)
                    .coalesce()
                    .await?;
            }

            let mut wifi = modem.wifi().await;

            let (wifi, netif) = wifi.split().await;

            info!("Wifi driver initialized");

            self.operate(&mut persist, wifi, netif, &handler, &mut user)
                .await?;
        }
    }
}

pub type WifiBleRootEndpointHandler<'a, M> = RootEndpointHandler<
    'a,
    HandlerCompat<comm::WifiNwCommCluster<'a, MAX_WIFI_NETWORKS, M>>,
    HandlerCompat<WifiNwDiagCluster>,
>;
