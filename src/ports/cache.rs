use crate::domains::impact::{BaselineFingerprint, DiffBaseline};

pub trait CachePort {
    type Error;

    fn load(&self, fingerprint: &BaselineFingerprint) -> Result<Option<DiffBaseline>, Self::Error>;
    fn store(&self, baseline: &DiffBaseline) -> Result<(), Self::Error>;
}
