use embassy_sync::blocking_mutex::raw::RawMutex;

use log::{error, info, warn};

use rs_matter::data_model::objects::{
    AttrDataEncoder, AttrDataWriter, AttrDetails, AttrType, CmdDataEncoder, CmdDetails, Dataver,
    Handler, NonBlockingHandler,
};
use rs_matter::data_model::sdm::nw_commissioning::{
    AddWifiNetworkRequest, Attributes, Commands, ConnectNetworkRequest, ConnectNetworkResponse,
    NetworkCommissioningStatus, NetworkConfigResponse, NwInfo, RemoveNetworkRequest,
    ReorderNetworkRequest, ResponseCommands, ScanNetworksRequest, WIFI_CLUSTER,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{FromTLV, Octets, TLVElement, TLVTag, TLVWrite, ToTLV};
use rs_matter::transport::exchange::Exchange;

use super::{WifiContext, WifiCredentials};

/// A cluster implementing the Matter Network Commissioning Cluster
/// for managing WiFi networks.
///
/// `N` is the maximum number of networks that can be stored.
pub struct WifiNwCommCluster<'a, const N: usize, M>
where
    M: RawMutex,
{
    data_ver: Dataver,
    networks: &'a WifiContext<N, M>,
}

impl<'a, const N: usize, M> WifiNwCommCluster<'a, N, M>
where
    M: RawMutex,
{
    /// Create a new instance.
    pub const fn new(data_ver: Dataver, networks: &'a WifiContext<N, M>) -> Self {
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
                WIFI_CLUSTER.read(attr.attr_id, writer)
            } else {
                match attr.attr_id.try_into()? {
                    Attributes::MaxNetworks => AttrType::<u8>::new().encode(writer, N as u8),
                    Attributes::Networks => {
                        writer.start_array(&AttrDataWriter::TAG)?;

                        self.networks.state.lock(|state| {
                            let state = state.borrow();

                            for network in &state.networks {
                                let nw_info = NwInfo {
                                    network_id: Octets(network.ssid.as_str().as_bytes()),
                                    connected: state
                                        .status
                                        .as_ref()
                                        .map(|status| {
                                            *status.ssid == network.ssid
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
                                .map(|o| Octets(o.ssid.as_str().as_bytes())),
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
                self.add_network(exchange, &AddWifiNetworkRequest::from_tlv(data)?, encoder)?;
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
            other => {
                error!("{other:?} (not supported)");
                Err(ErrorCode::CommandNotFound)?
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

    fn add_network(
        &self,
        _exchange: &Exchange<'_>,
        req: &AddWifiNetworkRequest<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        // TODO: Check failsafe status

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            let index = state
                .networks
                .iter()
                .position(|conf| conf.ssid.as_str().as_bytes() == req.ssid.0);

            let writer = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Update
                state.networks[index].ssid = core::str::from_utf8(req.ssid.0)
                    .unwrap()
                    .try_into()
                    .unwrap();
                state.networks[index].password = core::str::from_utf8(req.credentials.0)
                    .unwrap()
                    .try_into()
                    .unwrap();

                state.changed = true;
                self.networks.state_changed.notify();

                info!("Updated network with SSID {}", state.networks[index].ssid);

                writer.set(NetworkConfigResponse {
                    status: NetworkCommissioningStatus::Success,
                    debug_text: None,
                    network_index: Some(index as _),
                })?;
            } else {
                // Add
                let network = WifiCredentials {
                    // TODO
                    ssid: core::str::from_utf8(req.ssid.0)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                    password: core::str::from_utf8(req.credentials.0)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                };

                match state.networks.push(network) {
                    Ok(_) => {
                        state.changed = true;
                        self.networks.state_changed.notify();

                        info!(
                            "Added network with SSID {}",
                            state.networks.last().unwrap().ssid
                        );

                        writer.set(NetworkConfigResponse {
                            status: NetworkCommissioningStatus::Success,
                            debug_text: None,
                            network_index: Some((state.networks.len() - 1) as _),
                        })?;
                    }
                    Err(network) => {
                        warn!("Adding network with SSID {} failed: too many", network.ssid);

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
                .position(|conf| conf.ssid.as_str().as_bytes() == req.network_id.0);

            let writer = encoder.with_command(ResponseCommands::NetworkConfigResponse as _)?;

            if let Some(index) = index {
                // Found
                let network = state.networks.remove(index);
                state.changed = true;
                self.networks.state_changed.notify();

                info!("Removed network with SSID {}", network.ssid);

                writer.set(NetworkConfigResponse {
                    status: NetworkCommissioningStatus::Success,
                    debug_text: None,
                    network_index: Some(index as _),
                })?;
            } else {
                warn!(
                    "Network with SSID {} not found",
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
        // (i.e. only BLE is active, and the device BLE+Wifi co-exist
        // driver is not running, or does not even exist)

        let ssid = core::str::from_utf8(req.network_id.0).unwrap();

        info!(
            "Request to connect to network with SSID {} received",
            core::str::from_utf8(req.network_id.0).unwrap(),
        );

        self.networks.state.lock(|state| {
            let mut state = state.borrow_mut();

            state.connect_requested = Some(ssid.try_into().unwrap());
            state.changed = true;
            self.networks.state_changed.notify();
        });

        let writer = encoder.with_command(ResponseCommands::ConnectNetworkResponse as _)?;

        // As per spec, return success even though though whether we'll be able to connect to the network
        // will become apparent later, once we switch to Wifi
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
                .position(|conf| conf.ssid.as_str().as_bytes() == req.network_id.0);

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
                        "Network with SSID {} reordered to index {}",
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
                        "Reordering network with SSID {} to index {} failed: out of range",
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
                    "Network with SSID {} not found",
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

impl<'a, const N: usize, M> Handler for WifiNwCommCluster<'a, N, M>
where
    M: RawMutex,
{
    fn read(
        &self,
        exchange: &Exchange,
        attr: &AttrDetails,
        encoder: AttrDataEncoder,
    ) -> Result<(), Error> {
        WifiNwCommCluster::read(self, exchange, attr, encoder)
    }

    fn invoke(
        &self,
        exchange: &Exchange<'_>,
        cmd: &CmdDetails,
        data: &TLVElement,
        encoder: CmdDataEncoder,
    ) -> Result<(), Error> {
        WifiNwCommCluster::invoke(self, exchange, cmd, data, encoder)
    }
}

impl<'a, const N: usize, M> NonBlockingHandler for WifiNwCommCluster<'a, N, M> where M: RawMutex {}

// impl ChangeNotifier<()> for WifiCommCluster {
//     fn consume_change(&mut self) -> Option<()> {
//         self.data_ver.consume_change(())
//     }
// }
