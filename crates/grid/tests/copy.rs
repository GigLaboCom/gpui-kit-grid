//! Headless UI tests of copying (`.agents/COMPONENT_TEST_RULES.md`): `Copy`
//! takes the command scope in the shown order, `CopyAll` every shown line under
//! a header line, both over the visible columns in screen order; the cell modes
//! copy the bounding rectangle; the exporters set through the builders write
//! the text; a large copy is written by the task runner from the snapshot taken
//! when the command ran, and a new copy cancels it.
//!
//! A grouped view copies its summary rows (a caption's key value under the
//! level's column and its aggregates under theirs, footers, the totals row in
//! its band), `copy_scope(ExportScope::Expanded)` every group and tree row
//! expanded, and a command over a group caption its group's rows, from the keys
//! and from the caption's menu.
//! Assertions read the clipboard of the test platform.

mod common;

use std::{cell::Cell, fmt::Write as _, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, ClipboardItem, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels,
    Point, Render, Role, Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, px,
    size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    Aggregate, CellExport, CellExporter, CellValue, ColumnIx, ColumnSet, ColumnZone,
    DisplayedCells, EveryGroupFooter, ExpandTarget, ExportCell, ExportScope, ExportTable, Exporter,
    GridColumn, GridColumns, GridRow, GridSchema, GridTuning, GridWriter, GroupBy, GroupKey,
    GroupPath, LiveGrid, LiveGridDelegate, LiveGridState, ManualReorder, NumberFormat,
    NumberPrecision, PinnedEdge, RawCells, SelectionMode, SortDirection, StoreOptions, TreeSpec,
    ViewSpec,
    actions::{Copy, CopyAll},
};

use crate::common::{
    Grouped, GroupedColumn, GroupedRow, INSTRUMENTS, PausableRunner, caption_point, group_path,
    grouped_rows, grouped_schema, press_at, unthrottled,
};

use GroupedColumn::{Id, Instrument, Price, Qty, Side, Trader};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Col {
    Id,
    Symbol,
    Price,
    Venue,
    Live,
    Note,
}

impl GridColumns for Col {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Symbol,
            Self::Price,
            Self::Venue,
            Self::Live,
            Self::Note,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Symbol => "symbol",
            Self::Price => "price",
            Self::Venue => "venue",
            Self::Live => "live",
            Self::Note => "note",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone, Debug)]
struct Quote {
    id: u32,
    symbol: Arc<str>,
    price: f64,
    note: Arc<str>,
}

impl GridRow for Quote {
    type Key = u32;
    type Column = Col;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        match column {
            Col::Id => CellValue::Int(self.id.into()),
            Col::Symbol => CellValue::Text(&self.symbol),
            Col::Price => CellValue::Float(self.price),
            Col::Venue => CellValue::Text("XNAS"),
            Col::Live => CellValue::Bool(self.id % 2 == 0),
            Col::Note => CellValue::Text(&self.note),
        }
    }
}

/// `price = id * 10 + 0.257`; the note of row 7 holds a tab.
fn quote(id: u32, sign: f64) -> Arc<Quote> {
    Arc::new(Quote {
        id,
        symbol: format!("S{id}").into(),
        price: sign * (f64::from(id) * 10. + 0.257),
        note: if id == 7 {
            "a\tb".into()
        } else {
            format!("n{id}").into()
        },
    })
}

/// The TSV line `RawCells` writes for `quote(id, sign)`, in screen order:
/// the price with the two decimals the grid shows.
fn raw_line(id: u32, sign: f64) -> String {
    let quote = quote(id, sign);
    let note = if id == 7 {
        "\"a\tb\"".to_string()
    } else {
        quote.note.to_string()
    };
    let live = if id % 2 == 0 { "TRUE" } else { "FALSE" };
    format!("{id}\t{:.2}\t{live}\t{note}\tS{id}", quote.price)
}

struct Quotes;

impl LiveGridDelegate for Quotes {
    type Row = Quote;
}

/// Screen order Id | Price, Live, Note | Symbol: Id frozen left, Symbol
/// frozen right, Venue hidden.
fn schema() -> GridSchema<Col> {
    GridSchema::new()
        .column(GridColumn::new(Col::Id, "Id").with_zone(ColumnZone::FrozenLeft))
        .column(GridColumn::new(Col::Symbol, "Symbol").with_zone(ColumnZone::FrozenRight))
        .column(GridColumn::new(Col::Price, "Price"))
        .column(GridColumn::new(Col::Venue, "Venue").with_visible(false))
        .column(GridColumn::new(Col::Live, "Live"))
        .column(GridColumn::new(Col::Note, "Note \"short\""))
}

const HEADER: &str = "Id\tPrice\tLive\t\"Note \"\"short\"\"\"\tSymbol";

/// The grid under a div that counts the copy Actions the grid lets
/// through.
struct Root<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
    outer_copies: Rc<Cell<usize>>,
}

impl<D: LiveGridDelegate> Render for Root<D> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let copies = self.outer_copies.clone();
        let copies_all = self.outer_copies.clone();
        div()
            .size_full()
            .on_action(move |_: &Copy, _, _| copies.set(copies.get() + 1))
            .on_action(move |_: &CopyAll, _, _| copies_all.set(copies_all.get() + 1))
            .child(
                LiveGrid::new(&self.grid)
                    .with_size(Size::XSmall)
                    .bordered(false),
            )
    }
}

struct Fixture<D: LiveGridDelegate = Quotes> {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<D>>,
    outer_copies: Rc<Cell<usize>>,
}

/// An `xsmall` grid of `schema` in a window `width` wide with room for ten
/// rows, configured by `configure`, under a [`Root`].
fn open_root<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    schema: GridSchema<<D::Row as GridRow>::Column>,
    delegate: D,
    width: f32,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> Fixture<D> {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let outer_copies = Rc::new(Cell::new(0));
    let (window, root) = cx
        .update(|cx| {
            let outer_copies = outer_copies.clone();
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(width), px(26. * 11.)),
                    })),
                    ..Default::default()
                },
                cx,
                move |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            configure(LiveGridState::new_standalone(
                                schema,
                                delegate,
                                StoreOptions::default(),
                                cx,
                            ))
                        }),
                        outer_copies,
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    Fixture {
        window,
        grid,
        outer_copies,
    }
}

/// `count` rows sorted by price, highest first, configured by `configure`.
fn open(
    cx: &mut TestAppContext,
    count: u32,
    configure: impl FnOnce(LiveGridState<Quotes>) -> LiveGridState<Quotes> + 'static,
) -> Fixture {
    let fixture = open_root(cx, schema(), Quotes, 400., move |grid| {
        configure(
            grid.with_view(ViewSpec::new().with_sort([(Col::Price, SortDirection::Descending)]))
                .with_tuning(GridTuning::default().with_min_rebuild_interval(Duration::ZERO)),
        )
    });
    fixture
        .writer(cx)
        .replace_all((0..count).map(|id| quote(id, 1.)));
    fixture.settle(cx);
    fixture
}

/// A grid of the trades `rows` (`common::grouped_schema`, every column
/// visible), configured by `configure`.
fn open_trades(
    cx: &mut TestAppContext,
    rows: Vec<GroupedRow>,
    configure: impl FnOnce(LiveGridState<Grouped>) -> LiveGridState<Grouped> + 'static,
) -> Fixture<Grouped> {
    let fixture = open_root(cx, grouped_schema(), Grouped, 480., configure);
    fixture
        .writer(cx)
        .replace_all(rows.into_iter().map(Arc::new));
    fixture.settle(cx);
    fixture
}

impl<D: LiveGridDelegate> Fixture<D> {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            self.input(cx, |window, cx| window.render_frame(cx));
        }
    }

    fn input<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut Window, &mut gpui_kit::App) -> R,
    ) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    /// Focuses the grid, then dispatches `action` to it, as a key or a menu
    /// item does.
    fn dispatch(&self, cx: &mut TestAppContext, action: &dyn gpui_kit::Action) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.dispatch_action(action.boxed_clone(), cx);
        });
    }

    /// Focuses the grid and presses Cmd/Ctrl+C.
    fn press_copy(&self, cx: &mut TestAppContext) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.press("secondary-c", cx);
        });
    }

    fn update<T>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut LiveGridState<D>, &mut Context<LiveGridState<D>>) -> T,
    ) -> T {
        self.grid.update(cx, f)
    }

    fn writer(&self, cx: &mut TestAppContext) -> GridWriter<D::Row> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    /// Copies `scope`; the text copied, or `None` if nothing was.
    fn copied(&self, cx: &mut TestAppContext, scope: ExportScope) -> Option<String> {
        set_clipboard(cx, "kept");
        if !self.update(cx, |grid, cx| grid.copy_scope(scope, cx)) {
            assert_eq!(clipboard(cx).as_deref(), Some("kept"), "left alone");
            return None;
        }
        clipboard(cx)
    }

    /// Presses the right button at `position`, which opens the context
    /// menu.
    fn right_press(&self, cx: &mut TestAppContext, position: Point<Pixels>) {
        self.input(cx, |window, cx| {
            press_at(
                window,
                position,
                MouseButton::Right,
                Modifiers::default(),
                1,
                cx,
            )
        });
        self.settle(cx);
    }

    /// Clicks the open menu's item labelled `label`, which must be enabled.
    fn click_item(&self, cx: &mut TestAppContext, label: &str) {
        let item = self.input(cx, |window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|item| {
                    item.role() == Some(Role::MenuItem)
                        && item.visible()
                        && item.label() == Some(label)
                })
                .unwrap_or_else(|| panic!("no menu item {label:?}"))
        });
        assert_ne!(item.disabled(), Some(true), "{label} is enabled");
        self.input(cx, |window, cx| {
            press_at(
                window,
                item.bounds().center(),
                MouseButton::Left,
                Modifiers::default(),
                1,
                cx,
            )
        });
        self.settle(cx);
    }
}

fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

fn set_clipboard(cx: &mut TestAppContext, text: &str) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
}

fn lines(ids: &[u32], sign: f64) -> Vec<String> {
    ids.iter().map(|id| raw_line(*id, sign)).collect()
}

/// Compares a large copy line by line, naming the first line that differs
/// instead of printing both texts.
fn assert_lines(text: &str, expected: &[String]) {
    let actual: Vec<&str> = text.split('\n').collect();
    if let Some(ix) = (0..actual.len().min(expected.len())).find(|ix| actual[*ix] != expected[*ix])
    {
        panic!(
            "line {ix} differs: {:?} instead of {:?}",
            actual[ix], expected[ix]
        );
    }
    assert_eq!(actual.len(), expected.len(), "line count");
}

#[gpui_kit::test]
fn test_copy_selection_and_copy_all(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_reorder_policy(ManualReorder)
    });

    // Three selected rows, one removed: the other two in the shown order
    // (price, highest first), every visible column in screen order, raw
    // numbers, no header line. Cmd/Ctrl+C is the `Copy` key.
    fixture.update(cx, |grid, cx| grid.set_selected_keys([3, 5, 7], cx));
    fixture.update(cx, |grid, _| grid.writer()).remove(5);
    fixture.settle(cx);
    fixture.press_copy(cx);
    assert_eq!(clipboard(cx), Some(lines(&[7, 3], 1.).join("\n")));
    assert_eq!(fixture.outer_copies.get(), 0, "the grid handled the key");

    // The order held by `ManualReorder` is copied as it is on screen, with
    // the values of the snapshot shown: a header line, every live row, the
    // hidden column left out.
    let writer = fixture.update(cx, |grid, _| grid.writer());
    for id in [0, 1, 2, 3, 4, 6, 7, 8, 9] {
        writer.upsert(quote(id, -1.));
    }
    fixture.settle(cx);
    assert!(fixture.update(cx, |grid, _| grid.is_order_held()));
    fixture.dispatch(cx, &CopyAll);
    let mut expected = vec![HEADER.to_string()];
    expected.extend(lines(&[9, 8, 7, 6, 4, 3, 2, 1, 0], -1.));
    assert_eq!(clipboard(cx), Some(expected.join("\n")));
    assert_eq!(fixture.outer_copies.get(), 0);

    // An empty scope leaves the clipboard alone and lets the key go on.
    fixture.update(cx, |grid, cx| grid.clear_selection(cx));
    set_clipboard(cx, "kept");
    fixture.press_copy(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("kept"));
    assert_eq!(fixture.outer_copies.get(), 1, "the key went on");
    assert!(!fixture.update(cx, |grid, cx| grid.copy(cx)));

    // So does "Copy all" when no row is shown.
    fixture.update(cx, |grid, cx| grid.set_quick_filter("no such row", cx));
    fixture.settle(cx);
    assert!(fixture.update(cx, |grid, _| grid.projection().is_empty()));
    fixture.dispatch(cx, &CopyAll);
    assert_eq!(clipboard(cx).as_deref(), Some("kept"));
    assert_eq!(fixture.outer_copies.get(), 2);
}

#[gpui_kit::test]
fn test_copy_cell_rectangle(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| {
        grid.with_selection_mode(SelectionMode::CellSet)
    });

    // The visible columns that hold a selected cell, in screen order (Id,
    // Price, Note); unselected cells of the rectangle are empty. Row 4 has
    // only a cell of the hidden column: nothing to copy, no line.
    fixture.update(cx, |grid, cx| {
        grid.set_selected_cells(
            [
                (3, Col::Price),
                (7, Col::Note),
                (7, Col::Id),
                (4, Col::Venue),
            ],
            cx,
        )
    });
    assert!(fixture.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("7\t\t\"a\tb\"\n\t30.26\t"));

    // Selected cells all in hidden columns copy nothing.
    set_clipboard(cx, "kept");
    fixture.update(cx, |grid, cx| {
        grid.set_selected_cells([(4, Col::Venue)], cx)
    });
    assert!(!fixture.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("kept"));

    // "Copy all" copies every visible column whatever the selection.
    assert!(fixture.update(cx, |grid, cx| grid.copy_all(cx)));
    let text = clipboard(cx).expect("copied");
    assert_eq!(text.lines().next(), Some(HEADER));
    assert_eq!(text.lines().count(), 11);
}

/// Writes the shape of the table instead of its cells.
struct ShapeExporter;

impl Exporter<Quote> for ShapeExporter {
    fn export(&self, table: &ExportTable<'_, Quote>, _: &CellExport<Quote>, out: &mut String) {
        let _ = write!(
            out,
            "{} rows, columns {:?}, header {}",
            table.rows().len(),
            table
                .columns()
                .iter()
                .map(|column| column.column())
                .collect::<Vec<_>>(),
            table.has_header()
        );
    }
}

/// Writes the column id and the row key.
struct KeyCells;

impl CellExporter<Quote> for KeyCells {
    fn write_cell(&self, cell: &ExportCell<'_, Quote>, out: &mut String) {
        if let Some(row) = cell.row() {
            let _ = write!(out, "{}:{}", cell.column().id(), row.key());
        }
    }
}

#[gpui_kit::test]
fn test_copy_builders(cx: &mut TestAppContext) {
    // The text painted in the cell: two decimals, "✓" or nothing.
    let displayed = open(cx, 10, |grid| {
        grid.with_cell_export(CellExport::Displayed(DisplayedCells))
    });
    displayed.update(cx, |grid, cx| grid.set_selected_keys([4], cx));
    assert!(displayed.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("4\t40.26\t✓\tn4\tS4"));
    displayed.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
    assert!(displayed.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("3\t30.26\t\tn3\tS3"));

    // Raw values with a decimal comma.
    let comma = open(cx, 10, |grid| {
        grid.with_cell_export(CellExport::Raw(RawCells::new().with_decimal_separator(',')))
    });
    comma.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
    assert!(comma.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("3\t30,26\tFALSE\tn3\tS3"));

    // Every digit, for a program that computes with the copy.
    let full = open(cx, 10, |grid| {
        grid.with_cell_export(CellExport::Raw(
            RawCells::new().with_precision(NumberPrecision::Full),
        ))
    });
    full.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
    assert!(full.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("3\t30.257\tFALSE\tn3\tS3"));

    // A price shown to a tenth copies to a tenth, raw or as displayed.
    let tenth = GridSchema::new()
        .column(GridColumn::new(Col::Id, "Id"))
        .column(
            GridColumn::new(Col::Price, "Price")
                .with_number_format(NumberFormat::new().with_decimals(1)),
        );
    for cells in [CellExport::default(), CellExport::Displayed(DisplayedCells)] {
        let fixture = open_root(cx, tenth.clone(), Quotes, 400., |grid| {
            grid.with_cell_export(cells)
        });
        fixture
            .writer(cx)
            .replace_all((0..10).map(|id| quote(id, -1.)));
        fixture.settle(cx);
        fixture.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
        assert!(fixture.update(cx, |grid, cx| grid.copy(cx)));
        assert_eq!(clipboard(cx).as_deref(), Some("3\t-30.3"));
    }

    // An application cell exporter, called for every copied cell.
    let custom = open(cx, 10, |grid| {
        grid.with_cell_export(CellExport::Custom(Arc::new(KeyCells)))
    });
    custom.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
    assert!(custom.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("id:3\tprice:3\tlive:3\tnote:3\tsymbol:3")
    );

    // An application format sees the table: rows, columns in screen order,
    // the header line of "Copy all".
    let shape = open(cx, 10, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_exporter(ShapeExporter)
    });
    shape.update(cx, |grid, cx| grid.set_selected_keys([1, 2], cx));
    assert!(shape.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("2 rows, columns [Id, Price, Live, Note, Symbol], header false")
    );
    assert!(shape.update(cx, |grid, cx| grid.copy_all(cx)));
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("10 rows, columns [Id, Price, Live, Note, Symbol], header true")
    );
}

/// Rows of a copy written by the task runner: more than 40k cells.
const LARGE: u32 = 9_000;

#[gpui_kit::test]
fn test_large_copy_runs_as_a_job(cx: &mut TestAppContext) {
    let runner = PausableRunner::default();
    let fixture = open(cx, LARGE, {
        let runner = runner.clone();
        move |grid| {
            grid.with_selection_mode(SelectionMode::RowSet)
                .with_task_runner(runner)
        }
    });

    // The job waits; the clipboard does not change yet. Meanwhile a row
    // changes and another is removed: the copy is of the snapshot taken
    // when the command ran.
    set_clipboard(cx, "kept");
    runner.pause();
    fixture.dispatch(cx, &CopyAll);
    let writer = fixture.update(cx, |grid, _| grid.writer());
    writer.upsert(quote(LARGE - 1, -1.));
    writer.remove(LARGE - 2);
    fixture.settle(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("kept"));
    runner.resume();
    fixture.settle(cx);
    let text = clipboard(cx).expect("copied");
    let mut expected = vec![HEADER.to_string()];
    expected.extend(lines(&(0..LARGE).rev().collect::<Vec<_>>(), 1.));
    assert_lines(&text, &expected);

    // A new copy cancels the one being written: a small one is written at
    // once, and the large one never reaches the clipboard.
    runner.pause();
    assert!(fixture.update(cx, |grid, cx| grid.copy_all(cx)));
    fixture.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
    fixture.press_copy(cx);
    assert_lines(&clipboard(cx).expect("copied"), &[raw_line(3, 1.)]);
    runner.resume();
    fixture.settle(cx);
    assert_lines(&clipboard(cx).expect("copied"), &[raw_line(3, 1.)]);

    // A large selection is a job too; the removed row is not in it.
    fixture.update(cx, |grid, cx| grid.select_all(cx));
    runner.pause();
    fixture.press_copy(cx);
    fixture.settle(cx);
    assert_lines(&clipboard(cx).expect("copied"), &[raw_line(3, 1.)]);
    runner.resume();
    fixture.settle(cx);
    let text = clipboard(cx).expect("copied");
    assert_eq!(text.lines().count(), LARGE as usize - 1);
    assert_eq!(text.lines().next(), Some(&*raw_line(LARGE - 3, 1.)));
}

#[gpui_kit::test]
fn test_large_copy_in_the_background(cx: &mut TestAppContext) {
    // The default runner: the text reaches the clipboard once the
    // background job is done.
    let fixture = open(cx, LARGE, |grid| grid);
    fixture.dispatch(cx, &CopyAll);
    fixture.settle(cx);
    let text = clipboard(cx).expect("copied");
    assert_eq!(text.lines().count(), LARGE as usize + 1);
    assert_eq!(text.lines().nth(1), Some(&*raw_line(LARGE - 1, 1.)));
    assert_eq!(
        text.lines().last().map(str::to_string),
        Some(raw_line(0, 1.))
    );
}

// Copies of grouped views and trees.

/// Every column of the trades in screen order, and the header line of a
/// copy of all of them.
const TRADE_COLUMNS: [GroupedColumn; 6] = [Id, Instrument, Side, Qty, Price, Trader];
const TRADE_HEADER: &str = "id\tinstrument\tside\tqty\tprice\ttrader";

/// Grouped by instrument, the rows by id, `Sum(Qty)` and `Avg(Price)`.
fn by_instrument() -> ViewSpec<GroupedRow> {
    ViewSpec::new()
        .with_sort([(Id, SortDirection::Ascending)])
        .with_group_by([GroupBy::column(Instrument)])
        .with_aggregates([(Qty, Aggregate::Sum), (Price, Aggregate::Avg)])
}

/// [`by_instrument`], then by side.
fn by_instrument_and_side() -> ViewSpec<GroupedRow> {
    by_instrument().with_group_by([GroupBy::column(Instrument), GroupBy::column(Side)])
}

fn instrument(name: &str) -> GroupPath {
    group_path(&[(Instrument, name)])
}

/// The ids of `grouped_rows(12)` of the instrument `name`: three trades.
fn instrument_ids(name: &str) -> Vec<u32> {
    (0..12)
        .filter(|id| INSTRUMENTS[*id as usize % INSTRUMENTS.len()] == name)
        .collect()
}

/// The lines `RawCells` and `TsvExporter` write for some trades over some
/// columns, with the aggregates of [`by_instrument`].
struct Trades {
    rows: Vec<GroupedRow>,
    columns: Vec<GroupedColumn>,
}

impl Trades {
    /// Over every column.
    fn new(rows: &[GroupedRow]) -> Self {
        Self {
            rows: rows.to_vec(),
            columns: TRADE_COLUMNS.to_vec(),
        }
    }

    fn with_columns(mut self, columns: &[GroupedColumn]) -> Self {
        self.columns = columns.to_vec();
        self
    }

    fn header(&self) -> String {
        let titles: Vec<&str> = self.columns.iter().map(|column| column.id()).collect();
        titles.join("\t")
    }

    /// A line of `cells` (the first of a column wins), empty fields
    /// elsewhere.
    fn line(&self, cells: &[(GroupedColumn, String)]) -> String {
        let fields: Vec<&str> = self
            .columns
            .iter()
            .map(|column| {
                cells
                    .iter()
                    .find(|(each, _)| each == column)
                    .map_or("", |(_, text)| text.as_str())
            })
            .collect();
        fields.join("\t")
    }

    fn trade(&self, id: u32) -> &GroupedRow {
        self.rows
            .iter()
            .find(|row| row.id == id)
            .expect("a trade of the table")
    }

    fn data(&self, ids: &[u32]) -> Vec<String> {
        ids.iter()
            .map(|id| {
                let trade = self.trade(*id);
                self.line(&[
                    (Id, trade.id.to_string()),
                    (Instrument, trade.instrument.to_owned()),
                    (Side, trade.side.as_str().to_owned()),
                    (Qty, trade.qty.to_string()),
                    (Price, format!("{:.2}", trade.price)),
                    (Trader, trade.trader.to_owned()),
                ])
            })
            .collect()
    }

    /// `Sum(Qty)` and `Avg(Price)` of the trades `ids`, as shown. The prices are
    /// multiples of 2.5, so their sum is exact in any order.
    fn aggregates(&self, ids: &[u32]) -> Vec<(GroupedColumn, String)> {
        let trades: Vec<&GroupedRow> = ids.iter().map(|id| self.trade(*id)).collect();
        let qty: i64 = trades.iter().map(|trade| trade.qty).sum();
        let price = trades.iter().map(|trade| trade.price).sum::<f64>() / trades.len() as f64;
        vec![(Qty, qty.to_string()), (Price, format!("{price:.2}"))]
    }

    /// A caption: the key `value` under `column`, the aggregates of `ids`.
    fn caption(&self, column: GroupedColumn, value: &str, ids: &[u32]) -> String {
        let mut cells = vec![(column, value.to_owned())];
        cells.extend(self.aggregates(ids));
        self.line(&cells)
    }

    fn footer(&self, ids: &[u32]) -> String {
        self.line(&self.aggregates(ids))
    }

    /// The totals row: the label under the first column, the aggregates.
    fn totals(&self, ids: &[u32]) -> String {
        let mut cells = self.aggregates(ids);
        cells.push((self.columns[0], "Total".to_owned()));
        self.line(&cells)
    }
}

/// Writes what each copied row is, its depth and its key or its group's
/// key value, as `Caption@1 Buy`, a line each; `header` for a header line.
struct LineShapes;

impl Exporter<GroupedRow> for LineShapes {
    fn export(
        &self,
        table: &ExportTable<'_, GroupedRow>,
        _: &CellExport<GroupedRow>,
        out: &mut String,
    ) {
        let mut lines = Vec::new();
        if table.has_header() {
            lines.push("header".to_owned());
        }
        for row in table.rows() {
            let name = match (row.row(), row.group()) {
                (Some(trade), _) => trade.id.to_string(),
                (None, Some(group)) => match group.value().as_cell() {
                    CellValue::Text(text) => text.to_owned(),
                    value => format!("{value:?}"),
                },
                (None, None) => String::new(),
            };
            let line = format!("{:?}@{} {name}", row.line(), row.depth());
            lines.push(line.trim_end().to_owned());
        }
        out.push_str(&lines.join("\n"));
    }
}

fn selected_keys<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &Fixture<D>,
    cx: &mut TestAppContext,
) -> Vec<u32> {
    fixture.grid.read_with(cx, |grid, _| {
        let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
        keys.sort_unstable();
        keys
    })
}

#[gpui_kit::test]
fn test_copy_all_grouped_visible(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let pinned = GroupedRow {
        id: 99,
        instrument: "XLK",
        ..rows[0].clone()
    };
    let fixture = open_trades(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument())
            .with_tuning(unthrottled())
            .with_totals(PinnedEdge::Bottom)
    });
    fixture
        .writer(cx)
        .set_pinned_rows(PinnedEdge::Bottom, [Arc::new(pinned.clone())]);
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(instrument("MSFT")), false, cx)
    });
    fixture.settle(cx);

    // The header; each caption with its key under Instrument and its
    // aggregates under Qty and Price, then the rows of an expanded group;
    // the collapsed MSFT is its caption alone. The pinned row of the bottom
    // band follows, and the totals over every row of the view, those of
    // MSFT too, come last with the label under Id.
    let trades = Trades::new(&rows);
    let mut expected = vec![TRADE_HEADER.to_owned()];
    for name in INSTRUMENTS {
        let ids = instrument_ids(name);
        expected.push(trades.caption(Instrument, name, &ids));
        if name != "MSFT" {
            expected.extend(trades.data(&ids));
        }
    }
    expected.extend(Trades::new(&[pinned]).data(&[99]));
    expected.push(trades.totals(&(0..12).collect::<Vec<_>>()));
    let expected = expected.join("\n");
    fixture.dispatch(cx, &CopyAll);
    assert_eq!(clipboard(cx).as_deref(), Some(expected.as_str()));
    assert_eq!(fixture.outer_copies.get(), 0);
    assert_eq!(fixture.copied(cx, ExportScope::Visible), Some(expected));
}

#[gpui_kit::test]
fn test_copy_scope_expanded(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let fixture = open_trades(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument().with_group_footer(EveryGroupFooter))
            .with_tuning(unthrottled())
            .with_totals(PinnedEdge::Bottom)
    });
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(instrument("MSFT")), false, cx)
    });
    fixture.settle(cx);
    let trades = Trades::new(&rows);
    let all: Vec<u32> = (0..12).collect();

    // Every group expanded: the rows of the collapsed MSFT under its
    // caption, each group's footer after its rows, the totals last.
    let mut expanded = vec![TRADE_HEADER.to_owned()];
    for name in INSTRUMENTS {
        let ids = instrument_ids(name);
        expanded.push(trades.caption(Instrument, name, &ids));
        expanded.extend(trades.data(&ids));
        expanded.push(trades.footer(&ids));
    }
    expanded.push(trades.totals(&all));
    assert_eq!(
        fixture.copied(cx, ExportScope::Expanded),
        Some(expanded.join("\n"))
    );

    // As shown, a collapsed group has no footer either.
    let mut visible = vec![TRADE_HEADER.to_owned()];
    for name in INSTRUMENTS {
        let ids = instrument_ids(name);
        visible.push(trades.caption(Instrument, name, &ids));
        if name != "MSFT" {
            visible.extend(trades.data(&ids));
            visible.push(trades.footer(&ids));
        }
    }
    visible.push(trades.totals(&all));
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(visible.join("\n"))
    );

    // The totals at the top: the first line after the header.
    fixture.update(cx, |grid, cx| grid.set_totals(Some(PinnedEdge::Top), cx));
    let text = fixture.copied(cx, ExportScope::Expanded).expect("copied");
    let mut top = expanded.clone();
    let totals = top.pop().expect("the totals line");
    top.insert(1, totals);
    assert_lines(&text, &top);
}

#[gpui_kit::test]
fn test_copy_lines_and_depths(cx: &mut TestAppContext) {
    // AAPL and MSFT, a Buy and a Sell each.
    let rows: Vec<GroupedRow> = grouped_rows(8)
        .into_iter()
        .filter(|row| matches!(row.instrument, "AAPL" | "MSFT"))
        .collect();
    let fixture = open_trades(cx, rows, |grid| {
        grid.with_view(by_instrument_and_side().with_group_footer(EveryGroupFooter))
            .with_tuning(unthrottled())
            .with_totals(PinnedEdge::Bottom)
            .with_exporter(LineShapes)
    });
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(instrument("AAPL")), false, cx)
    });
    fixture.settle(cx);

    // A caption and a footer at their group's depth, a data row one level
    // below its group's caption; the totals at the top level.
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
    let mut visible = vec!["header", "Caption@0 AAPL"];
    visible.extend(msft);
    visible.push("Totals@0");
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(visible.join("\n"))
    );
    let mut expanded = vec![
        "header",
        "Caption@0 AAPL",
        "Caption@1 Buy",
        "Data@2 0",
        "Footer@1 Buy",
        "Caption@1 Sell",
        "Data@2 4",
        "Footer@1 Sell",
        "Footer@0 AAPL",
    ];
    expanded.extend(msft);
    expanded.push("Totals@0");
    assert_eq!(
        fixture.copied(cx, ExportScope::Expanded),
        Some(expanded.join("\n"))
    );
}

#[gpui_kit::test]
fn test_copy_caption_focus(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let fixture = open_trades(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
            .with_selection_mode(SelectionMode::RowSet)
    });
    let aapl = instrument("AAPL");
    let aapl_sell = group_path(&[(Instrument, "AAPL"), (Side, "Sell")]);
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(aapl_sell.clone()), false, cx)
    });
    fixture.update(cx, |grid, cx| grid.set_selected_keys([1], cx));
    fixture.settle(cx);
    let trades = Trades::new(&rows);

    // Cmd/Ctrl+C on the AAPL caption: every row of the group, those of the
    // collapsed Sell too, in the order of the rows (Buy before Sell), with
    // no summary row and no header. The selected MSFT row is not in it.
    assert!(fixture.update(cx, |grid, cx| grid.set_focused_group(&aapl, cx)));
    fixture.press_copy(cx);
    assert_eq!(clipboard(cx), Some(trades.data(&[0, 8, 4]).join("\n")));
    assert_eq!(fixture.outer_copies.get(), 0, "the grid handled the key");
    assert_eq!(
        selected_keys(&fixture, cx),
        [1],
        "a caption takes no selection"
    );

    // The caption of the collapsed subgroup: its hidden row.
    assert!(fixture.update(cx, |grid, cx| grid.set_focused_group(&aapl_sell, cx)));
    fixture.press_copy(cx);
    assert_eq!(clipboard(cx), Some(trades.data(&[4]).join("\n")));

    // A collapsed group: every hidden row.
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(aapl.clone()), false, cx)
    });
    fixture.settle(cx);
    assert!(fixture.update(cx, |grid, cx| grid.set_focused_group(&aapl, cx)));
    fixture.press_copy(cx);
    assert_eq!(clipboard(cx), Some(trades.data(&[0, 8, 4]).join("\n")));

    // Back on a data row: the selection.
    assert!(fixture.update(cx, |grid, cx| grid.set_focused_cell(&2, None, cx)));
    fixture.press_copy(cx);
    assert_eq!(clipboard(cx), Some(trades.data(&[1]).join("\n")));
}

#[gpui_kit::test]
fn test_copy_caption_menu(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let fixture = open_trades(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument()).with_tuning(unthrottled())
    });
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(instrument("AAPL")), false, cx)
    });
    fixture.settle(cx);
    let trades = Trades::new(&rows);

    // The collapsed AAPL on line 0, MSFT on line 1. A right press focuses
    // the caption, and its Copy item copies the group's rows, the hidden
    // ones of a collapsed group too.
    fixture.right_press(cx, caption_point(0));
    fixture.click_item(cx, "Copy");
    assert_eq!(clipboard(cx), Some(trades.data(&[0, 4, 8]).join("\n")));
    fixture.right_press(cx, caption_point(1));
    fixture.click_item(cx, "Copy");
    assert_eq!(clipboard(cx), Some(trades.data(&[1, 5, 9]).join("\n")));

    // Copy all: the whole view, as `copy_all` writes it.
    fixture.right_press(cx, caption_point(1));
    fixture.click_item(cx, "Copy all");
    let all = clipboard(cx).expect("copied");
    assert_eq!(fixture.copied(cx, ExportScope::Visible), Some(all.clone()));
    let mut lines = all.lines();
    assert_eq!(lines.next(), Some(TRADE_HEADER));
    assert_eq!(
        lines.next(),
        Some(&*trades.caption(Instrument, "AAPL", &[0, 4, 8]))
    );
    assert_eq!(fixture.outer_copies.get(), 0);
}

#[gpui_kit::test]
fn test_copy_selection_in_collapsed_group(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let fixture = open_trades(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument())
            .with_tuning(unthrottled())
            .with_selection_mode(SelectionMode::RowSet)
    });
    // Row 4 of AAPL and row 6 of NVDA, then AAPL collapsed.
    fixture.update(cx, |grid, cx| grid.set_selected_keys([4, 6], cx));
    fixture.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(instrument("AAPL")), false, cx)
    });
    fixture.settle(cx);

    // The focus on a data row of another group: the command acts on the
    // selection, which keeps the row selected in the collapsed group, in the
    // order of the rows.
    assert!(fixture.update(cx, |grid, cx| grid.set_focused_cell(&2, None, cx)));
    fixture.press_copy(cx);
    assert_eq!(
        clipboard(cx),
        Some(Trades::new(&rows).data(&[4, 6]).join("\n"))
    );
}

/// The desk of a trade: its trader.
fn desk(row: &GroupedRow) -> CellValue<'_> {
    CellValue::Text(row.trader)
}

#[gpui_kit::test]
fn test_copy_caption_value_column(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let fixture = open_trades(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument()).with_tuning(unthrottled())
    });
    let by_instrument_lines = |trades: &Trades, column| {
        let mut lines = vec![trades.header()];
        for name in INSTRUMENTS {
            let ids = instrument_ids(name);
            lines.push(trades.caption(column, name, &ids));
            lines.extend(trades.data(&ids));
        }
        lines.join("\n")
    };

    // The level's column hidden: the key under the first copied column.
    assert!(fixture.update(cx, |grid, cx| {
        grid.set_column_visible(Instrument, false, cx)
    }));
    fixture.settle(cx);
    let hidden = Trades::new(&rows).with_columns(&[Id, Side, Qty, Price, Trader]);
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(by_instrument_lines(&hidden, Id))
    );

    // Shown: under its own column.
    assert!(fixture.update(cx, |grid, cx| grid.set_column_visible(Instrument, true, cx)));
    fixture.settle(cx);
    let trades = Trades::new(&rows);
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(by_instrument_lines(&trades, Instrument))
    );

    // A level by a `GroupKey`: under the first copied column too.
    let mut trader = ColumnSet::EMPTY;
    trader.insert(Trader.ix());
    fixture.update(cx, |grid, cx| {
        grid.set_view(
            by_instrument().with_group_by([GroupBy::key(GroupKey::new("desk", trader, desk))]),
            cx,
        )
    });
    fixture.settle(cx);
    let (ann, bob): (Vec<u32>, Vec<u32>) = ((0..8).collect(), (8..12).collect());
    let mut expected = vec![TRADE_HEADER.to_owned()];
    expected.push(trades.caption(Id, "Ann", &ann));
    expected.extend(trades.data(&ann));
    expected.push(trades.caption(Id, "Bob", &bob));
    expected.extend(trades.data(&bob));
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(expected.join("\n"))
    );
}

#[gpui_kit::test]
fn test_copy_tree_scopes(cx: &mut TestAppContext) {
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
    let fixture = open_trades(cx, rows, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Id, SortDirection::Ascending)])
                .with_tree(TreeSpec::new()),
        )
        .with_tuning(unthrottled())
        .with_exporter(LineShapes)
    });
    assert!(fixture.update(cx, |grid, cx| grid.set_expanded(
        ExpandTarget::Row(4),
        false,
        cx
    )));
    fixture.settle(cx);

    // As shown: no descendant of the collapsed 4. Each row at its depth in
    // the tree.
    let visible = [
        "header", "Data@0 0", "Data@1 1", "Data@2 3", "Data@1 2", "Data@0 4", "Data@0 8",
        "Data@0 9",
    ];
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(visible.join("\n"))
    );
    // Expanded: every row of the view.
    let expanded = [
        "header", "Data@0 0", "Data@1 1", "Data@2 3", "Data@1 2", "Data@0 4", "Data@1 5",
        "Data@2 7", "Data@1 6", "Data@0 8", "Data@0 9",
    ];
    assert_eq!(
        fixture.copied(cx, ExportScope::Expanded),
        Some(expanded.join("\n"))
    );
}

#[gpui_kit::test]
fn test_copy_nothing_passes_on(cx: &mut TestAppContext) {
    // An empty grid that shows a totals row without aggregates: nothing to
    // copy, so the Actions go on to the grid's ancestors.
    let empty = open_trades(cx, Vec::new(), |grid| {
        grid.with_view(ViewSpec::new())
            .with_tuning(unthrottled())
            .with_totals(PinnedEdge::Bottom)
    });
    assert_eq!(empty.copied(cx, ExportScope::Visible), None);
    assert_eq!(empty.copied(cx, ExportScope::Expanded), None);
    empty.dispatch(cx, &CopyAll);
    empty.press_copy(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("kept"));
    assert_eq!(empty.outer_copies.get(), 2, "both went on");

    // With aggregates the totals row holds values, those of no row: empty
    // sums and averages under the label.
    empty.update(cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new().with_aggregates([(Qty, Aggregate::Sum), (Price, Aggregate::Avg)]),
            cx,
        )
    });
    empty.settle(cx);
    assert_eq!(
        empty.copied(cx, ExportScope::Visible),
        Some(format!("{TRADE_HEADER}\nTotal\t\t\t\t\t"))
    );

    // The focus on a caption whose rows the stream has just removed, before
    // the projection drops the group: the command over it has no row, the
    // selected row elsewhere is not its scope, and the key goes on.
    let runner = PausableRunner::default();
    let grouped = open_trades(cx, grouped_rows(12), {
        let runner = runner.clone();
        move |grid| {
            grid.with_view(by_instrument())
                .with_tuning(unthrottled())
                .with_task_runner(runner)
        }
    });
    let aapl = instrument("AAPL");
    grouped.update(cx, |grid, cx| grid.set_selected_keys([1], cx));
    assert!(grouped.update(cx, |grid, cx| grid.set_focused_group(&aapl, cx)));
    runner.pause();
    let writer = grouped.writer(cx);
    for id in [0, 4, 8] {
        writer.remove(id);
    }
    grouped.settle(cx);
    let has_aapl = |cx: &mut TestAppContext| {
        grouped.grid.read_with(cx, |grid, _| {
            grid.projection().groups().find(&aapl).is_some()
        })
    };
    assert!(has_aapl(cx), "the projection still shows the caption");
    set_clipboard(cx, "kept");
    grouped.press_copy(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("kept"));
    assert_eq!(grouped.outer_copies.get(), 1, "the key went on");
    runner.resume();
    grouped.settle(cx);
    assert!(!has_aapl(cx));
}

#[gpui_kit::test]
fn test_copy_flat_unchanged(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });

    // The header, then every row; in a flat view every scope of the view
    // gives that text.
    let mut expected = vec![HEADER.to_string()];
    expected.extend(lines(&(0..10).rev().collect::<Vec<_>>(), 1.));
    let flat = expected.join("\n");
    assert!(fixture.update(cx, |grid, cx| grid.copy_all(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some(flat.as_str()));
    assert_eq!(fixture.copied(cx, ExportScope::Visible), Some(flat.clone()));
    assert_eq!(
        fixture.copied(cx, ExportScope::Expanded),
        Some(flat.clone())
    );

    // Aggregates alone add no line: no totals row is shown.
    fixture.update(cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new()
                .with_sort([(Col::Price, SortDirection::Descending)])
                .with_aggregates([(Col::Price, Aggregate::Max), (Col::Note, Aggregate::Count)]),
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(fixture.copied(cx, ExportScope::Visible), Some(flat.clone()));

    // The totals row, the label under the first column: right after the
    // header at the top, the last line at the bottom.
    let totals = "Total\t90.26\t\t10\t".to_string();
    fixture.update(cx, |grid, cx| grid.set_totals(Some(PinnedEdge::Top), cx));
    let mut top = expected.clone();
    top.insert(1, totals.clone());
    assert_eq!(
        fixture.copied(cx, ExportScope::Visible),
        Some(top.join("\n"))
    );
    fixture.update(cx, |grid, cx| grid.set_totals(Some(PinnedEdge::Bottom), cx));
    let mut bottom = expected;
    bottom.push(totals);
    assert_eq!(
        fixture.copied(cx, ExportScope::Expanded),
        Some(bottom.join("\n"))
    );

    // The command scope, as `copy`: no header, no totals.
    fixture.update(cx, |grid, cx| grid.set_selected_keys([3], cx));
    assert_eq!(
        fixture.copied(cx, ExportScope::Selection),
        Some(raw_line(3, 1.))
    );
}
