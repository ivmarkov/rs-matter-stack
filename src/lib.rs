#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![allow(clippy::uninlined_format_args)]
#![warn(clippy::large_futures)]
#![warn(clippy::large_stack_frames)]
#![warn(clippy::large_types_passed_by_value)]

use core::future::Future;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;

use edge_nal::{UdpBind, UdpSplit};

use embassy_futures::select::{select4, Either4};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use persist::{KvBlobBuffer, KvBlobStore, MatterPersist, NetworkPersist};
use rs_matter::data_model::basic_info::BasicInfoConfig;
use rs_matter::data_model::core::IMBuffer;
use rs_matter::data_model::networks::NetChangeNotif;
use rs_matter::data_model::objects::{AsyncHandler, AsyncMetadata};
use rs_matter::data_model::sdm::dev_att::DevAttDataFetcher;
use rs_matter::data_model::sdm::gen_diag::NetifDiag;
use rs_matter::data_model::subscriptions::Subscriptions;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::mdns::{Mdns, MdnsService};
use rs_matter::respond::DefaultResponder;
use rs_matter::transport::network::{Address, ChainedNetwork, NetworkReceive, NetworkSend};
use rs_matter::utils::epoch::Epoch;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::rand::Rand;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::{BasicCommData, Matter, MATTER_PORT};

use crate::network::Network;
use crate::persist::SharedKvBlobStore;

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod eth;
pub mod matter;
pub mod mdns;
pub mod nal;
pub mod network;
pub mod persist;
pub mod rand;
pub mod udp;
pub mod utils;
pub mod wireless;

mod private {
    /// A marker super-trait for sealed traits
    pub trait Sealed {}

    impl Sealed for () {}
}

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
    store_buf: PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
    #[allow(unused)]
    network: N,
    #[allow(unused)]
    mdns: MdnsType<'a>,
    //netif_conf: Signal<NoopRawMutex, Option<NetifConf>>,
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
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
    ) -> Self {
        Self::new(
            dev_det,
            dev_comm,
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
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
        mdns: MdnsType<'a>,
        epoch: Epoch,
        rand: Rand,
    ) -> Self {
        Self {
            matter: Matter::new(
                dev_det,
                dev_comm,
                dev_att,
                mdns.mdns_service(),
                epoch,
                rand,
                MATTER_PORT,
            ),
            buffers: PooledBuffers::new(0),
            subscriptions: Subscriptions::new(),
            store_buf: PooledBuffers::new(0),
            network: N::INIT,
            mdns,
            //netif_conf: Signal::new(None),
        }
    }

    /// Create a new `MatterStack` instance.
    #[cfg(feature = "std")]
    #[allow(clippy::large_stack_frames)]
    pub fn init_default(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
    ) -> impl Init<Self> {
        Self::init(
            dev_det,
            dev_comm,
            dev_att,
            MdnsType::default(),
            rs_matter::utils::epoch::sys_epoch,
            rs_matter::utils::rand::sys_rand,
        )
    }

    #[allow(clippy::large_stack_frames)]
    pub fn init(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
        mdns: MdnsType<'a>,
        epoch: Epoch,
        rand: Rand,
    ) -> impl Init<Self> {
        init!(Self {
            matter <- Matter::init(
                dev_det,
                dev_comm,
                dev_att,
                mdns.mdns_service(),
                epoch,
                rand,
                MATTER_PORT,
            ),
            buffers <- PooledBuffers::init(0),
            subscriptions <- Subscriptions::init(),
            store_buf <- PooledBuffers::init(0),
            network <- N::init(),
            mdns,
            //netif_conf: Signal::new(None),
        })
    }

    /// Create a new `SharedKvBlobStore` instance wrapping the
    /// provided `KvBlobStore` implementation and the internal store buffer
    /// available in the `MatterStack` instance.
    pub fn create_shared_store<S>(&self, store: S) -> SharedKvBlobStore<'_, S>
    where
        S: KvBlobStore,
    {
        SharedKvBlobStore::new(store, &self.store_buf)
    }

    /// Create a new `MatterPersist` instance for the Matter stack.
    fn create_persist<'t, S>(
        &'t self,
        store: &'t SharedKvBlobStore<'t, S>,
    ) -> MatterPersist<'t, S, N::PersistContext<'t>>
    where
        S: KvBlobStore,
    {
        MatterPersist::new(store, self.matter(), self.network().persist_context())
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

    pub const fn store_buf(&self) -> &PooledBuffers<1, NoopRawMutex, KvBlobBuffer> {
        &self.store_buf
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

    // /// User code hook to get the state of the netif passed to the
    // /// `run_with_netif` method.
    // ///
    // /// Useful when user code needs to bring up/down its own IP services depending on
    // /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    // pub async fn get_netif_conf(&self) -> Option<NetifConf> {
    //     self.netif_conf
    //         .wait(|netif_conf| Some(netif_conf.clone()))
    //         .await
    // }

    // fn update_netif_conf(&self, netif_conf: Option<&NetifConf>) -> bool {
    //     self.netif_conf.modify(|global_ip_info| {
    //         if global_ip_info.as_ref() != netif_conf {
    //             *global_ip_info = netif_conf.cloned();
    //             (true, true)
    //         } else {
    //             (false, false)
    //         }
    //     })
    // }

    // /// User code hook to detect changes to the IP state of the netif passed to the
    // /// `run_with_netif` method.
    // ///
    // /// Useful when user code needs to bring up/down its own IP services depending on
    // /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    // pub async fn wait_netif_changed(
    //     &self,
    //     prev_netif_info: Option<&NetifConf>,
    // ) -> Option<NetifConf> {
    //     self.netif_conf
    //         .wait(|netif_info| (netif_info.as_ref() != prev_netif_info).then(|| netif_info.clone()))
    //         .await
    // }

    /// Return information whether the Matter instance is already commissioned.
    pub async fn is_commissioned(&self) -> Result<bool, Error> {
        Ok(self.matter().is_commissioned())
    }

    /// This method is a specialization of `run_transport_net` over the UDP transport (both IPv4 and IPv6).
    /// It calls `run_transport_net` and in parallel runs the mDNS service.
    ///
    /// The netif instance is necessary, so that the loop can monitor the network and bring up/down
    /// the main UDP transport and the mDNS service when the netif goes up/down or changes its IP addresses.
    ///
    /// Parameters:
    /// - `netif` - a user-provided `Netif` implementation
    /// - `until` - the method will return once this future becomes ready
    /// - `comm` - a tuple of additional and optional `NetworkReceive` and `NetworkSend` transport implementations
    ///   (useful when a second transport needs to run in parallel with the operational Matter transport,
    ///   i.e. when using concurrent commissisoning)
    async fn run_oper_net<U, I, X, R, S>(
        &self,
        udp: U,
        netif: I,
        until: X,
        mut comm: Option<(R, S)>,
    ) -> Result<(), Error>
    where
        U: UdpBind,
        I: NetifDiag + NetChangeNotif,
        X: Future<Output = Result<(), Error>>,
        R: NetworkReceive,
        S: NetworkSend,
    {
        #[derive(Clone, Debug, Eq, PartialEq, Hash)]
        #[cfg_attr(feature = "defmt", derive(defmt::Format))]
        struct Netif {
            ipv6: Ipv6Addr,
            ipv4: Ipv4Addr,
            mac: [u8; 8],
            operational: bool,
        }

        impl Netif {
            pub const fn new() -> Self {
                Self {
                    ipv6: Ipv6Addr::UNSPECIFIED,
                    ipv4: Ipv4Addr::UNSPECIFIED,
                    mac: [0; 8],
                    operational: false,
                }
            }
        }

        fn load_netif<C>(net_ctl: C, netif: &mut Netif) -> Result<(), Error>
        where
            C: NetifDiag,
        {
            netif.operational = false;
            netif.ipv6 = Ipv6Addr::UNSPECIFIED;
            netif.ipv4 = Ipv4Addr::UNSPECIFIED;
            netif.mac = [0; 8];

            net_ctl.netifs(&mut |ni| {
                if ni.operational && !ni.ipv6_addrs.is_empty() {
                    netif.operational = true;
                    netif.ipv6 = ni.ipv6_addrs[0];
                    netif.ipv4 = ni
                        .ipv4_addrs
                        .first()
                        .copied()
                        .unwrap_or(Ipv4Addr::UNSPECIFIED);
                    netif.mac = *ni.hw_addr;
                }

                Ok(())
            })
        }

        async fn wait_changed<C>(
            net_ctl: C,
            cur_netif: &Netif,
            new_netif: &mut Netif,
        ) -> Result<(), Error>
        where
            C: NetifDiag + NetChangeNotif,
        {
            loop {
                load_netif(&net_ctl, new_netif)?;

                if &*new_netif != cur_netif {
                    trace!("Change detected: {:?}", new_netif);
                    break Ok(());
                }

                trace!("No change");
                net_ctl.wait_changed().await;
            }
        }

        let mut until_task = pin!(until);

        // let _guard = scopeguard::guard((), |_| {
        //     self.update_netif_conf(None);
        // });

        let mut new_netif = Netif::new();
        load_netif(&netif, &mut new_netif)?;

        loop {
            let cur_netif = new_netif.clone();

            let mut netif_changed_task = pin!(wait_changed(&netif, &cur_netif, &mut new_netif));

            let result = if cur_netif.operational {
                info!("Netif up: {:?}", cur_netif);

                let mut socket = udp
                    .bind(SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::UNSPECIFIED,
                        MATTER_PORT,
                        0,
                        0,
                    )))
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;

                let (recv, send) = socket.split();

                let mut mdns_task = pin!(self.run_builtin_mdns(
                    &udp,
                    &cur_netif.mac,
                    cur_netif.ipv4,
                    cur_netif.ipv6,
                    0,
                ));

                if let Some((comm_recv, comm_send)) = comm.as_mut() {
                    info!("Running operational and extra networks");

                    let mut netw_task = pin!(self.run_transport_net(
                        ChainedNetwork::new(Address::is_udp, udp::Udp(send), comm_send),
                        ChainedNetwork::new(Address::is_udp, udp::Udp(recv), comm_recv),
                    ));

                    select4(
                        &mut netw_task,
                        &mut mdns_task,
                        &mut netif_changed_task,
                        &mut until_task,
                    )
                    .await
                } else {
                    info!("Running operational network");

                    let mut netw_task =
                        pin!(self.run_transport_net(udp::Udp(send), udp::Udp(recv),));

                    select4(
                        &mut netw_task,
                        &mut mdns_task,
                        &mut netif_changed_task,
                        &mut until_task,
                    )
                    .await
                }
            } else if let Some((comm_recv, comm_send)) = comm.as_mut() {
                info!("Running commissioning network only");

                let mut netw_task = pin!(self.run_transport_net(comm_send, comm_recv));

                select4(
                    &mut netw_task,
                    core::future::pending(),
                    &mut netif_changed_task,
                    &mut until_task,
                )
                .await
            } else {
                info!("Netif down");

                select4(
                    core::future::pending(),
                    core::future::pending(),
                    &mut netif_changed_task,
                    &mut until_task,
                )
                .await
            };

            match result {
                Either4::Third(_) => info!("IP network change detected"),
                Either4::First(r) | Either4::Second(r) | Either4::Fourth(r) => break r,
            }
        }
    }

    async fn run_handler<H>(&self, handler: H) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        // TODO
        // Reset the Matter transport buffers and all sessions first
        // self.matter().reset_transport()?;

        self.run_responder(handler).await
    }

    async fn run_psm<S, C>(&self, persist: &MatterPersist<'_, S, C>) -> Result<(), Error>
    where
        S: KvBlobStore,
        C: NetworkPersist,
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

    async fn run_builtin_mdns<U>(
        &self,
        _udp: U,
        _mac: &[u8],
        _ipv4: Ipv4Addr,
        _ipv6: Ipv6Addr,
        _interface: u32,
    ) -> Result<(), Error>
    where
        U: UdpBind,
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

                let mut socket = _udp
                    .bind(SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::UNSPECIFIED,
                        MDNS_PORT,
                        0,
                        _interface,
                    )))
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;

                socket
                    .join_v4(MDNS_IPV4_BROADCAST_ADDR, _ipv4)
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;
                socket
                    .join_v6(MDNS_IPV6_BROADCAST_ADDR, _interface)
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;

                let (recv, send) = socket.split();

                let mut hostname = heapless::String::<16>::new();
                if _mac.len() == 6 {
                    write_unwrap!(
                        hostname,
                        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                        _mac[0],
                        _mac[1],
                        _mac[2],
                        _mac[3],
                        _mac[4],
                        _mac[5]
                    );
                } else if _mac.len() == 8 {
                    write_unwrap!(
                        hostname,
                        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                        _mac[0],
                        _mac[1],
                        _mac[2],
                        _mac[3],
                        _mac[4],
                        _mac[5],
                        _mac[6],
                        _mac[7]
                    );
                } else {
                    panic!("Invalid MAC address length: should be 6 or 8 bytes");
                }

                self.matter()
                    .run_builtin_mdns(
                        udp::Udp(send),
                        udp::Udp(recv),
                        &Host {
                            id: 0,
                            hostname: &hostname,
                            ip: _ipv4,
                            ipv6: _ipv6,
                        },
                        Some(_ipv4),
                        Some(_interface),
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

    async fn run_transport_net<S, R>(&self, send: S, recv: R) -> Result<(), Error>
    where
        S: NetworkSend,
        R: NetworkReceive,
    {
        self.matter().run_transport(send, recv).await?;

        Ok(())
    }
}

/// A trait representing a user task that needs access to the operational network interface
/// (Netif and UDP stack) to perform its work.
///
/// Note that the task would be started only when `rs-matter`
/// brings up the operational interface (eth, wifi or thread)
/// and if the interface goes down, the user task would be stopped.
/// Upon re-connection, the task would be started again.
pub trait UserTask {
    /// Run the task with the given network interface and UDP stack
    async fn run<U, N>(&mut self, udp: U, netif: N) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif;
}

impl<T> UserTask for &mut T
where
    T: UserTask,
{
    async fn run<U, N>(&mut self, udp: U, netif: N) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
    {
        (*self).run(udp, netif).await
    }
}

impl UserTask for () {
    async fn run<U, N>(&mut self, _udp: U, _netif: N) -> Result<(), Error>
    where
        U: UdpBind,
        N: NetifDiag + NetChangeNotif,
    {
        core::future::pending::<()>().await;

        Ok(())
    }
}
