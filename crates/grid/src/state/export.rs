//! Exporting the view to a stream or a file in the background, and what a
//! copy and an export take when their command runs ([`ExportCapture`]).
//!
//! An export is taken when the command runs, as a copy is: the snapshot, the
//! shown order (a held one included) with its expansion, the columns with
//! their metadata, the totals label and the optimistic values of waiting
//! edits. Rows that arrive or change later are not in it. A task runner job
//! (by default in the background; on the web, one task on the main thread)
//! builds the table and hands it to an [`ExportFormat`] or to the
//! application's closure. No delegate hook runs and no event is emitted:
//! the application awaits the [`ExportJob`].

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};

use gpui_kit::{Context, SharedString, Task};
use rust_i18n::t;

use super::{
    ColumnOf, LiveGridDelegate, LiveGridState, columns::column_of, pending::PendingValues,
    selection::ScopedSelection,
};
use crate::{
    engine::{DisplayRow, Projection},
    export::{
        CellExport, ExportColumn, ExportControl, ExportError, ExportFormat, ExportLine, ExportRow,
        ExportScope, ExportTable,
    },
    model::{ColumnId, ColumnIx, ColumnSet, GridColumns as _, GridRow, NumberFormat},
    state::{ColumnAlign, ColumnZone},
    store::{PinnedEdge, StoreSnapshot},
};

/// What an export takes from the grid: the scope, the header line, the
/// columns and how a text cell is written.
///
/// ```
/// use gpui_kit_grid::{EmptyExport, ExportHeader, ExportRequest, ExportScope, GridRow};
///
/// /// The whole view, every group and tree row expanded, without a header
/// /// line; a file of the header line alone when there is nothing to export.
/// fn everything<R: GridRow>() -> ExportRequest<R> {
///     ExportRequest::new(ExportScope::Expanded)
///         .with_header(ExportHeader::None)
///         .with_empty(EmptyExport::HeaderOnly)
/// }
/// ```
pub struct ExportRequest<R: GridRow> {
    scope: ExportScope,
    header: ExportHeader,
    empty: EmptyExport,
    columns: Option<Vec<R::Column>>,
    cells: Option<CellExport<R>>,
}

impl<R: GridRow> ExportRequest<R> {
    /// An export of `scope` with a header line, the visible columns in
    /// screen order and the grid's [`CellExport`]
    /// (`LiveGridState::with_cell_export`).
    pub fn new(scope: ExportScope) -> Self {
        Self {
            scope,
            header: ExportHeader::default(),
            empty: EmptyExport::default(),
            columns: None,
            cells: None,
        }
    }

    /// Sets the header lines (default `Titles`, whatever the scope; a copy
    /// of the selection has none).
    pub fn with_header(self, header: ExportHeader) -> Self {
        Self { header, ..self }
    }

    /// Sets what an export of an empty scope does (default
    /// `EmptyExport::Nothing`).
    pub fn with_empty(self, empty: EmptyExport) -> Self {
        Self { empty, ..self }
    }

    /// Exports these columns in this order, hidden ones included; columns
    /// the schema does not hold are left out. By default: the visible
    /// columns in screen order; for `ExportScope::Selection` in a cell
    /// mode, the visible columns that hold a selected cell.
    pub fn with_columns(self, columns: impl IntoIterator<Item = R::Column>) -> Self {
        Self {
            columns: Some(columns.into_iter().collect()),
            ..self
        }
    }

    /// Sets how a text format writes a cell (default: the grid's).
    pub fn with_cell_export(self, cells: CellExport<R>) -> Self {
        Self {
            cells: Some(cells),
            ..self
        }
    }

    /// The rows the export takes.
    pub fn scope(&self) -> ExportScope {
        self.scope
    }

    /// The header lines.
    pub fn header(&self) -> ExportHeader {
        self.header
    }

    /// What an export of an empty scope does.
    pub fn empty(&self) -> EmptyExport {
        self.empty
    }
}

impl<R: GridRow> Default for ExportRequest<R> {
    /// The visible lines: `ExportScope::Visible`.
    fn default() -> Self {
        Self::new(ExportScope::Visible)
    }
}

impl<R: GridRow> Clone for ExportRequest<R> {
    fn clone(&self) -> Self {
        Self {
            scope: self.scope,
            header: self.header,
            empty: self.empty,
            columns: self.columns.clone(),
            cells: self.cells.clone(),
        }
    }
}

/// The header lines of an export. An enum, not a flag: the band titles
/// above the column titles would be one more variant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportHeader {
    /// No header line.
    None,
    /// One line of column titles.
    #[default]
    Titles,
}

/// What an export of an empty scope does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmptyExport {
    /// `Err(ExportError::Nothing)` at once: nothing is written and no file
    /// is created, as a copy leaves the clipboard alone.
    #[default]
    Nothing,
    /// The header line alone, as AG Grid writes it.
    HeaderOnly,
}

/// A running export: its progress, its cancellation and, as a `Future`, its
/// result.
///
/// Dropping it cancels the export: the format stops at its next
/// [`ExportControl::advance`], and a file being written to a path is
/// removed. [`detach`](Self::detach) lets it run to the end with nobody
/// waiting. On the web the export runs as one task on the main thread: its
/// progress goes from nothing to done at once, and a cancellation takes
/// effect only before the task starts.
#[must_use = "dropping an export job cancels the export"]
pub struct ExportJob<T> {
    control: ExportControl,
    /// The job's task: dropping it before the job starts keeps it from
    /// running.
    task: Option<Task<()>>,
    /// Closed once the job has run or was dropped. Boxed: a receiver is
    /// not `Unpin`, the job is.
    done: Box<async_channel::Receiver<Result<ExportOutcome<T>, ExportError>>>,
    /// Waits for the result.
    result: Pin<Box<dyn Future<Output = Result<ExportOutcome<T>, ExportError>> + Send>>,
    detached: bool,
}

impl<T: Send + 'static> ExportJob<T> {
    fn new(
        control: ExportControl,
        task: Task<()>,
        done: async_channel::Receiver<Result<ExportOutcome<T>, ExportError>>,
    ) -> Self {
        let result = {
            let done = done.clone();
            Box::pin(async move {
                // A job dropped without running sends nothing.
                done.recv().await.unwrap_or(Err(ExportError::Cancelled))
            })
        };
        Self {
            control,
            task: Some(task),
            done: Box::new(done),
            result,
            detached: false,
        }
    }
}

impl<T> ExportJob<T> {
    /// The rows written of the rows to write: lock-free, for a status bar
    /// or `GridProgress::new(progress.fraction())`.
    pub fn progress(&self) -> ExportProgress {
        ExportProgress {
            rows_written: self.control.rows_written(),
            rows_total: self.control.rows_total(),
        }
    }

    /// Cancels the export: the job's result is `Err(ExportError::Cancelled)`
    /// unless it has already finished.
    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// Returns true if the job has finished, whatever its result.
    pub fn is_finished(&self) -> bool {
        self.done.is_closed()
    }

    /// Lets the export run to the end with nobody waiting for its result.
    pub fn detach(mut self) {
        if let Some(task) = self.task.take() {
            task.detach();
        }
        self.detached = true;
    }
}

impl<T> Future for ExportJob<T> {
    type Output = Result<ExportOutcome<T>, ExportError>;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = std::task::ready!(this.result.as_mut().poll(cx));
        this.task = None;
        Poll::Ready(result)
    }
}

impl<T> Drop for ExportJob<T> {
    fn drop(&mut self) {
        if !self.detached {
            self.control.cancel();
        }
    }
}

/// The rows an export has written of the rows it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportProgress {
    rows_written: usize,
    rows_total: usize,
}

impl ExportProgress {
    /// The rows written so far.
    pub fn rows_written(&self) -> usize {
        self.rows_written
    }

    /// The rows to write: counted ahead when the export starts, exactly
    /// once its job has built the table.
    pub fn rows_total(&self) -> usize {
        self.rows_total
    }

    /// The share written, from 0.0 to 1.0; 1.0 with no row to write.
    pub fn fraction(&self) -> f32 {
        if self.rows_total == 0 {
            return 1.0;
        }
        (self.rows_written.min(self.rows_total) as f64 / self.rows_total as f64) as f32
    }
}

/// A finished export: what it made, and how much it wrote.
#[derive(Clone, Debug)]
pub struct ExportOutcome<T> {
    output: T,
    rows: usize,
    data_rows: usize,
    bytes: Option<u64>,
}

impl<T> ExportOutcome<T> {
    /// What the export made: the sink of `export`, the path of
    /// `export_to_path`, the value of the closure of `export_with`.
    pub fn output(&self) -> &T {
        &self.output
    }

    /// Takes what the export made.
    pub fn into_output(self) -> T {
        self.output
    }

    /// The rows of the table, summary and band rows included, the header
    /// line not.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The application's rows among them.
    pub fn data_rows(&self) -> usize {
        self.data_rows
    }

    /// The bytes written to the sink or the file; `None` for `export_with`.
    pub fn bytes(&self) -> Option<u64> {
        self.bytes
    }
}

/// The rows a copy or an export writes, in the order they are shown.
enum CaptureRows<K> {
    /// The shown lines ([`ExportScope::Visible`]): group captions, the data
    /// rows of expanded groups and tree rows, group footers. The bands go
    /// around them.
    Lines(Arc<Projection>),
    /// The lines as if every group and tree row were expanded
    /// ([`ExportScope::Expanded`]), those the screen hides marked. The bands
    /// go around them.
    Expanded(Arc<Projection>),
    /// The command scope: rows by key, each with its selected columns.
    Scoped(ScopedSelection<K>),
}

impl<K> CaptureRows<K> {
    /// The number of lines, before rows removed since are left out; at most
    /// that for `Expanded`, whose footers are not counted ahead.
    fn len(&self) -> usize {
        match self {
            Self::Lines(projection) => projection.len(),
            Self::Expanded(projection) => projection
                .rows()
                .len()
                .saturating_add(projection.groups().len().saturating_mul(2)),
            Self::Scoped(scoped) => scoped.len(),
        }
    }
}

/// An exported column as the grid showed it when the command ran.
struct CapturedColumn<C> {
    column: C,
    title: SharedString,
    width: f32,
    align: ColumnAlign,
    zone: ColumnZone,
    /// The titles of the bands over it, outermost first.
    bands: Box<[SharedString]>,
}

/// The pinned rows of both bands of `snapshot`.
fn pinned_count<R: GridRow>(snapshot: &StoreSnapshot<R>) -> usize {
    snapshot.pinned_rows(PinnedEdge::Top).len() + snapshot.pinned_rows(PinnedEdge::Bottom).len()
}

/// One copy or export, as taken when its command ran: everything its table
/// is built from. Moves to the job that writes it.
pub(super) struct ExportCapture<R: GridRow> {
    snapshot: Arc<StoreSnapshot<R>>,
    rows: CaptureRows<R::Key>,
    /// The exported columns in order.
    columns: Vec<CapturedColumn<R::Column>>,
    header: bool,
    /// The column of a caption's key value, by the id of its grouping
    /// level: the level's own column, for the column levels that are
    /// exported. Any other caption's value goes to the first exported
    /// column.
    value_columns: Vec<(ColumnId, R::Column)>,
    /// The band of the totals row and its label, translated when the
    /// command ran; `None`: no totals row.
    totals: Option<(PinnedEdge, SharedString)>,
    /// The optimistic values of the waiting edits: a cell is copied and
    /// exported as the screen shows it.
    pending: Arc<PendingValues<R::Key, R::Column>>,
    /// How each column shows its numbers, by `ColumnIx`: a cell is
    /// written as the screen shows it.
    formats: Arc<[NumberFormat]>,
    cells: CellExport<R>,
    /// No row or no column to write.
    is_empty: bool,
}

impl<R: GridRow> ExportCapture<R> {
    /// Returns true if there is nothing to write: no line, pinned row or
    /// totals row in the scope, or no column.
    pub(super) fn is_empty(&self) -> bool {
        self.is_empty
    }

    /// The rows of the table, counted ahead: the lines (at most, for
    /// `Expanded`) and the bands.
    pub(super) fn row_estimate(&self) -> usize {
        let bands = match self.rows {
            CaptureRows::Lines(_) | CaptureRows::Expanded(_) => {
                pinned_count(&self.snapshot) + usize::from(self.totals.is_some())
            }
            CaptureRows::Scoped(_) => 0,
        };
        self.rows.len().saturating_add(bands)
    }

    /// The cells of the table, counted ahead.
    pub(super) fn cell_count(&self) -> usize {
        self.row_estimate().saturating_mul(self.columns.len())
    }

    /// Builds the table and hands it to `write` with the cell export. A
    /// line whose row the snapshot no longer has is left out, so the table
    /// may have no row even when the capture was not empty.
    pub(super) fn write<T>(
        &self,
        write: impl FnOnce(&ExportTable<'_, R>, &CellExport<R>) -> T,
    ) -> T {
        let band_titles: Vec<&str> = self
            .columns
            .iter()
            .flat_map(|column| column.bands.iter().map(SharedString::as_ref))
            .collect();
        let mut band_start = 0;
        let columns: Vec<ExportColumn<'_, R::Column>> = self
            .columns
            .iter()
            .map(|column| {
                let bands = &band_titles[band_start..band_start + column.bands.len()];
                band_start += column.bands.len();
                ExportColumn::new(column.column, &column.title)
                    .with_width(column.width)
                    .with_align(column.align)
                    .with_zone(column.zone)
                    .with_bands(bands)
            })
            .collect();
        let mut rows = self.rows();
        for row in &mut rows {
            *row = row.with_number_formats(&self.formats);
        }
        write(
            &ExportTable::new(&columns, &rows).with_header(self.header),
            &self.cells,
        )
    }

    /// The rows of the table, in the shown order.
    fn rows(&self) -> Vec<ExportRow<'_, R>> {
        let exported = self
            .columns
            .iter()
            .fold(ColumnSet::EMPTY, |mut set, column| {
                set.insert(column.column.ix());
                set
            });
        match &self.rows {
            CaptureRows::Lines(projection) => self.view_rows(
                projection,
                (0..projection.len()).map_while(|ix| projection.line(ix)),
                false,
            ),
            CaptureRows::Expanded(projection) => {
                self.view_rows(projection, projection.expanded_lines(), true)
            }
            // The scope was resolved on this snapshot, so every key is in it.
            // A row whose selected cells are all left out has nothing to
            // write.
            CaptureRows::Scoped(scoped) => scoped
                .keys()
                .iter()
                .enumerate()
                .filter_map(|(ix, key)| {
                    let selected = scoped.columns(ix);
                    if !selected.intersects(exported) {
                        return None;
                    }
                    let row = self.snapshot.get(key)?;
                    Some(self.data_row(row).with_columns(selected))
                })
                .collect(),
        }
    }

    /// A data row with the optimistic values of its waiting edits: one
    /// lookup, and none while no edit waits.
    fn data_row<'a>(&'a self, row: &'a R) -> ExportRow<'a, R> {
        let export = ExportRow::new(row);
        if self.pending.is_empty() {
            return export;
        }
        match self.pending.get(row.key()) {
            Some(values) => export.with_pending(values),
            None => export,
        }
    }

    /// The rows of the view: the top band (the totals row, the outermost,
    /// then the pinned rows), `lines` of `projection`, the bottom band (the
    /// pinned rows, then the totals row). The band rows carry their edge,
    /// tree rows their branch; with `mark_hidden`, the lines the shown
    /// expansion hides are marked.
    ///
    /// A data line whose row the snapshot no longer has, removed or fading,
    /// is left out: a shown order built before a removal still holds it.
    fn view_rows<'a>(
        &'a self,
        projection: &'a Projection,
        lines: impl Iterator<Item = DisplayRow> + 'a,
        mark_hidden: bool,
    ) -> Vec<ExportRow<'a, R>> {
        let snapshot = &*self.snapshot;
        let groups = projection.groups();
        let is_tree = projection.is_tree();
        let first_column = self.columns[0].column;
        let totals = |edge| {
            self.totals
                .as_ref()
                .filter(|(at, _)| *at == edge)
                .map(|(_, label)| {
                    ExportRow::totals(projection.totals(), Some((first_column, label.as_ref())))
                        .with_band(edge)
                })
        };
        // Pinned rows have keys of their own: no edit waits on them.
        let pinned = |edge| {
            snapshot
                .pinned_rows(edge)
                .iter()
                .map(move |row| ExportRow::new(&**row).with_band(edge))
        };

        let mut hidden = HiddenLines::default();
        let mut rows = Vec::with_capacity(self.rows.len() + pinned_count(snapshot) + 1);
        rows.extend(totals(PinnedEdge::Top));
        rows.extend(pinned(PinnedEdge::Top));
        rows.extend(lines.filter_map(|line| {
            let (row, is_hidden) = match line {
                DisplayRow::Data { row: id, depth } => {
                    let branch = if is_tree { projection.branch(id) } else { None };
                    let collapsed = branch.is_some_and(|branch| !branch.is_expanded());
                    let is_hidden = mark_hidden && hidden.next(depth, collapsed, false);
                    let row = self.data_row(snapshot.row(id)?).with_depth(depth);
                    (
                        match branch {
                            Some(branch) => row.with_branch(branch),
                            None => row,
                        },
                        is_hidden,
                    )
                }
                DisplayRow::GroupCaption { group, depth } => {
                    let group = groups.get(group)?;
                    let is_hidden = mark_hidden && hidden.next(depth, !group.is_expanded(), false);
                    let level = group.level_id();
                    let value_column = self
                        .value_columns
                        .iter()
                        .find(|(id, _)| id == level)
                        .map_or(first_column, |(_, column)| *column);
                    (ExportRow::caption(value_column, group), is_hidden)
                }
                DisplayRow::GroupFooter { group, depth } => {
                    let group = groups.get(group)?;
                    let is_hidden = mark_hidden && hidden.next(depth, false, !group.is_expanded());
                    (ExportRow::footer(group), is_hidden)
                }
            };
            Some(row.with_hidden(is_hidden))
        }));
        rows.extend(pinned(PinnedEdge::Bottom));
        rows.extend(totals(PinnedEdge::Bottom));
        rows
    }

    /// Writes the table with `write` and counts what was written, in a job.
    /// `Err(Cancelled)` if the export was cancelled before the job started;
    /// `Err(Nothing)` if the table has no row and `empty` asks for nothing.
    fn run<T>(
        &self,
        empty: EmptyExport,
        control: &ExportControl,
        write: impl FnOnce(
            &ExportTable<'_, R>,
            &CellExport<R>,
            &ExportControl,
        ) -> Result<(T, Option<u64>), ExportError>,
    ) -> Result<ExportOutcome<T>, ExportError> {
        control.advance(0)?;
        self.write(|table, cells| {
            let rows = table.rows().len();
            control.set_rows_total(rows);
            if rows == 0 && empty == EmptyExport::Nothing {
                return Err(ExportError::Nothing);
            }
            let (output, bytes) = write(table, cells, control)?;
            Ok(ExportOutcome {
                output,
                rows,
                data_rows: table
                    .rows()
                    .iter()
                    .filter(|row| row.line() == ExportLine::Data)
                    .count(),
                bytes,
            })
        })
    }
}

/// Marks the lines of an expanded walk that the shown expansion hides: the
/// lines deeper than a collapsed caption or tree row after it, and the
/// footer of a collapsed group.
#[derive(Default)]
struct HiddenLines {
    /// The depth of the shown collapsed line whose deeper lines are hidden.
    below: Option<u16>,
}

impl HiddenLines {
    /// Returns true if the next line, at `depth`, is hidden. `collapsed`:
    /// the line hides the deeper lines after it; `collapsed_footer`: it is
    /// the footer of a collapsed group.
    fn next(&mut self, depth: u16, collapsed: bool, collapsed_footer: bool) -> bool {
        let under_collapsed = match self.below {
            Some(below) if depth > below => true,
            _ => {
                self.below = None;
                false
            }
        };
        if !under_collapsed && collapsed {
            self.below = Some(depth);
        }
        under_collapsed || collapsed_footer
    }
}

/// A sink that counts the bytes written to it.
struct CountingWriter<W> {
    inner: W,
    bytes: u64,
}

impl<W: io::Write> io::Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)?;
        self.bytes += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes a file through `<path>.part` beside it, renamed over `path` once
/// complete: a cancelled or failed export removes the part file and leaves
/// `path` as it was. Returns the bytes written.
#[cfg(not(target_family = "wasm"))]
fn write_through_part_file(
    path: &std::path::Path,
    control: &ExportControl,
    write: impl FnOnce(&mut dyn io::Write) -> Result<(), ExportError>,
) -> Result<u64, ExportError> {
    let Some(name) = path.file_name() else {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "the export path names no file").into(),
        );
    };
    let mut part_name = name.to_os_string();
    part_name.push(".part");
    let part = path.with_file_name(part_name);
    let written = (|| {
        let file = std::fs::File::create(&part)?;
        let mut out = CountingWriter {
            inner: io::BufWriter::new(file),
            bytes: 0,
        };
        write(&mut out)?;
        let bytes = out.bytes;
        let file = out
            .inner
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        drop(file);
        // Cancelled after its last row: `path` still stays as it was.
        control.advance(0)?;
        std::fs::rename(&part, path)?;
        Ok(bytes)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Writes `request` with `format` to `sink`, in a task runner job, and
    /// gives the sink back when it is done. The snapshot, the shown order (a
    /// held one included) with its expansion, the columns, the totals label
    /// and the optimistic values of waiting edits are taken now.
    ///
    /// Returns `Err(ExportError::Nothing)` at once, without touching the
    /// sink, if the scope is empty ([`Self::is_export_empty`]), unless the
    /// request asks for the header line alone (`EmptyExport::HeaderOnly`).
    /// The job writes on the web too: a `Vec<u8>` sink there holds the
    /// bytes of the file the application lets the user save.
    ///
    /// ```
    /// use gpui_kit::{Context, Task};
    /// use gpui_kit_grid::{
    ///     CsvFormat, ExportError, ExportRequest, ExportScope, LiveGridDelegate, LiveGridState,
    /// };
    ///
    /// /// The CSV text of the visible lines, once it is written.
    /// fn csv_text<D: LiveGridDelegate>(
    ///     grid: &mut LiveGridState<D>,
    ///     cx: &mut Context<LiveGridState<D>>,
    /// ) -> Result<Task<Result<Vec<u8>, ExportError>>, ExportError> {
    ///     let job = grid.export(
    ///         ExportRequest::new(ExportScope::Visible),
    ///         CsvFormat::new(),
    ///         Vec::new(),
    ///         cx,
    ///     )?;
    ///     Ok(cx.spawn(async move |_, _| Ok(job.await?.into_output())))
    /// }
    /// ```
    pub fn export<W: io::Write + Send + 'static>(
        &mut self,
        request: ExportRequest<D::Row>,
        format: impl ExportFormat<D::Row>,
        sink: W,
        cx: &mut Context<Self>,
    ) -> Result<ExportJob<W>, ExportError> {
        self.start_export(
            request,
            move |table, cells, control| {
                let mut out = CountingWriter {
                    inner: sink,
                    bytes: 0,
                };
                format.export(table, cells, &mut out, control)?;
                Ok((out.inner, Some(out.bytes)))
            },
            cx,
        )
    }

    /// As [`Self::export`], to a file at `path`: written to `<path>.part`
    /// beside it, then renamed over `path`. A cancelled or failed export
    /// removes the part file and leaves `path` as it was; on Windows a file
    /// another program holds open fails the rename with `ExportError::Io`.
    /// Not on the web, which has no file system: export to a `Vec<u8>` there.
    ///
    /// The application asks for the path first (`App::prompt_for_new_path`
    /// with the format's extension) and may open the file once the job is
    /// done (`App::open_with_system`).
    #[cfg(not(target_family = "wasm"))]
    pub fn export_to_path(
        &mut self,
        request: ExportRequest<D::Row>,
        format: impl ExportFormat<D::Row>,
        path: impl Into<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) -> Result<ExportJob<std::path::PathBuf>, ExportError> {
        let path = path.into();
        self.start_export(
            request,
            move |table, cells, control| {
                let bytes = write_through_part_file(&path, control, |out| {
                    format.export(table, cells, out, control)
                })?;
                Ok((path, Some(bytes)))
            },
            cx,
        )
    }

    /// Hands the export table to `write` in a task runner job: an
    /// application builds its own file there, such as an xlsx workbook with
    /// its own library, and returns what it made. `write` calls
    /// [`ExportControl::advance`] as an [`ExportFormat`] does. What is taken
    /// now and when nothing is exported: as [`Self::export`].
    pub fn export_with<T: Send + 'static>(
        &mut self,
        request: ExportRequest<D::Row>,
        write: impl FnOnce(
            &ExportTable<'_, D::Row>,
            &CellExport<D::Row>,
            &ExportControl,
        ) -> Result<T, ExportError>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) -> Result<ExportJob<T>, ExportError> {
        self.start_export(
            request,
            move |table, cells, control| Ok((write(table, cells, control)?, None)),
            cx,
        )
    }

    /// Returns true if an export of `scope` would have nothing to write
    /// (`ExportError::Nothing`): no line, pinned row or totals row with
    /// aggregates for `Visible` and `Expanded`, an empty command scope for
    /// `Selection`. Check it before asking for a file name.
    pub fn is_export_empty(&self, scope: ExportScope) -> bool {
        self.capture(scope, None, true, CellExport::default())
            .is_empty()
    }

    /// Takes the export of `request` and starts its job.
    fn start_export<T: Send + 'static>(
        &mut self,
        request: ExportRequest<D::Row>,
        write: impl FnOnce(
            &ExportTable<'_, D::Row>,
            &CellExport<D::Row>,
            &ExportControl,
        ) -> Result<(T, Option<u64>), ExportError>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) -> Result<ExportJob<T>, ExportError> {
        let ExportRequest {
            scope,
            header,
            empty,
            columns,
            cells,
        } = request;
        let cells = cells.unwrap_or_else(|| self.copy_cells().clone());
        let capture = self.capture(
            scope,
            columns.as_deref(),
            header == ExportHeader::Titles,
            cells,
        );
        if capture.is_empty() && (empty == EmptyExport::Nothing || capture.columns.is_empty()) {
            return Err(ExportError::Nothing);
        }
        let control = ExportControl::new(capture.row_estimate());
        let (done, result) = async_channel::bounded(1);
        let job_control = control.clone();
        let task = self.runner.run(
            Box::new(move || {
                let _ = done.try_send(capture.run(empty, &job_control, write));
            }),
            cx,
        );
        Ok(ExportJob::new(control, task, result))
    }

    /// What a copy or an export of `scope` takes now: the snapshot, the
    /// shown order or the command scope, `columns` (by default the visible
    /// columns in screen order; for `Selection`, those that hold a selected
    /// cell) with their metadata, the summaries of a view scope and the
    /// optimistic values. O(columns), and O(selection) for `Selection`;
    /// the rows are walked in the job.
    pub(super) fn capture(
        &self,
        scope: ExportScope,
        columns: Option<&[ColumnOf<D>]>,
        header: bool,
        cells: CellExport<D::Row>,
    ) -> ExportCapture<D::Row> {
        let (rows, selected) = match scope {
            ExportScope::Selection => {
                let scoped = self.command_scope(self.focus_target().as_ref());
                let selected = (0..scoped.len()).fold(ColumnSet::EMPTY, |mut set, ix| {
                    set.union_with(scoped.columns(ix));
                    set
                });
                (CaptureRows::Scoped(scoped), selected)
            }
            ExportScope::Visible => (
                CaptureRows::Lines(self.projection.clone()),
                self.columns.visible_set(),
            ),
            ExportScope::Expanded => (
                CaptureRows::Expanded(self.projection.clone()),
                self.columns.visible_set(),
            ),
        };
        let columns: Vec<CapturedColumn<ColumnOf<D>>> = match columns {
            Some(columns) => columns
                .iter()
                .filter_map(|column| self.captured_column(column.ix()))
                .collect(),
            None => {
                // An empty scope: the header line of every visible column.
                let selected = if selected.is_empty() {
                    self.columns.visible_set()
                } else {
                    selected
                };
                self.columns
                    .visible()
                    .filter(|ix| selected.contains(*ix))
                    .filter_map(|ix| self.captured_column(ix))
                    .collect()
            }
        };
        let (value_columns, totals) = match rows {
            CaptureRows::Scoped(_) => (Vec::new(), None),
            CaptureRows::Lines(_) | CaptureRows::Expanded(_) => {
                let value_columns = self
                    .view
                    .group_by()
                    .iter()
                    .filter_map(|level| {
                        let column = level.as_column()?;
                        let exported = columns.iter().any(|each| each.column == column);
                        exported.then(|| (level.id(), column))
                    })
                    .collect();
                // A totals row without aggregates holds only its label:
                // nothing to write.
                let totals = self
                    .totals()
                    .filter(|_| !self.projection.totals().is_empty())
                    .map(|edge| {
                        let label = SharedString::from(t!("live_grid.totals.label").into_owned());
                        (edge, label)
                    });
                (value_columns, totals)
            }
        };
        let no_rows = match &rows {
            CaptureRows::Scoped(scoped) => scoped.is_empty(),
            CaptureRows::Lines(projection) | CaptureRows::Expanded(projection) => {
                projection.is_empty() && pinned_count(&self.snapshot) == 0 && totals.is_none()
            }
        };
        ExportCapture {
            snapshot: self.snapshot.clone(),
            rows,
            is_empty: no_rows || columns.is_empty(),
            columns,
            header,
            value_columns,
            totals,
            pending: self.pending_values(),
            formats: self.columns.number_formats(),
            cells,
        }
    }

    /// The column `ix` as the grid shows it now; `None` outside the schema.
    fn captured_column(&self, ix: ColumnIx) -> Option<CapturedColumn<ColumnOf<D>>> {
        let column = self.columns.get(ix)?;
        Some(CapturedColumn {
            column: column_of(ix),
            title: column.title().clone(),
            width: f32::from(self.columns.width(ix)),
            align: column.align(),
            zone: self.columns.zone_of(ix),
            bands: self
                .columns
                .bands_of(ix)
                .iter()
                .map(|band| {
                    self.columns
                        .band_title(band)
                        .cloned()
                        .unwrap_or_else(|| SharedString::new_static(band))
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hidden_lines() {
        // A collapsed caption at depth 0 hides its rows and subgroups, and
        // its footer; the next top-level caption is shown.
        let mut hidden = HiddenLines::default();
        let walk = [
            // (depth, collapsed, collapsed footer, hidden)
            (0, true, false, false),
            (1, false, false, true),
            (2, false, false, true),
            (1, true, false, true),
            (2, false, false, true),
            (1, false, true, true),
            (0, false, true, true),
            (0, false, false, false),
            // An expanded caption with a collapsed subgroup.
            (1, true, false, false),
            (2, false, false, true),
            (1, false, true, true),
            (1, false, false, false),
            (2, false, false, false),
            (1, false, false, false),
            (0, false, false, false),
        ];
        for (ix, (depth, collapsed, collapsed_footer, expected)) in walk.into_iter().enumerate() {
            assert_eq!(
                hidden.next(depth, collapsed, collapsed_footer),
                expected,
                "line {ix}"
            );
        }
    }

    #[test]
    fn test_progress_fraction() {
        let progress = |rows_written, rows_total| ExportProgress {
            rows_written,
            rows_total,
        };
        assert_eq!(progress(0, 300).fraction(), 0.0);
        assert_eq!(progress(150, 300).fraction(), 0.5);
        assert_eq!(progress(300, 300).fraction(), 1.0);
        // A format that counts more than the table: done, not more.
        assert_eq!(progress(400, 300).fraction(), 1.0);
        // Nothing to write is done.
        assert_eq!(progress(0, 0).fraction(), 1.0);
    }
}
