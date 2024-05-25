use core::fmt::Write as _;
use core::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;

use edge_nal::{Multicast, Readable, UdpBind, UdpSplit};
use embassy_futures::select::select3;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use log::info;

use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::core::IMBuffer;
use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata};
use rs_matter::data_model::sdm::dev_att::DevAttDataFetcher;
use rs_matter::data_model::subscriptions::Subscriptions;
use rs_matter::error::ErrorCode;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::respond::DefaultResponder;
use rs_matter::transport::network::{NetworkReceive, NetworkSend};
use rs_matter::utils::buf::PooledBuffers;
use rs_matter::utils::epoch::Epoch;
use rs_matter::utils::rand::Rand;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::signal::Signal;
use rs_matter::{CommissioningData, Matter, MATTER_PORT};

use crate::error::Error;
use crate::netif::{Netif, NetifConf};
use crate::persist::{self, Persist};
use crate::udp;

pub use eth::*;
pub use wifible::*;

const MAX_SUBSCRIPTIONS: usize = 3;
const MAX_IM_BUFFERS: usize = 10;
const MAX_RESPONDERS: usize = 4;
const MAX_BUSY_RESPONDERS: usize = 2;

mod eth;
mod wifible;

/// A trait modeling a specific network type.
/// `MatterStack` is parameterized by a network type implementing this trait.
pub trait Network {
    const INIT: Self;

    type Mutex: RawMutex;

    fn network_context(&self) -> persist::NetworkContext<'_, MAX_WIFI_NETWORKS, Self::Mutex> {
        persist::NetworkContext::Eth
    }
}

/// An enum modeling the mDNS service to be used.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum MdnsType {
    /// The mDNS service provided by the `rs-matter` crate.
    #[default]
    Builtin,
}

impl MdnsType {
    pub const fn default() -> Self {
        Self::Builtin
    }
}

/// The `MatterStack` struct is the main entry point for the Matter stack.
///
/// It wraps the actual `rs-matter` Matter instance and provides a simplified API for running the stack.
pub struct MatterStack<'a, N>
where
    N: Network,
{
    matter: Matter<'a>,
    buffers: PooledBuffers<MAX_IM_BUFFERS, NoopRawMutex, IMBuffer>,
    subscriptions: Subscriptions<MAX_SUBSCRIPTIONS>,
    #[allow(unused)]
    network: N,
    #[allow(unused)]
    mdns: MdnsType,
    ip_info: Signal<NoopRawMutex, Option<NetifConf>>,
}

impl<'a, N> MatterStack<'a, N>
where
    N: Network,
{
    /// Create a new `MatterStack` instance.
    pub const fn new(
        dev_det: &'a BasicInfoConfig,
        dev_att: &'a dyn DevAttDataFetcher,
        mdns: MdnsType,
        epoch: Epoch,
        rand: Rand,
    ) -> Self {
        Self {
            matter: Matter::new(
                dev_det,
                dev_att,
                rs_matter::mdns::MdnsService::Builtin,
                epoch,
                rand,
                MATTER_PORT,
            ),
            buffers: PooledBuffers::new(0),
            subscriptions: Subscriptions::new(),
            network: N::INIT,
            mdns,
            ip_info: Signal::new(None),
        }
    }

    /// Get a reference to the `Matter` instance.
    pub const fn matter(&self) -> &Matter<'a> {
        &self.matter
    }

    /// Get a reference to the `Network` instance.
    /// Useful when the user instantiates `MatterStack` with a custom network type.
    pub const fn network(&self) -> &N {
        &self.network
    }

    /// Notifies the Matter instance that there is a change in the state
    /// of one of the clusters.
    ///
    /// User is expected to call this method when user-provided clusters
    /// change their state.
    ///
    /// This is necessary so as the Matter instance can notify clients
    /// that have active subscriptions to some of the changed clusters.
    pub fn notify_changed(&self) {
        self.subscriptions.notify_changed();
    }

    /// User code hook to get the state of the netif passed to the
    /// `run_with_netif` method.
    ///
    /// Useful when user code needs to bring up/down its own IP services depending on
    /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    pub async fn get_netif_info(&self) -> Option<NetifConf> {
        self.ip_info
            .wait(|netif_info| Some(netif_info.clone()))
            .await
    }

    /// User code hook to detect changes to the IP state of the netif passed to the
    /// `run_with_netif` method.
    ///
    /// Useful when user code needs to bring up/down its own IP services depending on
    /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    pub async fn wait_netif_changed(
        &self,
        prev_netif_info: Option<&NetifConf>,
    ) -> Option<NetifConf> {
        self.ip_info
            .wait(|netif_info| (netif_info.as_ref() != prev_netif_info).then(|| netif_info.clone()))
            .await
    }

    /// This method is a specialization of `run_with_transport` over the UDP transport (both IPv4 and IPv6).
    /// It calls `run_with_transport` and in parallel runs the mDNS service.
    ///
    /// The netif instance is necessary, so that the loop can monitor the network and bring up/down
    /// the main UDP transport and the mDNS service when the netif goes up/down or changes its IP addresses.
    pub async fn run_with_netif<'d, H, P, I>(
        &self,
        mut persist: P,
        netif: I,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
        handler: H,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        I: Netif + UdpBind,
        for<'s> I::Socket<'s>: UdpSplit,
        for<'s> I::Socket<'s>: Multicast,
        for<'s, 'r> <I::Socket<'s> as UdpSplit>::Receive<'r>: Readable,
    {
        loop {
            info!("Waiting for the network to come up...");

            let reset_netif_info = || {
                self.ip_info.modify(|global_netif_info| {
                    if global_netif_info.is_some() {
                        *global_netif_info = None;
                        (true, ())
                    } else {
                        (false, ())
                    }
                });
            };

            let _guard = scopeguard::guard((), |_| reset_netif_info());

            reset_netif_info();

            let ip_info = loop {
                if let Some(ip_info) = netif.get_conf() {
                    break ip_info;
                }

                netif.wait_conf_change().await;
            };

            info!("Got IP network: {ip_info}");

            self.ip_info.modify(|global_ip_info| {
                *global_ip_info = Some(ip_info.clone());

                (true, ())
            });

            let mut socket = netif
                .bind(SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::UNSPECIFIED,
                    MATTER_PORT,
                    0,
                    ip_info.interface,
                )))
                .await
                .map_err(|_| ErrorCode::StdIoError)?;

            let (recv, send) = socket.split();

            let mut main = pin!(self.run_with_transport(
                udp::Udp(send),
                udp::Udp(recv),
                &mut persist,
                dev_comm.clone(),
                &handler
            ));
            let mut mdns = pin!(self.run_builtin_mdns(&netif, &ip_info));
            let mut down = pin!(async {
                loop {
                    let next = netif.get_conf();
                    let next = next.as_ref();

                    if Some(&ip_info) != next {
                        break;
                    }

                    netif.wait_conf_change().await;
                }

                Ok(())
            });

            select3(&mut main, &mut mdns, &mut down).coalesce().await?;

            info!("Network change detected");
        }
    }

    /// A transport-agnostic method to run the main Matter loop.
    /// The user is expected to provide a transport implementation in the form of
    /// `NetworkSend` and `NetworkReceive` implementations.
    ///
    /// The utility runs the following tasks:
    /// - The main Matter transport via the user-provided traits
    /// - The PSM task handling changes to fabrics and ACLs as well as initial load of these from NVS
    /// - The Matter responder (i.e. handling incoming exchanges)
    ///
    /// Unlike `run_with_netif`, this utility method does _not_ run the mDNS service, as the
    /// user-provided transport might not be IP-based (i.e. BLE).
    ///
    /// It also has no facilities for monitoring the transport network state.
    pub async fn run_with_transport<'d, S, R, P, H>(
        &self,
        send: S,
        recv: R,
        persist: P,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
        handler: H,
    ) -> Result<(), Error>
    where
        S: NetworkSend,
        R: NetworkReceive,
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
    {
        // Reset the Matter transport buffers and all sessions first
        self.matter().reset_transport()?;

        let mut psm = pin!(self.run_psm(persist));
        let mut respond = pin!(self.run_responder(handler));
        let mut transport = pin!(self.run_transport(send, recv, dev_comm));

        select3(&mut psm, &mut respond, &mut transport)
            .coalesce()
            .await?;

        Ok(())
    }

    async fn run_psm<P>(&self, mut persist: P) -> Result<(), Error>
    where
        P: Persist,
    {
        if false {
            persist
                .run(self.matter(), self.network().network_context())
                .await
        } else {
            core::future::pending().await
        }
    }

    async fn run_responder<H>(&self, handler: H) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        let responder =
            DefaultResponder::new(self.matter(), &self.buffers, &self.subscriptions, handler);

        info!(
            "Responder memory: Responder={}B, Runner={}B",
            core::mem::size_of_val(&responder),
            core::mem::size_of_val(&responder.run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>())
        );

        // Run the responder with up to MAX_RESPONDERS handlers (i.e. MAX_RESPONDERS exchanges can be handled simultenously)
        // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
        responder
            .run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>()
            .await?;

        Ok(())
    }

    async fn run_builtin_mdns<I>(&self, netif: &I, netif_conf: &NetifConf) -> Result<(), Error>
    where
        I: UdpBind,
        for<'s> I::Socket<'s>: UdpSplit,
        for<'s> I::Socket<'s>: Multicast,
        for<'s, 'r> <I::Socket<'s> as UdpSplit>::Receive<'r>: Readable,
    {
        use rs_matter::mdns::{
            Host, MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_PORT,
        };

        let mut socket = netif
            .bind(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::UNSPECIFIED,
                MDNS_PORT,
                0,
                netif_conf.interface,
            )))
            .await
            .map_err(|_| ErrorCode::StdIoError)?;

        socket
            .join(IpAddr::V4(MDNS_IPV4_BROADCAST_ADDR))
            .await
            .map_err(|_| ErrorCode::StdIoError)?; // TODO: netif_conf.ipv4
        socket
            .join(IpAddr::V6(MDNS_IPV6_BROADCAST_ADDR))
            .await
            .map_err(|_| ErrorCode::StdIoError)?; // TODO: netif_conf.interface

        let (recv, send) = socket.split();

        let mut hostname = heapless::String::<12>::new();
        write!(
            hostname,
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            netif_conf.mac[0],
            netif_conf.mac[1],
            netif_conf.mac[2],
            netif_conf.mac[3],
            netif_conf.mac[4],
            netif_conf.mac[5]
        )
        .unwrap();

        self.matter()
            .run_builtin_mdns(
                udp::Udp(send),
                udp::Udp(recv),
                &Host {
                    id: 0,
                    hostname: &hostname,
                    ip: netif_conf.ipv4.octets(),
                    ipv6: Some(netif_conf.ipv6.octets()),
                },
                Some(netif_conf.interface),
            )
            .await?;

        Ok(())
    }

    async fn run_transport<S, R>(
        &self,
        send: S,
        recv: R,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
    ) -> Result<(), Error>
    where
        S: NetworkSend,
        R: NetworkReceive,
    {
        self.matter().run(send, recv, dev_comm).await?;

        Ok(())
    }
}

// /// A utility function to initialize the `async-io` Reactor which is
// /// used for IP-based networks (UDP and TCP).
// ///
// /// User is expected to call this method early in the application's lifecycle
// /// when there is plenty of task stack space available, as the initialization
// /// consumes > 10KB of stack space, so it has to be done with care.
// #[inline(never)]
// #[cold]
// pub fn init_async_io() -> Result<(), Error> {
//     // We'll use `async-io` for networking, so ESP IDF VFS needs to be initialized
//     esp_idf_svc::io::vfs::initialize_eventfd(3)?;

//     block_on(init_async_io_async());

//     Ok(())
// }

// #[inline(never)]
// #[cold]
// async fn init_async_io_async() {
//     #[cfg(not(feature = "async-io-mini"))]
//     {
//         // Force the `async-io` lazy initialization to trigger earlier rather than later,
//         // as it consumes a lot of temp stack memory
//         async_io::Timer::after(core::time::Duration::from_millis(100)).await;
//         info!("Async IO initialized; using `async-io`");
//     }

//     #[cfg(feature = "async-io-mini")]
//     {
//         // Nothing to initialize for `async-io-mini`
//         info!("Async IO initialized; using `async-io-mini`");
//     }
// }
