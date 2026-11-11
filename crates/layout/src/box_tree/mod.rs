//! The box tree (CSS Display 3 §1, CSS 2.2 §9.2).
//!
//! Box generation turns the styled DOM into a tree of boxes: elements with
//! `display: none` generate nothing, `display: contents` elements are
//! replaced by their children, `::before`/`::after`/`::marker` add boxes,
//! and anonymous boxes are inserted so that every block container holds
//! either only block-level or only inline-level children (CSS 2.2
//! §9.2.1.1). Inline boxes that contain block-level boxes are split around
//! them.
//!
//! The tree is an arena indexed by [`BoxId`]. Layout reads it; later
//! formatting contexts (tables, flex, floats, positioning) dispatch on
//! [`LayoutBox::formatting`] and the box's style.

mod build;
mod table;

use std::sync::Arc;

use wb_dom::NodeId;
use wb_style::{ComputedStyle, PseudoElement};

pub(crate) use build::build_box_tree;
pub(crate) use table::Span;

/// The deepest nesting of boxes the tree builder produces. Content nested
/// deeper is dropped (and [`BoxTree::truncated`] is set): this bounds the
/// recursion of every layout pass.
pub const MAX_BOX_DEPTH: usize = 192;

/// The most boxes one tree holds. Content past the limit is dropped (and
/// [`BoxTree::truncated`] is set).
pub const MAX_BOXES: usize = 1 << 20;

/// Identifies a box in its [`BoxTree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoxId(u32);

impl BoxId {
    /// The box's index in [`BoxTree::boxes`].
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn from_index(i: usize) -> Option<BoxId> {
        u32::try_from(i).ok().map(BoxId)
    }
}

/// The outer display type of a box: how it takes part in its parent's
/// formatting context (CSS Display 3 §2.1). `run-in` is treated as block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// Block-level: stacked vertically in a block formatting context.
    Block,
    /// Inline-level: laid out in lines in an inline formatting context.
    Inline,
}

/// The kind of formatting context a container box lays out its contents
/// in (CSS Display 3 §2.2), which picks the layout algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Formatting {
    /// `flow`: a block container whose children take part in the parent's
    /// block formatting context (or, for an inline box, its parent's inline
    /// formatting context).
    Flow,
    /// `flow-root` (and every block container that establishes a new block
    /// formatting context: `inline-block`, `overflow` other than `visible`,
    /// the root element, ...). See [`LayoutBox::establishes_bfc`].
    FlowRoot,
    /// `table` / `inline-table` (laid out by the table module, CSS 2.2
    /// §17; the box tree is fixed up per §17.2.1 so that table-internal
    /// boxes always sit inside a table).
    Table,
    /// A table-internal box (`table-row`, `table-cell`, ...).
    TableInternal(wb_style::DisplayInternal),
    /// `flex` / `inline-flex`.
    // M1 follow-up: the flex module lays these out; until then they are
    // laid out as block containers.
    Flex,
    /// `grid` / `inline-grid`: falls back to block layout in M1.
    Grid,
}

/// What kind of replaced content a replaced box shows.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplacedKind {
    /// An image (`<img>`, `<input type=image>`): the node's decoded image,
    /// sized from [`crate::LayoutHost::image_size`].
    Image,
    /// A form control drawn by the UA (HTML §15.5).
    FormControl(FormControl),
    /// An embedded context or plugin without layout of its own in M1
    /// (`<iframe>`, `<video>`, `<canvas>`, `<embed>`, `<object>`, `<svg>`):
    /// a box with the default 300×150 object size.
    // M1 follow-up: nested browsing contexts and SVG rendering.
    Embedded,
}

/// A form control laid out as a replaced box, with what paint needs to
/// draw it.
#[derive(Clone, Debug, PartialEq)]
pub struct FormControl {
    /// The kind of control.
    pub kind: FormControlKind,
    /// The text the control shows: the value of a text field, the label of
    /// a button, the selected option of a drop-down. Empty when none.
    pub text: String,
    /// Whether a checkbox or radio button is checked.
    pub checked: bool,
    /// The width hint in characters: the `size` attribute of a text field,
    /// `cols` of a textarea, the longest option of a select. 0 when unused.
    pub cols: u32,
    /// The height hint in rows: `rows` of a textarea, the visible rows of a
    /// list box. 0 when unused.
    pub rows: u32,
}

/// The kinds of form control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormControlKind {
    /// A single-line text field (`text`, `search`, `email`, `url`, `tel`,
    /// `password`, `number`, and the date/time types).
    TextField,
    /// A push button (`<input type=submit|reset|button|file>`).
    Button,
    /// `<input type=checkbox>`.
    Checkbox,
    /// `<input type=radio>`.
    Radio,
    /// `<input type=range>`.
    Range,
    /// `<input type=color>`.
    Color,
    /// A drop-down `<select>`.
    Select,
    /// A list-box `<select>` (`multiple` or `size` > 1).
    ListBox,
    /// `<textarea>`.
    TextArea,
    /// `<progress>`.
    Progress,
    /// `<meter>`.
    Meter,
}

/// The content of a box.
#[derive(Clone, Debug, PartialEq)]
pub enum BoxKind {
    /// A box that contains other boxes: a block container, an inline box,
    /// or a table/flex/grid container. Its children are in
    /// [`LayoutBox::children`].
    Container {
        /// The formatting context its contents are laid out in.
        formatting: Formatting,
        /// For a block container: whether its children are inline-level
        /// (it establishes an inline formatting context) rather than
        /// block-level. Always `true` for inline boxes.
        inline_children: bool,
    },
    /// A run of text. `text` is the text node's data (or the generated
    /// text of a pseudo-element); white space is processed during layout.
    Text {
        /// The text.
        text: Arc<str>,
    },
    /// A replaced element (an atomic box whose content is outside CSS).
    Replaced(ReplacedKind),
    /// A forced line break (`<br>`).
    LineBreak,
    /// A line-break opportunity (`<wbr>`).
    WordBreakOpportunity,
}

/// One box of the box tree.
#[derive(Clone, Debug)]
pub struct LayoutBox {
    /// The DOM node that generated the box: the element or text node, or
    /// for a pseudo-element box (and its generated text) the originating
    /// element. `None` for anonymous boxes.
    pub node: Option<NodeId>,
    /// The pseudo-element this box (or its text) belongs to.
    pub pseudo: Option<PseudoElement>,
    /// Whether this is an anonymous box (CSS 2.2 §9.2.1.1, §9.2.2.1).
    /// Anonymous boxes inherit their style from their parent box. An
    /// inline box split around a block keeps its node and is not anonymous.
    pub anonymous: bool,
    /// The computed style.
    pub style: Arc<ComputedStyle>,
    /// The outer display type.
    pub level: Level,
    /// What the box holds.
    pub kind: BoxKind,
    /// The parent box.
    pub parent: Option<BoxId>,
    /// The children, in order.
    pub children: Vec<BoxId>,
    /// For a list item with `list-style-position: outside`: its `::marker`
    /// box, which sits outside the principal box and is not in
    /// [`LayoutBox::children`] (an inside marker is the first inline child).
    pub marker: Option<BoxId>,
    /// For an inline box split around block-level boxes (CSS 2.2
    /// §9.2.1.1): whether this piece has the box's start and end edges.
    /// Unsplit boxes have both.
    pub split: SplitEdges,
}

/// Which edges a piece of a split inline box keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SplitEdges {
    /// The piece holds the box's start (left) margin, border and padding.
    pub start: bool,
    /// The piece holds the box's end (right) margin, border and padding.
    pub end: bool,
}

impl SplitEdges {
    /// An unsplit box.
    pub const WHOLE: SplitEdges = SplitEdges {
        start: true,
        end: true,
    };
}

impl LayoutBox {
    /// The formatting context the box's contents use, for container boxes.
    pub fn formatting(&self) -> Option<Formatting> {
        match self.kind {
            BoxKind::Container { formatting, .. } => Some(formatting),
            _ => None,
        }
    }

    /// Whether this is a non-replaced inline box (`display: inline`), whose
    /// contents take part in the surrounding inline formatting context.
    pub fn is_inline_box(&self) -> bool {
        self.level == Level::Inline
            && matches!(
                self.kind,
                BoxKind::Container {
                    formatting: Formatting::Flow,
                    ..
                }
            )
    }

    /// Whether this is an atomic inline-level box (`inline-block`,
    /// `inline-table`, `inline-flex`, an inline replaced element, ...),
    /// laid out as a single unbreakable unit in a line (CSS 2.2 §9.2.2).
    pub fn is_atomic_inline(&self) -> bool {
        self.level == Level::Inline
            && match self.kind {
                BoxKind::Container { formatting, .. } => formatting != Formatting::Flow,
                BoxKind::Replaced(_) => true,
                _ => false,
            }
    }

    /// Whether this box establishes a new block formatting context for its
    /// contents (CSS 2.2 §9.4.1, CSS Display 3 §2.2): `flow-root`, inline
    /// blocks, table cells and captions, flex/grid containers, the root,
    /// and block containers whose `overflow` is not `visible`. Floats and
    /// absolutely positioned boxes do too.
    pub fn establishes_bfc(&self) -> bool {
        match self.kind {
            BoxKind::Container { formatting, .. } => match formatting {
                Formatting::Flow => {
                    self.level == Level::Block
                        && (self.style.is_out_of_flow()
                            || !matches!(self.style.overflow_x, wb_style::Overflow::Visible)
                            || !matches!(self.style.overflow_y, wb_style::Overflow::Visible))
                }
                _ => true,
            },
            _ => false,
        }
    }
}

/// The box tree of a document.
#[derive(Clone, Debug)]
pub struct BoxTree {
    pub(crate) boxes: Vec<LayoutBox>,
    pub(crate) root: Option<BoxId>,
    pub(crate) truncated: bool,
    pub(crate) viewport_overflow: (wb_style::Overflow, wb_style::Overflow),
    /// `colspan`/`rowspan` of cells and `span` of columns (HTML §4.9.11,
    /// §4.9.3), for boxes whose values are not 1.
    pub(crate) table_spans: std::collections::HashMap<BoxId, table::Span>,
}

impl Default for BoxTree {
    fn default() -> Self {
        BoxTree {
            boxes: Vec::new(),
            root: None,
            truncated: false,
            viewport_overflow: (wb_style::Overflow::Visible, wb_style::Overflow::Visible),
            table_spans: std::collections::HashMap::new(),
        }
    }
}

impl BoxTree {
    /// The `overflow-x`/`overflow-y` that apply to the viewport, propagated
    /// from the root element or `<body>` (CSS Overflow 3 §3.3).
    pub fn viewport_overflow(&self) -> (wb_style::Overflow, wb_style::Overflow) {
        self.viewport_overflow
    }

    /// The root box (the root element's principal box), if the document
    /// renders anything.
    pub fn root(&self) -> Option<BoxId> {
        self.root
    }

    /// The box with id `id`.
    pub fn get(&self, id: BoxId) -> Option<&LayoutBox> {
        self.boxes.get(id.index())
    }

    /// All boxes, indexed by [`BoxId::index`].
    pub fn boxes(&self) -> &[LayoutBox] {
        &self.boxes
    }

    /// The number of boxes.
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    /// Whether the tree has no boxes.
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    /// Whether content was dropped because it was nested deeper than
    /// [`MAX_BOX_DEPTH`] or the tree reached [`MAX_BOXES`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The children of `id` (empty for unknown ids).
    pub fn children(&self, id: BoxId) -> &[BoxId] {
        self.get(id).map(|b| b.children.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn push(&mut self, b: LayoutBox) -> Option<BoxId> {
        if self.boxes.len() >= MAX_BOXES {
            self.truncated = true;
            return None;
        }
        let id = BoxId::from_index(self.boxes.len())?;
        self.boxes.push(b);
        Some(id)
    }

    pub(crate) fn get_mut(&mut self, id: BoxId) -> Option<&mut LayoutBox> {
        self.boxes.get_mut(id.index())
    }
}
