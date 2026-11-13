//! The account at work: `AccountDesk` keeps the simulated account
//! (`account.rs`) on the UI thread, places the ticket's orders, fills the
//! working ones four times a second and writes the Positions and Orders
//! grids' stores; and the two grids' hooks and columns.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, AppContext as _, Context, Entity, FontWeight, Task, px, transparent_black};
use gpui_kit_grid::{
    CellStyle, CellStyleContext, GridColumn, GridColumns, GridSchema, GridStore, GridWriter,
    HighlightPolicy, LiveGridDelegate, LiveGridState, SelectionMode, SortDirection, StoreOptions,
    ViewSpec,
};

use super::{
    account::{
        Account, Holding, HoldingColumn, Order, OrderColumn, OrderRequest, OrderStatus, Reject,
        Summary,
    },
    colors::PriceColors,
};
use crate::trading::{
    instruments::InstrumentId,
    market,
    rows::{Quote, Side},
};

/// How often the desk fills working orders and writes the positions.
const TICK: Duration = Duration::from_millis(250);
/// How long a position's or an order's change shows.
const FLASH: Duration = Duration::from_millis(900);

/// The simulated account, the quotes it trades against and the stores of
/// its two grids.
pub struct AccountDesk {
    account: Account,
    quotes: GridStore<Quote>,
    holdings: GridWriter<Holding>,
    orders: GridWriter<Order>,
    /// The holdings last written: a sold-out one is removed.
    written: BTreeSet<InstrumentId>,
    summary: Summary,
    _tick: Task<()>,
}

impl AccountDesk {
    pub fn new(
        quotes: GridStore<Quote>,
        holdings: GridWriter<Holding>,
        orders: GridWriter<Order>,
        cx: &mut Context<Self>,
    ) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |desk, cx| desk.tick(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            account: Account::new(),
            quotes,
            holdings,
            orders,
            written: BTreeSet::new(),
            summary: Summary::default(),
            _tick: tick,
        }
    }

    pub fn account(&self) -> &Account {
        &self.account
    }

    /// The account at the prices of the last tick.
    pub fn summary(&self) -> Summary {
        self.summary
    }

    /// The simulated quote of `id`, as last written.
    pub fn quote(&self, id: InstrumentId) -> Option<Arc<Quote>> {
        self.quotes.snapshot().get(&id).cloned()
    }

    /// Places `request` against the current quote: the order, filled or
    /// working, or why not.
    pub fn place(
        &mut self,
        request: &OrderRequest,
        cx: &mut Context<Self>,
    ) -> Result<Order, Reject> {
        let Some(quote) = self.quote(request.instrument) else {
            return Err(Reject::NoPrice);
        };
        let time = market::utc_time(web_time::SystemTime::now(), false);
        let order = self.account.place(request, &quote, time)?.clone();
        self.orders.upsert(order.clone());
        self.refresh(cx);
        Ok(order)
    }

    /// Fills the working orders the quotes reach, then refreshes.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.quotes.snapshot();
        if snapshot.is_empty() {
            return;
        }
        let filled = self
            .account
            .fill_working(|id| snapshot.get(&id).map(|quote| &**quote));
        for seq in filled {
            if let Some(order) = self.account.order(seq) {
                self.orders.upsert(order.clone());
            }
        }
        self.refresh(cx);
    }

    /// Writes the holdings at the current prices and the summary.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.quotes.snapshot();
        let holdings = self
            .account
            .holdings(|id| snapshot.get(&id).map(|quote| &**quote));
        self.summary = self.account.summary(&holdings);
        let held: BTreeSet<InstrumentId> = holdings.iter().map(|holding| holding.id).collect();
        let gone: Vec<InstrumentId> = self.written.difference(&held).copied().collect();
        self.holdings.batch(|rows| {
            for id in gone {
                rows.remove(id);
            }
            for holding in holdings {
                rows.upsert(holding);
            }
        });
        self.written = held;
        cx.notify();
    }
}

/// The Positions and Orders stores' options.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 6))
}

/// The Positions grid's hooks: the symbol bold, the day's and the total
/// P&L in their direction's colour (`PriceColors`); a change shows on Last
/// alone. They take `&self`, never read the grid and never allocate.
pub struct Positions;

impl LiveGridDelegate for Positions {
    type Row = Holding;

    fn cell_style(&self, cell: &CellStyleContext<'_, Holding>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let colors = PriceColors::current(cx);
        if cell.column() != HoldingColumn::Last {
            style.set_trend(None);
            if cell.change().is_some() {
                style.set_background(transparent_black());
            }
        }
        let row = cell.row();
        match cell.column() {
            HoldingColumn::Ticker => style.set_font_weight(FontWeight::BOLD),
            HoldingColumn::Average => style.set_foreground(theme.muted_foreground),
            HoldingColumn::DayPnl => style.set_foreground(colors.of(row.day_pnl(), theme)),
            HoldingColumn::TotalPnl => {
                style.set_foreground(colors.of(row.total_pnl(), theme));
                style.set_font_weight(FontWeight::BOLD);
            }
            _ => {}
        }
    }
}

/// The Orders grid's hooks: Buy in the colour of a rise and Sell in that
/// of a fall (`PriceColors`), a working order's status in the accent, a
/// filled one's muted. No change flashes: an order changes once.
pub struct Orders;

impl LiveGridDelegate for Orders {
    type Row = Order;

    fn cell_style(&self, cell: &CellStyleContext<'_, Order>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let colors = PriceColors::current(cx);
        style.set_trend(None);
        if cell.change().is_some() {
            style.set_background(transparent_black());
        }
        let row = cell.row();
        match cell.column() {
            OrderColumn::Ticker => style.set_font_weight(FontWeight::BOLD),
            OrderColumn::Side => {
                style.set_foreground(match row.side {
                    Side::Buy => colors.rise(theme),
                    Side::Sell => colors.fall(theme),
                });
                style.set_font_weight(FontWeight::BOLD);
            }
            OrderColumn::Status => match row.status {
                OrderStatus::Working => {
                    style.set_foreground(theme.primary);
                    style.set_background(theme.primary.opacity(0.1));
                    style.set_font_weight(FontWeight::BOLD);
                }
                OrderStatus::Filled { .. } => style.set_foreground(theme.muted_foreground),
            },
            OrderColumn::Time => style.set_foreground(theme.muted_foreground),
            _ => {}
        }
    }
}

fn column<C: GridColumns>(id: C, title: &'static str, width: f32) -> GridColumn<C> {
    GridColumn::new(id, title).with_width(px(width))
}

/// The Positions grid: every holding, by market value, largest first.
pub fn positions(store: GridStore<Holding>, cx: &mut App) -> Entity<LiveGridState<Positions>> {
    cx.new(|cx| {
        let schema = HoldingColumn::all()
            .iter()
            .fold(GridSchema::new(), |schema, &id| {
                schema.column(match id {
                    HoldingColumn::Ticker => column(id, "Symbol", 96.),
                    HoldingColumn::Quantity => column(id, "Qty", 80.),
                    HoldingColumn::Average => column(id, "Avg cost", 104.),
                    HoldingColumn::Last => column(id, "Last", 104.),
                    HoldingColumn::Value => column(id, "Market value", 128.),
                    HoldingColumn::DayPnl => column(id, "Day P&L", 112.),
                    HoldingColumn::TotalPnl => column(id, "Total P&L", 112.),
                })
            });
        LiveGridState::new(schema, Positions, store, cx)
            .with_view_id("positions")
            .with_selection_mode(SelectionMode::RowSingle)
            .with_change_paint(PriceColors::current(cx).change_paint())
            .with_view(
                ViewSpec::new().with_sort([(HoldingColumn::Value, SortDirection::Descending)]),
            )
    })
}

/// The Orders grid: newest first.
pub fn orders(store: GridStore<Order>, cx: &mut App) -> Entity<LiveGridState<Orders>> {
    cx.new(|cx| {
        let schema = OrderColumn::all()
            .iter()
            .fold(GridSchema::new(), |schema, &id| {
                schema.column(match id {
                    OrderColumn::Time => column(id, "Time", 92.),
                    OrderColumn::Ticker => column(id, "Symbol", 84.),
                    OrderColumn::Side => column(id, "Side", 68.),
                    OrderColumn::Type => column(id, "Type", 76.),
                    OrderColumn::Quantity => column(id, "Qty", 68.),
                    OrderColumn::Limit => column(id, "Limit", 90.),
                    OrderColumn::Fill => column(id, "Fill price", 96.),
                    OrderColumn::Status => column(id, "Status", 96.),
                    OrderColumn::Seq => column(id, "No.", 60.).with_visible(false),
                })
            });
        LiveGridState::new(schema, Orders, store, cx)
            .with_view_id("orders")
            .with_selection_mode(SelectionMode::RowSingle)
            .with_view(ViewSpec::new().with_sort([(OrderColumn::Seq, SortDirection::Descending)]))
    })
}
