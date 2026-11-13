//! The trades tape: the newest trade on top, the latest `TAPE_LEN` kept by
//! the simulator, an old trade fading out as it leaves
//! (`HighlightPolicy::with_removal_fade`). The totals row at the bottom
//! sums the size and, for one instrument, gives the VWAP. `TAPE CUR|ALL`
//! switches between the current instrument's trades and every
//! instrument's.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size,
    dock::{BasePanel, Panel, PanelEvent},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, ParentElement as _, Render, SharedString, Subscription, Window, px,
    transparent_black,
};
use gpui_kit_grid::{
    Aggregate, AggregateFn, AggregateStyleContext, CellStyle, CellStyleContext, ColumnSet,
    GridColumn, GridColumns as _, GridSchema, GridStore, HighlightPolicy, LiveGrid,
    LiveGridDelegate, LiveGridState, OwnedValue, PinnedEdge, RowChange, SortDirection,
    StoreOptions, ViewSpec,
};

use super::panels::{ROW_HEIGHT, dock_panel, grid_font, ticker};
use crate::trading::{
    bus::InstrumentBus,
    market::{Control, TapeScope},
    paint::{TINT, quiet_changes},
    rows::{Print, PrintColumn, Side},
};

/// How long a new trade flashes.
const FLASH: Duration = Duration::from_millis(1_500);
/// How long an old trade takes to fade out.
const FADE_OUT: Duration = Duration::from_secs(2);
/// A trade worth this much or more is a block, in bold.
const BLOCK_VALUE: f64 = 250_000.;

/// The tape store's options: a short flash for a new trade, and a fade out
/// for an old one.
pub fn store_options() -> StoreOptions {
    StoreOptions::default()
        .with_highlight(HighlightPolicy::new(FLASH, 6).with_removal_fade(FADE_OUT))
}

/// The tape's hooks, colours from the theme only: side and price in
/// `success` for a buy and `danger` for a sell, a new trade tinted faintly
/// in its side's colour, an old one fading to nothing (no `danger` flash), the
/// ticker in `base.yellow`, the time in `muted.foreground`, blocks in
/// bold, totals in `base.cyan`. They take `&self` and never read the grid.
pub struct Tape;

impl LiveGridDelegate for Tape {
    type Row = Print;

    fn cell_style(&self, cell: &CellStyleContext<'_, Print>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        let side = match row.side {
            Side::Buy => theme.success,
            Side::Sell => theme.danger,
        };
        match cell.change().map(|change| (change, change.change())) {
            Some((change, RowChange::Removed)) => {
                style.set_background(transparent_black());
                style.set_foreground(change.fade(theme.muted_foreground));
                return;
            }
            Some((change, RowChange::Inserted)) => {
                style.set_background(side.opacity(TINT * change.intensity()));
            }
            _ => {}
        }
        match cell.column() {
            PrintColumn::Time => style.set_foreground(theme.muted_foreground),
            PrintColumn::Ticker => style.set_foreground(theme.yellow),
            PrintColumn::Side | PrintColumn::Price => style.set_foreground(side),
            PrintColumn::Size | PrintColumn::Seq => {}
        }
        if row.price * row.size as f64 >= BLOCK_VALUE {
            style.set_font_weight(FontWeight::BOLD);
        }
        if cell.is_selected() && style.background().is_none() {
            style.set_background(theme.table_active_border);
        }
    }

    fn aggregate_style(
        &self,
        _: &AggregateStyleContext<'_, Print>,
        style: &mut CellStyle,
        cx: &App,
    ) {
        style.set_foreground(cx.theme().cyan);
        // The arrow would take the digits' room.
        style.set_trend(None);
    }
}

/// The volume-weighted average price: the trades' value over their size.
pub struct Vwap;

impl AggregateFn<Print> for Vwap {
    fn aggregate(&self, _: PrintColumn, rows: &mut dyn Iterator<Item = &Print>) -> OwnedValue {
        let (value, size) = rows.fold((0., 0), |(value, size), row| {
            (value + row.price * row.size as f64, size + row.size)
        });
        if size == 0 {
            OwnedValue::Empty
        } else {
            OwnedValue::Float(value / size as f64)
        }
    }

    fn depends_on(&self, _: PrintColumn) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        columns.insert(PrintColumn::Price.ix());
        columns.insert(PrintColumn::Size.ix());
        columns
    }

    fn name(&self) -> &str {
        "VWAP"
    }
}

/// The totals of `scope`: the summed size, and the VWAP under Price for one
/// instrument (over several it means nothing).
fn aggregates(scope: TapeScope, vwap: &Aggregate<Print>) -> Vec<(PrintColumn, Aggregate<Print>)> {
    let mut aggregates = vec![(PrintColumn::Size, Aggregate::Sum)];
    if scope == TapeScope::Current {
        aggregates.push((PrintColumn::Price, vwap.clone()));
    }
    aggregates
}

/// The widths fit the tape's panel at the default window size (about
/// 350 px) in the widest theme font: the time with its milliseconds, a
/// six-digit total size.
fn schema() -> GridSchema<PrintColumn> {
    GridSchema::new()
        .column(GridColumn::new(PrintColumn::Time, "Time").with_width(px(114.)))
        .column(GridColumn::new(PrintColumn::Ticker, "Ticker").with_width(px(56.)))
        .column(GridColumn::new(PrintColumn::Side, "Side").with_width(px(46.)))
        .column(GridColumn::new(PrintColumn::Price, "Price").with_width(px(66.)))
        .column(GridColumn::new(PrintColumn::Size, "Size").with_width(px(64.)))
        // The order of the trades: newest first.
        .column(
            GridColumn::new(PrintColumn::Seq, "Seq")
                .with_width(px(72.))
                .with_visible(false),
        )
}

pub struct TapePanel {
    grid: Entity<LiveGridState<Tape>>,
    bus: Entity<InstrumentBus>,
    control: Arc<Control>,
    vwap: Aggregate<Print>,
    _observe: Subscription,
}

impl TapePanel {
    /// The tape over `store`, which the simulator fills with the trades of
    /// the scope in `control`.
    pub fn new(
        store: GridStore<Print>,
        bus: &Entity<InstrumentBus>,
        control: Arc<Control>,
        cx: &mut Context<Self>,
    ) -> Self {
        let vwap = Aggregate::Custom(Arc::new(Vwap));
        let view = ViewSpec::new()
            .with_sort([(PrintColumn::Seq, SortDirection::Descending)])
            .with_aggregates(aggregates(control.tape_scope(), &vwap));
        let grid = cx.new(|cx| {
            LiveGridState::new(schema(), Tape, store, cx)
                .with_view_id("tape")
                .with_change_paint(quiet_changes())
                .with_view(view)
                .with_totals(PinnedEdge::Bottom)
        });
        Self {
            grid,
            bus: bus.clone(),
            control,
            vwap,
            _observe: cx.observe(bus, |_, _, cx| cx.notify()),
        }
    }

    pub fn scope(&self) -> TapeScope {
        self.control.tape_scope()
    }

    /// Shows the trades of `scope`: the simulator starts the tape afresh,
    /// and the totals follow.
    pub fn set_scope(&mut self, scope: TapeScope, cx: &mut Context<Self>) {
        self.control.set_tape_scope(scope);
        let aggregates = aggregates(scope, &self.vwap);
        self.grid
            .update(cx, |grid, cx| grid.set_aggregates(aggregates, cx));
        cx.notify();
    }
}

dock_panel!(TapePanel, "desk.tape");

impl Panel for TapePanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(match self.scope() {
            TapeScope::Current => format!("Trades · {}", ticker(self.bus.read(cx).current())),
            TapeScope::All => "Trades · All".to_owned(),
        })
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for TapePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for TapePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        grid_font(cx).child(
            LiveGrid::new(&self.grid)
                .with_size(Size::Size(ROW_HEIGHT))
                .bordered(false)
                .accessibility_label("Trades"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print(seq: u64, price: f64, size: i64) -> Print {
        Print {
            seq,
            time: Arc::from("00:00:00.000"),
            ticker: "NVDA",
            price,
            size,
            side: Side::Buy,
        }
    }

    #[test]
    fn test_vwap_weighs_prices_by_size() {
        let prints = [print(0, 10., 100), print(1, 20., 300)];
        let value = Vwap.aggregate(PrintColumn::Price, &mut prints.iter());
        assert_eq!(value, OwnedValue::Float(17.5));
        let value = Vwap.aggregate(PrintColumn::Price, &mut [].iter());
        assert_eq!(value, OwnedValue::Empty);
    }

    #[test]
    fn test_totals_give_the_vwap_of_one_instrument_only() {
        let vwap = Aggregate::Custom(Arc::new(Vwap));
        let columns = |scope| -> Vec<PrintColumn> {
            aggregates(scope, &vwap)
                .into_iter()
                .map(|(column, _)| column)
                .collect()
        };
        assert_eq!(
            columns(TapeScope::Current),
            [PrintColumn::Size, PrintColumn::Price]
        );
        assert_eq!(columns(TapeScope::All), [PrintColumn::Size]);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod wiring {
    use gpui_kit::TestAppContext;

    use super::super::positions::tests::{open, settle};
    use super::*;

    fn print(seq: u64, price: f64, size: i64, side: Side) -> Print {
        Print {
            seq,
            time: Arc::from("00:00:00.000"),
            ticker: "NVDA",
            price,
            size,
            side,
        }
    }

    #[gpui_kit::test]
    fn test_tape_shows_the_newest_first_and_switches_its_scope(cx: &mut TestAppContext) {
        let (window, panel, _, control) = open(cx, |bus, control, cx| {
            let store = GridStore::new(store_options(), cx);
            store.writer().replace_all(
                [
                    print(0, 10., 100, Side::Buy),
                    print(1, 20., 300, Side::Sell),
                    print(2, 15., 200, Side::Buy),
                ]
                .map(Arc::new),
            );
            TapePanel::new(store, bus, control.clone(), cx)
        });
        settle(cx, window);
        let grid = panel.read_with(cx, |panel, _| panel.grid.clone());
        let shown = grid.read_with(cx, |grid, _| {
            let snapshot = grid.snapshot();
            grid.projection()
                .rows()
                .iter()
                .filter_map(|id| snapshot.row(*id).map(|row| row.seq))
                .collect::<Vec<_>>()
        });
        assert_eq!(shown, [2, 1, 0]);
        let totals = |cx: &mut TestAppContext| {
            grid.read_with(cx, |grid, _| {
                grid.aggregates()
                    .iter()
                    .map(|(column, _)| *column)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(totals(cx), [PrintColumn::Size, PrintColumn::Price]);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.totals()),
            Some(PinnedEdge::Bottom)
        );

        // `TAPE ALL` tells the simulator and drops the VWAP.
        panel.update(cx, |panel, cx| panel.set_scope(TapeScope::All, cx));
        settle(cx, window);
        assert_eq!(control.tape_scope(), TapeScope::All);
        assert_eq!(totals(cx), [PrintColumn::Size]);
        panel.update(cx, |panel, cx| panel.set_scope(TapeScope::Current, cx));
        assert_eq!(control.tape_scope(), TapeScope::Current);
        assert_eq!(totals(cx), [PrintColumn::Size, PrintColumn::Price]);
    }
}
