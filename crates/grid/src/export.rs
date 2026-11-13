//! Copying and exporting rows: how one cell becomes text ([`CellExporter`],
//! [`CellExport`]), how a table of cells becomes clipboard text
//! ([`Exporter`], [`TsvExporter`]) and how it becomes the bytes of a file
//! ([`ExportFormat`], [`CsvFormat`]). A table holds the application's rows
//! and the summary rows of a grouped view: group captions, group footers and
//! the totals ([`ExportLine`]).
//!
//! This module has no GPUI types, so an export runs in the background on one
//! snapshot. Every seam is `Send + Sync`, and the per-cell seam is dispatched
//! statically for the built-in exporters.
//!
//! The grid writes CSV and TSV itself and depends on no spreadsheet library:
//! an application writes xlsx with its own, from the typed values and the
//! metadata of the table ([`ExportFormat`], "Spreadsheets").

use std::{
    cell::RefCell,
    convert::Infallible,
    fmt,
    fmt::Write as _,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use crate::{
    engine::{AggregateRow, Branch, GroupInfo},
    model::{
        CellValue, ColumnSet, GridColumns, GridRow, NumberFormat, OwnedValue, display_text_in,
        number_fraction_digits, push_fixed, push_timestamp,
    },
    state::{ColumnAlign, ColumnZone},
    store::PinnedEdge,
};

/// Which rows an export takes. A grid copies any of them with
/// [`LiveGridState::copy_scope`].
///
/// [`LiveGridState::copy_scope`]: crate::LiveGridState::copy_scope
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportScope {
    /// Every shown line in the shown order: group captions, the rows of
    /// expanded groups and tree rows, group footers; the pinned bands and
    /// the totals row around them.
    Visible,
    /// The command scope: the selection within the shown order, or the rows
    /// of the group caption the command is over.
    Selection,
    /// As `Visible`, with every group and tree row expanded (LiveGrid's
    /// `forceExpand`).
    Expanded,
}

/// What an exported row is: a row of the application, or a summary row of a
/// grouped view.
///
/// The built-in exporters write a summary row as any other row, so pasted
/// columns stay uniform: a caption holds its group's key value under the
/// group's column and its aggregates under theirs, a footer and the totals
/// hold aggregates. To write them another way, an [`Exporter`] tells rows
/// apart by [`ExportRow::line`] and a [`CellExporter`] tells cells apart by
/// [`ExportCell::line`]; [`LineFilter`] leaves lines out. For example, the
/// row count after a group's key value, `AAPL (12)`:
///
/// ```
/// use std::fmt::Write as _;
///
/// use gpui_kit_grid::{CellExporter, ExportCell, ExportLine, GridRow, RawCells};
///
/// struct CountedCaptions;
///
/// impl<R: GridRow> CellExporter<R> for CountedCaptions {
///     fn write_cell(&self, cell: &ExportCell<'_, R>, out: &mut String) {
///         RawCells::new().write_cell(cell, out);
///         if cell.line() == ExportLine::Caption
///             && !cell.is_aggregate()
///             && let Some(group) = cell.group()
///         {
///             let _ = write!(out, " ({})", group.row_count());
///         }
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExportLine {
    /// A row of the application.
    Data,
    /// A group caption: the group's key value and its aggregates.
    Caption,
    /// A group footer: the group's aggregates.
    Footer,
    /// The totals row: the aggregates of the whole view.
    Totals,
}

impl ExportLine {
    /// Every line, in the order [`ExportLines`] lists them.
    const ALL: [Self; 4] = [Self::Data, Self::Caption, Self::Footer, Self::Totals];

    /// The line's bit in [`ExportLines`].
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`ExportLine`]s: the lines a [`LineFilter`] writes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExportLines(u8);

impl ExportLines {
    /// No line.
    pub const EMPTY: Self = Self(0);

    /// Every line: what the built-in exporters write.
    pub const ALL: Self = Self((1 << ExportLine::ALL.len()) - 1);

    /// The set with `line` added.
    pub const fn with(self, line: ExportLine) -> Self {
        Self(self.0 | line.bit())
    }

    /// The set with `line` left out.
    pub const fn without(self, line: ExportLine) -> Self {
        Self(self.0 & !line.bit())
    }

    /// Returns true if the set holds `line`.
    pub const fn contains(self, line: ExportLine) -> bool {
        self.0 & line.bit() != 0
    }
}

impl Default for ExportLines {
    fn default() -> Self {
        Self::ALL
    }
}

impl fmt::Debug for ExportLines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(ExportLine::ALL.iter().filter(|line| self.contains(**line)))
            .finish()
    }
}

/// Writes the text of one exported cell.
///
/// Called for every exported cell, in the background: it must not lock, and
/// it should not allocate. Implementations append to `out` and never change
/// what is already there; quoting is the [`Exporter`]'s job.
pub trait CellExporter<R: GridRow>: Send + Sync + 'static {
    /// Appends the text of `cell` to `out`.
    fn write_cell(&self, cell: &ExportCell<'_, R>, out: &mut String);
}

/// Raw values that spreadsheets parse back: numbers without digit grouping
/// with the decimals the grid shows, `Bool` as `TRUE` or `FALSE`. A
/// summary cell is written by its value as any other: an aggregate `Float`
/// as `1234.50`, a `Bool` group key as `TRUE`.
///
/// | Value | Text |
/// |---|---|
/// | `Int` | `-1234` |
/// | `Float` | With the decimals of its column's [`NumberFormat`] (`-0.49`, `1234.50` with two), never with an exponent; with [`NumberPrecision::Full`], the shortest text that parses back to the same number (`-0.4899999999999949`, `1234.5`). `NaN` and infinities: empty |
/// | `Decimal` | Its digits with its scale (`12.30`), never with an exponent (feature `decimal`) |
/// | `Bool` | `TRUE`, `FALSE` |
/// | `Text` | As is |
/// | `Timestamp` | In UTC as `2026-11-04 12:34:56`, the text of the cell: Excel and LibreOffice read it as a date and time |
/// | `Icon`, `Image` | Its text |
/// | `Custom` | [`GridValue::export_value`](crate::GridValue::export_value) as above, or the text of the cell |
/// | `Empty` | Empty |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawCells {
    decimal_separator: char,
    precision: NumberPrecision,
}

/// How many decimals [`RawCells`] writes for a `Float`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NumberPrecision {
    /// The decimals the grid shows (the column's [`NumberFormat`]): a copy
    /// pastes what the user saw, `-0.49` and not `-0.4899999999999949`. The
    /// default.
    #[default]
    Shown,
    /// Every digit of the value: the shortest text that parses back to the
    /// same number, for a file another program computes with.
    Full,
}

impl RawCells {
    /// Raw values with `.` as the decimal separator and the decimals the
    /// grid shows.
    pub const fn new() -> Self {
        Self {
            decimal_separator: '.',
            precision: NumberPrecision::Shown,
        }
    }

    /// Sets how many decimals a `Float` gets (default
    /// [`NumberPrecision::Shown`]).
    pub const fn with_precision(mut self, precision: NumberPrecision) -> Self {
        self.precision = precision;
        self
    }

    /// How many decimals a `Float` gets.
    pub const fn precision(&self) -> NumberPrecision {
        self.precision
    }

    /// Writes `separator` between the whole and fractional digits of a
    /// `Float` or a `Decimal`, for example `,` for a spreadsheet in a Russian
    /// locale.
    pub const fn with_decimal_separator(mut self, separator: char) -> Self {
        self.decimal_separator = separator;
        self
    }

    /// The decimal separator of a `Float` or a `Decimal`.
    pub const fn decimal_separator(&self) -> char {
        self.decimal_separator
    }

    fn write_float(self, value: f64, format: NumberFormat, out: &mut String) {
        if !value.is_finite() {
            return;
        }
        let start = out.len();
        match self.precision {
            NumberPrecision::Shown => push_fixed(value, format.decimals(), out),
            // `Display` for `f64` writes the shortest text that parses back
            // to the same value, with at most one `.` and never an exponent.
            NumberPrecision::Full => {
                let _ = write!(out, "{value}");
            }
        }
        self.replace_point(start, out);
    }

    /// Writes a decimal with its scale (`12.30`); `Display` for `Decimal`
    /// never writes an exponent.
    #[cfg(feature = "decimal")]
    fn write_decimal(self, value: rust_decimal::Decimal, out: &mut String) {
        let start = out.len();
        let _ = write!(out, "{value}");
        self.replace_point(start, out);
    }

    /// Replaces the `.` written after `start` with the decimal separator.
    fn replace_point(self, start: usize, out: &mut String) {
        if self.decimal_separator != '.'
            && let Some(dot) = out[start..].find('.')
        {
            let dot = start + dot;
            let mut separator = [0; 4];
            out.replace_range(
                dot..dot + 1,
                self.decimal_separator.encode_utf8(&mut separator),
            );
        }
    }
}

impl Default for RawCells {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: GridRow> CellExporter<R> for RawCells {
    #[inline]
    fn write_cell(&self, cell: &ExportCell<'_, R>, out: &mut String) {
        match cell.value() {
            CellValue::Custom(value) => match value.export_value() {
                // A custom value again is written by its text, as `write_value` does.
                Some(export) => self.write_value(export, cell.number_format(), out),
                None => value.write_text(out),
            },
            value => self.write_value(value, cell.number_format(), out),
        }
    }
}

impl RawCells {
    /// Writes a value other than `Custom`, which `write_cell` resolves.
    #[inline]
    fn write_value(self, value: CellValue<'_>, format: NumberFormat, out: &mut String) {
        match value {
            CellValue::Empty => {}
            CellValue::Bool(value) => out.push_str(if value { "TRUE" } else { "FALSE" }),
            CellValue::Int(value) => {
                let _ = write!(out, "{value}");
            }
            CellValue::Float(value) => self.write_float(value, format, out),
            #[cfg(feature = "decimal")]
            CellValue::Decimal(value) => self.write_decimal(value, out),
            CellValue::Text(text) => out.push_str(text),
            CellValue::Timestamp(value) => push_timestamp(value, out),
            CellValue::Icon(icon) => out.push_str(icon.text()),
            CellValue::Image(image) => out.push_str(image.text()),
            // A link is copied as its text, what the cell shows; its address
            // is in `ExportCell::value` for an application's format.
            CellValue::Link(link) => out.push_str(link.text()),
            CellValue::Custom(value) => value.write_text(out),
        }
    }
}

/// The text painted in the cell: the same function the body paints with
/// (`Float` with its column's decimals, `Bool(true)` as "✓"). A summary
/// cell is written by its value as any other: an aggregate `Float` as
/// `1234.50`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayedCells;

thread_local! {
    /// The number buffer of `DisplayedCells`, reused across cells so that an
    /// export does not allocate per cell.
    static DISPLAY_BUFFER: RefCell<String> = const { RefCell::new(String::new()) };
}

impl<R: GridRow> CellExporter<R> for DisplayedCells {
    #[inline]
    fn write_cell(&self, cell: &ExportCell<'_, R>, out: &mut String) {
        DISPLAY_BUFFER.with_borrow_mut(|buffer| {
            out.push_str(display_text_in(cell.value(), cell.number_format(), buffer))
        });
    }
}

/// The cell exporter of a grid: the built-in exporters dispatched
/// statically, plus a custom one.
///
/// The default is `Raw(RawCells::new())`. A custom exporter is called through
/// its `Arc` without allocating per cell. For example, raw values for a
/// spreadsheet in a Russian locale:
///
/// ```
/// use std::sync::Arc;
///
/// use gpui_kit_grid::{CellExport, CellExporter, CellValue, ExportCell, GridRow, RawCells};
///
/// /// Raw values with a decimal comma and `ИСТИНА` or `ЛОЖЬ`.
/// struct RussianSpreadsheetCells(RawCells);
///
/// impl<R: GridRow> CellExporter<R> for RussianSpreadsheetCells {
///     fn write_cell(&self, cell: &ExportCell<'_, R>, out: &mut String) {
///         match cell.value() {
///             CellValue::Bool(value) => out.push_str(if value { "ИСТИНА" } else { "ЛОЖЬ" }),
///             _ => self.0.write_cell(cell, out),
///         }
///     }
/// }
///
/// fn russian_spreadsheet_cells<R: GridRow>() -> CellExport<R> {
///     CellExport::Custom(Arc::new(RussianSpreadsheetCells(
///         RawCells::new().with_decimal_separator(','),
///     )))
/// }
/// ```
#[non_exhaustive]
pub enum CellExport<R: GridRow> {
    /// Raw values that spreadsheets parse back.
    Raw(RawCells),
    /// The text painted in the cell.
    Displayed(DisplayedCells),
    /// An application exporter.
    Custom(Arc<dyn CellExporter<R>>),
}

impl<R: GridRow> Default for CellExport<R> {
    fn default() -> Self {
        Self::Raw(RawCells::new())
    }
}

impl<R: GridRow> Clone for CellExport<R> {
    fn clone(&self) -> Self {
        match self {
            Self::Raw(raw) => Self::Raw(*raw),
            Self::Displayed(displayed) => Self::Displayed(*displayed),
            Self::Custom(custom) => Self::Custom(custom.clone()),
        }
    }
}

impl<R: GridRow> CellExporter<R> for CellExport<R> {
    #[inline]
    fn write_cell(&self, cell: &ExportCell<'_, R>, out: &mut String) {
        match self {
            Self::Raw(raw) => raw.write_cell(cell, out),
            Self::Displayed(displayed) => displayed.write_cell(cell, out),
            Self::Custom(custom) => custom.write_cell(cell, out),
        }
    }
}

/// Turns an export table into text for the clipboard, or for a file with
/// [`TextFile`].
///
/// Called once per export, in the background.
pub trait Exporter<R: GridRow>: Send + Sync + 'static {
    /// Appends the text of `table` to `out`, writing each cell with `cells`.
    fn export(&self, table: &ExportTable<'_, R>, cells: &CellExport<R>, out: &mut String);
}

/// Turns an export table into the bytes of a file or a stream: CSV, TSV, or
/// an application format such as xlsx written with the application's own
/// library.
///
/// Called once per export, in the background (a `TaskRunner` job). It must
/// call [`ExportControl::advance`] as rows are written, at least once per
/// 1024 rows, and stop with the error it returns: that is how progress shows
/// and how a cancelled export stops.
///
/// # Spreadsheets
///
/// The grid depends on no spreadsheet library. An application writes xlsx,
/// or any other format, with its own: in a format of its own, or in a
/// closure the grid hands the table to
/// ([`LiveGridState::export_with`](crate::LiveGridState::export_with)). The
/// table carries what such a file needs:
///
/// | The table tells | A spreadsheet makes of it |
/// |---|---|
/// | [`ExportCell::value`]: `Int`, `Float`, `Decimal`, `Bool`, `Text`, `Timestamp` | A number, a boolean, a string or a date and time cell |
/// | [`ExportCell::number_hint`] | The number format: the digits after the point, as on screen |
/// | [`ExportColumn::title`] | The header row |
/// | [`ExportColumn::bands`], [`ExportTable::band_depth`] | Rows of merged header cells above the titles |
/// | [`ExportColumn::width`] | The column width |
/// | [`ExportColumn::align`] | The horizontal alignment |
/// | [`ExportTable::has_header`], [`ExportTable::frozen_columns`], [`ExportRow::band`] | The frozen panes: the header rows, the leading columns and the top band |
/// | [`ExportRow::depth`], [`ExportRow::is_hidden`], [`ExportRow::branch`], [`GroupInfo::is_expanded`] | The outline level of a row, and rows hidden under a collapsed level |
/// | [`ExportRow::line`], [`ExportCell::is_aggregate`] | Summary rows set apart, or formulas in place of their values |
/// | [`ExportCell::is_pending`], [`ExportCell::row_value`] | An edit not yet confirmed: its value, or the stored one |
///
/// A spreadsheet keeps 15 significant digits: an `Int` beyond 2^53 loses its
/// last digits unless the format writes it as text.
///
/// The same walk over the table, without a library: a report with one line
/// per row, indented by its depth, numbers with the digits the screen shows,
/// captions and totals marked.
///
/// ```
/// use std::io::Write;
///
/// use gpui_kit_grid::{
///     CellExport, CellValue, ExportControl, ExportError, ExportFormat, ExportLine, ExportTable,
///     GridRow,
/// };
///
/// struct OutlineReport;
///
/// impl<R: GridRow> ExportFormat<R> for OutlineReport {
///     fn extension(&self) -> &str {
///         "txt"
///     }
///
///     fn export(
///         &self,
///         table: &ExportTable<'_, R>,
///         _cells: &CellExport<R>,
///         out: &mut dyn Write,
///         control: &ExportControl,
///     ) -> Result<(), ExportError> {
///         for (ix, row) in table.rows().iter().enumerate() {
///             let mark = match row.line() {
///                 ExportLine::Caption => "▸ ",
///                 ExportLine::Totals => "Σ ",
///                 _ => "",
///             };
///             write!(out, "{:indent$}{mark}", "", indent = usize::from(row.depth()) * 2)?;
///             for column in table.columns() {
///                 let Some(cell) = row.cell(column.column()) else {
///                     continue;
///                 };
///                 let digits = cell
///                     .number_hint()
///                     .map_or(0, |hint| hint.fraction_digits() as usize);
///                 match cell.value() {
///                     CellValue::Float(value) => write!(out, "{value:.digits$} ")?,
///                     CellValue::Int(value) => write!(out, "{value} ")?,
///                     CellValue::Text(text) => write!(out, "{text} ")?,
///                     _ => {}
///                 }
///             }
///             writeln!(out)?;
///             if ix % 1024 == 1023 {
///                 control.advance(1024)?;
///             }
///         }
///         control.advance(table.rows().len() % 1024)
///     }
/// }
/// ```
pub trait ExportFormat<R: GridRow>: Send + Sync + 'static {
    /// The file name extension, without a dot: "csv", "tsv".
    fn extension(&self) -> &str;

    /// Writes `table` to `out`; text cells with `cells`, as a text format
    /// does (a spreadsheet format reads [`ExportCell::value`] instead).
    fn export(
        &self,
        table: &ExportTable<'_, R>,
        cells: &CellExport<R>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError>;
}

/// The progress and cancellation of one export, shared between the job that
/// writes it and the application that waits for it. Clones share them.
#[derive(Clone, Debug)]
pub struct ExportControl {
    shared: Arc<ControlState>,
}

#[derive(Debug)]
struct ControlState {
    rows_total: AtomicUsize,
    rows_written: AtomicUsize,
    cancelled: AtomicBool,
}

impl ExportControl {
    /// A control for an export of `rows_total` rows; the grid makes one per
    /// export, a test of a format makes its own.
    pub fn new(rows_total: usize) -> Self {
        Self {
            shared: Arc::new(ControlState {
                rows_total: AtomicUsize::new(rows_total),
                rows_written: Default::default(),
                cancelled: Default::default(),
            }),
        }
    }

    /// The rows of the table, summary and band rows included, the header
    /// line not. A grid's export counts them ahead when it starts and
    /// exactly once its job has built the table.
    pub fn rows_total(&self) -> usize {
        self.shared.rows_total.load(Ordering::Relaxed)
    }

    /// Sets the rows of the table, once the job has built it.
    pub(crate) fn set_rows_total(&self, rows_total: usize) {
        self.shared.rows_total.store(rows_total, Ordering::Relaxed);
    }

    /// The rows written so far, as the format reported them.
    pub fn rows_written(&self) -> usize {
        self.shared.rows_written.load(Ordering::Relaxed)
    }

    /// Records `rows` more rows written. Returns
    /// `Err(ExportError::Cancelled)` once the export is cancelled: the
    /// format stops and returns it. `advance(0)` only checks.
    pub fn advance(&self, rows: usize) -> Result<(), ExportError> {
        self.shared.rows_written.fetch_add(rows, Ordering::Relaxed);
        if self.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        Ok(())
    }

    /// Returns true if the export is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Relaxed)
    }

    /// Cancels the export: the next [`advance`](Self::advance) fails.
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Relaxed);
    }
}

/// Why an export did not finish.
///
/// `Display` writes the reason; for `Io` and `Other` that is the text of the
/// error inside. What to tell the user is the application's choice.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ExportError {
    /// No line, pinned row or totals row in the scope: nothing was written
    /// and no file was created.
    Nothing,
    /// The export was cancelled, or the application stopped waiting for it.
    Cancelled,
    /// Writing failed; a file being written to a path was removed.
    Io(Arc<io::Error>),
    /// An application format failed, for example its spreadsheet library.
    Other(Arc<dyn std::error::Error + Send + Sync>),
}

impl ExportError {
    /// An error of an application format: an error value, or a message.
    pub fn other(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Other(Arc::from(error.into()))
    }
}

impl From<io::Error> for ExportError {
    fn from(error: io::Error) -> Self {
        Self::Io(Arc::new(error))
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nothing => f.write_str("nothing to export"),
            Self::Cancelled => f.write_str("the export was cancelled"),
            Self::Io(error) => fmt::Display::fmt(error, f),
            Self::Other(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for ExportError {
    /// The source of the error inside, whose text `Display` already wrote.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => error.source(),
            Self::Other(error) => error.source(),
            Self::Nothing | Self::Cancelled => None,
        }
    }
}

/// Tab-separated values that Excel, Numbers and LibreOffice paste as cells.
///
/// Cells are separated by `\t` and lines by `\n`, with nothing after the last
/// line. A field that holds `\t`, `\n`, `\r` or `"` is put in quotes, with
/// inner quotes doubled, as Excel does. A cell the row does not export is an
/// empty field.
///
/// A summary row is a line like any other ([`ExportLine`]): a caption's key
/// value and the aggregates land under their columns, its other fields are
/// empty. A row's depth adds no indent, so the key value stays data; wrap the
/// exporter in a [`LineFilter`] to leave lines out.
///
/// As an [`ExportFormat`] ("tsv") it writes the same text as UTF-8 without a
/// byte order mark, 1024 rows at a time. Both traits name their method
/// `export`: with both in scope, call it through the trait, as
/// `Exporter::export(&TsvExporter, &table, &cells, &mut text)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TsvExporter;

/// The bytes a cell usually takes in delimited text, to reserve the output.
const CELL_ESTIMATE: usize = 8;

/// The rows a file format writes between two writes to its sink: one
/// progress step, and a buffer of about `1024 × columns × 8` bytes.
const FLUSH_ROWS: usize = 1024;

/// How delimited text separates its fields and lines.
#[derive(Clone, Copy)]
struct TextDialect {
    separator: char,
    /// Written before every line but the first.
    between_lines: &'static str,
    /// Written after every line, the last included.
    after_line: &'static str,
}

impl TextDialect {
    /// Tab-separated, lines between lines: the clipboard text.
    const TSV: Self = Self {
        separator: '\t',
        between_lines: "\n",
        after_line: "",
    };

    /// RFC 4180: CRLF after every line.
    const fn csv(separator: char) -> Self {
        Self {
            separator,
            between_lines: "",
            after_line: "\r\n",
        }
    }
}

impl<R: GridRow> Exporter<R> for TsvExporter {
    fn export(&self, table: &ExportTable<'_, R>, cells: &CellExport<R>, out: &mut String) {
        out.reserve(table.rows().len() * (table.columns().len() * CELL_ESTIMATE + 1));
        // The whole text stays in `out`: nothing to flush, nothing to fail.
        write_delimited(table, cells, TextDialect::TSV, out, |_, _| {
            Ok::<_, Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
    }
}

impl<R: GridRow> ExportFormat<R> for TsvExporter {
    fn extension(&self) -> &str {
        "tsv"
    }

    fn export(
        &self,
        table: &ExportTable<'_, R>,
        cells: &CellExport<R>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        write_delimited_file(table, cells, TextDialect::TSV, "", out, control)
    }
}

/// Comma-separated values as RFC 4180 has them: fields separated by `,`,
/// every line ended by CRLF, a field in quotes when it holds the separator,
/// a quote, CR or LF, inner quotes doubled. Leading and trailing spaces are
/// kept without quotes.
///
/// Cells are written by the export's [`CellExport`]: raw values by default
/// (numbers with a `.`, `TRUE` and `FALSE`). A summary row is a line like
/// any other and a cell the row does not export is an empty field, as in
/// [`TsvExporter`]. The header line holds the column titles; header bands
/// are not written.
///
/// The text is UTF-8 with a byte order mark by default, which Excel needs to
/// read it as UTF-8 ([`CsvEncoding`]). A spreadsheet whose decimal separator
/// is a comma reads `;` between fields:
///
/// ```
/// use gpui_kit_grid::{CellExport, CsvEncoding, CsvFormat, GridRow, RawCells};
///
/// let format = CsvFormat::new().with_separator(';');
/// assert_eq!(format.encoding(), CsvEncoding::Utf8WithBom);
///
/// // The cells to export with it: `1234,5`.
/// fn decimal_comma<R: GridRow>() -> CellExport<R> {
///     CellExport::Raw(RawCells::new().with_decimal_separator(','))
/// }
/// ```
///
/// No protection against formulas: a field that starts with `=`, `+`, `-` or
/// `@` is written as it is (`-5` is a number). An application that exports
/// text its users typed adds a `'` with its own [`CellExport::Custom`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsvFormat {
    separator: char,
    encoding: CsvEncoding,
}

impl CsvFormat {
    /// `,` between fields, CRLF after lines, UTF-8 with a byte order mark.
    pub const fn new() -> Self {
        Self {
            separator: ',',
            encoding: CsvEncoding::Utf8WithBom,
        }
    }

    /// Separates fields with `separator`: `;` for a spreadsheet whose
    /// decimal separator is a comma, with
    /// [`RawCells::with_decimal_separator`]`(',')`.
    ///
    /// # Panics
    ///
    /// If `separator` is a quote, CR or LF, which no reader tells apart.
    pub const fn with_separator(mut self, separator: char) -> Self {
        assert!(
            !matches!(separator, '"' | '\r' | '\n'),
            "a CSV separator cannot be a quote or a line break"
        );
        self.separator = separator;
        self
    }

    /// The separator between fields.
    pub const fn separator(&self) -> char {
        self.separator
    }

    /// Encodes the text with `encoding`.
    pub const fn with_encoding(mut self, encoding: CsvEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// How the text is encoded.
    pub const fn encoding(&self) -> CsvEncoding {
        self.encoding
    }
}

impl Default for CsvFormat {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: GridRow> ExportFormat<R> for CsvFormat {
    fn extension(&self) -> &str {
        "csv"
    }

    fn export(
        &self,
        table: &ExportTable<'_, R>,
        cells: &CellExport<R>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        write_delimited_file(
            table,
            cells,
            TextDialect::csv(self.separator),
            self.encoding.preamble(),
            out,
            control,
        )
    }
}

/// How the text of a CSV file is encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CsvEncoding {
    /// UTF-8 starting with the byte order mark `EF BB BF`: Excel reads it as
    /// UTF-8, and without the mark as the system's legacy code page.
    #[default]
    Utf8WithBom,
    /// UTF-8 without a mark: for scripts, Google Sheets and `diff`.
    Utf8,
}

impl CsvEncoding {
    /// The text written before the first line.
    const fn preamble(self) -> &'static str {
        match self {
            Self::Utf8WithBom => "\u{feff}",
            Self::Utf8 => "",
        }
    }
}

/// Writes `table` as delimited text into `buffer`: the header line, then the
/// rows. Calls `flush` after every [`FLUSH_ROWS`] rows and after the last,
/// with the rows written since the previous call; `flush` takes the text out
/// of `buffer` or leaves it there to grow.
fn write_delimited<R: GridRow, E>(
    table: &ExportTable<'_, R>,
    cells: &CellExport<R>,
    dialect: TextDialect,
    buffer: &mut String,
    mut flush: impl FnMut(&mut String, usize) -> Result<(), E>,
) -> Result<(), E> {
    let columns = table.columns();
    let separator = dialect.separator;
    // Holds a field while it is quoted; quoting is rare, so this rarely
    // allocates at all.
    let mut scratch = String::new();
    let mut is_first_line = true;
    let mut begin_line = |buffer: &mut String| {
        if !is_first_line {
            buffer.push_str(dialect.between_lines);
        }
        is_first_line = false;
    };

    if table.has_header() {
        begin_line(buffer);
        for (ix, column) in columns.iter().enumerate() {
            if ix > 0 {
                buffer.push(separator);
            }
            let start = buffer.len();
            buffer.push_str(column.title());
            quote_field(buffer, start, separator, &mut scratch);
        }
        buffer.push_str(dialect.after_line);
    }
    if table.rows().is_empty() {
        return flush(buffer, 0);
    }
    for chunk in table.rows().chunks(FLUSH_ROWS) {
        for row in chunk {
            begin_line(buffer);
            for (ix, column) in columns.iter().enumerate() {
                if ix > 0 {
                    buffer.push(separator);
                }
                if let Some(cell) = row.cell(column.column()) {
                    let start = buffer.len();
                    cells.write_cell(&cell, buffer);
                    quote_field(buffer, start, separator, &mut scratch);
                }
            }
            buffer.push_str(dialect.after_line);
        }
        flush(buffer, chunk.len())?;
    }
    Ok(())
}

/// Writes `table` as delimited text to `out`, after `preamble`: a buffer of
/// [`FLUSH_ROWS`] rows at a time, one progress step per write.
fn write_delimited_file<R: GridRow>(
    table: &ExportTable<'_, R>,
    cells: &CellExport<R>,
    dialect: TextDialect,
    preamble: &str,
    out: &mut dyn io::Write,
    control: &ExportControl,
) -> Result<(), ExportError> {
    let rows = table.rows().len().min(FLUSH_ROWS);
    let mut buffer = String::with_capacity(
        preamble.len() + (rows + 1) * (table.columns().len() * CELL_ESTIMATE + 2),
    );
    buffer.push_str(preamble);
    write_delimited(table, cells, dialect, &mut buffer, |buffer, rows| {
        out.write_all(buffer.as_bytes())?;
        buffer.clear();
        control.advance(rows)
    })?;
    out.flush()?;
    Ok(())
}

/// Quotes the field from `start` to the end of `out` if it holds the
/// separator, a quote, CR or LF, doubling its quotes: one rule for TSV and
/// CSV.
#[inline]
fn quote_field(out: &mut String, start: usize, separator: char, scratch: &mut String) {
    let field = &out.as_bytes()[start..];
    let needs_quotes = if separator.is_ascii() {
        let separator = separator as u8;
        field
            .iter()
            .any(|&byte| byte == separator || matches!(byte, b'\n' | b'\r' | b'"'))
    } else {
        field
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r' | b'"'))
            || out[start..].contains(separator)
    };
    if !needs_quotes {
        return;
    }
    scratch.clear();
    scratch.push_str(&out[start..]);
    out.truncate(start);
    out.push('"');
    for (ix, part) in scratch.split('"').enumerate() {
        if ix > 0 {
            out.push_str("\"\"");
        }
        out.push_str(part);
    }
    out.push('"');
}

/// Any clipboard [`Exporter`] as a file format: the whole text is written,
/// then saved as UTF-8 without a byte order mark. Progress jumps from 0 to
/// done.
///
/// ```
/// use gpui_kit_grid::{TextFile, TsvExporter};
///
/// let format = TextFile::new(TsvExporter, "txt");
/// let _: &TsvExporter = format.exporter();
/// ```
#[derive(Clone, Debug)]
pub struct TextFile<E> {
    exporter: E,
    extension: Arc<str>,
}

impl<E> TextFile<E> {
    /// Saves the text of `exporter` in a file named with `extension`, given
    /// without a dot.
    pub fn new(exporter: E, extension: impl Into<Arc<str>>) -> Self {
        Self {
            exporter,
            extension: extension.into(),
        }
    }

    /// The exporter that writes the text.
    pub fn exporter(&self) -> &E {
        &self.exporter
    }
}

impl<R: GridRow, E: Exporter<R>> ExportFormat<R> for TextFile<E> {
    fn extension(&self) -> &str {
        &self.extension
    }

    fn export(
        &self,
        table: &ExportTable<'_, R>,
        cells: &CellExport<R>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        // A cancelled export does not build its text.
        control.advance(0)?;
        let mut text = String::new();
        self.exporter.export(table, cells, &mut text);
        out.write_all(text.as_bytes())?;
        out.flush()?;
        control.advance(table.rows().len())
    }
}

/// Writes only some lines of a table, with another exporter or format: for
/// example a copy without group footers and the totals, or the application's
/// rows alone. It wraps any clipboard format, and a grid takes it as any
/// exporter (`LiveGridState::with_exporter`); it wraps any file format, and
/// an export takes it as any format.
///
/// ```
/// use gpui_kit_grid::{CsvFormat, ExportLine, ExportLines, LineFilter, TsvExporter};
///
/// let exporter = LineFilter::new(
///     TsvExporter,
///     ExportLines::ALL
///         .without(ExportLine::Footer)
///         .without(ExportLine::Totals),
/// );
/// assert!(!exporter.lines().contains(ExportLine::Footer));
///
/// // The application's rows alone, to a CSV file.
/// let format = LineFilter::new(CsvFormat::new(), ExportLines::EMPTY.with(ExportLine::Data));
/// ```
///
/// A table without a left-out line goes to the exporter as it is; otherwise
/// the kept rows are copied into one list per export, in the background. A
/// file format's progress counts the left-out rows as written at the start.
#[derive(Clone, Copy, Debug, Default)]
pub struct LineFilter<E> {
    exporter: E,
    lines: ExportLines,
}

impl<E> LineFilter<E> {
    /// Writes the `lines` of a table with `exporter`.
    pub fn new(exporter: E, lines: ExportLines) -> Self {
        Self { exporter, lines }
    }

    /// The exporter that writes the kept lines.
    pub fn exporter(&self) -> &E {
        &self.exporter
    }

    /// The lines written.
    pub fn lines(&self) -> ExportLines {
        self.lines
    }

    /// The kept rows of `table`, or `None` if it keeps every row.
    fn kept_rows<'a, R: GridRow>(
        &self,
        table: &ExportTable<'a, R>,
    ) -> Option<Vec<ExportRow<'a, R>>> {
        let is_kept = |row: &ExportRow<'_, R>| self.lines.contains(row.line());
        if table.rows().iter().all(is_kept) {
            return None;
        }
        Some(table.rows().iter().copied().filter(is_kept).collect())
    }
}

impl<R: GridRow, E: Exporter<R>> Exporter<R> for LineFilter<E> {
    fn export(&self, table: &ExportTable<'_, R>, cells: &CellExport<R>, out: &mut String) {
        match self.kept_rows(table) {
            None => self.exporter.export(table, cells, out),
            Some(kept) => self.exporter.export(&table.with_rows(&kept), cells, out),
        }
    }
}

impl<R: GridRow, E: ExportFormat<R>> ExportFormat<R> for LineFilter<E> {
    fn extension(&self) -> &str {
        self.exporter.extension()
    }

    fn export(
        &self,
        table: &ExportTable<'_, R>,
        cells: &CellExport<R>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        match self.kept_rows(table) {
            None => self.exporter.export(table, cells, out, control),
            Some(kept) => {
                control.advance(table.rows().len() - kept.len())?;
                self.exporter
                    .export(&table.with_rows(&kept), cells, out, control)
            }
        }
    }
}

/// A column of an [`ExportTable`]: the application column, its header text,
/// and how the grid shows it, for a spreadsheet format to match.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportColumn<'a, C> {
    column: C,
    title: &'a str,
    width: Option<f32>,
    align: ColumnAlign,
    zone: ColumnZone,
    bands: &'a [&'a str],
}

impl<'a, C: GridColumns> ExportColumn<'a, C> {
    /// Creates a column with its header text: no width, `Auto` alignment, in
    /// the scrolling zone, under no band.
    pub const fn new(column: C, title: &'a str) -> Self {
        Self {
            column,
            title,
            width: None,
            align: ColumnAlign::Auto,
            zone: ColumnZone::Scrolling,
            bands: &[],
        }
    }

    /// Sets the column's width on screen, in logical pixels. A spreadsheet
    /// format derives its own width from it: in Excel, about
    /// `(width - 5) / 7` characters of Calibri 11.
    pub fn with_width(self, width: f32) -> Self {
        Self {
            width: Some(width),
            ..self
        }
    }

    /// Sets the column's alignment.
    pub fn with_align(self, align: ColumnAlign) -> Self {
        Self { align, ..self }
    }

    /// Sets the zone the column is shown in.
    pub fn with_zone(self, zone: ColumnZone) -> Self {
        Self { zone, ..self }
    }

    /// Sets the titles of the header bands over the column, outermost first
    /// (`ColumnBand`): rows of merged header cells in a spreadsheet.
    pub fn with_bands(self, bands: &'a [&'a str]) -> Self {
        Self { bands, ..self }
    }

    /// The application column.
    pub fn column(&self) -> C {
        self.column
    }

    /// The header text, written when the table has a header line.
    pub fn title(&self) -> &'a str {
        self.title
    }

    /// The column's width on screen, in logical pixels; `None` if not given.
    pub fn width(&self) -> Option<f32> {
        self.width
    }

    /// The column's alignment; `Auto` puts numbers right and the rest left.
    pub fn align(&self) -> ColumnAlign {
        self.align
    }

    /// The zone the column is shown in: the leading `FrozenLeft` columns are
    /// what a spreadsheet freezes ([`ExportTable::frozen_columns`]).
    pub fn zone(&self) -> ColumnZone {
        self.zone
    }

    /// The titles of the header bands over the column, outermost first;
    /// empty under no band.
    pub fn bands(&self) -> &'a [&'a str] {
        self.bands
    }
}

/// A row of an [`ExportTable`]: a row of the application, or a summary row
/// of a grouped view ([`ExportLine`]).
///
/// | Row | Cell of a column | Value |
/// |---|---|---|
/// | [`new`](Self::new) | Every column | The row's value |
/// | [`caption`](Self::caption) | `value_column` | The group's key value, also where the column has an aggregate |
/// | | An aggregated column | The group's aggregate |
/// | [`footer`](Self::footer) | An aggregated column | The group's aggregate |
/// | [`totals`](Self::totals) | An aggregated column | The view's aggregate |
/// | | The label's column without an aggregate | The label |
///
/// A summary row has no cell in any other column: an empty field. An empty
/// aggregate is a cell with [`CellValue::Empty`].
///
/// Marks tell where the row is on screen, for a spreadsheet's outline and
/// frozen panes: its pinned band ([`band`](Self::band)), whether a collapsed
/// group or tree row hides it ([`is_hidden`](Self::is_hidden)), and its
/// children in a tree ([`branch`](Self::branch)). A data row may carry the
/// optimistic values of edits still waiting for their answer
/// ([`with_pending`](Self::with_pending)): its cells show them, as the
/// screen does.
pub struct ExportRow<'a, R: GridRow> {
    source: RowSource<'a, R>,
    columns: ColumnSet,
    /// How each column shows its numbers, by `ColumnIx`; empty: the
    /// default.
    formats: &'a [NumberFormat],
    depth: u16,
    band: Option<PinnedEdge>,
    hidden: bool,
}

/// What an exported row is made of.
enum RowSource<'a, R: GridRow> {
    /// A row of the application, with what only a data row has. Smaller than
    /// the totals variant, so the extras cost the row no space.
    Data {
        row: &'a R,
        /// Optimistic values by column, which replace the row's values.
        pending: &'a [(R::Column, OwnedValue)],
        branch: Option<Branch>,
    },
    /// The group's key value goes to `value_column`.
    Caption {
        group: GroupInfo<'a>,
        value_column: R::Column,
    },
    Footer(GroupInfo<'a>),
    /// `label` goes to its column when the column has no aggregate.
    Totals {
        aggregates: AggregateRow<'a>,
        label: Option<(R::Column, &'a str)>,
    },
}

impl<R: GridRow> Clone for RowSource<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: GridRow> Copy for RowSource<'_, R> {}

impl<'a, R: GridRow> ExportRow<'a, R> {
    /// A row of the application that exports every column of the table, at
    /// depth 0.
    pub fn new(row: &'a R) -> Self {
        Self::of(
            RowSource::Data {
                row,
                pending: &[],
                branch: None,
            },
            0,
        )
    }

    /// The caption of `group`: its key value in `value_column`, its
    /// aggregates in their columns, at the group's depth. The grid passes
    /// the group's own column when it is exported, else the first exported
    /// column.
    pub fn caption(value_column: R::Column, group: GroupInfo<'a>) -> Self {
        Self::of(
            RowSource::Caption {
                group,
                value_column,
            },
            group.depth(),
        )
    }

    /// The footer of `group`: its aggregates in their columns, at the
    /// group's depth.
    pub fn footer(group: GroupInfo<'a>) -> Self {
        Self::of(RowSource::Footer(group), group.depth())
    }

    /// The totals row: the aggregates of the whole view in their columns,
    /// and `label` in its column when that column has no aggregate; at
    /// depth 0.
    pub fn totals(aggregates: AggregateRow<'a>, label: Option<(R::Column, &'a str)>) -> Self {
        Self::of(RowSource::Totals { aggregates, label }, 0)
    }

    fn of(source: RowSource<'a, R>, depth: u16) -> Self {
        Self {
            source,
            columns: ColumnSet::first(R::Column::all().len()),
            formats: &[],
            depth,
            band: None,
            hidden: false,
        }
    }

    /// Sets how each column shows its numbers, by `ColumnIx`
    /// (`GridColumn::with_number_format`): a column past the end shows the
    /// default. The grid sets its columns' formats on every row it
    /// exports, so a cell is written as the screen shows it.
    pub fn with_number_formats(self, formats: &'a [NumberFormat]) -> Self {
        Self { formats, ..self }
    }

    /// Sets the row's level under group captions or tree parents: a data
    /// row of a group is one level below the group's caption.
    pub fn with_depth(self, depth: u16) -> Self {
        Self { depth, ..self }
    }

    /// Exports only `columns` of this row; its other cells in the table are
    /// empty fields. A cell selection copies its bounding rectangle this way.
    pub fn with_columns(self, columns: ColumnSet) -> Self {
        Self { columns, ..self }
    }

    /// Marks a row of a pinned band, or the totals row in it, with the band's
    /// edge. The top band comes before the other lines and is frozen with the
    /// header on screen.
    pub fn with_band(self, edge: PinnedEdge) -> Self {
        Self {
            band: Some(edge),
            ..self
        }
    }

    /// Marks the row hidden on screen: under a collapsed group caption or
    /// tree row. Only an `ExportScope::Expanded` export writes such rows; a
    /// spreadsheet may write them hidden under a collapsed outline level.
    pub fn with_hidden(self, hidden: bool) -> Self {
        Self { hidden, ..self }
    }

    /// Marks a tree row with shown children: their count and whether it is
    /// expanded on screen. A summary row is no tree row: ignored there.
    pub fn with_branch(self, branch: Branch) -> Self {
        let source = match self.source {
            RowSource::Data { row, pending, .. } => RowSource::Data {
                row,
                pending,
                branch: Some(branch),
            },
            source => source,
        };
        Self { source, ..self }
    }

    /// Sets the optimistic values of edits to this row that still wait for
    /// their answer, by column: they replace the row's values of their
    /// columns, as on screen ([`ExportCell::is_pending`]). A summary row has
    /// none: ignored there.
    pub fn with_pending(self, pending: &'a [(R::Column, OwnedValue)]) -> Self {
        let source = match self.source {
            RowSource::Data { row, branch, .. } => RowSource::Data {
                row,
                pending,
                branch,
            },
            source => source,
        };
        Self { source, ..self }
    }

    /// What the row is.
    pub fn line(&self) -> ExportLine {
        match self.source {
            RowSource::Data { .. } => ExportLine::Data,
            RowSource::Caption { .. } => ExportLine::Caption,
            RowSource::Footer(_) => ExportLine::Footer,
            RowSource::Totals { .. } => ExportLine::Totals,
        }
    }

    /// The application row; `None` for a caption, a footer or the totals.
    pub fn row(&self) -> Option<&'a R> {
        match self.source {
            RowSource::Data { row, .. } => Some(row),
            _ => None,
        }
    }

    /// The edge of the pinned band the row is in; `None` outside the bands.
    pub fn band(&self) -> Option<PinnedEdge> {
        self.band
    }

    /// Returns true if the row is hidden on screen, under a collapsed group
    /// caption or tree row.
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// The shown children of a tree row; `None` for a leaf, outside a tree
    /// and for a summary row.
    pub fn branch(&self) -> Option<Branch> {
        match self.source {
            RowSource::Data { branch, .. } => branch,
            _ => None,
        }
    }

    /// The group of a caption or a footer: its path, row count and
    /// aggregates.
    pub fn group(&self) -> Option<GroupInfo<'a>> {
        match self.source {
            RowSource::Caption { group, .. } | RowSource::Footer(group) => Some(group),
            _ => None,
        }
    }

    /// The row's level under group captions or tree parents; 0 at the top.
    pub fn depth(&self) -> u16 {
        self.depth
    }

    /// The columns this row exports.
    pub fn columns(&self) -> ColumnSet {
        self.columns
    }

    /// The cell of `column`, or `None` if this row does not export it or,
    /// in a summary row, has nothing in it.
    pub fn cell(&self, column: R::Column) -> Option<ExportCell<'a, R>> {
        let ix = column.ix();
        if !self.columns.contains(ix) {
            return None;
        }
        let mut is_pending = false;
        let (source, value, is_aggregate) = match self.source {
            RowSource::Data { row, pending, .. } => {
                let value = match pending.iter().find(|(edited, _)| *edited == column) {
                    Some((_, value)) => {
                        is_pending = true;
                        value.as_cell()
                    }
                    None => row.value(column),
                };
                (CellSource::Data(row), value, false)
            }
            RowSource::Caption {
                group,
                value_column,
            } => {
                let source = CellSource::Caption(group);
                if column == value_column {
                    (source, group.value().as_cell(), false)
                } else {
                    (source, group.aggregates().get(ix)?.as_cell(), true)
                }
            }
            RowSource::Footer(group) => (
                CellSource::Footer(group),
                group.aggregates().get(ix)?.as_cell(),
                true,
            ),
            RowSource::Totals { aggregates, label } => match (aggregates.get(ix), label) {
                (Some(aggregate), _) => (CellSource::Totals, aggregate.as_cell(), true),
                (None, Some((label_column, label))) if label_column == column => {
                    (CellSource::Totals, CellValue::Text(label), false)
                }
                (None, _) => return None,
            },
        };
        Some(ExportCell {
            source,
            column,
            value,
            number_format: self.formats.get(ix.get()).copied().unwrap_or_default(),
            depth: self.depth,
            is_aggregate,
            is_pending,
        })
    }
}

impl<R: GridRow> Clone for ExportRow<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: GridRow> Copy for ExportRow<'_, R> {}

/// One exported cell: what a [`CellExporter`] writes.
pub struct ExportCell<'a, R: GridRow> {
    source: CellSource<'a, R>,
    column: R::Column,
    value: CellValue<'a>,
    number_format: NumberFormat,
    depth: u16,
    is_aggregate: bool,
    is_pending: bool,
}

/// The row an exported cell is of: what [`ExportCell`] reads without the
/// row's other cells.
enum CellSource<'a, R: GridRow> {
    Data(&'a R),
    Caption(GroupInfo<'a>),
    Footer(GroupInfo<'a>),
    Totals,
}

impl<R: GridRow> Clone for CellSource<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: GridRow> Copy for CellSource<'_, R> {}

impl<'a, R: GridRow> ExportCell<'a, R> {
    /// The application row of the cell; `None` in a caption, a footer or
    /// the totals.
    pub fn row(&self) -> Option<&'a R> {
        match self.source {
            CellSource::Data(row) => Some(row),
            _ => None,
        }
    }

    /// The column of the cell.
    pub fn column(&self) -> R::Column {
        self.column
    }

    /// The value of the cell, read once: the row's value, or the optimistic
    /// value of an edit waiting for its answer, as on screen; a summary
    /// row's group key value, aggregate or totals label.
    pub fn value(&self) -> CellValue<'a> {
        self.value
    }

    /// Returns true if [`value`](Self::value) is an edit's optimistic value,
    /// not yet confirmed.
    pub fn is_pending(&self) -> bool {
        self.is_pending
    }

    /// The row's own value of the cell, under a pending one; the same as
    /// [`value`](Self::value) otherwise. `None` in a summary row.
    pub fn row_value(&self) -> Option<CellValue<'a>> {
        match self.source {
            CellSource::Data(row) if self.is_pending => Some(row.value(self.column)),
            CellSource::Data(_) => Some(self.value),
            _ => None,
        }
    }

    /// How the cell's column shows its numbers
    /// (`GridColumn::with_number_format`).
    pub fn number_format(&self) -> NumberFormat {
        self.number_format
    }

    /// How the grid shows the number in the cell, so that a spreadsheet
    /// format can match the screen: `Float` with its column's digits after
    /// the point,
    /// `Decimal` with its scale, `Int` with none; a custom value by its
    /// [`GridValue::export_value`](crate::GridValue::export_value). `None`
    /// for a value that is not a number, a `Timestamp` included. No
    /// allocation.
    pub fn number_hint(&self) -> Option<NumberHint> {
        number_fraction_digits(self.value, self.number_format)
            .map(|fraction_digits| NumberHint { fraction_digits })
    }

    /// What the cell's row is.
    pub fn line(&self) -> ExportLine {
        match self.source {
            CellSource::Data(_) => ExportLine::Data,
            CellSource::Caption(_) => ExportLine::Caption,
            CellSource::Footer(_) => ExportLine::Footer,
            CellSource::Totals => ExportLine::Totals,
        }
    }

    /// The level of the cell's row under group captions or tree parents.
    pub fn depth(&self) -> u16 {
        self.depth
    }

    /// The group of a caption or a footer cell: its path, row count and
    /// aggregates.
    pub fn group(&self) -> Option<GroupInfo<'a>> {
        match self.source {
            CellSource::Caption(group) | CellSource::Footer(group) => Some(group),
            _ => None,
        }
    }

    /// Returns true if the cell holds an aggregate: its group's in a caption
    /// or a footer, the whole view's in the totals. A caption's key value and
    /// the totals label are not aggregates.
    pub fn is_aggregate(&self) -> bool {
        self.is_aggregate
    }
}

impl<R: GridRow> Clone for ExportCell<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: GridRow> Copy for ExportCell<'_, R> {}

/// How the grid shows a number, for an export format to match it
/// ([`ExportCell::number_hint`]): today the digits after the decimal point,
/// from the column's [`NumberFormat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NumberHint {
    fraction_digits: u32,
}

impl NumberHint {
    /// The digits the grid shows after the decimal point: a spreadsheet
    /// format such as `0.00` for 2.
    pub const fn fraction_digits(&self) -> u32 {
        self.fraction_digits
    }
}

/// The rows and columns an [`Exporter`] turns into text and an
/// [`ExportFormat`] into a file.
///
/// Columns are in screen order and rows in the shown order, summary rows
/// among them ([`ExportLine`]). The grid builds the table from one snapshot,
/// so an export never mixes two store versions.
pub struct ExportTable<'a, R: GridRow> {
    columns: &'a [ExportColumn<'a, R::Column>],
    rows: &'a [ExportRow<'a, R>],
    header: bool,
}

impl<'a, R: GridRow> ExportTable<'a, R> {
    /// A table without a header line.
    pub fn new(columns: &'a [ExportColumn<'a, R::Column>], rows: &'a [ExportRow<'a, R>]) -> Self {
        Self {
            columns,
            rows,
            header: false,
        }
    }

    /// Writes a line of column titles before the rows.
    pub fn with_header(self, header: bool) -> Self {
        Self { header, ..self }
    }

    /// The exported columns, in screen order.
    pub fn columns(&self) -> &'a [ExportColumn<'a, R::Column>] {
        self.columns
    }

    /// Returns true if a line of column titles comes before the rows.
    pub fn has_header(&self) -> bool {
        self.header
    }

    /// The exported rows, in the shown order.
    pub fn rows(&self) -> &'a [ExportRow<'a, R>] {
        self.rows
    }

    /// The leading columns in the `FrozenLeft` zone: the columns a
    /// spreadsheet freezes.
    pub fn frozen_columns(&self) -> usize {
        self.columns
            .iter()
            .take_while(|column| column.zone() == ColumnZone::FrozenLeft)
            .count()
    }

    /// The depth of the deepest band over the exported columns: the rows of
    /// band titles a multi-level header takes above the column titles.
    pub fn band_depth(&self) -> usize {
        self.columns
            .iter()
            .map(|column| column.bands().len())
            .max()
            .unwrap_or(0)
    }

    /// The same table with `rows`.
    fn with_rows<'b>(&self, rows: &'b [ExportRow<'b, R>]) -> ExportTable<'b, R>
    where
        'a: 'b,
    {
        ExportTable {
            columns: self.columns,
            rows,
            header: self.header,
        }
    }
}

impl<R: GridRow> Clone for ExportTable<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: GridRow> Copy for ExportTable<'_, R> {}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
    };

    use super::*;
    use crate::{
        engine::{
            Aggregate, EveryGroupFooter, GroupBy, GroupId, Projection, ProjectionEngine as _,
            Rebuild, ViewSpec,
        },
        highlight::HighlightPolicy,
        model::{ColumnIx, OwnedValue, display_text},
        store::{Ingest, Op, StoreSnapshot},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Column {
        Symbol,
        Price,
        Note,
    }

    impl GridColumns for Column {
        fn all() -> &'static [Self] {
            &[Self::Symbol, Self::Price, Self::Note]
        }

        fn id(self) -> &'static str {
            match self {
                Self::Symbol => "symbol",
                Self::Price => "price",
                Self::Note => "note",
            }
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self as u16)
        }
    }

    struct Row {
        key: u32,
        cells: [CellValue<'static>; 3],
        parent: Option<u32>,
    }

    impl Row {
        fn new(key: u32, cells: [CellValue<'static>; 3]) -> Self {
            Self {
                key,
                cells,
                parent: None,
            }
        }

        fn with_parent(self, parent: u32) -> Self {
            Self {
                parent: Some(parent),
                ..self
            }
        }
    }

    impl GridRow for Row {
        type Key = u32;
        type Column = Column;

        fn key(&self) -> &u32 {
            &self.key
        }

        fn value(&self, column: Column) -> CellValue<'_> {
            self.cells[column.ix().get()]
        }

        fn parent_key(&self) -> Option<&u32> {
            self.parent.as_ref()
        }
    }

    const COLUMNS: [ExportColumn<'static, Column>; 3] = [
        ExportColumn::new(Column::Symbol, "Symbol"),
        ExportColumn::new(Column::Price, "Price"),
        ExportColumn::new(Column::Note, "Note \"short\""),
    ];

    fn columns(columns: &[Column]) -> ColumnSet {
        let mut set = ColumnSet::EMPTY;
        for column in columns {
            set.insert(column.ix());
        }
        set
    }

    /// The text `cells` writes for one value.
    fn cell_text(cells: &CellExport<Row>, value: CellValue<'static>) -> String {
        let row = Row::new(0, [value, CellValue::Empty, CellValue::Empty]);
        let cell = ExportRow::new(&row).cell(Column::Symbol).unwrap();
        let mut out = String::new();
        cells.write_cell(&cell, &mut out);
        out
    }

    fn tsv(table: &ExportTable<'_, Row>, cells: &CellExport<Row>) -> String {
        let mut out = String::new();
        Exporter::export(&TsvExporter, table, cells, &mut out);
        out
    }

    #[test]
    fn test_tsv_quoting_and_raw_cells() {
        // Every digit with `NumberPrecision::Full`.
        let raw = CellExport::Raw(RawCells::new().with_precision(NumberPrecision::Full));
        let cases = [
            (CellValue::Int(-1234), "-1234"),
            (CellValue::Int(i64::MIN), "-9223372036854775808"),
            (CellValue::Float(1234.5), "1234.5"),
            (CellValue::Float(0.1), "0.1"),
            (CellValue::Float(0.1 + 0.2), "0.30000000000000004"),
            (CellValue::Float(-0.5), "-0.5"),
            (CellValue::Float(2.0), "2"),
            (CellValue::Float(1e21), "1000000000000000000000"),
            (CellValue::Float(1e-7), "0.0000001"),
            (CellValue::Float(f64::NAN), ""),
            (CellValue::Float(f64::INFINITY), ""),
            (CellValue::Float(f64::NEG_INFINITY), ""),
            (CellValue::Bool(true), "TRUE"),
            (CellValue::Bool(false), "FALSE"),
            (CellValue::Empty, ""),
            (CellValue::Text("Сбербанк"), "Сбербанк"),
        ];
        for (value, expected) in cases {
            assert_eq!(cell_text(&raw, value), expected, "{value:?}");
        }
        // By default, the decimals the grid shows: two unless the column's
        // format says otherwise.
        let shown = CellExport::default();
        for (value, expected) in [
            (CellValue::Float(-0.489_999_999_999_994_9), "-0.49"),
            (CellValue::Float(1234.5), "1234.50"),
            (CellValue::Float(0.1 + 0.2), "0.30"),
            (CellValue::Float(2.0), "2.00"),
            (CellValue::Float(1e21), "1000000000000000000000.00"),
            (CellValue::Float(f64::NAN), ""),
            (CellValue::Int(-1234), "-1234"),
        ] {
            assert_eq!(cell_text(&shown, value), expected, "{value:?}");
        }
        let row = Row::new(0, [CellValue::Float(-0.489_999_999_999_994_9); 3]);
        let formats = [
            NumberFormat::new().with_decimals(1),
            NumberFormat::new().with_decimals(4),
        ];
        let exported = ExportRow::new(&row).with_number_formats(&formats);
        let written = |cells: &CellExport<Row>, column| {
            let mut out = String::new();
            cells.write_cell(&exported.cell(column).unwrap(), &mut out);
            out
        };
        assert_eq!(written(&shown, Column::Symbol), "-0.5");
        assert_eq!(written(&shown, Column::Price), "-0.4900");
        // A column past the formats shows the default.
        assert_eq!(written(&shown, Column::Note), "-0.49");
        assert_eq!(
            written(&CellExport::Displayed(DisplayedCells), Column::Symbol),
            "-0.5"
        );
        assert_eq!(written(&raw, Column::Symbol), "-0.4899999999999949");
        assert_eq!(
            exported.cell(Column::Price).unwrap().number_hint(),
            Some(NumberHint { fraction_digits: 4 })
        );

        // The decimal separator replaces only the dot of a `Float`, also with
        // a separator longer than one byte.
        let comma = RawCells::new().with_decimal_separator(',');
        assert_eq!(comma.decimal_separator(), ',');
        let comma = CellExport::Raw(comma);
        assert_eq!(cell_text(&comma, CellValue::Float(-1234.5)), "-1234,50");
        assert_eq!(
            cell_text(&comma, CellValue::Float(1e21)),
            "1000000000000000000000,00"
        );
        assert_eq!(cell_text(&comma, CellValue::Int(-1234)), "-1234");
        assert_eq!(cell_text(&comma, CellValue::Text("1.5")), "1.5");
        let arabic = CellExport::Raw(RawCells::new().with_decimal_separator('٫'));
        assert_eq!(cell_text(&arabic, CellValue::Float(0.25)), "0٫25");
        assert_eq!(cell_text(&arabic, CellValue::Float(0.5)), "0٫50");

        // A header line, quoted fields and no line break after the last row.
        let rows = [
            Row::new(
                1,
                [
                    CellValue::Text("AAPL"),
                    CellValue::Float(187.25),
                    CellValue::Text("tab\there"),
                ],
            ),
            Row::new(
                2,
                [
                    CellValue::Text("say \"hi\""),
                    CellValue::Int(-3),
                    CellValue::Text("two\nlines\r"),
                ],
            ),
            Row::new(
                3,
                [CellValue::Empty, CellValue::Bool(true), CellValue::Empty],
            ),
        ];
        let all: Vec<_> = rows.iter().map(ExportRow::new).collect();
        let table = ExportTable::new(&COLUMNS, &all).with_header(true);
        assert!(table.has_header());
        assert_eq!(
            tsv(&table, &raw),
            "Symbol\tPrice\t\"Note \"\"short\"\"\"\n\
             AAPL\t187.25\t\"tab\there\"\n\
             \"say \"\"hi\"\"\"\t-3\t\"two\nlines\r\"\n\
             \tTRUE\t"
        );

        // A cell rectangle: cells a row does not export are empty fields, and
        // the exporter is not asked for them.
        let rectangle = [
            ExportRow::new(&rows[0]).with_columns(columns(&[Column::Symbol])),
            ExportRow::new(&rows[1]).with_columns(columns(&[Column::Note])),
        ];
        assert_eq!(rectangle[1].columns(), columns(&[Column::Note]));
        assert!(rectangle[1].cell(Column::Symbol).is_none());
        let table = ExportTable::new(&COLUMNS, &rectangle);
        assert!(!table.has_header());
        assert_eq!(tsv(&table, &raw), "AAPL\t\t\n\t\t\"two\nlines\r\"");

        // Columns are written in the table's order, not the schema's.
        let reordered = [
            ExportColumn::new(Column::Price, "Price"),
            ExportColumn::new(Column::Symbol, "Symbol"),
        ];
        let table = ExportTable::new(&reordered, &all[..1]);
        assert_eq!(tsv(&table, &comma), "187,25\tAAPL");

        // An empty table writes nothing, or only its header line; an export
        // appends to what is already in the buffer.
        assert_eq!(tsv(&ExportTable::new(&COLUMNS[..2], &[]), &raw), "");
        let header_only = ExportTable::new(&COLUMNS[..2], &[]).with_header(true);
        assert_eq!(tsv(&header_only, &raw), "Symbol\tPrice");
        let mut out = String::from("x");
        Exporter::export(
            &TsvExporter,
            &ExportTable::new(&COLUMNS[..1], &all[..1]),
            &raw,
            &mut out,
        );
        assert_eq!(out, "xAAPL");
    }

    #[test]
    fn test_raw_cells_new_values() {
        use chrono::{TimeZone as _, Utc};

        use crate::model::{GridValue, IconValue, ImageValue};

        /// Exports as a number, shows as `3M`.
        #[derive(Debug)]
        struct Tenor(i64);

        impl GridValue for Tenor {
            fn cmp_value(&self, other: &dyn GridValue) -> std::cmp::Ordering {
                other
                    .downcast_ref::<Tenor>()
                    .map_or(std::cmp::Ordering::Less, |other| self.0.cmp(&other.0))
            }

            fn write_text(&self, out: &mut String) {
                let _ = write!(out, "{}M", self.0 / 30);
            }

            fn export_value(&self) -> Option<CellValue<'_>> {
                Some(CellValue::Float(self.0 as f64 / 360.0))
            }
        }

        /// Shows its name and has no export of its own.
        #[derive(Debug)]
        struct Severity;

        impl GridValue for Severity {
            fn cmp_value(&self, _: &dyn GridValue) -> std::cmp::Ordering {
                std::cmp::Ordering::Equal
            }

            fn write_text(&self, out: &mut String) {
                out.push_str("Critical");
            }
        }

        /// Exports another custom value: written as that value's text.
        #[derive(Debug)]
        struct Alias;

        impl GridValue for Alias {
            fn cmp_value(&self, _: &dyn GridValue) -> std::cmp::Ordering {
                std::cmp::Ordering::Equal
            }

            fn write_text(&self, out: &mut String) {
                out.push_str("alias");
            }

            fn export_value(&self) -> Option<CellValue<'_>> {
                Some(CellValue::Custom(&Severity))
            }
        }

        static TENOR: Tenor = Tenor(90);
        let placed = Utc.with_ymd_and_hms(2026, 11, 4, 12, 34, 56).unwrap();
        let raw = CellExport::default();
        let comma = CellExport::Raw(RawCells::new().with_decimal_separator(','));
        let displayed = CellExport::Displayed(DisplayedCells);
        let mut buffer = String::new();
        for (value, expected, with_comma) in [
            (
                CellValue::Timestamp(placed),
                "2026-11-04 12:34:56",
                "2026-11-04 12:34:56",
            ),
            (
                CellValue::Icon(IconValue::new("icons/check.svg", "Filled")),
                "Filled",
                "Filled",
            ),
            (
                CellValue::Image(ImageValue::new("images/ada.png", "Ada")),
                "Ada",
                "Ada",
            ),
            (CellValue::Custom(&Severity), "Critical", "Critical"),
            (CellValue::Custom(&TENOR), "0.25", "0,25"),
            (CellValue::Custom(&Alias), "Critical", "Critical"),
        ] {
            assert_eq!(cell_text(&raw, value), expected, "{value:?}");
            assert_eq!(cell_text(&comma, value), with_comma, "{value:?}");
            // A displayed copy writes the text the body paints.
            assert_eq!(
                cell_text(&displayed, value),
                display_text(value, &mut buffer),
                "{value:?}"
            );
        }
        assert_eq!(cell_text(&displayed, CellValue::Custom(&TENOR)), "3M");

        // In a line, a custom value appends to what is there.
        let rows = [Row::new(
            1,
            [
                CellValue::Icon(IconValue::new("icons/check.svg", "Filled")),
                CellValue::Custom(&TENOR),
                CellValue::Custom(&Severity),
            ],
        )];
        let all: Vec<_> = rows.iter().map(ExportRow::new).collect();
        assert_eq!(
            tsv(&ExportTable::new(&COLUMNS, &all), &raw),
            "Filled\t0.25\tCritical"
        );
        assert_eq!(
            tsv(&ExportTable::new(&COLUMNS, &all), &displayed),
            "Filled\t3M\tCritical"
        );
    }

    #[test]
    fn test_cell_export_variants() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<CellExport<Row>>();
        assert_send_sync::<TsvExporter>();
        assert_send_sync::<Box<dyn Exporter<Row>>>();
        assert_send_sync::<Box<dyn ExportFormat<Row>>>();
        assert_send_sync::<CsvFormat>();
        assert_send_sync::<TextFile<TsvExporter>>();
        assert_send_sync::<LineFilter<CsvFormat>>();
        assert_send_sync::<ExportControl>();
        assert_send_sync::<ExportError>();

        // The default is raw values with a dot.
        assert!(matches!(
            CellExport::<Row>::default(),
            CellExport::Raw(raw) if raw == RawCells::new() && raw.decimal_separator() == '.'
        ));
        assert_eq!(RawCells::default(), RawCells::new());

        // `Displayed` writes the text the body paints.
        let displayed = CellExport::Displayed(DisplayedCells);
        let mut buffer = String::new();
        for value in [
            CellValue::Empty,
            CellValue::Bool(true),
            CellValue::Bool(false),
            CellValue::Int(-42),
            CellValue::Float(1234.567),
            CellValue::Float(0.1),
            CellValue::Float(f64::NAN),
            CellValue::Text("tab\there"),
        ] {
            assert_eq!(
                cell_text(&displayed, value),
                display_text(value, &mut buffer),
                "{value:?}"
            );
        }

        // `Custom` is called once for every exported cell, with its row and
        // column, and its text is quoted like any other.
        #[derive(Default)]
        struct KeyAndColumn {
            calls: AtomicUsize,
        }

        impl CellExporter<Row> for KeyAndColumn {
            fn write_cell(&self, cell: &ExportCell<'_, Row>, out: &mut String) {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let key = cell.row().expect("a data row").key();
                let _ = write!(out, "{key}:{}", cell.column().id());
                if cell.value() == CellValue::Empty {
                    out.push('\t');
                }
            }
        }

        let custom = Arc::new(KeyAndColumn::default());
        let cells = CellExport::Custom(custom.clone());
        let rows = [
            Row::new(7, [CellValue::Int(1), CellValue::Empty, CellValue::Int(3)]),
            Row::new(9, [CellValue::Int(4); 3]),
        ];
        let exported = [
            ExportRow::new(&rows[0]),
            ExportRow::new(&rows[1]).with_columns(columns(&[Column::Price])),
        ];
        let table = ExportTable::new(&COLUMNS, &exported).with_header(true);
        assert_eq!(
            tsv(&table, &cells.clone()),
            "Symbol\tPrice\t\"Note \"\"short\"\"\"\n\
             7:symbol\t\"7:price\t\"\t7:note\n\
             \t9:price\t"
        );
        assert_eq!(custom.calls.load(Ordering::Relaxed), 4);
    }

    /// A snapshot of `rows`, keyed by their position.
    fn snapshot_of(
        rows: impl IntoIterator<Item = [CellValue<'static>; 3]>,
    ) -> Arc<StoreSnapshot<Row>> {
        snapshot_of_rows(
            rows.into_iter()
                .zip(0..)
                .map(|(cells, key)| Row::new(key, cells)),
        )
    }

    /// A snapshot of `rows`.
    fn snapshot_of_rows(rows: impl IntoIterator<Item = Row>) -> Arc<StoreSnapshot<Row>> {
        let epoch = Instant::now();
        let mut ingest = Ingest::new(HighlightPolicy::default(), epoch);
        ingest.apply(
            rows.into_iter().map(|row| Op::UpsertSilent(Arc::new(row))),
            epoch,
        );
        ingest.snapshot()
    }

    /// Twelve trades of three symbols. AAPL is two rows, keys 0 and 3, whose
    /// prices sum to 1234.5 and one of which has a note; the prices of all
    /// sum to 1392, and nine rows have a note.
    fn trades() -> Arc<StoreSnapshot<Row>> {
        const TRADES: [(&str, f64, &str); 12] = [
            ("AAPL", 1000.25, "buy"),
            ("MSFT", 10.0, "x"),
            ("SBER", 1.5, "y"),
            ("AAPL", 234.25, ""),
            ("MSFT", 20.0, "x"),
            ("SBER", 1.5, ""),
            ("MSFT", 30.0, "x"),
            ("SBER", 1.5, "y"),
            ("MSFT", 40.0, ""),
            ("SBER", 1.5, "y"),
            ("MSFT", 50.0, "x"),
            ("SBER", 1.5, "y"),
        ];
        snapshot_of(TRADES.map(|(symbol, price, note)| {
            [
                CellValue::Text(symbol),
                CellValue::Float(price),
                if note.is_empty() {
                    CellValue::Empty
                } else {
                    CellValue::Text(note)
                },
            ]
        }))
    }

    /// `snapshot` grouped by `levels` with `aggregates`, every group with a
    /// footer.
    fn grouped(
        snapshot: &StoreSnapshot<Row>,
        levels: &[Column],
        aggregates: impl IntoIterator<Item = (Column, Aggregate<Row>)>,
    ) -> Projection {
        let spec = ViewSpec::new()
            .with_group_by(levels.iter().map(|&level| GroupBy::column(level)))
            .with_aggregates(aggregates)
            .with_group_footer(EveryGroupFooter);
        Rebuild::default()
            .with_max_threads(1)
            .rebuild(snapshot, &spec)
    }

    /// The top-level group whose key is `value`.
    fn group<'a>(projection: &'a Projection, value: &str) -> GroupInfo<'a> {
        projection
            .groups()
            .iter()
            .find(|group| group.depth() == 0 && group.value() == &OwnedValue::Text(value.into()))
            .expect("the group")
    }

    /// The caption of AAPL, its two rows, its footer and the totals labelled
    /// in the symbol column.
    fn aapl_lines<'a>(
        snapshot: &'a StoreSnapshot<Row>,
        projection: &'a Projection,
    ) -> Vec<ExportRow<'a, Row>> {
        let aapl = group(projection, "AAPL");
        let mut rows = vec![ExportRow::caption(Column::Symbol, aapl)];
        rows.extend(
            aapl.rows()
                .iter()
                .map(|&id| ExportRow::new(&**snapshot.row(id).unwrap()).with_depth(1)),
        );
        rows.push(ExportRow::footer(aapl));
        rows.push(ExportRow::totals(
            projection.totals(),
            Some((Column::Symbol, "Total")),
        ));
        rows
    }

    const SUMMARY_COLUMNS: [ExportColumn<'static, Column>; 3] = [
        ExportColumn::new(Column::Symbol, "Symbol"),
        ExportColumn::new(Column::Price, "Price"),
        ExportColumn::new(Column::Note, "Note"),
    ];

    /// Each column's cell of `row`: its value, and whether it is an
    /// aggregate.
    fn cells_of<'a>(row: &ExportRow<'a, Row>) -> Vec<Option<(CellValue<'a>, bool)>> {
        Column::all()
            .iter()
            .map(|&column| {
                row.cell(column)
                    .map(|cell| (cell.value(), cell.is_aggregate()))
            })
            .collect()
    }

    #[test]
    fn test_export_summary_cells() {
        use CellValue::{Empty, Float, Int, Text};

        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [
                (Column::Symbol, Aggregate::Count),
                (Column::Price, Aggregate::Sum),
                // A sum of text: an empty aggregate.
                (Column::Note, Aggregate::Sum),
            ],
        );
        let aapl = group(&projection, "AAPL");

        // A caption: the key value in its column, over that column's
        // aggregate; the other aggregates in theirs; an empty aggregate is a
        // cell with an empty value.
        let caption = ExportRow::caption(Column::Symbol, aapl);
        assert_eq!(caption.line(), ExportLine::Caption);
        assert!(caption.row().is_none());
        assert_eq!(caption.group().map(|group| group.id()), Some(aapl.id()));
        assert_eq!(caption.depth(), 0);
        assert_eq!(
            cells_of(&caption),
            [
                Some((Text("AAPL"), false)),
                Some((Float(1234.5), true)),
                Some((Empty, true)),
            ]
        );
        // The key value in another column, as when the group's column is
        // hidden: the group's column shows its aggregate.
        assert_eq!(
            cells_of(&ExportRow::caption(Column::Price, aapl)),
            [
                Some((Int(2), true)),
                Some((Text("AAPL"), false)),
                Some((Empty, true)),
            ]
        );
        // `with_columns` narrows a summary row as it does a data row.
        assert_eq!(
            cells_of(&caption.with_columns(columns(&[Column::Price]))),
            [None, Some((Float(1234.5), true)), None]
        );

        // A footer: the group's aggregates, at the group's depth.
        let footer = ExportRow::footer(aapl);
        assert_eq!(footer.line(), ExportLine::Footer);
        assert!(footer.row().is_none());
        assert_eq!(footer.group().map(|group| group.id()), Some(aapl.id()));
        assert_eq!(footer.depth(), 0);
        assert_eq!(
            cells_of(&footer),
            [
                Some((Int(2), true)),
                Some((Float(1234.5), true)),
                Some((Empty, true)),
            ]
        );

        // The totals: an aggregate wins over the label.
        let totals = ExportRow::totals(projection.totals(), Some((Column::Symbol, "Total")));
        assert_eq!(totals.line(), ExportLine::Totals);
        assert!(totals.row().is_none() && totals.group().is_none());
        assert_eq!(
            cells_of(&totals),
            [
                Some((Int(12), true)),
                Some((Float(1392.0), true)),
                Some((Empty, true)),
            ]
        );

        // Columns without an aggregate have no summary cell, except the
        // caption's key value and the totals label.
        let summed = grouped(
            &snapshot,
            &[Column::Symbol],
            [(Column::Price, Aggregate::Sum)],
        );
        let aapl = group(&summed, "AAPL");
        assert_eq!(
            cells_of(&ExportRow::caption(Column::Symbol, aapl)),
            [
                Some((Text("AAPL"), false)),
                Some((Float(1234.5), true)),
                None,
            ]
        );
        assert_eq!(
            cells_of(&ExportRow::footer(aapl)),
            [None, Some((Float(1234.5), true)), None]
        );
        for (label, expected) in [
            (
                Some((Column::Symbol, "Total")),
                Some((Text("Total"), false)),
            ),
            (Some((Column::Price, "Total")), None),
            (None, None),
        ] {
            assert_eq!(
                cells_of(&ExportRow::totals(summed.totals(), label)),
                [expected, Some((Float(1392.0), true)), None],
                "{label:?}"
            );
        }

        // A data row: the application row and its values, at depth 0 until
        // set.
        let trade = &**snapshot.row(aapl.rows()[0]).unwrap();
        let data = ExportRow::new(trade);
        assert_eq!(data.line(), ExportLine::Data);
        assert!(data.row().is_some_and(|row| std::ptr::eq(row, trade)));
        assert!(data.group().is_none());
        assert_eq!(data.depth(), 0);
        assert_eq!(data.with_depth(1).depth(), 1);
        assert_eq!(
            cells_of(&data),
            [
                Some((Text("AAPL"), false)),
                Some((Float(1000.25), false)),
                Some((Text("buy"), false)),
            ]
        );

        // A subgroup's caption and footer are at its depth, and their cells
        // know it.
        let nested = grouped(&snapshot, &[Column::Symbol, Column::Note], []);
        let subgroup = nested
            .groups()
            .iter()
            .find(|group| group.depth() == 1)
            .expect("a subgroup");
        let caption = ExportRow::<Row>::caption(Column::Note, subgroup);
        assert_eq!(caption.depth(), 1);
        assert_eq!(caption.cell(Column::Note).map(|cell| cell.depth()), Some(1));
        assert_eq!(ExportRow::<Row>::footer(subgroup).depth(), 1);
        assert_eq!(caption.with_depth(3).depth(), 3);
    }

    #[test]
    fn test_tsv_with_summaries() {
        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [
                (Column::Price, Aggregate::Sum),
                (Column::Note, Aggregate::Count),
            ],
        );
        let rows = aapl_lines(&snapshot, &projection);
        let table = ExportTable::new(&SUMMARY_COLUMNS, &rows).with_header(true);

        // Fields without a cell are empty; aggregates are raw, or as on
        // screen; the totals label is in the first column.
        let raw = CellExport::default();
        assert_eq!(
            tsv(&table, &raw),
            "Symbol\tPrice\tNote\n\
             AAPL\t1234.50\t1\n\
             AAPL\t1000.25\tbuy\n\
             AAPL\t234.25\t\n\
             \t1234.50\t1\n\
             Total\t1392.00\t9"
        );
        assert_eq!(
            tsv(&table, &CellExport::Displayed(DisplayedCells)),
            "Symbol\tPrice\tNote\n\
             AAPL\t1234.50\t1\n\
             AAPL\t1000.25\tbuy\n\
             AAPL\t234.25\t\n\
             \t1234.50\t1\n\
             Total\t1392.00\t9"
        );
        // The decimal separator is an aggregate's too.
        let comma = CellExport::Raw(RawCells::new().with_decimal_separator(','));
        assert!(tsv(&table, &comma).ends_with("\n\t1234,50\t1\nTotal\t1392,00\t9"));

        // Without a header line, the first line is the caption.
        assert!(
            tsv(&ExportTable::new(&SUMMARY_COLUMNS, &rows), &raw)
                .starts_with("AAPL\t1234.50\t1\nAAPL\t1000.25\tbuy\n")
        );

        // A key value is a value: `Bool` raw as `TRUE`, as on screen "✓";
        // one that needs quotes is quoted.
        let snapshot = snapshot_of([
            [
                CellValue::Text("a\tb"),
                CellValue::Int(1),
                CellValue::Bool(true),
            ],
            [
                CellValue::Text("a\tb"),
                CellValue::Int(2),
                CellValue::Bool(false),
            ],
        ]);
        let by_flag = grouped(&snapshot, &[Column::Note, Column::Symbol], []);
        let captions: Vec<_> = by_flag
            .groups()
            .iter()
            .map(|group| {
                let value_column = if group.depth() == 0 {
                    Column::Note
                } else {
                    Column::Symbol
                };
                ExportRow::caption(value_column, group)
            })
            .collect();
        let table = ExportTable::new(&SUMMARY_COLUMNS, &captions);
        assert_eq!(
            tsv(&table, &raw),
            "\t\tFALSE\n\"a\tb\"\t\t\n\t\tTRUE\n\"a\tb\"\t\t"
        );
        assert_eq!(
            tsv(&table, &CellExport::Displayed(DisplayedCells)),
            "\t\t\n\"a\tb\"\t\t\n\t\t✓\n\"a\tb\"\t\t"
        );
    }

    #[test]
    fn test_line_filter() {
        assert_eq!(ExportLines::default(), ExportLines::ALL);
        for line in ExportLine::ALL {
            assert!(ExportLines::ALL.contains(line));
            assert!(!ExportLines::EMPTY.contains(line));
            assert_eq!(ExportLines::ALL.without(line).with(line), ExportLines::ALL);
            assert!(!ExportLines::ALL.without(line).contains(line));
        }
        assert_eq!(
            format!(
                "{:?}",
                ExportLines::EMPTY
                    .with(ExportLine::Totals)
                    .with(ExportLine::Data)
            ),
            "{Data, Totals}"
        );

        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [
                (Column::Price, Aggregate::Sum),
                (Column::Note, Aggregate::Count),
            ],
        );
        let rows = aapl_lines(&snapshot, &projection);
        let table = ExportTable::new(&SUMMARY_COLUMNS, &rows).with_header(true);
        let raw = CellExport::default();
        let filtered = |lines: ExportLines| {
            let filter = LineFilter::new(TsvExporter, lines);
            assert_eq!(filter.lines(), lines);
            let mut out = String::new();
            Exporter::export(&filter, &table, &raw, &mut out);
            out
        };

        // Every line: the table as it is.
        assert_eq!(filtered(ExportLines::ALL), tsv(&table, &raw));
        // Without the footers and the totals; the application's rows alone;
        // the summaries alone; nothing but the header line.
        assert_eq!(
            filtered(
                ExportLines::ALL
                    .without(ExportLine::Footer)
                    .without(ExportLine::Totals)
            ),
            "Symbol\tPrice\tNote\nAAPL\t1234.50\t1\nAAPL\t1000.25\tbuy\nAAPL\t234.25\t"
        );
        assert_eq!(
            filtered(ExportLines::EMPTY.with(ExportLine::Data)),
            "Symbol\tPrice\tNote\nAAPL\t1000.25\tbuy\nAAPL\t234.25\t"
        );
        assert_eq!(
            filtered(ExportLines::ALL.without(ExportLine::Data)),
            "Symbol\tPrice\tNote\nAAPL\t1234.50\t1\n\t1234.50\t1\nTotal\t1392.00\t9"
        );
        assert_eq!(filtered(ExportLines::EMPTY), "Symbol\tPrice\tNote");

        // The wrapped exporter gets the table's columns and header line.
        struct Shape;

        impl Exporter<Row> for Shape {
            fn export(&self, table: &ExportTable<'_, Row>, _: &CellExport<Row>, out: &mut String) {
                let lines: Vec<_> = table.rows().iter().map(ExportRow::line).collect();
                let _ = write!(
                    out,
                    "{} {} {lines:?}",
                    table.columns().len(),
                    table.has_header()
                );
            }
        }

        let filter = LineFilter::new(Shape, ExportLines::EMPTY.with(ExportLine::Caption));
        let mut out = String::new();
        Exporter::export(&filter, &table, &raw, &mut out);
        assert_eq!(out, "3 true [Caption]");
        let _: &Shape = filter.exporter();
    }

    #[test]
    fn test_export_cell_row_is_optional() {
        /// Records each cell's row key, line, depth, whether it is an
        /// aggregate and its group; writes raw values with the row count
        /// after a caption's key value.
        #[derive(Default)]
        struct Seen(Mutex<Vec<(Option<u32>, ExportLine, u16, bool, Option<GroupId>)>>);

        impl CellExporter<Row> for Seen {
            fn write_cell(&self, cell: &ExportCell<'_, Row>, out: &mut String) {
                self.0.lock().unwrap().push((
                    cell.row().map(|row| row.key),
                    cell.line(),
                    cell.depth(),
                    cell.is_aggregate(),
                    cell.group().map(|group| group.id()),
                ));
                RawCells::new().write_cell(cell, out);
                if cell.line() == ExportLine::Caption
                    && !cell.is_aggregate()
                    && let Some(group) = cell.group()
                {
                    let _ = write!(out, " ({})", group.row_count());
                }
            }
        }

        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [(Column::Price, Aggregate::Sum)],
        );
        let rows = aapl_lines(&snapshot, &projection);
        let aapl = group(&projection, "AAPL").id();
        let seen = Arc::new(Seen::default());
        let table = ExportTable::new(&SUMMARY_COLUMNS, &rows);
        assert_eq!(
            tsv(&table, &CellExport::Custom(seen.clone())),
            "AAPL (2)\t1234.50\t\n\
             AAPL\t1000.25\tbuy\n\
             AAPL\t234.25\t\n\
             \t1234.50\t\n\
             Total\t1392.00\t"
        );
        use ExportLine::{Caption, Data, Footer, Totals};
        assert_eq!(
            *seen.0.lock().unwrap(),
            [
                (None, Caption, 0, false, Some(aapl)),
                (None, Caption, 0, true, Some(aapl)),
                (Some(0), Data, 1, false, None),
                (Some(0), Data, 1, false, None),
                (Some(0), Data, 1, false, None),
                (Some(3), Data, 1, false, None),
                (Some(3), Data, 1, false, None),
                (Some(3), Data, 1, false, None),
                (None, Footer, 0, true, Some(aapl)),
                (None, Totals, 0, false, None),
                (None, Totals, 0, true, None),
            ]
        );
    }

    #[test]
    fn test_export_scope_variants() {
        // Exhaustive matches: a new scope or line fails to build here, in
        // the crate, where `#[non_exhaustive]` does not apply.
        fn scope_name(scope: ExportScope) -> &'static str {
            match scope {
                ExportScope::Visible => "Visible",
                ExportScope::Selection => "Selection",
                ExportScope::Expanded => "Expanded",
            }
        }
        fn line_ix(line: ExportLine) -> usize {
            match line {
                ExportLine::Data => 0,
                ExportLine::Caption => 1,
                ExportLine::Footer => 2,
                ExportLine::Totals => 3,
            }
        }

        let scopes = [
            ExportScope::Visible,
            ExportScope::Selection,
            ExportScope::Expanded,
        ];
        for (ix, scope) in scopes.into_iter().enumerate() {
            assert_eq!(format!("{scope:?}"), scope_name(scope));
            for (other_ix, other) in scopes.into_iter().enumerate() {
                assert_eq!(scope == other, ix == other_ix, "{scope:?} {other:?}");
            }
        }
        // `ExportLines::ALL` lists every line.
        for (ix, line) in ExportLine::ALL.into_iter().enumerate() {
            assert_eq!(line_ix(line), ix);
        }
    }

    #[cfg(feature = "decimal")]
    #[test]
    fn test_decimal_group_value() {
        let price = rust_decimal::Decimal::new(1230, 2);
        let snapshot = snapshot_of([[
            CellValue::Text("AAPL"),
            CellValue::Decimal(price),
            CellValue::Empty,
        ]]);
        let projection = grouped(
            &snapshot,
            &[Column::Price],
            [(Column::Symbol, Aggregate::Count)],
        );
        let caption = [ExportRow::caption(
            Column::Price,
            projection.groups().iter().next().expect("a group"),
        )];
        let table = ExportTable::new(&SUMMARY_COLUMNS, &caption);
        assert_eq!(tsv(&table, &CellExport::default()), "1\t12.30\t");
        let comma = CellExport::Raw(RawCells::new().with_decimal_separator(','));
        assert_eq!(tsv(&table, &comma), "1\t12,30\t");
        assert_eq!(
            tsv(&table, &CellExport::Displayed(DisplayedCells)),
            "1\t12.30\t"
        );
    }

    /// The bytes `format` writes for `table`; its progress reaches the
    /// table's rows.
    fn file(
        format: &dyn ExportFormat<Row>,
        table: &ExportTable<'_, Row>,
        cells: &CellExport<Row>,
    ) -> Vec<u8> {
        let control = ExportControl::new(table.rows().len());
        let mut out = Vec::new();
        format
            .export(table, cells, &mut out, &control)
            .expect("the export");
        assert_eq!(control.rows_written(), control.rows_total());
        out
    }

    /// The text `format` writes for `table`.
    fn file_text(
        format: &dyn ExportFormat<Row>,
        table: &ExportTable<'_, Row>,
        cells: &CellExport<Row>,
    ) -> String {
        String::from_utf8(file(format, table, cells)).expect("UTF-8")
    }

    #[test]
    fn test_csv_rfc4180() {
        use CellValue::{Bool, Empty, Float, Int, Text};

        let rows = [
            Row::new(1, [Text("a,b"), Float(1234.5), Text("say \"hi\"")]),
            Row::new(2, [Text("two\nlines"), Bool(true), Text("cr\rhere")]),
            Row::new(3, [Text(" padded "), Empty, Text("semi;colon")]),
            Row::new(4, [Text("plain"), Int(-3), Bool(false)]),
        ];
        let all: Vec<_> = rows.iter().map(ExportRow::new).collect();
        let table = ExportTable::new(&COLUMNS, &all).with_header(true);
        let raw = CellExport::default();

        let format = CsvFormat::new();
        assert_eq!(format, CsvFormat::default());
        assert_eq!(format.separator(), ',');
        assert_eq!(format.encoding(), CsvEncoding::Utf8WithBom);
        assert_eq!(CsvEncoding::default(), CsvEncoding::Utf8WithBom);
        assert_eq!(ExportFormat::<Row>::extension(&format), "csv");

        // A byte order mark, then quotes where a field holds the separator, a
        // quote, CR or LF; spaces, empty fields and raw values without; CRLF
        // after every line, the last included.
        let expected = "Symbol,Price,\"Note \"\"short\"\"\"\r\n\
                        \"a,b\",1234.50,\"say \"\"hi\"\"\"\r\n\
                        \"two\nlines\",TRUE,\"cr\rhere\"\r\n\
                        \x20padded ,,semi;colon\r\n\
                        plain,-3,FALSE\r\n";
        let with_mark = file(&format, &table, &raw);
        assert_eq!(with_mark[..3], [0xEF, 0xBB, 0xBF]);
        assert_eq!(with_mark[3..], *expected.as_bytes());
        let utf8 = format.with_encoding(CsvEncoding::Utf8);
        assert_eq!(utf8.encoding(), CsvEncoding::Utf8);
        assert_eq!(file_text(&utf8, &table, &raw), expected);

        // `;` with a decimal comma: `1234,50` needs no quotes, `semi;colon`
        // does, and `a,b` no longer.
        let semicolon = utf8.with_separator(';');
        assert_eq!(semicolon.separator(), ';');
        let comma = CellExport::Raw(RawCells::new().with_decimal_separator(','));
        assert_eq!(
            file_text(&semicolon, &table, &comma),
            "Symbol;Price;\"Note \"\"short\"\"\"\r\n\
             a,b;1234,50;\"say \"\"hi\"\"\"\r\n\
             \"two\nlines\";TRUE;\"cr\rhere\"\r\n\
             \x20padded ;;\"semi;colon\"\r\n\
             plain;-3;FALSE\r\n"
        );

        // Cells as the screen shows them; a cell the row does not export is
        // an empty field.
        let narrowed = [
            ExportRow::new(&rows[0]).with_columns(columns(&[Column::Price])),
            ExportRow::new(&rows[3]),
        ];
        assert_eq!(
            file_text(
                &utf8,
                &ExportTable::new(&COLUMNS, &narrowed),
                &CellExport::Displayed(DisplayedCells)
            ),
            ",1234.50,\r\nplain,-3,\r\n"
        );

        // The header line alone; no line at all, only the mark.
        let header_only = ExportTable::new(&COLUMNS[..2], &[]).with_header(true);
        assert_eq!(file_text(&utf8, &header_only, &raw), "Symbol,Price\r\n");
        assert_eq!(
            file(&format, &ExportTable::new(&COLUMNS, &[]), &raw),
            [0xEF, 0xBB, 0xBF]
        );

        // A separator beyond ASCII quotes as any other.
        let mut field = String::from("x¦a¦b");
        quote_field(&mut field, 3, '¦', &mut String::new());
        assert_eq!(field, "x¦\"a¦b\"");
        let mut field = String::from("a|b");
        quote_field(&mut field, 0, '¦', &mut String::new());
        assert_eq!(field, "a|b");
    }

    #[test]
    #[should_panic(expected = "a CSV separator cannot be a quote or a line break")]
    fn test_csv_separator_is_not_a_quote() {
        let _ = CsvFormat::new().with_separator('"');
    }

    #[test]
    fn test_tsv_file_matches_clipboard() {
        use CellValue::{Bool, Empty, Float, Int, Text};

        let rows = [
            Row::new(1, [Text("AAPL"), Float(187.25), Text("tab\there")]),
            Row::new(2, [Text("say \"hi\""), Int(-3), Text("two\nlines\r")]),
            Row::new(3, [Empty, Bool(true), Empty]),
        ];
        let all: Vec<_> = rows.iter().map(ExportRow::new).collect();
        let rectangle = [
            ExportRow::new(&rows[0]).with_columns(columns(&[Column::Symbol])),
            ExportRow::new(&rows[1]).with_columns(columns(&[Column::Note])),
        ];
        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [
                (Column::Price, Aggregate::Sum),
                (Column::Note, Aggregate::Count),
            ],
        );
        let summaries = aapl_lines(&snapshot, &projection);
        let tables = [
            ExportTable::new(&COLUMNS, &all).with_header(true),
            ExportTable::new(&COLUMNS, &all),
            ExportTable::new(&COLUMNS, &rectangle),
            ExportTable::new(&SUMMARY_COLUMNS, &summaries).with_header(true),
            ExportTable::new(&SUMMARY_COLUMNS, &summaries),
            ExportTable::new(&COLUMNS[..2], &[]).with_header(true),
            ExportTable::new(&COLUMNS, &[]),
        ];
        assert_eq!(ExportFormat::<Row>::extension(&TsvExporter), "tsv");
        for cells in [
            CellExport::default(),
            CellExport::Displayed(DisplayedCells),
            CellExport::Raw(RawCells::new().with_decimal_separator(',')),
        ] {
            for table in &tables {
                // The clipboard's text as UTF-8: no mark, `\n` between lines.
                assert_eq!(
                    file(&TsvExporter, table, &cells),
                    tsv(table, &cells).as_bytes()
                );
            }
        }
        assert!(file(&TsvExporter, &tables[5], &CellExport::default()) == b"Symbol\tPrice");
    }

    /// A sink that records the rows its control counted at each write.
    struct ProgressSink {
        control: ExportControl,
        bytes: Vec<u8>,
        writes: Vec<usize>,
    }

    impl ProgressSink {
        fn new(control: &ExportControl) -> Self {
            Self {
                control: control.clone(),
                bytes: Vec::new(),
                writes: Vec::new(),
            }
        }
    }

    impl io::Write for ProgressSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes.push(self.control.rows_written());
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_formats_report_progress_and_stop() {
        // A control: clones share the progress and the cancellation.
        let control = ExportControl::new(10);
        assert_eq!((control.rows_total(), control.rows_written()), (10, 0));
        assert!(control.advance(4).is_ok());
        let shared = control.clone();
        assert_eq!(shared.rows_written(), 4);
        assert!(!control.is_cancelled());
        shared.cancel();
        assert!(control.is_cancelled());
        assert!(matches!(control.advance(0), Err(ExportError::Cancelled)));

        let rows: Vec<Row> = (0..3000)
            .map(|key| {
                Row::new(
                    key,
                    [
                        CellValue::Int(key.into()),
                        CellValue::Float(f64::from(key) / 4.0),
                        CellValue::Text("x"),
                    ],
                )
            })
            .collect();
        let lines: Vec<_> = rows.iter().map(ExportRow::new).collect();
        let table = ExportTable::new(&COLUMNS, &lines).with_header(true);
        let raw = CellExport::default();

        // (format, line break, line breaks with the header and 1024 rows)
        let csv = CsvFormat::new();
        let formats: [(&dyn ExportFormat<Row>, &str, usize); 2] =
            [(&csv, "\r\n", 1025), (&TsvExporter, "\n", 1024)];
        for (format, line_break, first_buffer_breaks) in formats {
            let name = format.extension();
            let control = ExportControl::new(table.rows().len());
            let mut sink = ProgressSink::new(&control);
            format
                .export(&table, &raw, &mut sink, &control)
                .expect("the export");
            // A write every 1024 rows, each counted before the next.
            assert_eq!(sink.writes, [0, 1024, 2048], "{name}");
            assert_eq!(control.rows_written(), control.rows_total(), "{name}");
            let whole = sink.bytes;
            assert_eq!(whole, file(format, &table, &raw), "{name}");

            // Cancelled before it starts: one buffer written, then
            // `Cancelled`.
            let control = ExportControl::new(table.rows().len());
            control.cancel();
            let mut sink = ProgressSink::new(&control);
            assert!(
                matches!(
                    format.export(&table, &raw, &mut sink, &control),
                    Err(ExportError::Cancelled)
                ),
                "{name}"
            );
            assert_eq!(sink.writes, [0], "{name}");
            assert!(whole.starts_with(&sink.bytes), "{name}");
            let text = String::from_utf8(sink.bytes).expect("UTF-8");
            assert_eq!(
                text.matches(line_break).count(),
                first_buffer_breaks,
                "{name}"
            );
        }

        // A text file is written whole: progress jumps from 0 to done, and a
        // cancelled one writes nothing.
        let text_file = TextFile::new(TsvExporter, "tsv");
        let control = ExportControl::new(table.rows().len());
        let mut sink = ProgressSink::new(&control);
        text_file
            .export(&table, &raw, &mut sink, &control)
            .expect("the export");
        assert_eq!(sink.writes, [0]);
        assert_eq!(control.rows_written(), 3000);
        let control = ExportControl::new(table.rows().len());
        control.cancel();
        let mut sink = ProgressSink::new(&control);
        assert!(matches!(
            text_file.export(&table, &raw, &mut sink, &control),
            Err(ExportError::Cancelled)
        ));
        assert!(sink.bytes.is_empty());

        // A failing sink: the error comes back as `Io`.
        struct Full;

        impl io::Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let control = ExportControl::new(table.rows().len());
        let error = CsvFormat::new()
            .export(&table, &raw, &mut Full, &control)
            .expect_err("a full disk");
        assert!(
            matches!(&error, ExportError::Io(error) if error.kind() == io::ErrorKind::StorageFull)
        );
        assert_eq!(control.rows_written(), 0);
    }

    #[test]
    fn test_text_file_and_line_filter() {
        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [
                (Column::Price, Aggregate::Sum),
                (Column::Note, Aggregate::Count),
            ],
        );
        let rows = aapl_lines(&snapshot, &projection);
        let table = ExportTable::new(&SUMMARY_COLUMNS, &rows).with_header(true);
        let raw = CellExport::default();

        // Any clipboard exporter, saved under its extension.
        let text_file = TextFile::new(TsvExporter, "tsv");
        let _: &TsvExporter = text_file.exporter();
        assert_eq!(ExportFormat::<Row>::extension(&text_file), "tsv");
        assert_eq!(file(&text_file, &table, &raw), tsv(&table, &raw).as_bytes());
        let data_only = TextFile::new(
            LineFilter::new(TsvExporter, ExportLines::EMPTY.with(ExportLine::Data)),
            String::from("txt"),
        );
        assert_eq!(ExportFormat::<Row>::extension(&data_only), "txt");
        assert_eq!(
            file_text(&data_only, &table, &raw),
            "Symbol\tPrice\tNote\nAAPL\t1000.25\tbuy\nAAPL\t234.25\t"
        );

        // A filter over a file format keeps its extension, and its progress
        // counts the rows left out (`file` checks it).
        let utf8 = CsvFormat::new().with_encoding(CsvEncoding::Utf8);
        let without_footers = LineFilter::new(utf8, ExportLines::ALL.without(ExportLine::Footer));
        assert_eq!(ExportFormat::<Row>::extension(&without_footers), "csv");
        assert_eq!(
            file_text(&without_footers, &table, &raw),
            "Symbol,Price,Note\r\n\
             AAPL,1234.50,1\r\n\
             AAPL,1000.25,buy\r\n\
             AAPL,234.25,\r\n\
             Total,1392.00,9\r\n"
        );
        // Every line: the table as it is; no line: the header line alone.
        assert_eq!(
            file(&LineFilter::new(utf8, ExportLines::ALL), &table, &raw),
            file(&utf8, &table, &raw)
        );
        assert_eq!(
            file_text(&LineFilter::new(utf8, ExportLines::EMPTY), &table, &raw),
            "Symbol,Price,Note\r\n"
        );
    }

    #[test]
    fn test_export_metadata() {
        use crate::engine::TreeSpec;

        // A column without metadata.
        let price = ExportColumn::new(Column::Price, "Price");
        assert_eq!(price.width(), None);
        assert_eq!(price.align(), ColumnAlign::Auto);
        assert_eq!(price.zone(), ColumnZone::Scrolling);
        assert!(price.bands().is_empty());

        // And with it.
        let bands = ["Market", "Quote"];
        let price = price
            .with_width(140.0)
            .with_align(ColumnAlign::End)
            .with_zone(ColumnZone::FrozenLeft)
            .with_bands(&bands);
        assert_eq!(price.width(), Some(140.0));
        assert_eq!(price.align(), ColumnAlign::End);
        assert_eq!(price.zone(), ColumnZone::FrozenLeft);
        assert_eq!(price.bands(), ["Market", "Quote"]);
        assert_eq!((price.column(), price.title()), (Column::Price, "Price"));
        assert_ne!(price, ExportColumn::new(Column::Price, "Price"));

        // Only the leading `FrozenLeft` columns freeze; the deepest band
        // sets the header rows over the titles.
        let symbol = ExportColumn::new(Column::Symbol, "Symbol").with_zone(ColumnZone::FrozenLeft);
        let note = ExportColumn::new(Column::Note, "Note").with_bands(&bands[..1]);
        let metadata = [symbol, price, note, symbol];
        let table = ExportTable::<Row>::new(&metadata, &[]);
        assert_eq!(table.frozen_columns(), 2);
        assert_eq!(table.band_depth(), 2);
        let right = note.with_zone(ColumnZone::FrozenRight);
        for (columns, frozen, depth) in [
            (&[note, symbol][..], 0, 1),
            (&[symbol, right][..], 1, 1),
            (&COLUMNS[..], 0, 0),
            (&[][..], 0, 0),
        ] {
            let table = ExportTable::<Row>::new(columns, &[]);
            assert_eq!(table.frozen_columns(), frozen, "{columns:?}");
            assert_eq!(table.band_depth(), depth, "{columns:?}");
        }

        // Rows carry no marks until set.
        let snapshot = snapshot_of_rows([
            Row::new(0, [CellValue::Text("Book"); 3]),
            Row::new(1, [CellValue::Text("Desk"); 3]).with_parent(0),
            Row::new(2, [CellValue::Text("Trader"); 3]).with_parent(1),
            Row::new(3, [CellValue::Text("Trader"); 3]).with_parent(1),
        ]);
        let tree = Rebuild::default().with_max_threads(1).rebuild(
            &snapshot,
            &ViewSpec::new().with_tree(TreeSpec::new().with_column(Column::Symbol)),
        );
        let row = |key: u32| &**snapshot.get(&key).expect("the row");
        let branch = |key: u32| tree.branch(snapshot.row_id(&key).expect("the row"));
        let desk = branch(1).expect("a branch");
        assert_eq!(desk.child_count(), 2);
        assert_eq!(branch(2), None);

        let data = ExportRow::new(row(1));
        assert_eq!(data.band(), None);
        assert!(!data.is_hidden());
        assert_eq!(data.branch(), None);
        let marked = data
            .with_band(PinnedEdge::Top)
            .with_hidden(true)
            .with_branch(desk);
        assert_eq!(marked.band(), Some(PinnedEdge::Top));
        assert!(marked.is_hidden());
        assert_eq!(marked.branch(), Some(desk));
        assert!(!marked.with_hidden(false).is_hidden());
        // Other builders keep the marks.
        let pending = [(Column::Note, OwnedValue::Int(1))];
        let rebuilt = marked
            .with_depth(1)
            .with_columns(columns(&[Column::Note]))
            .with_pending(&pending);
        assert_eq!(
            (rebuilt.band(), rebuilt.is_hidden(), rebuilt.branch()),
            (Some(PinnedEdge::Top), true, Some(desk))
        );
        assert!(
            rebuilt
                .cell(Column::Note)
                .is_some_and(|cell| cell.is_pending())
        );

        // A summary row takes a band and the hidden mark, but no branch.
        let trades = trades();
        let projection = grouped(
            &trades,
            &[Column::Symbol],
            [(Column::Price, Aggregate::Sum)],
        );
        let totals = ExportRow::<Row>::totals(projection.totals(), None)
            .with_band(PinnedEdge::Bottom)
            .with_branch(desk);
        assert_eq!(
            (totals.band(), totals.branch()),
            (Some(PinnedEdge::Bottom), None)
        );
        let footer = ExportRow::<Row>::footer(group(&projection, "AAPL")).with_hidden(true);
        assert!(footer.is_hidden() && footer.band().is_none());

        // The marks fit in the row: a data row's extras in its variant, the
        // number formats in one shared slice.
        #[cfg(target_pointer_width = "64")]
        assert!(
            size_of::<ExportRow<'_, Row>>() <= 128,
            "{} bytes",
            size_of::<ExportRow<'_, Row>>()
        );
    }

    #[test]
    fn test_number_hint() {
        use CellValue::{Bool, Empty, Float, Int, Text, Timestamp};

        let digits = |value: CellValue<'static>| {
            let row = Row::new(0, [value, Empty, Empty]);
            ExportRow::new(&row)
                .cell(Column::Symbol)
                .unwrap()
                .number_hint()
                .map(|hint| hint.fraction_digits())
        };
        assert_eq!(digits(Float(1.0)), Some(2));
        assert_eq!(digits(Float(-0.125)), Some(2));
        assert_eq!(digits(Int(5)), Some(0));
        for value in [
            Text("1.5"),
            Bool(true),
            Empty,
            Timestamp(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        ] {
            assert_eq!(digits(value), None, "{value:?}");
        }
        #[cfg(feature = "decimal")]
        {
            use rust_decimal::Decimal;

            assert_eq!(digits(CellValue::Decimal(Decimal::new(1230, 2))), Some(2));
            assert_eq!(digits(CellValue::Decimal(Decimal::new(5, 0))), Some(0));
        }

        // Summary cells by the aggregate's value: a `Float` sum and an `Int`
        // count; a caption's key text has none.
        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [
                (Column::Price, Aggregate::Sum),
                (Column::Note, Aggregate::Count),
            ],
        );
        let aapl = group(&projection, "AAPL");
        let hint = |row: ExportRow<'_, Row>, column| row.cell(column).unwrap().number_hint();
        let two = hint(ExportRow::footer(aapl), Column::Price).expect("a hint");
        assert_eq!(two.fraction_digits(), 2);
        assert_eq!(two, two.clone());
        assert_eq!(
            hint(ExportRow::footer(aapl), Column::Note).map(|hint| hint.fraction_digits()),
            Some(0)
        );
        assert_eq!(
            hint(ExportRow::caption(Column::Symbol, aapl), Column::Symbol),
            None
        );
        assert_eq!(
            hint(ExportRow::totals(projection.totals(), None), Column::Price),
            Some(two)
        );
    }

    #[test]
    fn test_pending_values() {
        use CellValue::{Float, Int, Text};

        let rows = [Row::new(1, [Text("AAPL"), Float(187.25), Text("buy")])];
        let pending = [
            (Column::Price, OwnedValue::Float(2.5)),
            (Column::Note, OwnedValue::Int(7)),
        ];
        let row = ExportRow::new(&rows[0]).with_pending(&pending);

        // A pending value replaces the row's value; the row's stays readable.
        let price = row.cell(Column::Price).unwrap();
        assert_eq!(price.value(), Float(2.5));
        assert!(price.is_pending());
        assert_eq!(price.row_value(), Some(Float(187.25)));
        // The hint follows the value shown.
        let note = row.cell(Column::Note).unwrap();
        assert_eq!(
            (note.value(), note.row_value()),
            (Int(7), Some(Text("buy")))
        );
        assert_eq!(
            note.number_hint().map(|hint| hint.fraction_digits()),
            Some(0)
        );
        // Other columns are as they were.
        let symbol = row.cell(Column::Symbol).unwrap();
        assert!(!symbol.is_pending());
        assert_eq!(
            (symbol.value(), symbol.row_value()),
            (Text("AAPL"), Some(Text("AAPL")))
        );
        // Without pending values, nothing is.
        let plain = ExportRow::new(&rows[0]).cell(Column::Price).unwrap();
        assert!(!plain.is_pending());
        assert_eq!(plain.row_value(), Some(Float(187.25)));
        // Narrowing the columns keeps them.
        assert!(
            row.with_columns(columns(&[Column::Price]))
                .cell(Column::Price)
                .is_some_and(|cell| cell.is_pending())
        );

        // A text format writes what the screen shows; a cell exporter may
        // write the stored value instead.
        struct Stored;

        impl CellExporter<Row> for Stored {
            fn write_cell(&self, cell: &ExportCell<'_, Row>, out: &mut String) {
                RawCells::new().write_value(
                    cell.row_value().unwrap_or(cell.value()),
                    cell.number_format(),
                    out,
                );
            }
        }

        let table = ExportTable::new(&COLUMNS, std::slice::from_ref(&row));
        assert_eq!(tsv(&table, &CellExport::default()), "AAPL\t2.50\t7");
        assert_eq!(
            file_text(
                &CsvFormat::new().with_encoding(CsvEncoding::Utf8),
                &table,
                &CellExport::default()
            ),
            "AAPL,2.50,7\r\n"
        );
        assert_eq!(
            tsv(&table, &CellExport::Custom(Arc::new(Stored))),
            "AAPL\t187.25\tbuy"
        );

        // A summary row has no pending values and no row value.
        let snapshot = trades();
        let projection = grouped(
            &snapshot,
            &[Column::Symbol],
            [(Column::Price, Aggregate::Sum)],
        );
        let footer = ExportRow::<Row>::footer(group(&projection, "AAPL")).with_pending(&pending);
        let sum = footer.cell(Column::Price).unwrap();
        assert_eq!(sum.value(), Float(1234.5));
        assert!(!sum.is_pending());
        assert_eq!(sum.row_value(), None);
    }

    #[test]
    fn test_export_error() {
        use std::error::Error as _;

        fn assert_error<T: std::error::Error + Clone + Send + Sync + 'static>() {}
        assert_error::<ExportError>();

        assert_eq!(ExportError::Nothing.to_string(), "nothing to export");
        assert_eq!(
            ExportError::Cancelled.to_string(),
            "the export was cancelled"
        );
        assert!(ExportError::Cancelled.source().is_none());

        // An I/O error converts with `?`, shows its own text and clones by
        // sharing it.
        fn write_to_missing_directory() -> Result<(), ExportError> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such directory"))?
        }
        let error = write_to_missing_directory().expect_err("an error");
        assert_eq!(error.to_string(), "no such directory");
        let ExportError::Io(io_error) = &error else {
            panic!("{error:?}");
        };
        assert_eq!(io_error.kind(), io::ErrorKind::NotFound);
        let ExportError::Io(cloned) = error.clone() else {
            panic!("a clone of {error:?}");
        };
        assert!(Arc::ptr_eq(io_error, &cloned));

        // An application error, from a message or an error value with a
        // source of its own.
        let message = ExportError::other("the workbook is too large");
        assert!(matches!(message, ExportError::Other(_)));
        assert_eq!(message.to_string(), "the workbook is too large");
        assert!(message.source().is_none());

        #[derive(Debug)]
        struct Library(io::Error);

        impl fmt::Display for Library {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("the library failed")
            }
        }

        impl std::error::Error for Library {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let library = ExportError::other(Library(io::Error::other("zip")));
        assert_eq!(library.to_string(), "the library failed");
        assert_eq!(
            library.source().map(ToString::to_string).as_deref(),
            Some("zip")
        );
        assert_eq!(library.clone().to_string(), "the library failed");
    }
}
