//! Table box tree fix-up (CSS 2.2 §17.2.1 "Anonymous table objects", CSS
//! Tables 3 §3.2) and the HTML span attributes of cells and columns.
//!
//! Runs once over the finished box tree, top-down, so that afterwards:
//!
//! - every table-internal box sits in its proper parent: cells in rows,
//!   rows in row groups or tables, columns in column groups or tables,
//!   row groups, column groups and captions in tables (missing parents
//!   are generated as anonymous boxes, §17.2.1 step 3);
//! - tables, row groups and rows hold only their proper children (other
//!   content is wrapped in anonymous rows and cells, step 2), and white
//!   space between table parts is dropped (step 1);
//! - columns have no children and column groups only hold columns.
//!
//! Every box is visited once and each generated box wraps at least one
//! existing box, so the pass is linear in the size of the tree.

use std::collections::HashMap;
use std::sync::Arc;

use wb_dom::Document;
use wb_style::{
    ComputedStyle, Display, DisplayInside, DisplayInternal, DisplayOutside, WhiteSpace,
};

use super::build::{inherit_style, parse_integer};
use super::{BoxId, BoxKind, BoxTree, Formatting, LayoutBox, Level, MAX_BOX_DEPTH, SplitEdges};

/// The largest `colspan` and column `span` (HTML §4.9.11, §4.9.3).
pub(crate) const MAX_COLSPAN: u32 = 1000;

/// The largest `rowspan` (HTML §4.9.11).
pub(crate) const MAX_ROWSPAN: u32 = 65534;

/// The spans HTML gives a cell (`colspan`, `rowspan`) or a column or
/// column group (`span`, stored in `cols`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Span {
    /// Columns spanned, 1..=[`MAX_COLSPAN`].
    pub cols: u32,
    /// Rows spanned, 0..=[`MAX_ROWSPAN`]; 0 means "to the end of the row
    /// group".
    pub rows: u32,
}

impl Span {
    /// One column, one row.
    pub(crate) const ONE: Span = Span { cols: 1, rows: 1 };
}

/// What a box is, as far as the table fix-up cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Table,
    RowGroup,
    Row,
    Cell,
    Column,
    ColumnGroup,
    Caption,
    Other,
}

impl Part {
    fn of(b: &LayoutBox) -> Part {
        match b.kind {
            BoxKind::Container {
                formatting: Formatting::Table,
                ..
            } => Part::Table,
            BoxKind::Container {
                formatting: Formatting::TableInternal(k),
                ..
            } => match k {
                DisplayInternal::TableRowGroup
                | DisplayInternal::TableHeaderGroup
                | DisplayInternal::TableFooterGroup => Part::RowGroup,
                DisplayInternal::TableRow => Part::Row,
                DisplayInternal::TableCell => Part::Cell,
                DisplayInternal::TableColumn => Part::Column,
                DisplayInternal::TableColumnGroup => Part::ColumnGroup,
                DisplayInternal::TableCaption => Part::Caption,
                _ => Part::Other,
            },
            _ => Part::Other,
        }
    }

    /// A "proper table child" (CSS 2.2 §17.2.1).
    fn is_proper_table_child(self) -> bool {
        matches!(
            self,
            Part::RowGroup | Part::Row | Part::Column | Part::ColumnGroup | Part::Caption
        )
    }
}

struct Fixer<'a> {
    tree: &'a mut BoxTree,
    /// Anonymous styles by (parent style address, kind).
    styles: HashMap<(usize, u8), Arc<ComputedStyle>>,
}

/// Fixes up the table structure of `tree` (see the module docs) and
/// records the HTML spans of cells and columns.
pub(crate) fn fix_up_tables(tree: &mut BoxTree, doc: &Document) {
    let Some(root) = tree.root else {
        return;
    };
    // Most documents without tables pay one scan.
    if !tree.boxes.iter().any(|b| Part::of(b) != Part::Other) {
        return;
    }
    let mut f = Fixer {
        tree,
        styles: HashMap::new(),
    };
    let mut stack = vec![(root, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        if depth > MAX_BOX_DEPTH * 2 {
            continue;
        }
        f.fix_children(id);
        f.record_span(doc, id);
        for &c in f.tree.children(id).iter().rev() {
            stack.push((c, depth + 1));
        }
    }
}

impl Fixer<'_> {
    fn part(&self, id: BoxId) -> Part {
        self.tree.get(id).map_or(Part::Other, Part::of)
    }

    /// Reads `colspan`/`rowspan` (cells) or `span` (columns).
    fn record_span(&mut self, doc: &Document, id: BoxId) {
        let Some(b) = self.tree.get(id) else { return };
        if b.anonymous || b.pseudo.is_some() {
            return;
        }
        let Some(el) = b.node.and_then(|n| doc.element(n)) else {
            return;
        };
        let span = match Part::of(b) {
            Part::Cell if el.is_html("td") || el.is_html("th") => Span {
                cols: parse_colspan(el.attribute("colspan")),
                rows: parse_rowspan(el.attribute("rowspan")),
            },
            Part::Column if el.is_html("col") => Span {
                cols: parse_colspan(el.attribute("span")),
                rows: 1,
            },
            Part::ColumnGroup if el.is_html("colgroup") => Span {
                cols: parse_colspan(el.attribute("span")),
                rows: 1,
            },
            _ => return,
        };
        if span != Span::ONE {
            self.tree.table_spans.insert(id, span);
        }
    }

    fn fix_children(&mut self, id: BoxId) {
        let Some(b) = self.tree.get(id) else { return };
        let parent_part = Part::of(b);
        let children = b.children.clone();
        if children.is_empty() {
            return;
        }
        let new_children = match parent_part {
            // §17.2.1 1.1: children of a column are treated as `none`.
            Part::Column => Vec::new(),
            // 1.2: a column group keeps only its columns.
            Part::ColumnGroup => children
                .into_iter()
                .filter(|c| self.part(*c) == Part::Column)
                .collect(),
            // 2.1: runs of improper children of a table go in an anonymous
            // row (whose own fix-up then wraps them in a cell).
            Part::Table => self.wrap_runs(id, &children, |p| !p.is_proper_table_child(), Wrap::Row),
            // 2.2: improper children of a row group go in an anonymous row.
            Part::RowGroup => self.wrap_runs(id, &children, |p| p != Part::Row, Wrap::Row),
            // 2.3: improper children of a row go in an anonymous cell.
            Part::Row => self.wrap_runs(id, &children, |p| p != Part::Cell, Wrap::Cell),
            // 3: misparented table parts get an anonymous table (cells
            // get their row when that table is fixed up in turn).
            Part::Cell | Part::Caption | Part::Other => self.wrap_runs(
                id,
                &children,
                |p| p == Part::Cell || p.is_proper_table_child(),
                Wrap::Table,
            ),
        };
        let tabular = matches!(parent_part, Part::Table | Part::RowGroup | Part::Row);
        let inline = !tabular
            && !matches!(parent_part, Part::Column | Part::ColumnGroup)
            && !new_children.is_empty()
            && new_children.iter().all(|c| {
                self.tree
                    .get(*c)
                    .is_some_and(|cb| cb.level == Level::Inline)
            });
        for c in &new_children {
            if let Some(cb) = self.tree.get_mut(*c) {
                cb.parent = Some(id);
            }
        }
        if let Some(b) = self.tree.get_mut(id) {
            b.children = new_children;
            if let BoxKind::Container {
                inline_children, ..
            } = &mut b.kind
            {
                *inline_children = inline;
            }
        }
    }

    /// Wraps each maximal run of in-flow children whose part satisfies
    /// `pick` in a new anonymous box of kind `wrap`. Runs of collapsible white space in
    /// tabular containers are dropped instead (§17.2.1 1.3–1.5).
    fn wrap_runs(
        &mut self,
        parent: BoxId,
        children: &[BoxId],
        pick: impl Fn(Part) -> bool,
        wrap: Wrap,
    ) -> Vec<BoxId> {
        let mut out = Vec::with_capacity(children.len());
        let mut run: Vec<BoxId> = Vec::new();
        for &c in children {
            // Absolutely positioned children are not table parts and are
            // never wrapped (CSS Tables 3 §3.2): the positioning module
            // lays them out.
            let out_of_flow = self
                .tree
                .get(c)
                .is_some_and(|b| b.parent.is_some() && b.style.is_absolutely_positioned());
            if !out_of_flow && pick(self.part(c)) {
                run.push(c);
            } else {
                self.flush(parent, &mut run, wrap, &mut out);
                out.push(c);
            }
        }
        self.flush(parent, &mut run, wrap, &mut out);
        out
    }

    fn flush(&mut self, parent: BoxId, run: &mut Vec<BoxId>, wrap: Wrap, out: &mut Vec<BoxId>) {
        if run.is_empty() {
            return;
        }
        let run = std::mem::take(run);
        if wrap != Wrap::Table && run.iter().all(|c| self.collapsible_whitespace(*c, 0)) {
            return;
        }
        let Some(pb) = self.tree.get(parent) else {
            return;
        };
        let parent_style = pb.style.clone();
        let (display, formatting) = match wrap {
            Wrap::Table => (
                Display::Pair {
                    outside: DisplayOutside::Block,
                    inside: DisplayInside::Table,
                    list_item: false,
                },
                Formatting::Table,
            ),
            Wrap::Row => (
                Display::Internal(DisplayInternal::TableRow),
                Formatting::TableInternal(DisplayInternal::TableRow),
            ),
            Wrap::Cell => (
                Display::Internal(DisplayInternal::TableCell),
                Formatting::TableInternal(DisplayInternal::TableCell),
            ),
        };
        let key = (Arc::as_ptr(&parent_style) as usize, wrap as u8);
        let style = self
            .styles
            .entry(key)
            .or_insert_with(|| Arc::new(inherit_style(&parent_style, display)))
            .clone();
        let Some(anon) = self.tree.push(LayoutBox {
            node: None,
            pseudo: None,
            anonymous: true,
            style,
            level: Level::Block,
            kind: BoxKind::Container {
                formatting,
                inline_children: false,
            },
            parent: Some(parent),
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        }) else {
            // Out of boxes: leave the run unwrapped (laid out as blocks).
            out.extend(run);
            return;
        };
        let inline = run
            .iter()
            .all(|c| self.tree.get(*c).is_some_and(|b| b.level == Level::Inline));
        for c in &run {
            if let Some(cb) = self.tree.get_mut(*c) {
                cb.parent = Some(anon);
            }
        }
        if let Some(a) = self.tree.get_mut(anon) {
            a.children = run;
            if let BoxKind::Container {
                inline_children, ..
            } = &mut a.kind
            {
                *inline_children = inline && wrap == Wrap::Cell;
            }
        }
        out.push(anon);
    }

    /// Whether `id` is inline-level and holds only collapsible white space
    /// (CSS Text 3 §4.1.1).
    fn collapsible_whitespace(&self, id: BoxId, depth: usize) -> bool {
        let Some(b) = self.tree.get(id) else {
            return true;
        };
        match &b.kind {
            BoxKind::Text { text } => {
                let ws = b.style.white_space;
                let newlines = matches!(ws, WhiteSpace::Normal | WhiteSpace::Nowrap);
                let spaces = newlines || ws == WhiteSpace::PreLine;
                spaces
                    && text.chars().all(|c| match c {
                        ' ' | '\t' | '\r' => true,
                        '\n' => newlines,
                        _ => false,
                    })
            }
            BoxKind::Container { .. } if b.is_inline_box() && b.anonymous => {
                depth < MAX_BOX_DEPTH
                    && b.children
                        .iter()
                        .all(|c| self.collapsible_whitespace(*c, depth + 1))
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wrap {
    Table,
    Row,
    Cell,
}

/// `colspan` / `span`: HTML §4.9.11 "rules for parsing non-negative
/// integers"; a missing, invalid or zero value is 1; at most 1000.
fn parse_colspan(v: Option<&str>) -> u32 {
    match v.and_then(parse_integer) {
        Some(n) if n >= 1 => u32::try_from(n.min(i64::from(MAX_COLSPAN))).unwrap_or(1),
        _ => 1,
    }
}

/// `rowspan`: a missing or invalid value is 1; 0 is kept (the cell spans
/// to the end of its row group); at most 65534.
fn parse_rowspan(v: Option<&str>) -> u32 {
    match v.and_then(parse_integer) {
        Some(n) if n >= 0 => u32::try_from(n.min(i64::from(MAX_ROWSPAN))).unwrap_or(1),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(parse_colspan(None), 1);
        assert_eq!(parse_colspan(Some("0")), 1);
        assert_eq!(parse_colspan(Some("-3")), 1);
        assert_eq!(parse_colspan(Some("x")), 1);
        assert_eq!(parse_colspan(Some(" 3px")), 3);
        assert_eq!(parse_colspan(Some("5000")), 1000);
        assert_eq!(parse_rowspan(Some("0")), 0);
        assert_eq!(parse_rowspan(Some("-1")), 1);
        assert_eq!(parse_rowspan(Some("99999999999")), 65534);
        assert_eq!(parse_rowspan(None), 1);
    }
}
