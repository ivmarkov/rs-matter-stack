#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![warn(clippy::large_futures)]
#![warn(clippy::large_stack_frames)]
#![warn(clippy::large_types_passed_by_value)]

use core::future::Future;
use core::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;

use edge_nal::{UdpBind, UdpSplit};

use embassy_futures::select::{select3, select4};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use log::info;

use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::core::IMBuffer;
use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata};
use rs_matter::data_model::sdm::dev_att::DevAttDataFetcher;
use rs_matter::data_model::subscriptions::Subscriptions;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::mdns::{Mdns, MdnsService};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::respond::DefaultResponder;
use rs_matter::transport::network::{NetworkReceive, NetworkSend};
use rs_matter::utils::buf::PooledBuffers;
use rs_matter::utils::epoch::Epoch;
use rs_matter::utils::rand::Rand;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::signal::Signal;
use rs_matter::{CommissioningData, Matter, MATTER_PORT};

use crate::netif::{Netif, NetifConf};
use crate::network::Network;
use crate::persist::Persist;

pub use eth::*;
pub use wifible::*;

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

mod eth;
#[cfg(feature = "edge-mdns")]
pub mod mdns;
pub mod modem;
pub mod netif;
pub mod network;
pub mod persist;
mod udp;
pub mod wifi;
mod wifible;

const MAX_SUBSCRIPTIONS: usize = 3;
const MAX_IM_BUFFERS: usize = 10;
const MAX_RESPONDERS: usize = 4;
const MAX_BUSY_RESPONDERS: usize = 2;

/// An enum modeling the mDNS service to be used.
#[derive(Copy, Clone, Default)]
pub enum MdnsType<'a> {
    /// The mDNS service provided by the `rs-matter` crate.
    #[default]
    Builtin,
    /// User-provided mDNS service.
    Provided(&'a dyn Mdns),
}

impl<'a> MdnsType<'a> {
    pub const fn default() -> Self {
        Self::Builtin
    }

    pub const fn mdns_service(&self) -> MdnsService<'a> {
        match self {
            MdnsType::Builtin => MdnsService::Builtin,
            MdnsType::Provided(mdns) => MdnsService::Provided(*mdns),
        }
    }
}

impl<'a> From<MdnsType<'a>> for MdnsService<'a> {
    fn from(value: MdnsType<'a>) -> Self {
        value.mdns_service()
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
    mdns: MdnsType<'a>,
    netif_conf: Signal<NoopRawMutex, Option<NetifConf>>,
}

impl<'a, N> MatterStack<'a, N>
where
    N: Network,
{
    /// Create a new `MatterStack` instance.
    #[cfg(feature = "std")]
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    pub const fn new_default(
        dev_det: &'a BasicInfoConfig,
        dev_att: &'a dyn DevAttDataFetcher,
    ) -> Self {
        Self::new(
            dev_det,
            dev_att,
            MdnsType::default(),
            rs_matter::utils::epoch::sys_epoch,
            rs_matter::utils::rand::sys_rand,
        )
    }

    /// Create a new `MatterStack` instance.
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    pub const fn new(
        dev_det: &'a BasicInfoConfig,
        dev_att: &'a dyn DevAttDataFetcher,
        mdns: MdnsType<'a>,
        epoch: Epoch,
        rand: Rand,
    ) -> Self {
        Self {
            matter: Matter::new(
                dev_det,
                dev_att,
                mdns.mdns_service(),
                epoch,
                rand,
                MATTER_PORT,
            ),
            buffers: PooledBuffers::new(0),
            subscriptions: Subscriptions::new(),
            network: N::INIT,
            mdns,
            netif_conf: Signal::new(None),
        }
    }

    /// A utility method to replace the initial mDNS implementation with another one.
    ///
    /// Useful in particular with `MdnsType::Provided()`, where the user would still like
    /// to create the `MatterStack` instance in a const-context, as in e.g.:
    /// `const Stack: MatterStack<'static, ...> = MatterStack::new(...);`
    ///
    /// The above const-creation is incompatible with `MdnsType::Provided()` which carries a
    /// `&dyn Mdns` pointer, which cannot be initialized from within a const context with anything
    /// else than a `const`. (At least not yet - there is an unstable nightly Rust feature for that).
    ///
    /// The solution is to const-construct the `MatterStack` object with `MdnsType::Disabled`, and
    /// after that - while/if we still have exclusive, mutable access to the `MatterStack` object -
    /// replace the `MdnsType::Disabled` initial impl with another, like `MdnsType::Provided`.
    pub fn replace_mdns(&mut self, mdns: MdnsType<'a>) {
        self.mdns = mdns;
        self.matter.replace_mdns(mdns.mdns_service());
    }

    /// A utility method to replace the initial Device Attestation Data Fetcher with another one.
    ///
    /// Reasoning and use-cases explained in the documentation of `replace_mdns`.
    pub fn replace_dev_att(&mut self, dev_att: &'a dyn DevAttDataFetcher) {
        self.matter.replace_dev_att(dev_att);
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
    pub async fn get_netif_conf(&self) -> Option<NetifConf> {
        self.netif_conf
            .wait(|netif_conf| Some(netif_conf.clone()))
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
        self.netif_conf
            .wait(|netif_info| (netif_info.as_ref() != prev_netif_info).then(|| netif_info.clone()))
            .await
    }

    /// This method is a specialization of `run_with_transport` over the UDP transport (both IPv4 and IPv6).
    /// It calls `run_with_transport` and in parallel runs the mDNS service.
    ///
    /// The netif instance is necessary, so that the loop can monitor the network and bring up/down
    /// the main UDP transport and the mDNS service when the netif goes up/down or changes its IP addresses.
    ///
    /// Parameters:
    /// - `persist` - a user-provided `Persist` implementation
    /// - `netif` - a user-provided `Netif` implementation
    /// - `dev_comm` - the commissioning data and discovery capabilities
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run_with_netif<'d, H, P, I, U>(
        &self,
        mut persist: P,
        netif: I,
        dev_comm: Option<(CommissioningData, DiscoveryCapabilities)>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
        P: Persist,
        I: Netif + UdpBind,
        U: Future<Output = Result<(), Error>>,
    {
        let mut user = pin!(user);

        loop {
            info!("Waiting for the network to come up...");

            let reset_netif_info = || {
                self.netif_conf.modify(|global_netif_info| {
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

            let netif_conf = loop {
                if let Some(netif_conf) = netif.get_conf().await? {
                    break netif_conf;
                }

                netif.wait_conf_change().await?;
            };

            info!("Got IP network: {netif_conf}");

            self.netif_conf.modify(|global_ip_info| {
                *global_ip_info = Some(netif_conf.clone());

                (true, ())
            });

            let mut socket = netif
                .bind(SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::UNSPECIFIED,
                    MATTER_PORT,
                    0,
                    netif_conf.interface,
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
            let mut mdns = pin!(self.run_builtin_mdns(&netif, &netif_conf));
            let mut down = pin!(async {
                loop {
                    let next = netif.get_conf().await?;
                    let next = next.as_ref();

                    if Some(&netif_conf) != next {
                        break;
                    }

                    netif.wait_conf_change().await?;
                }

                Ok(())
            });

            select4(&mut main, &mut mdns, &mut down, &mut user)
                .coalesce()
                .await?;

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
            persist.run().await
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

    async fn run_builtin_mdns<I>(&self, _netif: &I, _netif_conf: &NetifConf) -> Result<(), Error>
    where
        I: UdpBind,
    {
        if matches!(self.mdns, MdnsType::Builtin) {
            #[cfg(not(all(
                feature = "std",
                any(target_os = "macos", all(feature = "zeroconf", target_os = "linux"))
            )))]
            {
                use core::fmt::Write as _;

                use {edge_nal::MulticastV4, edge_nal::MulticastV6};

                use rs_matter::mdns::{
                    Host, MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_PORT,
                };

                let mut socket = _netif
                    .bind(SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::UNSPECIFIED,
                        MDNS_PORT,
                        0,
                        _netif_conf.interface,
                    )))
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;

                socket
                    .join_v4(MDNS_IPV4_BROADCAST_ADDR, _netif_conf.ipv4)
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;
                socket
                    .join_v6(MDNS_IPV6_BROADCAST_ADDR, _netif_conf.interface)
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;

                let (recv, send) = socket.split();

                let mut hostname = heapless::String::<12>::new();
                write!(
                    hostname,
                    "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                    _netif_conf.mac[0],
                    _netif_conf.mac[1],
                    _netif_conf.mac[2],
                    _netif_conf.mac[3],
                    _netif_conf.mac[4],
                    _netif_conf.mac[5]
                )
                .unwrap();

                self.matter()
                    .run_builtin_mdns(
                        udp::Udp(send),
                        udp::Udp(recv),
                        &Host {
                            id: 0,
                            hostname: &hostname,
                            ip: _netif_conf.ipv4.octets(),
                            ipv6: Some(_netif_conf.ipv6.octets()),
                        },
                        Some(_netif_conf.interface),
                    )
                    .await?;
            }

            #[cfg(all(
                feature = "std",
                any(target_os = "macos", all(feature = "zeroconf", target_os = "linux"))
            ))]
            core::future::pending::<()>().await;
        } else {
            core::future::pending::<()>().await;
        }

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
