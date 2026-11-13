//! Headless UI tests of exporting (`.agents/COMPONENT_TEST_RULES.md`): an
//! export writes the view taken when its command ran to a sink, a file or the
//! application's closure in a task runner job; it marks band rows, rows the
//! screen hides and tree branches, carries the layout's column metadata and the
//! optimistic values of waiting edits; it reports progress, stops when
//! cancelled or dropped, and writes nothing when there is nothing to export. A
//! file appears whole or not at all.
//!
//! Jobs run on the test dispatcher, or on a thread of their own where a test
//! steps through a job while the grid goes on ([`ThreadRunner`], [`Gate`]).

mod common;

use std::{
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use gpui_kit::{App, AppContext as _, Task, TestAppContext, px};
use gpui_kit_grid::{
    Aggregate, CellExport, CellValue, ColumnAlign, ColumnBand, ColumnZone, CsvEncoding, CsvFormat,
    DisplayedCells, EmptyExport, EveryGroupFooter, ExpandTarget, ExportControl, ExportError,
    ExportFormat, ExportHeader, ExportJob, ExportLine, ExportOutcome, ExportRequest, ExportRow,
    ExportScope, ExportTable, GridColumn, GridColumns as _, GridRow, GridSchema, GroupBy,
    LiveGridDelegate, LiveGridState, OwnedValue, PinnedEdge, SelectionMode, SortDirection,
    TaskRunner, TreeSpec, ViewSpec,
};

use crate::common::{
    GroupedColumn, GroupedRow, Item, ItemColumn, OrderColumn, Orders, group_path, grouped_rows,
    open, open_configured, open_grouped, open_orders, open_orders_in, orders, unthrottled,
    with_order_editors,
};

/// CSV without a byte order mark, for texts to compare.
const CSV: CsvFormat = CsvFormat::new().with_encoding(CsvEncoding::Utf8);

/// Lets the job run on the test dispatcher, then takes its result.
fn finish<T>(cx: &mut TestAppContext, job: ExportJob<T>) -> Result<ExportOutcome<T>, ExportError> {
    cx.run_until_parked();
    assert!(job.is_finished(), "the job ran");
    smol::block_on(job)
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8")
}

/// Runs jobs inline, or the next one on a thread of its own: an export job
/// a test steps through ([`Gate`]) while the grid goes on. Or holds the
/// next one back until the test releases it.
#[derive(Clone, Default)]
struct ThreadRunner {
    next_on_thread: Arc<AtomicBool>,
    hold_next: Arc<AtomicBool>,
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    held: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>,
}

impl ThreadRunner {
    /// Runs the next job on a thread of its own.
    fn next_on_thread(&self) {
        self.next_on_thread.store(true, Ordering::SeqCst);
    }

    /// Holds the next job back until [`Self::release`].
    fn hold_next(&self) {
        self.hold_next.store(true, Ordering::SeqCst);
    }

    /// Runs the jobs held back.
    fn release(&self) {
        let held = std::mem::take(&mut *self.held.lock().expect("not poisoned"));
        for job in held {
            job();
        }
    }

    /// Waits for the jobs on threads to end.
    fn join(&self) {
        let threads = std::mem::take(&mut *self.threads.lock().expect("not poisoned"));
        for thread in threads {
            thread.join().expect("the job did not panic");
        }
    }
}

impl TaskRunner for ThreadRunner {
    fn run(&self, job: Box<dyn FnOnce() + Send>, _: &App) -> Task<()> {
        if self.next_on_thread.swap(false, Ordering::SeqCst) {
            let thread = std::thread::spawn(job);
            self.threads.lock().expect("not poisoned").push(thread);
        } else if self.hold_next.swap(false, Ordering::SeqCst) {
            self.held.lock().expect("not poisoned").push(job);
        } else {
            job();
        }
        Task::ready(())
    }
}

/// Steps a job on a thread through numbered points: the job stops at each
/// until the test opens it.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<GateState>, Condvar)>);

#[derive(Default)]
struct GateState {
    /// The points reached.
    reached: usize,
    /// The points opened.
    opened: usize,
}

const GATE_TIMEOUT: Duration = Duration::from_secs(10);

impl Gate {
    /// In the job: reaches point `point` and waits until it is opened.
    fn pass(&self, point: usize) {
        let (state, changed) = &*self.0;
        let mut state = state.lock().expect("not poisoned");
        state.reached = point + 1;
        changed.notify_all();
        let (state, waited) = changed
            .wait_timeout_while(state, GATE_TIMEOUT, |state| state.opened <= point)
            .expect("not poisoned");
        drop(state);
        assert!(!waited.timed_out(), "point {point} was never opened");
    }

    /// In the test: waits until the job reaches point `point`.
    fn wait_for(&self, point: usize) {
        let (state, changed) = &*self.0;
        let (state, waited) = changed
            .wait_timeout_while(state.lock().expect("not poisoned"), GATE_TIMEOUT, |state| {
                state.reached <= point
            })
            .expect("not poisoned");
        drop(state);
        assert!(!waited.timed_out(), "point {point} was never reached");
    }

    /// In the test: lets the job past point `point`.
    fn open(&self, point: usize) {
        let (state, changed) = &*self.0;
        let mut state = state.lock().expect("not poisoned");
        state.opened = state.opened.max(point + 1);
        changed.notify_all();
    }
}

/// A sink the test keeps a handle to.
#[derive(Clone, Default)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl SharedSink {
    fn is_empty(&self) -> bool {
        self.0.lock().expect("not poisoned").is_empty()
    }
}

impl io::Write for SharedSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("not poisoned")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A directory of the test's files, removed at the end.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gpui-kit-grid-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a temporary directory");
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The names of the files in the directory, sorted.
    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .expect("the directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A line of a table: its kind, depth, key or group value, and marks.
fn shape<R: GridRow<Key = u32>>(row: &ExportRow<'_, R>) -> String {
    let mut shape = match row.line() {
        ExportLine::Data => format!("Data@{} {}", row.depth(), row.row().expect("a row").key()),
        ExportLine::Caption | ExportLine::Footer => {
            let group = row.group().expect("a group");
            let value = match group.value() {
                OwnedValue::Text(text) => text.to_string(),
                other => format!("{other:?}"),
            };
            let collapsed = if group.is_expanded() {
                ""
            } else {
                " collapsed"
            };
            format!("{:?}@{} {value}{collapsed}", row.line(), row.depth())
        }
        line => format!("{line:?}@{}", row.depth()),
    };
    if let Some(branch) = row.branch() {
        let state = if branch.is_expanded() {
            "expanded"
        } else {
            "collapsed"
        };
        let _ = write!(shape, " branch {} {state}", branch.child_count());
    }
    if let Some(edge) = row.band() {
        let _ = write!(shape, " band {edge:?}");
    }
    if row.is_hidden() {
        shape.push_str(" hidden");
    }
    shape
}

/// Exports `scope` with a closure that records the shape of every row.
fn shapes<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    grid: &gpui_kit::Entity<LiveGridState<D>>,
    scope: ExportScope,
) -> Vec<String>
where
    D::Row: GridRow<Key = u32>,
{
    let job = grid
        .update(cx, |grid, cx| {
            grid.export_with(
                ExportRequest::new(scope),
                |table, _, control| {
                    control.advance(table.rows().len())?;
                    Ok(table.rows().iter().map(shape).collect::<Vec<_>>())
                },
                cx,
            )
        })
        .expect("something to export");
    finish(cx, job).expect("written").into_output()
}

#[gpui_kit::test]
fn test_export_csv_visible(cx: &mut TestAppContext) {
    let fixture = open_configured(cx, 12, None, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(ItemColumn::Id, SortDirection::Descending)])
                .with_aggregates([(ItemColumn::Price, Aggregate::Sum)]),
        )
        .with_tuning(unthrottled())
        .with_totals(PinnedEdge::Bottom)
    });
    fixture
        .writer(cx)
        .set_pinned_rows(PinnedEdge::Bottom, [Arc::new(Item { id: 99, price: -5 })]);
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();

    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            assert!(!grid.is_export_empty(ExportScope::Visible));
            grid.export(
                ExportRequest::new(ExportScope::Visible),
                CsvFormat::new(),
                Vec::new(),
                cx,
            )
        })
        .expect("something to export");
    let outcome = finish(cx, job).expect("written");

    // A byte order mark, the header, the rows in the shown order, the pinned
    // row of the bottom band, then the totals with their label.
    let mut expected = String::from("\u{feff}Id,Price\r\n");
    for id in (0..12).rev() {
        let _ = write!(expected, "{id},{}\r\n", id * 10);
    }
    expected.push_str("99,-5\r\nTotal,660\r\n");
    assert_eq!(text(outcome.output()), expected);
    assert_eq!(outcome.rows(), 14);
    assert_eq!(outcome.data_rows(), 13);
    assert_eq!(outcome.bytes(), Some(expected.len() as u64));
    // An export emits no event.
    assert!(fixture.events.borrow().is_empty());

    // The cells as on screen, by the request; the grid's own default stays.
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export(
                ExportRequest::new(ExportScope::Visible)
                    .with_header(ExportHeader::None)
                    .with_cell_export(CellExport::Displayed(DisplayedCells)),
                CSV,
                Vec::new(),
                cx,
            )
        })
        .expect("something to export");
    let outcome = finish(cx, job).expect("written");
    assert!(text(outcome.output()).starts_with("11,110\r\n"));
    assert_eq!(outcome.rows(), 14);
}

/// Stops at its gate, then writes CSV.
struct GatedCsv(Gate);

impl ExportFormat<Item> for GatedCsv {
    fn extension(&self) -> &str {
        "csv"
    }

    fn export(
        &self,
        table: &ExportTable<'_, Item>,
        cells: &CellExport<Item>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        self.0.pass(0);
        CSV.export(table, cells, out, control)
    }
}

#[gpui_kit::test]
fn test_export_snapshot_taken_at_command(cx: &mut TestAppContext) {
    let runner = ThreadRunner::default();
    let fixture = open_configured(cx, 6, None, {
        let runner = runner.clone();
        move |grid| grid.with_tuning(unthrottled()).with_task_runner(runner)
    });
    let gate = Gate::default();
    runner.next_on_thread();
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export(
                ExportRequest::new(ExportScope::Visible),
                GatedCsv(gate.clone()),
                Vec::new(),
                cx,
            )
        })
        .expect("something to export");
    gate.wait_for(0);

    // The data moves on while the job waits: a new price, a removed row.
    let writer = fixture.writer(cx);
    writer.upsert(Item { id: 2, price: -1 });
    writer.remove(3);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), [0, 1, 2, 4, 5]);

    // The file is of the moment of the command.
    gate.open(0);
    runner.join();
    assert!(job.is_finished());
    let outcome = smol::block_on(job).expect("written");
    assert_eq!(
        text(outcome.output()),
        "Id,Price\r\n0,0\r\n1,10\r\n2,20\r\n3,30\r\n4,40\r\n5,50\r\n"
    );
}

/// Trades grouped by instrument and side, footers and totals at the bottom.
fn by_instrument_and_side() -> ViewSpec<GroupedRow> {
    ViewSpec::new()
        .with_sort([(GroupedColumn::Id, SortDirection::Ascending)])
        .with_group_by([
            GroupBy::column(GroupedColumn::Instrument),
            GroupBy::column(GroupedColumn::Side),
        ])
        .with_aggregates([(GroupedColumn::Qty, Aggregate::Sum)])
        .with_group_footer(EveryGroupFooter)
}

#[gpui_kit::test]
fn test_export_expanded_marks(cx: &mut TestAppContext) {
    // AAPL and MSFT, a Buy and a Sell each.
    let rows: Vec<GroupedRow> = grouped_rows(8)
        .into_iter()
        .filter(|row| matches!(row.instrument, "AAPL" | "MSFT"))
        .collect();
    let fixture = open_grouped(cx, rows, |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
            .with_totals(PinnedEdge::Bottom)
    });
    fixture.grid.update(cx, |grid, cx| {
        grid.set_expanded(
            ExpandTarget::Group(group_path(&[(GroupedColumn::Instrument, "AAPL")])),
            false,
            cx,
        )
    });
    fixture.settle(cx);

    // Everything under the collapsed AAPL caption is hidden, its footer
    // too; the caption itself and the rest are shown. The totals carry
    // their band.
    let msft = [
        "Caption@0 MSFT",
        "Caption@1 Buy",
        "Data@2 1",
        "Footer@1 Buy",
        "Caption@1 Sell",
        "Data@2 5",
        "Footer@1 Sell",
        "Footer@0 MSFT",
    ];
    let mut expanded = vec![
        "Caption@0 AAPL collapsed",
        "Caption@1 Buy hidden",
        "Data@2 0 hidden",
        "Footer@1 Buy hidden",
        "Caption@1 Sell hidden",
        "Data@2 4 hidden",
        "Footer@1 Sell hidden",
        "Footer@0 AAPL collapsed hidden",
    ];
    expanded.extend(msft);
    expanded.push("Totals@0 band Bottom");
    assert_eq!(shapes(cx, &fixture.grid, ExportScope::Expanded), expanded);

    // As shown: nothing is hidden, and the collapsed group has its caption
    // alone.
    let mut visible = vec!["Caption@0 AAPL collapsed"];
    visible.extend(msft);
    visible.push("Totals@0 band Bottom");
    assert_eq!(shapes(cx, &fixture.grid, ExportScope::Visible), visible);

    // A collapsed subgroup under an expanded group: its rows and footer.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_expanded(
            ExpandTarget::Group(group_path(&[(GroupedColumn::Instrument, "AAPL")])),
            true,
            cx,
        );
        grid.set_expanded(
            ExpandTarget::Group(group_path(&[
                (GroupedColumn::Instrument, "MSFT"),
                (GroupedColumn::Side, "Sell"),
            ])),
            false,
            cx,
        );
    });
    fixture.settle(cx);
    let shapes = shapes(cx, &fixture.grid, ExportScope::Expanded);
    assert_eq!(
        shapes[12..16],
        [
            "Caption@1 Sell collapsed",
            "Data@2 5 hidden",
            "Footer@1 Sell collapsed hidden",
            "Footer@0 MSFT",
        ]
    );
    assert!(shapes[..12].iter().all(|shape| !shape.contains("hidden")));
}

#[gpui_kit::test]
fn test_export_tree_branch(cx: &mut TestAppContext) {
    // 0 ─ 1 ─ 3, 0 ─ 2; 4 ─ 5 ─ 7, 4 ─ 6; 8; 9.
    let parent_of = |id| match id {
        1 | 2 => Some(0),
        3 => Some(1),
        5 | 6 => Some(4),
        7 => Some(5),
        _ => None,
    };
    let rows = grouped_rows(10)
        .into_iter()
        .map(|row| GroupedRow {
            parent: parent_of(row.id),
            ..row
        })
        .collect();
    let fixture = open_grouped(cx, rows, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(GroupedColumn::Id, SortDirection::Ascending)])
                .with_tree(TreeSpec::new()),
        )
        .with_tuning(unthrottled())
    });
    assert!(fixture.grid.update(cx, |grid, cx| grid.set_expanded(
        ExpandTarget::Row(4),
        false,
        cx
    )));
    fixture.settle(cx);

    // A row with shown children carries their count and its state; the
    // descendants of the collapsed 4 are hidden.
    assert_eq!(
        shapes(cx, &fixture.grid, ExportScope::Expanded),
        [
            "Data@0 0 branch 2 expanded",
            "Data@1 1 branch 1 expanded",
            "Data@2 3",
            "Data@1 2",
            "Data@0 4 branch 2 collapsed",
            "Data@1 5 branch 1 expanded hidden",
            "Data@2 7 hidden",
            "Data@1 6 hidden",
            "Data@0 8",
            "Data@0 9",
        ]
    );
    assert_eq!(
        shapes(cx, &fixture.grid, ExportScope::Visible),
        [
            "Data@0 0 branch 2 expanded",
            "Data@1 1 branch 1 expanded",
            "Data@2 3",
            "Data@1 2",
            "Data@0 4 branch 2 collapsed",
            "Data@0 8",
            "Data@0 9",
        ]
    );
}

/// The metadata of an exported column: id, title, width, alignment, zone
/// and band titles.
type ColumnMetadata = (
    &'static str,
    String,
    Option<f32>,
    ColumnAlign,
    ColumnZone,
    Vec<String>,
);

/// The metadata of each exported column, the frozen columns and the band
/// depth.
type Metadata = (Vec<ColumnMetadata>, usize, usize);

fn column_metadata(
    id: &'static str,
    title: &str,
    width: f32,
    align: ColumnAlign,
    zone: ColumnZone,
    bands: &[&str],
) -> ColumnMetadata {
    (
        id,
        title.to_string(),
        Some(width),
        align,
        zone,
        bands.iter().map(|band| band.to_string()).collect(),
    )
}

fn metadata<D: LiveGridDelegate<Row = common::Order>>(
    cx: &mut TestAppContext,
    grid: &gpui_kit::Entity<LiveGridState<D>>,
    request: ExportRequest<common::Order>,
) -> Metadata {
    let job = grid
        .update(cx, |grid, cx| {
            grid.export_with(
                request,
                |table, _, _| {
                    let columns = table
                        .columns()
                        .iter()
                        .map(|column| {
                            (
                                column.column().id(),
                                column.title().to_string(),
                                column.width(),
                                column.align(),
                                column.zone(),
                                column.bands().iter().map(|band| band.to_string()).collect(),
                            )
                        })
                        .collect();
                    Ok((columns, table.frozen_columns(), table.band_depth()))
                },
                cx,
            )
        })
        .expect("something to export");
    finish(cx, job).expect("written").into_output()
}

#[gpui_kit::test]
fn test_export_metadata_from_layout(cx: &mut TestAppContext) {
    use OrderColumn::{Id, Instrument, Note, Price, Quantity};

    let schema = GridSchema::new()
        .column(GridColumn::new(Id, "Id").with_width(px(50.)))
        .column(GridColumn::new(Instrument, "Instrument").with_width(px(90.)))
        .band(
            ColumnBand::new("trade", "Trade")
                .column(GridColumn::new(Quantity, "Qty").with_width(px(70.)))
                .column(
                    GridColumn::new(Price, "Price")
                        .with_width(px(140.))
                        .with_align(ColumnAlign::End),
                ),
        )
        .column(
            GridColumn::new(Note, "Note")
                .with_width(px(120.))
                .with_visible(false),
        )
        .with_frozen(2);
    let fixture = open_orders_in(cx, schema, orders(6), Orders, |grid| {
        grid.with_tuning(unthrottled())
    });

    // The visible columns in screen order, as the layout shows them.
    use ColumnAlign::{Auto, End};
    use ColumnZone::{FrozenLeft, Scrolling};
    let (columns, frozen, depth) =
        metadata(cx, &fixture.grid, ExportRequest::new(ExportScope::Visible));
    assert_eq!(
        columns,
        [
            column_metadata("id", "Id", 50., Auto, FrozenLeft, &[]),
            column_metadata("instrument", "Instrument", 90., Auto, FrozenLeft, &[]),
            column_metadata("quantity", "Qty", 70., Auto, Scrolling, &["Trade"]),
            column_metadata("price", "Price", 140., End, Scrolling, &["Trade"]),
        ]
    );
    assert_eq!((frozen, depth), (2, 1));

    // Chosen columns, a hidden one included, in the order asked; a width
    // set since is the layout's.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_width(Price, px(160.), cx));
    let (columns, frozen, depth) = metadata(
        cx,
        &fixture.grid,
        ExportRequest::new(ExportScope::Visible).with_columns([Note, Price, Id]),
    );
    let ids: Vec<_> = columns.iter().map(|column| column.0).collect();
    assert_eq!(ids, ["note", "price", "id"]);
    assert_eq!(columns[0].2, Some(120.));
    assert_eq!(columns[1].2, Some(160.));
    assert_eq!(columns[1].5, ["Trade"]);
    assert_eq!((frozen, depth), (0, 1));
}

#[gpui_kit::test]
fn test_export_selection_cells(cx: &mut TestAppContext) {
    use OrderColumn::{Instrument, Price};

    let fixture = open_orders(cx, orders(12), |grid| {
        grid.with_selection_mode(SelectionMode::CellSet)
            .with_tuning(unthrottled())
    });
    fixture.grid.update(cx, |grid, cx| {
        grid.set_selected_cells(
            [(1, Price), (3, Price), (5, Instrument), (3, Instrument)],
            cx,
        )
    });

    // The columns that hold a selected cell, in screen order, under a header
    // line; the other cells of the rectangle are empty.
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export(
                ExportRequest::new(ExportScope::Selection),
                CSV,
                Vec::new(),
                cx,
            )
        })
        .expect("something to export");
    let outcome = finish(cx, job).expect("written");
    assert_eq!(
        text(outcome.output()),
        "instrument,price\r\n,100.25\r\nGAZP,100.75\r\nMSFT,\r\n"
    );
    assert_eq!((outcome.rows(), outcome.data_rows()), (3, 3));
}

#[gpui_kit::test]
fn test_export_nothing(cx: &mut TestAppContext) {
    let dir = TempDir::new("export-nothing");
    let path = dir.file("empty.csv");

    // An empty grid: nothing in any scope, the sink untouched, no file and
    // no part file.
    let fixture = open(cx, 0, ViewSpec::new(), Duration::ZERO);
    for scope in [
        ExportScope::Visible,
        ExportScope::Expanded,
        ExportScope::Selection,
    ] {
        let sink = SharedSink::default();
        fixture.grid.update(cx, |grid, cx| {
            assert!(grid.is_export_empty(scope), "{scope:?}");
            assert!(matches!(
                grid.export(
                    ExportRequest::new(scope),
                    CsvFormat::new(),
                    sink.clone(),
                    cx
                ),
                Err(ExportError::Nothing)
            ));
            assert!(matches!(
                grid.export_to_path(ExportRequest::new(scope), CsvFormat::new(), &path, cx),
                Err(ExportError::Nothing)
            ));
            assert!(matches!(
                grid.export_with(ExportRequest::new(scope), |_, _, _| Ok(()), cx),
                Err(ExportError::Nothing)
            ));
        });
        cx.run_until_parked();
        assert!(sink.is_empty());
        assert!(dir.files().is_empty(), "{:?}", dir.files());
    }

    // The header line alone, when asked for.
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export_to_path(
                ExportRequest::new(ExportScope::Visible).with_empty(EmptyExport::HeaderOnly),
                CsvFormat::new(),
                &path,
                cx,
            )
        })
        .expect("a header line");
    let outcome = finish(cx, job).expect("written");
    assert_eq!((outcome.rows(), outcome.data_rows()), (0, 0));
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        "\u{feff}Id,Price\r\n"
    );
    assert_eq!(dir.files(), ["empty.csv"]);

    // A quick filter that hides every row; a selection that is empty.
    let fixture = open(cx, 5, ViewSpec::new(), Duration::ZERO);
    fixture.grid.update(cx, |grid, _| {
        assert!(!grid.is_export_empty(ExportScope::Visible));
        assert!(grid.is_export_empty(ExportScope::Selection));
    });
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("no such row", cx));
    fixture.settle(cx);
    fixture.grid.update(cx, |grid, cx| {
        assert!(grid.is_export_empty(ExportScope::Visible));
        assert!(grid.is_export_empty(ExportScope::Expanded));
        assert!(matches!(
            grid.export(
                ExportRequest::new(ExportScope::Visible),
                CSV,
                Vec::new(),
                cx
            ),
            Err(ExportError::Nothing)
        ));
    });

    // A pinned row is something to export.
    fixture
        .writer(cx)
        .set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id: 7, price: 1 })]);
    fixture.settle(cx);
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            assert!(!grid.is_export_empty(ExportScope::Visible));
            grid.export(
                ExportRequest::new(ExportScope::Visible),
                CSV,
                Vec::new(),
                cx,
            )
        })
        .expect("the pinned row");
    let outcome = finish(cx, job).expect("written");
    assert_eq!(text(outcome.output()), "Id,Price\r\n7,1\r\n");
}

/// Writes a line, counts a row, cancels its own export and counts another.
struct CancelsItself;

impl ExportFormat<Item> for CancelsItself {
    fn extension(&self) -> &str {
        "txt"
    }

    fn export(
        &self,
        _: &ExportTable<'_, Item>,
        _: &CellExport<Item>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        writeln!(out, "partial")?;
        control.advance(1)?;
        control.cancel();
        control.advance(1)
    }
}

#[gpui_kit::test]
fn test_export_to_path_atomic(cx: &mut TestAppContext) {
    let dir = TempDir::new("export-atomic");
    let path = dir.file("items.csv");
    fs::write(&path, "previous").expect("a file to replace");
    let fixture = open(cx, 3, ViewSpec::new(), Duration::ZERO);
    let export = |cx: &mut TestAppContext, format: Box<dyn ExportFormat<Item>>, path: &Path| {
        fixture.grid.update(cx, |grid, cx| {
            grid.export_to_path(
                ExportRequest::new(ExportScope::Visible),
                BoxedFormat(format),
                path,
                cx,
            )
        })
    };

    // Success replaces the file whole; the job gives the path back.
    let job = export(cx, Box::new(CSV), &path).expect("something to export");
    let outcome = finish(cx, job).expect("written");
    let written = "Id,Price\r\n0,0\r\n1,10\r\n2,20\r\n";
    assert_eq!(outcome.output(), &path);
    assert_eq!(outcome.bytes(), Some(written.len() as u64));
    assert_eq!(fs::read_to_string(&path).expect("the file"), written);
    assert_eq!(dir.files(), ["items.csv"]);

    // Cancelled midway: the file stays as it was, the part file is gone.
    let job = export(cx, Box::new(CancelsItself), &path).expect("something to export");
    assert!(matches!(finish(cx, job), Err(ExportError::Cancelled)));
    assert_eq!(fs::read_to_string(&path).expect("the file"), written);
    assert_eq!(dir.files(), ["items.csv"]);

    // A directory that does not exist: an I/O error, nothing created.
    let missing = dir.file("missing").join("items.csv");
    let job = export(cx, Box::new(CSV), &missing).expect("something to export");
    assert!(matches!(
        finish(cx, job),
        Err(ExportError::Io(error)) if error.kind() == io::ErrorKind::NotFound
    ));
    assert_eq!(dir.files(), ["items.csv"]);

    // A detached export runs to its end with nobody waiting.
    let other = dir.file("other.csv");
    export(cx, Box::new(CSV), &other)
        .expect("something to export")
        .detach();
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&other).expect("the file"), written);
}

/// A boxed format: one type for the formats of a test.
struct BoxedFormat(Box<dyn ExportFormat<Item>>);

impl ExportFormat<Item> for BoxedFormat {
    fn extension(&self) -> &str {
        self.0.extension()
    }

    fn export(
        &self,
        table: &ExportTable<'_, Item>,
        cells: &CellExport<Item>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        self.0.export(table, cells, out, control)
    }
}

/// Writes a line and counts a row, waits at its gate, then counts another;
/// records what each count returned.
struct TwoSteps {
    gate: Gate,
    counts: Arc<Mutex<Vec<bool>>>,
}

impl ExportFormat<Item> for TwoSteps {
    fn extension(&self) -> &str {
        "txt"
    }

    fn export(
        &self,
        _: &ExportTable<'_, Item>,
        _: &CellExport<Item>,
        out: &mut dyn io::Write,
        control: &ExportControl,
    ) -> Result<(), ExportError> {
        writeln!(out, "first")?;
        let first = control.advance(1);
        self.counts
            .lock()
            .expect("not poisoned")
            .push(first.is_ok());
        first?;
        self.gate.pass(0);
        let second = control.advance(1);
        self.counts
            .lock()
            .expect("not poisoned")
            .push(second.is_ok());
        second?;
        writeln!(out, "second")?;
        Ok(())
    }
}

#[gpui_kit::test]
fn test_export_job_drop_cancels(cx: &mut TestAppContext) {
    let dir = TempDir::new("export-drop");
    let path = dir.file("items.txt");
    let runner = ThreadRunner::default();
    let fixture = open_configured(cx, 3, None, {
        let runner = runner.clone();
        move |grid| grid.with_tuning(unthrottled()).with_task_runner(runner)
    });
    let gate = Gate::default();
    let counts = Arc::new(Mutex::new(Vec::new()));
    runner.next_on_thread();
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export_to_path(
                ExportRequest::new(ExportScope::Visible),
                TwoSteps {
                    gate: gate.clone(),
                    counts: counts.clone(),
                },
                &path,
                cx,
            )
        })
        .expect("something to export");

    // Midway, the part file is there; dropping the job cancels the export:
    // the next count fails and the part file is removed.
    gate.wait_for(0);
    assert_eq!(dir.files(), ["items.txt.part"]);
    drop(job);
    gate.open(0);
    runner.join();
    assert_eq!(*counts.lock().expect("not poisoned"), [true, false]);
    assert!(dir.files().is_empty(), "{:?}", dir.files());

    // Cancelled before the job starts: it never writes.
    runner.hold_next();
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export_to_path(ExportRequest::new(ExportScope::Visible), CSV, &path, cx)
        })
        .expect("something to export");
    job.cancel();
    assert!(!job.is_finished());
    runner.release();
    assert!(matches!(finish(cx, job), Err(ExportError::Cancelled)));
    assert!(dir.files().is_empty(), "{:?}", dir.files());
}

#[gpui_kit::test]
fn test_export_progress(cx: &mut TestAppContext) {
    let runner = ThreadRunner::default();
    let fixture = open_configured(cx, 300, None, {
        let runner = runner.clone();
        move |grid| grid.with_tuning(unthrottled()).with_task_runner(runner)
    });
    let gate = Gate::default();
    runner.next_on_thread();
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            let gate = gate.clone();
            grid.export_with(
                ExportRequest::new(ExportScope::Visible),
                move |table, _, control| {
                    for step in 0..3 {
                        gate.pass(step);
                        control.advance(100)?;
                    }
                    gate.pass(3);
                    Ok(table.rows().len())
                },
                cx,
            )
        })
        .expect("something to export");

    // The rows written grow by the format's counts, read without a lock.
    for step in 0..=3 {
        gate.wait_for(step);
        let progress = job.progress();
        assert_eq!(progress.rows_written(), step * 100);
        assert_eq!(progress.rows_total(), 300);
        assert_eq!(progress.fraction(), step as f32 / 3.);
        assert!(!job.is_finished());
        gate.open(step);
    }
    runner.join();
    assert!(job.is_finished());
    assert_eq!(job.progress().fraction(), 1.0);
    let outcome = smol::block_on(job).expect("written");
    assert_eq!(outcome.output(), &300);
    assert_eq!(outcome.bytes(), None);
}

#[gpui_kit::test]
fn test_export_pending_edit(cx: &mut TestAppContext) {
    use OrderColumn::{Id, Note, Price};

    let fixture = open_orders(cx, orders(4), |grid| {
        with_order_editors(grid)
            .with_selection_mode(SelectionMode::RowSet)
            .with_tuning(unthrottled())
    });
    // A test sets the values a waiting commit shows, without real commits.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_test_pending_values([(2, Price, OwnedValue::Float(150.5))], cx)
    });

    // A text format writes what the screen shows.
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export(
                ExportRequest::new(ExportScope::Visible).with_columns([Id, Price]),
                CSV,
                Vec::new(),
                cx,
            )
        })
        .expect("something to export");
    let outcome = finish(cx, job).expect("written");
    assert_eq!(
        text(outcome.output()),
        "id,price\r\n1,100.25\r\n2,150.50\r\n3,100.75\r\n4,101.00\r\n"
    );

    // An application format sees the pending value and the stored one.
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export_with(
                ExportRequest::new(ExportScope::Visible),
                |table, _, _| {
                    Ok(table
                        .rows()
                        .iter()
                        .filter_map(|row| {
                            let cell = row.cell(Price)?;
                            Some((
                                *row.row()?.key(),
                                cell.value() == CellValue::Float(150.5),
                                cell.is_pending(),
                                cell.row_value() == Some(CellValue::Float(100.5)),
                            ))
                        })
                        .collect::<Vec<_>>())
                },
                cx,
            )
        })
        .expect("something to export");
    assert_eq!(
        finish(cx, job).expect("written").into_output(),
        [
            (1, false, false, false),
            (2, true, true, true),
            (3, false, false, false),
            (4, false, false, false),
        ]
    );

    // A copy writes it too.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_selected_keys([2], cx);
        assert!(grid.copy(cx));
    });
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("copied");
    assert!(copied.contains("\t150.50\t"), "{copied}");

    // The draft of an open editor is not a value yet: not exported.
    let started = cx
        .update_window(fixture.window, |_, window, cx| {
            fixture
                .grid
                .update(cx, |grid, cx| grid.start_edit(&3, Note, window, cx))
        })
        .expect("window is open");
    assert!(started);
    fixture.type_text(cx, "draft");
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export(
                ExportRequest::new(ExportScope::Visible).with_columns([Id, Note]),
                CSV,
                Vec::new(),
                cx,
            )
        })
        .expect("something to export");
    let outcome = finish(cx, job).expect("written");
    assert!(
        text(outcome.output()).contains("\r\n3,Order 3\r\n"),
        "{}",
        text(outcome.output())
    );
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_editing()));
}

#[gpui_kit::test]
fn test_app_format_typed_cells(cx: &mut TestAppContext) {
    use OrderColumn::{Active, Id, Instrument, Placed, Price, Quantity, Side, Status};

    let fixture = open_orders(cx, orders(4), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_group_by([GroupBy::column(Side)])
                .with_aggregates([
                    (Quantity, Aggregate::Sum),
                    (Price, Aggregate::Avg),
                    (Id, Aggregate::Count),
                ]),
        )
        .with_tuning(unthrottled())
    });

    // A format "like xlsx" records the kind of each value and its number
    // hint instead of writing bytes.
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export_with(
                ExportRequest::new(ExportScope::Expanded).with_columns([
                    Id, Side, Instrument, Quantity, Price, Status, Placed, Active,
                ]),
                |table, _, _| {
                    let mut cells = Vec::new();
                    for row in table.rows() {
                        for column in table.columns() {
                            let Some(cell) = row.cell(column.column()) else {
                                continue;
                            };
                            let kind = match cell.value() {
                                CellValue::Int(_) => "number",
                                CellValue::Float(_) => "number",
                                CellValue::Bool(_) => "boolean",
                                CellValue::Text(_) => "text",
                                CellValue::Timestamp(_) => "date",
                                CellValue::Icon(_) => "icon",
                                _ => "other",
                            };
                            cells.push((
                                row.line(),
                                column.column(),
                                kind,
                                cell.number_hint().map(|hint| hint.fraction_digits()),
                            ));
                        }
                    }
                    Ok(cells)
                },
                cx,
            )
        })
        .expect("something to export");
    let cells = finish(cx, job).expect("written").into_output();
    let data = |column| {
        cells
            .iter()
            .find(|(line, each, ..)| *line == ExportLine::Data && *each == column)
            .map(|(_, _, kind, hint)| (*kind, *hint))
    };
    assert_eq!(data(Id), Some(("number", Some(0))));
    assert_eq!(data(Quantity), Some(("number", Some(0))));
    assert_eq!(data(Price), Some(("number", Some(2))));
    assert_eq!(data(Active), Some(("boolean", None)));
    assert_eq!(data(Instrument), Some(("text", None)));
    assert_eq!(data(Placed), Some(("date", None)));
    assert_eq!(data(Status), Some(("icon", None)));

    // A caption's key value is text; its aggregates are numbers: a count
    // and a sum without decimals, an average with two.
    let caption: Vec<_> = cells
        .iter()
        .filter(|(line, ..)| *line == ExportLine::Caption)
        .take(4)
        .map(|(_, column, kind, hint)| (*column, *kind, *hint))
        .collect();
    assert_eq!(
        caption,
        [
            (Id, "number", Some(0)),
            (Side, "text", None),
            (Quantity, "number", Some(0)),
            (Price, "number", Some(2)),
        ]
    );
}
