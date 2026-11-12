//! A counting Bloom filter over the ancestors of the element being styled.
//!
//! Before matching a selector that needs certain ancestors (`.a .b`,
//! `#nav > li`), the matcher checks that every required id, class and tag
//! hash is in the filter. A miss proves the selector cannot match; a hit is
//! only a "maybe" and full matching follows. Entries are hashed
//! ASCII-lowercased so that quirks-mode matching stays correct.

/// Number of counters (a power of two).
const SIZE: usize = 4096;
const MASK: u32 = (SIZE as u32) - 1;

/// What kind of name is hashed, so `#a`, `.a` and `a` differ.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Tag,
    Id,
    Class,
}

/// FNV-1a over the lowercased name, seeded by the kind.
pub(crate) fn hash(kind: Kind, name: &str) -> u32 {
    let mut h: u32 = match kind {
        Kind::Tag => 0x811c_9dc5,
        Kind::Id => 0x0100_0193,
        Kind::Class => 0x9e37_79b9,
    };
    for b in name.bytes() {
        h ^= u32::from(b.to_ascii_lowercase());
        h = h.wrapping_mul(0x0100_0193);
    }
    // Avoid zero, which `RuleSelector` uses for "no hash".
    if h == 0 { 1 } else { h }
}

#[derive(Clone, Debug)]
pub(crate) struct AncestorFilter {
    counters: Vec<u8>,
}

impl AncestorFilter {
    pub(crate) fn new() -> Self {
        AncestorFilter {
            counters: vec![0; SIZE],
        }
    }

    fn slots(h: u32) -> [usize; 2] {
        [(h & MASK) as usize, ((h >> 12) & MASK) as usize]
    }

    pub(crate) fn insert(&mut self, h: u32) {
        for s in Self::slots(h) {
            if let Some(c) = self.counters.get_mut(s) {
                // A saturated counter is never decremented again (it stays
                // a permanent "maybe"), which keeps the filter sound.
                *c = c.saturating_add(1);
            }
        }
    }

    pub(crate) fn remove(&mut self, h: u32) {
        for s in Self::slots(h) {
            if let Some(c) = self.counters.get_mut(s)
                && *c != u8::MAX
                && *c != 0
            {
                *c -= 1;
            }
        }
    }

    pub(crate) fn might_contain(&self, h: u32) -> bool {
        Self::slots(h)
            .iter()
            .all(|&s| self.counters.get(s).is_some_and(|&c| c != 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_remove() {
        let mut f = AncestorFilter::new();
        let a = hash(Kind::Class, "foo");
        let b = hash(Kind::Class, "bar");
        assert!(!f.might_contain(a));
        f.insert(a);
        f.insert(a);
        assert!(f.might_contain(a));
        f.remove(a);
        assert!(f.might_contain(a));
        f.remove(a);
        assert!(!f.might_contain(a));
        assert!(!f.might_contain(b));
        assert_eq!(hash(Kind::Tag, "DIV"), hash(Kind::Tag, "div"));
        assert_ne!(hash(Kind::Tag, "a"), hash(Kind::Class, "a"));
    }
}
