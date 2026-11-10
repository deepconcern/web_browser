//! Small, fast hashing for the filter indexes.
//!
//! Index keys are already 64-bit hashes, so the hash maps use an identity
//! hasher instead of SipHash. The key hashes are FNV-1a followed by a 64-bit
//! finalizer so that both the high and low bits are well mixed.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Incremental FNV-1a state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) const fn new() -> Self {
        Self(FNV_OFFSET)
    }

    pub(crate) fn push(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    /// Finishes the hash with a 64-bit avalanche step.
    pub(crate) fn finish(self) -> u64 {
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^= x >> 33;
        x
    }
}

/// Hashes bytes, folding ASCII letters to lower case.
pub(crate) fn hash_lower(bytes: &[u8]) -> u64 {
    let mut h = Fnv::new();
    for &b in bytes {
        h.push(b.to_ascii_lowercase());
    }
    h.finish()
}

/// A hasher for keys that are already good 64-bit hashes.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Not used for u64 keys, but must be correct for any key type.
        let mut h = Fnv(self.0 ^ FNV_OFFSET);
        for &b in bytes {
            h.push(b);
        }
        self.0 = h.finish();
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// A map keyed by a precomputed 64-bit hash.
pub(crate) type IdMap<V> = HashMap<u64, V, BuildHasherDefault<IdHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_case_insensitive() {
        assert_eq!(hash_lower(b"AdServer"), hash_lower(b"adserver"));
        assert_ne!(hash_lower(b"ads"), hash_lower(b"adt"));
    }

    #[test]
    fn incremental_matches_one_shot() {
        let mut h = Fnv::new();
        for &b in b"banner" {
            h.push(b);
        }
        assert_eq!(h.finish(), hash_lower(b"banner"));
    }
}
