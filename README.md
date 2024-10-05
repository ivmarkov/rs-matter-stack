# (WIP) Easily configure and run [rs-matter](https://github.com/project-chip/rs-matter)

[![CI](https://github.com/ivmarkov/rs-matter-stack/actions/workflows/ci.yml/badge.svg)](https://github.com/ivmarkov/rs-matter-stack/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/rs-matter-stack.svg)](https://crates.io/crates/rs-matter-stack)
[![Matrix](https://img.shields.io/matrix/matter-rs:matrix.org?label=join%20matrix&color=BEC5C9&logo=matrix)](https://matrix.to/#/#matter-rs:matrix.org)

## Overview

Configuring the [`rs-matter`](https://github.com/project-chip/rs-matter) crate is not trivial, as it is more of a toolkit rather than a monolitic all-in-one runtime.

Furthermore, _operating_ the assembled Matter stack is also challenging, as various features might need to be switched on or off depending on whether Matter is running in commissioning or operating mode, and also depending on the current network connectivity (as in e.g. Wifi signal lost).

**This crate addresses these issues by providing an all-in-one [`MatterStack`](https://github.com/ivmarkov/rs-matter-stack/blob/master/src/lib.rs) assembly that configures `rs-matter` for reliable operation.**

Instantiate it and then call `MatterStack::<...>::run(...)`.

## OK, but what would I sacrifice?

**Flexibility**.

The Matter stack is assembled as one large future which is not `Send`. Using an executor to poll that future together with others is still possible, but the executor should be a local one (i.e. Tokio's `LocalSet`, `async_executor::LocalExecutor` and so on).

## The examples are STD-only?

The core of `rs-matter-stack` is `no_std` and no-`alloc`.

You need to provide platform-specific implementations of the following traits for your embedded platform:
- `Persist` - non-volatile storage abstraction. Easiest is to implement `KvBlobStore` instead, and then use it with the provided `KvPersist` utility. 
  - For STD, `rs-matter-stack` provides `DirKvBlobStore`.
- `Netif` - network interface abstraction (i.e. monitoring when the network interface is up or down, and what is its IP configuration).
  - For Unix-like OSes, `rs-matter-stack` provides `UnixNetif`, which uses a simple polling every 2 seconds to detect changes to the network interface.
  - Note that For IP (TCP & UDP) IO, the stack uses the [`edge-nal`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal) crate, and is thus compatible with [`STD`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-std) and [`Embassy`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-embassy) out of the box. You only need to worry about networking IO if you use other platforms than these two.
- `Ble` - BLE abstraction of the device radio. Not necessary for Ethernet connectivity
  - For Linux, `rs-matter-stack` provides `BuiltinBle`, which uses the Linux BlueZ BT stack.
- `Wireless` - Wireless (Wifi or Thread) abstraction of the device radio. Not necessary for Ethernet connectivity.
  - `DummyWireless` is a no-op wireless implementation that is useful for testing. I.e. on Linux, one can use `DummyWireless` together with `BuiltinBle` and `UnixNetif` to test the stack in wireless mode. For production embedded Linux use-cases, you'll have to provide a true `Wireless` implementation, possibly based on WPA Supplicant, or NetworkManager (not available out of the box in `rs-matter-stack` yet).

## ESP-IDF

The [`esp-idf-matter`](https://github.com/ivmarkov/esp-idf-matter) crate provides implementations for `Persist`, `Netif`, `Ble` and `Wireless` for the ESP-IDF SDK.

## Embassy

TBD - upcoming!

## Example

(See also [All examples](#all-examples))

```rust
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

use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use env_logger::Target;
use log::info;

use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::cluster_on_off;
use rs_matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter::data_model::objects::{Dataver, Endpoint, HandlerCompat, Node};
use rs_matter::data_model::system_model::descriptor;
use rs_matter::error::Error;
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::utils::select::Coalesce;
use rs_matter::BasicCommData;

use rs_matter_stack::netif::UnixNetif;
use rs_matter_stack::persist::{new_kv, DirKvBlobStore, KvBlobBuf};
use rs_matter_stack::EthMatterStack;

use static_cell::StaticCell;

#[path = "dev_att/dev_att.rs"]
mod dev_att;

fn main() -> Result<(), Error> {
    env_logger::Builder::from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    )
    .target(Target::Stdout)
    .init();

    info!("Starting...");

    // Initialize the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK
        .uninit()
        .init_with(EthMatterStack::init_default(
            &BasicInfoConfig {
                vid: 0xFFF1,
                pid: 0x8001,
                hw_ver: 2,
                sw_ver: 1,
                sw_ver_str: "1",
                serial_no: "aabbccdd",
                device_name: "MyLight",
                product_name: "ACME Light",
                vendor_name: "ACME",
            },
            BasicCommData {
                password: 20202021,
                discriminator: 3840,
            },
            &DEV_ATT,
        ));

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::data_model::AsyncHandler`
    let on_off = cluster_on_off::OnOffCluster::new(Dataver::new_rand(stack.matter().rand()));

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
            HandlerCompat(descriptor::DescriptorCluster::new(Dataver::new_rand(
                stack.matter().rand(),
            ))),
        );

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let mut matter = pin!(stack.run(
        // Will try to find a default network interface
        UnixNetif::default(),
        // Will persist in `<tmp-dir>/rs-matter`
        new_kv(DirKvBlobStore::new_default(), stack),
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user future to run
        core::future::pending(),
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
static MATTER_STACK: StaticCell<EthMatterStack<KvBlobBuf<()>>> = StaticCell::new();

const DEV_ATT: dev_att::HardCodedDevAtt = dev_att::HardCodedDevAtt::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EthMatterStack::<KvBlobBuf<()>>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: &[DEV_TYPE_ON_OFF_LIGHT],
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};
```

## All examples

To build all examples, use:

```
cargo build --examples --features os,nix
```
