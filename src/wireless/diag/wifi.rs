use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;

use rs_matter::data_model::objects::*;
use rs_matter::data_model::sdm::wifi_nw_diagnostics::{Attributes, Commands, CLUSTER};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{TLVElement, TLVTag, TLVWrite};
use rs_matter::transport::exchange::Exchange;

use crate::wireless::traits::{Controller, WifiData};

/// A cluster implementing the Matter Wifi Diagnostics Cluster.
pub struct WifiNwDiagCluster<M, T>
where
    M: RawMutex,
{
    data_ver: Dataver,
    controller: Mutex<M, T>,
}

impl<M, T> WifiNwDiagCluster<M, T>
where
    M: RawMutex,
    T: Controller<Data = WifiData>,
{
    /// Create a new instance.
    pub const fn new(data_ver: Dataver, controller: T) -> Self {
        Self {
            data_ver,
            controller: Mutex::new(controller),
        }
    }

    /// Read the value of an attribute.
    pub async fn read(
        &self,
        _exchange: &Exchange<'_>,
        attr: &AttrDetails<'_>,
        encoder: AttrDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        if let Some(mut writer) = encoder.with_dataver(self.data_ver.get())? {
            if attr.is_system() {
                CLUSTER.read(attr.attr_id, writer)
            } else {
                let mut controller = self.controller.lock().await;

                let data = controller.stats().await?;

                if let Some(data) = data {
                    match attr.attr_id.try_into()? {
                        Attributes::Bssid => writer.str(&TLVTag::Anonymous, &data.bssid),
                        Attributes::SecurityType(codec) => codec.encode(writer, data.security_type),
                        Attributes::WifiVersion(codec) => codec.encode(writer, data.wifi_version),
                        Attributes::ChannelNumber(codec) => {
                            codec.encode(writer, data.channel_number)
                        }
                        Attributes::Rssi(codec) => codec.encode(writer, data.rssi),
                        _ => Err(ErrorCode::AttributeNotFound.into()),
                    }
                } else {
                    match attr.attr_id.try_into()? {
                        Attributes::Bssid
                        | Attributes::SecurityType(_)
                        | Attributes::WifiVersion(_)
                        | Attributes::ChannelNumber(_)
                        | Attributes::Rssi(_) => writer.null(&TLVTag::Anonymous),
                        _ => Err(ErrorCode::AttributeNotFound.into()),
                    }
                }
            }
        } else {
            Ok(())
        }
    }

    /// Write the value of an attribute.
    pub async fn write(
        &self,
        _exchange: &Exchange<'_>,
        _attr: &AttrDetails<'_>,
        data: AttrData<'_>,
    ) -> Result<(), Error> {
        let _data = data.with_dataver(self.data_ver.get())?;

        self.data_ver.changed();

        Ok(())
    }

    /// Invoke a command.
    pub async fn invoke(
        &self,
        _exchange: &Exchange<'_>,
        cmd: &CmdDetails<'_>,
        _data: &TLVElement<'_>,
        _encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        match cmd.cmd_id.try_into()? {
            Commands::ResetCounts => {
                info!("ResetCounts: Not yet supported");
            }
        }

        self.data_ver.changed();

        Ok(())
    }
}

impl<M, T> AsyncHandler for WifiNwDiagCluster<M, T>
where
    M: RawMutex,
    T: Controller<Data = WifiData>,
{
    async fn read(
        &self,
        exchange: &Exchange<'_>,
        attr: &AttrDetails<'_>,
        encoder: AttrDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        WifiNwDiagCluster::read(self, exchange, attr, encoder).await
    }

    async fn write(
        &self,
        exchange: &Exchange<'_>,
        attr: &AttrDetails<'_>,
        data: AttrData<'_>,
    ) -> Result<(), Error> {
        WifiNwDiagCluster::write(self, exchange, attr, data).await
    }

    async fn invoke(
        &self,
        exchange: &Exchange<'_>,
        cmd: &CmdDetails<'_>,
        data: &TLVElement<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        WifiNwDiagCluster::invoke(self, exchange, cmd, data, encoder).await
    }
}
