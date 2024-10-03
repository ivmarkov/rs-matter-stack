//! Wireless network commissioning cluster.

use embassy_sync::blocking_mutex::raw::RawMutex;

use log::{info, warn};

use rs_matter::data_model::objects::{
    AttrDataEncoder, AttrDataWriter, AttrDetails, AttrType, CmdDataEncoder, CmdDetails, Dataver,
    Handler, NonBlockingHandler,
};
use rs_matter::data_model::sdm::nw_commissioning::{
    AddThreadNetworkRequest, AddWifiNetworkRequest, Attributes, Commands, ConnectNetworkRequest,
    ConnectNetworkResponse, NetworkCommissioningStatus, NetworkConfigResponse, NwInfo,
    RemoveNetworkRequest, ReorderNetworkRequest, ResponseCommands, ScanNetworksRequest,
    THR_CLUSTER, WIFI_CLUSTER,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, Octets, TLVElement, TLVTag, TLVWrite, ToTLV};
use rs_matter::transport::exchange::Exchange;

use super::store::NetworkContext;
use super::NetworkCredentials;

/// A cluster implementing the Matter Network Commissioning Cluster
/// for managing wireless networks.
///
/// `N` is the maximum number of networks that can be stored.
pub struct WirelessNwCommCluster<'a, const N: usize, M, T>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'b> FromTLV<'b> + ToTLV,
{
    data_ver: Dataver,
    networks: &'a NetworkContext<N, M, T>,
}

impl<'a, const N: usize, M, T> WirelessNwCommCluster<'a, N, M, T>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'b> FromTLV<'b> + ToTLV,
{
    /// Create a new instance.
    pub const fn new(data_ver: Dataver, networks: &'a NetworkContext<N, M, T>) -> Self {
        Self { data_ver, networks }
    }

    /// Read an attribute.
    pub fn read(
        &self,
        _exchange: &Exchange,
        attr: &AttrDetails,
        encoder: AttrDataEncoder,
    ) -> Result<(), Error> {
        if let Some(mut writer) = encoder.with_dataver(self.data_ver.get())? {
            if attr.is_system() {
                if T::is_wifi() {
                    WIFI_CLUSTER.read(attr.attr_id, writer)
                } else {
                    THR_CLUSTER.read(attr.attr_id, writer)
                }
            } else {
                match attr.attr_id.try_into()? {
                    Attributes::MaxNetworks => AttrType::<u8>::new().encode(writer, N as u8),
                    Attributes::Networks => {
                        writer.start_array(&AttrDataWriter::TAG)?;

                        self.networks.state.lock(|state| {
                            let state = state.borrow();

                            for network in &state.networks {
                                let nw_info = NwInfo {
                                    network_id: Octets(network.network_id().as_ref()),
                                    connected: state
                                        .status
                                        .as_ref()
                                        .map(|status| {
                                            &status.network_id == network.network_id()
                                                && matches!(
                                                    status.status,
                                                    NetworkCommissioningStatus::Success
                                                )
                                        })
                                        .unwrap_or(false),
                                };

                                nw_info.to_tlv(&TLVTag::Anonymous, &mut *writer)?;
                            }

                            Ok::<_, Error>(())
                        })?;

                        writer.end_container()?;
                        writer.complete()
                    }
                    Attributes::ScanMaxTimeSecs => AttrType::new().encode(writer, 30_u8),
                    Attributes::ConnectMaxTimeSecs => AttrType::new().encode(writer, 60_u8),
                    Attributes::InterfaceEnabled => AttrType::new().encode(writer, true),
                    Attributes::LastNetworkingStatus => self.networks.state.lock(|state| {
                        AttrType::new().encode(
                            writer,
                            state.borrow().status.as_ref().map(|o| o.status as u8),
                        )
                    }),
                    Attributes::LastNetworkID => self.networks.state.lock(|state| {
                        AttrType::new().encode(
                            writer,
                            state
                                .borrow()
                                .status
                                .as_ref()
                                .map(|o| Octets(o.network_id.as_ref())),
                        )
                    }),
                    Attributes::LastConnectErrorValue => self.networks.state.lock(|state| {
                        AttrType::new()
                            .encode(writer, state.borrow().status.as_ref().map(|o| o.value))
                    }),
                }
            }
        } else {
            Ok(())
        }
    }

    /// Invoke a command.
    pub fn invoke(
        &self,
        exchange: &Exchange,
        cmd: &CmdDetails,
        data: &TLVElement,
        encoder: CmdDataEncoder,
    ) -> Result<(), Error> {
        match cmd.cmd_id.try_into()? {
            Commands::ScanNetworks => {
                info!("ScanNetworks");
                self.scan_networks(exchange, &ScanNetworksRequest::from_tlv(data)?, encoder)?;
            }
            Commands::AddOrUpdateWifiNetwork => {
                info!("AddOrUpdateWifiNetwork");
                self.add_wifi_network(exchange, &AddWifiNetworkRequest::from_tlv(data)?, encoder)?;
            }
            Commands::AddOrUpdateThreadNetwork => {
                info!("AddOrUpdateThreadNetwork");
                self.add_thread_network(
                    exchange,
                    &AddThreadNetworkRequest::from_tlv(data)?,
                    encoder,
                )?;
            }
            Commands::RemoveNetwork => {
                info!("RemoveNetwork");
                self.remove_network(exchange, &RemoveNetworkRequest::from_tlv(data)?, encoder)?;
            }
            Commands::ConnectNetwork => {
                info!("ConnectNetwork");
                self.connect_network(exchange, &ConnectNetworkRequest::from_tlv(data)?, encoder)?;
            }
            Commands::ReorderNetwork => {
                info!("ReorderNetwork");
                self.reorder_network(exchange, &ReorderNetworkRequest::from_tlv(data)?, encoder)?;
            }
        }

        self.data_ver.changed();

        Ok(())
    }

    fn scan_networks(
        &self,
        _exchange: &Exchange<'_>,
        req: &ScanNetworksRequest<'_>,
        _encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        info!("ScanNetworks req: {:?}", req);
        warn!("Scan network not supported");

        // Unfortunately Alexa calls `ScanNetworks` even if we have explicitly communicated
        // that we do not support concurrent commissioning
        //
        // Cheat and declare that the SSID it is asking for is found

        // let mut writer = encoder.with_command(ResponseCommands::ScanNetworksResponse as _)?;

        // writer.start_struct(&CmdDataWriter::TAG)?;

        // NetworkCommissioningStatus::Success.to_tlv(&TLVTag::Context(ScanNetworksResponseTag::Status as _), &mut *writer)?;

        // writer.utf8(&TLVTag::Context(ScanNetworksResponseTag::DebugText as _), "")?;

        // writer.start_array(&TLVTag::Context(ScanNetworksResponseTag::WifiScanResults as _))?;

        // WiFiInterfaceScanResult {
        //     security: WiFiSecurity::Wpa2Personal,
        //     ssid: Octets(b"test\0"),
        //     bssid: Octets(&[0xF4, 0x6A, 0xDD, 0xF4, 0xF2, 0xB5]),
        //     channel: 20,
        //     band: None,
        //     rssi: None,
        // }
        // .to_tlv(&TLVTag::Anonymous, &mut *writer)?;

        // writer.end_container()?;
        // writer.end_container()?;

        // writer.complete()?;

        // Ok(())

        Err(ErrorCode::Busy)?
    }

    fn add_wifi_network(
        &self,
        exchange: &Exchange<'_>,
        req: &AddWifiNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.add_network(exchange, T::try_from(req)?, encoder)
    }

    fn add_thread_network(
        &self,
        exchange: &Exchange<'_>,
        req: &AddThreadNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.add_network(exchange, T::try_from(req)?, encoder)
    }

    fn add_network(
        &self,
        _exchange: &Exchange<'_>,
        network: T,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|nw| nw.network_id() == network.network_id());

            let writer = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Update
                state.networks[index] = network;

                state.changed = true;
                self.networks.state_changed.notify();

                info!(
                    "Updated network with ID {}",
                    state.networks[index].network_id()
                );

                writer.set(NetworkConfigResponse {
                    status: NetworkCommissioningStatus::Success,
                    debug_text: None,
                    network_index: Some(index as _),
                })?;
            } else {
                // Add
                match state.networks.push(network) {
                    Ok(_) => {
                        state.changed = true;
                        self.networks.state_changed.notify();

                        info!(
                            "Added network with ID {}",
                            state.networks.last().unwrap().network_id()
                        );

                        writer.set(NetworkConfigResponse {
                            status: NetworkCommissioningStatus::Success,
                            debug_text: None,
                            network_index: Some((state.networks.len() - 1) as _),
                        })?;
                    }
                    Err(network) => {
                        warn!(
                            "Adding network with ID {} failed: too many",
                            network.network_id()
                        );

                        writer.set(NetworkConfigResponse {
                            status: NetworkCommissioningStatus::BoundsExceeded,
                            debug_text: None,
                            network_index: None,
                        })?;
                    }
                }
            }

            Ok(())
        })
    }

    fn remove_network(
        &self,
        _exchange: &Exchange<'_>,
        req: &RemoveNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|conf| conf.network_id().as_ref() == req.network_id.0);

            let writer = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Found
                let network = state.networks.remove(index);
                state.changed = true;
                self.networks.state_changed.notify();

                info!("Removed network with ID {}", network.network_id());

                writer.set(NetworkConfigResponse {
                    status: NetworkCommissioningStatus::Success,
                    debug_text: None,
                    network_index: Some(index as _),
                })?;
            } else {
                warn!(
                    "Network with ID {} not found",
                    core::str::from_utf8(req.network_id.0).unwrap()
                );

                // Not found
                writer.set(NetworkConfigResponse {
                    status: NetworkCommissioningStatus::NetworkIdNotFound,
                    debug_text: None,
                    network_index: None,
                })?;
            }

            Ok(())
        })
    }

    fn connect_network(
        &self,
        _exchange: &Exchange<'_>,
        req: &ConnectNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        // Non-concurrent commissioning scenario
        // (i.e. only BLE is active, and the device BLE+Wifi/Thread co-exist
        // driver is not running, or does not even exist)

        let network_id: T::NetworkId = req.network_id.0.try_into()?;

        info!(
            "Request to connect to network with ID {} received",
            core::str::from_utf8(req.network_id.0).unwrap(),
        );

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            state.connect_requested = Some(network_id.clone());
            state.changed = true;
            self.networks.state_changed.notify();
        });

        let writer = encoder.with_command(ResponseCommands::ConnectNetworkResponse as _)?;

        // As per spec, return success even though though whether we'll be able to connect to the network
        // will become apparent later, once we switch to Wifi/Thread
        writer.set(ConnectNetworkResponse {
            status: NetworkCommissioningStatus::Success,
            debug_text: None,
            error_value: 0,
        })?;

        // Notify that we have received a connect command
        self.networks.network_connect_requested.notify();

        Ok(())
    }

    fn reorder_network(
        &self,
        _exchange: &Exchange<'_>,
        req: &ReorderNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|conf| conf.network_id().as_ref() == req.network_id.0);

            let writer = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Found

                if req.index < state.networks.len() as u8 {
                    let conf = state.networks.remove(index);
                    state
                        .networks
                        .insert(req.index as usize, conf)
                        .map_err(|_| ())
                        .unwrap();

                    state.changed = true;
                    self.networks.state_changed.notify();

                    info!(
                        "Network with ID {} reordered to index {}",
                        core::str::from_utf8(req.network_id.0).unwrap(),
                        req.index
                    );

                    writer.set(NetworkConfigResponse {
                        status: NetworkCommissioningStatus::Success,
                        debug_text: None,
                        network_index: Some(req.index as _),
                    })?;
                } else {
                    warn!(
                        "Reordering network with ID {} to index {} failed: out of range",
                        core::str::from_utf8(req.network_id.0).unwrap(),
                        req.index
                    );

                    writer.set(NetworkConfigResponse {
                        status: NetworkCommissioningStatus::OutOfRange,
                        debug_text: None,
                        network_index: Some(req.index as _),
                    })?;
                }
            } else {
                warn!(
                    "Network with ID {} not found",
                    core::str::from_utf8(req.network_id.0).unwrap()
                );

                // Not found
                writer.set(NetworkConfigResponse {
                    status: NetworkCommissioningStatus::NetworkIdNotFound,
                    debug_text: None,
                    network_index: None,
                })?;
            }

            Ok(())
        })
    }
}

impl<'a, const N: usize, M, T> Handler for WirelessNwCommCluster<'a, N, M, T>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'b> FromTLV<'b> + ToTLV,
{
    fn read(
        &self,
        exchange: &Exchange,
        attr: &AttrDetails,
        encoder: AttrDataEncoder,
    ) -> Result<(), Error> {
        WirelessNwCommCluster::read(self, exchange, attr, encoder)
    }

    fn invoke(
        &self,
        exchange: &Exchange<'_>,
        cmd: &CmdDetails,
        data: &TLVElement,
        encoder: CmdDataEncoder,
    ) -> Result<(), Error> {
        WirelessNwCommCluster::invoke(self, exchange, cmd, data, encoder)
    }
}

impl<'a, const N: usize, M, T> NonBlockingHandler for WirelessNwCommCluster<'a, N, M, T>
where
    M: RawMutex,
    T: NetworkCredentials + Clone + for<'b> FromTLV<'b> + ToTLV,
{
}

// impl ChangeNotifier<()> for WirelessCommCluster {
//     fn consume_change(&mut self) -> Option<()> {
//         self.data_ver.consume_change(())
//     }
// }