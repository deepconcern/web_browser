//! One module per decoded format. Each wraps a third-party decoder and
//! exposes `decode(bytes, limits)` and `probe(bytes)`.

pub(crate) mod gif;
pub(crate) mod jpeg;
pub(crate) mod png;
pub(crate) mod webp;
