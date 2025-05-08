pub use rand_core::{CryptoRng, RngCore};

use rs_matter::utils::rand::Rand;

/// Adapt `rs-matter`'s `Rand` to `RngCore`
pub struct MatterRngCore(Rand);

impl MatterRngCore {
    pub const fn new(rand: Rand) -> Self {
        Self(rand)
    }
}

impl RngCore for MatterRngCore {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        (self.0)(&mut bytes);

        u32::from_ne_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        (self.0)(&mut bytes);

        u64::from_ne_bytes(bytes)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        (self.0)(dest);
    }
}

impl CryptoRng for MatterRngCore {}

impl rand_core06::RngCore for MatterRngCore {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        (self.0)(&mut bytes);

        u32::from_ne_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        (self.0)(&mut bytes);

        u64::from_ne_bytes(bytes)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        (self.0)(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core06::Error> {
        (self.0)(dest);

        Ok(())
    }
}

impl rand_core06::CryptoRng for MatterRngCore {}
