//! Column widths: measuring columns from their cells (CSS 2.2 §17.5.2.2,
//! CSS Tables 3 §3.9 "Computing column measures") and distributing the
//! table's width over them (CSS Tables 3 §3.9.3 "Distributing width to
//! the columns"), plus the fixed table layout algorithm (CSS 2.2
//! §17.5.2.1).
//!
//! Every function here is linear in the number of columns plus the total
//! span of the cells it is given.

use crate::geom::{MAX_COORD, sane_non_negative};

/// What a column needs, from its cells and `<col>`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ColMeasure {
    /// Min-content width (outer widths of the cells, without spacing).
    pub min: f32,
    /// Max-content width.
    pub max: f32,
    /// Percentage width, 0..=100 (0 when none).
    pub pct: f32,
    /// A cell or column gives it a fixed (non-percentage) width: it is a
    /// "constrained" column.
    pub fixed: bool,
}

/// One cell's contribution, outer widths (border and padding included).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CellMeasure {
    /// The first column.
    pub col: usize,
    /// Columns spanned, at least 1.
    pub span: usize,
    /// Outer min-content width.
    pub min: f32,
    /// Outer max-content width (already raised to a fixed `width`).
    pub max: f32,
    /// Percentage `width`, 0 when none.
    pub pct: f32,
    /// The cell has a fixed `width`.
    pub fixed: bool,
}

/// Adds the cells' contributions to `cols` (CSS Tables 3 §3.9.1): first
/// cells spanning one column, then spanning cells in order of increasing
/// span, whose excess is shared among their columns in proportion to the
/// columns' max-content widths. `hs` is the horizontal border spacing
/// (spanned spacing counts towards a spanning cell's width).
pub(crate) fn measure(cols: &mut [ColMeasure], cells: &[CellMeasure], hs: f32) {
    for c in cells.iter().filter(|c| c.span == 1) {
        if let Some(col) = cols.get_mut(c.col) {
            col.min = col.min.max(c.min);
            col.max = col.max.max(c.max);
            col.pct = col.pct.max(c.pct);
            col.fixed |= c.fixed;
        }
    }
    for col in cols.iter_mut() {
        col.max = col.max.max(col.min);
    }
    let mut spanning: Vec<&CellMeasure> = cells.iter().filter(|c| c.span > 1).collect();
    spanning.sort_by_key(|c| c.span);
    for cell in spanning {
        let end = cell.col.saturating_add(cell.span).min(cols.len());
        let Some(range) = cols.get_mut(cell.col..end) else {
            continue;
        };
        if range.is_empty() {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let spacing = hs * (range.len() - 1) as f32;
        let total_max: f32 = range.iter().map(|c| c.max).sum();
        let total_min: f32 = range.iter().map(|c| c.min).sum::<f32>() + spacing;
        if cell.min > total_min {
            let shares = shares(range, cell.min - total_min, |c| c.max, |_| true);
            for (c, s) in range.iter_mut().zip(shares) {
                c.min += s;
            }
        }
        let total_max_sp = total_max + spacing;
        if cell.max > total_max_sp {
            let shares = shares(range, cell.max - total_max_sp, |c| c.max, |_| true);
            for (c, s) in range.iter_mut().zip(shares) {
                c.max += s;
            }
        }
        if cell.pct > 0.0 {
            let have: f32 = range.iter().map(|c| c.pct).sum();
            if cell.pct > have && range.iter().any(|c| c.pct == 0.0) {
                let shares = shares(range, cell.pct - have, |c| c.max, |c| c.pct == 0.0);
                for (c, s) in range.iter_mut().zip(shares) {
                    c.pct += s;
                }
            }
        }
        for c in range.iter_mut() {
            c.max = c.max.max(c.min);
        }
    }
    // Percentages beyond 100% in total are cut, later columns first.
    let mut total = 0.0f32;
    for c in cols.iter_mut() {
        c.pct = c.pct.clamp(0.0, (100.0 - total).max(0.0));
        total += c.pct;
    }
}

/// Splits `amount` over the columns accepted by `eligible`, in proportion
/// to `weight` (equally when every weight is 0).
fn shares(
    cols: &[ColMeasure],
    amount: f32,
    weight: impl Fn(&ColMeasure) -> f32,
    eligible: impl Fn(&ColMeasure) -> bool,
) -> Vec<f32> {
    let total: f32 = cols.iter().filter(|c| eligible(c)).map(&weight).sum();
    let count = cols.iter().filter(|c| eligible(c)).count();
    cols.iter()
        .map(|c| {
            if !eligible(c) || count == 0 {
                0.0
            } else if total > 0.0 {
                amount * weight(c) / total
            } else {
                #[allow(clippy::cast_precision_loss)]
                let n = count as f32;
                amount / n
            }
        })
        .collect()
}

/// The sum of the columns' min-content widths.
pub(crate) fn min_width(cols: &[ColMeasure]) -> f32 {
    sane_non_negative(cols.iter().map(|c| c.min).sum())
}

/// The width the columns need to reach their max-content widths and
/// their percentages (CSS 2.2 §17.5.2.2 and common practice): a column
/// of `p`% with max-content width `m` needs a table of `m / p`; the
/// non-percentage columns need the rest of the table.
pub(crate) fn max_width(cols: &[ColMeasure]) -> f32 {
    let mut w: f32 = cols.iter().map(|c| c.max).sum();
    let pct: f32 = cols.iter().map(|c| c.pct).sum();
    let rest: f32 = cols.iter().filter(|c| c.pct == 0.0).map(|c| c.max).sum();
    for c in cols.iter().filter(|c| c.pct > 0.0) {
        w = w.max(c.max * 100.0 / c.pct);
    }
    if pct >= 100.0 {
        if rest > 0.0 {
            w = MAX_COORD;
        }
    } else if pct > 0.0 {
        w = w.max(rest * 100.0 / (100.0 - pct));
    }
    sane_non_negative(w).min(MAX_COORD)
}

/// Distributes `avail` (the table's content width minus spacing) over the
/// columns (CSS Tables 3 §3.9.3): picks the two adjacent width guesses
/// (min-content, min-content-percentage, min-content-specified,
/// max-content) that bracket `avail` and interpolates between them;
/// beyond max-content, the excess goes to auto columns, then fixed
/// columns, then percentage columns, then all columns.
pub(crate) fn distribute(cols: &[ColMeasure], avail: f32) -> Vec<f32> {
    let avail = sane_non_negative(avail);
    let pct_w = |c: &ColMeasure| c.min.max(c.pct / 100.0 * avail);
    let guesses: [Vec<f32>; 4] = [
        cols.iter().map(|c| c.min).collect(),
        cols.iter()
            .map(|c| if c.pct > 0.0 { pct_w(c) } else { c.min })
            .collect(),
        cols.iter()
            .map(|c| {
                if c.pct > 0.0 {
                    pct_w(c)
                } else if c.fixed {
                    c.max
                } else {
                    c.min
                }
            })
            .collect(),
        cols.iter()
            .map(|c| if c.pct > 0.0 { pct_w(c) } else { c.max })
            .collect(),
    ];
    let sums: Vec<f32> = guesses.iter().map(|g| g.iter().sum()).collect();
    let [g0, _, _, g3] = &guesses;
    if sums.first().is_none_or(|s| avail <= *s) {
        return g0.clone();
    }
    for k in 1..guesses.len() {
        let (Some(lo), Some(hi)) = (sums.get(k - 1), sums.get(k)) else {
            continue;
        };
        if avail <= *hi {
            let (Some(a), Some(b)) = (guesses.get(k - 1), guesses.get(k)) else {
                break;
            };
            let t = if hi > lo {
                (avail - lo) / (hi - lo)
            } else {
                1.0
            };
            return a
                .iter()
                .zip(b)
                .map(|(a, b)| sane_non_negative(a + t * (b - a)))
                .collect();
        }
    }
    // Wider than every guess: hand out the excess.
    let excess = avail - sums.last().copied().unwrap_or(0.0);
    let is_auto = |c: &ColMeasure| c.pct == 0.0 && !c.fixed;
    let shares = if cols.iter().any(|c| is_auto(c) && c.max > 0.0) {
        shares(cols, excess, |c| c.max, |c| is_auto(c) && c.max > 0.0)
    } else if cols.iter().any(is_auto) {
        shares(cols, excess, |_| 0.0, is_auto)
    } else if cols.iter().any(|c| c.fixed && c.max > 0.0 && c.pct == 0.0) {
        shares(
            cols,
            excess,
            |c| c.max,
            |c| c.fixed && c.max > 0.0 && c.pct == 0.0,
        )
    } else if cols.iter().any(|c| c.pct > 0.0) {
        shares(cols, excess, |c| c.pct, |c| c.pct > 0.0)
    } else {
        shares(cols, excess, |_| 0.0, |_| true)
    };
    g3.iter()
        .zip(shares)
        .map(|(w, s)| sane_non_negative(w + s))
        .collect()
}

/// A column width given by a `<col>` or a first-row cell in the fixed
/// table layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FixedWidth {
    /// Not given.
    Auto,
    /// Pixels (outer width).
    Px(f32),
    /// A percentage of the table's content width.
    Pct(f32),
}

/// The fixed table layout (CSS 2.2 §17.5.2.1): given widths are used,
/// remaining columns share what is left equally; when every column has a
/// width and they are narrower than the table, the excess is shared in
/// proportion to the widths. `avail` is the table's content width minus
/// spacing. Returns the widths; their sum may exceed `avail`.
pub(crate) fn fixed(given: &[FixedWidth], avail: f32) -> Vec<f32> {
    let avail = sane_non_negative(avail);
    let mut out: Vec<Option<f32>> = given
        .iter()
        .map(|g| match *g {
            FixedWidth::Auto => None,
            FixedWidth::Px(w) => Some(sane_non_negative(w)),
            FixedWidth::Pct(p) => Some(sane_non_negative(p / 100.0 * avail)),
        })
        .collect();
    let known: f32 = out.iter().flatten().sum();
    let unknown = out.iter().filter(|w| w.is_none()).count();
    if unknown > 0 {
        #[allow(clippy::cast_precision_loss)]
        let each = ((avail - known) / unknown as f32).max(0.0);
        for w in &mut out {
            w.get_or_insert(each);
        }
    }
    let mut widths: Vec<f32> = out.into_iter().map(|w| w.unwrap_or(0.0)).collect();
    let sum: f32 = widths.iter().sum();
    if unknown == 0 && sum < avail && !widths.is_empty() {
        let extra = avail - sum;
        #[allow(clippy::cast_precision_loss)]
        let n = widths.len() as f32;
        for w in &mut widths {
            *w += if sum > 0.0 {
                extra * *w / sum
            } else {
                extra / n
            };
        }
    }
    widths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(min: f32, max: f32) -> ColMeasure {
        ColMeasure {
            min,
            max,
            ..ColMeasure::default()
        }
    }

    #[test]
    fn interpolates_between_min_and_max() {
        let cols = [col(10.0, 100.0), col(20.0, 50.0)];
        assert_eq!(distribute(&cols, 30.0), vec![10.0, 20.0]);
        assert_eq!(distribute(&cols, 10.0), vec![10.0, 20.0]);
        assert_eq!(distribute(&cols, 150.0), vec![100.0, 50.0]);
        // Halfway: each column halfway between its min and max.
        assert_eq!(distribute(&cols, 90.0), vec![55.0, 35.0]);
    }

    #[test]
    fn excess_goes_to_auto_columns_by_max() {
        let mut fixed = col(10.0, 50.0);
        fixed.fixed = true;
        let cols = [fixed, col(0.0, 30.0), col(0.0, 10.0)];
        assert_eq!(distribute(&cols, 130.0), vec![50.0, 60.0, 20.0]);
    }

    #[test]
    fn percentages() {
        let mut p = col(0.0, 10.0);
        p.pct = 50.0;
        let cols = [p, col(0.0, 10.0)];
        let w = distribute(&cols, 200.0);
        assert_eq!(w, vec![100.0, 100.0]);
        // The table wants max/pct.
        assert_eq!(max_width(&cols), 20.0);
        let mut big = col(0.0, 80.0);
        big.pct = 50.0;
        assert_eq!(max_width(&[big, col(0.0, 10.0)]), 160.0);
        let mut full = col(0.0, 10.0);
        full.pct = 100.0;
        assert_eq!(max_width(&[full, col(0.0, 10.0)]), MAX_COORD);
    }

    #[test]
    fn spanning_cells_share_excess() {
        let mut cols = vec![ColMeasure::default(); 2];
        let cells = [
            CellMeasure {
                col: 0,
                span: 1,
                min: 10.0,
                max: 30.0,
                ..CellMeasure::default()
            },
            CellMeasure {
                col: 1,
                span: 1,
                min: 10.0,
                max: 10.0,
                ..CellMeasure::default()
            },
            CellMeasure {
                col: 0,
                span: 2,
                min: 42.0,
                max: 82.0,
                ..CellMeasure::default()
            },
        ];
        measure(&mut cols, &cells, 2.0);
        // min: 42 - (20 + 2) = 20 shared 3:1 by max.
        assert_eq!(cols[0].min, 25.0);
        assert_eq!(cols[1].min, 15.0);
        // max: 82 - (40 + 2) = 40 shared 3:1.
        assert_eq!(cols[0].max, 60.0);
        assert_eq!(cols[1].max, 20.0);
    }

    #[test]
    fn percentages_are_capped_at_100() {
        let mut cols = vec![ColMeasure::default(); 3];
        let cells: Vec<CellMeasure> = (0..3)
            .map(|i| CellMeasure {
                col: i,
                span: 1,
                pct: 60.0,
                ..CellMeasure::default()
            })
            .collect();
        measure(&mut cols, &cells, 0.0);
        assert_eq!(
            cols.iter().map(|c| c.pct).collect::<Vec<_>>(),
            vec![60.0, 40.0, 0.0]
        );
    }

    #[test]
    fn fixed_layout() {
        use FixedWidth::*;
        assert_eq!(
            fixed(&[Px(50.0), Auto, Auto], 150.0),
            vec![50.0, 50.0, 50.0]
        );
        assert_eq!(fixed(&[Pct(50.0), Auto], 200.0), vec![100.0, 100.0]);
        assert_eq!(fixed(&[Px(10.0), Px(30.0)], 80.0), vec![20.0, 60.0]);
        // Too wide: the table grows (the caller widens it).
        assert_eq!(fixed(&[Px(100.0), Auto], 50.0), vec![100.0, 0.0]);
    }
}
