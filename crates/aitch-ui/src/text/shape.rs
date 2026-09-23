//! Trivial monospace layout — Phase 1 Track B.
//!
//! Aitch is a monospace editor (PLAN.md's stack table names `cosmic-text`'s
//! shaping as the piece being replaced, but nothing in this project ever
//! asked for proportional fonts, kerning, or ligatures — every column is the
//! same width). So "shaping" here is arithmetic, not a shaping engine: the
//! Nth character on a line sits at `N * advance_width`, full stop. This is
//! deliberately the smallest useful thing, per the task's own scope note —
//! the complexity in this phase lives in the rasterizer, not here.

/// One character placed on a line: what to draw, and where its pen origin
/// (baseline, left edge) sits in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphPlacement {
    pub ch: char,
    pub x: f32,
    pub y: f32,
}

/// Lay out `text` as a single monospace line: every non-tab character
/// advances exactly one column (`advance_width` pixels); a tab advances the
/// column to the next multiple of `tab_columns` (matching this project's
/// existing `tab_width` setting in spirit — see `docs/config.md`), the same
/// expanding-to-column-width behavior nano-style tab stops have always had
/// here.
///
/// A tab produces no placement of its own: there is no glyph to draw for it,
/// only a gap. A caller that needs to know a tab's own screen column (for
/// cursor placement, say) can recompute it from the string up to that point
/// with the same `tab_columns` value — that is a cursor/hit-testing concern
/// for a later integration step, not layout's.
///
/// `tab_columns == 0` is treated as "no tab stops": a tab just advances one
/// column, the same as any other character, rather than looping forever
/// looking for the next multiple of zero.
#[must_use]
pub fn layout_line(
    text: &str,
    advance_width: f32,
    tab_columns: usize,
    y: f32,
) -> Vec<GlyphPlacement> {
    let mut placements = Vec::with_capacity(text.len());
    let mut column: usize = 0;

    for ch in text.chars() {
        if ch == '\t' {
            column = next_tab_stop(column, tab_columns);
        } else {
            placements.push(GlyphPlacement {
                ch,
                x: column as f32 * advance_width,
                y,
            });
            column += 1;
        }
    }

    placements
}

fn next_tab_stop(column: usize, tab_columns: usize) -> usize {
    if tab_columns == 0 {
        return column + 1;
    }
    (column / tab_columns + 1) * tab_columns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_character_advances_by_exactly_one_column() {
        let placements = layout_line("abc", 10.0, 4, 100.0);
        assert_eq!(
            placements,
            vec![
                GlyphPlacement {
                    ch: 'a',
                    x: 0.0,
                    y: 100.0
                },
                GlyphPlacement {
                    ch: 'b',
                    x: 10.0,
                    y: 100.0
                },
                GlyphPlacement {
                    ch: 'c',
                    x: 20.0,
                    y: 100.0
                },
            ]
        );
    }

    #[test]
    fn an_empty_line_places_nothing() {
        assert!(layout_line("", 10.0, 4, 0.0).is_empty());
    }

    #[test]
    fn a_tab_jumps_to_the_next_tab_stop_and_places_no_glyph_of_its_own() {
        // "a" at column 0, tab jumps column 1 -> 4, "b" at column 4.
        let placements = layout_line("a\tb", 10.0, 4, 0.0);
        assert_eq!(
            placements,
            vec![
                GlyphPlacement {
                    ch: 'a',
                    x: 0.0,
                    y: 0.0
                },
                GlyphPlacement {
                    ch: 'b',
                    x: 40.0,
                    y: 0.0
                },
            ]
        );
    }

    #[test]
    fn a_tab_already_on_a_stop_advances_a_full_stop_width() {
        // Four letters land exactly on a tab stop (columns 0..4); the tab
        // must still advance to the *next* stop (8), not stay put.
        let placements = layout_line("abcd\te", 10.0, 4, 0.0);
        let e = placements.last().unwrap();
        assert_eq!(e.ch, 'e');
        assert_eq!(e.x, 80.0);
    }

    #[test]
    fn tab_columns_of_zero_advances_one_column_instead_of_looping() {
        let placements = layout_line("a\tb", 10.0, 0, 0.0);
        assert_eq!(
            placements,
            vec![
                GlyphPlacement {
                    ch: 'a',
                    x: 0.0,
                    y: 0.0
                },
                GlyphPlacement {
                    ch: 'b',
                    x: 20.0,
                    y: 0.0
                },
            ]
        );
    }
}
