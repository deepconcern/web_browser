//! Floats and `clear` (CSS 2.2 §9.5).
//!
//! M1 follow-up: the float module adds a float list per block formatting
//! context (placement rules §9.5.1, line box shortening next to floats,
//! clearance §9.5.2, BFC roots avoiding floats). [`is_float`] is the hook
//! block and inline layout call; until floats land it returns `false`, so
//! floated boxes (already blockified by the style system) are laid out in
//! normal flow.

use crate::box_tree::LayoutBox;

/// Whether `b` is laid out as a float by the float module.
pub(crate) fn is_float(b: &LayoutBox) -> bool {
    // M1 follow-up: `b.style.is_floating()` once floats are implemented.
    let _ = b;
    false
}
