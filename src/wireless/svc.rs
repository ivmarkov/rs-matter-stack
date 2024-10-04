//! Implementation of `Controller` over types implementing `embedded_svc::wifi::asynch::Wifi`

use embedded_svc::wifi::{asynch::Wifi, AuthMethod, ClientConfiguration, Configuration};

use rs_matter::data_model::sdm::nw_commissioning::WiFiSecurity;
use rs_matter::data_model::sdm::wifi_nw_diagnostics::{self, WiFiVersion, WifiNwDiagData};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::OctetsOwned;
use rs_matter::utils::storage::Vec;

use super::traits::{
    Controller, NetworkCredentials, WifiData, WifiScanResult, WifiSsid, WirelessData,
};

/// A wireless controller for the `embedded_svc::wifi::asynch::Wifi` type.
pub struct SvcWifiController<W>(W);

impl<W> SvcWifiController<W> {
    /// Create a new `SvcWifi` instance.
    pub const fn new(wifi: W) -> Self {
        Self(wifi)
    }

    /// Get a reference to the inner `embedded_svc::wifi::asynch::Wifi` instance.
    pub fn wifi(&self) -> &W {
        &self.0
    }

    /// Get a mutable reference to the inner `embedded_svc::wifi::asynch::Wifi` instance.
    pub fn wifi_mut(&mut self) -> &mut W {
        &mut self.0
    }
}

impl<W> SvcWifiController<W>
where
    W: Wifi,
{
    fn to_err(_err: W::Error) -> Error {
        Error::new(ErrorCode::NoNetworkInterface) // TODO
    }
}

impl<T> Controller for SvcWifiController<T>
where
    T: Wifi,
{
    type Data = WifiData;

    async fn scan<F>(&mut self, network_id: Option<&WifiSsid>, mut callback: F) -> Result<(), Error>
    where
        F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>) -> Result<(), Error>,
    {
        let _ = self.0.start().await;

        let (result, _) = self.0.scan_n::<5>().await.map_err(Self::to_err)?;

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
                }))?;
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
            if (auth_method == AuthMethod::None) != creds.password.is_empty() {
                // Try open wifi networks only if the provided password is empty
                continue;
            }

            let _ = self.0.stop().await;

            self.0
                .set_configuration(&Configuration::Client(ClientConfiguration {
                    ssid: creds.ssid.0.clone(),
                    auth_method,
                    password: creds.password.clone(),
                    ..Default::default()
                }))
                .await
                .map_err(Self::to_err)?;

            self.0.start().await.map_err(Self::to_err)?;

            if self.0.connect().await.is_ok() {
                return Ok(());
            }
        }

        Err(ErrorCode::NoNetworkInterface.into()) // TODO
    }

    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId>,
        Error,
    > {
        let conf = self.0.get_configuration().await.map_err(Self::to_err)?;

        Ok(match conf {
            Configuration::Client(ClientConfiguration { ssid, .. }) => Some(WifiSsid(ssid)),
            _ => None,
        })
    }

    async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
        // TODO: We need stats() in the Wifi trait

        let conf = self.0.get_configuration().await.map_err(Self::to_err)?;

        Ok(match conf {
            Configuration::Client(_) => Some(WifiNwDiagData {
                bssid: [0; 6],
                security_type: wifi_nw_diagnostics::WiFiSecurity::Unspecified,
                wifi_version: WiFiVersion::B,
                channel_number: 20,
                rssi: 0,
            }),
            _ => None,
        })
    }
}
