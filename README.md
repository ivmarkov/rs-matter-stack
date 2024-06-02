# (WIP) Easily configure and run [rs-matter](https://github.com/project-chip/rs-matter)

[![CI](https://github.com/ivmarkov/rs-matter-stack/actions/workflows/ci.yml/badge.svg)](https://github.com/ivmarkov/rs-matter-stack/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/rs-matter-stack.svg)](https://crates.io/crates/rs-matter-stack)
[![Matrix](https://img.shields.io/matrix/matter-rs:matrix.org?label=join%20matrix&color=BEC5C9&logo=matrix)](https://matrix.to/#/#matter-rs:matrix.org)

## Overview

Configuring and running the [`rs-matter`](https://github.com/project-chip/rs-matter) crate is not trivial, as it is more of a toolkit rather than a monolitic all-in-one runtime.

Furthermore, _operating_ the assembled Matter stack is also challenging, as various features might need to be switched on or off depending on whether Matter is running in commissioning or operating mode, and also depending on the current network connectivity (as in e.g. Wifi signal lost).

**This crate addresses these issues by providing an all-in-one [`MatterStack`](https://github.com/ivmarkov/rs-matter-stack/blob/master/src/lib.rs) assembly that configures `rs-matter` for reliable operation.**

Instantiate it and then call `MatterStack::<...>::run(...)`.

## OK, but what would I sacrifice?

**Flexibility**.

Using `MatterStack<...>` hard-codes the following:
* _One large future_: The Matter stack is assembled as one large future which is not `Send`. Using an executor to poll that future together with others is still possible, but the executor should be a local one (i.e. Tokio's `LocalSet`, `async_executor::LocalExecutor` and so on).
* _Allocation strategy_: a number of large-ish buffers are const-allocated inside the `MatterStack` struct. This allows the whole stack to be statically-allocated with `ConstStaticCell` - yet - that would eat up 20 to 60K of your flash size, depending on the selected max number of subscriptions, exchange buffers and so on. A different allocation strategy might be provided in future.

## The examples are STD-only?

The core of `rs-matter-stack` is `no_std` and no-`alloc`.

You need to provide platform-specific implementations of the following traits for your embedded platform:
- `Persist` - non-volatile storage abstraction. Easiest is to implement `KvBlobStore` instead, and then use it with the provided `KvPersist` utility. 
  - For STD, `rs-matter-stack` provides `DirKvBlobStore`.
- `Netif` - network interface abstraction (i.e. monitoring when the network interface is up or down, and what is its IP configuration).
  - For Unix-like OSes, `rs-matter-stack` provides `UnixNetif`, which uses a simple polling every 2 seconds to detect changes to the network interface.
  - Note that For IP (TCP & UDP) IO, the stack uses the [`edge-nal`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal) crate, and is thus compatible with [`STD`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-std) and [`Embassy`](https://github.com/ivmarkov/edge-net/tree/master/edge-nal-embassy) out of the box. You only need to worry about networking IO if you use other platforms than these two.
- `Modem` (for BLE & Wifi only) - abstraction of the device radio that can operate either in Wifi, or in BLE mode. 
  - `DummyLinuxModem` can be used on Linux to test with BLE. This modem uses Linux BlueZ and the simple `UnixNetif` netif implementation from above, but fakes the Wifi interface with a dummy no-nop one. For production embedded Linux use-cases, you'll have to provide a `Wifi` implementation, possibly based on WPA Supplicant, or NetworkManager.

## ESP-IDF

The [`esp-idf-matter`](https://github.com/ivmarkov/esp-idf-matter) crate provides implementations for `Persist`, `Netif` and `Modem` for the ESP-IDF SDK.

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
use rs_matter::error::Error;
use rs_matter::secure_channel::spake2p::VerifierData;
use rs_matter::utils::select::Coalesce;
use rs_matter::CommissioningData;

use rs_matter_stack::netif::UnixNetif;
use rs_matter_stack::persist::{DirKvBlobStore, KvBlobBuf, KvPersist};
use rs_matter_stack::EthMatterStack;

use static_cell::ConstStaticCell;

#[path = "dev_att/dev_att.rs"]
mod dev_att;

fn main() -> Result<(), Error> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

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
        // Will persist in `<tmp-dir>/rs-matter`
        KvPersist::new_eth(DirKvBlobStore::new_default(), stack),
        // Will try to find a default network interface
        UnixNetif::default(),
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
static MATTER_STACK: ConstStaticCell<EthMatterStack<KvBlobBuf<()>>> =
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
        EthMatterStack::<KvBlobBuf<()>>::root_metadata(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_type: DEV_TYPE_ON_OFF_LIGHT,
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
