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

use edge_mdns::host::Service;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::signal::Signal;

use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::mdns::{Mdns, ServiceMode};
use rs_matter::utils::blmutex::Mutex;
use rs_matter::utils::buf::BufferAccess;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::refcell::RefCell;

const MAX_MATTER_SERVICES: usize = 4;
const MAX_MATTER_SERVICE_NAME_LEN: usize = 40;

/// An adaptor from `rs-matter` buffers to `edge-mdns` buffers.
pub struct MatterBuffer<B>(B);

impl<B> MatterBuffer<B> {
    /// Create a new instance of `MatterBuffer`
    pub const fn new(buffer: B) -> Self {
        Self(buffer)
    }
}

impl<B, T> edge_mdns::buf::BufferAccess<T> for MatterBuffer<B>
where
    B: BufferAccess<T>,
    T: ?Sized,
{
    type Buffer<'a> = B::Buffer<'a> where Self: 'a;

    async fn get(&self) -> Option<Self::Buffer<'_>> {
        self.0.get().await
    }
}

/// An adaptor struct that does two things:
/// - Implements the `rs-matter` `Mdns` trait and thus can be used as an mDNS implementation
///   for `rs-matter` with e.g. `MdnsService::Provided(&matter_services)`
/// - Implements a publc visitor method - `visit_services` - that represents all services
///   registered by `rs-matter` via the `Mdns` trait as `edge-net::host::Service` instances.
///   With this in-place, `edge-mdns` can easily respond to queries concerning `rs-matter`
///   services by e.g. using the `HostAnswersMdnsHandler` struct and the `ServiceAnswers` adaptor.
pub struct MatterServices<'a, M>
where
    M: RawMutex,
{
    dev_det: &'a BasicInfoConfig<'a>,
    matter_port: u16,
    services: Mutex<
        M,
        RefCell<
            rs_matter::utils::vec::Vec<
                (heapless::String<MAX_MATTER_SERVICE_NAME_LEN>, ServiceMode),
                MAX_MATTER_SERVICES,
            >,
        >,
    >,
    broadcast_signal: Signal<M, ()>,
}

impl<'a, M> MatterServices<'a, M>
where
    M: RawMutex,
{
    /// Create a new instance of `MatterServices`
    #[inline(always)]
    pub const fn new(dev_det: &'a BasicInfoConfig<'a>, matter_port: u16) -> Self {
        Self {
            dev_det,
            matter_port,
            services: Mutex::new(RefCell::new(rs_matter::utils::vec::Vec::new())),
            broadcast_signal: Signal::new(),
        }
    }

    /// Create an in-place initializer for `MatterServices`
    pub fn init(dev_det: &'a BasicInfoConfig<'a>, matter_port: u16) -> impl Init<Self> {
        init!(Self {
            dev_det,
            matter_port,
            services <- Mutex::init(RefCell::init(rs_matter::utils::vec::Vec::init())),
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
                .push((service.try_into().unwrap(), mode))
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

    /// Visit all services registered by `rs-matter` via the `Mdns` trait as `edge-net::host::Service` instances.
    pub fn visit_services<T>(&self, mut visitor: T) -> Result<(), Error>
    where
        T: FnMut(&Service) -> Result<(), Error>,
    {
        self.services.lock(|services| {
            let services = services.borrow();

            for (service, mode) in &*services {
                mode.service(
                    self.dev_det,
                    self.matter_port,
                    service,
                    |matter_service: &rs_matter::mdns::Service| {
                        let service = Service {
                            name: matter_service.name,
                            service: matter_service.service,
                            protocol: matter_service.protocol,
                            port: matter_service.port,
                            service_subtypes: matter_service.service_subtypes,
                            txt_kvs: matter_service.txt_kvs,
                            priority: 0,
                            weight: 0,
                        };

                        visitor(&service)
                    },
                )?;
            }

            Ok(())
        })
    }
}

impl<'a, M> Mdns for MatterServices<'a, M>
where
    M: RawMutex,
{
    fn reset(&self) {
        MatterServices::reset(self)
    }

    fn add(&self, service: &str, mode: ServiceMode) -> Result<(), Error> {
        MatterServices::add(self, service, mode)
    }

    fn remove(&self, service: &str) -> Result<(), Error> {
        MatterServices::remove(self, service)
    }
}
