//! The two-row shortcut list at the bottom of the window.
//!
//! This is the most recognizable thing about nano and the reason the editor
//! exists, so it gets its own module and its own tests. Three rules:
//!
//! - **Two rows. Never three.** A third row is a "stop and ask" change in
//!   `CLAUDE.md`, and the whole design falls apart without that limit.
//! - **Column-major, like nano.** Entries fill down then across, so the first
//!   two entries are the leftmost column. The keymap files number their
//!   priorities to suit, which is why `^G Help` and `^X Exit` sit together.
//! - **Narrow windows drop the least important entries**, not the rightmost
//!   ones — and they drop whole columns, so the grid never looks ragged.
//!
//! Nothing here knows about pixels. Widths are in character cells, because the
//! font is monospace and because the harness has to be able to assert on this
//! without a window.

use crate::keymap::FooterEntry;

/// Spaces between one column and the next.
const COLUMN_GAP: usize = 2;

/// The footer has two rows. This is not configurable, deliberately.
pub const ROWS: usize = 2;

/// One cell: a chord and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub chord: String,
    pub label: String,
}

impl Cell {
    /// Rendered width in character cells, as `^X Exit`.
    pub fn width(&self) -> usize {
        self.chord.chars().count() + 1 + self.label.chars().count()
    }

    /// The text as it appears on screen.
    pub fn text(&self) -> String {
        format!("{} {}", self.chord, self.label)
    }
}

/// A laid-out footer: two rows of cells, and what did not fit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Footer {
    pub rows: Vec<Vec<Cell>>,
    /// How many entries were dropped for want of width.
    pub dropped: usize,
    /// Width of one column in cells, including the gap after it.
    pub column_width: usize,
}

impl Footer {
    /// Every cell that made it, in priority order.
    pub fn cells(&self) -> Vec<&Cell> {
        let mut cells = Vec::new();
        let columns = self.rows.first().map(Vec::len).unwrap_or(0);
        for column in 0..columns {
            for row in &self.rows {
                if let Some(cell) = row.get(column) {
                    cells.push(cell);
                }
            }
        }
        cells
    }

    /// The footer as two lines of text, for tests and for the help pane.
    pub fn lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| {
                        let text = cell.text();
                        let padding = self.column_width.saturating_sub(text.chars().count());
                        format!("{text}{}", " ".repeat(padding))
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }
}

/// Lay the footer out for a window `width` character cells wide.
///
/// Entries must already be in priority order — [`crate::Keymap::footer_entries`]
/// returns them that way.
pub fn layout(entries: &[FooterEntry<'_>], width: usize) -> Footer {
    if entries.is_empty() {
        return Footer::default();
    }

    // One column width for all of them, as nano does: a ragged grid is harder
    // to scan than a slightly wasteful even one. It is measured over the cells
    // that are actually shown — sizing columns to an entry that gets dropped
    // would waste width and drop more entries than necessary.
    //
    // Keeping more entries can only widen the column, so the width a given
    // count needs never decreases: take the largest count that still fits.
    let mut kept = 0;
    let mut column_width = COLUMN_GAP;
    for count in 1..=entries.len() {
        let widest = entries[..count]
            .iter()
            .map(|entry| entry.width())
            .max()
            .unwrap_or(0);
        let candidate = widest + COLUMN_GAP;
        let columns = count.div_ceil(ROWS);
        if columns * candidate > width {
            break;
        }
        kept = count;
        column_width = candidate;
    }

    // At least one column, even in a window too narrow for it: showing a
    // clipped `^G Help` beats showing an empty bar.
    if kept == 0 {
        kept = entries.len().min(ROWS);
        column_width = entries[..kept]
            .iter()
            .map(|entry| entry.width())
            .max()
            .unwrap_or(0)
            + COLUMN_GAP;
    }

    let mut rows = vec![Vec::new(); ROWS];
    for (index, entry) in entries.iter().take(kept).enumerate() {
        // Column-major: entry 0 and 1 are the first column, top and bottom.
        rows[index % ROWS].push(Cell {
            chord: entry.chord.to_string(),
            label: entry.label.to_string(),
        });
    }

    Footer {
        rows,
        dropped: entries.len() - kept,
        column_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Keymap};

    fn nano_footer(width: usize) -> Footer {
        let keymap = Keymap::nano();
        layout(&keymap.footer_entries(Context::Editor), width)
    }

    #[test]
    fn the_footer_has_two_rows_and_only_two() {
        for width in [10, 40, 80, 200, 2000] {
            let footer = nano_footer(width);
            assert_eq!(footer.rows.len(), ROWS, "at width {width}");
        }
    }

    #[test]
    fn the_first_column_is_help_over_exit() {
        // The single most recognizable thing about a nano screen.
        let footer = nano_footer(200);
        assert_eq!(footer.rows[0][0].text(), "^G Help");
        assert_eq!(footer.rows[1][0].text(), "^X Exit");
    }

    #[test]
    fn entries_fill_down_then_across() {
        let footer = nano_footer(200);
        assert_eq!(footer.rows[0][1].text(), "^O Write Out");
        assert_eq!(footer.rows[1][1].text(), "^R Read File");
        assert_eq!(footer.rows[0][2].text(), "^W Where Is");
        assert_eq!(footer.rows[1][2].text(), "^\\ Replace");
    }

    #[test]
    fn a_narrow_window_drops_the_lowest_priority_entries() {
        let wide = nano_footer(2000);
        let narrow = nano_footer(40);

        assert!(narrow.dropped > 0, "something should have been dropped");
        assert_eq!(wide.dropped, 0, "a wide window drops nothing");

        // What survives is a prefix of what a wide window shows.
        let wide_text: Vec<String> = wide.cells().iter().map(|c| c.text()).collect();
        let narrow_text: Vec<String> = narrow.cells().iter().map(|c| c.text()).collect();
        assert_eq!(narrow_text, wide_text[..narrow_text.len()]);
    }

    #[test]
    fn help_and_exit_survive_the_narrowest_window() {
        // If only one column fits, it has to be the one that gets you out.
        let footer = nano_footer(1);
        assert_eq!(footer.rows[0].len(), 1);
        assert_eq!(footer.rows[0][0].text(), "^G Help");
        assert_eq!(footer.rows[1][0].text(), "^X Exit");
    }

    #[test]
    fn widening_the_window_only_ever_adds_entries() {
        let mut previous = 0;
        for width in [20, 40, 60, 80, 120, 200, 400] {
            let footer = nano_footer(width);
            let shown = footer.cells().len();
            assert!(
                shown >= previous,
                "width {width} showed {shown} after {previous}"
            );
            previous = shown;
        }
    }

    #[test]
    fn columns_are_even_and_fit_the_window() {
        let footer = nano_footer(80);
        for line in footer.lines() {
            assert!(
                line.chars().count() <= 80,
                "a footer row overflowed the window: {line:?}"
            );
        }
        // Every column is the same width, so the grid lines up.
        let widest = footer.cells().iter().map(|c| c.width()).max().unwrap_or(0);
        assert_eq!(footer.column_width, widest + COLUMN_GAP);
    }

    #[test]
    fn the_two_rows_stay_balanced() {
        // Never a dangling entry on the top row with a gap under it, unless
        // there is an odd number of entries in total.
        let footer = nano_footer(2000);
        let difference = footer.rows[0].len() as isize - footer.rows[1].len() as isize;
        assert!((0..=1).contains(&difference), "rows are lopsided");
    }

    #[test]
    fn every_context_lays_out_without_panicking() {
        let keymap = Keymap::nano();
        for context in [
            Context::Editor,
            Context::Prompt,
            Context::Tree,
            Context::Search,
            Context::Help,
        ] {
            let footer = layout(&keymap.footer_entries(context), 80);
            assert!(
                !footer.rows[0].is_empty(),
                "context {context} has no footer"
            );
        }
    }

    #[test]
    fn an_empty_keymap_lays_out_to_nothing() {
        let footer = layout(&[], 80);
        assert!(footer.cells().is_empty());
        assert_eq!(footer.dropped, 0);
    }
}
