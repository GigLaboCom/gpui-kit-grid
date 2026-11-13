//! Copying rows to the clipboard: `Copy` writes the command scope, `CopyAll`
//! every shown line under a line of column titles, with the pinned rows and
//! the totals row of the top band before the lines and those of the bottom
//! band after them; [`LiveGridState::copy_scope`] takes either, or the view
//! with every group and tree row expanded.
//!
//! The lines of a grouped view are summary rows too ([`ExportLine`]): a
//! group caption holds its group's key value under the level's column and
//! its aggregates under theirs, a group footer and the totals hold
//! aggregates. No delegate hook runs for a copy: a caption is its key value,
//! not the caption text a frame paints (per-frame hooks run only in
//! prepaint).
//!
//! A copy is taken when the command runs, as an export is (one
//! `ExportCapture` for both): the snapshot, the shown order (a held one
//! included) with its expansion, the visible columns in screen order,
//! the totals label and the optimistic values of waiting edits, so a cell is
//! copied as the screen shows it. Rows that arrive or change later are not
//! in it. The grid's [`Exporter`] and [`CellExport`] write the text. A small
//! copy is written at once; a larger one is written by a [`TaskRunner`] job
//! (by default in the background), and only the clipboard write runs on the
//! UI thread. A new copy cancels one still being written.
//!
//! [`ExportLine`]: crate::ExportLine
//! [`TaskRunner`]: crate::TaskRunner

use std::sync::Arc;

use gpui_kit::{ClipboardItem, Context, Task, Window};

use super::{LiveGridDelegate, LiveGridState, export::ExportCapture};
use crate::{
    actions,
    export::{CellExport, ExportScope, Exporter, TsvExporter},
    model::GridRow,
};

#[cfg(doc)]
use crate::export::RawCells;

/// A copy of at most this many cells (1k rows of 40 columns) is written at
/// once on the UI thread, in about 2 ms with raw values; a larger one runs as
/// a task runner job.
const INLINE_COPY_CELLS: usize = 40_000;

/// How a grid copies, and the copy being written.
pub(super) struct CopySlot<R: GridRow> {
    exporter: Arc<dyn Exporter<R>>,
    cells: CellExport<R>,
    /// Waits for the job writing the latest copy; dropping it cancels the
    /// copy, so its text never reaches the clipboard.
    job: Option<Task<()>>,
}

impl<R: GridRow> Default for CopySlot<R> {
    fn default() -> Self {
        Self {
            exporter: Arc::new(TsvExporter),
            cells: CellExport::default(),
            job: None,
        }
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets how one cell becomes text when copied, and when exported unless
    /// the export asks otherwise (`ExportRequest::with_cell_export`). The
    /// default is `CellExport::Raw(RawCells::new())`: raw numbers with a
    /// `.`, `TRUE` and `FALSE`; see also [`RawCells::with_decimal_separator`]
    /// and `CellExport::Displayed`.
    pub fn with_cell_export(mut self, export: CellExport<D::Row>) -> Self {
        self.copy.cells = export;
        self
    }

    /// Sets the clipboard format of a copy (default [`TsvExporter`]).
    pub fn with_exporter(mut self, exporter: impl Exporter<D::Row>) -> Self {
        self.copy.exporter = Arc::new(exporter);
        self
    }

    /// Copies the command scope of the focused line in the shown order,
    /// without a header line (`Copy`, Cmd/Ctrl+C): on a group caption every
    /// row of its group that passes the view, those of collapsed subgroups
    /// included; otherwise [`Self::scoped_selection`], which holds a row of
    /// a collapsed group only if it is selected. Summary rows are never in
    /// it. The visible columns are copied in screen order; in the cell modes
    /// only the columns that hold a selected cell, and the cells of that
    /// rectangle that are not selected are empty. Returns false, leaving the
    /// clipboard alone, if there is nothing to copy.
    ///
    /// The same as `copy_scope(ExportScope::Selection)`. A large copy is
    /// written by the task runner, and its text reaches the clipboard when
    /// the job is done.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        self.copy_scope(ExportScope::Selection, cx)
    }

    /// Copies every shown line in the shown order (a held order as it is on
    /// screen), every visible column in screen order, under a line of column
    /// titles (`CopyAll`): group captions, the rows of expanded groups and
    /// tree rows, group footers. The top band comes first, the totals row
    /// before its pinned rows, and the bottom band last, the totals row
    /// after its pinned rows. Returns false, leaving the clipboard alone, if
    /// there is no line, no pinned row and no totals row to copy.
    ///
    /// The same as `copy_scope(ExportScope::Visible)`. A large copy is
    /// written by the task runner, and its text reaches the clipboard when
    /// the job is done.
    pub fn copy_all(&mut self, cx: &mut Context<Self>) -> bool {
        self.copy_scope(ExportScope::Visible, cx)
    }

    /// Copies `scope` to the clipboard: `Selection` as [`Self::copy`],
    /// `Visible` as [`Self::copy_all`], `Expanded` as `copy_all` with every
    /// group and tree row expanded (the rows of collapsed groups under their
    /// captions, the footers of collapsed groups, every descendant of a
    /// collapsed tree row). Returns false, leaving the clipboard alone, if
    /// there is nothing to copy.
    ///
    /// A group caption is written as its group's key value under the
    /// level's column, or under the first copied column when that column is
    /// hidden or the level groups by a `GroupKey`, and its aggregates under
    /// theirs; a footer and the totals row as their aggregates, the totals
    /// label in the first copied column when it has no aggregate. A totals
    /// row is copied when the grid shows one and the view has aggregates.
    ///
    /// A large copy is written by the task runner, and its text reaches the
    /// clipboard when the job is done.
    pub fn copy_scope(&mut self, scope: ExportScope, cx: &mut Context<Self>) -> bool {
        // A copy of the selection has no header line.
        let header = scope != ExportScope::Selection;
        let capture = self.capture(scope, None, header, self.copy.cells.clone());
        if capture.is_empty() {
            return false;
        }
        self.start_copy(capture, cx)
    }

    /// Cmd/Ctrl+C; with nothing to copy the key goes on.
    pub(super) fn on_copy(&mut self, _: &actions::Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.copy(cx) {
            cx.propagate();
        }
    }

    /// "Copy all"; with nothing to copy the command goes on.
    pub(super) fn on_copy_all(
        &mut self,
        _: &actions::CopyAll,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.copy_all(cx) {
            cx.propagate();
        }
    }

    /// How the grid writes a cell's text: an export's default too.
    pub(super) fn copy_cells(&self) -> &CellExport<D::Row> {
        &self.copy.cells
    }

    /// Writes a copy at once if it is small, else starts a job for it. A
    /// copy still being written is cancelled either way.
    fn start_copy(&mut self, capture: ExportCapture<D::Row>, cx: &mut Context<Self>) -> bool {
        self.copy.job = None;
        let exporter = self.copy.exporter.clone();
        let write = move |capture: &ExportCapture<D::Row>| {
            capture.write(|table, cells| {
                // Every line's row was removed since the order was built.
                if table.rows().is_empty() {
                    return None;
                }
                let mut text = String::new();
                exporter.export(table, cells, &mut text);
                Some(text)
            })
        };
        if capture.cell_count() <= INLINE_COPY_CELLS {
            let Some(text) = write(&capture) else {
                return false;
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return true;
        }
        let (done, result) = async_channel::bounded(1);
        let job = self.runner.run(
            Box::new(move || {
                let _ = done.try_send(write(&capture));
            }),
            cx,
        );
        self.copy.job = Some(cx.spawn(async move |this, cx| {
            job.await;
            let Ok(text) = result.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| {
                state.copy.job = None;
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            });
        }));
        true
    }
}
