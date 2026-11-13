//! Headless UI tests of the column layout: resize, reorder and
//! autofit with the mouse, frozen zones, bands, the column Actions, and
//! column gestures under live data (`.agents/COMPONENT_TEST_RULES.md`).

mod common;

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::{
    Action, AnyWindowHandle, App, AppContext as _, Bounds, Context, ElementId, Entity, Font,
    FontFeatures, IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels, Point, Render,
    Role, ScrollDelta, SharedString, Styled as _, TestAppContext, TextRun, Window, WindowBounds,
    WindowOptions, black, div, point, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellTooltip, CellValue, ColumnBand, ColumnIx, ColumnSet, ColumnZone, FreeMove, GridColumn,
    GridColumns, GridRow, GridSchema, GridTuning, HeaderTooltipContext, HoldReorder, LayoutOrigin,
    LayoutPart, LayoutParts, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState,
    MIN_COLUMN_WIDTH, ManualReorder, SortDirection, StoreOptions, ViewSpec,
    actions::{
        AutofitColumns, FreezeColumnRight, HideColumn, MoveColumnLeft, MoveColumnRight,
        ShowAllColumns, UnfreezeColumn,
    },
};

use crate::common::{drag_move_to, mouse_down_at, mouse_up_at, move_pointer_to, press_at};

const ROW: f32 = 26.;
/// One band level over the columns: two header rows.
const HEADER: f32 = ROW * 2.;
const WIDTH: f32 = 400.;
/// The header plus ten rows at the `xsmall` size.
const HEIGHT: f32 = HEADER + ROW * 10.;
const NOTE: &str = "Allocation pending confirmation from the middle office";

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
    symbol: String,
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
            Col::Symbol => CellValue::Text(&self.symbol),
            Col::Bid => CellValue::Int(self.bid),
            Col::Ask => CellValue::Int(self.bid + 1),
            Col::Size => CellValue::Int(100),
            Col::Note => CellValue::Text(NOTE),
            Col::Total => CellValue::Int(0),
        }
    }
}

/// Symbols of growing width; row 500 has a much longer one.
fn symbol(id: u32) -> String {
    let wide = if id == 500 { 40 } else { id as usize % 7 };
    format!("S{}", "W".repeat(wide))
}

fn quote(id: u32, sign: i64) -> Quote {
    Quote {
        id,
        symbol: symbol(id),
        bid: sign * i64::from(id) * 10,
    }
}

struct Quotes;

impl LiveGridDelegate for Quotes {
    type Row = Quote;
}

/// Id frozen left (60 px); Symbol 80, band "quote" over Bid 70 and Ask 70,
/// Size 70, Note 100 scrolling; Total frozen right (60). In the 400 px
/// window the scrolling zone is 60..340.
fn schema() -> GridSchema<Col> {
    schema_with(GridColumn::new(Col::Symbol, "Symbol").with_width(px(80.)))
}

/// [`schema`] with another Symbol column.
fn schema_with(symbol: GridColumn<Col>) -> GridSchema<Col> {
    GridSchema::new()
        .column(
            GridColumn::new(Col::Id, "Id")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenLeft),
        )
        .column(symbol)
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

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Quotes>>,
    events: Events,
}

/// A grid of `count` rows outside any window.
fn new_grid(
    cx: &mut TestAppContext,
    count: u32,
    configure: impl FnOnce(LiveGridState<Quotes>) -> LiveGridState<Quotes>,
) -> (Entity<LiveGridState<Quotes>>, Events) {
    new_grid_with(cx, schema(), count, configure)
}

fn new_grid_with(
    cx: &mut TestAppContext,
    schema: GridSchema<Col>,
    count: u32,
    configure: impl FnOnce(LiveGridState<Quotes>) -> LiveGridState<Quotes>,
) -> (Entity<LiveGridState<Quotes>>, Events) {
    cx.update(gpui_kit::init);
    let grid = cx.new(|cx| {
        configure(LiveGridState::new_standalone(
            schema,
            Quotes,
            StoreOptions::default(),
            cx,
        ))
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
    writer.replace_all((0..count).map(|id| Arc::new(quote(id, 1))));
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    (grid, events)
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

fn unthrottled() -> GridTuning {
    GridTuning::default().with_min_rebuild_interval(Duration::ZERO)
}

fn set(columns: impl IntoIterator<Item = Col>) -> ColumnSet {
    columns
        .into_iter()
        .fold(ColumnSet::EMPTY, |mut set, column| {
            set.insert(column.ix());
            set
        })
}

impl Fixture {
    /// Runs ingest, pulls and background jobs, then draws a frame.
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx);
        }
    }

    fn frame(&self, cx: &mut TestAppContext) {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        self.input(cx, |window, cx| window.render_frame(cx));
    }

    fn input<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut Window, &mut gpui_kit::App) -> R,
    ) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    fn bounds(
        &self,
        cx: &mut TestAppContext,
        scope: &'static str,
        id: &'static str,
    ) -> Bounds<Pixels> {
        self.input(cx, |window, _| window.within(scope).find(id).bounds())
    }

    fn has(&self, cx: &mut TestAppContext, scope: &'static str, id: impl Into<ElementId>) -> bool {
        let id = id.into();
        self.input(cx, |window, _| window.within(scope).try_find(id).is_some())
    }

    fn has_drop_gap(&self, cx: &mut TestAppContext) -> bool {
        self.input(cx, |window, _| {
            window.try_find("live-grid-drop-gap").is_some()
        })
    }

    fn drag(&self, cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>) {
        self.input(cx, |window, cx| window.drag(from, to, cx));
    }

    /// Presses at `from`, drags through `path`, and keeps the button down.
    fn press_and_move(&self, cx: &mut TestAppContext, from: Point<Pixels>, path: &[Point<Pixels>]) {
        self.input(cx, |window, cx| {
            mouse_down_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
            for position in path {
                drag_move_to(window, *position, cx);
            }
        });
    }

    fn release(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        self.input(cx, |window, cx| {
            mouse_up_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
            window.render_frame(cx);
        });
    }

    fn dispatch(&self, cx: &mut TestAppContext, action: impl Action) {
        self.input(cx, |window, cx| {
            window.dispatch_action(action.boxed_clone(), cx);
            window.render_frame(cx);
        });
    }

    fn width(&self, cx: &mut TestAppContext, column: Col) -> Pixels {
        self.grid.read_with(cx, |grid, _| grid.column_width(column))
    }

    fn columns(&self, cx: &mut TestAppContext) -> Vec<Col> {
        self.grid.read_with(cx, |grid, _| grid.columns().collect())
    }

    fn zone(&self, cx: &mut TestAppContext, zone: ColumnZone) -> Vec<Col> {
        self.grid.read_with(cx, |grid, _| {
            grid.columns()
                .filter(|column| grid.column_zone(*column) == zone)
                .collect()
        })
    }

    fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    /// Every `LayoutChange` so far, then forgets them.
    fn take_layout_events(&self) -> Vec<(LayoutParts, ColumnSet, LayoutOrigin)> {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        events
            .into_iter()
            .filter_map(|event| match event {
                LiveGridEvent::LayoutChange {
                    parts,
                    columns,
                    origin,
                } => Some((parts, columns, origin)),
                _ => None,
            })
            .collect()
    }

    fn write_bids(&self, cx: &mut TestAppContext, count: u32, sign: i64) {
        let writer = self.grid.read_with(cx, |grid, _| grid.writer());
        for id in 0..count {
            writer.upsert(quote(id, sign));
        }
    }
}

fn width_part() -> LayoutParts {
    LayoutPart::Width.into()
}

fn order_part() -> LayoutParts {
    LayoutPart::Order.into()
}

fn zone_and_order() -> LayoutParts {
    [LayoutPart::Zone, LayoutPart::Order].into_iter().collect()
}

/// The room a sortable column's header keeps for the sort indicator.
const SORT_LANE: Pixels = px(28.);

/// The width autofit gives a column: the widest of the title (header font)
/// with the sort indicator lane and the painted texts (body font with
/// tabular figures), plus the cell padding on both sides and the grid line.
fn fitted(window: &Window, title: &str, texts: &[String]) -> Pixels {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let measure = |text: &str, font: &Font| {
        let runs = [TextRun {
            len: text.len(),
            font: font.clone(),
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        window
            .text_system()
            .layout_line(text, font_size, &runs, None)
            .width
    };
    let mut body = style.font();
    body.features = FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
    let widest = texts.iter().map(|text| measure(text, &body)).fold(
        measure(title, &style.font()) + SORT_LANE,
        |widest, width| widest.max(width),
    );
    widest.ceil() + px(13.)
}

#[gpui_kit::test]
fn test_resize_by_dragging_header_edge(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    let handle = fixture.bounds(cx, "live-grid-resize", "symbol");
    assert_eq!(handle.size.width, px(8.), "4 px on each side of the edge");
    assert_eq!(handle.center().x, px(140.));
    let bid = fixture.bounds(cx, "live-grid-header", "bid");

    let start = handle.center();
    fixture.drag(cx, start, start + point(px(40.), px(0.)));
    assert_eq!(fixture.width(cx, Col::Symbol), px(120.));
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "symbol").size.width,
        px(120.)
    );
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "bid").origin.x,
        bid.origin.x + px(40.),
        "the next column moved right"
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(width_part(), set([Col::Symbol]), LayoutOrigin::User)],
        "one event per gesture, after the release"
    );

    // Far to the left: the minimum width.
    let start = fixture.bounds(cx, "live-grid-resize", "symbol").center();
    fixture.drag(cx, start, point(px(0.), start.y));
    assert_eq!(fixture.width(cx, Col::Symbol), MIN_COLUMN_WIDTH);
    assert_eq!(fixture.take_layout_events().len(), 1);

    // A press without a move resizes nothing and says nothing.
    let start = fixture.bounds(cx, "live-grid-resize", "symbol").center();
    fixture.press_and_move(cx, start, &[]);
    fixture.release(cx, start);
    assert!(fixture.take_layout_events().is_empty());

    // A column that is not resizable has no handle.
    let schema = schema_with(
        GridColumn::new(Col::Symbol, "Symbol")
            .with_width(px(80.))
            .with_resizable(false),
    );
    let (grid, events) = new_grid_with(cx, schema, 30, |grid| grid);
    let fixed = show(cx, grid, events);
    assert!(!fixed.has(cx, "live-grid-resize", "symbol"));
    assert!(fixed.has(cx, "live-grid-resize", "bid"));
}

#[gpui_kit::test]
fn test_resize_in_right_zone_moves_left_edge(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    let total = fixture.bounds(cx, "live-grid-header", "total");
    assert_eq!((total.left(), total.right()), (px(340.), px(400.)));
    // The right zone is pinned to the right edge: its handle is on the
    // column's left edge, and dragging it left widens the column.
    let handle = fixture.bounds(cx, "live-grid-resize", "total").center();
    assert_eq!(handle.x, px(340.));
    fixture.drag(cx, handle, handle - point(px(40.), px(0.)));
    assert_eq!(fixture.width(cx, Col::Total), px(100.));
    let total = fixture.bounds(cx, "live-grid-header", "total");
    assert_eq!((total.left(), total.right()), (px(300.), px(400.)));
    assert_eq!(
        fixture.take_layout_events(),
        [(width_part(), set([Col::Total]), LayoutOrigin::User)]
    );
}

#[gpui_kit::test]
fn test_double_click_resize_handle_autofits(cx: &mut TestAppContext) {
    let fixture = open(cx, 600, |grid| grid);
    let keys = fixture.visible_keys(cx);
    assert_eq!(keys, (0..10).collect::<Vec<_>>());
    fixture.input(cx, |window, cx| {
        window.within("live-grid-resize").double_click("symbol", cx)
    });
    fixture.settle(cx);
    let texts: Vec<String> = keys.iter().map(|id| symbol(*id)).collect();
    let expected = fixture.input(cx, |window, _| fitted(window, "Symbol", &texts));
    assert_eq!(fixture.width(cx, Col::Symbol), expected);
    assert!(
        expected < px(200.),
        "row 500 is not painted, so its long text is not measured"
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(width_part(), set([Col::Symbol]), LayoutOrigin::User)]
    );

    // An empty grid fits the title and the sort indicator lane (Symbol:
    // Bid would fit its 70 px exactly and change nothing).
    let empty = open(cx, 0, |grid| grid);
    empty.input(cx, |window, cx| {
        window.within("live-grid-resize").double_click("symbol", cx)
    });
    empty.settle(cx);
    let expected = empty.input(cx, |window, _| fitted(window, "Symbol", &[]));
    assert_eq!(empty.width(cx, Col::Symbol), expected);
    assert_eq!(empty.take_layout_events().len(), 1);
}

#[gpui_kit::test]
fn test_autofit_waits_for_first_painted_frame(cx: &mut TestAppContext) {
    let (grid, events) = new_grid(cx, 30, |grid| grid);
    grid.update(cx, |grid, cx| grid.autofit_columns(cx));
    cx.run_until_parked();
    assert_eq!(
        grid.read_with(cx, |grid, _| grid.column_width(Col::Symbol)),
        px(80.)
    );
    assert!(events.borrow().is_empty(), "nothing is painted yet");

    let fixture = show(cx, grid, events);
    fixture.settle(cx);
    let keys = fixture.visible_keys(cx);
    let texts: Vec<String> = keys.iter().map(|id| symbol(*id)).collect();
    let expected = fixture.input(cx, |window, _| fitted(window, "Symbol", &texts));
    assert_eq!(fixture.width(cx, Col::Symbol), expected);
    let events = fixture.take_layout_events();
    assert_eq!(events.len(), 1, "one event for every column: {events:?}");
    assert_eq!(events[0].0, width_part());
    assert!(events[0].1.contains(Col::Symbol.ix()));
    assert_eq!(events[0].2, LayoutOrigin::Programmatic);
}

#[gpui_kit::test]
fn test_reorder_by_dragging_header_shows_gap(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    // Size (280..350) to the left of the first scrolling column (x 60).
    let size = fixture.bounds(cx, "live-grid-header", "size");
    let from = point(px(315.), size.center().y);
    let to = point(px(99.), size.center().y);
    fixture.press_and_move(cx, from, &[from - point(px(10.), px(0.)), to]);
    let gap = fixture.input(cx, |window, _| window.find("live-grid-drop-gap").bounds());
    assert_eq!((gap.origin.x, gap.size.width), (px(59.), px(2.)));
    assert!(
        fixture.take_layout_events().is_empty(),
        "no event mid-gesture"
    );

    fixture.release(cx, to);
    assert_eq!(
        fixture.columns(cx),
        [
            Col::Id,
            Col::Size,
            Col::Symbol,
            Col::Bid,
            Col::Ask,
            Col::Note,
            Col::Total
        ]
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(order_part(), set([Col::Size]), LayoutOrigin::User)]
    );
    assert!(!fixture.has_drop_gap(cx));
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "size").origin.x,
        px(60.)
    );
}

#[gpui_kit::test]
fn test_reorder_stays_in_zone(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    let before = fixture.columns(cx);
    // A scrolling column over the left zone: no gap, nothing on release.
    let from = point(px(100.), px(HEADER / 2.));
    let to = point(px(30.), px(HEADER / 2.));
    fixture.press_and_move(cx, from, &[from + point(px(10.), px(0.)), to]);
    assert!(!fixture.has_drop_gap(cx));
    fixture.release(cx, to);
    assert_eq!(fixture.columns(cx), before);
    // Over the right zone, and outside the window, the same.
    fixture.press_and_move(
        cx,
        from,
        &[from + point(px(10.), px(0.)), point(px(370.), px(20.))],
    );
    assert!(!fixture.has_drop_gap(cx));
    fixture.release(cx, point(px(370.), px(20.)));
    fixture.press_and_move(
        cx,
        from,
        &[from + point(px(10.), px(0.)), point(px(200.), px(-40.))],
    );
    assert!(!fixture.has_drop_gap(cx));
    fixture.release(cx, point(px(200.), px(-40.)));
    fixture.settle(cx);
    assert_eq!(fixture.columns(cx), before);
    assert!(fixture.take_layout_events().is_empty());
}

fn band_cell(first: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Name("quote".into())), first.into())
}

#[gpui_kit::test]
fn test_band_moves_as_block(cx: &mut TestAppContext) {
    // Banded (default): a column cannot leave its band.
    let fixture = open(cx, 30, |grid| grid);
    let band = fixture.input(cx, |window, _| {
        window.within("live-grid-bands").find(band_cell("bid"))
    });
    // A column header of the one header row.
    assert_eq!(band.role(), Some(Role::ColumnHeader));
    assert_eq!(band.label(), Some("Quote"));
    assert!(band.path().contains(&ElementId::from("live-grid-header")));
    let band = band.bounds();
    assert_eq!((band.left(), band.right()), (px(140.), px(280.)));
    let bid = fixture.bounds(cx, "live-grid-header", "bid").center();
    let to = point(px(99.), bid.y);
    fixture.press_and_move(cx, bid, &[bid - point(px(10.), px(0.)), to]);
    assert!(!fixture.has_drop_gap(cx), "out of the band is refused");
    fixture.release(cx, to);
    assert_eq!(fixture.take_layout_events(), []);

    // The band cell drags the whole band.
    let from = band.center();
    let to = point(px(99.), from.y);
    fixture.press_and_move(cx, from, &[from - point(px(10.), px(0.)), to]);
    assert!(fixture.has_drop_gap(cx));
    fixture.release(cx, to);
    assert_eq!(
        fixture.zone(cx, ColumnZone::Scrolling),
        [Col::Bid, Col::Ask, Col::Symbol, Col::Size, Col::Note]
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(order_part(), set([Col::Bid, Col::Ask]), LayoutOrigin::User)]
    );

    // Free moves: the column leaves its band, which splits in two cells.
    let fixture = open(cx, 30, |grid| grid.with_column_move_policy(FreeMove));
    let bid = fixture.bounds(cx, "live-grid-header", "bid").center();
    let to = point(px(99.), bid.y);
    fixture.press_and_move(cx, bid, &[bid - point(px(10.), px(0.)), to]);
    fixture.release(cx, to);
    assert_eq!(
        fixture.zone(cx, ColumnZone::Scrolling),
        [Col::Bid, Col::Symbol, Col::Ask, Col::Size, Col::Note]
    );
    assert!(fixture.has(cx, "live-grid-bands", band_cell("bid")));
    assert!(fixture.has(cx, "live-grid-bands", band_cell("ask")));
}

#[gpui_kit::test]
fn test_frozen_right_zone_ignores_horizontal_scroll(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    let id = fixture.bounds(cx, "live-grid-header", "id");
    let total = fixture.bounds(cx, "live-grid-header", "total");
    let symbol = fixture.bounds(cx, "live-grid-header", "symbol");
    fixture.input(cx, |window, cx| {
        window.scroll(
            "live-grid",
            ScrollDelta::Pixels(point(px(-50.), px(0.))),
            cx,
        )
    });
    fixture.frame(cx);
    assert_eq!(fixture.bounds(cx, "live-grid-header", "id"), id);
    assert_eq!(fixture.bounds(cx, "live-grid-header", "total"), total);
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "symbol").origin.x,
        symbol.origin.x - px(50.)
    );

    // A click in the right zone selects the row by its key.
    let keys = fixture.visible_keys(cx);
    fixture.input(cx, |window, cx| {
        window.click_at("live-grid", point(px(370.), px(HEADER + ROW * 2. + 5.)), cx)
    });
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.selection().iter().next().copied()),
        Some(keys[2])
    );
}

#[gpui_kit::test]
fn test_hide_show_and_zone_by_api_and_actions(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    // The API: one event per call, `Programmatic`.
    let hidden = fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Col::Note, false, cx));
    assert!(hidden);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_zone(Col::Size, ColumnZone::FrozenLeft, cx)
    });
    assert_eq!(
        fixture.take_layout_events(),
        [
            (
                LayoutPart::Visibility.into(),
                set([Col::Note]),
                LayoutOrigin::Programmatic
            ),
            (
                zone_and_order(),
                set([Col::Size]),
                LayoutOrigin::Programmatic
            ),
        ]
    );
    assert_eq!(
        fixture.zone(cx, ColumnZone::FrozenLeft),
        [Col::Id, Col::Size]
    );
    fixture.frame(cx);

    // The Actions act on the column pressed last: `User`.
    fixture.input(cx, |window, cx| {
        window.within("live-grid-header").click("symbol", cx)
    });
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Symbol)
    );
    fixture.dispatch(cx, HideColumn);
    assert!(
        !fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Symbol))
    );
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        None,
        "a hidden column is not current"
    );
    fixture.dispatch(cx, HideColumn);
    fixture.dispatch(cx, ShowAllColumns);
    assert_eq!(
        fixture.take_layout_events(),
        [
            (
                LayoutPart::Visibility.into(),
                set([Col::Symbol]),
                LayoutOrigin::User
            ),
            (
                LayoutPart::Visibility.into(),
                set([Col::Symbol, Col::Note]),
                LayoutOrigin::User
            ),
        ]
    );
    // Shown columns are back at their places.
    assert_eq!(
        fixture.zone(cx, ColumnZone::Scrolling),
        [Col::Symbol, Col::Bid, Col::Ask, Col::Note]
    );

    // Freezing a banded column takes its whole band, to the right zone's
    // start; unfreezing returns it to the end of the scrolling zone.
    fixture.input(cx, |window, cx| {
        window.within("live-grid-header").click("bid", cx)
    });
    fixture.dispatch(cx, FreezeColumnRight);
    assert_eq!(
        fixture.zone(cx, ColumnZone::FrozenRight),
        [Col::Bid, Col::Ask, Col::Total]
    );
    fixture.dispatch(cx, UnfreezeColumn);
    assert_eq!(
        fixture.zone(cx, ColumnZone::Scrolling),
        [Col::Symbol, Col::Note, Col::Bid, Col::Ask]
    );
    // Moves stay inside the band.
    fixture.dispatch(cx, MoveColumnLeft);
    fixture.dispatch(cx, MoveColumnRight);
    assert_eq!(
        fixture.zone(cx, ColumnZone::Scrolling),
        [Col::Symbol, Col::Note, Col::Ask, Col::Bid]
    );
    let zone_and_order = zone_and_order();
    assert_eq!(
        fixture.take_layout_events(),
        [
            (
                zone_and_order,
                set([Col::Bid, Col::Ask]),
                LayoutOrigin::User
            ),
            (
                zone_and_order,
                set([Col::Bid, Col::Ask]),
                LayoutOrigin::User
            ),
            (order_part(), set([Col::Bid]), LayoutOrigin::User),
        ]
    );
    fixture.dispatch(cx, AutofitColumns);
    fixture.settle(cx);
    let events = fixture.take_layout_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].2, LayoutOrigin::User);

    // The last visible column stays.
    let refused = fixture.grid.update(cx, |grid, cx| {
        for column in [
            Col::Id,
            Col::Size,
            Col::Symbol,
            Col::Note,
            Col::Ask,
            Col::Bid,
        ] {
            assert!(grid.set_column_visible(column, false, cx));
        }
        !grid.set_column_visible(Col::Total, false, cx)
    });
    assert!(refused);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.visible_columns().collect::<Vec<_>>()),
        [Col::Total]
    );
}

#[gpui_kit::test]
fn test_column_gestures_do_not_touch_row_order(cx: &mut TestAppContext) {
    let by_bid = || ViewSpec::new().with_sort([(Col::Bid, SortDirection::Descending)]);
    // A header dragged over the body does not hold a new order.
    let fixture = open(cx, 30, |grid| {
        grid.with_view(by_bid())
            .with_tuning(unthrottled())
            .with_reorder_policy(HoldReorder::new(Duration::from_secs(60)))
    });
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
    let from = point(px(100.), px(HEADER / 2.));
    let over_body = point(px(100.), px(HEADER + ROW * 4.));
    fixture.press_and_move(cx, from, &[from + point(px(5.), px(0.)), over_body]);
    fixture.write_bids(cx, 30, -1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    assert!(!fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    fixture.press_and_move(cx, over_body, &[from]);
    fixture.release(cx, from);
    assert!(fixture.take_layout_events().is_empty());

    // A held order survives resize, reorder and hiding; so does the selection.
    let fixture = open(cx, 30, |grid| {
        grid.with_view(by_bid())
            .with_tuning(unthrottled())
            .with_reorder_policy(ManualReorder)
    });
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(25), cx));
    fixture.write_bids(cx, 30, -1);
    fixture.settle(cx);
    let shown: Vec<u32> = (20..30).rev().collect();
    assert_eq!(fixture.visible_keys(cx), shown);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));

    let handle = fixture.bounds(cx, "live-grid-resize", "symbol").center();
    fixture.drag(cx, handle, handle + point(px(30.), px(ROW * 5.)));
    let size = fixture.bounds(cx, "live-grid-header", "size");
    let size = point(size.left() + px(10.), px(HEADER / 2.));
    let to = point(px(99.), px(HEADER / 2.));
    fixture.press_and_move(cx, size, &[size - point(px(10.), px(0.)), to]);
    fixture.release(cx, to);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Col::Note, false, cx));
    fixture.settle(cx);
    assert_eq!(fixture.take_layout_events().len(), 3);
    assert_eq!(fixture.visible_keys(cx), shown);
    fixture.grid.read_with(cx, |grid, _| {
        assert!(grid.is_order_held());
        assert_eq!(grid.selection().iter().next().copied(), Some(25));
    });
}

#[gpui_kit::test]
fn test_column_ops_on_hidden_grid(cx: &mut TestAppContext) {
    let (grid, events) = new_grid(cx, 30, |grid| grid);
    grid.update(cx, |grid, cx| {
        assert!(grid.set_column_visible(Col::Note, false, cx));
        grid.set_column_width(Col::Symbol, px(150.), cx);
        grid.set_column_zone(Col::Size, ColumnZone::FrozenRight, cx);
    });
    assert_eq!(events.borrow().len(), 3, "the API changes state at once");
    let fixture = show(cx, grid, events);
    assert!(!fixture.has(cx, "live-grid-header", "note"));
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "symbol").size.width,
        px(150.)
    );
    // Size went to the start of the right zone: 270..340, then Total.
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "size").origin.x,
        px(270.)
    );
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
}

#[gpui_kit::test]
fn test_horizontal_anchor_keeps_first_visible_column(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    // Symbol (80 px) is scrolled out; Bid is the first scrolling column on
    // screen, 20 px under the left zone.
    // The place of Bid in the scrolling zone, whose left edge is the right
    // edge of the frozen Id (autofit resizes Id too).
    let bid_in_zone = |cx: &mut TestAppContext| {
        fixture.bounds(cx, "live-grid-header", "bid").origin.x
            - fixture.bounds(cx, "live-grid-header", "id").right()
    };
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(100.), cx));
    fixture.frame(cx);
    assert_eq!(bid_in_zone(cx), px(-20.));

    // Fitting every column changes the width of Symbol, left of the screen.
    fixture.grid.update(cx, |grid, cx| grid.autofit_columns(cx));
    fixture.settle(cx);
    assert_ne!(fixture.width(cx, Col::Symbol), px(80.));
    assert_eq!(bid_in_zone(cx), px(-20.));

    // Hiding it does not move Bid either.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_visible(Col::Symbol, false, cx)
    });
    fixture.frame(cx);
    assert_eq!(bid_in_zone(cx), px(-20.));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid
            .column_at(gpui_kit_grid::ViewColumnIx::new(1))),
        Some(Col::Bid)
    );
}

/// A zone scrolled to its start stays there: a column shown before the
/// first one on screen comes into view, while a scrolled zone keeps its
/// first column in place.
#[gpui_kit::test]
fn test_horizontal_start_stays_at_start(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, |grid| grid);
    let scroll_x = |cx: &mut TestAppContext| fixture.grid.read_with(cx, |grid, _| grid.scroll_x());
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_visible(Col::Symbol, false, cx)
    });
    fixture.frame(cx);
    assert_eq!(scroll_x(cx), px(0.));
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_visible(Col::Symbol, true, cx)
    });
    fixture.frame(cx);
    assert_eq!(scroll_x(cx), px(0.));
    assert_eq!(
        fixture.bounds(cx, "live-grid-header", "symbol").origin.x,
        fixture.bounds(cx, "live-grid-header", "id").right()
    );

    // Scrolled past Symbol: hiding and showing it keeps Bid in place.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(100.), cx));
    fixture.frame(cx);
    let bid = fixture.bounds(cx, "live-grid-header", "bid").origin.x;
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_visible(Col::Symbol, false, cx);
        grid.set_column_visible(Col::Symbol, true, cx);
    });
    fixture.frame(cx);
    assert_eq!(fixture.bounds(cx, "live-grid-header", "bid").origin.x, bid);
}

// Header icons and columns the user may not hide.

/// [`schema`] with a search icon before the title of Symbol, a check icon
/// alone in the header of Size, and an Id the user may not hide.
fn icon_schema() -> GridSchema<Col> {
    GridSchema::new()
        .column(
            GridColumn::new(Col::Id, "Id")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenLeft)
                .with_hideable(false),
        )
        .column(
            GridColumn::new(Col::Symbol, "Symbol")
                .with_width(px(80.))
                .with_icon(IconName::Search),
        )
        .band(
            ColumnBand::new("quote", "Quote")
                .column(GridColumn::new(Col::Bid, "Bid").with_width(px(70.)))
                .column(GridColumn::new(Col::Ask, "Ask").with_width(px(70.))),
        )
        .column(
            GridColumn::new(Col::Size, "Size")
                .with_width(px(70.))
                .with_icon_only(IconName::Check),
        )
        .column(GridColumn::new(Col::Note, "Note").with_width(px(100.)))
        .column(
            GridColumn::new(Col::Total, "Total")
                .with_width(px(60.))
                .with_zone(ColumnZone::FrozenRight),
        )
}

/// Records what `header_tooltip` is asked about and what it gives by
/// default: the column, whether its title is truncated, the tooltip text.
#[derive(Clone, Default)]
struct HeaderTips(Rc<RefCell<Vec<(Col, bool, Option<SharedString>)>>>);

impl LiveGridDelegate for HeaderTips {
    type Row = Quote;

    fn header_tooltip(
        &self,
        header: &HeaderTooltipContext<'_, Col>,
        _: &App,
    ) -> Option<CellTooltip> {
        let tooltip = header.default_tooltip();
        self.0.borrow_mut().push((
            header.column(),
            header.is_truncated(),
            tooltip
                .as_ref()
                .and_then(|tooltip| tooltip.as_text().cloned()),
        ));
        tooltip
    }
}

struct TipsRoot {
    grid: Entity<LiveGridState<HeaderTips>>,
}

impl Render for TipsRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .xsmall()
                .bordered(false)
                .group_panel(true),
        )
    }
}

/// The width of `title` in the header font.
fn title_width(window: &Window, title: &str) -> Pixels {
    let style = window.text_style();
    let runs = [TextRun {
        len: title.len(),
        font: style.font(),
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    window
        .text_system()
        .layout_line(
            title,
            style.font_size.to_pixels(window.rem_size()),
            &runs,
            None,
        )
        .width
}

#[gpui_kit::test]
fn test_icon_header(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let tips = HeaderTips::default();
    let grid = cx.new(|cx| {
        LiveGridState::new_standalone(icon_schema(), tips.clone(), StoreOptions::default(), cx)
            .with_tuning(unthrottled())
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all((0..30).map(|id| Arc::new(quote(id, 1))));
    let (window, _) = cx
        .update(|cx| {
            let grid = grid.clone();
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(WIDTH), px(HEIGHT + ROW)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| cx.new(|_| TipsRoot { grid }),
            )
        })
        .expect("open window");
    let settle = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
        }
    };
    settle(cx);
    let input = |cx: &mut TestAppContext, f: &mut dyn FnMut(&mut Window, &mut gpui_kit::App)| {
        cx.update_window(window, |_, window, cx| f(window, cx))
            .expect("window is open")
    };
    let icon = |cx: &mut TestAppContext, column: &'static str| {
        let mut found = None;
        input(cx, &mut |window, _| {
            found = window
                .within("live-grid-header")
                .within(column)
                .try_find("header-icon")
                .map(|icon| icon.bounds());
        });
        found
    };
    let cell = |cx: &mut TestAppContext, column: &'static str| {
        let mut found = None;
        input(cx, &mut |window, _| {
            found = Some(window.within("live-grid-header").find(column));
        });
        found.expect("the header cell")
    };

    // An icon before the title of Symbol, and alone in Size, 14 px in an
    // `xsmall` grid; no icon in Bid.
    let symbol_icon = icon(cx, "symbol").expect("an icon before the title");
    assert_eq!(symbol_icon.size.width, px(14.));
    let symbol = cell(cx, "symbol");
    assert_eq!(symbol_icon.left(), symbol.bounds().left() + px(6.));
    assert!(icon(cx, "bid").is_none());
    // Alone, the icon is centered; the title stays the accessible name.
    let size_icon = icon(cx, "size").expect("the icon alone");
    let size_cell = cell(cx, "size");
    assert!((size_icon.center().x - size_cell.bounds().center().x).abs() < px(1.));
    assert_eq!(size_cell.label(), Some("Size"));
    assert_eq!(symbol.label(), Some("Symbol"));

    // The default tooltip of a header that shows its icon alone is its
    // title, truncated or not.
    let hover = |cx: &mut TestAppContext, column: &'static str| {
        let center = cell(cx, column).bounds().center();
        input(cx, &mut |window, cx| move_pointer_to(window, center, cx));
        settle(cx);
        tips.0.borrow().last().cloned()
    };
    assert_eq!(
        hover(cx, "size"),
        Some((Col::Size, false, Some("Size".into())))
    );
    // An icon before a title takes room from it: a width the title alone
    // fits truncates it beside the icon.
    let mut title = px(0.);
    input(cx, &mut |window, _| title = title_width(window, "Symbol"));
    // The padding, the icon and its gap: 12 + 14 + 4.
    grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Symbol, title.ceil() + px(12. + 18. + 1.), cx)
    });
    settle(cx);
    assert_eq!(hover(cx, "symbol"), Some((Col::Symbol, false, None)));
    grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Symbol, title.ceil() + px(12. + 10.), cx)
    });
    settle(cx);
    assert_eq!(
        hover(cx, "symbol"),
        Some((Col::Symbol, true, Some("Symbol".into())))
    );

    // Autofit of the icon alone: the icon and the sort indicator lane, wider
    // than the cells' "100", the cell padding and the grid line.
    grid.update(cx, |grid, cx| grid.autofit_column(Col::Size, cx));
    settle(cx);
    assert_eq!(
        grid.read_with(cx, |grid, _| grid.column_width(Col::Size)),
        px(14.) + SORT_LANE + px(13.)
    );

    // A chip of the group panel names the column by its title.
    grid.update(cx, |grid, cx| {
        grid.set_group_by([gpui_kit_grid::GroupBy::column(Col::Size)], cx)
    });
    settle(cx);
    let mut chips = Vec::new();
    input(cx, &mut |window, _| {
        chips = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|item| item.role() == Some(Role::Button))
            .filter_map(|item| item.label().map(str::to_owned))
            .filter(|label| label.starts_with("Size"))
            .collect();
    });
    assert_eq!(chips, ["Size, Sorted ascending"]);
}

#[gpui_kit::test]
fn test_hideable_column(cx: &mut TestAppContext) {
    cx.update(gpui_kit_grid::init);
    let (grid, events) = new_grid_with(cx, icon_schema(), 10, |grid| grid);
    let fixture = show(cx, grid, events);
    fixture.take_layout_events();

    // `HideColumn` leaves a column the user may not hide alone.
    fixture.input(cx, |window, cx| {
        window.within("live-grid-header").click("id", cx)
    });
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Id)
    );
    fixture.dispatch(cx, HideColumn);
    fixture.settle(cx);
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Id))
    );
    assert!(fixture.take_layout_events().is_empty());

    // Its "Hide column" is disabled: a click leaves the menu open and the
    // column shown. Another column's hides it.
    let click_hide = |cx: &mut TestAppContext, column: &'static str| {
        let center = fixture.bounds(cx, "live-grid-header", column).center();
        fixture.input(cx, |window, cx| {
            press_at(
                window,
                center,
                MouseButton::Right,
                Modifiers::default(),
                1,
                cx,
            )
        });
        fixture.settle(cx);
        let item = fixture.input(cx, |window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|item| {
                    item.role() == Some(Role::MenuItem)
                        && item.visible()
                        && item.label() == Some("Hide column")
                })
                .expect("a Hide column item")
                .bounds()
                .center()
        });
        fixture.input(cx, |window, cx| {
            press_at(window, item, MouseButton::Left, Modifiers::default(), 1, cx)
        });
        fixture.settle(cx);
    };
    click_hide(cx, "id");
    assert!(
        fixture.grid.read_with(cx, |grid, _| grid.is_menu_open()),
        "a disabled item does not close the menu"
    );
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Id))
    );
    assert!(fixture.take_layout_events().is_empty());
    fixture.input(cx, |window, cx| window.press("escape", cx));
    fixture.settle(cx);
    click_hide(cx, "bid");
    assert!(
        !fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Bid))
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(
            LayoutPart::Visibility.into(),
            set([Col::Bid]),
            LayoutOrigin::User
        )]
    );

    // The API hides it.
    assert!(
        fixture
            .grid
            .update(cx, |grid, cx| grid.set_column_visible(Col::Id, false, cx))
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(
            LayoutPart::Visibility.into(),
            set([Col::Id]),
            LayoutOrigin::Programmatic
        )]
    );
}

#[gpui_kit::test]
fn test_icon_title_in_exports(cx: &mut TestAppContext) {
    let (grid, events) = new_grid_with(cx, icon_schema(), 3, |grid| grid);
    let fixture = show(cx, grid, events);
    // The header line names a column that shows its icon alone by its
    // title.
    assert!(fixture.grid.update(cx, |grid, cx| grid.copy_all(cx)));
    let text = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("copied");
    assert_eq!(
        text.lines().next(),
        Some("Id\tSymbol\tBid\tAsk\tSize\tNote\tTotal")
    );
}
