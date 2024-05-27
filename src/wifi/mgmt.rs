use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Timer};

use embedded_svc::wifi::{self, asynch::Wifi, AuthMethod};
use log::{error, info, warn};

use rs_matter::{
    data_model::sdm::nw_commissioning::NetworkCommissioningStatus,
    error::{Error, ErrorCode},
};

use super::{WifiContext, WifiCredentials, WifiStatus};

/// A generic Wifi manager.
///
/// Utilizes the information w.r.t. Wifi networks that the
/// Matter stack pushes into the `WifiContext` struct to connect
/// to one of these networks, in preference order matching the order of the
/// networks there, and the connect request that might be provided by the
/// Matter stack.
///
/// Also monitors the Wifi connection status and retries the connection
/// with a backoff strategy and in a round-robin fashion with the other
/// networks in case of a failure.
pub struct WifiManager<'a, const N: usize, M, W>
where
    M: RawMutex,
{
    wifi: W,
    context: &'a WifiContext<N, M>,
}

impl<'a, const N: usize, M, W> WifiManager<'a, N, M, W>
where
    M: RawMutex,
    W: Wifi,
{
    /// Create a new Wifi manager.
    pub const fn new(wifi: W, context: &'a WifiContext<N, M>) -> Self {
        Self { wifi, context }
    }

    /// Runs the Wifi manager.
    ///
    /// This function will try to connect to the networks in the `WifiContext`
    /// and will retry the connection in case of a failure.
    pub async fn run(&mut self) -> Result<(), Error> {
        let mut ssid = None;

        loop {
            let creds = self.context.state.lock(|state| {
                let mut state = state.borrow_mut();

                state.get_next_network(ssid.as_deref())
            });

            let Some(creds) = creds else {
                // No networks, bail out
                warn!("No networks found");
                return Err(ErrorCode::Invalid.into()); // TODO
            };

            ssid = Some(creds.ssid.clone());

            let _ = self.connect_with_retries(&creds).await;
        }
    }

    async fn connect_with_retries(&mut self, creds: &WifiCredentials) -> Result<(), W::Error> {
        loop {
            let mut result = Ok(());

            for delay in [2, 5, 10, 20, 30, 60].iter().copied() {
                result = self.connect(creds).await;

                if result.is_ok() {
                    break;
                } else {
                    warn!(
                        "Connection to SSID {} failed: {:?}, retrying in {delay}s",
                        creds.ssid, result
                    );
                }

                Timer::after(Duration::from_secs(delay)).await;
            }

            self.context.state.lock(|state| {
                let mut state = state.borrow_mut();

                if result.is_ok() {
                    state.connected_once = true;
                }

                state.status = Some(WifiStatus {
                    ssid: creds.ssid.clone(),
                    status: if result.is_ok() {
                        NetworkCommissioningStatus::Success
                    } else {
                        NetworkCommissioningStatus::OtherConnectionFailure
                    },
                    value: 0,
                });
            });

            if result.is_ok() {
                info!("Connected to SSID {}", creds.ssid);

                self.wait_disconnect().await?;
            } else {
                error!("Failed to connect to SSID {}: {:?}", creds.ssid, result);

                break result;
            }
        }
    }

    async fn wait_disconnect(&mut self) -> Result<(), W::Error> {
        loop {
            {
                if !self.wifi.is_connected().await? {
                    break Ok(());
                }
            }

            let timer = Timer::after(Duration::from_secs(5));

            timer.await;
        }
    }

    async fn connect(&mut self, creds: &WifiCredentials) -> Result<(), W::Error> {
        info!("Connecting to SSID {}", creds.ssid);

        let auth_methods: &[AuthMethod] = if creds.password.is_empty() {
            &[AuthMethod::None]
        } else {
            &[
                AuthMethod::WPAWPA2Personal,
                AuthMethod::WPA2WPA3Personal,
                AuthMethod::WEP,
            ]
        };

        let mut result = Ok(());

        for auth_method in auth_methods.iter().copied() {
            let conf = wifi::Configuration::Client(wifi::ClientConfiguration {
                ssid: creds.ssid.clone(),
                auth_method,
                password: creds.password.clone(),
                ..Default::default()
            });

            result = self.connect_with(&conf).await;

            if result.is_ok() {
                break;
            }
        }

        result
    }

    async fn connect_with(&mut self, conf: &wifi::Configuration) -> Result<(), W::Error> {
        info!("Connecting with {:?}", conf);

        let _ = self.wifi.stop().await;

        self.wifi.set_configuration(conf).await?;

        self.wifi.start().await?;

        let connect = matches!(conf, wifi::Configuration::Client(_))
            && !matches!(
                conf,
                wifi::Configuration::Client(wifi::ClientConfiguration {
                    auth_method: wifi::AuthMethod::None,
                    ..
                })
            );

        if connect {
            self.wifi.connect().await?;
        }

        info!("Successfully connected with {:?}", conf);

        // TODO esp!(unsafe { esp_netif_create_ip6_linklocal(wifi.wifi().sta_netif().handle() as _) })?;

        Ok(())
    }
}
