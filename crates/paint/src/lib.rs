//! Display lists, rasterization and hit testing.
//!
//! The pipeline (issue #3 §B.1): layout produces a fragment tree;
//! [`build_display_list`] turns it into a [`DisplayList`] in CSS 2.2
//! Appendix E painting order (see [`build`] for the order, clips and hit
//! tags); [`rasterize`] draws
//! the list with tiny-skia (ADR-0002) into a [`Raster`] of premultiplied
//! RGBA8 pixels that the shell uploads as a texture; [`hit_test`] maps a
//! pointer position back to the tagged items under it, for links, form
//! controls and text selection.
//!
//! - **Units.** Display lists are in CSS px. The device scale factor (and
//!   zoom) is applied only at raster time, through
//!   [`RasterOptions::scale`], so one list serves every scale.
//! - **Order.** The list is in paint order and paint never reorders it:
//!   the builder emits items in CSS 2.1 Appendix E stacking order. See
//!   [`DisplayList`] for the full builder contract.
//! - **Bounds.** Lists cap their item count, glyph count and nesting
//!   depth ([`ListLimits`]); rasterization caps the output size and the
//!   memory for layers and masks ([`RasterLimits`]). Page content can't
//!   exhaust memory through paint.
//!
//! # Coordinates and pixel snapping
//!
//! A CSS px coordinate `v` becomes the device coordinate
//! `(v + scroll - viewport_origin) * scale`, where `scroll` is the sum of
//! the enclosing [`DisplayItem::PushScroll`] offsets. Then:
//!
//! - **Box edges** (fills, borders, clips, image destinations, background
//!   tiles) snap each edge independently to the nearest device pixel
//!   (half rounds up). Boxes that share an edge in CSS px share it on the
//!   device too, with no gaps or overlaps, and square edges are crisp.
//!   Corner radii are scaled but not snapped; curves are antialiased.
//! - **Border widths** snap per CSS Values 4 "snap as a border width": a
//!   width between 0 and 1 device px becomes 1, others are floored.
//! - **Text**: the baseline snaps to a whole device pixel; glyph x
//!   positions keep a quarter-pixel subpixel position (see
//!   [`wb_text::subpixel_position`]).
//!
//! # Clipping, layers and bounds
//!
//! Rectangular clips are applied to geometry exactly and cost nothing.
//! Rounded clips need a coverage mask the size of the current surface, and
//! an opacity group needs a layer the size of the current clip. Both come
//! out of [`RasterLimits::max_scratch_bytes`]; when the budget or
//! [`RasterLimits::max_layers`] runs out, a rounded clip falls back to its
//! bounding rectangle and an opacity group is applied to each item inside
//! it instead (overlaps inside the group then show through). Items whose
//! bounds miss the current clip (which starts as the dirty rectangle) are
//! skipped.
//!
//! tiny-skia is private to this crate: no type of its appears in this API.

mod border;
pub mod build;
mod geom;
mod hit;
mod list;
mod raster;
mod shape;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use build::{
    BuildOptions, HitTarget, NoResources, PaintResources, build_display_list, fragment_tag,
    resolve_hit,
};
pub use geom::{Color, CornerRadii, Point, Radius, Rect, RoundedRect};
pub use hit::{Hit, hit_test, hit_test_topmost};
pub use list::{
    BackgroundImageItem, BorderItem, BorderSide, BorderStyle, DisplayItem, DisplayList, FillItem,
    HitTag, ImageItem, ImageRendering, ListLimits, TextGlyph, TextItem,
};
pub use raster::{
    GlyphRasterizer, PaintError, Raster, RasterLimits, RasterOptions, rasterize, repaint,
};
