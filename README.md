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

The Matter stack is assembled as one large future which is not `Send`. Using an executor to poll that future together with others is still possible, but the executor should be a local one (i.e. Embassy's `embassy-executor`, Tokio's `LocalSet`, `async_executor::LocalExecutor` and so on).

## The examples are STD-only?

The core of `rs-matter-stack` is `no_std` and no-`alloc`.

You need to provide platform-specific implementations of the following traits for your embedded platform:
- `KvBlobStore` - non-volatile key-value storage abstraction.
  - For STD, `rs-matter-stack` provides `DirKvBlobStore`.
- `Netif` - network interface abstraction (i.e. monitoring when the network interface is up or down, and what is its IP configuration).
  - For Unix-like OSes, `rs-matter-stack` provides `UnixNetif`, which uses a simple polling every 2 seconds to detect changes to the network interface.
  - Note that For IP (TCP & UDP) IO, the stack uses the [`edge-nal`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal) crate, and is thus compatible with [`STD`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-std) and [`Embassy`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-embassy) out of the box. You only need to worry about networking IO if you use other platforms than these two.
- Implementation of the UDP traits from [edge-nal](https://github.com/ivmarkov/edge-net/tree/master/edge-nal).
  - There are out-of-the-box implementations for [Rust STD BSD sockets](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-std) as well as for [`embassy-net`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-embassy) and for [OpenThread](https://github.com/ivmarkov/esp-openthread/blob/main/openthread/src/enal.rs).
- `Gatt` - BLE GATT peripheral abstraction of the device radio. Not necessary for Ethernet connectivity
  - For Linux, `rs-matter-stack` provides `BuiltinBle`, which uses the Linux BlueZ BT stack.
- `Controller` - Wifi controller implementation when using Wifi connectivity (Thread has a built-in one in OpenThread).
  - `DisconnectedController` is a no-op wireless implementation of a Wifi controller that is useful for testing. I.e. on Linux, one can use `PreexistingWireless` + `DisconnectedController` together with `BuiltinBle` and `UnixNetif` to test the stack in wireless mode. For production embedded Linux use-cases, you'll have to provide a true `Controller` implementation, possibly based on WPA Supplicant, or NetworkManager (not available out of the box in `rs-matter-stack` yet).

## Embassy

The [`rs-matter-embassy`](https://github.com/ivmarkov/rs-matter-embassy) crate provides implementations for `KvBlobStore`, `Netif`, `Gatt` and others for the [`embassy`](https://github.com/embassy-rs/embassy) framework.

## ESP-IDF

The [`esp-idf-matter`](https://github.com/ivmarkov/esp-idf-matter) crate provides implementations for `KvBlobStore`, `Netif`, `Gatt` and others for the [ESP-IDF SDK](https://github.com/esp-rs/esp-idf-svc).

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

use rs_matter_stack::eth::EthMatterStack;
use rs_matter_stack::matter::dm::clusters::desc;
use rs_matter_stack::matter::dm::clusters::desc::ClusterHandler as _;
use rs_matter_stack::matter::dm::clusters::on_off;
use rs_matter_stack::matter::dm::clusters::on_off::ClusterHandler as _;
use rs_matter_stack::matter::dm::devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter_stack::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_stack::matter::dm::networks::unix::UnixNetifs;
use rs_matter_stack::matter::dm::{Async, Dataver, Endpoint, Node};
use rs_matter_stack::matter::dm::{EmptyHandler, EpClMatcher};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::utils::init::InitMaybeUninit;
use rs_matter_stack::matter::utils::select::Coalesce;
use rs_matter_stack::matter::{clusters, devices};
use rs_matter_stack::persist::DirKvBlobStore;

use static_cell::StaticCell;

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
            &TEST_DEV_DET,
            TEST_DEV_COMM,
            &TEST_DEV_ATT,
        ));

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::dm::AsyncHandler`
    let on_off = on_off::OnOffHandler::new(Dataver::new_rand(stack.matter().rand()));

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = EmptyHandler
        .chain(
            EpClMatcher::new(
                Some(LIGHT_ENDPOINT_ID),
                Some(on_off::OnOffHandler::CLUSTER.id),
            ),
            Async(on_off::HandlerAdaptor(&on_off)),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(desc::DescHandler::CLUSTER.id)),
            Async(desc::DescHandler::new(Dataver::new_rand(stack.matter().rand())).adapt()),
        );

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let store = stack.create_shared_store(DirKvBlobStore::new_default());
    let mut matter = pin!(stack.run_preex(
        // The Matter stack needs UDP sockets to communicate with other Matter devices
        edge_nal_std::Stack::new(),
        // Will try to find a default network interface
        UnixNetifs,
        // Will persist in `<tmp-dir>/rs-matter`
        &store,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user task future to run
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
    futures_lite::future::block_on(select(&mut matter, &mut device).coalesce())
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
static MATTER_STACK: StaticCell<EthMatterStack> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EthMatterStack::<()>::root_endpoint(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters: clusters!(desc::DescHandler::CLUSTER, on_off::OnOffHandler::CLUSTER),
        },
    ],
};
```

## All examples

To build all examples, use:

```
cargo build --examples --features examples
```
