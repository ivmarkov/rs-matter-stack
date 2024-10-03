use core::cell::RefCell;
use core::pin::pin;

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use heapless::Vec;
use rs_matter::{
    error::Error,
    utils::sync::{IfMutex, Signal},
};

use super::traits::{Controller, NetworkCredentials, WirelessDTOs};

pub enum ControllerExchange<T>
where
    T: WirelessDTOs,
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
    T: WirelessDTOs,
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

pub struct ControllerProxy<T>
where
    T: WirelessDTOs,
{
    supports_concurrent_connection: bool,
    connected: Signal<NoopRawMutex, bool>,
    pipe: IfMutex<NoopRawMutex, RefCell<ControllerExchange<T>>>,
    lock: IfMutex<NoopRawMutex, ()>,
}

impl<T> ControllerProxy<T>
where
    T: WirelessDTOs,
{
    pub const fn new(supports_concurrent_connection: bool) -> Self {
        Self {
            supports_concurrent_connection,
            connected: Signal::new(false),
            pipe: IfMutex::new(RefCell::new(ControllerExchange::Empty)),
            lock: IfMutex::new(()),
        }
    }

    pub async fn process_with<C>(&self, mut controller: C) -> Result<(), Error>
    where
        C: Controller<
            NetworkCredentials = T::NetworkCredentials,
            ScanResult = T::ScanResult,
            Stats = T::Stats,
        >,
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

    async fn pipe<P, M>(&self, predicate: P, modifier: M) -> bool
    where
        P: Fn(&ControllerExchange<T>) -> bool,
        M: FnOnce(&mut ControllerExchange<T>),
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

    async fn command(&self, command: ControllerExchange<T>) -> bool {
        self.pipe(
            |data| matches!(*data, ControllerExchange::Empty),
            |data| *data = command,
        )
        .await
    }

    async fn reply_with<F>(&self, process: F) -> Result<ControllerExchange<T>, ()>
    where
        F: FnOnce(&mut ControllerExchange<T>) -> ControllerExchange<T>,
    {
        let mut reply = None;

        let connected = self
            .pipe(ControllerExchange::is_reply, |data| {
                reply = Some(process(data));
            })
            .await;

        if connected {
            Ok(reply.unwrap())
        } else {
            Err(())
        }
    }

    async fn reply(&self) -> Result<ControllerExchange<T>, ()> {
        self.reply_with(|data| core::mem::replace(data, ControllerExchange::Empty))
            .await
    }
}

impl<T> WirelessDTOs for &ControllerProxy<T>
where
    T: WirelessDTOs,
    T::ScanResult: Clone,
    T::Stats: Default,
{
    type NetworkCredentials = T::NetworkCredentials;
    type ScanResult = T::ScanResult;
    type Stats = T::Stats;
}

impl<T> Controller for &ControllerProxy<T>
where
    T: WirelessDTOs,
    T::ScanResult: Clone,
    T::Stats: Default,
{
    fn supports_concurrent_connection(&self) -> bool {
        self.supports_concurrent_connection
    }

    async fn scan<F>(
        &mut self,
        network_id: Option<&<Self::NetworkCredentials as NetworkCredentials>::NetworkId>,
        mut callback: F,
    ) -> Result<(), Error>
    where
        F: FnMut(Option<&Self::ScanResult>),
    {
        if !self
            .command(ControllerExchange::Scan(network_id.cloned()))
            .await
        {
            callback(None);
            return Ok(());
        }

        let reply = self.reply().await;

        match reply {
            Ok(ControllerExchange::ScanResult(result)) => match result {
                Ok(result) => {
                    result.iter().for_each(|result| callback(Some(result)));
                    callback(None);

                    Ok(())
                }
                Err(e) => Err(e),
            },
            Ok(_) => unreachable!(),
            Err(_) => {
                callback(None);
                Ok(())
            }
        }
    }

    async fn connect(&mut self, creds: &Self::NetworkCredentials) -> Result<(), Error> {
        if !self
            .command(ControllerExchange::Connect(creds.clone()))
            .await
        {
            return Ok(());
        }

        let reply = self.reply().await;

        match reply {
            Ok(ControllerExchange::ConnectResult(result)) => result,
            Ok(_) => unreachable!(),
            Err(_) => Ok(()),
        }
    }

    async fn connected_network(
        &mut self,
    ) -> Result<Option<<Self::NetworkCredentials as NetworkCredentials>::NetworkId>, Error> {
        if !self.command(ControllerExchange::ConnectedNetwork).await {
            return Ok(None);
        }

        let reply = self.reply().await;

        match reply {
            Ok(ControllerExchange::ConnectedNetworkResult(result)) => result,
            Ok(_) => unreachable!(),
            Err(_) => Ok(None),
        }
    }

    async fn stats(&mut self) -> Result<Self::Stats, Error> {
        if !self.command(ControllerExchange::Stats).await {
            return Ok(Default::default());
        }

        let reply = self.reply().await;

        match reply {
            Ok(ControllerExchange::StatsResult(result)) => result,
            Ok(_) => unreachable!(),
            Err(_) => Ok(Default::default()),
        }
    }
}
