#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![warn(clippy::large_futures)]

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

pub use error::*;
pub use stack::*;

mod error;
pub mod modem;
pub mod netif;
pub mod persist;
mod stack;
mod udp;
pub mod wifi;
