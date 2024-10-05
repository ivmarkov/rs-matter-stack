use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use log::info;

use rs_matter::data_model::objects::*;
use rs_matter::data_model::sdm::thread_nw_diagnostics::{Attributes, Commands, CLUSTER};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{TLVElement, TLVWrite};
use rs_matter::transport::exchange::Exchange;

use crate::wireless::traits::{Controller, ThreadData};

/// A cluster implementing the Matter Thread Diagnostics Cluster.
pub struct ThreadNwDiagCluster<M, T>
where
    M: RawMutex,
{
    data_ver: Dataver,
    controller: Mutex<M, T>,
}

impl<M, T> ThreadNwDiagCluster<M, T>
where
    M: RawMutex,
    T: Controller<Data = ThreadData>,
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

                // TODO: Implement proper statistics
                controller.stats().await?;

                if Attributes::try_from(attr.attr_id).is_ok() {
                    writer.null(&AttrDataWriter::TAG)?;
                } else {
                    Err(ErrorCode::AttributeNotFound)?;
                }

                Ok(())
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

impl<M, T> AsyncHandler for ThreadNwDiagCluster<M, T>
where
    M: RawMutex,
    T: Controller<Data = ThreadData>,
{
    async fn read(
        &self,
        exchange: &Exchange<'_>,
        attr: &AttrDetails<'_>,
        encoder: AttrDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        ThreadNwDiagCluster::read(self, exchange, attr, encoder).await
    }

    async fn write(
        &self,
        exchange: &Exchange<'_>,
        attr: &AttrDetails<'_>,
        data: AttrData<'_>,
    ) -> Result<(), Error> {
        ThreadNwDiagCluster::write(self, exchange, attr, data).await
    }

    async fn invoke(
        &self,
        exchange: &Exchange<'_>,
        cmd: &CmdDetails<'_>,
        data: &TLVElement<'_>,
        encoder: CmdDataEncoder<'_, '_, '_>,
    ) -> Result<(), Error> {
        ThreadNwDiagCluster::invoke(self, exchange, cmd, data, encoder).await
    }
}
