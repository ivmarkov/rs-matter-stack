/// Re-export the `edge-nal` crate
pub use edge_nal::*;

/// Re-export the `edge-nal-std` crate
#[cfg(feature = "std")]
pub mod std {
    pub use edge_nal_std::*;
}
