//! The quotes view: the trading examples' 150 simulated quotes, grouped by
//! sector with the average %Chg, in the desk's Depth theme. With a feed
//! address the quotes come from the gateway (`crates/grid-gateway`) over
//! WebSocket instead of the simulator.

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{ActiveTheme as _, Sizable as _, Size};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontFeatures, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Task, Window, div, px,
};
use gpui_kit_grid::{
    Aggregate, AggregateStyleContext, CellStyle, CellStyleContext, GridColumn, GridSchema,
    GridStore, GroupBy, HighlightPolicy, LiveGrid, LiveGridDelegate, LiveGridState, SelectionMode,
    SortDirection, StoreOptions, ViewSpec,
};
use gpui_kit_grid_web_shell::feed::{self, Feed};
use web_time::Instant;

use crate::trading::{
    instruments::{INSTRUMENTS, InstrumentId},
    market::{Control, Feeds, Market, Pump},
    paint::quiet_changes,
    rows::{Quote, QuoteColumn},
};

/// The grid's row height, as on the desk.
const ROW_HEIGHT: f32 = 24.;
/// How long a quote's change shows, as on the desk.
const QUOTE_FLASH: Duration = Duration::from_millis(800);

/// The desk watchlist's hooks: tickers in `base.yellow`, Chg and %Chg by
/// sign, Bid in `success`, Ask in `danger`, aggregates in `base.cyan`; only
/// Last shows the change arrow.
pub struct Watchlist;

impl LiveGridDelegate for Watchlist {
    type Row = Quote;

    fn cell_style(&self, cell: &CellStyleContext<'_, Quote>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        match cell.column() {
            QuoteColumn::Ticker => style.set_foreground(theme.yellow),
            QuoteColumn::Change | QuoteColumn::ChangePct => {
                style.set_foreground(if row.change() >= 0. {
                    theme.chart_bullish
                } else {
                    theme.chart_bearish
                })
            }
            QuoteColumn::Bid => style.set_foreground(theme.success),
            QuoteColumn::Ask => style.set_foreground(theme.danger),
            _ => {}
        }
        if cell.column() != QuoteColumn::Last {
            style.set_trend(None);
        }
    }

    fn aggregate_style(
        &self,
        _: &AggregateStyleContext<'_, Quote>,
        style: &mut CellStyle,
        cx: &App,
    ) {
        style.set_foreground(cx.theme().cyan);
        style.set_trend(None);
    }
}

fn schema() -> GridSchema<QuoteColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(QuoteColumn::Ticker, "Ticker").with_width(px(84.)))
        .column(GridColumn::new(QuoteColumn::Name, "Name").with_width(px(200.)))
        .column(
            GridColumn::new(QuoteColumn::Sector, "Sector")
                .with_width(px(150.))
                .with_visible(false),
        )
        .column(GridColumn::new(QuoteColumn::Last, "Last").with_width(px(92.)))
        .column(GridColumn::new(QuoteColumn::Change, "Chg").with_width(px(76.)))
        .column(GridColumn::new(QuoteColumn::ChangePct, "%Chg").with_width(px(72.)))
        .column(GridColumn::new(QuoteColumn::Bid, "Bid").with_width(px(84.)))
        .column(GridColumn::new(QuoteColumn::Ask, "Ask").with_width(px(84.)))
        .column(GridColumn::new(QuoteColumn::Volume, "Volume").with_width(px(100.)))
}

/// What the page asked for.
pub struct Setup {
    pub seed: u64,
    pub extra_rows: u32,
    pub sort_by_change: bool,
    /// The gateway's address, instead of the simulator.
    pub feed: Option<String>,
}

/// The page: a status line over the quotes grid.
pub struct Quotes {
    grid: Entity<LiveGridState<Watchlist>>,
    control: Arc<Control>,
    /// The gateway's feed, instead of the simulator.
    feed: Option<Rc<RefCell<Feed>>>,
    /// How long building the simulated market (two hours of bars for 150
    /// instruments) took on the main thread.
    market_built: Duration,
    /// The instruments and the stress test's static copies.
    rows: usize,
    /// The rates of the last second, empty before the first.
    status: SharedString,
    /// The feed's median server → ingest latency over the last second.
    latency: Option<f64>,
    _tasks: [Task<()>; 2],
}

impl Quotes {
    pub fn new(control: Arc<Control>, setup: Setup, cx: &mut Context<Self>) -> Self {
        let store = GridStore::new(
            StoreOptions::default().with_highlight(HighlightPolicy::new(QUOTE_FLASH, 4)),
            cx,
        );
        let grid = cx.new(|cx| {
            LiveGridState::new(schema(), Watchlist, store.clone(), cx)
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(quiet_changes())
                .with_view(
                    ViewSpec::new()
                        .with_sort([if setup.sort_by_change {
                            (QuoteColumn::ChangePct, SortDirection::Descending)
                        } else {
                            (QuoteColumn::Ticker, SortDirection::Ascending)
                        }])
                        .with_group_by([GroupBy::column(QuoteColumn::Sector)])
                        .with_aggregates([
                            (QuoteColumn::ChangePct, Aggregate::Avg),
                            (QuoteColumn::Volume, Aggregate::Sum),
                        ]),
                )
        });
        if let Some(url) = setup.feed {
            let feed = Feed::shared();
            return Self {
                grid,
                control,
                feed: Some(feed.clone()),
                market_built: Duration::ZERO,
                rows: INSTRUMENTS.len(),
                status: SharedString::default(),
                latency: None,
                _tasks: [
                    feed::connect(url, feed, store.writer(), cx),
                    Self::refresh(cx),
                ],
            };
        }
        let started = Instant::now();
        let market = Market::new(Some(setup.seed));
        let market_built = started.elapsed();
        let seeds = market.quotes().to_vec();
        let pump = Pump::start(
            market,
            Feeds::new().with_quotes(store.writer()),
            control.clone(),
        );
        if setup.extra_rows > 0 {
            store.writer().batch(|rows| {
                for ix in 0..setup.extra_rows as usize {
                    let mut quote = seeds[ix % seeds.len()].clone();
                    quote.id = InstrumentId((seeds.len() + ix) as u16);
                    // Spread the copies over the order.
                    quote.last *= 1. + ((ix * 7919) % 2001) as f64 * 1e-5 - 0.01;
                    rows.upsert(quote);
                }
            });
        }
        Self {
            grid,
            control,
            feed: None,
            market_built,
            rows: seeds.len() + setup.extra_rows as usize,
            status: SharedString::default(),
            latency: None,
            _tasks: [Self::simulate(pump, cx), Self::refresh(cx)],
        }
    }

    /// Runs the simulator on the main thread, one 10 ms batch per timer.
    fn simulate(mut pump: Pump, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |_, cx| {
            let executor = cx.background_executor().clone();
            while !pump.is_closed() {
                // Never a zero timer: the loop must yield to the page.
                let wait = pump.step().max(Duration::from_millis(1));
                executor.timer(wait).await;
            }
        })
    }

    /// The quotes grid.
    pub fn grid(&self) -> &Entity<LiveGridState<Watchlist>> {
        &self.grid
    }

    /// The simulator's rate and counters.
    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    /// The gateway's feed, if the quotes come from one.
    pub fn feed(&self) -> Option<&Rc<RefCell<Feed>>> {
        self.feed.as_ref()
    }

    /// How long building the simulated market took.
    pub fn market_built(&self) -> Duration {
        self.market_built
    }

    /// Updates written: the simulator's, or the rows the feed wrote.
    pub fn updates(&self) -> u64 {
        self.feed
            .as_ref()
            .map_or_else(|| self.control.updates(), |feed| feed.borrow().rows())
    }

    /// The status line: the rows, where the quotes come from (with a feed,
    /// its state at once) and the rates of the last second.
    fn status_line(&self) -> SharedString {
        let source = self.feed.as_ref().map_or_else(
            || "simulated".to_owned(),
            |feed| feed.borrow().status(self.latency),
        );
        let mut line = format!("{} rows, {source}", self.rows);
        if !self.status.is_empty() {
            line.push_str(" · ");
            line.push_str(&self.status);
        }
        line.into()
    }

    /// Every second: the measured update rate, the grid's frames and its
    /// last projection rebuild; with a feed, the median latency.
    fn refresh(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let (mut updates, mut frames) = (0, 0);
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let updated = this.update(cx, |quotes, cx| {
                    let stats = quotes.grid.read(cx).stats();
                    let now = (quotes.updates(), stats.frames());
                    quotes.latency = quotes
                        .feed
                        .as_ref()
                        .and_then(|feed| feed.borrow_mut().take_latency());
                    quotes.status = format!(
                        "{} updates/s · {} fps · rebuild {:.1} ms",
                        now.0 - updates,
                        now.1 - frames,
                        stats.last_rebuild().as_secs_f64() * 1000.,
                    )
                    .into();
                    (updates, frames) = now;
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        })
    }
}

impl Render for Quotes {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .flex_none()
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child("Quotes by sector")
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(self.status_line()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(theme.mono_font_size)
                    // A slashed zero; the grid adds tabular figures.
                    .font_features(FontFeatures(Arc::new(vec![("zero".into(), 1)])))
                    .child(
                        LiveGrid::new(&self.grid)
                            .with_size(Size::Size(px(ROW_HEIGHT)))
                            .bordered(false)
                            .accessibility_label("Quotes"),
                    ),
            )
    }
}
