//! The grid layout engine.
//!
//! wafflebar's defining feature is that the bar is a **grid**, not three fixed slots.
//! Modules declare a [`Cell`](crate::config::Cell) (`row`, `col`, `rowspan`, `colspan`) on a
//! `rows × columns` track. This engine validates those placements — every module must fit
//! inside the track and no two modules may overlap — and produces a flat list of
//! [`Placement`]s the GTK front-end attaches to a `gtk::Grid`.
//!
//! Keeping this logic here (GTK-free, unit-tested) means both the bar and the future GUI
//! configurator share one source of truth for "is this layout valid?".

use crate::config::{Align, Config};
use thiserror::Error;

/// A validated, ready-to-attach module placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Module type (e.g. `"clock"`).
    pub kind: String,
    /// Zero-based grid column.
    pub col: u32,
    /// Zero-based grid row.
    pub row: u32,
    /// Column span (>= 1).
    pub colspan: u32,
    /// Row span (>= 1).
    pub rowspan: u32,
    /// Index of the source module in [`Config::modules`] (stable widget identity).
    pub index: usize,
    /// Requested alignment within the cell.
    pub align: Align,
}

/// Errors produced while validating a grid layout.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GridError {
    /// The grid was declared with zero rows or columns.
    #[error("grid must have at least 1 row and 1 column (got {rows}x{cols})")]
    EmptyGrid { rows: u32, cols: u32 },
    /// A module has a zero rowspan or colspan.
    #[error("module #{index} ({kind}) has a zero span")]
    ZeroSpan { index: usize, kind: String },
    /// A module extends past the edge of the grid.
    #[error("module #{index} ({kind}) at ({row},{col}) span {rowspan}x{colspan} exceeds the {rows}x{cols} grid")]
    OutOfBounds {
        index: usize,
        kind: String,
        row: u32,
        col: u32,
        rowspan: u32,
        colspan: u32,
        rows: u32,
        cols: u32,
    },
    /// Two modules claim the same cell.
    #[error("module #{index} ({kind}) overlaps module #{other} at cell ({row},{col})")]
    Overlap {
        index: usize,
        kind: String,
        other: usize,
        row: u32,
        col: u32,
    },
}

/// A validated grid: the track dimensions plus the resolved [`Placement`]s.
#[derive(Debug, Clone)]
pub struct GridEngine {
    pub rows: u32,
    pub cols: u32,
    pub placements: Vec<Placement>,
}

impl GridEngine {
    /// Validate a [`Config`]'s grid + modules, returning placements or the first error found.
    pub fn build(config: &Config) -> Result<Self, GridError> {
        let rows = config.grid.rows;
        let cols = config.grid.columns;
        if rows == 0 || cols == 0 {
            return Err(GridError::EmptyGrid { rows, cols });
        }

        // Occupancy map: which module index (if any) owns each cell.
        let mut owner: Vec<Option<usize>> = vec![None; (rows * cols) as usize];
        let mut placements = Vec::with_capacity(config.modules.len());

        for (index, m) in config.modules.iter().enumerate() {
            let c = &m.cell;
            if c.rowspan == 0 || c.colspan == 0 {
                return Err(GridError::ZeroSpan {
                    index,
                    kind: m.kind.clone(),
                });
            }
            // Bounds: the far corner must stay inside the track. Use checked math so a
            // huge span can't wrap around.
            let row_end = c.row.checked_add(c.rowspan);
            let col_end = c.col.checked_add(c.colspan);
            let fits = matches!((row_end, col_end), (Some(re), Some(ce)) if re <= rows && ce <= cols);
            if !fits {
                return Err(GridError::OutOfBounds {
                    index,
                    kind: m.kind.clone(),
                    row: c.row,
                    col: c.col,
                    rowspan: c.rowspan,
                    colspan: c.colspan,
                    rows,
                    cols,
                });
            }
            // Overlap: claim every covered cell.
            for r in c.row..c.row + c.rowspan {
                for col in c.col..c.col + c.colspan {
                    let idx = (r * cols + col) as usize;
                    if let Some(other) = owner[idx] {
                        return Err(GridError::Overlap {
                            index,
                            kind: m.kind.clone(),
                            other,
                            row: r,
                            col,
                        });
                    }
                    owner[idx] = Some(index);
                }
            }
            placements.push(Placement {
                kind: m.kind.clone(),
                col: c.col,
                row: c.row,
                colspan: c.colspan,
                rowspan: c.rowspan,
                index,
                align: m.align,
            });
        }

        Ok(GridEngine {
            rows,
            cols,
            placements,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg(toml: &str) -> Config {
        Config::parse(toml).expect("config should parse")
    }

    #[test]
    fn valid_three_module_row() {
        let c = cfg(r#"
            [grid]
            rows = 1
            columns = 3
            [[modules]]
            type = "tags"
            cell = { row = 0, col = 0 }
            [[modules]]
            type = "clock"
            cell = { row = 0, col = 1 }
            [[modules]]
            type = "tray"
            cell = { row = 0, col = 2 }
        "#);
        let g = GridEngine::build(&c).expect("valid");
        assert_eq!(g.placements.len(), 3);
        assert_eq!(g.placements[1].kind, "clock");
        assert_eq!(g.placements[1].index, 1);
    }

    #[test]
    fn spanning_module_ok() {
        let c = cfg(r#"
            [grid]
            rows = 2
            columns = 2
            [[modules]]
            type = "tray"
            cell = { row = 0, col = 1, rowspan = 2, colspan = 1 }
        "#);
        let g = GridEngine::build(&c).expect("valid");
        assert_eq!(g.placements[0].rowspan, 2);
    }

    #[test]
    fn detects_out_of_bounds() {
        let c = cfg(r#"
            [grid]
            rows = 1
            columns = 2
            [[modules]]
            type = "clock"
            cell = { row = 0, col = 1, colspan = 2 }
        "#);
        let err = GridEngine::build(&c).unwrap_err();
        assert!(matches!(err, GridError::OutOfBounds { index: 0, .. }));
    }

    #[test]
    fn detects_overlap() {
        let c = cfg(r#"
            [grid]
            rows = 1
            columns = 3
            [[modules]]
            type = "clock"
            cell = { row = 0, col = 0, colspan = 2 }
            [[modules]]
            type = "cpu"
            cell = { row = 0, col = 1 }
        "#);
        let err = GridEngine::build(&c).unwrap_err();
        assert_eq!(
            err,
            GridError::Overlap {
                index: 1,
                kind: "cpu".to_string(),
                other: 0,
                row: 0,
                col: 1,
            }
        );
    }

    #[test]
    fn rejects_zero_span() {
        let c = cfg(r#"
            [grid]
            rows = 1
            columns = 2
            [[modules]]
            type = "clock"
            cell = { row = 0, col = 0, colspan = 0 }
        "#);
        assert!(matches!(
            GridEngine::build(&c).unwrap_err(),
            GridError::ZeroSpan { .. }
        ));
    }

    #[test]
    fn rejects_empty_grid() {
        let c = cfg(r#"
            [grid]
            rows = 0
            columns = 0
        "#);
        assert!(matches!(
            GridEngine::build(&c).unwrap_err(),
            GridError::EmptyGrid { .. }
        ));
    }
}
