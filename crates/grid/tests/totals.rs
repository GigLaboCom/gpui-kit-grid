//! Headless UI tests of the totals row and group footers
//! (`.agents/COMPONENT_TEST_RULES.md`): the totals row's place in its pinned
//! band, its values over the view, live updates and their change paint, its
//! weight, label and `aggregate_style`; group footers on screen and under the
//! keyboard and the pointer; aggregate tooltips; autofit by aggregate texts;
//! and the accessibility rows of both. Small data: a dozen or two trades in
//! three or four groups. Assertions read the grid, the delegate's hook calls
//! and the planned accessibility tree, not pixels.

mod common;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use gpui_kit::component::Size;
use gpui_kit::{
    App, Context, Focusable as _, FontWeight, Modifiers, MouseButton, Pixels, TestAppContext, px,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    Aggregate, AggregateFn, AggregateLine, AggregateStyleContext, AutofitContext, AutofitPolicy,
    CellStyle, ChangeDirection, ChangePaint, DirectionalChanges, EveryGroupFooter, ExpandTarget,
    GridColumns as _, GroupBy, GroupPath, HighlightPolicy, LiveGridDelegate, LiveGridState,
    ManualReorder, OwnedValue, PinnedEdge, Projection, ProjectionEngine, Rebuild, RowChange,
    SelectionMode, StoreSnapshot, ViewSpec, VisibleRowsAutofit,
};

use crate::common::{
    GroupedColumn, GroupedFixture, GroupedRow, INSTRUMENTS, Item, ItemColumn, LineKind, ROW,
    a11y_row_tops, drag_move_to, footer_row_id, group_path, grouped_cell, grouped_rows,
    mouse_down_at, mouse_up_at, move_pointer_to, open_configured, open_grouped_with, press_at,
    totals_row_id, unthrottled,
};

use GroupedColumn::{Id, Instrument, Price, Qty};
use PinnedEdge::{Bottom, Top};

/// What a summary cell's line is, as `aggregate_style` saw it.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Caption(GroupPath),
    Footer(GroupPath),
    Totals(PinnedEdge),
}

/// One `aggregate_style` call: the line, the column, the value, the
/// highlighted change, and the weight and arrow the style had on entry.
#[derive(Clone, Debug)]
struct Styled {
    line: Line,
    column: GroupedColumn,
    value: Option<OwnedValue>,
    change: Option<(RowChange, Option<ChangeDirection>)>,
    weight: Option<FontWeight>,
    trend: Option<ChangeDirection>,
}

/// A delegate that records `aggregate_style`, and may set the totals in
/// the grid's weight.
#[derive(Clone, Default)]
struct Recording {
    styled: Rc<RefCell<Vec<Styled>>>,
    normal_totals: Rc<Cell<bool>>,
}

impl LiveGridDelegate for Recording {
    type Row = GroupedRow;

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, GroupedRow>,
        style: &mut CellStyle,
        _: &App,
    ) {
        let line = match cell.line() {
            AggregateLine::Caption(group) => Line::Caption(group.path().clone()),
            AggregateLine::Footer(group) => Line::Footer(group.path().clone()),
            AggregateLine::Totals(edge) => Line::Totals(edge),
            line => panic!("an unknown line {line:?}"),
        };
        assert!(
            matches!(line, Line::Caption(_)) || !cell.is_focused(),
            "footers and totals are never focused"
        );
        let entry = style.font_weight();
        if self.normal_totals.get() && matches!(line, Line::Totals(_)) {
            style.set_font_weight(FontWeight::NORMAL);
        }
        self.styled.borrow_mut().push(Styled {
            line,
            column: cell.column(),
            value: cell.value().map(OwnedValue::from_cell),
            change: cell
                .change()
                .map(|change| (change.change(), change.direction())),
            weight: entry,
            trend: style.trend(),
        });
    }
}

impl Recording {
    /// The cells of `line` in the next frame, left to right: the last
    /// call for each column (a repaint may draw more than one frame).
    fn next_frame(
        &self,
        fixture: &GroupedFixture<Recording>,
        cx: &mut TestAppContext,
        line: &Line,
    ) -> Vec<Styled> {
        self.styled.borrow_mut().clear();
        fixture.grid.update(cx, |_, cx| cx.notify());
        fixture.frame(cx, Duration::from_millis(20));
        let mut cells: Vec<Styled> = Vec::new();
        for cell in self
            .styled
            .borrow()
            .iter()
            .filter(|cell| cell.line == *line)
        {
            match cells.iter_mut().find(|each| each.column == cell.column) {
                Some(each) => *each = cell.clone(),
                None => cells.push(cell.clone()),
            }
        }
        cells
    }

    /// The value of each column of `line` in the next frame.
    fn values(
        &self,
        fixture: &GroupedFixture<Recording>,
        cx: &mut TestAppContext,
        line: &Line,
    ) -> Vec<(GroupedColumn, Option<OwnedValue>)> {
        self.next_frame(fixture, cx, line)
            .into_iter()
            .map(|cell| (cell.column, cell.value))
            .collect()
    }
}

fn open_recording(
    cx: &mut TestAppContext,
    rows: Vec<GroupedRow>,
    configure: impl FnOnce(LiveGridState<Recording>) -> LiveGridState<Recording> + 'static,
) -> (GroupedFixture<Recording>, Recording) {
    let recording = Recording::default();
    let fixture = open_grouped_with(cx, rows, recording.clone(), configure);
    (fixture, recording)
}

/// Twelve trades in three instruments of four, ids in order within them:
/// AAPL 0, 3, 6, 9; MSFT 1, 4, 7, 10; NVDA 2, 5, 8, 11.
fn three_groups() -> Vec<GroupedRow> {
    grouped_rows(12)
        .into_iter()
        .map(|row| GroupedRow {
            instrument: INSTRUMENTS[row.id as usize % 3],
            ..row
        })
        .collect()
}

fn by_id() -> ViewSpec<GroupedRow> {
    ViewSpec::new().with_sort([(Id, gpui_kit_grid::SortDirection::Ascending)])
}

fn by_instrument() -> ViewSpec<GroupedRow> {
    by_id().with_group_by([GroupBy::column(Instrument)])
}

fn instrument(name: &str) -> GroupPath {
    group_path(&[(Instrument, name)])
}

fn sum_qty(rows: &[GroupedRow], keep: impl Fn(&GroupedRow) -> bool) -> OwnedValue {
    OwnedValue::Int(rows.iter().filter(|row| keep(row)).map(|row| row.qty).sum())
}

fn update<T>(
    fixture: &GroupedFixture<Recording>,
    cx: &mut TestAppContext,
    f: impl FnOnce(&mut LiveGridState<Recording>, &mut Context<LiveGridState<Recording>>) -> T,
) -> T {
    fixture.grid.update(cx, f)
}

#[gpui_kit::test]
fn test_totals_builder(cx: &mut TestAppContext) {
    let (fixture, _) = open_recording(cx, grouped_rows(4), |grid| grid.with_totals(Top));
    fixture.events.borrow_mut().clear();
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.totals()),
        Some(Top)
    );
    update(&fixture, cx, |grid, cx| grid.set_totals(None, cx));
    assert_eq!(fixture.grid.read_with(cx, |grid, _| grid.totals()), None);
    update(&fixture, cx, |grid, cx| grid.set_totals(Some(Bottom), cx));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.totals()),
        Some(Bottom)
    );
    // No event: where the totals show is not part of the view.
    assert!(fixture.events.borrow().is_empty());
}

#[gpui_kit::test]
fn test_totals_band_placement(cx: &mut TestAppContext) {
    // 20 items, a header and ten `xsmall` lines; one application row in
    // each band.
    let fixture = open_configured(cx, 20, Some(Size::XSmall), |grid| {
        grid.with_view(ViewSpec::new().with_aggregates([(ItemColumn::Price, Aggregate::Sum)]))
            .with_tuning(unthrottled())
            .with_totals(Bottom)
    });
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    let writer = fixture.writer(cx);
    writer.set_pinned_rows(Top, [Arc::new(Item { id: 1000, price: 1 })]);
    writer.set_pinned_rows(Bottom, [Arc::new(Item { id: 2000, price: 2 })]);
    fixture.settle(cx);
    let line_top = |line: f32| px(ROW * (line + 1.));
    let pinned_keys = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            (grid.pinned_keys(Top), grid.pinned_keys(Bottom))
        })
    };

    // At the bottom: the last line of the bottom band, after its
    // application row; one data row fewer on screen.
    assert_eq!(fixture.visible_keys(cx), (0..7).collect::<Vec<_>>());
    assert_eq!(pinned_keys(cx), (vec![1000], vec![2000]));
    let rows = a11y_row_tops(cx, fixture.window);
    let last = rows.len() - 1;
    assert_eq!(rows[last], (totals_row_id(Bottom), line_top(9.)));
    assert_eq!(rows[last - 1].1, line_top(8.), "the application row");

    // At the top: right under the header, before the application row.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_totals(Some(Top), cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..7).collect::<Vec<_>>());
    let rows = a11y_row_tops(cx, fixture.window);
    assert_eq!(rows[1], (totals_row_id(Top), line_top(0.)));
    assert_eq!(rows[2].1, line_top(1.), "the application row");
    assert_eq!(rows.last().map(|row| row.1), Some(line_top(9.)));

    // None: the row goes and the data rows get their line back.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_totals(None, cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..8).collect::<Vec<_>>());
    let rows = a11y_row_tops(cx, fixture.window);
    assert!(!rows.iter().any(|(id, _)| *id == totals_row_id(Top)));

    // A body shorter than the bands: eight lines on top leave two lines at
    // the bottom, laid out from the body's edge. The totals stay there; the
    // band loses the rows next to the data rows.
    writer.set_pinned_rows(Top, (100..108).map(|id| Arc::new(Item { id, price: 0 })));
    writer.set_pinned_rows(Bottom, (200..203).map(|id| Arc::new(Item { id, price: 0 })));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_totals(Some(Bottom), cx));
    fixture.settle(cx);
    assert!(fixture.visible_keys(cx).is_empty());
    assert_eq!(pinned_keys(cx), ((100..108).collect(), vec![202]));
    let rows = a11y_row_tops(cx, fixture.window);
    assert_eq!(rows.last(), Some(&(totals_row_id(Bottom), line_top(9.))));
}

#[gpui_kit::test]
fn test_totals_values_follow_the_view(cx: &mut TestAppContext) {
    let rows = grouped_rows(24);
    let (fixture, recording) = open_recording(cx, rows.clone(), |grid| {
        grid.with_view(by_instrument().with_aggregates([
            (Qty, Aggregate::Sum),
            (Price, Aggregate::Avg),
            (Id, Aggregate::Count),
        ]))
        .with_tuning(unthrottled())
        .with_totals(Bottom)
    });
    update(&fixture, cx, |grid, cx| {
        grid.set_a11y_forced(true);
        grid.set_expanded(ExpandTarget::Group(instrument("MSFT")), false, cx)
    });
    fixture.settle(cx);
    let totals = Line::Totals(Bottom);

    // Every row the view passes, the collapsed group's included.
    let values = recording.values(&fixture, cx, &totals);
    let value = |values: &[(GroupedColumn, Option<OwnedValue>)], column| {
        values
            .iter()
            .find(|(each, _)| *each == column)
            .and_then(|(_, value)| value.clone())
    };
    assert_eq!(value(&values, Qty), Some(sum_qty(&rows, |_| true)));
    assert_eq!(value(&values, Id), Some(OwnedValue::Int(24)));
    let mean = rows.iter().map(|row| row.price).sum::<f64>() / 24.;
    match value(&values, Price) {
        Some(OwnedValue::Float(avg)) => assert!((avg - mean).abs() < 1e-9, "{avg} vs {mean}"),
        other => panic!("an average, not {other:?}"),
    }
    // Every painted column is styled; those without an aggregate have no
    // value.
    assert_eq!(values.len(), GroupedColumn::all().len());
    assert_eq!(value(&values, Instrument), None);

    // A quick filter narrows the view: the totals follow.
    update(&fixture, cx, |grid, cx| grid.set_quick_filter("AAPL", cx));
    fixture.settle(cx);
    let values = recording.values(&fixture, cx, &totals);
    assert_eq!(
        value(&values, Qty),
        Some(sum_qty(&rows, |row| row.instrument == "AAPL"))
    );
    assert_eq!(value(&values, Id), Some(OwnedValue::Int(6)));

    // Without aggregates the row stays, with its label alone.
    update(&fixture, cx, |grid, cx| {
        grid.set_quick_filter("", cx);
        grid.set_aggregates([], cx)
    });
    fixture.settle(cx);
    let values = recording.values(&fixture, cx, &totals);
    assert_eq!(values.len(), GroupedColumn::all().len());
    assert!(values.iter().all(|(_, value)| value.is_none()));
    let cells = fixture
        .grid
        .read_with(cx, |grid, _| grid.a11y_row_cells(&totals_row_id(Bottom)));
    assert_eq!(cells, [("Total".to_owned(), Some("id".to_owned()))]);
}

/// The default engine, with the change stamps of aggregates the view puts
/// on its projections, at the test's clock.
struct Stamping {
    now_ms: Arc<AtomicU32>,
}

impl ProjectionEngine<GroupedRow> for Stamping {
    fn rebuild(
        &self,
        snapshot: &StoreSnapshot<GroupedRow>,
        spec: &ViewSpec<GroupedRow>,
    ) -> Projection {
        Rebuild::default().rebuild(snapshot, spec)
    }

    fn aggregate(
        &self,
        previous: &Projection,
        snapshot: &StoreSnapshot<GroupedRow>,
        spec: &ViewSpec<GroupedRow>,
    ) -> Projection {
        let mut projection = Rebuild::default().aggregate(previous, snapshot, spec);
        projection.carry_summary_stamps(
            previous,
            self.now_ms.load(Ordering::Relaxed),
            HighlightPolicy::default(),
        );
        projection
    }
}

#[gpui_kit::test]
fn test_totals_update_without_rebuild(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let now_ms = Arc::new(AtomicU32::new(0));
    let (fixture, recording) = open_recording(cx, rows.clone(), {
        let now_ms = now_ms.clone();
        move |grid| {
            grid.with_view(by_id().with_aggregates([(Qty, Aggregate::Sum)]))
                .with_engine(Stamping { now_ms })
                .with_tuning(unthrottled())
                .with_change_paint(ChangePaint::Directional(DirectionalChanges))
                .with_totals(Bottom)
        }
    });
    let set_clock = |cx: &mut TestAppContext| {
        let now = fixture.grid.read_with(cx, |grid, _| {
            grid.store()
                .now()
                .saturating_duration_since(grid.snapshot().epoch())
                .as_millis() as u32
        });
        now_ms.store(now, Ordering::Relaxed);
    };
    let qty_cell = |cx: &mut TestAppContext| {
        recording
            .next_frame(&fixture, cx, &Line::Totals(Bottom))
            .into_iter()
            .find(|cell| cell.column == Qty)
            .expect("the Qty cell of the totals")
    };
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());
    let total = rows.iter().map(|row| row.qty).sum::<i64>();
    let cell = qty_cell(cx);
    assert_eq!(cell.value, Some(OwnedValue::Int(total)));
    assert_eq!(cell.change, None);

    // The stream changes a quantity: no sort or grouping reads it, so the
    // aggregate job recounts the totals and nothing is rebuilt. The cell
    // is highlighted, rising, with its arrow.
    set_clock(cx);
    let writer = fixture.writer(cx);
    writer.upsert(GroupedRow {
        qty: rows[3].qty + 1000,
        ..rows[3].clone()
    });
    fixture.settle(cx);
    let cell = qty_cell(cx);
    assert_eq!(cell.value, Some(OwnedValue::Int(total + 1000)));
    assert_eq!(
        cell.change,
        Some((RowChange::Updated, Some(ChangeDirection::Up)))
    );
    assert_eq!(cell.trend, Some(ChangeDirection::Up));
    assert_eq!(cell.weight, Some(FontWeight::SEMIBOLD));
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds()),
        rebuilds
    );

    // A held order (an insert under `ManualReorder`): the totals of the
    // order on screen still follow the values.
    update(&fixture, cx, |grid, cx| {
        grid.set_reorder_policy(ManualReorder, cx)
    });
    writer.upsert(GroupedRow {
        id: 100,
        ..rows[0].clone()
    });
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    set_clock(cx);
    writer.upsert(GroupedRow {
        qty: rows[4].qty - 50,
        ..rows[4].clone()
    });
    fixture.settle(cx);
    let cell = qty_cell(cx);
    assert_eq!(cell.value, Some(OwnedValue::Int(total + 1000 - 50)));
    assert_eq!(
        cell.change,
        Some((RowChange::Updated, Some(ChangeDirection::Down)))
    );
    assert_eq!(cell.trend, Some(ChangeDirection::Down));
}

#[gpui_kit::test]
fn test_totals_style_and_label(cx: &mut TestAppContext) {
    let rows = grouped_rows(12);
    let (fixture, recording) = open_recording(cx, rows.clone(), |grid| {
        grid.with_view(by_id().with_aggregates([(Qty, Aggregate::Sum)]))
            .with_tuning(unthrottled())
            .with_totals(Bottom)
    });
    update(&fixture, cx, |grid, _| grid.set_a11y_forced(true));
    fixture.settle(cx);
    let totals = Line::Totals(Bottom);
    let sum = match sum_qty(&rows, |_| true) {
        OwnedValue::Int(sum) => sum.to_string(),
        _ => unreachable!(),
    };
    let a11y = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            (
                grid.a11y_row_label(&totals_row_id(Bottom)),
                grid.a11y_row_cells(&totals_row_id(Bottom)),
            )
        })
    };

    // Semibold by default, every cell; the label sits in the first visible
    // column, which has no aggregate; the row is named after it.
    let cells = recording.next_frame(&fixture, cx, &totals);
    assert!(
        cells
            .iter()
            .all(|cell| cell.weight == Some(FontWeight::SEMIBOLD))
    );
    assert!(cells.iter().all(|cell| cell.change.is_none()));
    assert_eq!(
        cells
            .iter()
            .find(|cell| cell.column == Id)
            .map(|cell| &cell.value),
        Some(&None),
        "the label has no value"
    );
    assert_eq!(
        a11y(cx),
        (
            Some("Total".to_owned()),
            vec![
                ("Total".to_owned(), Some("id".to_owned())),
                (sum.clone(), Some("Sum, qty".to_owned())),
            ]
        )
    );
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.text_stats().bold_entries())
            > 0
    );

    // The delegate may take the weight back: the totals leave the bold
    // text cache, which drops them after some frames unused.
    recording.normal_totals.set(true);
    for _ in 0..200 {
        recording.next_frame(&fixture, cx, &totals);
    }
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.text_stats().bold_entries()),
        0
    );
    recording.normal_totals.set(false);

    // An aggregate in the first column: no label; the row keeps its name.
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates([(Id, Aggregate::Count), (Qty, Aggregate::Sum)], cx)
    });
    fixture.settle(cx);
    assert_eq!(
        a11y(cx),
        (
            Some("Total".to_owned()),
            vec![
                ("12".to_owned(), Some("Count, id".to_owned())),
                (sum.clone(), Some("Sum, qty".to_owned())),
            ]
        )
    );

    // The first visible column hidden: the label moves to the next one.
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates([(Qty, Aggregate::Sum)], cx);
        grid.set_column_visible(Id, false, cx);
    });
    fixture.settle(cx);
    assert_eq!(
        a11y(cx).1,
        [
            ("Total".to_owned(), Some("instrument".to_owned())),
            (sum, Some("Sum, qty".to_owned())),
        ]
    );
}

#[gpui_kit::test]
fn test_group_footers(cx: &mut TestAppContext) {
    let rows = three_groups();
    let (fixture, recording) = open_recording(cx, rows.clone(), |grid| {
        grid.with_view(
            by_instrument()
                .with_aggregates([(Qty, Aggregate::Sum)])
                .with_group_footer(EveryGroupFooter),
        )
        .with_tuning(unthrottled())
        .with_selection_mode(SelectionMode::RowSet)
    });
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(instrument("MSFT")), false, cx)
    });
    fixture.settle(cx);
    let (aapl, msft, nvda) = (instrument("AAPL"), instrument("MSFT"), instrument("NVDA"));

    // A footer after the rows of each expanded group; none for the
    // collapsed one. Footers are lines of the projection.
    assert_eq!(
        fixture.lines(cx),
        [
            LineKind::Caption(aapl.clone()),
            LineKind::Data(0),
            LineKind::Data(3),
            LineKind::Data(6),
            LineKind::Data(9),
            LineKind::Footer(aapl.clone()),
            LineKind::Caption(msft.clone()),
            LineKind::Caption(nvda.clone()),
            LineKind::Data(2),
            LineKind::Data(5),
            LineKind::Data(8),
            LineKind::Data(11),
            LineKind::Footer(nvda.clone()),
        ]
    );
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.projection().len()),
        13
    );
    // The footer's cells: the group's aggregates, in the grid's weight.
    let cells = recording.next_frame(&fixture, cx, &Line::Footer(aapl.clone()));
    let qty = cells.iter().find(|cell| cell.column == Qty).expect("Qty");
    assert_eq!(
        qty.value,
        Some(sum_qty(&rows, |row| row.instrument == "AAPL"))
    );
    assert_eq!(qty.weight, None);
    assert_eq!(cells.len(), GroupedColumn::all().len());

    // ↓ from the last row of a group steps over its footer to the next
    // caption.
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
    });
    update(&fixture, cx, |grid, cx| grid.select_key(Some(9), cx));
    fixture.settle(cx);
    fixture.input(cx, |window, cx| window.press("down", cx));
    fixture.settle(cx);
    let focus = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.focused_cell()
                .map(|cell| (cell.row().copied(), cell.group().cloned()))
        })
    };
    assert_eq!(focus(cx), Some((None, Some(msft.clone()))));

    // A press on a footer changes neither the selection nor the focus.
    update(&fixture, cx, |grid, cx| grid.select_key(Some(0), cx));
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            grouped_cell(5, Qty),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    let selected = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
            keys.sort_unstable();
            keys
        })
    };
    assert_eq!(selected(cx), [0]);
    assert_eq!(focus(cx), Some((Some(0), None)));
    assert!(fixture.take_seen().is_empty());

    // A drag-selection over the footer and the captions takes the data
    // rows on screen alone.
    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            grouped_cell(3, Qty),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
        drag_move_to(window, grouped_cell(5, Qty), cx);
        drag_move_to(window, grouped_cell(8, Qty), cx);
        mouse_up_at(
            window,
            grouped_cell(8, Qty),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    });
    fixture.settle(cx);
    assert_eq!(selected(cx), [2, 6, 9]);

    // Collapsing a group takes its footer away.
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(aapl.clone()), false, cx)
    });
    fixture.settle(cx);
    assert!(!fixture.lines(cx).contains(&LineKind::Footer(aapl)));
}

/// A custom aggregate without a name: the number of rows.
struct Rows;

impl AggregateFn<GroupedRow> for Rows {
    fn aggregate(
        &self,
        _: GroupedColumn,
        rows: &mut dyn Iterator<Item = &GroupedRow>,
    ) -> OwnedValue {
        OwnedValue::Int(rows.count() as i64)
    }
}

#[gpui_kit::test]
fn test_aggregate_tooltip(cx: &mut TestAppContext) {
    let rows = three_groups();
    let (fixture, _) = open_recording(cx, rows.clone(), |grid| {
        grid.with_view(
            by_instrument()
                .with_aggregates([
                    (Qty, Aggregate::Sum),
                    (Price, Aggregate::Custom(Arc::new(Rows))),
                ])
                .with_group_footer(EveryGroupFooter),
        )
        .with_tuning(unthrottled())
        .with_totals(Bottom)
    });
    let tooltip_at_point = |cx: &mut TestAppContext, point: gpui_kit::Point<Pixels>| {
        fixture.input(cx, |window, cx| move_pointer_to(window, point, cx));
        fixture.settle(cx);
        fixture
            .grid
            .read_with(cx, |grid, _| grid.hovered_tooltip_text())
            .map(|text| text.to_string())
    };
    let tooltip_at = |cx: &mut TestAppContext, line: usize, column: GroupedColumn| {
        tooltip_at_point(cx, grouped_cell(line, column))
    };
    let total = match sum_qty(&rows, |_| true) {
        OwnedValue::Int(sum) => sum,
        _ => unreachable!(),
    };
    let aapl = match sum_qty(&rows, |row| row.instrument == "AAPL") {
        OwnedValue::Int(sum) => sum,
        _ => unreachable!(),
    };

    // The totals row is screen line 9; the name and the full value.
    assert_eq!(tooltip_at(cx, 9, Qty), Some(format!("Sum: {total}")));
    // A custom aggregate without a name: the value alone.
    assert_eq!(tooltip_at(cx, 9, Price), Some("12".to_owned()));
    // A footer (AAPL's, line 5) as the totals.
    assert_eq!(tooltip_at(cx, 5, Qty), Some(format!("Sum: {aapl}")));
    // A cell without an aggregate has none; the label only when cut.
    assert_eq!(tooltip_at(cx, 5, GroupedColumn::Side), None);
    let label = gpui_kit::point(px(8.), px(ROW * 10.5));
    update(&fixture, cx, |grid, cx| {
        grid.set_column_width(Id, px(160.), cx)
    });
    fixture.settle(cx);
    assert_eq!(tooltip_at_point(cx, label), None);
    update(&fixture, cx, |grid, cx| {
        grid.set_column_width(Id, px(16.), cx)
    });
    fixture.settle(cx);
    assert_eq!(tooltip_at_point(cx, label), Some("Total".to_owned()));
}

/// The standard fit, recording what it was given.
#[derive(Clone, Default)]
struct RecordedFit {
    summary: Rc<Cell<Pixels>>,
    content: Rc<Cell<Pixels>>,
}

impl AutofitPolicy for RecordedFit {
    fn content_width(&self, context: &mut AutofitContext<'_>) -> Pixels {
        let content = VisibleRowsAutofit.content_width(context);
        self.summary.set(context.summary_width());
        self.content.set(content);
        content
    }
}

#[gpui_kit::test]
fn test_autofit_measures_summaries(cx: &mut TestAppContext) {
    // Large quantities: their sum is the longest text of the column.
    let rows: Vec<GroupedRow> = grouped_rows(12)
        .into_iter()
        .map(|row| GroupedRow {
            qty: 123_456_789_012,
            ..row
        })
        .collect();
    let fit = RecordedFit::default();
    let (fixture, _) = open_recording(cx, rows, {
        let fit = fit.clone();
        move |grid| {
            grid.with_view(by_id().with_aggregates([(Qty, Aggregate::Sum)]))
                .with_tuning(unthrottled())
                .with_autofit_policy(fit)
                .with_totals(Bottom)
        }
    });
    update(&fixture, cx, |grid, cx| {
        grid.set_column_width(Qty, px(30.), cx);
        grid.autofit_column(Qty, cx);
    });
    fixture.settle(cx);
    // The policy saw the total, in its weight, wider than every row: the
    // column fits it with its padding.
    let summary = fit.summary.get();
    assert!(summary > px(0.));
    assert_eq!(fit.content.get(), summary);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.column_width(Qty)),
        summary.ceil() + px(13.)
    );

    // Without totals there is nothing to measure there.
    update(&fixture, cx, |grid, cx| {
        grid.set_totals(None, cx);
        grid.autofit_column(Qty, cx);
    });
    fixture.settle(cx);
    assert_eq!(fit.summary.get(), px(0.));
    assert!(fit.content.get() < summary);
}

#[gpui_kit::test]
fn test_totals_a11y(cx: &mut TestAppContext) {
    let rows = three_groups();
    let (fixture, _) = open_recording(cx, rows.clone(), |grid| {
        grid.with_view(
            by_instrument()
                .with_aggregates([(Qty, Aggregate::Sum)])
                .with_group_footer(EveryGroupFooter),
        )
        .with_tuning(unthrottled())
        .with_totals(Bottom)
    });
    update(&fixture, cx, |grid, cx| {
        grid.set_a11y_forced(true);
        grid.set_expanded(ExpandTarget::Group(instrument("MSFT")), false, cx)
    });
    fixture.settle(cx);
    let text = |value: OwnedValue| match value {
        OwnedValue::Int(value) => value.to_string(),
        _ => unreachable!(),
    };

    // The totals row last, at the bottom; the footer of AAPL on its line,
    // screen line 5.
    let tops = a11y_row_tops(cx, fixture.window);
    assert_eq!(tops.last(), Some(&(totals_row_id(Bottom), px(ROW * 10.))));
    let aapl = instrument("AAPL");
    assert!(tops.contains(&(footer_row_id(&aapl), px(ROW * 6.))));
    let (totals, footer, label) = fixture.grid.read_with(cx, |grid, _| {
        (
            grid.a11y_row_cells(&totals_row_id(Bottom)),
            grid.a11y_row_cells(&footer_row_id(&aapl)),
            grid.a11y_row_label(&footer_row_id(&aapl)),
        )
    });
    assert_eq!(
        totals,
        [
            ("Total".to_owned(), Some("id".to_owned())),
            (text(sum_qty(&rows, |_| true)), Some("Sum, qty".to_owned())),
        ]
    );
    // A footer: the aggregate cells alone, no label, no name.
    assert_eq!(
        footer,
        [(
            text(sum_qty(&rows, |row| row.instrument == "AAPL")),
            Some("Sum, qty".to_owned())
        )]
    );
    assert_eq!(label, None);
    // Rows: the header, the nine lines on screen, the totals.
    assert_eq!(tops.len(), 11);
}
