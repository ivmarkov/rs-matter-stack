//! `Controller` proxy for bridging the wireless clusters with the actual wireless controller.
//!
//! Necessary because the wireless clusters have a different life-cycle from the controller.

use core::pin::pin;

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::RawMutex;

use rs_matter::error::Error;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::storage::Vec;
use rs_matter::utils::sync::{IfMutex, Signal};

use super::traits::{Controller, NetworkCredentials, WirelessData};

enum ControllerExchange<T>
where
    T: WirelessData,
{
    Empty,
    Processing,
    Scan(Option<<T::NetworkCredentials as NetworkCredentials>::NetworkId>),
    ScanResult(Result<Vec<T::ScanResult, 5>, Error>),
    Connect(T::NetworkCredentials),
    ConnectResult(Result<(), Error>),
    ConnectedNetwork,
    ConnectedNetworkResult(
        Result<Option<<T::NetworkCredentials as NetworkCredentials>::NetworkId>, Error>,
    ),
    Stats,
    StatsResult(Result<T::Stats, Error>),
}

impl<T> ControllerExchange<T>
where
    T: WirelessData,
{
    fn is_reply(&self) -> bool {
        !self.is_command() && !matches!(self, ControllerExchange::Processing)
    }

    fn is_command(&self) -> bool {
        matches!(
            self,
            ControllerExchange::Scan(_)
                | ControllerExchange::Connect(_)
                | ControllerExchange::ConnectedNetwork
        )
    }
}

/// A proxy for a wireless controller.
///
/// Solves lifetime/lifecycle issues between the wireless clusters and the controller -
/// in other words, allows the proxy to be created earlier and live longer than the controller.
///
/// When there is no controller (i.e. the wireless network is disconnected), the proxy will ignore
/// some commands, return default values for others (statistics) and will error out on the remaining.
pub struct ControllerProxy<M, T>
where
    M: RawMutex,
    T: WirelessData,
{
    connected: Signal<M, bool>,
    pipe: IfMutex<M, RefCell<ControllerExchange<T>>>,
    lock: IfMutex<M, ()>,
}

impl<M, T> Default for ControllerProxy<M, T>
where
    M: RawMutex,
    T: WirelessData,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<M, T> ControllerProxy<M, T>
where
    M: RawMutex,
    T: WirelessData,
{
    /// Create a new controller proxy.
    pub const fn new() -> Self {
        Self {
            connected: Signal::new(false),
            pipe: IfMutex::new(RefCell::new(ControllerExchange::Empty)),
            lock: IfMutex::new(()),
        }
    }

    pub fn init() -> impl Init<Self> {
        init!(Self {
            connected <- Signal::init(false),
            pipe <- IfMutex::init(RefCell::init(ControllerExchange::Empty)),
            lock <- IfMutex::init(()),
        })
    }

    /// Connect the proxy to an active controller
    pub async fn process_with<C>(&self, mut controller: C) -> Result<(), Error>
    where
        C: Controller<Data = T>,
        T::ScanResult: Clone,
    {
        let _lock = self.lock.lock().await;

        let _guard = scopeguard::guard((), |_| {
            self.connected.modify(|connected| {
                *connected = false;
                (true, ())
            });
        });

        self.connected.modify(|connected| {
            *connected = true;
            (true, ())
        });

        loop {
            let pipe = self.pipe.lock_if(|data| data.borrow().is_command()).await;

            let command =
                core::mem::replace(&mut *pipe.borrow_mut(), ControllerExchange::Processing);

            match command {
                ControllerExchange::Connect(creds) => {
                    let result = controller.connect(&creds).await;

                    *pipe.borrow_mut() = ControllerExchange::ConnectResult(result);
                }
                ControllerExchange::ConnectedNetwork => {
                    let result = controller.connected_network().await;

                    *pipe.borrow_mut() = ControllerExchange::ConnectedNetworkResult(result);
                }
                ControllerExchange::Scan(network_id) => {
                    let mut vec = Vec::new();

                    let result = controller
                        .scan(network_id.as_ref(), |result| {
                            if let Some(result) = result {
                                let _ = vec.push(result.clone());
                            }
                        })
                        .await;

                    match result {
                        Ok(_) => *pipe.borrow_mut() = ControllerExchange::ScanResult(Ok(vec)),
                        Err(e) => *pipe.borrow_mut() = ControllerExchange::ScanResult(Err(e)),
                    }
                }
                ControllerExchange::Stats => {
                    let result = controller.stats().await;

                    *pipe.borrow_mut() = ControllerExchange::StatsResult(result);
                }
                _ => unreachable!(),
            }
        }
    }

    async fn pipe<P, O>(&self, predicate: P, modifier: O) -> bool
    where
        P: Fn(&ControllerExchange<T>) -> bool,
        O: FnOnce(&mut ControllerExchange<T>),
    {
        let mut signal = pin!(self.connected.wait(|connected| (!*connected).then_some(())));
        let mut pipe = pin!(self.pipe.lock_if(|data| predicate(&data.borrow())));

        let result = select(&mut signal, &mut pipe).await;

        match result {
            Either::First(_) => false,
            Either::Second(pipe) => {
                modifier(&mut *pipe.borrow_mut());

                true
            }
        }
    }

    async fn command(&self, command: ControllerExchange<T>) -> Option<ControllerExchange<T>> {
        let connected = self
            .pipe(
                |data| matches!(*data, ControllerExchange::Empty),
                |data| *data = command,
            )
            .await;

        if !connected {
            return None;
        }

        let mut reply = None;

        let connected = self
            .pipe(ControllerExchange::is_reply, |data| {
                reply = Some(core::mem::replace(data, ControllerExchange::Empty));
            })
            .await;

        if !connected {
            return None;
        }

        reply
    }
}

impl<M, T> Controller for &ControllerProxy<M, T>
where
    M: RawMutex,
    T: WirelessData,
    T::ScanResult: Clone,
    T::Stats: Default,
{
    type Data = T;

    async fn scan<F>(
        &mut self,
        network_id: Option<
            &<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId,
        >,
        mut callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&<Self::Data as WirelessData>::ScanResult>),
    {
        let reply = self
            .command(ControllerExchange::Scan(network_id.cloned()))
            .await;

        match reply {
            Some(ControllerExchange::ScanResult(result)) => match result {
                Ok(result) => {
                    result.iter().for_each(|result| callback(Some(result)));
                    callback(None);

                    Ok(())
                }
                Err(e) => Err(e),
            },
            Some(_) => unreachable!(),
            None => {
                callback(None);
                Ok(())
            }
        }
    }

    async fn connect(
        &mut self,
        creds: &<Self::Data as WirelessData>::NetworkCredentials,
    ) -> Result<(), Error> {
        // TODO: Fire a signal on network connection attempt, if in disconnected mode
        // (non-concurrent commissioning)

        let reply = self
            .command(ControllerExchange::Connect(creds.clone()))
            .await;

        match reply {
            Some(ControllerExchange::ConnectResult(result)) => result,
            Some(_) => unreachable!(),
            None => Ok(()),
        }
    }

    async fn connected_network(
        &mut self,
    ) -> Result<
        Option<<<Self::Data as WirelessData>::NetworkCredentials as NetworkCredentials>::NetworkId>,
        Error,
    > {
        let reply = self.command(ControllerExchange::ConnectedNetwork).await;

        match reply {
            Some(ControllerExchange::ConnectedNetworkResult(result)) => result,
            Some(_) => unreachable!(),
            None => Ok(None),
        }
    }

    async fn stats(&mut self) -> Result<<Self::Data as WirelessData>::Stats, Error> {
        let reply = self.command(ControllerExchange::Stats).await;

        match reply {
            Some(ControllerExchange::StatsResult(result)) => result,
            Some(_) => unreachable!(),
            None => Ok(Default::default()),
        }
    }
}
