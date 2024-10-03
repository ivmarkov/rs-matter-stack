//! Implementation of `WirelessController` and `WirelessStats` over types implementing `embedded_svc::wifi::asynch::Wifi`

use embedded_svc::wifi::{asynch::Wifi, AccessPointInfo};
use rs_matter::error::Error;

use super::traits::{Controller, WifiCredentials, WifiSsid, WirelessDTOs};

pub struct SvcWifi<W> {
    wifi: W,
}

impl<T> WirelessDTOs for SvcWifi<T>
where
    T: Wifi,
{
    type NetworkCredentials = WifiCredentials;

    type ScanResult = AccessPointInfo;

    type Stats = ();

    fn supports_concurrent_connection(&self) -> bool {
        true // By default
    }
}

impl<T> Controller for SvcWifi<T>
where
    T: Wifi,
{
    async fn scan<F>(&mut self, network_id: Option<&WifiSsid>, mut callback: F) -> Result<(), Error>
    where
        F: FnMut(Option<&Self::ScanResult>),
    {
        let (result, _) = self.wifi.scan_n::<5>().await.unwrap();

        for r in &result {
            if network_id.map(|id| r.ssid == id.0).unwrap_or(true) {
                callback(Some(r));
            }
        }

        callback(None);

        Ok(())
    }

    async fn connect(&mut self, _creds: &Self::NetworkCredentials) -> Result<(), Error> {
        todo!()
    }

    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<Self::NetworkCredentials as super::traits::NetworkCredentials>::NetworkId>,
        Error,
    > {
        todo!()
    }

    async fn stats(&mut self) -> Result<Self::Stats, Error> {
        todo!()
    }
}
