//! Table layout (CSS 2.2 §17, CSS Tables 3 draft for column widths).
//!
//! The box tree arrives fixed up (CSS 2.2 §17.2.1, see
//! `box_tree::table`): a table box's children are captions, columns and
//! column groups, row groups and rows; rows hold cells. This module:
//!
//! 1. builds the table grid (HTML §4.9.12 "forming a table" for slot
//!    assignment): header group first, footer group last (CSS 2.2
//!    §17.2), `colspan` 1..=1000, `rowspan` 0..=65534 clipped to the row
//!    group (0 spans to its end);
//! 2. in the collapsing border model resolves every grid edge (§17.6.2.1,
//!    `borders`);
//! 3. computes column widths with the automatic (§17.5.2.2 and CSS Tables
//!    3 §3.9) or fixed (§17.5.2.1) table layout algorithm (`columns`);
//! 4. lays out cells, computes row heights with baseline alignment
//!    (§17.5.3), aligns cell contents (`vertical-align`), and distributes
//!    a specified table height over the rows;
//! 5. places captions (`caption-side`) in the table wrapper box (§17.4).
//!
//! # Fragments
//!
//! A table produces a fragment for the table box whose children are, in
//! painting order (§17.5.1), its column groups and columns (spanning the
//! rows), then its row groups and rows, and in each row the cells that
//! start in it (a cell spanning rows extends below its row). Rows
//! directly in the table (no row group) are children of the table. When
//! the table has captions, the returned fragment is an anonymous wrapper
//! (no node, no decorations) holding the captions and the table fragment.
//!
//! Paint reads borders and backgrounds from [`crate::Fragment::style`]
//! as usual; for table parts that style is adjusted so paint needs no
//! table knowledge:
//!
//! - Rows, row groups, columns and column groups never draw borders
//!   (separated model: they have none, §17.6.1; collapsing model: their
//!   borders are merged into the cells'), so their fragment style has
//!   every border set to `none`.
//! - `border-collapse: collapse`: each cell fragment's style carries the
//!   *resolved* collapsed borders: on each side the winning border of
//!   that grid edge (the widest segment when the cell spans several), at
//!   half its width, with the winner's style and color. The cell's
//!   border box runs from grid line to grid line, so the two cells either
//!   side of a grid line each draw their half and together draw the full
//!   collapsed border, centred on the line. The table fragment's style
//!   likewise carries the outer halves (§17.6.2: top and bottom from the
//!   widest segment, left and right from the first row) and no padding.
//!   Fragment rectangles agree with these widths.
//! - `empty-cells: hide` (separated model) on a cell without content: its
//!   style has no borders and a transparent background.
//!
//! # Bounds
//!
//! At most [`MAX_TABLE_COLUMNS`] columns and [`MAX_TABLE_ROWS`] rows, and
//! rows × columns at most [`MAX_GRID_SLOTS`]; cells past the limits are
//! dropped. Tables nested more than [`MAX_TABLE_NESTING`] deep are laid
//! out as blocks. Every step is linear in the number of cells plus the
//! grid area.

mod borders;
mod columns;
#[cfg(test)]
mod tests;

use std::ops::Range;
use std::sync::Arc;

use wb_style::{
    BorderCollapse, BorderSide, ComputedStyle, DisplayInternal, EmptyCells, LengthPercentage,
    LineStyle, Rgba, Size as CssSize, TableLayout, VerticalAlign, VerticalAlignKeyword, Visibility,
};

use self::borders::{Origin, Resolved, resolve_edge};
use self::columns::{CellMeasure, ColMeasure, FixedWidth};
use super::sizing::{
    BoxEdges, SizeSpec, box_edges, clamp_min_max, height_px, max_size, min_size, solve_margins,
    solve_width, used_height, width_spec,
};
use super::{
    CollapsedMargin, ContainingBlock, IntrinsicSizes, LayoutResult, WidthMode, block, inline,
    intrinsic_sizes, layout_block_level, positioned,
};
use crate::box_tree::{BoxId, BoxKind, Formatting, LayoutBox, Level, MAX_BOX_DEPTH, Span};
use crate::context::LayoutCtx;
use crate::fragment::{BoxFragmentKind, FragmentId, FragmentKind};
use crate::geom::{Edges, MAX_COORD, Point, Size, sane, sane_non_negative};

/// The most columns a table has; cells beyond are dropped.
pub(crate) const MAX_TABLE_COLUMNS: usize = 1000;

/// The most rows a table has; rows beyond are dropped.
pub(crate) const MAX_TABLE_ROWS: usize = 65_535;

/// The most grid slots (rows × columns) a table has: the column limit
/// shrinks for tables with many rows.
pub(crate) const MAX_GRID_SLOTS: usize = 1 << 19;

/// Tables nested deeper than this inside other tables are laid out as
/// block containers.
pub(crate) const MAX_TABLE_NESTING: usize = 32;

/// A run of rows in display order: a row group, or consecutive rows that
/// sit directly in the table.
#[derive(Clone, Debug)]
struct Section {
    group: Option<BoxId>,
    rows: Range<usize>,
}

/// A cell and the grid slots it covers.
#[derive(Clone, Copy, Debug)]
struct Cell {
    id: BoxId,
    row: usize,
    col: usize,
    rowspan: usize,
    colspan: usize,
}

/// A grid column and the boxes that define it.
#[derive(Clone, Copy, Debug, Default)]
struct Column {
    col: Option<BoxId>,
    group: Option<BoxId>,
}

/// A `table-column` or `table-column-group` box and its columns.
#[derive(Clone, Debug)]
struct ColBox {
    id: BoxId,
    first: usize,
    count: usize,
    /// For a column group: its columns' entries in `Grid::col_boxes`.
    kids: Range<usize>,
    in_group: bool,
}

/// The table grid (CSS 2.2 §17.5).
#[derive(Debug, Default)]
struct Grid {
    captions: Vec<BoxId>,
    columns: Vec<Column>,
    col_boxes: Vec<ColBox>,
    sections: Vec<Section>,
    /// Row boxes in display order.
    rows: Vec<BoxId>,
    /// The section of each row.
    row_section: Vec<usize>,
    /// Cells in display (row) order.
    cells: Vec<Cell>,
    truncated: bool,
}

/// Everything layout and intrinsic sizing share about one table.
struct Prepared {
    grid: Grid,
    style: Arc<ComputedStyle>,
    collapse: bool,
    /// `border-spacing` (0 when collapsing).
    hs: f32,
    vs: f32,
    /// The table box's used border and padding.
    border: Edges,
    padding: Edges,
    /// The table fragment's style when it differs (collapsed borders).
    table_style: Option<Arc<ComputedStyle>>,
    /// Per cell: used border, padding, and fragment style override.
    cell_border: Vec<Edges>,
    cell_padding: Vec<Edges>,
    cell_style: Vec<Option<Arc<ComputedStyle>>>,
}

fn part(b: &LayoutBox) -> Option<DisplayInternal> {
    match b.kind {
        BoxKind::Container {
            formatting: Formatting::TableInternal(k),
            ..
        } => Some(k),
        _ => None,
    }
}

/// How many tables enclose box `id`.
fn nesting(ctx: &LayoutCtx<'_>, id: BoxId) -> usize {
    let mut n = 0;
    let mut cur = ctx.get(id).and_then(|b| b.parent);
    let mut steps = 0;
    while let Some(p) = cur {
        let Some(b) = ctx.get(p) else { break };
        if b.formatting() == Some(Formatting::Table) {
            n += 1;
        }
        steps += 1;
        if steps > MAX_BOX_DEPTH * 4 {
            break;
        }
        cur = b.parent;
    }
    n
}

fn is_table(ctx: &LayoutCtx<'_>, id: BoxId) -> bool {
    ctx.get(id)
        .is_some_and(|b| b.formatting() == Some(Formatting::Table))
        && nesting(ctx, id) <= MAX_TABLE_NESTING
}

fn span_of(ctx: &LayoutCtx<'_>, id: BoxId) -> Span {
    ctx.tree.table_spans.get(&id).copied().unwrap_or(Span::ONE)
}

/// Builds the grid of table `table`.
fn build_grid(ctx: &LayoutCtx<'_>, table: BoxId) -> Grid {
    let tree = ctx.tree;
    let mut g = Grid::default();
    let mut header: Option<BoxId> = None;
    let mut footer: Option<BoxId> = None;
    // Row groups and runs of direct rows, in document order.
    let mut body: Vec<Result<BoxId, Vec<BoxId>>> = Vec::new();
    for &c in tree.children(table) {
        let Some(b) = tree.get(c) else { continue };
        match part(b) {
            Some(DisplayInternal::TableCaption) => g.captions.push(c),
            Some(DisplayInternal::TableColumn) => add_column(ctx, &mut g, c, None),
            Some(DisplayInternal::TableColumnGroup) => add_column_group(ctx, &mut g, c),
            Some(DisplayInternal::TableHeaderGroup) if header.is_none() => header = Some(c),
            Some(DisplayInternal::TableFooterGroup) if footer.is_none() => footer = Some(c),
            Some(
                DisplayInternal::TableHeaderGroup
                | DisplayInternal::TableFooterGroup
                | DisplayInternal::TableRowGroup,
            ) => body.push(Ok(c)),
            Some(DisplayInternal::TableRow) => match body.last_mut() {
                Some(Err(run)) => run.push(c),
                _ => body.push(Err(vec![c])),
            },
            _ => {}
        }
    }
    let ordered = header.map(Ok).into_iter().chain(body).chain(footer.map(Ok));
    for item in ordered {
        let (group, rows): (Option<BoxId>, Vec<BoxId>) = match item {
            Ok(group) => (
                Some(group),
                tree.children(group)
                    .iter()
                    .copied()
                    .filter(|r| {
                        tree.get(*r)
                            .is_some_and(|b| part(b) == Some(DisplayInternal::TableRow))
                    })
                    .collect(),
            ),
            Err(rows) => (None, rows),
        };
        let start = g.rows.len();
        let room = MAX_TABLE_ROWS.saturating_sub(start);
        if rows.len() > room {
            g.truncated = true;
        }
        let si = g.sections.len();
        for r in rows.into_iter().take(room) {
            g.rows.push(r);
            g.row_section.push(si);
        }
        g.sections.push(Section {
            group,
            rows: start..g.rows.len(),
        });
    }
    let max_cols = MAX_TABLE_COLUMNS.min(MAX_GRID_SLOTS / g.rows.len().max(1));
    if g.columns.len() > max_cols {
        g.columns.truncate(max_cols);
        g.truncated = true;
    }
    place_cells(ctx, &mut g, max_cols);
    g
}

fn add_column(ctx: &LayoutCtx<'_>, g: &mut Grid, id: BoxId, group: Option<BoxId>) {
    let span = span_of(ctx, id).cols as usize;
    let first = g.columns.len();
    let count = span.min(MAX_TABLE_COLUMNS.saturating_sub(first));
    g.columns.extend(std::iter::repeat_n(
        Column {
            col: Some(id),
            group,
        },
        count,
    ));
    g.col_boxes.push(ColBox {
        id,
        first,
        count,
        kids: 0..0,
        in_group: group.is_some(),
    });
}

fn add_column_group(ctx: &LayoutCtx<'_>, g: &mut Grid, id: BoxId) {
    let index = g.col_boxes.len();
    let first = g.columns.len();
    g.col_boxes.push(ColBox {
        id,
        first,
        count: 0,
        kids: 0..0,
        in_group: false,
    });
    let cols: Vec<BoxId> = ctx.tree.children(id).to_vec();
    if cols.is_empty() {
        // HTML §4.9.3: a column group without columns spans `span`.
        let span = span_of(ctx, id).cols as usize;
        let count = span.min(MAX_TABLE_COLUMNS.saturating_sub(first));
        g.columns.extend(std::iter::repeat_n(
            Column {
                col: None,
                group: Some(id),
            },
            count,
        ));
    } else {
        for c in cols {
            add_column(ctx, g, c, Some(id));
        }
    }
    let end = g.col_boxes.len();
    let count = g.columns.len() - first;
    if let Some(cb) = g.col_boxes.get_mut(index) {
        cb.count = count;
        cb.kids = index + 1..end;
    }
}

/// Assigns cells to slots (HTML §4.9.12): each cell takes the first
/// column not covered by a cell from an earlier row. Linear in the grid
/// area.
fn place_cells(ctx: &LayoutCtx<'_>, g: &mut Grid, max_cols: usize) {
    let tree = ctx.tree;
    // For each column, the row index (exclusive) it is occupied until.
    let mut occupied: Vec<usize> = Vec::new();
    for sec in &g.sections {
        for r in sec.rows.clone() {
            let Some(&row) = g.rows.get(r) else { continue };
            let mut x = 0usize;
            for &cell in tree.children(row) {
                if tree
                    .get(cell)
                    .is_none_or(|b| part(b) != Some(DisplayInternal::TableCell))
                {
                    continue;
                }
                while occupied.get(x).is_some_and(|until| *until > r) {
                    x += 1;
                }
                if x >= max_cols {
                    g.truncated = true;
                    break;
                }
                let span = span_of(ctx, cell);
                let colspan = (span.cols as usize).clamp(1, max_cols - x);
                let left = sec.rows.end - r;
                let rowspan = match span.rows {
                    0 => left,
                    n => (n as usize).min(left),
                }
                .max(1);
                if occupied.len() < x + colspan {
                    occupied.resize(x + colspan, 0);
                }
                if let Some(slots) = occupied.get_mut(x..x + colspan) {
                    for s in slots {
                        *s = (*s).max(r + rowspan);
                    }
                }
                g.cells.push(Cell {
                    id: cell,
                    row: r,
                    col: x,
                    rowspan,
                    colspan,
                });
                x += colspan;
            }
        }
    }
    let ncols = occupied.len().max(g.columns.len()).min(max_cols);
    g.columns.resize(ncols, Column::default());
}

/// A border side that draws nothing.
fn no_border(s: &BorderSide) -> BorderSide {
    BorderSide {
        width: 0.0,
        style: LineStyle::None,
        color: s.color,
    }
}

fn has_visible_border(s: &ComputedStyle) -> bool {
    s.border.top.is_visible()
        || s.border.right.is_visible()
        || s.border.bottom.is_visible()
        || s.border.left.is_visible()
}

fn strip_borders(s: &mut ComputedStyle) {
    s.border.top = no_border(&s.border.top);
    s.border.right = no_border(&s.border.right);
    s.border.bottom = no_border(&s.border.bottom);
    s.border.left = no_border(&s.border.left);
}

/// The style of a row, row group or column fragment: no borders.
fn part_style(s: &Arc<ComputedStyle>) -> Option<Arc<ComputedStyle>> {
    has_visible_border(s).then(|| {
        let mut c = (**s).clone();
        strip_borders(&mut c);
        Arc::new(c)
    })
}

/// A style without box decorations (for the anonymous table wrapper and
/// for hidden empty cells).
fn undecorated(s: &ComputedStyle) -> ComputedStyle {
    let mut c = s.clone();
    strip_borders(&mut c);
    c.background_color = Rgba::TRANSPARENT;
    c.background_layers = Arc::new([]);
    c
}

/// Prepares table `id`: grid, border model, used edges. `basis` resolves
/// percentage padding.
fn prepare(ctx: &LayoutCtx<'_>, id: BoxId, basis: f32) -> Option<Prepared> {
    let b = ctx.get(id)?;
    let style = b.style.clone();
    let grid = build_grid(ctx, id);
    let collapse = style.border_collapse == BorderCollapse::Collapse;
    let (hs, vs) = if collapse {
        (0.0, 0.0)
    } else {
        (
            sane_non_negative(style.border_spacing.0),
            sane_non_negative(style.border_spacing.1),
        )
    };
    let own = box_edges(&style, basis);
    let mut p = Prepared {
        style: style.clone(),
        collapse,
        hs,
        vs,
        border: own.border,
        padding: own.padding,
        table_style: None,
        cell_border: Vec::with_capacity(grid.cells.len()),
        cell_padding: Vec::with_capacity(grid.cells.len()),
        cell_style: Vec::with_capacity(grid.cells.len()),
        grid,
    };
    for c in &p.grid.cells {
        let e = ctx
            .get(c.id)
            .map(|cb| box_edges(&cb.style, basis))
            .unwrap_or_default();
        p.cell_border.push(e.border);
        p.cell_padding.push(e.padding);
        p.cell_style.push(None);
    }
    if collapse {
        apply_collapsed_borders(ctx, &mut p);
    }
    Some(p)
}

/// The collapsing border model: resolves every grid edge and sets the
/// cells' and the table's used borders (half the collapsed widths) and
/// fragment styles (see the module docs).
fn apply_collapsed_borders(ctx: &LayoutCtx<'_>, p: &mut Prepared) {
    let g = &p.grid;
    let rows = g.rows.len();
    let cols = g.columns.len();
    p.padding = Edges::ZERO;
    if rows == 0 || cols == 0 {
        return;
    }
    let tree = ctx.tree;
    let style_of = |id: Option<BoxId>| id.and_then(|i| tree.get(i)).map(|b| &*b.style);
    let mut slots = vec![u32::MAX; rows * cols];
    for (i, c) in g.cells.iter().enumerate() {
        let Ok(i) = u32::try_from(i) else { break };
        for r in c.row..c.row + c.rowspan {
            if let Some(s) = slots.get_mut(r * cols + c.col..r * cols + c.col + c.colspan) {
                s.fill(i);
            }
        }
    }
    let slot = |r: usize, c: usize| slots.get(r * cols + c).copied().filter(|v| *v != u32::MAX);
    let cell_style = |i: u32| {
        g.cells
            .get(i as usize)
            .and_then(|c| tree.get(c.id))
            .map(|b| &*b.style)
    };
    let row_style = |r: usize| style_of(g.rows.get(r).copied());
    let section = |r: usize| g.row_section.get(r).copied();
    let section_style =
        |s: Option<usize>| style_of(s.and_then(|s| g.sections.get(s)).and_then(|s| s.group));
    let table = &*p.style;
    let mut out = borders::Grid {
        cols,
        h: Vec::with_capacity((rows + 1) * cols),
        v: Vec::with_capacity(rows * (cols + 1)),
    };
    let mut cands: Vec<(BorderSide, Origin)> = Vec::with_capacity(10);
    for line in 0..=rows {
        for c in 0..cols {
            cands.clear();
            let above = if line > 0 { slot(line - 1, c) } else { None };
            let below = if line < rows { slot(line, c) } else { None };
            if above.is_some() && above == below {
                out.h.push(Resolved::NONE);
                continue;
            }
            if let Some(s) = above.and_then(cell_style) {
                cands.push((s.border.bottom, Origin::Cell));
            }
            if let Some(s) = below.and_then(cell_style) {
                cands.push((s.border.top, Origin::Cell));
            }
            if line > 0
                && let Some(s) = row_style(line - 1)
            {
                cands.push((s.border.bottom, Origin::Row));
            }
            if line < rows
                && let Some(s) = row_style(line)
            {
                cands.push((s.border.top, Origin::Row));
            }
            let sa = if line > 0 { section(line - 1) } else { None };
            let sb = if line < rows { section(line) } else { None };
            if sa != sb {
                if let Some(s) = section_style(sa) {
                    cands.push((s.border.bottom, Origin::RowGroup));
                }
                if let Some(s) = section_style(sb) {
                    cands.push((s.border.top, Origin::RowGroup));
                }
            }
            if line == 0 || line == rows {
                let column = g.columns.get(c).copied().unwrap_or_default();
                let pick = |s: &ComputedStyle| {
                    if line == 0 {
                        s.border.top
                    } else {
                        s.border.bottom
                    }
                };
                if let Some(s) = style_of(column.col) {
                    cands.push((pick(s), Origin::Column));
                }
                if let Some(s) = style_of(column.group) {
                    cands.push((pick(s), Origin::ColumnGroup));
                }
                cands.push((pick(table), Origin::Table));
            }
            out.h.push(resolve_edge(&cands));
        }
    }
    for r in 0..rows {
        for line in 0..=cols {
            cands.clear();
            let left = if line > 0 { slot(r, line - 1) } else { None };
            let right = if line < cols { slot(r, line) } else { None };
            if left.is_some() && left == right {
                out.v.push(Resolved::NONE);
                continue;
            }
            if let Some(s) = left.and_then(cell_style) {
                cands.push((s.border.right, Origin::Cell));
            }
            if let Some(s) = right.and_then(cell_style) {
                cands.push((s.border.left, Origin::Cell));
            }
            if line == 0 || line == cols {
                let pick = |s: &ComputedStyle| {
                    if line == 0 {
                        s.border.left
                    } else {
                        s.border.right
                    }
                };
                if let Some(s) = row_style(r) {
                    cands.push((pick(s), Origin::Row));
                }
                if let Some(s) = section_style(section(r)) {
                    cands.push((pick(s), Origin::RowGroup));
                }
            }
            let cl = if line > 0 {
                g.columns.get(line - 1).copied()
            } else {
                None
            };
            let cr = g.columns.get(line).copied();
            if let Some(s) = style_of(cl.and_then(|c| c.col)) {
                cands.push((s.border.right, Origin::Column));
            }
            if let Some(s) = style_of(cr.and_then(|c| c.col)) {
                cands.push((s.border.left, Origin::Column));
            }
            let gl = cl.and_then(|c| c.group);
            let gr = cr.and_then(|c| c.group);
            if gl != gr {
                if let Some(s) = style_of(gl) {
                    cands.push((s.border.right, Origin::ColumnGroup));
                }
                if let Some(s) = style_of(gr) {
                    cands.push((s.border.left, Origin::ColumnGroup));
                }
            }
            if line == 0 {
                cands.push((table.border.left, Origin::Table));
            } else if line == cols {
                cands.push((table.border.right, Origin::Table));
            }
            out.v.push(resolve_edge(&cands));
        }
    }
    for (i, c) in g.cells.iter().enumerate() {
        let top = borders::Grid::widest((c.col..c.col + c.colspan).map(|x| out.h(c.row, x)));
        let bottom =
            borders::Grid::widest((c.col..c.col + c.colspan).map(|x| out.h(c.row + c.rowspan, x)));
        let left = borders::Grid::widest((c.row..c.row + c.rowspan).map(|y| out.v(y, c.col)));
        let right =
            borders::Grid::widest((c.row..c.row + c.rowspan).map(|y| out.v(y, c.col + c.colspan)));
        let edges = Edges::new(
            top.width / 2.0,
            right.width / 2.0,
            bottom.width / 2.0,
            left.width / 2.0,
        );
        if let Some(e) = p.cell_border.get_mut(i) {
            *e = edges;
        }
        if let Some(cs) = cell_style(u32::try_from(i).unwrap_or(u32::MAX)) {
            let mut s = cs.clone();
            s.border.top = top.side(edges.top);
            s.border.right = right.side(edges.right);
            s.border.bottom = bottom.side(edges.bottom);
            s.border.left = left.side(edges.left);
            if let Some(slot) = p.cell_style.get_mut(i) {
                *slot = Some(Arc::new(s));
            }
        }
    }
    let top = borders::Grid::widest((0..cols).map(|x| out.h(0, x)));
    let bottom = borders::Grid::widest((0..cols).map(|x| out.h(rows, x)));
    let left = out.v(0, 0);
    let right = out.v(0, cols);
    p.border = Edges::new(
        top.width / 2.0,
        right.width / 2.0,
        bottom.width / 2.0,
        left.width / 2.0,
    );
    let mut s = (*p.style).clone();
    s.border.top = top.side(p.border.top);
    s.border.right = right.side(p.border.right);
    s.border.bottom = bottom.side(p.border.bottom);
    s.border.left = left.side(p.border.left);
    p.table_style = Some(Arc::new(s));
}

/// The min-/max-content widths of a cell's contents.
fn cell_content_intrinsic(ctx: &mut LayoutCtx<'_>, id: BoxId, depth: usize) -> IntrinsicSizes {
    let Some(b) = ctx.get(id) else {
        return IntrinsicSizes::default();
    };
    match b.kind {
        BoxKind::Container {
            inline_children: true,
            ..
        } => inline::intrinsic_content(ctx, id, depth),
        _ => {
            let mut acc = IntrinsicSizes::default();
            for c in &b.children {
                let s = intrinsic_sizes(ctx, *c, depth + 1);
                acc.min = acc.min.max(s.min);
                acc.max = acc.max.max(s.max);
            }
            acc
        }
    }
}

/// The column measures of the automatic table layout.
fn measure_columns(ctx: &mut LayoutCtx<'_>, p: &Prepared, depth: usize) -> Vec<ColMeasure> {
    let g = &p.grid;
    let mut cols: Vec<ColMeasure> = g
        .columns
        .iter()
        .map(|c| {
            let mut m = ColMeasure::default();
            // A `<col>` width, else its group's (HTML: applies to each
            // column of the group).
            for src in [c.col, c.group].into_iter().flatten() {
                let Some(b) = ctx.get(src) else { continue };
                match &b.style.width {
                    CssSize::LengthPercentage(LengthPercentage::Percentage(pct)) => {
                        m.pct = sane_non_negative(*pct);
                        break;
                    }
                    CssSize::LengthPercentage(lp) if !lp.has_percentage() => {
                        let w = sane_non_negative(lp.resolve(0.0));
                        m.max = m.max.max(w);
                        m.fixed = true;
                        break;
                    }
                    _ => {}
                }
            }
            m
        })
        .collect();
    let mut cells = Vec::with_capacity(g.cells.len());
    for (i, c) in g.cells.iter().enumerate() {
        let Some(b) = ctx.get(c.id) else { continue };
        let style = b.style.clone();
        let bp = p
            .cell_border
            .get(i)
            .copied()
            .unwrap_or_default()
            .add(&p.cell_padding.get(i).copied().unwrap_or_default())
            .horizontal();
        let content = cell_content_intrinsic(ctx, c.id, depth + 3);
        let min_w = min_size(&style, &style.min_width, None, bp);
        let min = sane_non_negative(content.min.max(min_w) + bp);
        let (pct, fixed) = match &style.width {
            CssSize::LengthPercentage(LengthPercentage::Percentage(v)) => {
                (sane_non_negative(*v), None)
            }
            CssSize::LengthPercentage(lp) if !lp.has_percentage() => {
                match width_spec(&style, None, bp) {
                    SizeSpec::Px(w) => (0.0, Some(w + bp)),
                    _ => (0.0, None),
                }
            }
            _ => (0.0, None),
        };
        let max = match fixed {
            Some(w) => w.max(min),
            None => (content.max.max(min_w) + bp).max(min),
        };
        cells.push(CellMeasure {
            col: c.col,
            span: c.colspan,
            min,
            max: sane_non_negative(max),
            pct,
            fixed: fixed.is_some(),
        });
    }
    columns::measure(&mut cols, &cells, p.hs);
    cols
}

/// Column widths given by columns and first-row cells, for the fixed
/// table layout.
fn fixed_given(ctx: &LayoutCtx<'_>, p: &Prepared) -> Vec<FixedWidth> {
    let g = &p.grid;
    let width_of = |style: &ComputedStyle, bp: f32| match &style.width {
        CssSize::LengthPercentage(LengthPercentage::Percentage(v)) => {
            FixedWidth::Pct(sane_non_negative(*v))
        }
        CssSize::LengthPercentage(lp) if !lp.has_percentage() => {
            match width_spec(style, None, bp) {
                SizeSpec::Px(w) => FixedWidth::Px(w + bp),
                _ => FixedWidth::Auto,
            }
        }
        _ => FixedWidth::Auto,
    };
    let mut given: Vec<FixedWidth> = g
        .columns
        .iter()
        .map(|c| {
            [c.col, c.group]
                .into_iter()
                .flatten()
                .filter_map(|id| ctx.get(id))
                .map(|b| width_of(&b.style, 0.0))
                .find(|w| *w != FixedWidth::Auto)
                .unwrap_or(FixedWidth::Auto)
        })
        .collect();
    for (i, c) in g.cells.iter().enumerate() {
        if c.row != 0 {
            break;
        }
        let Some(b) = ctx.get(c.id) else { continue };
        let bp = p
            .cell_border
            .get(i)
            .copied()
            .unwrap_or_default()
            .add(&p.cell_padding.get(i).copied().unwrap_or_default())
            .horizontal();
        #[allow(clippy::cast_precision_loss)]
        let n = c.colspan as f32;
        let w = match width_of(&b.style, bp) {
            FixedWidth::Px(w) => FixedWidth::Px(w / n),
            FixedWidth::Pct(v) => FixedWidth::Pct(v / n),
            FixedWidth::Auto => continue,
        };
        if let Some(slots) = given.get_mut(c.col..c.col + c.colspan) {
            for s in slots.iter_mut().filter(|s| **s == FixedWidth::Auto) {
                *s = w;
            }
        }
    }
    given
}

fn spacing(n: usize, s: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    if n > 0 { s * (n + 1) as f32 } else { 0.0 }
}

fn uses_fixed_layout(style: &ComputedStyle, spec: SizeSpec) -> bool {
    style.table_layout == TableLayout::Fixed && matches!(spec, SizeSpec::Px(_))
}

/// The largest min-content contribution of the captions.
fn caption_min(ctx: &mut LayoutCtx<'_>, g: &Grid, depth: usize) -> f32 {
    g.captions
        .iter()
        .map(|c| intrinsic_sizes(ctx, *c, depth + 1).min)
        .fold(0.0, f32::max)
}

/// The min-/max-content contributions of table `id` (CSS Tables 3 §3.9:
/// the columns' widths plus spacing, border and padding, at least the
/// captions' min-content width), margins included.
pub(crate) fn intrinsic(ctx: &mut LayoutCtx<'_>, id: BoxId, depth: usize) -> IntrinsicSizes {
    if !is_table(ctx, id) {
        return IntrinsicSizes::default();
    }
    let Some(p) = prepare(ctx, id, 0.0) else {
        return IntrinsicSizes::default();
    };
    let style = p.style.clone();
    let bp = p.border.add(&p.padding).horizontal();
    let sp = spacing(p.grid.columns.len(), p.hs);
    let spec = width_spec(&style, None, bp);
    let (min, max) = if uses_fixed_layout(&style, spec) {
        let w = match spec {
            SizeSpec::Px(w) => w,
            _ => 0.0,
        };
        (w, w)
    } else {
        let cols = measure_columns(ctx, &p, depth);
        let gmin = columns::min_width(&cols) + sp;
        let gmax = columns::max_width(&cols) + sp;
        match spec {
            SizeSpec::Px(w) => (w.max(gmin), w.max(gmin)),
            _ => (gmin, gmax.max(gmin)),
        }
    };
    let min_w = min_size(&style, &style.min_width, None, bp);
    let max_w = max_size(&style, &style.max_width, None, bp);
    let cap = (caption_min(ctx, &p.grid, depth) - bp).max(0.0);
    let floor = |v: f32| clamp_min_max(v, min_w, max_w).max(cap);
    let edges = box_edges(&style, 0.0);
    let outer = bp + edges.margin.left.unwrap_or(0.0) + edges.margin.right.unwrap_or(0.0);
    let min = floor(min).max(min);
    let max = floor(max).max(min);
    IntrinsicSizes {
        min: sane_non_negative(min + outer).min(MAX_COORD),
        max: sane_non_negative(max + outer).min(MAX_COORD),
    }
}

/// One laid out cell before its row heights are known.
struct CellLayout {
    content: block::ContentLayout,
    width: f32,
    /// Border-box height the cell needs.
    height: f32,
    /// Baseline from the border-box top.
    baseline: f32,
    align: Align,
    empty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    Top,
    Middle,
    Bottom,
    Baseline,
}

/// CSS 2.2 §17.5.3: `top`, `middle` and `bottom` align the cell's
/// content in the row; every other value behaves as `baseline`.
fn align_of(v: &VerticalAlign) -> Align {
    match v {
        VerticalAlign::Keyword(VerticalAlignKeyword::Top) => Align::Top,
        VerticalAlign::Keyword(VerticalAlignKeyword::Middle) => Align::Middle,
        VerticalAlign::Keyword(VerticalAlignKeyword::Bottom) => Align::Bottom,
        _ => Align::Baseline,
    }
}

/// Splits `extra` over `sizes` in proportion to them (equally if all 0).
fn grow(sizes: &mut [f32], extra: f32) {
    if extra <= 0.0 || sizes.is_empty() {
        return;
    }
    let total: f32 = sizes.iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let n = sizes.len() as f32;
    for s in sizes.iter_mut() {
        *s += if total > 0.0 {
            extra * *s / total
        } else {
            extra / n
        };
    }
}

/// Lays out a table box (or, past the nesting limit or for a stray
/// table-internal box, a block container).
pub(crate) fn layout_table_box(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    mode: WidthMode,
) -> LayoutResult {
    if !is_table(ctx, id) {
        if ctx
            .get(id)
            .is_some_and(|b| b.formatting() == Some(Formatting::Table))
        {
            ctx.truncated = true;
        }
        return block::layout_block_box(ctx, id, cb, depth, mode);
    }
    let Some(b) = ctx.get(id) else {
        return LayoutResult::default();
    };
    let Some(p) = prepare(ctx, id, cb.width) else {
        return LayoutResult::default();
    };
    if p.grid.truncated {
        ctx.truncated = true;
    }
    let style = p.style.clone();
    let bp = p.border.add(&p.padding);
    let bp_h = bp.horizontal();
    let bp_v = bp.vertical();
    let edges = box_edges(&style, cb.width);
    let ncols = p.grid.columns.len();
    let nrows = p.grid.rows.len();
    let sp_w = spacing(ncols, p.hs);
    let sp_h = spacing(nrows, p.vs);
    let cap_min = caption_min(ctx, &p.grid, depth);

    // Widths (§17.5.2).
    let spec = width_spec(&style, Some(cb.width), bp_h);
    // The table wrapper's width goes through the shared width equation in
    // shrink-to-fit mode (tables are shrink-to-fit, §17.5.2.2: auto is
    // min(max-content, max(min-content, available))), so that sizes
    // imposed by other formatting contexts apply to tables too. Its
    // intrinsic contributions come from [`intrinsic`].
    let used_edges = BoxEdges {
        margin: edges.margin,
        border: p.border,
        padding: p.padding,
    };
    let hl = solve_width(
        ctx,
        id,
        &style,
        cb.width,
        &used_edges,
        WidthMode::ShrinkToFit,
        depth,
    );
    let (content_w, mut widths) = if uses_fixed_layout(&style, spec) {
        let w = hl.content_width.max(cap_min - bp_h);
        let given = fixed_given(ctx, &p);
        let widths = columns::fixed(&given, w - sp_w);
        let sum: f32 = widths.iter().sum();
        (w.max(sum + sp_w), widths)
    } else {
        let cols = measure_columns(ctx, &p, depth);
        let gmin = columns::min_width(&cols) + sp_w;
        let w = sane_non_negative(hl.content_width.max(gmin).max(cap_min - bp_h));
        (w, columns::distribute(&cols, w - sp_w))
    };
    let content_w = sane_non_negative(content_w);
    for (w, c) in widths.iter_mut().zip(&p.grid.columns) {
        let collapsed = [c.col, c.group]
            .into_iter()
            .flatten()
            .filter_map(|id| ctx.get(id))
            .any(|b| b.style.visibility == Visibility::Collapse);
        if collapsed {
            *w = 0.0;
        }
    }
    widths.resize(ncols, 0.0);
    // Column left edges in the table's border box.
    let mut xs = Vec::with_capacity(ncols);
    let mut x = bp.left + p.hs;
    for w in &widths {
        xs.push(x);
        x = sane(x + w + p.hs);
    }
    let col_x = |c: usize| xs.get(c).copied().unwrap_or(bp.left);
    let col_end = |c: usize| col_x(c) + widths.get(c).copied().unwrap_or(0.0);
    let grid_left = col_x(0);
    let grid_right = if ncols > 0 {
        col_end(ncols - 1)
    } else {
        bp.left
    };

    // Cells (§17.5.3).
    let mut laid: Vec<CellLayout> = Vec::with_capacity(p.grid.cells.len());
    for (i, c) in p.grid.cells.iter().enumerate() {
        let Some(cbox) = ctx.get(c.id) else {
            laid.push(CellLayout {
                content: block::ContentLayout::default(),
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
                align: Align::Top,
                empty: true,
            });
            continue;
        };
        let border = p.cell_border.get(i).copied().unwrap_or_default();
        let padding = p.cell_padding.get(i).copied().unwrap_or_default();
        let cbp = border.add(&padding);
        let width = sane_non_negative(col_end(c.col + c.colspan - 1) - col_x(c.col));
        let ccb = ContainingBlock {
            width: (width - cbp.horizontal()).max(0.0),
            height: None,
        };
        let content = if ctx.exhausted() {
            ctx.truncated = true;
            block::ContentLayout::default()
        } else {
            block::layout_contents(ctx, cbox, c.id, &ccb, depth + 3, false, false)
        };
        let ch = used_height(&cbox.style, content.height, None, cbp.vertical());
        let empty = cbox.children.is_empty()
            || (matches!(
                cbox.kind,
                BoxKind::Container {
                    inline_children: true,
                    ..
                }
            ) && content.empty);
        laid.push(CellLayout {
            baseline: cbp.top + content.first_baseline.unwrap_or(ch),
            height: ch + cbp.vertical(),
            width,
            align: align_of(&cbox.style.vertical_align),
            empty,
            content,
        });
    }

    // Row heights.
    let mut row_h: Vec<f32> = p
        .grid
        .rows
        .iter()
        .map(|r| {
            ctx.get(*r).map_or(0.0, |rb| {
                let s = &rb.style;
                height_px(s, None, 0.0)
                    .unwrap_or(0.0)
                    .max(min_size(s, &s.min_height, None, 0.0))
            })
        })
        .collect();
    let mut above: Vec<Option<f32>> = vec![None; nrows];
    let mut below: Vec<f32> = vec![0.0; nrows];
    for (c, l) in p.grid.cells.iter().zip(&laid) {
        if l.align == Align::Baseline {
            if let Some(a) = above.get_mut(c.row) {
                *a = Some(a.unwrap_or(0.0).max(l.baseline));
            }
            if c.rowspan == 1
                && let Some(b) = below.get_mut(c.row)
            {
                *b = b.max(l.height - l.baseline);
            }
        } else if c.rowspan == 1
            && let Some(h) = row_h.get_mut(c.row)
        {
            *h = h.max(l.height);
        }
    }
    for ((h, a), b) in row_h.iter_mut().zip(&above).zip(&below) {
        *h = sane_non_negative(h.max(a.unwrap_or(0.0) + b));
    }
    for (c, l) in p.grid.cells.iter().zip(&laid) {
        if c.rowspan > 1
            && let Some(hs) = row_h.get_mut(c.row..c.row + c.rowspan)
        {
            #[allow(clippy::cast_precision_loss)]
            let total = hs.iter().sum::<f32>() + p.vs * (c.rowspan - 1) as f32;
            if l.height > total {
                #[allow(clippy::cast_precision_loss)]
                let each = (l.height - total) / c.rowspan as f32;
                for h in hs {
                    *h += each;
                }
            }
        }
    }
    for (h, r) in row_h.iter_mut().zip(&p.grid.rows) {
        if ctx
            .get(*r)
            .is_some_and(|b| b.style.visibility == Visibility::Collapse)
        {
            *h = 0.0;
        }
    }
    // A taller table makes its rows taller (§17.5.3).
    // M1 follow-up: honour a height imposed by a flex container
    // (`ctx.flex.forced(id)` on the flex branch) once both are merged.
    let rows_total: f32 = row_h.iter().sum::<f32>() + sp_h;
    let content_h = used_height(&style, rows_total, cb.height, bp_v).max(rows_total);
    if nrows > 0 {
        grow(&mut row_h, content_h - rows_total);
    }
    let mut ys = Vec::with_capacity(nrows);
    let mut y = bp.top + p.vs;
    for h in &row_h {
        ys.push(y);
        y = sane(y + h + p.vs);
    }
    let row_y = |r: usize| ys.get(r).copied().unwrap_or(bp.top);
    let row_end = |r: usize| row_y(r) + row_h.get(r).copied().unwrap_or(0.0);

    // Cell fragments, grouped by starting row.
    let mut row_cells: Vec<Vec<FragmentId>> = vec![Vec::new(); nrows];
    let mut first_baseline: Option<f32> = None;
    for (i, (c, l)) in p.grid.cells.iter().zip(laid).enumerate() {
        let Some(cbox) = ctx.get(c.id) else { continue };
        let border = p.cell_border.get(i).copied().unwrap_or_default();
        let padding = p.cell_padding.get(i).copied().unwrap_or_default();
        let cbp = border.add(&padding);
        let last = c.row + c.rowspan - 1;
        let height = sane_non_negative(row_end(last) - row_y(c.row));
        let room = height - cbp.vertical();
        let shift = match l.align {
            Align::Top => 0.0,
            Align::Middle => (room - l.content.height) / 2.0,
            Align::Bottom => room - l.content.height,
            Align::Baseline => {
                above.get(c.row).copied().flatten().unwrap_or(l.baseline) - l.baseline
            }
        }
        .max(0.0);
        if c.row == 0 && first_baseline.is_none() {
            first_baseline = Some(
                row_y(0)
                    + above
                        .first()
                        .copied()
                        .flatten()
                        .unwrap_or(l.baseline + shift),
            );
        }
        let offset = Point::new(cbp.left, cbp.top + shift);
        for f in &l.content.children {
            ctx.translate(*f, offset);
        }
        let Some(f) = ctx.box_fragment(
            cbox,
            Some(c.id),
            FragmentKind::Box(BoxFragmentKind::Block),
            Size::new(l.width, height),
            border,
            padding,
            l.content.children,
            false,
        ) else {
            continue;
        };
        let override_style = if !p.collapse && l.empty && cbox.style.empty_cells == EmptyCells::Hide
        {
            Some(Arc::new(undecorated(&cbox.style)))
        } else {
            p.cell_style.get(i).cloned().flatten()
        };
        if let Some(s) = override_style
            && let Some(fr) = ctx.frag_mut(f)
        {
            fr.style = s;
        }
        ctx.set_position(f, Point::new(col_x(c.col) - grid_left, 0.0));
        positioned::apply_relative(
            ctx,
            f,
            &cbox.style,
            &ContainingBlock {
                width: content_w,
                height: None,
            },
        );
        if let Some(v) = row_cells.get_mut(c.row) {
            v.push(f);
        }
    }

    // Row, row group and column fragments.
    let grid_w = sane_non_negative(grid_right - grid_left);
    let mut table_children: Vec<FragmentId> = Vec::new();
    let grid_top = row_y(0);
    let grid_h = if nrows > 0 {
        sane_non_negative(row_end(nrows - 1) - grid_top)
    } else {
        0.0
    };
    let col_frag = |ctx: &mut LayoutCtx<'_>, cbx: &ColBox, kids: Vec<FragmentId>| {
        let b = ctx.get(cbx.id)?;
        if cbx.count == 0 || cbx.first >= ncols {
            return None;
        }
        let last = (cbx.first + cbx.count - 1).min(ncols - 1);
        let x0 = col_x(cbx.first);
        let size = Size::new(sane_non_negative(col_end(last) - x0), grid_h);
        let f = part_fragment(ctx, b, cbx.id, size, kids)?;
        Some((f, x0))
    };
    let mut ci = 0;
    while let Some(cbx) = p.grid.col_boxes.get(ci) {
        ci += 1;
        if cbx.in_group {
            continue;
        }
        let mut kids = Vec::new();
        let gx = col_x(cbx.first);
        for k in cbx.kids.clone() {
            if let Some(kb) = p.grid.col_boxes.get(k)
                && let Some((f, x0)) = col_frag(ctx, kb, Vec::new())
            {
                ctx.set_position(f, Point::new(x0 - gx, 0.0));
                kids.push(f);
            }
        }
        if let Some((f, x0)) = col_frag(ctx, cbx, kids) {
            ctx.set_position(f, Point::new(x0, grid_top));
            table_children.push(f);
        }
    }
    for sec in &p.grid.sections {
        let (top, bottom) = if sec.rows.is_empty() {
            continue;
        } else {
            (row_y(sec.rows.start), row_end(sec.rows.end - 1))
        };
        let mut rows = Vec::with_capacity(sec.rows.len());
        let base = if sec.group.is_some() { top } else { 0.0 };
        for r in sec.rows.clone() {
            let Some(&rid) = p.grid.rows.get(r) else {
                continue;
            };
            let Some(rb) = ctx.get(rid) else { continue };
            let kids = row_cells.get_mut(r).map(std::mem::take).unwrap_or_default();
            let size = Size::new(grid_w, row_h.get(r).copied().unwrap_or(0.0));
            if let Some(f) = part_fragment(ctx, rb, rid, size, kids) {
                let gx = if sec.group.is_some() { 0.0 } else { grid_left };
                ctx.set_position(f, Point::new(gx, row_y(r) - base));
                rows.push(f);
            }
        }
        match sec.group.and_then(|g| ctx.get(g).map(|b| (g, b))) {
            Some((gid, gb)) => {
                let size = Size::new(grid_w, sane_non_negative(bottom - top));
                if let Some(f) = part_fragment(ctx, gb, gid, size, rows) {
                    ctx.set_position(f, Point::new(grid_left, top));
                    table_children.push(f);
                }
            }
            None => table_children.extend(rows),
        }
    }

    // The table box.
    let table_size = Size::new(content_w + bp_h, content_h + bp_v);
    let has_captions = !p.grid.captions.is_empty();
    let level_kind = match b.level {
        Level::Block => BoxFragmentKind::Block,
        Level::Inline => BoxFragmentKind::AtomicInline,
    };
    let table_kind = if has_captions {
        BoxFragmentKind::Block
    } else {
        level_kind
    };
    let table_frag = ctx.box_fragment(
        b,
        Some(id),
        FragmentKind::Box(table_kind),
        table_size,
        p.border,
        p.padding,
        table_children,
        b.parent.is_none(),
    );
    if let Some(s) = p.table_style.clone()
        && let Some(f) = table_frag
        && let Some(fr) = ctx.frag_mut(f)
    {
        fr.style = s;
    }

    // The wrapper box and captions (§17.4, §17.4.1).
    let (fragment, size, table_y) = if has_captions {
        let ccb = ContainingBlock {
            width: table_size.width,
            height: None,
        };
        let mut kids = Vec::new();
        let mut y = 0.0f32;
        let mut table_y = 0.0;
        for bottom_side in [false, true] {
            if bottom_side {
                if let Some(f) = table_frag {
                    ctx.set_position(f, Point::new(0.0, y));
                    kids.push(f);
                }
                table_y = y;
                y = sane(y + table_size.height);
            }
            for &cap in &p.grid.captions {
                let Some(capb) = ctx.get(cap) else { continue };
                let is_bottom = capb.style.caption_side == wb_style::CaptionSide::Bottom;
                if is_bottom != bottom_side {
                    continue;
                }
                let r = layout_block_level(ctx, cap, &ccb, depth + 1);
                let Some(f) = r.fragment else { continue };
                ctx.set_position(f, Point::new(r.margin.left, sane(y + r.margin.top)));
                positioned::apply_relative(ctx, f, &capb.style, &ccb);
                kids.push(f);
                y = sane(y + r.margin_box().height);
            }
        }
        let wrapper = LayoutBox {
            anonymous: true,
            style: Arc::new(undecorated(&style)),
            ..b.clone()
        };
        let size = Size::new(table_size.width, sane_non_negative(y));
        let f = ctx.box_fragment(
            &wrapper,
            Some(id),
            FragmentKind::Box(level_kind),
            size,
            Edges::ZERO,
            Edges::ZERO,
            kids,
            false,
        );
        (f, size, table_y)
    } else {
        (table_frag, table_size, 0.0)
    };

    let ml = edges.margin.left;
    let mr = edges.margin.right;
    let (ml, mr) = match mode {
        WidthMode::Stretch if b.level == Level::Block => {
            solve_margins(cb.width, size.width, ml, mr)
        }
        _ => (ml.unwrap_or(0.0), mr.unwrap_or(0.0)),
    };
    let mt = edges.margin.top.unwrap_or(0.0);
    let mb = edges.margin.bottom.unwrap_or(0.0);
    let baseline = first_baseline.map(|v| sane(v + table_y));
    LayoutResult {
        fragment,
        size,
        margin: Edges::new(mt, mr, mb, ml),
        collapsed_top: CollapsedMargin::new(mt),
        collapsed_bottom: CollapsedMargin::new(mb),
        collapses_through: false,
        first_baseline: baseline,
        last_baseline: baseline,
    }
}

/// A fragment for a row, row group, column or column group: no border or
/// padding of its own (see the module docs).
fn part_fragment(
    ctx: &mut LayoutCtx<'_>,
    b: &LayoutBox,
    id: BoxId,
    size: Size,
    kids: Vec<FragmentId>,
) -> Option<FragmentId> {
    let f = ctx.box_fragment(
        b,
        Some(id),
        FragmentKind::Box(BoxFragmentKind::Block),
        size,
        Edges::ZERO,
        Edges::ZERO,
        kids,
        false,
    )?;
    if let Some(s) = part_style(&b.style)
        && let Some(fr) = ctx.frag_mut(f)
    {
        fr.style = s;
    }
    Some(f)
}
