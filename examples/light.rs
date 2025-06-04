//! An example utilizing the `WifiMatterStack` struct.
//!
//! As the name suggests, this Matter stack assembly uses Wifi as the main transport
//! (and thus also BLE for commissioning).
//!
//! If you want to use Ethernet, utilize `EthMatterStack` instead.
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).

use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use log::info;

use rs_matter::data_model::networks::unix::UnixNetifs;
use rs_matter::data_model::networks::wireless::NoopWirelessNetCtl;
use rs_matter::data_model::objects::EmptyHandler;
use rs_matter::data_model::sdm::net_comm::NetworkType;
use rs_matter::test_device::TEST_DEV_DET;
use rs_matter_stack::matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_stack::matter::data_model::objects::{Async, Dataver, Endpoint, Node};
use rs_matter_stack::matter::data_model::on_off;
use rs_matter_stack::matter::data_model::on_off::{ClusterHandler as _, OnOffHandler};
use rs_matter_stack::matter::data_model::system_model::desc::{ClusterHandler as _, DescHandler};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::test_device::{TEST_DEV_ATT, TEST_DEV_COMM};
use rs_matter_stack::matter::transport::network::btp::BuiltinGattPeripheral;
use rs_matter_stack::matter::utils::init::InitMaybeUninit;
use rs_matter_stack::matter::utils::select::Coalesce;
use rs_matter_stack::matter::utils::sync::blocking::raw::StdRawMutex;
use rs_matter_stack::matter::{clusters, devices};
use rs_matter_stack::persist::DirKvBlobStore;
use rs_matter_stack::wireless::PreexistingWireless;
use rs_matter_stack::wireless::WifiMatterStack;

use static_cell::StaticCell;

fn main() -> Result<(), Error> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

    info!("Starting...");

    // Initialize the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK
        .uninit()
        .init_with(WifiMatterStack::init_default(
            &TEST_DEV_DET,
            TEST_DEV_COMM,
            &TEST_DEV_ATT,
        ));

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = on_off::OnOffHandler::new(Dataver::new_rand(stack.matter().rand()));

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = EmptyHandler
        // Our on-off cluster, on Endpoint 1
        .chain(
            LIGHT_ENDPOINT_ID,
            OnOffHandler::CLUSTER.id,
            Async(on_off::HandlerAdaptor(&on_off)),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            LIGHT_ENDPOINT_ID,
            DescHandler::CLUSTER.id,
            Async(DescHandler::new(Dataver::new_rand(stack.matter().rand())).adapt()),
        );

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let store = stack.create_shared_store(DirKvBlobStore::new_default());
    let mut matter = pin!(stack.run(
        PreexistingWireless::new(
            edge_nal_std::Stack::new(),
            UnixNetifs,
            // A dummy wireless controller that does nothing
            NoopWirelessNetCtl::new(NetworkType::Wifi),
            BuiltinGattPeripheral::new(None),
        ),
        // Will persist in `<tmp-dir>/rs-matter`
        &store,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user task to run
        (),
    ));

    // Just for demoing purposes:
    //
    // Run a sample loop that simulates state changes triggered by the HAL
    // Changes will be properly communicated to the Matter controllers
    // (i.e. Google Home, Alexa) and other Matter devices thanks to subscriptions
    let mut device = pin!(async {
        loop {
            // Simulate user toggling the light with a physical switch every 5 seconds
            Timer::after(Duration::from_secs(5)).await;

            // Toggle
            on_off.set(!on_off.get());

            // Let the Matter stack know that we have changed
            // the state of our Light device
            stack.notify_changed();

            info!("Light toggled");
        }
    });

    // Schedule the Matter run & the device loop together
    futures_lite::future::block_on(async_compat::Compat::new(
        select(&mut matter, &mut device).coalesce(),
    ))
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
/// It is also a mandatory requirement when the `WifiBle` stack variation is used.
static MATTER_STACK: StaticCell<WifiMatterStack<StdRawMutex>> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        WifiMatterStack::<StdRawMutex, ()>::root_endpoint(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters: clusters!(DescHandler::CLUSTER, OnOffHandler::CLUSTER),
        },
    ],
};
