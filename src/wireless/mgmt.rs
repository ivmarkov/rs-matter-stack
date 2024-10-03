//! Wireless manager module.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Timer};

use log::{error, info, warn};

use rs_matter::data_model::sdm::nw_commissioning::NetworkCommissioningStatus;
use rs_matter::error::Error;

use crate::wireless::NetworkCredentials;

use super::store::{NetworkContext, NetworkStatus};
use super::Controller;

/// A generic Wireless manager.
///
/// Utilizes the information w.r.t. wireless networks that the
/// Matter stack pushes into the `WirelessContext` struct to connect
/// to one of these networks, in preference order matching the order of the
/// networks there, and the connect request that might be provided by the
/// Matter stack.
///
/// Also monitors the Wireless connection status and retries the connection
/// with a backoff strategy and in a round-robin fashion with the other
/// networks in case of a failure.
///
/// The comminication with the wireless device is done through the `WirelessController` trait.
pub struct WirelessManager<W>(W);

impl<W> WirelessManager<W>
where
    W: Controller,
    W::NetworkCredentials: Clone,
{
    /// Create a new wireless manager.
    pub const fn new(controller: W) -> Self {
        Self(controller)
    }

    /// Runs the wireless manager.
    ///
    /// This function will try to connect to the networks in the `NetworkContext`
    /// and will retry the connection in case of a failure.
    pub async fn run<const N: usize, M>(
        &mut self,
        context: &NetworkContext<N, M, W::NetworkCredentials>,
    ) -> Result<(), Error>
    where
        M: RawMutex,
    {
        let mut network_id = None;

        loop {
            let creds = context.state.lock(|state| {
                let mut state = state.borrow_mut();

                state.get_next_network(network_id.as_ref())
            });

            let Some(creds) = creds else {
                context.wait_network_activated().await;
                continue;
            };

            network_id = Some(creds.network_id().clone());

            let _ = self.connect_with_retries(&creds, context).await;
        }
    }

    async fn connect_with_retries<const N: usize, M>(
        &mut self,
        creds: &W::NetworkCredentials,
        context: &NetworkContext<N, M, W::NetworkCredentials>,
    ) -> Result<(), Error>
    where
        M: RawMutex,
    {
        loop {
            let mut result = Ok(());

            for delay in [2, 5, 10, 20, 30, 60].iter().copied() {
                result = self.connect(creds).await;

                if result.is_ok() {
                    break;
                } else {
                    warn!(
                        "Connection to network with ID {} failed: {:?}, retrying in {delay}s",
                        creds.network_id(),
                        result
                    );
                }

                Timer::after(Duration::from_secs(delay)).await;
            }

            context.state.lock(|state| {
                let mut state = state.borrow_mut();

                if result.is_ok() {
                    state.connected_once = true;
                }

                state.status = Some(NetworkStatus {
                    network_id: creds.network_id().clone(),
                    status: if result.is_ok() {
                        NetworkCommissioningStatus::Success
                    } else {
                        NetworkCommissioningStatus::OtherConnectionFailure
                    },
                    value: 0,
                });
            });

            if result.is_ok() {
                info!("Connected to network with ID {}", creds.network_id());

                self.wait_disconnect().await?;
            } else {
                error!(
                    "Failed to connect to network with ID {}: {:?}",
                    creds.network_id(),
                    result
                );

                break result;
            }
        }
    }

    async fn wait_disconnect(&mut self) -> Result<(), Error> {
        loop {
            {
                if self.0.connected_network().await?.is_none() {
                    break Ok(());
                }
            }

            let timer = Timer::after(Duration::from_secs(5));

            timer.await;
        }
    }

    async fn connect(&mut self, creds: &W::NetworkCredentials) -> Result<(), Error> {
        info!("Connecting to network with ID {}", creds.network_id());

        // TODO
        // let auth_methods: &[AuthMethod] = if creds.password.is_empty() {
        //     &[AuthMethod::None]
        // } else {
        //     &[
        //         AuthMethod::WPAWPA2Personal,
        //         AuthMethod::WPA2WPA3Personal,
        //         AuthMethod::WEP,
        //     ]
        // };

        // let mut result = Ok(());

        // for auth_method in auth_methods.iter().copied() {
        //     let conf = wifi::Configuration::Client(wifi::ClientConfiguration {
        //         ssid: creds.ssid.clone(),
        //         auth_method,
        //         password: creds.password.clone(),
        //         ..Default::default()
        //     });

        //     result = self.connect_with(&conf).await;

        //     if result.is_ok() {
        //         break;
        //     }
        // }

        // result

        Ok(())
    }

    // TODO
    // async fn connect_with(&mut self, conf: &wifi::Configuration) -> Result<(), Error> {
    //     info!("Connecting with {:?}", conf);

    //     let _ = self.0.stop().await;

    //     self.0.set_configuration(conf).await?;

    //     self.0.start().await?;

    //     let connect = matches!(conf, wifi::Configuration::Client(_))
    //         && !matches!(
    //             conf,
    //             wifi::Configuration::Client(wifi::ClientConfiguration {
    //                 auth_method: wifi::AuthMethod::None,
    //                 ..
    //             })
    //         );

    //     if connect {
    //         self.0.connect().await?;
    //     }

    //     info!("Successfully connected with {:?}", conf);

    //     // TODO esp!(unsafe { esp_netif_create_ip6_linklocal(wifi.wifi().sta_netif().handle() as _) })?;

    //     Ok(())
    // }
}

// impl<W> Wireless<WifiCredentials> for WirelessManager<W>
// where
//     W: Wifi,
// {
//     async fn run<const N: usize, M>(&mut self, context: &NetworkContext<N, M, WifiCredentials>) -> Result<(), Error>
//     where
//         M: RawMutex,
//     {
//         WirelessManager::run(self, context).await
//     }
// }
