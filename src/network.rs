use rs_matter::utils::init::{init_from_closure, Init};

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

    type Embedding: Embedding + 'static;

    fn embedding(&self) -> &Self::Embedding;

    fn init() -> impl Init<Self>;
}
