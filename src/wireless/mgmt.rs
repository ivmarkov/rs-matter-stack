//! Wireless manager module.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Timer};

use log::{error, info, warn};

use rs_matter::data_model::sdm::nw_commissioning::NetworkCommissioningStatus;
use rs_matter::error::Error;

use crate::wireless::NetworkCredentials;

use super::store::{NetworkContext, NetworkStatus};
use super::traits::WirelessData;
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
/// The comminication with the wireless device is done through the `Controller` trait.
pub struct WirelessManager<T>(T);

impl<T> WirelessManager<T>
where
    T: Controller,
    <T::Data as WirelessData>::NetworkCredentials: Clone,
{
    /// Create a new wireless manager.
    pub const fn new(controller: T) -> Self {
        Self(controller)
    }

    /// Runs the wireless manager.
    ///
    /// This function will try to connect to the networks in the `NetworkContext`
    /// and will retry the connection in case of a failure.
    pub async fn run<const N: usize, M>(
        &mut self,
        context: &NetworkContext<N, M, T::Data>,
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
        creds: &<T::Data as WirelessData>::NetworkCredentials,
        context: &NetworkContext<N, M, T::Data>,
    ) -> Result<(), Error>
    where
        M: RawMutex,
    {
        loop {
            let mut result = Ok(());

            for delay in [2, 5, 10, 20, 30, 60].iter().copied() {
                info!("Connecting to network with ID {}", creds.network_id());

                result = self.0.connect(creds).await;

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
}
