/// User data that can be embedded in the stack network
pub trait Embedding {
    const INIT: Self;
}

impl Embedding for () {
    const INIT: Self = ();
}

/// A trait modeling a specific network type.
/// `MatterStack` is parameterized by a network type implementing this trait.
pub trait Network {
    const INIT: Self;

    type Embedding: Embedding + 'static;

    fn embedding(&self) -> &Self::Embedding;
}
