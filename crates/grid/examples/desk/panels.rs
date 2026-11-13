//! The dock panels: the watchlist and the news, and what the other panels
//! share. The order book, the trades tape and the positions are in
//! `book.rs`, `tape.rs` and `positions.rs`; the chart is the trading
//! examples' (`trading/chart.rs`).

use std::{sync::Arc, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size,
    dock::{BasePanel, Panel, PanelEvent},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontFeatures,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Styled as _, Subscription,
    Window, div, px,
};
use gpui_kit_grid::{
    Aggregate, AggregateStyleContext, CellStyle, CellStyleContext, ColumnSet, GridColumn,
    GridSchema, GridStore, GroupBy, HighlightPolicy, LiveGrid, LiveGridDelegate, LiveGridEvent,
    LiveGridState, SelectionMode, SortDirection, StoreOptions, ViewSpec,
};

use crate::trading::{
    bus::{InstrumentBus, InstrumentChange},
    instruments::InstrumentId,
    paint::quiet_changes,
    rows::{Headline, HeadlineColumn, Quote, QuoteColumn},
};

pub use crate::trading::chart::ChartPanel;

/// The row height of the desk's grids: denser than gpui-kit's `xsmall`.
pub const ROW_HEIGHT: Pixels = px(24.);
/// How long a quote's change shows: an instrument moves about twice a
/// second at the default rate, so a longer flash would light every cell.
const QUOTE_FLASH: Duration = Duration::from_millis(800);
/// How long a new headline shows.
const NEWS_FLASH: Duration = Duration::from_secs(4);

/// Implements the dock's behaviour for a panel: its name in a saved dock,
/// and not closable (the desk has no menu to reopen one).
macro_rules! dock_panel {
    ($panel:ty, $name:literal) => {
        impl BasePanel for $panel {
            fn panel_name(&self) -> &'static str {
                $name
            }

            fn closable(&self, _: &App) -> bool {
                false
            }
        }

        impl EventEmitter<PanelEvent> for $panel {}
    };
}

pub(crate) use dock_panel;

/// The font of a grid panel: the theme's monospace family and size, with a
/// slashed zero (OpenType `zero`); the grid adds tabular figures to it.
pub fn grid_font(cx: &App) -> gpui_kit::Div {
    let theme = cx.theme();
    div()
        .size_full()
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
        .font_features(FontFeatures(Arc::new(vec![("zero".into(), 1)])))
}

/// The ticker of `id` for a title, or "—".
pub fn ticker(id: Option<InstrumentId>) -> &'static str {
    id.map_or("—", |id| id.get().ticker)
}

// The watchlist.

/// The quotes store's options: short flashes.
pub fn quote_store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(QUOTE_FLASH, 4))
}

/// The watchlist's hooks, colours from the theme only: tickers in
/// `base.yellow`, Chg and %Chg by sign in `chart.bullish`/`chart.bearish`,
/// Bid in `success`, Ask in `danger`, aggregates in `base.cyan` (calculated
/// values), and a solid selection bar in `table.active.border`, since
/// gpui-kit caps `table.active.background` at 20 % opacity. A change tints
/// its cell faintly (`QuietChanges`); only Last shows the arrow, which
/// takes room from the digits. They take `&self` and never read the grid.
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
        // A change's flash shows through; the grid's translucent selection
        // lies over both.
        if cell.is_selected() && style.background().is_none() {
            style.set_background(theme.table_active_border);
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

/// The widths fit the watchlist's panel at the default window size (about
/// 600 px) in the widest theme font (8.4 px a digit): six digits and the
/// cell padding, Last with its arrow too, Volume eight digits; Name gives
/// way.
fn watchlist_schema() -> GridSchema<QuoteColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(QuoteColumn::Ticker, "Ticker").with_width(px(64.)))
        .column(GridColumn::new(QuoteColumn::Name, "Name").with_width(px(112.)))
        .column(
            GridColumn::new(QuoteColumn::Sector, "Sector")
                .with_width(px(150.))
                .with_visible(false),
        )
        .column(GridColumn::new(QuoteColumn::Last, "Last").with_width(px(78.)))
        .column(GridColumn::new(QuoteColumn::Change, "Chg").with_width(px(64.)))
        .column(GridColumn::new(QuoteColumn::ChangePct, "%Chg").with_width(px(56.)))
        .column(GridColumn::new(QuoteColumn::Bid, "Bid").with_width(px(68.)))
        .column(GridColumn::new(QuoteColumn::Ask, "Ask").with_width(px(68.)))
        .column(GridColumn::new(QuoteColumn::Volume, "Volume").with_width(px(82.)))
}

/// The grouping of `GROUP <column>`: by that column, ascending.
pub fn group_by(column: Option<QuoteColumn>) -> Vec<GroupBy<Quote>> {
    column.into_iter().map(GroupBy::column).collect()
}

pub struct WatchlistPanel {
    grid: Entity<LiveGridState<Watchlist>>,
    _subscriptions: [Subscription; 2],
}

impl WatchlistPanel {
    /// A watchlist over `store`, grouped by sector with the average %Chg
    /// and the summed volume, that follows and sets the current instrument.
    pub fn new(
        store: GridStore<Quote>,
        bus: &Entity<InstrumentBus>,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| {
            LiveGridState::new(watchlist_schema(), Watchlist, store, cx)
                .with_view_id("watchlist")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(quiet_changes())
                .with_view(
                    ViewSpec::new()
                        .with_sort([(QuoteColumn::Ticker, SortDirection::Ascending)])
                        .with_group_by(group_by(Some(QuoteColumn::Sector)))
                        .with_aggregates([
                            (QuoteColumn::ChangePct, Aggregate::Avg),
                            (QuoteColumn::Volume, Aggregate::Sum),
                        ]),
                )
        });
        let sets_current = cx.subscribe(&grid, {
            let bus = bus.clone();
            move |_, _, event: &LiveGridEvent<InstrumentId, QuoteColumn>, cx| {
                if let LiveGridEvent::SelectionChange { selection, .. } = event
                    && let Some(id) = selection.iter().next().copied()
                {
                    bus.update(cx, |bus, cx| bus.set_current(Some(id), cx));
                }
            }
        });
        let follows_current =
            cx.subscribe(&bus.clone(), |panel, _, change: &InstrumentChange, cx| {
                let Some(id) = change.current else {
                    return;
                };
                panel.grid.update(cx, |grid, cx| {
                    if !grid.selection().contains(&id) {
                        grid.select_key(Some(id), cx);
                    }
                    grid.scroll_to_key(&id, cx);
                });
            });
        Self {
            grid,
            _subscriptions: [sets_current, follows_current],
        }
    }

    pub fn grid(&self) -> &Entity<LiveGridState<Watchlist>> {
        &self.grid
    }
}

dock_panel!(WatchlistPanel, "desk.watchlist");

impl Panel for WatchlistPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Watchlist"
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for WatchlistPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for WatchlistPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        grid_font(cx).child(
            LiveGrid::new(&self.grid)
                .with_size(Size::Size(ROW_HEIGHT))
                .bordered(false)
                .accessibility_label("Watchlist"),
        )
    }
}

// The news.

/// The news store's options: a new headline shows for a few seconds.
pub fn news_store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(NEWS_FLASH, 8))
}

/// The news grid's hooks, colours from the theme only: the time in
/// `muted.foreground`, the SIM tag in `warning`, tickers in `base.yellow`,
/// the headline in the theme's foreground, a new headline tinted
/// (`QuietChanges`), and the watchlist's selection bar. They take `&self`
/// and never read the grid.
pub struct News;

impl LiveGridDelegate for News {
    type Row = Headline;

    fn cell_style(&self, cell: &CellStyleContext<'_, Headline>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        match cell.column() {
            HeadlineColumn::Time | HeadlineColumn::Seq => {
                style.set_foreground(theme.muted_foreground)
            }
            HeadlineColumn::Tag => style.set_foreground(theme.warning),
            HeadlineColumn::Ticker => style.set_foreground(theme.yellow),
            HeadlineColumn::Text => {}
        }
        if cell.is_selected() && style.background().is_none() {
            style.set_background(theme.table_active_border);
        }
    }
}

/// The grid has no column that takes the rest of the width, so Headline is
/// as wide as the panel leaves at the default window size; a longer line
/// ends in an ellipsis.
fn news_schema() -> GridSchema<HeadlineColumn> {
    GridSchema::new()
        .column(GridColumn::new(HeadlineColumn::Time, "Time").with_width(px(80.)))
        .column(GridColumn::new(HeadlineColumn::Tag, "Tag").with_width(px(44.)))
        .column(GridColumn::new(HeadlineColumn::Ticker, "Ticker").with_width(px(64.)))
        .column(GridColumn::new(HeadlineColumn::Text, "Headline").with_width(px(400.)))
        // The order of the headlines: newest first.
        .column(
            GridColumn::new(HeadlineColumn::Seq, "Seq")
                .with_width(px(64.))
                .with_visible(false),
        )
}

/// The headlines of `current` and the market-wide ones, or all of them
/// with no current instrument, newest first. A headline's instrument never
/// changes, so the filter depends on no column.
fn news_view(current: Option<InstrumentId>) -> ViewSpec<Headline> {
    ViewSpec::new()
        .with_sort([(HeadlineColumn::Seq, SortDirection::Descending)])
        .with_filter(ColumnSet::EMPTY, move |headline: &Headline| {
            current.is_none() || headline.instrument.is_none() || headline.instrument == current
        })
}

/// The simulated headlines on a grid, written by the simulator: those of
/// the current instrument and the market-wide ones, newest first.
pub struct NewsPanel {
    grid: Entity<LiveGridState<News>>,
    bus: Entity<InstrumentBus>,
    _follows_current: Subscription,
}

impl NewsPanel {
    pub fn new(
        store: GridStore<Headline>,
        bus: &Entity<InstrumentBus>,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = bus.read(cx).current();
        let grid = cx.new(|cx| {
            LiveGridState::new(news_schema(), News, store, cx)
                .with_view_id("news")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(quiet_changes())
                .with_view(news_view(current))
        });
        // A new filter, the order kept (a click on a header may have
        // changed it).
        let follows_current = cx.subscribe(bus, |panel, _, change: &InstrumentChange, cx| {
            panel.grid.update(cx, |grid, cx| {
                let view = news_view(change.current).with_sort(grid.sort().to_vec());
                grid.set_view(view, cx);
            });
            cx.notify();
        });
        Self {
            grid,
            bus: bus.clone(),
            _follows_current: follows_current,
        }
    }
}

dock_panel!(NewsPanel, "desk.news");

impl Panel for NewsPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.bus.read(cx).current() {
            Some(id) => SharedString::from(format!("News · {}", id.get().ticker)),
            None => SharedString::from("News"),
        }
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for NewsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for NewsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Headlines are prose: the theme's interface family, at the grids'
        // size.
        grid_font(cx)
            .font_family(cx.theme().font_family.clone())
            .child(
                LiveGrid::new(&self.grid)
                    .with_size(Size::Size(ROW_HEIGHT))
                    .bordered(false)
                    .accessibility_label("News"),
            )
    }
}

#[cfg(all(test, feature = "test-support"))]
mod wiring {
    use gpui_kit::TestAppContext;

    use super::super::positions::tests::{open, settle};
    use super::*;

    fn headline(seq: u64, instrument: Option<InstrumentId>) -> Headline {
        Headline {
            seq,
            time: Arc::from("00:00:00"),
            instrument,
            text: Arc::from("A simulated line"),
        }
    }

    #[gpui_kit::test]
    fn test_news_shows_the_newest_first_for_the_current_instrument(cx: &mut TestAppContext) {
        let (first, second) = (InstrumentId(0), InstrumentId(1));
        let (window, panel, bus, _) = open(cx, move |bus, _, cx| {
            let store = GridStore::new(news_store_options(), cx);
            store.writer().replace_all(
                [
                    headline(0, Some(first)),
                    headline(1, None),
                    headline(2, Some(second)),
                    headline(3, Some(first)),
                ]
                .map(Arc::new),
            );
            NewsPanel::new(store, bus, cx)
        });
        settle(cx, window);
        let grid = panel.read_with(cx, |panel, _| panel.grid.clone());
        let shown = |cx: &mut TestAppContext| {
            grid.read_with(cx, |grid, _| {
                let snapshot = grid.snapshot();
                grid.projection()
                    .rows()
                    .iter()
                    .filter_map(|id| snapshot.row(*id).map(|row| row.seq))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(shown(cx), [3, 2, 1, 0]);

        // The current instrument's and the market-wide ones.
        bus.update(cx, |bus, cx| bus.set_current(Some(first), cx));
        settle(cx, window);
        assert_eq!(shown(cx), [3, 1, 0]);
        bus.update(cx, |bus, cx| bus.set_current(None, cx));
        settle(cx, window);
        assert_eq!(shown(cx), [3, 2, 1, 0]);
    }
}
