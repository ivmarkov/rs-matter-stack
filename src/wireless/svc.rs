//! Implementation of `WirelessController` and `WirelessStats` over types implementing `embedded_svc::wifi::asynch::Wifi`

use embedded_svc::wifi::{asynch::Wifi, AuthMethod, ClientConfiguration, Configuration};

use rs_matter::{
    data_model::sdm::nw_commissioning::WiFiSecurity, error::Error, tlv::OctetsOwned,
    utils::storage::Vec,
};

use super::traits::{
    Controller, NetworkCredentials, WifiData, WifiScanResult, WifiSsid, WirelessData,
};

/// A wireless controller for the `embedded_svc::wifi::asynch::Wifi` type.
pub struct SvcWifi<W> {
    wifi: W,
}

impl<W> SvcWifi<W> {
    /// Create a new `SvcWifi` instance.
    pub fn new(wifi: W) -> Self {
        Self { wifi }
    }
}

impl<T> Controller for SvcWifi<T>
where
    T: Wifi,
{
    type Data = WifiData;

    async fn scan<F>(&mut self, network_id: Option<&WifiSsid>, mut callback: F) -> Result<(), Error>
    where
        F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>),
    {
        self.wifi.start().await.unwrap();

        let (result, _) = self.wifi.scan_n::<5>().await.unwrap();

        for r in &result {
            if network_id.map(|id| r.ssid == id.0).unwrap_or(true) {
                fn to_sec(value: Option<AuthMethod>) -> WiFiSecurity {
                    let Some(value) = value else {
                        // Best guess
                        return WiFiSecurity::Wpa2Personal;
                    };

                    match value {
                        AuthMethod::None => WiFiSecurity::Unencrypted,
                        AuthMethod::WEP => WiFiSecurity::Wep,
                        AuthMethod::WPA => WiFiSecurity::WpaPersonal,
                        AuthMethod::WPA2Personal
                        | AuthMethod::WPAWPA2Personal
                        | AuthMethod::WPA2Enterprise => WiFiSecurity::Wpa2Personal,
                        _ => WiFiSecurity::Wpa3Personal,
                    }
                }

                callback(Some(&WifiScanResult {
                    security: to_sec(r.auth_method),
                    ssid: WifiSsid(r.ssid.clone()),
                    bssid: OctetsOwned {
                        vec: Vec::from_slice(&r.bssid).unwrap(),
                    },
                    channel: r.channel as _,
                    band: None,
                    rssi: Some(r.signal_strength),
                }));
            }
        }

        callback(None);

        Ok(())
    }

    async fn connect(
        &mut self,
        creds: &<Self::Data as WirelessData>::NetworkCredentials,
    ) -> Result<(), Error> {
        for auth_method in [
            AuthMethod::WPA2WPA3Personal,
            AuthMethod::WPAWPA2Personal,
            AuthMethod::WEP,
            AuthMethod::None,
        ] {
            let _ = self.wifi.stop().await;

            self.wifi
                .set_configuration(&Configuration::Client(ClientConfiguration {
                    ssid: creds.ssid.0.clone(),
                    auth_method,
                    password: creds.password.clone(),
                    ..Default::default()
                }))
                .await
                .unwrap();

            self.wifi.start().await.unwrap();

            self.wifi.connect().await.unwrap();

            // TODO: Wait for connection to be established
        }

        Ok(())
    }

    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId>,
        Error,
    > {
        todo!()
    }

    async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
        todo!()
    }
}
