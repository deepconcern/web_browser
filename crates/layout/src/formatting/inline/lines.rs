//! Line breaking (CSS Text 3 §5, CSS 2.2 §9.4.2): the paragraph is cut
//! into units between break opportunities, then units are packed into
//! lines greedily. Trailing collapsible spaces hang (§4.1.2, phase II) and
//! do not count when checking whether a unit fits. Units that cannot fit
//! on an empty line are broken anywhere when `overflow-wrap` allows it
//! (§5.5). Everything is linear in the paragraph length.

use wb_style::{OverflowWrap, WhiteSpace, WordBreak};

use super::prepare::{ItemKind, Prepared};

/// Slack for floating-point comparisons when fitting units.
const EPSILON: f32 = 0.01;

/// The text between two consecutive break opportunities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Unit {
    pub start: usize,
    pub end: usize,
    /// Items `item_start..item_end` take part (the first and last may be
    /// text items that only partly fall in the unit).
    pub item_start: usize,
    pub item_end: usize,
    /// The advance of the unit (tabs counted as 0).
    pub width: f32,
    /// The advance of trailing spaces that hang at the end of a line.
    pub hang: f32,
    /// A forced break follows the unit.
    pub mandatory: bool,
    pub has_tab: bool,
    /// The unit holds something that gives a line box height (CSS 2.2
    /// §9.4.2): text other than collapsible spaces, preserved white space,
    /// an atomic inline, a forced break, or an inline box edge with width.
    pub content: bool,
}

/// A line: the span of the paragraph it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LineSpan {
    pub start: usize,
    pub item_start: usize,
    pub end: usize,
    pub item_end: usize,
    /// The advance of the line's content, without hanging spaces.
    pub width: f32,
    /// The line ends in a forced break (or is the last line).
    pub forced: bool,
    pub content: bool,
}

/// Whether white space in `ws` collapses (and so is removed, or hangs, at
/// the end of a line).
pub(crate) fn spaces_hang(ws: WhiteSpace) -> bool {
    matches!(
        ws,
        WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine | WhiteSpace::PreWrap
    )
}

fn collapsible(ws: WhiteSpace) -> bool {
    matches!(
        ws,
        WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
    )
}

/// The advance of a tab at pen position `x` (CSS Text 3 §4.2: tab stops
/// every 8 spaces).
// M1 follow-up: the `tab-size` property.
pub(crate) fn tab_advance(x: f32, space: f32) -> f32 {
    let stop = space * 8.0;
    if stop.is_nan() || stop <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    stop - x.rem_euclid(stop)
}

/// Measures items `items` restricted to paragraph bytes `a..b`, starting
/// at pen position `x0` (for tabs). `fixed` holds the widths of
/// open/close edges and atomic inlines by item index.
pub(crate) fn span_width(
    prep: &Prepared,
    fixed: &[f32],
    a: usize,
    b: usize,
    items: std::ops::Range<usize>,
    x0: f32,
) -> f32 {
    let mut x = x0;
    for i in items {
        let Some(it) = prep.items.get(i) else { break };
        match &it.kind {
            ItemKind::Text(t) => {
                let s = a.max(it.range.start).saturating_sub(it.range.start);
                let e = b.min(it.range.end).saturating_sub(it.range.start);
                x += t.measure(s, e);
            }
            ItemKind::Tab => {
                if it.range.start >= a && it.range.start < b {
                    x += tab_advance(x, prep.tab_space.get(i).copied().unwrap_or(0.0));
                }
            }
            _ => x += fixed.get(i).copied().unwrap_or(0.0),
        }
    }
    x - x0
}

/// Cuts the paragraph into units. `fixed` holds edge and atomic widths.
pub(crate) fn build_units(prep: &Prepared, fixed: &[f32]) -> Vec<Unit> {
    let mut units = Vec::with_capacity(prep.breaks.len() + 1);
    let n = prep.items.len();
    let mut i = 0usize;
    let mut a = 0usize;
    for brk in &prep.breaks {
        let b = brk.offset;
        if b <= a && !units.is_empty() {
            continue;
        }
        let item_start = i;
        let mut j = i;
        let mut partial = false;
        while let Some(it) = prep.items.get(j) {
            if it.range.start > b {
                break;
            }
            if it.range.start == b {
                if it.range.is_empty() && matches!(it.kind, ItemKind::Close) {
                    j += 1;
                    continue;
                }
                break;
            }
            if it.range.end > b {
                partial = true;
                break;
            }
            j += 1;
        }
        let item_end = if partial { j + 1 } else { j };
        units.push(make_unit(
            prep,
            fixed,
            a,
            b,
            item_start,
            item_end,
            brk.mandatory,
        ));
        i = j;
        a = b;
    }
    // Items after the last break (or a paragraph with no text at all: only
    // empty inline boxes).
    if i < n {
        let end = prep.text.len();
        match units.last_mut() {
            Some(last) if a >= end => {
                let mandatory = last.mandatory;
                let (start, item_start) = (last.start, last.item_start);
                *last = make_unit(prep, fixed, start, end, item_start, n, mandatory);
            }
            _ => units.push(make_unit(prep, fixed, a, end, i, n, true)),
        }
    }
    units
}

fn make_unit(
    prep: &Prepared,
    fixed: &[f32],
    a: usize,
    b: usize,
    item_start: usize,
    item_end: usize,
    mandatory: bool,
) -> Unit {
    let mut width = 0.0f32;
    let mut has_tab = false;
    let mut content = false;
    for i in item_start..item_end {
        let Some(it) = prep.items.get(i) else { break };
        match &it.kind {
            ItemKind::Text(t) => {
                let s = a.max(it.range.start).saturating_sub(it.range.start);
                let e = b.min(it.range.end).saturating_sub(it.range.start);
                width += t.measure(s, e);
                if !content {
                    let slice = t.text.get(s..e).unwrap_or("");
                    content = !collapsible(it.white_space) && !slice.is_empty()
                        || slice.chars().any(|c| c != ' ');
                }
            }
            ItemKind::Tab => {
                has_tab = true;
                content = true;
            }
            ItemKind::Atomic | ItemKind::ForcedBreak => {
                width += fixed.get(i).copied().unwrap_or(0.0);
                content = true;
            }
            ItemKind::Open | ItemKind::Close => {
                let w = fixed.get(i).copied().unwrap_or(0.0);
                width += w;
                content |= w != 0.0;
            }
            ItemKind::WordBreak => {}
        }
    }
    let hang = trailing_hang(prep, a, b, item_start, item_end).1;
    Unit {
        start: a,
        end: b,
        item_start,
        item_end,
        width,
        hang,
        mandatory,
        has_tab,
        content,
    }
}

/// The trailing spaces of `a..b` that hang or are removed at the end of a
/// line: returns the offset where they start and their advance.
pub(crate) fn trailing_hang(
    prep: &Prepared,
    a: usize,
    b: usize,
    item_start: usize,
    item_end: usize,
) -> (usize, f32) {
    let mut cut = b;
    let mut hang = 0.0f32;
    for i in (item_start..item_end).rev() {
        let Some(it) = prep.items.get(i) else {
            continue;
        };
        match &it.kind {
            ItemKind::Text(t) => {
                let s = a.max(it.range.start);
                let e = b.min(it.range.end);
                if e <= s {
                    continue;
                }
                if !spaces_hang(it.white_space) {
                    return (cut, hang);
                }
                let local_s = s - it.range.start;
                let local_e = e - it.range.start;
                let slice = t.text.get(local_s..local_e).unwrap_or("");
                let trimmed = slice.trim_end_matches(' ');
                let new_e = local_s + trimmed.len();
                hang += t.measure(new_e, local_e);
                cut = it.range.start + new_e;
                if !trimmed.is_empty() {
                    return (cut, hang);
                }
            }
            ItemKind::ForcedBreak | ItemKind::WordBreak | ItemKind::Open | ItemKind::Close => {}
            ItemKind::Atomic | ItemKind::Tab => return (cut, hang),
        }
    }
    (cut, hang)
}

fn may_break_anywhere(prep: &Prepared, items: std::ops::Range<usize>) -> bool {
    items
        .filter_map(|i| prep.items.get(i))
        .find(|it| matches!(it.kind, ItemKind::Text(_)))
        .is_some_and(|it| {
            matches!(
                it.style.overflow_wrap,
                OverflowWrap::Anywhere | OverflowWrap::BreakWord
            ) || matches!(it.style.word_break, WordBreak::BreakWord)
        })
}

/// Packs units into lines of width `avail`; the first line starts at
/// `indent`. Stops after `max_lines` lines.
pub(crate) fn break_lines(
    prep: &Prepared,
    fixed: &[f32],
    units: &[Unit],
    avail: f32,
    indent: f32,
    max_lines: usize,
) -> Vec<LineSpan> {
    break_lines_from(prep, fixed, units, None, avail, indent, max_lines)
}

/// [`break_lines`] starting with `first` (the rest of a unit split by an
/// emergency break) before `units`; the float module breaks lines one at
/// a time with it.
pub(crate) fn break_lines_from(
    prep: &Prepared,
    fixed: &[f32],
    units: &[Unit],
    first: Option<Unit>,
    avail: f32,
    indent: f32,
    max_lines: usize,
) -> Vec<LineSpan> {
    let mut lines: Vec<LineSpan> = Vec::new();
    let mut cur: Option<LineSpan> = None;
    let mut x = indent;
    let mut last_hang = 0.0f32;
    let mut k = 0usize;
    // The current unit, possibly with its front already placed on earlier
    // lines by an emergency break.
    let mut pending: Option<Unit> = first;
    loop {
        if lines.len() >= max_lines {
            return lines;
        }
        let u = match pending.take() {
            Some(u) => u,
            None => match units.get(k) {
                Some(u) => {
                    k += 1;
                    *u
                }
                None => break,
            },
        };
        let w = if u.has_tab {
            span_width(prep, fixed, u.start, u.end, u.item_start..u.item_end, x)
        } else {
            u.width
        };
        let fits = x + w - u.hang <= avail + EPSILON;
        if let Some(line) = cur
            && !fits
        {
            // Break before this unit.
            lines.push(LineSpan {
                width: x - last_hang,
                ..line
            });
            cur = None;
            x = 0.0;
            pending = Some(u);
            continue;
        }
        if cur.is_none()
            && !fits
            && may_break_anywhere(prep, u.item_start..u.item_end)
            && let Some((split, split_item, used)) = emergency_split(prep, fixed, &u, x, avail)
        {
            lines.push(LineSpan {
                start: u.start,
                item_start: u.item_start,
                end: split,
                item_end: split_item + 1,
                width: x + used,
                forced: false,
                content: true,
            });
            x = 0.0;
            pending = Some(Unit {
                start: split,
                item_start: split_item,
                width: (u.width - used).max(0.0),
                ..u
            });
            continue;
        }
        let line = cur.get_or_insert(LineSpan {
            start: u.start,
            item_start: u.item_start,
            end: u.end,
            item_end: u.item_end,
            width: 0.0,
            forced: false,
            content: false,
        });
        line.end = u.end;
        line.item_end = u.item_end;
        line.content |= u.content;
        x += w;
        last_hang = u.hang;
        if u.mandatory {
            lines.push(LineSpan {
                width: x - last_hang,
                forced: true,
                ..*line
            });
            cur = None;
            x = 0.0;
            last_hang = 0.0;
        }
    }
    if let Some(line) = cur {
        lines.push(LineSpan {
            width: x - last_hang,
            forced: true,
            ..line
        });
    }
    lines
}

/// Finds the longest prefix of unit `u` (at least one cluster) that fits
/// in `avail - x`, breaking between clusters of a text item. Returns the
/// split offset, the index of the text item it falls in, and the prefix
/// advance.
fn emergency_split(
    prep: &Prepared,
    fixed: &[f32],
    u: &Unit,
    x: f32,
    avail: f32,
) -> Option<(usize, usize, f32)> {
    let mut used = 0.0f32;
    let mut placed_any = false;
    for i in u.item_start..u.item_end {
        let it = prep.items.get(i)?;
        match &it.kind {
            ItemKind::Text(t) => {
                let s = u.start.max(it.range.start) - it.range.start;
                let e = u.end.min(it.range.end).saturating_sub(it.range.start);
                for (off, _end, adv) in t.clusters_in(s, e) {
                    if placed_any && x + used + adv > avail + EPSILON {
                        let split = it.range.start + off;
                        if split <= u.start {
                            return None;
                        }
                        return Some((split, i, used));
                    }
                    used += adv;
                    placed_any = true;
                }
            }
            ItemKind::Tab => {
                used += tab_advance(x + used, prep.tab_space.get(i).copied().unwrap_or(0.0))
            }
            _ => {
                used += fixed.get(i).copied().unwrap_or(0.0);
                if matches!(it.kind, ItemKind::Atomic) {
                    placed_any = true;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_stops() {
        assert_eq!(tab_advance(0.0, 5.0), 40.0);
        assert_eq!(tab_advance(10.0, 5.0), 30.0);
        assert_eq!(tab_advance(10.0, 0.0), 0.0);
        assert_eq!(tab_advance(f32::NAN, 5.0), 0.0);
    }
}
