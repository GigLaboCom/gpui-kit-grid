//! Headless UI tests of custom row keys (`.agents/COMPONENT_TEST_RULES.md`):
//! a composite key that is not a column, and a key with hand-written
//! equality. Upserts replace and removals find rows by key equality; the
//! selection, the focus and the events follow keys through a re-sort, a
//! filter and live updates; the command scope uses the keys, and a copy
//! writes the columns, never the key. Assertions read the state, the events
//! and the test clipboard, not pixels.

mod common;

use std::{
    cell::RefCell,
    collections::HashSet,
    hash::{Hash, Hasher},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, Entity, Focusable as _, IntoElement,
    Modifiers, MouseButton, ParentElement as _, Render, Styled as _, TestAppContext, Window,
    WindowBounds, WindowOptions, div, point, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellValue, ColumnIx, ColumnSet, GridColumn, GridColumns, GridRow, GridSchema, GridTarget,
    GridTuning, GridWriter, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState,
    SelectionMode, SortDirection, StoreOptions, ViewSpec,
};

use crate::common::{ROW, press_at};

use OrderColumn::{Price, Symbol};
use SortDirection::Descending;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Venue {
    Xnas,
    Xlon,
}

/// The key of an order: its venue and the venue's order id. Neither is a
/// column. No `Copy` and no `Ord`: the grid needs neither.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct OrderKey {
    venue: Venue,
    id: u64,
}

const NAS_1: OrderKey = OrderKey {
    venue: Venue::Xnas,
    id: 1,
};
const LON_1: OrderKey = OrderKey {
    venue: Venue::Xlon,
    id: 1,
};
const NAS_2: OrderKey = OrderKey {
    venue: Venue::Xnas,
    id: 2,
};
const LON_2: OrderKey = OrderKey {
    venue: Venue::Xlon,
    id: 2,
};
const NAS_3: OrderKey = OrderKey {
    venue: Venue::Xnas,
    id: 3,
};

/// The columns of both row types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OrderColumn {
    Symbol,
    Price,
}

impl GridColumns for OrderColumn {
    fn all() -> &'static [Self] {
        &[Self::Symbol, Self::Price]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Symbol => "symbol",
            Self::Price => "price",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Debug)]
struct Order {
    key: OrderKey,
    symbol: &'static str,
    price: i64,
}

impl GridRow for Order {
    type Key = OrderKey;
    type Column = OrderColumn;

    fn key(&self) -> &OrderKey {
        &self.key
    }

    fn value(&self, column: OrderColumn) -> CellValue<'_> {
        match column {
            Symbol => CellValue::Text(self.symbol),
            Price => CellValue::Int(self.price),
        }
    }
}

fn order(key: OrderKey, symbol: &'static str, price: i64) -> Arc<Order> {
    Arc::new(Order { key, symbol, price })
}

/// Five orders in insertion order; `NAS_1` and `LON_1` differ only in the
/// venue.
fn orders() -> Vec<Arc<Order>> {
    vec![
        order(NAS_1, "AAPL", 10),
        order(LON_1, "VOD", 20),
        order(NAS_2, "MSFT", 30),
        order(LON_2, "BP", 40),
        order(NAS_3, "AMZN", 50),
    ]
}

struct Orders;

impl LiveGridDelegate for Orders {
    type Row = Order;
}

/// A ticker that compares and hashes ignoring ASCII case, as a key with an
/// `OrdinalIgnoreCase` comparer does.
#[derive(Clone, Debug)]
struct Ticker(Arc<str>);

impl PartialEq for Ticker {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for Ticker {}

impl Hash for Ticker {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Equal tickers hash the same bytes, so `Hash` agrees with `Eq`.
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_uppercase());
        }
        state.write_u8(0xff);
    }
}

fn ticker(text: &str) -> Ticker {
    Ticker(text.into())
}

/// A quote keyed by its ticker; the symbol column shows the ticker as the
/// producer wrote it.
#[derive(Debug)]
struct Quote {
    ticker: Ticker,
    price: i64,
}

impl GridRow for Quote {
    type Key = Ticker;
    type Column = OrderColumn;

    fn key(&self) -> &Ticker {
        &self.ticker
    }

    fn value(&self, column: OrderColumn) -> CellValue<'_> {
        match column {
            Symbol => CellValue::Text(&self.ticker.0),
            Price => CellValue::Int(self.price),
        }
    }
}

fn quote(text: &str, price: i64) -> Arc<Quote> {
    Arc::new(Quote {
        ticker: ticker(text),
        price,
    })
}

struct Quotes;

impl LiveGridDelegate for Quotes {
    type Row = Quote;
}

type Key<D> = <<D as LiveGridDelegate>::Row as GridRow>::Key;
type Events<D> = Rc<RefCell<Vec<LiveGridEvent<Key<D>, OrderColumn>>>>;

struct Root<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
}

impl<D: LiveGridDelegate> Render for Root<D> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .with_size(Size::XSmall)
                .bordered(false),
        )
    }
}

/// A grid in a window and its events.
struct Fixture<D: LiveGridDelegate> {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<D>>,
    events: Events<D>,
}

/// An `xsmall` grid of `rows` with the Symbol (x 0..100) and Price
/// (100..200) columns, configured by `configure`; its events so far
/// forgotten.
fn open<D>(
    cx: &mut TestAppContext,
    delegate: D,
    rows: Vec<Arc<D::Row>>,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> Fixture<D>
where
    D: LiveGridDelegate,
    D::Row: GridRow<Column = OrderColumn>,
{
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let schema = GridSchema::new()
        .column(GridColumn::new(Symbol, "Symbol"))
        .column(GridColumn::new(Price, "Price"));
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(400.), px(ROW * 11.)),
                    })),
                    ..Default::default()
                },
                cx,
                move |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            configure(
                                LiveGridState::new_standalone(
                                    schema,
                                    delegate,
                                    StoreOptions::default(),
                                    cx,
                                )
                                .with_tuning(
                                    GridTuning::default().with_min_rebuild_interval(Duration::ZERO),
                                ),
                            )
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Events::<D>::default();
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(
            &grid,
            move |_, event: &LiveGridEvent<Key<D>, OrderColumn>, _| {
                events.borrow_mut().push(event.clone());
            },
        )
        .detach();
    });
    grid.read_with(cx, |grid, _| grid.writer())
        .replace_all(rows);
    let fixture = Fixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    fixture
}

impl<D> Fixture<D>
where
    D: LiveGridDelegate,
    D::Row: GridRow<Column = OrderColumn>,
{
    /// Runs ingest, pulls and background jobs, then draws a frame.
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
        }
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

    /// The painted rows, top to bottom.
    fn shown(&self, cx: &mut TestAppContext) -> Vec<Arc<D::Row>> {
        self.grid.read_with(cx, |grid, _| {
            grid.visible_keys()
                .iter()
                .map(|key| grid.snapshot().get(key).expect("painted").clone())
                .collect()
        })
    }

    /// The row of `key` in the shown snapshot.
    fn row(&self, cx: &mut TestAppContext, key: &Key<D>) -> Option<Arc<D::Row>> {
        self.grid
            .read_with(cx, |grid, _| grid.snapshot().get(key).cloned())
    }

    /// The data rows of the shown order.
    fn row_count(&self, cx: &mut TestAppContext) -> usize {
        self.grid.read_with(cx, |grid, _| grid.projection().len())
    }

    fn selected(&self, cx: &mut TestAppContext) -> HashSet<Key<D>> {
        self.grid
            .read_with(cx, |grid, _| grid.selection().iter().cloned().collect())
    }

    fn focused(&self, cx: &mut TestAppContext) -> Option<(Key<D>, OrderColumn)> {
        self.grid.read_with(cx, |grid, _| {
            let cell = grid.focused_cell()?;
            Some((cell.row()?.clone(), cell.column()))
        })
    }

    fn scoped(&self, cx: &mut TestAppContext) -> Vec<Key<D>> {
        self.grid
            .read_with(cx, |grid, _| grid.scoped_selection().keys().to_vec())
    }

    /// Clicks the Price cell of screen line `line` with `modifiers` held.
    fn click(&self, cx: &mut TestAppContext, line: usize, modifiers: Modifiers) {
        let position = point(px(150.), px(ROW * (line as f32 + 1.5)));
        cx.update_window(self.window, |_, window, cx| {
            press_at(window, position, MouseButton::Left, modifiers, 1, cx)
        })
        .expect("window is open");
        self.settle(cx);
    }

    /// Focuses the grid and presses `key`.
    fn press(&self, cx: &mut TestAppContext, key: &str) {
        let grid = self.grid.clone();
        cx.update_window(self.window, |_, window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.press(key, cx);
        })
        .expect("window is open");
    }
}

/// `(key, price)` of each order.
fn prices(orders: &[Arc<Order>]) -> Vec<(OrderKey, i64)> {
    orders
        .iter()
        .map(|order| (order.key.clone(), order.price))
        .collect()
}

fn keys<const N: usize>(keys: [OrderKey; N]) -> HashSet<OrderKey> {
    keys.into_iter().collect()
}

fn cell(key: OrderKey, column: OrderColumn) -> GridTarget<OrderKey, OrderColumn> {
    GridTarget::Cell { key, column }
}

/// The events of an `Orders` grid that carry keys, in order.
#[derive(Debug, PartialEq)]
enum Seen {
    Selection(HashSet<OrderKey>),
    Focus(Option<(OrderKey, OrderColumn)>),
    Confirm(Vec<OrderKey>),
    Click(GridTarget<OrderKey, OrderColumn>),
}

impl Fixture<Orders> {
    /// Every followed event so far, then forgets them.
    fn take_events(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.events.borrow_mut())
            .into_iter()
            .filter_map(|event| match event {
                LiveGridEvent::SelectionChange { selection, .. } => {
                    Some(Seen::Selection(selection.iter().cloned().collect()))
                }
                LiveGridEvent::FocusChange { cell, .. } => Some(Seen::Focus(
                    cell.and_then(|cell| Some((cell.row()?.clone(), cell.column()))),
                )),
                LiveGridEvent::Confirm { keys, .. } => Some(Seen::Confirm(keys.to_vec())),
                LiveGridEvent::Click { target, .. } => Some(Seen::Click(target)),
                _ => None,
            })
            .collect()
    }
}

fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

#[gpui_kit::test]
fn test_composite_key_outside_the_columns(cx: &mut TestAppContext) {
    let fixture = open(cx, Orders, orders(), |grid| grid);
    // Keys that differ only in the venue are two rows.
    assert_eq!(
        prices(&fixture.shown(cx)),
        [
            (NAS_1, 10),
            (LON_1, 20),
            (NAS_2, 30),
            (LON_2, 40),
            (NAS_3, 50)
        ]
    );

    // An upsert under an equal key, built anew from its parts, replaces the
    // row in its place: still five rows, the new price shown, the other
    // venue's row untouched.
    fixture.writer(cx).upsert(order(
        OrderKey {
            venue: Venue::Xlon,
            id: 1,
        },
        "VOD",
        25,
    ));
    fixture.settle(cx);
    assert_eq!(fixture.row_count(cx), 5);
    assert_eq!(
        prices(&fixture.shown(cx)),
        [
            (NAS_1, 10),
            (LON_1, 25),
            (NAS_2, 30),
            (LON_2, 40),
            (NAS_3, 50)
        ]
    );

    // A removal by key takes that venue's row alone.
    fixture.writer(cx).remove(NAS_1);
    fixture.settle(cx);
    assert_eq!(fixture.row_count(cx), 4);
    assert!(fixture.row(cx, &NAS_1).is_none());
    assert_eq!(fixture.row(cx, &LON_1).map(|order| order.price), Some(25));
    assert_eq!(
        prices(&fixture.shown(cx)),
        [(LON_1, 25), (NAS_2, 30), (LON_2, 40), (NAS_3, 50)]
    );
}

#[gpui_kit::test]
fn test_key_with_custom_equality(cx: &mut TestAppContext) {
    let fixture = open(cx, Quotes, vec![quote("ABC", 1), quote("XYZ", 2)], |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });
    let shown = |cx: &mut TestAppContext| -> Vec<(String, i64)> {
        fixture
            .shown(cx)
            .iter()
            .map(|quote| (quote.ticker.0.to_string(), quote.price))
            .collect()
    };

    // "abc" equals "ABC": the upsert replaces that row, and the symbol shows
    // the new row's spelling.
    fixture.writer(cx).upsert(quote("abc", 10));
    fixture.settle(cx);
    assert_eq!(fixture.row_count(cx), 2);
    assert_eq!(shown(cx), [("abc".into(), 10), ("XYZ".into(), 2)]);
    for spelling in ["ABC", "abc", "aBc"] {
        assert_eq!(
            fixture.row(cx, &ticker(spelling)).map(|quote| quote.price),
            Some(10),
            "{spelling}"
        );
    }
    assert!(fixture.row(cx, &ticker("ABCD")).is_none());

    // The selection and removals match by the same equality.
    fixture.update(cx, |grid, cx| {
        grid.set_selected_keys([ticker("Abc"), ticker("xyz")], cx)
    });
    assert_eq!(fixture.selected(cx).len(), 2);
    fixture.writer(cx).remove(ticker("xYz"));
    fixture.settle(cx);
    assert_eq!(shown(cx), [("abc".into(), 10)]);
    assert_eq!(fixture.selected(cx), HashSet::from([ticker("ABC")]));
}

#[gpui_kit::test]
fn test_identity_follows_custom_keys(cx: &mut TestAppContext) {
    let fixture = open(cx, Orders, orders(), |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
    });

    // A click and a Cmd/Ctrl+click select two rows and focus the second;
    // the events carry the keys.
    fixture.click(cx, 1, Modifiers::default());
    fixture.click(cx, 2, Modifiers::secondary_key());
    assert_eq!(
        fixture.take_events(),
        [
            Seen::Selection(keys([LON_1])),
            Seen::Focus(Some((LON_1, Price))),
            Seen::Click(cell(LON_1, Price)),
            Seen::Selection(keys([LON_1, NAS_2])),
            Seen::Focus(Some((NAS_2, Price))),
            Seen::Click(cell(NAS_2, Price)),
        ]
    );

    // A re-sort, a filter that hides another row, and live updates move
    // both rows; the selection and the focus stay on their keys.
    fixture.update(cx, |grid, cx| grid.set_sort([(Price, Descending)], cx));
    fixture.settle(cx);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.visible_keys()),
        [NAS_3, LON_2, NAS_2, LON_1, NAS_1]
    );
    let mut symbol = ColumnSet::EMPTY;
    symbol.insert(Symbol.ix());
    fixture.update(cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new()
                .with_sort([(Price, Descending)])
                .with_filter(symbol, |order: &Order| order.symbol != "AAPL"),
            cx,
        )
    });
    fixture.settle(cx);
    let writer = fixture.writer(cx);
    writer.upsert(order(LON_1, "VOD", 60));
    writer.upsert(order(NAS_2, "MSFT", 5));
    fixture.settle(cx);
    assert_eq!(
        prices(&fixture.shown(cx)),
        [(LON_1, 60), (NAS_3, 50), (LON_2, 40), (NAS_2, 5)]
    );
    assert_eq!(fixture.selected(cx), keys([LON_1, NAS_2]));
    assert_eq!(fixture.focused(cx), Some((NAS_2, Price)));
    assert_eq!(fixture.take_events(), [], "no selection or focus change");

    // Enter confirms the selection in the new display order.
    fixture.press(cx, "enter");
    assert_eq!(fixture.take_events(), [Seen::Confirm(vec![LON_1, NAS_2])]);
}

#[gpui_kit::test]
fn test_scope_and_copy_with_custom_keys(cx: &mut TestAppContext) {
    let fixture = open(cx, Orders, orders(), |grid| {
        grid.with_selection_mode(SelectionMode::RowSet)
            .with_view(ViewSpec::new().with_sort([(Price, Descending)]))
    });

    // Selected in any order, scoped in the display order (price, highest
    // first).
    fixture.update(cx, |grid, cx| {
        grid.set_selected_keys([NAS_1, LON_2, NAS_2], cx)
    });
    assert_eq!(fixture.scoped(cx), [LON_2, NAS_2, NAS_1]);

    // A copy writes the visible columns of those rows: no venue, no id.
    assert!(fixture.update(cx, |grid, cx| grid.copy(cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("BP\t40\nMSFT\t30\nAAPL\t10"));

    // An update moves a selected row; the scope follows its key.
    fixture.writer(cx).upsert(order(NAS_1, "AAPL", 45));
    fixture.settle(cx);
    assert_eq!(fixture.scoped(cx), [NAS_1, LON_2, NAS_2]);
    assert!(fixture.update(cx, |grid, cx| grid.copy_all(cx)));
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("Symbol\tPrice\nAMZN\t50\nAAPL\t45\nBP\t40\nMSFT\t30\nVOD\t20")
    );
}
