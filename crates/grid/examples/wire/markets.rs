//! The quote grids: the market overview (by sector, sorted by %Chg, the
//! index funds pinned on top), its wide form on the Markets tab, the
//! instruments the open story mentions and the watchlist; and the relay
//! that fills the overview's store.
//!
//! The overview has a store of its own: pinned rows belong to a store, and
//! the other quote grids must not show the index funds pinned. The relay
//! copies the simulator's quotes into it four times a second.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, AppContext as _, Context, Entity, Subscription, px, transparent_black};
use gpui_kit_grid::{
    Aggregate, AggregateOrder, AggregateStyleContext, CellStyle, CellStyleContext, CellValue,
    ColumnSet, GridColumn, GridColumns as _, GridSchema, GridStore, GridTuning, GridWriter,
    GroupBy, GroupCaptionContext, HighlightPolicy, HoldReorder, LiveGridDelegate, LiveGridEvent,
    LiveGridState, PinnedEdge, SelectionMode, SortDirection, StoreOptions, ViewSpec,
};

use crate::trading::{
    bus::{InstrumentBus, InstrumentChange},
    instruments::{self, InstrumentId, Sector},
    paint::quiet_changes,
    rows::{Quote, QuoteColumn},
};

/// The quote grids' rows: denser than the news, still roomy.
pub const ROW_HEIGHT: gpui_kit::Pixels = px(32.);
/// How long a quote's change shows.
const QUOTE_FLASH: Duration = Duration::from_millis(900);
/// How often the relay copies the quotes into the overview's store.
const RELAY_INTERVAL: Duration = Duration::from_millis(250);
/// The overview re-sorts at most this often: a list to read, not a race.
const OVERVIEW_REBUILD: Duration = Duration::from_secs(1);
/// The simulated watchlist of the Watchlist tab.
pub const WATCHLIST: [&str; 16] = [
    "AAPL", "MSFT", "NVDA", "AMZN", "GOOGL", "META", "TSLA", "JPM", "V", "XOM", "UNH", "LLY",
    "COST", "WMT", "SPY", "QQQ",
];

/// The quote stores' options: short flashes.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(QUOTE_FLASH, 6))
}

/// Copies `quotes` into `overview` every `RELAY_INTERVAL` until the
/// overview's store is gone: the index funds as its top pinned rows, the
/// stocks as its rows. A quote that did not change is the same `Arc`,
/// which the store skips.
pub fn relay(quotes: GridWriter<Quote>, overview: GridWriter<Quote>) {
    let mut version = None;
    while !overview.is_closed() {
        let snapshot = quotes.snapshot();
        if !snapshot.is_empty() && version != Some(snapshot.version()) {
            version = Some(snapshot.version());
            let (mut funds, stocks): (Vec<Arc<Quote>>, Vec<Arc<Quote>>) = snapshot
                .iter()
                .map(|(_, quote)| quote.clone())
                .partition(|quote| quote.sector == Sector::IndexEtfs);
            funds.sort_by_key(|quote| quote.id);
            overview.batch(|rows| {
                rows.set_pinned_rows(PinnedEdge::Top, funds);
                for quote in stocks {
                    rows.upsert(quote);
                }
            });
        }
        std::thread::sleep(RELAY_INTERVAL);
    }
}

/// The quote grids' hooks, colours from the theme only: Chg and %Chg by
/// sign in `chart.bullish`/`chart.bearish`, names and volume muted, the
/// pinned index funds' tickers in the accent; aggregates likewise. A
/// change shows on Last alone: a faint tint and an arrow (`QuietChanges`). Group captions name the sector, short where the caption lane is
/// narrow. They take `&self`, never read the grid and never allocate.
pub struct Quotes {
    short_captions: bool,
}

impl Quotes {
    /// For a narrow overview: short sector names in the captions.
    pub fn narrow() -> Self {
        Self {
            short_captions: true,
        }
    }

    pub fn wide() -> Self {
        Self {
            short_captions: false,
        }
    }
}

impl LiveGridDelegate for Quotes {
    type Row = Quote;

    fn cell_style(&self, cell: &CellStyleContext<'_, Quote>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        match cell.column() {
            QuoteColumn::Ticker if cell.pinned_edge().is_some() => {
                style.set_foreground(theme.primary)
            }
            QuoteColumn::Change | QuoteColumn::ChangePct => {
                style.set_foreground(if row.change() >= 0. {
                    theme.chart_bullish
                } else {
                    theme.chart_bearish
                })
            }
            QuoteColumn::Name | QuoteColumn::Sector | QuoteColumn::Volume => {
                style.set_foreground(theme.muted_foreground)
            }
            _ => {}
        }
        // Only Last shows a change: an airy list, not a flashing board.
        if cell.column() != QuoteColumn::Last {
            style.set_trend(None);
            if cell.change().is_some() {
                style.set_background(transparent_black());
            }
        }
    }

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, Quote>,
        style: &mut CellStyle,
        cx: &App,
    ) {
        let theme = cx.theme();
        style.set_trend(None);
        if cell.change().is_some() {
            style.set_background(transparent_black());
        }
        match (cell.column(), cell.value()) {
            (QuoteColumn::ChangePct, Some(CellValue::Float(average))) => {
                style.set_foreground(if average >= 0. {
                    theme.chart_bullish
                } else {
                    theme.chart_bearish
                })
            }
            _ => style.set_foreground(theme.muted_foreground),
        }
    }

    fn group_caption(&self, caption: &GroupCaptionContext<'_, Quote>, out: &mut String, _: &App) {
        let sector = caption.value_text();
        out.push_str(if self.short_captions {
            short_sector(sector)
        } else {
            sector
        });
    }
}

/// A sector's name for a narrow caption.
fn short_sector(label: &str) -> &str {
    match label {
        "Consumer Discretionary" => "Discretionary",
        "Communication Services" => "Communication",
        "Consumer Staples" => "Staples",
        label => label,
    }
}

fn column(column: QuoteColumn, title: &'static str, width: f32) -> GridColumn<QuoteColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// The columns of `shown`, in schema order, each with its width; the
/// others hidden.
fn schema(shown: &[(QuoteColumn, f32)]) -> GridSchema<QuoteColumn> {
    QuoteColumn::all()
        .iter()
        .fold(GridSchema::new(), |schema, &id| {
            let title = match id {
                QuoteColumn::Ticker => "Ticker",
                QuoteColumn::Name => "Name",
                QuoteColumn::Sector => "Sector",
                QuoteColumn::Last => "Last",
                QuoteColumn::Change => "Chg",
                QuoteColumn::ChangePct => "%Chg",
                QuoteColumn::Bid => "Bid",
                QuoteColumn::Ask => "Ask",
                QuoteColumn::Volume => "Volume",
            };
            let width = shown.iter().find(|(shown, _)| *shown == id);
            schema.column(match width {
                Some((_, width)) => column(id, title, *width),
                None => column(id, title, 80.).with_visible(false),
            })
        })
}

/// Grouped by sector, sectors and names by %Chg, highest first; each
/// caption with the sector's average %Chg and summed volume.
fn overview_view() -> ViewSpec<Quote> {
    ViewSpec::new()
        .with_sort([(QuoteColumn::ChangePct, SortDirection::Descending)])
        .with_group_by([GroupBy::column(QuoteColumn::Sector)])
        .with_group_order(AggregateOrder::new(
            QuoteColumn::ChangePct,
            SortDirection::Descending,
        ))
        .with_aggregates([
            (QuoteColumn::ChangePct, Aggregate::Avg),
            (QuoteColumn::Volume, Aggregate::Sum),
        ])
}

fn quote_grid(
    store: GridStore<Quote>,
    delegate: Quotes,
    schema: GridSchema<QuoteColumn>,
    view_id: &'static str,
    view: ViewSpec<Quote>,
    cx: &mut Context<LiveGridState<Quotes>>,
) -> LiveGridState<Quotes> {
    LiveGridState::new(schema, delegate, store, cx)
        .with_view_id(view_id)
        .with_selection_mode(SelectionMode::RowSingle)
        .with_change_paint(quiet_changes())
        .with_view(view)
}

/// The overview beside the news: ticker, last, %Chg and volume.
pub fn overview(store: GridStore<Quote>, cx: &mut App) -> Entity<LiveGridState<Quotes>> {
    cx.new(|cx| {
        // Ticker and Last are wider together than the caption lane's
        // least (about 140 px): an aggregate in a column that starts
        // within it is not shown.
        let schema = schema(&[
            (QuoteColumn::Ticker, 62.),
            (QuoteColumn::Last, 94.),
            (QuoteColumn::ChangePct, 70.),
            (QuoteColumn::Volume, 96.),
        ]);
        quote_grid(
            store,
            Quotes::narrow(),
            schema,
            "overview",
            overview_view(),
            cx,
        )
        .with_tuning(GridTuning::default().with_min_rebuild_interval(OVERVIEW_REBUILD))
        .with_reorder_policy(HoldReorder::default())
    })
}

/// The Markets tab: the overview with every quote column.
pub fn markets(store: GridStore<Quote>, cx: &mut App) -> Entity<LiveGridState<Quotes>> {
    cx.new(|cx| {
        let schema = schema(&[
            (QuoteColumn::Ticker, 76.),
            (QuoteColumn::Name, 200.),
            (QuoteColumn::Last, 92.),
            (QuoteColumn::Change, 84.),
            (QuoteColumn::ChangePct, 84.),
            (QuoteColumn::Bid, 92.),
            (QuoteColumn::Ask, 92.),
            (QuoteColumn::Volume, 120.),
        ]);
        quote_grid(
            store,
            Quotes::wide(),
            schema,
            "markets",
            overview_view(),
            cx,
        )
        .with_tuning(GridTuning::default().with_min_rebuild_interval(OVERVIEW_REBUILD))
        .with_reorder_policy(HoldReorder::default())
    })
}

/// The quotes of `tickers` only, in the store's order.
pub fn mentioned_view(tickers: Arc<[InstrumentId]>) -> ViewSpec<Quote> {
    // An instrument's id never changes: the filter depends on no column.
    ViewSpec::new().with_filter(ColumnSet::EMPTY, move |quote: &Quote| {
        tickers.contains(&quote.id)
    })
}

/// The instruments the open story mentions.
pub fn mentioned(store: GridStore<Quote>, cx: &mut App) -> Entity<LiveGridState<Quotes>> {
    cx.new(|cx| {
        let schema = schema(&[
            (QuoteColumn::Ticker, 66.),
            (QuoteColumn::Name, 130.),
            (QuoteColumn::Last, 76.),
            (QuoteColumn::ChangePct, 64.),
            (QuoteColumn::Volume, 92.),
        ]);
        let view = mentioned_view(Arc::from([]));
        quote_grid(store, Quotes::wide(), schema, "mentioned", view, cx)
    })
}

/// The Watchlist tab: `WATCHLIST` with every quote column, by ticker.
pub fn watchlist(store: GridStore<Quote>, cx: &mut App) -> Entity<LiveGridState<Quotes>> {
    let listed: Arc<[InstrumentId]> = WATCHLIST
        .iter()
        .filter_map(|t| instruments::find(t))
        .collect();
    cx.new(|cx| {
        let schema = schema(&[
            (QuoteColumn::Ticker, 76.),
            (QuoteColumn::Name, 200.),
            (QuoteColumn::Sector, 190.),
            (QuoteColumn::Last, 92.),
            (QuoteColumn::Change, 84.),
            (QuoteColumn::ChangePct, 84.),
            (QuoteColumn::Bid, 92.),
            (QuoteColumn::Ask, 92.),
            (QuoteColumn::Volume, 120.),
        ]);
        let view =
            mentioned_view(listed).with_sort([(QuoteColumn::Ticker, SortDirection::Ascending)]);
        quote_grid(store, Quotes::wide(), schema, "watchlist", view, cx)
    })
}

/// Makes `grid` set the current instrument when its selection changes,
/// and follow it: select and scroll to the current instrument when it
/// shows that one.
pub fn follow_current(
    grid: &Entity<LiveGridState<Quotes>>,
    bus: &Entity<InstrumentBus>,
    cx: &mut App,
) -> [Subscription; 2] {
    let sets_current = cx.subscribe(grid, {
        let bus = bus.clone();
        move |_, event: &LiveGridEvent<InstrumentId, QuoteColumn>, cx| {
            if let LiveGridEvent::SelectionChange { selection, .. } = event
                && let Some(id) = selection.iter().next().copied()
            {
                bus.update(cx, |bus, cx| bus.set_current(Some(id), cx));
            }
        }
    });
    let follows_current = cx.subscribe(bus, {
        let grid = grid.downgrade();
        move |_, change: &InstrumentChange, cx| {
            let (Some(id), Some(grid)) = (change.current, grid.upgrade()) else {
                return;
            };
            grid.update(cx, |grid, cx| {
                let shown = grid
                    .snapshot()
                    .row_id(&id)
                    .is_some_and(|row| grid.projection().contains(row));
                if !shown {
                    return;
                }
                if !grid.selection().contains(&id) {
                    grid.select_key(Some(id), cx);
                }
                grid.scroll_to_key(&id, cx);
            });
        }
    });
    [sets_current, follows_current]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watchlist_tickers_are_instruments() {
        for ticker in WATCHLIST {
            assert!(instruments::find(ticker).is_some(), "{ticker}");
        }
    }

    #[test]
    fn test_short_sector_names_fit_a_narrow_caption() {
        for sector in Sector::ALL {
            assert!(
                short_sector(sector.label()).len() <= 14,
                "{}",
                sector.label()
            );
        }
    }
}
