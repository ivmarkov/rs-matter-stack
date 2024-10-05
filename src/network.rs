use rs_matter::utils::init::{init_from_closure, Init};

use crate::persist::NetworkPersist;

/// User data that can be embedded in the stack network
pub trait Embedding {
    const INIT: Self;

    fn init() -> impl Init<Self>;
}

impl Embedding for () {
    const INIT: Self = ();

    fn init() -> impl Init<Self> {
        unsafe { init_from_closure(|_| Ok(())) }
    }
}

/// A trait modeling a specific network type.
/// `MatterStack` is parameterized by a network type implementing this trait.
pub trait Network {
    const INIT: Self;

    /// The network peristence context to be used by the `Persist` trait.
    type PersistContext<'a>: NetworkPersist
    where
        Self: 'a;

    /// Optional additional state embedded in the network state
    type Embedding: Embedding + 'static;

    fn persist_context(&self) -> Self::PersistContext<'_>;

    fn embedding(&self) -> &Self::Embedding;

    fn init() -> impl Init<Self>;
}
