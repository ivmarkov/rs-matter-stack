//! An alternative implementation of the `rs-matter` `Mdns` trait based on `edge-mdns`.
//!
//! `rs-matter` does have a built-in mDNS imlementation, and that implementation is
//! in fact primarily maintained by the same author who maintains `edge-mdns`.
//!
//! However, the key difference between the two is that the `rs-matter` built-in mDNS
//! implementation - _for now_ -_only_ responds to queries which concern the Matter
//! protocol itself and also does not expose a query interface. This makes it unsuitable
//! for use in cases where the same host that operates an `rs-matter` stack needs to -
//! for whatever reasons - to host additional service types different than the ones
//! concerning Matter, and/or issue ad-hoc mDNS queries outside the queries necessary
//! for operating the Matter stack.
//!
//! Using `edge-mdns` solves this problem by providing a general-purpose mDNS which can be
//! shared between the `rs-matter` stack and other - user-specific use cases.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::signal::Signal;

use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::mdns::{Mdns, Service, ServiceMode};
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::sync::blocking::Mutex;

const MAX_MATTER_SERVICES: usize = 4;
const MAX_MATTER_SERVICE_NAME_LEN: usize = 40;

/// An adaptor from `rs-matter` buffers to `edge-mdns` buffers.
#[cfg(feature = "edge-mdns")]
pub struct MatterBuffer<B>(B);

#[cfg(feature = "edge-mdns")]
impl<B> MatterBuffer<B> {
    /// Create a new instance of `MatterBuffer`
    pub const fn new(buffer: B) -> Self {
        Self(buffer)
    }
}

#[cfg(feature = "edge-mdns")]
impl<B, T> edge_mdns::buf::BufferAccess<T> for MatterBuffer<B>
where
    B: rs_matter::utils::storage::pooled::BufferAccess<T>,
    T: ?Sized,
{
    type Buffer<'a>
        = B::Buffer<'a>
    where
        Self: 'a;

    async fn get(&self) -> Option<Self::Buffer<'_>> {
        self.0.get().await
    }
}

/// An adaptor struct that does two things:
/// - Implements the `rs-matter` `Mdns` trait and thus can be used as an mDNS implementation
///   for `rs-matter` with e.g. `MdnsService::Provided(&matter_services)`
/// - Implements a publc visitor method - `visit_services` - that represents all services
///   registered by `rs-matter` via the `Mdns` trait
///   With this in-place, e.g. `edge-mdns` can easily respond to queries concerning `rs-matter`
///   services by e.g. using the `HostAnswersMdnsHandler` struct and the `ServiceAnswers` adaptor.
pub struct MatterMdnsServices<'a, M>
where
    M: RawMutex,
{
    dev_det: &'a BasicInfoConfig<'a>,
    matter_port: u16,
    services: Mutex<
        M,
        RefCell<
            rs_matter::utils::storage::Vec<
                (heapless::String<MAX_MATTER_SERVICE_NAME_LEN>, ServiceMode),
                MAX_MATTER_SERVICES,
            >,
        >,
    >,
    broadcast_signal: Signal<M, ()>,
}

impl<'a, M> MatterMdnsServices<'a, M>
where
    M: RawMutex,
{
    /// Create a new instance of `MatterServices`
    #[inline(always)]
    pub const fn new(dev_det: &'a BasicInfoConfig<'a>, matter_port: u16) -> Self {
        Self {
            dev_det,
            matter_port,
            services: Mutex::new(RefCell::new(rs_matter::utils::storage::Vec::new())),
            broadcast_signal: Signal::new(),
        }
    }

    /// Create an in-place initializer for `MatterServices`
    pub fn init(dev_det: &'a BasicInfoConfig<'a>, matter_port: u16) -> impl Init<Self> {
        init!(Self {
            dev_det,
            matter_port,
            services <- Mutex::init(RefCell::init(rs_matter::utils::storage::Vec::init())),
            broadcast_signal: Signal::new(),
        })
    }

    fn reset(&self) {
        self.services.lock(|services| {
            services.borrow_mut().clear();

            self.broadcast_signal.signal(());
        });
    }

    fn add(&self, service: &str, mode: ServiceMode) -> Result<(), Error> {
        self.services.lock(|services| {
            let mut services = services.borrow_mut();

            services.retain(|(name, _)| name != service);
            services
                .push((unwrap!(service.try_into()), mode))
                .map_err(|_| ErrorCode::NoSpace)?;

            self.broadcast_signal.signal(());

            Ok(())
        })
    }

    fn remove(&self, service: &str) -> Result<(), Error> {
        self.services.lock(|services| {
            let mut services = services.borrow_mut();

            services.retain(|(name, _)| name != service);

            self.broadcast_signal.signal(());

            Ok(())
        })
    }

    pub fn broadcast_signal(&self) -> &Signal<M, ()> {
        &self.broadcast_signal
    }

    /// Visit all services registered by `rs-matter` via the `Mdns` trait.
    pub fn visit_services<T>(&self, mut visitor: T) -> Result<(), Error>
    where
        T: FnMut(&ServiceMode, &Service) -> Result<(), Error>,
    {
        self.services.lock(|services| {
            let services = services.borrow();

            for (service, mode) in &*services {
                mode.service(self.dev_det, self.matter_port, service, |service| {
                    visitor(mode, service)
                })?;
            }

            Ok(())
        })
    }

    /// Visit all services registered by `rs-matter` via the `Mdns` trait as `edge_mdns::host::Service` instances.
    #[cfg(feature = "edge-mdns")]
    pub fn visit_emdns_services<T>(&self, mut visitor: T) -> Result<(), Error>
    where
        T: FnMut(&edge_mdns::host::Service) -> Result<(), Error>,
    {
        self.visit_services(|_, service| {
            let service = edge_mdns::host::Service {
                name: service.name,
                service: service.service,
                protocol: service.protocol,
                port: service.port,
                service_subtypes: service.service_subtypes,
                txt_kvs: service.txt_kvs,
                priority: 0,
                weight: 0,
            };

            visitor(&service)
        })
    }
}

impl<M> Mdns for MatterMdnsServices<'_, M>
where
    M: RawMutex,
{
    fn reset(&self) {
        MatterMdnsServices::reset(self)
    }

    fn add(&self, service: &str, mode: ServiceMode) -> Result<(), Error> {
        MatterMdnsServices::add(self, service, mode)
    }

    fn remove(&self, service: &str) -> Result<(), Error> {
        MatterMdnsServices::remove(self, service)
    }
}
