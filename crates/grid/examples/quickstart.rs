//! The smallest complete gpui-kit-grid application: a trade blotter fed by a
//! producer thread. The README and the site's Getting started page show
//! this file's regions (`// #region <name>`), so keep them compiling and
//! self-explanatory.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example quickstart
//! ```

use std::{
    io::{Write as _, stdout},
    sync::Arc,
    time::Duration,
};

use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions,
    component::{ActiveTheme as _, Sizable as _},
    px, size,
};
use gpui_kit_grid::{
    CellStyle, CellStyleContext, CellValue, ColumnIx, GridColumn, GridColumns, GridRow, GridSchema,
    LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, SortDirection, StoreOptions,
    ViewSpec,
};

// #region columns
// 1. Columns: an enum. `id` is what saved layouts store, `ix` the schema index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TradeColumn {
    Id,
    Instrument,
    Side,
    Quantity,
    Price,
}

impl GridColumns for TradeColumn {
    fn all() -> &'static [Self] {
        use TradeColumn::*;
        &[Id, Instrument, Side, Quantity, Price]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Quantity => "quantity",
            Self::Price => "price",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}
// #endregion columns

// #region row
// 2. Rows: an immutable value with a key. `value` must not allocate: it runs
//    for every visible cell on every frame and for every row when sorting.
#[derive(Clone)]
struct Trade {
    id: u64,
    instrument: Arc<str>,
    sell: bool,
    quantity: i64,
    price: f64,
}

impl GridRow for Trade {
    type Key = u64;
    type Column = TradeColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: TradeColumn) -> CellValue<'_> {
        match column {
            TradeColumn::Id => CellValue::Int(self.id as i64),
            TradeColumn::Instrument => CellValue::Text(&self.instrument),
            TradeColumn::Side => CellValue::Text(if self.sell { "Sell" } else { "Buy" }),
            TradeColumn::Quantity => CellValue::Int(self.quantity),
            TradeColumn::Price => CellValue::Float(self.price),
        }
    }
}
// #endregion row

// #region delegate
// 3. Presentation hooks: `&self`, called for visible cells only.
struct Trades;

impl LiveGridDelegate for Trades {
    type Row = Trade;

    fn cell_style(&self, cell: &CellStyleContext<'_, Trade>, style: &mut CellStyle, cx: &App) {
        if cell.row().sell {
            style.set_foreground(cx.theme().blue);
        }
    }
}
// #endregion delegate

// #region window
// 4. The window content places the grid element; `small` = 30 px rows, the
//    name is what a screen reader calls the table.
struct TradesWindow {
    grid: Entity<LiveGridState<Trades>>,
}

impl Render for TradesWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        LiveGrid::new(&self.grid)
            .small()
            .accessibility_label("Trades")
    }
}
// #endregion window

fn trades_window(cx: &mut App) -> Entity<TradesWindow> {
    // #region state
    // 5. The grid state: schema, delegate, its own store, a sort.
    let schema = GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(TradeColumn::Id, "Id").with_width(px(80.)))
        .column(GridColumn::new(TradeColumn::Instrument, "Instrument").with_width(px(140.)))
        .column(GridColumn::new(TradeColumn::Side, "Side").with_width(px(80.)))
        .column(GridColumn::new(TradeColumn::Quantity, "Quantity"))
        .column(GridColumn::new(TradeColumn::Price, "Price"));
    let grid = cx.new(|cx| {
        LiveGridState::new_standalone(schema, Trades, StoreOptions::default(), cx)
            .with_view(ViewSpec::new().with_sort([(TradeColumn::Price, SortDirection::Descending)]))
    });
    // #endregion state

    // #region events
    // 6. Events: selection by key, Enter or a double click. `writeln!` instead
    //    of `println!`: a print to a closed stdout (a parent that exited,
    //    `| head`) panics, and a panic here aborts the app.
    cx.subscribe(
        &grid,
        |_, event: &LiveGridEvent<u64, TradeColumn>, _| match event {
            LiveGridEvent::SelectionChange { selection, origin } => {
                let keys: Vec<_> = selection.iter().collect();
                let _ = writeln!(stdout().lock(), "selected {keys:?} ({origin:?})");
            }
            LiveGridEvent::Confirm { keys, .. } => {
                let _ = writeln!(stdout().lock(), "open {keys:?}");
            }
            _ => {}
        },
    )
    .detach();
    // #endregion events

    // #region writer
    // 7. Data from any thread: replace everything, then upsert by key.
    let writer = grid.read(cx).writer();
    std::thread::spawn(move || {
        let instruments: Vec<Arc<str>> = ["SBER", "GAZP", "LKOH", "YNDX", "VTBR"]
            .map(Arc::from)
            .to_vec();
        let mut rng = fastrand::Rng::new();
        let mut trades: Vec<Trade> = (0..1_000)
            .map(|id| Trade {
                id,
                instrument: instruments[rng.usize(..instruments.len())].clone(),
                sell: rng.bool(),
                quantity: rng.i64(1..10_000),
                price: rng.f64() * 300.,
            })
            .collect();
        writer.replace_all(trades.iter().cloned().map(Arc::new));
        while !writer.is_closed() {
            let trade = &mut trades[rng.usize(..1_000)];
            trade.price *= 1. + (rng.f64() - 0.5) / 100.;
            writer.upsert(trade.clone());
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    // #endregion writer

    cx.new(|_| TradesWindow { grid })
}

// #region main
fn main() {
    gpui_kit::application()
        // gpui-kit's icons (sort arrows, menu checks): without them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            // The grid's key bindings: arrows, Page, Home/End, Enter, Escape.
            gpui_kit_grid::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1100.), px(720.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_, cx| trades_window(cx)).expect("open window");
            cx.activate(true);
        });
}
// #endregion main
