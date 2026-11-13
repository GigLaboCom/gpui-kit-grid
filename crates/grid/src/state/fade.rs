//! Removed rows fading out on screen.
//!
//! With `HighlightPolicy::with_removal_fade`, the store keeps a removed
//! row's last version in its snapshots while its removal stamp burns
//! (`StoreSnapshot::fading_row`), and the engine keeps its id in its place
//! among the shown lines. The frame walk meets such a line as a data line
//! whose row the snapshot no longer has: [`RowPlanner::plan_fading_line`]
//! paints it as a row whose every cell shows `RowChange::Removed`, through
//! the change paint and the delegate's `cell_style` (and `render_cell` for
//! the cells of `Element` and `Control` columns, without presses), until
//! the fade ends on
//! the global tick; then the line is skipped, as a removed row of stage 1
//! is, and the rows below move up in the same frame. The background
//! compaction drops the id later (`ChangeSet::faded`).
//!
//! A fading row is hot: the fade timer wakes the frame on its steps and on
//! its last tick, as for any highlight. It is not a row of the grid any
//! more: no selection, no focus, no tooltip, no accessibility row; a press
//! on it lands on empty space (`PaintedLine::Ghost`), and it is no scroll
//! anchor.

use gpui_kit::{Hsla, Pixels, Window};

use super::{
    CellChange, LiveGridDelegate,
    a11y::RowOutline,
    change::RowChanges,
    frame::{FillRuns, PlannedRow, RowPlanner},
};
use crate::{
    element::Region,
    engine::DisplayRow,
    highlight::RowChange,
    model::ColumnSet,
    store::{RowId, StoreSnapshot},
};

/// A shown data line whose row the snapshot no longer has.
pub(super) struct FadingLine {
    /// The row's id before removal.
    pub(super) id: RowId,
    /// The top edge in window coordinates.
    pub(super) y: Pixels,
    pub(super) display_ix: usize,
    pub(super) line: DisplayRow,
    /// The stripe of the line, as a live row there would have.
    pub(super) background: Option<Hsla>,
}

impl<D: LiveGridDelegate> RowPlanner<'_, D> {
    /// Plans a shown line whose row was removed: its last version, every
    /// cell with [`RowChange::Removed`] at the removal's intensity, while
    /// the row fades out. Returns false, planning nothing, once the fade
    /// ended on the global tick or without a removal fade: the line is
    /// skipped then.
    pub(super) fn plan_fading_line(
        &mut self,
        fading: &FadingLine,
        snapshot: &StoreSnapshot<D::Row>,
        regions: &mut [Region; 3],
        fills: &mut [FillRuns; 3],
        window: &mut Window,
    ) -> bool {
        let Some((row, removed)) = snapshot.fading_row(fading.id) else {
            return false;
        };
        let Some(intensity) = self.policy.removal_intensity(removed, self.tick) else {
            return false;
        };
        let change = CellChange {
            intensity,
            change: RowChange::Removed,
            direction: None,
            // `RowChanges::cell` names each cell's column.
            column_ix: None,
        };
        // The row is gone: it has no accessibility row, and the
        // rows around keep their indices, which count shown lines.
        let a11y = self.a11y.take();
        self.plan_row(
            &PlannedRow {
                row,
                y: fading.y,
                changes: RowChanges::Removed(change),
                display_ix: fading.display_ix,
                line: Some(fading.line),
                outline: RowOutline::default(),
                pinned: None,
                background: fading.background,
                foreground: self.palette.foreground,
                // Its key is in no selection and holds no focus; with no
                // hovered column it asks for no tooltip. The selection's
                // runs above end on it.
                selected: ColumnSet::EMPTY,
                whole_row: false,
                focused_column: None,
                hovered_column: None,
            },
            regions,
            Some(fills),
            window,
        );
        self.a11y = a11y;
        true
    }
}
