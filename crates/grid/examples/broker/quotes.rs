//! The quote grids: the watchlist and the two movers lists, and the relay
//! that fills the movers' stores.
//!
//! The grid has no "first N rows" view, so the movers have stores of their
//! own: the relay picks the top gainers and losers from the simulator's
//! quotes once a second and writes only them.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::{ActiveTheme as _, Sizable as _, tag::Tag};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement as _,
    ParentElement as _, Styled as _, Subscription, Window, px, transparent_black,
};
use gpui_kit_grid::{
    CellContent, CellElementContext, CellStyle, CellStyleContext, ColumnAlign, ColumnSet,
    GridColumn, GridColumns as _, GridRow, GridSchema, GridStore, GridWriter, HighlightPolicy,
    LiveGridDelegate, LiveGridEvent, LiveGridState, SelectionMode, SortDirection, StoreOptions,
    ViewSpec,
};

use super::colors::PriceColors;
use crate::trading::{
    bus::{InstrumentBus, InstrumentChange},
    instruments::{self, InstrumentId},
    rows::{Quote, QuoteColumn},
};

/// How long a quote's change shows.
const QUOTE_FLASH: Duration = Duration::from_millis(700);
/// How often the relay picks the movers.
pub const MOVERS_INTERVAL: Duration = Duration::from_secs(1);
/// Instruments in each movers list.
pub const MOVERS: usize = 5;
/// How strongly the %Chg pill shows its colour behind the text, and its
/// border.
const PILL_TINT: f32 = 0.14;
const PILL_BORDER: f32 = 0.35;

/// The simulated watchlist.
pub const WATCHLIST: [&str; 24] = [
    "AAPL", "MSFT", "NVDA", "AMZN", "GOOGL", "META", "TSLA", "AMD", "NFLX", "JPM", "V", "KO",
    "PEP", "DIS", "NKE", "SBUX", "COST", "WMT", "XOM", "LLY", "MCD", "SPY", "QQQ", "IWM",
];

/// The quote stores' options: short flashes.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(QUOTE_FLASH, 6))
}

/// Picks the `MOVERS` quotes with the highest %Chg for one store and the
/// `MOVERS` with the lowest for the other, each time `step` is called
/// (every `MOVERS_INTERVAL`, on a worker: `trading/worker.rs`). An
/// instrument that leaves a list is removed; a quote that did not change is
/// the same `Arc`, which the store skips.
pub struct MoversRelay {
    quotes: GridWriter<Quote>,
    gainers: GridWriter<Quote>,
    losers: GridWriter<Quote>,
    /// The quotes' version the lists were last picked from.
    version: Option<u64>,
    shown: [Vec<InstrumentId>; 2],
}

impl MoversRelay {
    pub fn new(
        quotes: GridWriter<Quote>,
        gainers: GridWriter<Quote>,
        losers: GridWriter<Quote>,
    ) -> Self {
        Self {
            quotes,
            gainers,
            losers,
            version: None,
            shown: Default::default(),
        }
    }

    /// Writes both lists if the quotes changed; false once both stores are
    /// gone.
    pub fn step(&mut self) -> bool {
        if self.gainers.is_closed() && self.losers.is_closed() {
            return false;
        }
        let snapshot = self.quotes.snapshot();
        if !snapshot.is_empty() && self.version != Some(snapshot.version()) {
            self.version = Some(snapshot.version());
            let mut ranked: Vec<Arc<Quote>> =
                snapshot.iter().map(|(_, quote)| quote.clone()).collect();
            ranked.sort_by(|a, b| b.change_pct().total_cmp(&a.change_pct()));
            let top = ranked.iter().take(MOVERS).cloned().collect();
            let bottom = ranked.iter().rev().take(MOVERS).cloned().collect();
            for ((writer, list), shown) in [(&self.gainers, top), (&self.losers, bottom)]
                .into_iter()
                .zip(&mut self.shown)
            {
                write_list(writer, list, shown);
            }
        }
        true
    }
}

/// Writes `list` into `writer`, removing what `shown` held and `list`
/// does not; `shown` becomes `list`'s instruments.
fn write_list(writer: &GridWriter<Quote>, list: Vec<Arc<Quote>>, shown: &mut Vec<InstrumentId>) {
    let ids: Vec<InstrumentId> = list.iter().map(|quote| quote.id).collect();
    writer.batch(|rows| {
        for gone in shown.iter().filter(|id| !ids.contains(id)) {
            rows.remove(*gone);
        }
        for quote in list {
            rows.upsert(quote);
        }
    });
    *shown = ids;
}

/// The quote grids' hooks, colours from the theme and `PriceColors` only:
/// the ticker bold, the name muted, %Chg a pill in its direction's colour
/// (an element in the cell: the value stays a number, for the sort and a
/// copy). A change shows on Last alone, as a tint and an arrow in the
/// convention's colours (`PriceChanges`). They take `&self` and never read
/// the grid; the styles never allocate (the pill, an element, does).
pub struct QuoteList;

impl LiveGridDelegate for QuoteList {
    type Row = Quote;

    fn cell_style(&self, cell: &CellStyleContext<'_, Quote>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let colors = PriceColors::current(cx);
        // Only Last shows a change: a calm list, not a flashing board.
        if cell.column() != QuoteColumn::Last {
            style.set_trend(None);
            if cell.change().is_some() {
                style.set_background(transparent_black());
            }
        }
        match cell.column() {
            QuoteColumn::Ticker => style.set_font_weight(FontWeight::BOLD),
            QuoteColumn::Name => style.set_foreground(theme.muted_foreground),
            // A pill's colour for the text that stands in for it: a
            // fading row, a pill with no room.
            QuoteColumn::ChangePct => {
                style.set_foreground(colors.of(cell.row().change(), theme));
                style.set_font_weight(FontWeight::BOLD);
            }
            _ => {}
        }
    }

    /// %Chg as a pill: the signed percentage in its direction's colour on a
    /// tint of it.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Quote>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        (cell.column() == QuoteColumn::ChangePct).then(|| {
            let change = cell.row().change();
            let color = PriceColors::current(cx).of(change, cx.theme());
            let sign = if change > 0. { "+" } else { "" };
            Tag::custom(color.opacity(PILL_TINT), color, color.opacity(PILL_BORDER))
                .with_size(cell.size())
                .rounded_full()
                .font_weight(FontWeight::BOLD)
                .child(format!("{sign}{}%", cell.text()))
                .into_any_element()
        })
    }
}

fn schema(shown: &[(QuoteColumn, f32)]) -> GridSchema<QuoteColumn> {
    QuoteColumn::all()
        .iter()
        .fold(GridSchema::new(), |schema, &id| {
            let title = match id {
                QuoteColumn::Ticker => "Symbol",
                QuoteColumn::Name => "Name",
                QuoteColumn::Sector => "Sector",
                QuoteColumn::Last => "Last",
                QuoteColumn::Change => "Chg",
                QuoteColumn::ChangePct => "Chg %",
                QuoteColumn::Bid => "Bid",
                QuoteColumn::Ask => "Ask",
                QuoteColumn::Volume => "Volume",
            };
            let column = GridColumn::new(id, title);
            // %Chg: a pill at the numbers' edge.
            let column = if id == QuoteColumn::ChangePct {
                column
                    .with_content(CellContent::Element)
                    .with_align(ColumnAlign::End)
            } else {
                column
            };
            schema.column(match shown.iter().find(|(shown, _)| *shown == id) {
                Some((_, width)) => column.with_width(px(*width)),
                None => column.with_visible(false),
            })
        })
}

fn quote_grid(
    store: GridStore<Quote>,
    schema: GridSchema<QuoteColumn>,
    view_id: &'static str,
    view: ViewSpec<Quote>,
    cx: &mut Context<LiveGridState<QuoteList>>,
) -> LiveGridState<QuoteList> {
    LiveGridState::new(schema, QuoteList, store, cx)
        .with_view_id(view_id)
        .with_selection_mode(SelectionMode::RowSingle)
        .with_change_paint(PriceColors::current(cx).change_paint())
        .with_view(view)
}

/// The watchlist: `WATCHLIST` by symbol; the search box filters it by
/// symbol and name.
pub fn watchlist(store: GridStore<Quote>, cx: &mut App) -> Entity<LiveGridState<QuoteList>> {
    let listed: Arc<[InstrumentId]> = WATCHLIST
        .iter()
        .filter_map(|ticker| instruments::find(ticker))
        .collect();
    cx.new(|cx| {
        let schema = schema(&[
            (QuoteColumn::Ticker, 86.),
            (QuoteColumn::Name, 84.),
            (QuoteColumn::Last, 80.),
            (QuoteColumn::ChangePct, 88.),
        ]);
        // An instrument's id never changes: the filter depends on no column.
        let view = ViewSpec::new()
            .with_filter(ColumnSet::EMPTY, move |quote: &Quote| {
                listed.contains(&quote.id)
            })
            .with_sort([(QuoteColumn::Ticker, SortDirection::Ascending)]);
        quote_grid(store, schema, "watchlist", view, cx)
            .with_quick_filter_columns([QuoteColumn::Ticker, QuoteColumn::Name])
    })
}

/// A movers list: its store's quotes by %Chg, highest first for the
/// gainers, lowest first for the losers.
pub fn movers(
    store: GridStore<Quote>,
    direction: SortDirection,
    cx: &mut App,
) -> Entity<LiveGridState<QuoteList>> {
    cx.new(|cx| {
        let schema = schema(&[
            (QuoteColumn::Ticker, 80.),
            (QuoteColumn::Name, 124.),
            (QuoteColumn::Last, 84.),
            (QuoteColumn::ChangePct, 88.),
        ]);
        let view = ViewSpec::new().with_sort([(QuoteColumn::ChangePct, direction)]);
        let view_id = match direction {
            SortDirection::Descending => "gainers",
            SortDirection::Ascending => "losers",
        };
        quote_grid(store, schema, view_id, view, cx)
    })
}

/// Makes `grid` open the instrument of the row selected in it, and follow
/// the instrument opened elsewhere: select it and scroll to it when it
/// shows it, else select nothing.
pub fn follow_current<D>(
    grid: &Entity<LiveGridState<D>>,
    bus: &Entity<InstrumentBus>,
    cx: &mut App,
) -> [Subscription; 2]
where
    D: LiveGridDelegate,
    D::Row: GridRow<Key = InstrumentId>,
{
    let opens = cx.subscribe(grid, {
        let bus = bus.clone();
        move |_, event: &LiveGridEvent<InstrumentId, <D::Row as GridRow>::Column>, cx| {
            if let LiveGridEvent::SelectionChange { selection, .. } = event
                && let Some(id) = selection.iter().next().copied()
            {
                bus.update(cx, |bus, cx| bus.set_current(Some(id), cx));
            }
        }
    });
    let follows = cx.subscribe(bus, {
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
                    if !grid.selection().is_empty() {
                        grid.select_key(None, cx);
                    }
                    return;
                }
                if !grid.selection().contains(&id) {
                    grid.select_key(Some(id), cx);
                }
                grid.scroll_to_key(&id, cx);
            });
        }
    });
    [opens, follows]
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
}
