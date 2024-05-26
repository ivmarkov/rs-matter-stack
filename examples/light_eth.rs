//! An example utilizing the `EthMatterStack` struct.
//! As the name suggests, this Matter stack assembly uses Ethernet as the main transport,
//! as well as for commissioning.
//!
//! Notice that it might be that rather than Ethernet, the actual L2 transport is Wifi.
//! From the POV of Matter - this case is indistinguishable from Ethernet as long as the
//! Matter stack is not concerned with connecting to the Wifi network, managing
//! its credentials etc. and can assume it "pre-exists".
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).
//!
//! Note that there is no real persistence in this example, so the device will always
//! start in commissioning mode.
//! Note also that the network interface is not monitored for connectivity changes,
//! so the device will always assume it is connected.

use core::borrow::Borrow;
use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use log::info;

use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::cluster_on_off;
use rs_matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter::data_model::objects::{Endpoint, HandlerCompat, Node};
use rs_matter::data_model::system_model::descriptor;
use rs_matter::secure_channel::spake2p::VerifierData;
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use rs_matter_stack::netif::DummyNetif;
use rs_matter_stack::persist::DummyPersist;
use rs_matter_stack::{Error, EthMatterStack};

use static_cell::ConstStaticCell;

#[path = "dev_att/dev_att.rs"]
mod dev_att;

fn main() -> Result<(), Error> {
    info!("Starting...");

    // Take the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK.take();

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(*stack.matter().borrow());

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = stack
        .root_handler()
        // Our on-off cluster, on Endpoint 1
        .chain(
            LIGHT_ENDPOINT_ID,
            cluster_on_off::ID,
            HandlerCompat(&on_off),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            LIGHT_ENDPOINT_ID,
            descriptor::ID,
            HandlerCompat(descriptor::DescriptorCluster::new(*stack.matter().borrow())),
        );

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let mut matter = pin!(stack.run(
        // Will not persist anything
        DummyPersist::default(),
        // Will use the default network interface and will assume it is always up
        DummyNetif::default(),
        // Hard-coded for demo purposes
        CommissioningData {
            verifier: VerifierData::new_with_pw(123456, *stack.matter().borrow()),
            discriminator: 250,
        },
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
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
    futures_lite::future::block_on(select(&mut matter, &mut device).coalesce())
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
static MATTER_STACK: ConstStaticCell<EthMatterStack<()>> =
    ConstStaticCell::new(EthMatterStack::new_default(
        &BasicInfoConfig {
            vid: 0xFFF1,
            pid: 0x8000,
            hw_ver: 2,
            sw_ver: 1,
            sw_ver_str: "1",
            serial_no: "aabbccdd",
            device_name: "MyLight",
            product_name: "ACME Light",
            vendor_name: "ACME",
        },
        &dev_att::HardCodedDevAtt::new(),
    ));

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EthMatterStack::<()>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_type: DEV_TYPE_ON_OFF_LIGHT,
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};
