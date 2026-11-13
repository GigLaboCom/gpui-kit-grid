//! Headless UI tests of saved layouts (`.agents/COMPONENT_TEST_RULES.md`):
//! export and restore, `with_layout` in any builder order, the events, the
//! report and the rebuilds of `set_layout`, the sort of a restored layout over
//! a held order, view ids and versions, the `ResetLayout` Action, a grid
//! outside any window, and when a saving subscriber is told to save.
//!
//! The grouping and the expansion of groups in a layout: what `layout()`
//! writes, a restore and its events, a layout of another schema, the reset to
//! the view's grouping, the builder order and a tree.

mod common;

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::Sizable as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, Focusable as _, IntoElement,
    Modifiers, MouseButton, ParentElement as _, Pixels, Render, Styled as _, TestAppContext,
    Window, WindowBounds, WindowOptions, div, point, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellValue, ColumnBand, ColumnIx, ColumnSet, ColumnZone, ExpandDepth, ExpandTarget, FreeMove,
    GridColumn, GridColumns, GridLayout, GridRow, GridSchema, GridTuning, GroupBy, GroupKey,
    GroupOrigin, GroupPath, JsonLayoutCodec, LayoutCodec as _, LayoutColumn, LayoutError,
    LayoutExpansion, LayoutGroup, LayoutMerge, LayoutMergeContext, LayoutOrigin, LayoutPart,
    LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, ManualReorder, OwnedValue,
    SortDirection, SortOrigin, StoreOptions, TreeSpec, ViewSpec, actions::ResetLayout,
};

use crate::common::{
    Grouped, GroupedColumn, GroupedFixture, GroupedRow, LineKind, drag_move_to, group_path,
    grouped_rows, grouped_schema, mouse_down_at, mouse_up_at, open_grouped, unthrottled,
};

use SortDirection::{Ascending, Descending};

const ROW: f32 = 26.;
/// One band level over the columns: two header rows.
const HEADER: f32 = ROW * 2.;
const WIDTH: f32 = 400.;
/// The header plus ten rows at the `xsmall` size.
const HEIGHT: f32 = HEADER + ROW * 10.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Col {
    Id,
    Symbol,
    Bid,
    Ask,
    Size,
    Note,
    Total,
}

impl GridColumns for Col {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Symbol,
            Self::Bid,
            Self::Ask,
            Self::Size,
            Self::Note,
            Self::Total,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Symbol => "symbol",
            Self::Bid => "bid",
            Self::Ask => "ask",
            Self::Size => "size",
            Self::Note => "note",
            Self::Total => "total",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone, Debug)]
struct Quote {
    id: u32,
    bid: i64,
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
            Col::Symbol => CellValue::Text("SBER"),
            Col::Bid => CellValue::Int(self.bid),
            Col::Ask => CellValue::Int(self.bid + 1),
            Col::Size => CellValue::Int(100),
            Col::Note => CellValue::Text("Pending"),
            Col::Total => CellValue::Int(0),
        }
    }
}

struct Quotes;

impl LiveGridDelegate for Quotes {
    type Row = Quote;
}

/// Id frozen left (60 px); Symbol 80, band "quote" over Bid 70 and Ask 70,
/// Size 70, Note 100 scrolling; Total frozen right (60).
fn schema() -> GridSchema<Col> {
    GridSchema::new()
        .column(
            GridColumn::new(Col::Id, "Id")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenLeft),
        )
        .column(GridColumn::new(Col::Symbol, "Symbol").with_width(px(80.)))
        .band(
            ColumnBand::new("quote", "Quote")
                .column(GridColumn::new(Col::Bid, "Bid").with_width(px(70.)))
                .column(GridColumn::new(Col::Ask, "Ask").with_width(px(70.))),
        )
        .column(GridColumn::new(Col::Size, "Size").with_width(px(70.)))
        .column(GridColumn::new(Col::Note, "Note").with_width(px(100.)))
        .column(
            GridColumn::new(Col::Total, "Total")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenRight),
        )
}

struct Root {
    grid: Entity<LiveGridState<Quotes>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(LiveGrid::new(&self.grid).xsmall().bordered(false))
    }
}

type Events = Rc<RefCell<Vec<LiveGridEvent<u32, Col>>>>;

/// The events a layout test follows, in order.
#[derive(Debug, PartialEq)]
enum Seen {
    Sort(Vec<(Col, SortDirection)>, SortOrigin),
    Layout(LayoutOrigin),
    Held(bool),
}

/// A grid of `count` rows (`bid = id * 10`) outside any window.
fn new_grid(
    cx: &mut TestAppContext,
    count: u32,
    configure: impl FnOnce(LiveGridState<Quotes>) -> LiveGridState<Quotes>,
) -> (Entity<LiveGridState<Quotes>>, Events) {
    cx.update(gpui_kit::init);
    let grid = cx.new(|cx| {
        configure(
            LiveGridState::new_standalone(schema(), Quotes, StoreOptions::default(), cx)
                .with_tuning(GridTuning::default().with_min_rebuild_interval(Duration::ZERO)),
        )
    });
    let events = Events::default();
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<u32, Col>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all((0..count).map(|id| {
        Arc::new(Quote {
            id,
            bid: i64::from(id) * 10,
        })
    }));
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    (grid, events)
}

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Quotes>>,
    events: Events,
}

/// Shows `grid` in a 400 px window.
fn show(cx: &mut TestAppContext, grid: Entity<LiveGridState<Quotes>>, events: Events) -> Fixture {
    let (window, _) = cx
        .update(|cx| {
            let grid = grid.clone();
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(WIDTH), px(HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| cx.new(|_| Root { grid }),
            )
        })
        .expect("open window");
    let fixture = Fixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    fixture
}

fn open(
    cx: &mut TestAppContext,
    count: u32,
    configure: impl FnOnce(LiveGridState<Quotes>) -> LiveGridState<Quotes>,
) -> Fixture {
    let (grid, events) = new_grid(cx, count, configure);
    show(cx, grid, events)
}

fn sorted_by(column: Col, direction: SortDirection) -> ViewSpec<Quote> {
    ViewSpec::new().with_sort([(column, direction)])
}

fn ids(ids: &[gpui_kit_grid::ColumnId]) -> Vec<&str> {
    ids.iter().map(|id| id.as_str()).collect()
}

fn set(columns: impl IntoIterator<Item = Col>) -> ColumnSet {
    columns
        .into_iter()
        .fold(ColumnSet::EMPTY, |mut set, column| {
            set.insert(column.ix());
            set
        })
}

/// `layout` with the width of `id` replaced.
fn with_width(layout: &GridLayout, id: &str, width: f32) -> GridLayout {
    layout
        .columns()
        .iter()
        .map(|column| {
            if column.id().as_str() == id {
                column.clone().with_width(width)
            } else {
                column.clone()
            }
        })
        .fold(
            layout.view_id().map_or_else(GridLayout::new, |view_id| {
                GridLayout::new().with_view_id(view_id)
            }),
            GridLayout::with_column,
        )
        .with_sort(layout.sort().to_vec())
}

impl Fixture {
    /// Runs ingest, pulls and background jobs, then draws a frame.
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

    /// The bounds of a header cell, if the column is shown.
    fn header_cell(&self, cx: &mut TestAppContext, column: Col) -> Option<Bounds<Pixels>> {
        self.input(cx, |window, _| {
            window
                .within("live-grid-header")
                .try_find(column.id())
                .map(|cell| cell.bounds())
        })
    }

    fn layout(&self, cx: &mut TestAppContext) -> GridLayout {
        self.grid.read_with(cx, |grid, _| grid.layout())
    }

    fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    fn set_layout(
        &self,
        cx: &mut TestAppContext,
        layout: &GridLayout,
    ) -> Result<gpui_kit_grid::LayoutReport, LayoutError> {
        self.grid.update(cx, |grid, cx| grid.set_layout(layout, cx))
    }

    /// The followed events so far, then forgets them.
    fn take_events(&self) -> Vec<Seen> {
        take_events(&self.events)
    }
}

fn take_events(events: &Events) -> Vec<Seen> {
    std::mem::take(&mut *events.borrow_mut())
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::SortChange { sort, origin } => Some(Seen::Sort(sort.to_vec(), origin)),
            LiveGridEvent::LayoutChange { origin, .. } => Some(Seen::Layout(origin)),
            LiveGridEvent::OrderHeldChange { held } => Some(Seen::Held(held)),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn test_layout_export_import_round_trip(cx: &mut TestAppContext) {
    let source = open(cx, 30, |grid| grid);
    source.grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Symbol, px(120.), cx);
        grid.move_column(Col::Size, Some(Col::Symbol), cx);
        grid.set_column_zone(Col::Note, ColumnZone::FrozenLeft, cx);
        grid.set_column_visible(Col::Ask, false, cx);
        grid.move_band("quote", Some(Col::Size), cx);
        grid.set_sort([(Col::Bid, Descending), (Col::Id, Ascending)], cx);
    });
    source.settle(cx);
    let layout = source.layout(cx);
    let order: Vec<&str> = layout.columns().iter().map(|c| c.id().as_str()).collect();
    assert_eq!(
        order,
        ["id", "note", "bid", "ask", "size", "symbol", "total"]
    );
    assert_eq!(layout.column("symbol").map(LayoutColumn::width), Some(120.));
    assert!(!layout.column("ask").unwrap().is_visible());
    assert_eq!(source.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());

    // Through text and back into a new grid. Its own view's sort is replaced
    // by the layout's; the default layout keeps the view's sort.
    let text = source
        .grid
        .read_with(cx, |grid, _| grid.export_layout())
        .unwrap();
    let decoded = JsonLayoutCodec::default().decode(&text).unwrap();
    assert_eq!(decoded, layout);
    let restored = open(cx, 30, move |grid| {
        grid.with_view(sorted_by(Col::Size, Ascending))
            .with_layout(decoded)
    });
    assert_eq!(restored.layout(cx), layout);
    assert_eq!(
        restored
            .grid
            .read_with(cx, |grid, _| grid.default_layout().sort().to_vec()),
        [gpui_kit_grid::LayoutSort::new("size", Ascending)]
    );
    for column in Col::all() {
        assert_eq!(
            restored.header_cell(cx, *column),
            source.header_cell(cx, *column),
            "{column:?}"
        );
    }
    assert_eq!(restored.header_cell(cx, Col::Ask), None, "hidden");
    assert_eq!(restored.visible_keys(cx), source.visible_keys(cx));
}

/// A merge that takes only the saved widths; order, zones and visibility
/// stay the schema's.
struct SavedWidthsOnly;

impl LayoutMerge for SavedWidthsOnly {
    fn merge(&self, context: &LayoutMergeContext<'_>, saved: &GridLayout) -> Vec<LayoutColumn> {
        context
            .columns()
            .iter()
            .map(|column| match saved.column(column.id().as_str()) {
                Some(saved) => column.clone().with_width(saved.width()),
                None => column.clone(),
            })
            .collect()
    }
}

#[gpui_kit::test]
fn test_with_layout_ignores_builder_order(cx: &mut TestAppContext) {
    type Configure = fn(LiveGridState<Quotes>) -> LiveGridState<Quotes>;

    // A layout of "trades": Total frozen left, Symbol wider and inside the
    // band (only `FreeMove` keeps it there), Note hidden, sorted by Bid.
    fn saved() -> GridLayout {
        GridLayout::new()
            .with_view_id("trades")
            .with_column(LayoutColumn::new("total", 60.).with_zone(ColumnZone::FrozenLeft))
            .with_column(LayoutColumn::new("id", 60.).with_zone(ColumnZone::FrozenLeft))
            .with_column(LayoutColumn::new("bid", 70.))
            .with_column(LayoutColumn::new("symbol", 140.))
            .with_column(LayoutColumn::new("ask", 70.))
            .with_column(LayoutColumn::new("size", 70.))
            .with_column(LayoutColumn::new("note", 100.).with_visible(false))
            .with_sort([gpui_kit_grid::LayoutSort::new("bid", Descending)])
    }
    fn view() -> ViewSpec<Quote> {
        sorted_by(Col::Size, Ascending)
    }
    // Built and never painted: `layout()` reads state alone.
    let layout_of = |cx: &mut TestAppContext, configure: Configure| {
        let (grid, events) = new_grid(cx, 30, configure);
        assert_eq!(take_events(&events), []);
        grid.read_with(cx, |grid, _| (grid.layout(), grid.default_layout()))
    };

    // Applied in every order, also when the view id matches only after the
    // last `with_view_id`. The view's sort stays the default layout's.
    let applied: [Configure; 4] = [
        |grid| {
            grid.with_view(view())
                .with_view_id("trades")
                .with_column_move_policy(FreeMove)
                .with_layout(saved())
        },
        |grid| {
            grid.with_layout(saved())
                .with_view(view())
                .with_view_id("trades")
                .with_column_move_policy(FreeMove)
        },
        |grid| {
            grid.with_column_move_policy(FreeMove)
                .with_layout(saved())
                .with_view_id("trades")
                .with_view(view())
        },
        |grid| {
            grid.with_view_id("orders")
                .with_layout(saved())
                .with_view(view())
                .with_column_move_policy(FreeMove)
                .with_view_id("trades")
        },
    ];
    for (i, configure) in applied.into_iter().enumerate() {
        let (layout, default) = layout_of(cx, configure);
        assert_eq!(layout, saved(), "order {i}");
        assert_eq!(
            default.sort(),
            [gpui_kit_grid::LayoutSort::new("size", Ascending)],
            "order {i}"
        );
    }

    // Refused in every order, also when the view id stops matching after
    // the layout was applied: the grid keeps its default layout.
    let refused: [Configure; 3] = [
        |grid| {
            grid.with_layout(saved())
                .with_view(view())
                .with_column_move_policy(FreeMove)
                .with_view_id("orders")
        },
        |grid| {
            grid.with_view_id("orders")
                .with_view(view())
                .with_layout(saved())
                .with_column_move_policy(FreeMove)
        },
        |grid| {
            grid.with_view_id("trades")
                .with_layout(saved())
                .with_column_move_policy(FreeMove)
                .with_view(view())
                .with_view_id("orders")
        },
    ];
    for (i, configure) in refused.into_iter().enumerate() {
        let (layout, default) = layout_of(cx, configure);
        assert_eq!(layout, default, "order {i}");
        assert_eq!(layout.view_id(), Some("orders"));
        assert_eq!(
            layout.sort(),
            [gpui_kit_grid::LayoutSort::new("size", Ascending)]
        );
    }

    // The merge resolves the layout, before or after it.
    let merged: [Configure; 2] = [
        |grid| grid.with_layout(saved()).with_layout_merge(SavedWidthsOnly),
        |grid| grid.with_layout_merge(SavedWidthsOnly).with_layout(saved()),
    ];
    let [first, second] = merged.map(|configure| layout_of(cx, configure).0);
    assert_eq!(first, second);
    let total = first.column("total").unwrap();
    assert_eq!(total.zone(), ColumnZone::FrozenRight, "the schema's zone");
    assert_eq!(first.column("symbol").map(LayoutColumn::width), Some(140.));
    assert!(first.column("note").unwrap().is_visible());
}

#[gpui_kit::test]
fn test_set_layout_events_and_report(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    // What a subscriber reads when each event arrives.
    let seen: Rc<RefCell<Vec<GridLayout>>> = Rc::default();
    cx.update(|cx| {
        let seen = seen.clone();
        cx.subscribe(
            &fixture.grid,
            move |grid, event: &LiveGridEvent<u32, Col>, cx| {
                if matches!(
                    event,
                    LiveGridEvent::LayoutChange { .. } | LiveGridEvent::SortChange { .. }
                ) {
                    seen.borrow_mut().push(grid.read(cx).layout());
                }
            },
        )
        .detach();
    });
    fixture.take_events();

    // Total moved to the left zone and wider, an unknown column, Bid wider,
    // Ask hidden, Symbol after the band; Note missing. A sort key on an
    // unknown column.
    let saved = GridLayout::new()
        .with_column(LayoutColumn::new("total", 90.).with_zone(ColumnZone::FrozenLeft))
        .with_column(LayoutColumn::new("gone", 50.))
        .with_column(LayoutColumn::new("id", 60.).with_zone(ColumnZone::FrozenLeft))
        .with_column(LayoutColumn::new("bid", 75.))
        .with_column(LayoutColumn::new("ask", 70.).with_visible(false))
        .with_column(LayoutColumn::new("symbol", 80.))
        .with_column(LayoutColumn::new("size", 70.))
        .with_sort([
            gpui_kit_grid::LayoutSort::new("gone", Ascending),
            gpui_kit_grid::LayoutSort::new("bid", Descending),
        ]);
    let report = fixture.set_layout(cx, &saved).unwrap();
    assert_eq!(ids(report.ignored_columns()), ["gone"]);
    assert_eq!(ids(report.added_columns()), ["note"]);
    assert_eq!(ids(report.dropped_sort()), ["gone"]);
    assert!(!report.is_exact());

    let layout = fixture.layout(cx);
    let order: Vec<&str> = layout.columns().iter().map(|c| c.id().as_str()).collect();
    assert_eq!(
        order,
        ["total", "id", "bid", "ask", "symbol", "size", "note"],
        "the new column ends its zone"
    );
    // One `SortChange`, then one `LayoutChange`, both `Programmatic`.
    let events = std::mem::take(&mut *fixture.events.borrow_mut());
    let followed: Vec<&LiveGridEvent<u32, Col>> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                LiveGridEvent::LayoutChange { .. }
                    | LiveGridEvent::SortChange { .. }
                    | LiveGridEvent::OrderHeldChange { .. }
            )
        })
        .collect();
    assert_eq!(followed.len(), 2, "{followed:?}");
    assert!(matches!(
        followed[0],
        LiveGridEvent::SortChange { sort, origin: SortOrigin::Programmatic }
            if **sort == [(Col::Bid, Descending)]
    ));
    let LiveGridEvent::LayoutChange {
        parts,
        columns,
        origin,
    } = followed[1]
    else {
        panic!("expected a LayoutChange, got {:?}", followed[1]);
    };
    assert_eq!(*origin, LayoutOrigin::Programmatic);
    for part in [
        LayoutPart::Width,
        LayoutPart::Order,
        LayoutPart::Visibility,
        LayoutPart::Zone,
    ] {
        assert!(parts.contains(part), "{part:?}");
    }
    // Total and Symbol moved; Total and Bid are wider; Ask is hidden.
    assert_eq!(*columns, set([Col::Total, Col::Symbol, Col::Bid, Col::Ask]));
    // Both events see the complete new layout.
    assert_eq!(*seen.borrow(), [layout.clone(), layout.clone()]);

    // The same layout again: no events, an exact report, no rebuild.
    fixture.settle(cx);
    fixture.take_events();
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());
    let report = fixture.set_layout(cx, &layout).unwrap();
    assert!(report.is_exact());
    fixture.settle(cx);
    assert_eq!(fixture.take_events(), []);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds()),
        rebuilds
    );

    // `import_layout` is the codec plus `set_layout`.
    let text = JsonLayoutCodec::default()
        .encode(&with_width(&layout, "size", 95.))
        .unwrap();
    let report = fixture
        .grid
        .update(cx, |grid, cx| grid.import_layout(&text, cx))
        .unwrap();
    assert!(report.is_exact());
    assert_eq!(
        fixture.take_events(),
        [Seen::Layout(LayoutOrigin::Programmatic)]
    );
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_width(Col::Size)),
        px(95.)
    );
}

#[gpui_kit::test]
fn test_set_layout_columns_only_does_not_rebuild(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| {
        grid.with_view(sorted_by(Col::Bid, Descending))
    });
    fixture.take_events();
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());

    let layout = with_width(&fixture.layout(cx), "symbol", 150.);
    fixture.set_layout(cx, &layout).unwrap();
    fixture.settle(cx);
    assert_eq!(fixture.layout(cx), layout);
    assert_eq!(
        fixture.header_cell(cx, Col::Symbol).unwrap().size.width,
        px(150.)
    );
    assert_eq!(
        fixture.take_events(),
        [Seen::Layout(LayoutOrigin::Programmatic)]
    );
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds()),
        rebuilds,
        "the same sort: no rebuild"
    );
}

#[gpui_kit::test]
fn test_set_layout_sort_releases_held_order(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| {
        grid.with_view(sorted_by(Col::Bid, Descending))
            .with_reorder_policy(ManualReorder)
    });
    let shown: Vec<u32> = (20..30).rev().collect();
    assert_eq!(fixture.visible_keys(cx), shown);
    // Reversed bids: the new order is held.
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    for id in 0..30 {
        writer.upsert(Quote {
            id,
            bid: -i64::from(id) * 10,
        });
    }
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), shown);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    fixture.take_events();

    // A restored sort is a user action of the sort path: the held order goes
    // first and the new sort shows without `apply_held_order`.
    let layout = fixture
        .layout(cx)
        .with_sort([gpui_kit_grid::LayoutSort::new("id", Ascending)]);
    fixture.set_layout(cx, &layout).unwrap();
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Held(false),
            Seen::Sort(vec![(Col::Id, Ascending)], SortOrigin::Programmatic)
        ],
        "the columns did not change: no LayoutChange"
    );
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    assert!(!fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
}

#[gpui_kit::test]
fn test_set_layout_rebuilds_once(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| {
        grid.with_view(sorted_by(Col::Bid, Descending))
            .with_reorder_policy(ManualReorder)
    });
    let rebuilds = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds())
    };
    // A quick filter over the visible columns: only Note says "Pending".
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("pending", cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());

    // A column command that hides Note: one rebuild, and no row passes.
    let before = rebuilds(cx);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Col::Note, false, cx));
    fixture.settle(cx);
    assert_eq!(rebuilds(cx) - before, 1, "a column command");
    assert!(fixture.visible_keys(cx).is_empty());
    let hidden = fixture.layout(cx);
    let shown = fixture
        .grid
        .read_with(cx, |grid, _| grid.default_layout())
        .with_sort(hidden.sort().to_vec());
    fixture.take_events();

    // Each restore rebuilds once, with the new sort and the quick filter
    // over the new columns both in place.
    let restore = |cx: &mut TestAppContext, layout: &GridLayout| {
        let before = rebuilds(cx);
        fixture.set_layout(cx, layout).unwrap();
        let events = fixture.take_events();
        fixture.settle(cx);
        assert_eq!(&fixture.layout(cx), layout);
        (rebuilds(cx) - before, events)
    };
    let by_id = shown
        .clone()
        .with_sort([gpui_kit_grid::LayoutSort::new("id", Ascending)]);
    assert_eq!(
        restore(cx, &by_id),
        (
            1,
            vec![
                Seen::Sort(vec![(Col::Id, Ascending)], SortOrigin::Programmatic),
                Seen::Layout(LayoutOrigin::Programmatic),
            ]
        ),
        "sort and visibility"
    );
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());

    assert_eq!(
        restore(cx, &shown),
        (
            1,
            vec![Seen::Sort(
                vec![(Col::Bid, Descending)],
                SortOrigin::Programmatic
            )]
        ),
        "sort only"
    );
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());

    // Reversed bids: the new order is held. Hiding Note releases it before
    // the `LayoutChange`, as a restored sort does before its `SortChange`.
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    for id in 0..30 {
        writer.upsert(Quote {
            id,
            bid: -i64::from(id) * 10,
        });
    }
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    fixture.take_events();
    assert_eq!(
        restore(cx, &hidden),
        (
            1,
            vec![Seen::Held(false), Seen::Layout(LayoutOrigin::Programmatic)]
        ),
        "visibility only"
    );
    assert!(fixture.visible_keys(cx).is_empty());
}

#[gpui_kit::test]
fn test_view_id_mismatch_is_refused(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid.with_view_id("trades"));
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.view_id().cloned()),
        Some("trades".into())
    );
    let before = fixture.layout(cx);
    assert_eq!(before.view_id(), Some("trades"));
    fixture.take_events();

    let wide = |layout: GridLayout| layout.with_column(LayoutColumn::new("symbol", 200.));
    let orders = wide(GridLayout::new().with_view_id("orders"));
    assert_eq!(
        fixture.set_layout(cx, &orders),
        Err(LayoutError::ViewMismatch {
            expected: "trades".into(),
            found: "orders".into(),
        })
    );
    // A version this build does not read: from plain serde, from the codec.
    let newer: GridLayout =
        serde_json::from_str(r#"{"version":2,"columns":[{"id":"symbol","width":200}]}"#).unwrap();
    assert_eq!(
        fixture.set_layout(cx, &newer),
        Err(LayoutError::UnsupportedVersion { found: 2 })
    );
    let import = |cx: &mut TestAppContext, text: &str| {
        fixture
            .grid
            .update(cx, |grid, cx| grid.import_layout(text, cx))
    };
    assert_eq!(
        import(cx, r#"{"version":99,"columns":[]}"#),
        Err(LayoutError::UnsupportedVersion { found: 99 })
    );
    assert!(matches!(
        import(cx, "not a layout"),
        Err(LayoutError::Decode(_))
    ));
    // Nothing changed and nothing was said.
    assert_eq!(fixture.layout(cx), before);
    assert_eq!(fixture.take_events(), []);

    // A layout without a view id is accepted.
    let report = fixture.set_layout(cx, &wide(GridLayout::new())).unwrap();
    assert_eq!(report.added_columns().len(), Col::all().len() - 1);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_width(Col::Symbol)),
        px(200.)
    );
    assert_eq!(
        fixture.take_events(),
        [Seen::Layout(LayoutOrigin::Programmatic)]
    );

    // A grid without a view id accepts a layout of any view.
    let anonymous = open(cx, 30, |grid| grid);
    anonymous.set_layout(cx, &orders).unwrap();
    assert_eq!(anonymous.layout(cx).view_id(), None);
}

#[gpui_kit::test]
fn test_reset_layout_action(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| {
        grid.with_view_id("trades")
            .with_view(sorted_by(Col::Bid, Descending))
    });
    let default = fixture.grid.read_with(cx, |grid, _| grid.default_layout());
    assert_eq!(default, fixture.layout(cx), "a new grid has the default");
    assert_eq!(default.view_id(), Some("trades"));
    assert_eq!(
        default.sort(),
        [gpui_kit_grid::LayoutSort::new("bid", Descending)]
    );

    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Symbol, px(150.), cx);
        grid.set_column_visible(Col::Note, false, cx);
        grid.set_column_zone(Col::Size, ColumnZone::FrozenRight, cx);
        grid.set_sort([(Col::Id, Ascending)], cx);
    });
    fixture.settle(cx);
    assert_ne!(fixture.layout(cx), default);
    fixture.take_events();

    // The Action restores the default layout, as the user.
    fixture.input(cx, |window, cx| {
        let focus = fixture.grid.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        window.dispatch_action(Box::new(ResetLayout), cx);
    });
    assert_eq!(fixture.layout(cx), default);
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Sort(vec![(Col::Bid, Descending)], SortOrigin::User),
            Seen::Layout(LayoutOrigin::User),
        ]
    );
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
    assert_eq!(
        fixture.header_cell(cx, Col::Symbol).unwrap().size.width,
        px(80.)
    );

    // `reset_layout` does the same from the API; at the default, nothing.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Symbol, px(150.), cx);
        grid.reset_layout(cx);
    });
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Layout(LayoutOrigin::Programmatic),
            Seen::Layout(LayoutOrigin::Programmatic),
        ]
    );
    fixture.grid.update(cx, |grid, cx| grid.reset_layout(cx));
    assert_eq!(fixture.take_events(), []);
}

#[gpui_kit::test]
fn test_layout_of_grid_not_in_window(cx: &mut TestAppContext) {
    let (grid, events) = new_grid(cx, 30, |grid| grid);
    let saved = GridLayout::new()
        .with_column(LayoutColumn::new("total", 60.).with_zone(ColumnZone::FrozenLeft))
        .with_column(LayoutColumn::new("id", 60.).with_zone(ColumnZone::FrozenLeft))
        .with_column(LayoutColumn::new("symbol", 140.))
        .with_column(LayoutColumn::new("bid", 70.))
        .with_column(LayoutColumn::new("ask", 70.).with_visible(false))
        .with_column(LayoutColumn::new("size", 70.))
        .with_column(LayoutColumn::new("note", 100.))
        .with_sort([gpui_kit_grid::LayoutSort::new("bid", Descending)]);
    // Never painted: the API works on state alone and the events come.
    let report = grid
        .update(cx, |grid, cx| grid.set_layout(&saved, cx))
        .unwrap();
    assert!(report.is_exact());
    assert_eq!(grid.read_with(cx, |grid, _| grid.layout()), saved);
    assert_eq!(
        take_events(&events),
        [
            Seen::Sort(vec![(Col::Bid, Descending)], SortOrigin::Programmatic),
            Seen::Layout(LayoutOrigin::Programmatic),
        ]
    );

    // The first frame shows it.
    let fixture = show(cx, grid, events);
    let cell = |cx: &mut TestAppContext, column| {
        let bounds = fixture.header_cell(cx, column).expect("shown");
        (bounds.left(), bounds.size.width)
    };
    assert_eq!(cell(cx, Col::Total), (px(0.), px(60.)));
    assert_eq!(cell(cx, Col::Id), (px(60.), px(60.)));
    assert_eq!(cell(cx, Col::Symbol), (px(120.), px(140.)));
    assert_eq!(fixture.header_cell(cx, Col::Ask), None);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
}

#[gpui_kit::test]
fn test_save_triggers_per_gesture(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    // A saving subscriber: `LayoutChange` and `SortChange` are its triggers.
    let triggers = Rc::new(RefCell::new(0));
    cx.update(|cx| {
        let triggers = triggers.clone();
        cx.subscribe(
            &fixture.grid,
            move |_, event: &LiveGridEvent<u32, Col>, _| {
                if matches!(
                    event,
                    LiveGridEvent::LayoutChange { .. } | LiveGridEvent::SortChange { .. }
                ) {
                    *triggers.borrow_mut() += 1;
                }
            },
        )
        .detach();
    });

    // A resize with the mouse: nothing while it moves, one after release.
    let start = fixture.input(cx, |window, _| {
        window
            .within("live-grid-resize")
            .find("symbol")
            .bounds()
            .center()
    });
    fixture.input(cx, |window, cx| {
        mouse_down_at(
            window,
            start,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    });
    for step in 1..=3 {
        let to = start + point(px(10. * step as f32), px(0.));
        fixture.input(cx, |window, cx| drag_move_to(window, to, cx));
        assert_eq!(*triggers.borrow(), 0, "none during the gesture");
    }
    let end = start + point(px(30.), px(0.));
    fixture.input(cx, |window, cx| {
        mouse_up_at(window, end, MouseButton::Left, Modifiers::default(), 1, cx);
    });
    assert_eq!(*triggers.borrow(), 1, "one after the release");
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_width(Col::Symbol)),
        px(110.)
    );

    // N commands: N triggers; a command that changes nothing: none.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Symbol, px(90.), cx);
        grid.set_column_visible(Col::Note, false, cx);
        grid.set_sort([(Col::Bid, Ascending)], cx);
        grid.set_column_zone(Col::Size, ColumnZone::FrozenRight, cx);
        grid.set_column_width(Col::Symbol, px(90.), cx);
    });
    assert_eq!(*triggers.borrow(), 5);
}

// Grouping and expansion in layouts, on the grid of trades of
// `common` (`GroupedFixture`).

use GroupOrigin::Programmatic;

/// What a test of a restored grouping follows of an event, in order.
#[derive(Debug, PartialEq)]
enum Restored {
    Held(bool),
    Sort(SortOrigin),
    Group(GroupOrigin),
    Expansion(Option<ExpandTarget<u32>>, GroupOrigin),
    Layout(LayoutOrigin),
}

/// The followed events of a grid of trades so far, then forgets every
/// event.
fn take_restored(fixture: &GroupedFixture) -> Vec<Restored> {
    std::mem::take(&mut *fixture.events.borrow_mut())
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::OrderHeldChange { held } => Some(Restored::Held(held)),
            LiveGridEvent::SortChange { origin, .. } => Some(Restored::Sort(origin)),
            LiveGridEvent::GroupChange { origin } => Some(Restored::Group(origin)),
            LiveGridEvent::ExpansionChange { target, origin } => {
                Some(Restored::Expansion(target, origin))
            }
            LiveGridEvent::LayoutChange { origin, .. } => Some(Restored::Layout(origin)),
            _ => None,
        })
        .collect()
}

fn by(column: GroupedColumn) -> GroupBy<GroupedRow> {
    GroupBy::column(column)
}

fn level(column: GroupedColumn, direction: SortDirection) -> LayoutGroup {
    LayoutGroup::new(column.id(), direction)
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::Text(value.into())
}

fn group(path: GroupPath) -> ExpandTarget<u32> {
    ExpandTarget::Group(path)
}

fn aapl() -> GroupPath {
    group_path(&[(GroupedColumn::Instrument, "AAPL")])
}

/// Grouped by instrument, then by side descending.
fn by_instrument_and_side() -> Vec<GroupBy<GroupedRow>> {
    vec![
        by(GroupedColumn::Instrument),
        by(GroupedColumn::Side).with_direction(Descending),
    ]
}

/// Groups by the trader's desk, a function of the row.
fn by_desk() -> GroupBy<GroupedRow> {
    let mut trader = ColumnSet::EMPTY;
    trader.insert(GroupedColumn::Trader.ix());
    GroupBy::key(GroupKey::new("desk", trader, |row: &GroupedRow| {
        CellValue::Text(row.trader)
    }))
}

/// The columns of `grouped_schema` as a layout saves them.
fn trade_columns() -> GridLayout {
    [
        ("id", 50.),
        ("instrument", 80.),
        ("side", 60.),
        ("qty", 70.),
        ("price", 80.),
        ("trader", 70.),
    ]
    .into_iter()
    .map(|(id, width)| LayoutColumn::new(id, width))
    .fold(GridLayout::new(), GridLayout::with_column)
}

fn layout_of(fixture: &GroupedFixture, cx: &mut TestAppContext) -> GridLayout {
    fixture.grid.read_with(cx, |grid, _| grid.layout())
}

fn default_layout_of(fixture: &GroupedFixture, cx: &mut TestAppContext) -> GridLayout {
    fixture.grid.read_with(cx, |grid, _| grid.default_layout())
}

fn level_ids(fixture: &GroupedFixture, cx: &mut TestAppContext) -> Vec<String> {
    fixture.grid.read_with(cx, |grid, _| {
        grid.group_by()
            .iter()
            .map(|level| level.id().as_str().to_owned())
            .collect()
    })
}

fn is_expanded(fixture: &GroupedFixture, cx: &mut TestAppContext, path: &GroupPath) -> bool {
    fixture
        .grid
        .read_with(cx, |grid, _| grid.is_expanded(&group(path.clone())))
}

fn rebuilds_of(fixture: &GroupedFixture, cx: &mut TestAppContext) -> u64 {
    fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds())
}

/// The captions of the instruments, every group collapsed.
fn instrument_captions() -> Vec<LineKind> {
    ["AAPL", "MSFT", "NVDA", "TSLA"]
        .map(|instrument| LineKind::Caption(group_path(&[(GroupedColumn::Instrument, instrument)])))
        .to_vec()
}

/// Dispatches `ResetLayout` to the focused grid, as the user.
fn dispatch_reset(fixture: &GroupedFixture, cx: &mut TestAppContext) {
    fixture.input(cx, |window, cx| {
        let focus = fixture.grid.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        window.dispatch_action(Box::new(ResetLayout), cx);
    });
}

#[gpui_kit::test]
fn test_layout_writes_grouping(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(24), |grid| {
        grid.with_view(ViewSpec::new().with_group_by(by_instrument_and_side()))
            .with_tuning(unthrottled())
    });
    let levels = [
        level(GroupedColumn::Instrument, Ascending),
        level(GroupedColumn::Side, Descending),
    ];
    // A new grid: the grouping of its view, every group expanded.
    let layout = layout_of(&fixture, cx);
    assert_eq!(layout.group_by(), levels);
    assert_eq!(layout.expansion(), None, "the default is not written");
    assert_eq!(layout, default_layout_of(&fixture, cx));

    // A collapsed group is a path of key values, without column ids.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_expanded(group(aapl()), false, cx);
    });
    let collapsed_aapl = LayoutExpansion::new(ExpandDepth::All).with_toggled([text("AAPL")]);
    assert_eq!(layout_of(&fixture, cx).expansion(), Some(&collapsed_aapl));

    // The groups of the current grouping only: a toggle of another grouping
    // is kept by the grid, and saved again with its grouping.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by([by(GroupedColumn::Side)], cx);
        let sell = group_path(&[(GroupedColumn::Side, "Sell")]);
        grid.set_expanded(group(sell), false, cx);
    });
    let layout = layout_of(&fixture, cx);
    assert_eq!(layout.group_by(), [level(GroupedColumn::Side, Ascending)]);
    assert_eq!(
        layout.expansion(),
        Some(&LayoutExpansion::new(ExpandDepth::All).with_toggled([text("Sell")]))
    );
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by(by_instrument_and_side(), cx)
    });
    let layout = layout_of(&fixture, cx);
    assert_eq!(layout.group_by(), levels);
    assert_eq!(layout.expansion(), Some(&collapsed_aapl));

    // A new depth clears the toggles and is written.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::Captions, cx);
    });
    assert_eq!(
        layout_of(&fixture, cx).expansion(),
        Some(&LayoutExpansion::new(ExpandDepth::Captions))
    );

    // A level by a function: neither the grouping nor the expansion.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by([by(GroupedColumn::Instrument), by_desk()], cx);
    });
    let layout = layout_of(&fixture, cx);
    assert!(layout.group_by().is_empty());
    assert_eq!(layout.expansion(), None);
    assert_eq!(layout.columns(), default_layout_of(&fixture, cx).columns());
}

#[gpui_kit::test]
fn test_set_layout_restores_grouping(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(24), |grid| grid.with_tuning(unthrottled()));
    // Trader wider, sorted by price, grouped by instrument and side, AAPL
    // collapsed.
    let widened = trade_columns()
        .columns()
        .iter()
        .map(|column| match column.id().as_str() {
            "trader" => column.clone().with_width(90.),
            _ => column.clone(),
        })
        .fold(GridLayout::new(), GridLayout::with_column)
        .with_sort([gpui_kit_grid::LayoutSort::new("price", Descending)])
        .with_group_by([
            level(GroupedColumn::Instrument, Ascending),
            level(GroupedColumn::Side, Descending),
        ])
        .with_expansion(LayoutExpansion::new(ExpandDepth::All).with_toggled([text("AAPL")]));
    take_restored(&fixture);
    let rebuilds = rebuilds_of(&fixture, cx);

    // Columns, sort, grouping and expansion in one call: the events in
    // order, each when everything has changed, and one rebuild.
    let report = fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&widened, cx))
        .unwrap();
    assert!(report.is_exact());
    assert_eq!(
        take_restored(&fixture),
        [
            Restored::Sort(SortOrigin::Programmatic),
            Restored::Group(Programmatic),
            Restored::Expansion(None, Programmatic),
            Restored::Layout(LayoutOrigin::Programmatic),
        ]
    );
    assert_eq!(layout_of(&fixture, cx), widened);
    fixture.settle(cx);
    assert_eq!(rebuilds_of(&fixture, cx) - rebuilds, 1, "one rebuild");
    assert_eq!(level_ids(&fixture, cx), ["instrument", "side"]);
    assert!(!is_expanded(&fixture, cx, &aapl()));
    let msft = group_path(&[(GroupedColumn::Instrument, "MSFT")]);
    assert_eq!(
        fixture.lines(cx)[..2],
        [LineKind::Caption(aapl()), LineKind::Caption(msft.clone())],
        "AAPL collapsed"
    );

    // The same layout again: nothing, not even a rebuild.
    let rebuilds = rebuilds_of(&fixture, cx);
    let report = fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&widened, cx))
        .unwrap();
    assert!(report.is_exact());
    fixture.settle(cx);
    assert_eq!(take_restored(&fixture), []);
    assert_eq!(rebuilds_of(&fixture, cx), rebuilds);

    // Only the expansion: the event, no rebuild, the lines laid out again.
    let msft_collapsed = widened
        .clone()
        .with_expansion(LayoutExpansion::new(ExpandDepth::All).with_toggled([text("MSFT")]));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&msft_collapsed, cx))
        .unwrap();
    assert_eq!(
        take_restored(&fixture),
        [Restored::Expansion(None, Programmatic)]
    );
    fixture.settle(cx);
    assert_eq!(rebuilds_of(&fixture, cx), rebuilds);
    assert!(is_expanded(&fixture, cx, &aapl()));
    assert!(!is_expanded(&fixture, cx, &msft));

    // Through text into a new grid of the same view: the same layout and
    // the same lines.
    let text = fixture
        .grid
        .read_with(cx, |grid, _| grid.export_layout())
        .unwrap();
    let decoded = JsonLayoutCodec::default().decode(&text).unwrap();
    assert_eq!(decoded, msft_collapsed);
    let restored = open_grouped(cx, grouped_rows(24), move |grid| grid.with_layout(decoded));
    assert_eq!(layout_of(&restored, cx), msft_collapsed);
    assert_eq!(restored.lines(cx), fixture.lines(cx));
}

#[gpui_kit::test]
fn test_set_layout_under_another_schema(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(24), |grid| grid.with_tuning(unthrottled()));
    take_restored(&fixture);

    // Saved by a build whose trades had a venue and no trader. The venue
    // level is dropped and the paths deeper than the instrument level with
    // it; the empty path too.
    let saved = [
        ("id", 50.),
        ("venue", 70.),
        ("instrument", 80.),
        ("side", 60.),
        ("qty", 70.),
        ("price", 80.),
    ]
    .into_iter()
    .map(|(id, width)| LayoutColumn::new(id, width))
    .fold(GridLayout::new(), GridLayout::with_column)
    .with_group_by([
        LayoutGroup::new("instrument", Ascending),
        LayoutGroup::new("venue", Ascending),
    ])
    .with_expansion(
        LayoutExpansion::new(ExpandDepth::All)
            .with_toggled([text("AAPL"), text("XNAS")])
            .with_toggled([text("AAPL")])
            .with_toggled([]),
    );
    let report = fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&saved, cx))
        .unwrap();
    assert_eq!(ids(report.ignored_columns()), ["venue"]);
    assert_eq!(ids(report.added_columns()), ["trader"]);
    assert_eq!(ids(report.dropped_group_by()), ["venue"]);
    assert_eq!(report.dropped_expansion(), 2);
    assert!(report.dropped_sort().is_empty());
    assert!(!report.is_exact());
    assert_eq!(
        take_restored(&fixture),
        [
            Restored::Group(Programmatic),
            Restored::Expansion(None, Programmatic)
        ],
        "the columns are the schema's: no LayoutChange"
    );
    fixture.settle(cx);
    assert_eq!(level_ids(&fixture, cx), ["instrument"]);
    assert!(!is_expanded(&fixture, cx, &aapl()));
    let layout = layout_of(&fixture, cx);
    assert_eq!(
        layout.group_by(),
        [level(GroupedColumn::Instrument, Ascending)]
    );
    assert_eq!(
        layout.expansion().map(LayoutExpansion::toggled),
        Some([vec![text("AAPL")]].as_slice())
    );

    // A layout without grouping turns it off. Its paths name no columns:
    // dropped and reported. The toggle of the instrument grouping stays
    // with the grid.
    let flat = trade_columns()
        .with_expansion(LayoutExpansion::new(ExpandDepth::All).with_toggled([text("MSFT")]));
    let report = fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&flat, cx))
        .unwrap();
    assert_eq!(report.dropped_expansion(), 1);
    assert_eq!(take_restored(&fixture), [Restored::Group(Programmatic)]);
    assert!(level_ids(&fixture, cx).is_empty());
    assert_eq!(layout_of(&fixture, cx), trade_columns());
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by([by(GroupedColumn::Instrument)], cx)
    });
    assert!(
        !is_expanded(&fixture, cx, &aapl()),
        "back with its grouping"
    );
}

#[gpui_kit::test]
fn test_reset_layout_restores_view_grouping(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(24), |grid| {
        grid.with_view(ViewSpec::new().with_group_by([by(GroupedColumn::Instrument)]))
            .with_expand_depth(ExpandDepth::Levels(0))
            .with_tuning(unthrottled())
    });
    let default = default_layout_of(&fixture, cx);
    assert_eq!(
        default,
        layout_of(&fixture, cx),
        "a new grid has the default"
    );
    assert_eq!(
        default.group_by(),
        [level(GroupedColumn::Instrument, Ascending)]
    );
    assert_eq!(
        default.expansion(),
        Some(&LayoutExpansion::new(ExpandDepth::Levels(0)))
    );

    // The user groups by side, expands everything, collapses a group.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by([by(GroupedColumn::Side)], cx);
        grid.set_expand_depth(ExpandDepth::All, cx);
        let buy = group_path(&[(GroupedColumn::Side, "Buy")]);
        grid.set_expanded(group(buy), false, cx);
    });
    fixture.settle(cx);
    take_restored(&fixture);

    // The Action restores the view's grouping and depth, as the user.
    dispatch_reset(&fixture, cx);
    assert_eq!(
        take_restored(&fixture),
        [
            Restored::Group(GroupOrigin::User),
            Restored::Expansion(None, GroupOrigin::User)
        ]
    );
    assert_eq!(layout_of(&fixture, cx), default);
    assert_eq!(level_ids(&fixture, cx), ["instrument"]);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.expand_depth()),
        ExpandDepth::Levels(0)
    );
    fixture.settle(cx);
    assert_eq!(
        fixture.lines(cx),
        instrument_captions(),
        "every group collapsed"
    );
    // At the default, nothing.
    fixture.grid.update(cx, |grid, cx| grid.reset_layout(cx));
    assert_eq!(take_restored(&fixture), []);

    // A grouping by a function is not saved, but the reset brings it back:
    // it works on the state, not on a file.
    let by_function = open_grouped(cx, grouped_rows(24), |grid| {
        grid.with_view(ViewSpec::new().with_group_by([by_desk(), by(GroupedColumn::Instrument)]))
            .with_tuning(unthrottled())
    });
    assert!(default_layout_of(&by_function, cx).group_by().is_empty());
    by_function.grid.update(cx, |grid, cx| {
        grid.set_group_by([by(GroupedColumn::Side)], cx);
        grid.reset_layout(cx);
    });
    assert_eq!(level_ids(&by_function, cx), ["desk", "instrument"]);
    assert_eq!(
        take_restored(&by_function),
        [Restored::Group(Programmatic), Restored::Group(Programmatic)]
    );
}

/// A grid of trades outside any window, built by `configure`, and its
/// layout and default layout. Never painted: `layout()` reads state alone.
fn built_layouts(
    cx: &mut TestAppContext,
    configure: fn(LiveGridState<Grouped>) -> LiveGridState<Grouped>,
) -> (GridLayout, GridLayout, Vec<String>, ExpandDepth) {
    cx.update(gpui_kit::init);
    let grid = cx.new(|cx| {
        configure(LiveGridState::new_standalone(
            grouped_schema(),
            Grouped,
            StoreOptions::default(),
            cx,
        ))
    });
    grid.read_with(cx, |grid, _| {
        (
            grid.layout(),
            grid.default_layout(),
            grid.group_by()
                .iter()
                .map(|level| level.id().as_str().to_owned())
                .collect(),
            grid.expand_depth(),
        )
    })
}

#[gpui_kit::test]
fn test_with_layout_builder_order_with_groups(cx: &mut TestAppContext) {
    type Configure = fn(LiveGridState<Grouped>) -> LiveGridState<Grouped>;

    // Sorted by price, grouped by side and instrument, captions only with
    // a side and a group of the second level toggled.
    fn saved() -> GridLayout {
        trade_columns()
            .with_view_id("trades")
            .with_sort([gpui_kit_grid::LayoutSort::new("price", Descending)])
            .with_group_by([
                level(GroupedColumn::Side, Descending),
                level(GroupedColumn::Instrument, Ascending),
            ])
            .with_expansion(
                LayoutExpansion::new(ExpandDepth::Captions)
                    .with_toggled([text("Buy")])
                    .with_toggled([text("Sell"), text("MSFT")]),
            )
    }
    // The application's view: by quantity, grouped by trader.
    fn view() -> ViewSpec<GroupedRow> {
        ViewSpec::new()
            .with_sort([(GroupedColumn::Qty, Ascending)])
            .with_group_by([by(GroupedColumn::Trader)])
    }
    let default_grouping = [level(GroupedColumn::Trader, Ascending)];
    let default_expansion = LayoutExpansion::new(ExpandDepth::Levels(1));

    let applied: [Configure; 4] = [
        |grid| {
            grid.with_view_id("trades")
                .with_view(view())
                .with_expand_depth(ExpandDepth::Levels(1))
                .with_layout(saved())
        },
        |grid| {
            grid.with_layout(saved())
                .with_view_id("trades")
                .with_view(view())
                .with_expand_depth(ExpandDepth::Levels(1))
        },
        |grid| {
            grid.with_expand_depth(ExpandDepth::Levels(1))
                .with_layout(saved())
                .with_view_id("trades")
                .with_view(view())
        },
        |grid| {
            grid.with_view_id("orders")
                .with_layout(saved())
                .with_expand_depth(ExpandDepth::Levels(1))
                .with_view(view())
                .with_view_id("trades")
        },
    ];
    for (i, configure) in applied.into_iter().enumerate() {
        let (layout, default, levels, depth) = built_layouts(cx, configure);
        assert_eq!(layout, saved(), "order {i}");
        assert_eq!(levels, ["side", "instrument"], "order {i}");
        assert_eq!(depth, ExpandDepth::Captions, "order {i}");
        assert_eq!(default.group_by(), default_grouping, "order {i}");
        assert_eq!(default.expansion(), Some(&default_expansion), "order {i}");
    }

    // Refused in any order: the view's grouping and depth.
    let refused: [Configure; 2] = [
        |grid| {
            grid.with_layout(saved())
                .with_expand_depth(ExpandDepth::Levels(1))
                .with_view(view())
                .with_view_id("orders")
        },
        |grid| {
            grid.with_view_id("trades")
                .with_view(view())
                .with_layout(saved())
                .with_view_id("orders")
                .with_expand_depth(ExpandDepth::Levels(1))
        },
    ];
    for (i, configure) in refused.into_iter().enumerate() {
        let (layout, default, levels, depth) = built_layouts(cx, configure);
        assert_eq!(layout, default, "order {i}");
        assert_eq!(levels, ["trader"], "order {i}");
        assert_eq!(depth, ExpandDepth::Levels(1), "order {i}");
        assert_eq!(layout.expansion(), Some(&default_expansion), "order {i}");
    }

    // A layout without grouping keeps the tree of the view, and the tree
    // its depth, before or after.
    let tree: [Configure; 2] = [
        |grid| {
            grid.with_layout(trade_columns())
                .with_view(ViewSpec::new().with_tree(TreeSpec::new()))
                .with_expand_depth(ExpandDepth::Levels(1))
        },
        |grid| {
            grid.with_expand_depth(ExpandDepth::Levels(1))
                .with_view(ViewSpec::new().with_tree(TreeSpec::new()))
                .with_layout(trade_columns())
        },
    ];
    for (i, configure) in tree.into_iter().enumerate() {
        let (layout, _, levels, depth) = built_layouts(cx, configure);
        assert_eq!(layout, trade_columns(), "order {i}");
        assert!(levels.is_empty(), "order {i}");
        assert_eq!(depth, ExpandDepth::Levels(1), "order {i}");
    }
}

#[gpui_kit::test]
fn test_layout_and_tree(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(24), |grid| {
        grid.with_view(ViewSpec::new().with_tree(TreeSpec::new()))
            .with_expand_depth(ExpandDepth::Levels(1))
            .with_tuning(unthrottled())
    });
    let is_tree =
        |cx: &mut TestAppContext| fixture.grid.read_with(cx, |grid, _| grid.tree().is_some());
    // Neither a tree nor its expansion is saved.
    let layout = layout_of(&fixture, cx);
    assert_eq!(layout, trade_columns());
    assert_eq!(layout, default_layout_of(&fixture, cx));
    take_restored(&fixture);

    // A layout of columns only, without grouping: the tree and its depth stay.
    let wider = trade_columns()
        .columns()
        .iter()
        .map(|column| column.clone().with_width(column.width() + 10.))
        .fold(GridLayout::new(), GridLayout::with_column);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&wider, cx))
        .unwrap();
    assert_eq!(
        take_restored(&fixture),
        [Restored::Layout(LayoutOrigin::Programmatic)]
    );
    assert!(is_tree(cx));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.expand_depth()),
        ExpandDepth::Levels(1)
    );

    // A layout with grouping replaces the tree.
    let grouped = wider
        .clone()
        .with_group_by([level(GroupedColumn::Instrument, Ascending)]);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&grouped, cx))
        .unwrap();
    assert_eq!(
        take_restored(&fixture),
        [
            Restored::Group(Programmatic),
            Restored::Expansion(None, Programmatic)
        ]
    );
    assert!(!is_tree(cx));
    assert_eq!(level_ids(&fixture, cx), ["instrument"]);
    assert_eq!(layout_of(&fixture, cx), grouped);
    fixture.settle(cx);
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.projection().is_grouped())
    );

    // The reset brings the tree of the view back.
    fixture.grid.update(cx, |grid, cx| grid.reset_layout(cx));
    assert_eq!(
        take_restored(&fixture),
        [
            Restored::Group(Programmatic),
            Restored::Layout(LayoutOrigin::Programmatic)
        ]
    );
    assert!(is_tree(cx));
    fixture.settle(cx);
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.projection().is_tree())
    );
}

#[gpui_kit::test]
fn test_set_layout_grouping_releases_held_order(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(24), |grid| {
        grid.with_view(ViewSpec::new().with_sort([(GroupedColumn::Price, Ascending)]))
            .with_tuning(unthrottled())
            .with_reorder_policy(ManualReorder)
    });
    // New prices: the new order is held.
    let writer = fixture.writer(cx);
    for mut row in grouped_rows(24) {
        row.price = -row.price;
        writer.upsert(row);
    }
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    take_restored(&fixture);

    // The grouping alone: the held order goes first.
    let grouped = layout_of(&fixture, cx)
        .with_group_by([level(GroupedColumn::Instrument, Ascending)])
        .with_expansion(LayoutExpansion::new(ExpandDepth::Levels(0)));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_layout(&grouped, cx))
        .unwrap();
    assert_eq!(
        take_restored(&fixture),
        [
            Restored::Held(false),
            Restored::Group(Programmatic),
            Restored::Expansion(None, Programmatic)
        ]
    );
    fixture.settle(cx);
    assert!(!fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    assert_eq!(
        fixture.lines(cx),
        instrument_captions(),
        "every group collapsed"
    );
}
