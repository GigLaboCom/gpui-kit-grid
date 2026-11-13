//! The positions as a tree: account, then sector, then the positions,
//! each caption with its summed market value and P&L, the totals at the
//! bottom. Selecting a position makes its instrument current; the current
//! instrument is selected when it is held.
//!
//! Accounts and sectors are not rows, so the tree is a grouping by two
//! `GroupKey`s rather than a `TreeSpec` over parent keys.

use std::{fmt::Write as _, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size, Theme,
    dock::{BasePanel, Panel, PanelEvent},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    ParentElement as _, Render, Subscription, Window, px,
};
use gpui_kit_grid::{
    Aggregate, AggregateStyleContext, CellStyle, CellStyleContext, CellValue, ColumnSet,
    GridColumn, GridSchema, GridStore, GroupBy, GroupCaptionContext, GroupKey, HighlightPolicy,
    LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, PinnedEdge, SelectionMode,
    SortDirection, StoreOptions, ViewSpec,
};

use super::panels::{ROW_HEIGHT, dock_panel, grid_font};
use crate::trading::{
    bus::{InstrumentBus, InstrumentChange},
    instruments::InstrumentId,
    paint::quiet_changes,
    rows::{Position, PositionColumn},
};

/// How long a change flashes: the simulator revalues the positions four
/// times a second.
const FLASH: Duration = Duration::from_millis(800);

/// The positions store's options: short flashes.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 4))
}

/// The positions' hooks, colours from the theme only: tickers in
/// `base.yellow`, short quantities in `danger`, P&L and P&L % (rows,
/// captions and totals) by sign in `chart.bullish` and `chart.bearish`,
/// summed market values in `base.cyan`, and the solid selection bar of the
/// watchlist. A change tints its cell faintly (`QuietChanges`); only Last
/// shows the arrow, so Mkt Value and P&L keep their room for digits. They
/// take `&self` and never read the grid.
pub struct Positions;

/// The theme's colour for a gain or a loss.
fn signed(value: f64, theme: &Theme) -> Hsla {
    if value >= 0. {
        theme.chart_bullish
    } else {
        theme.chart_bearish
    }
}

impl LiveGridDelegate for Positions {
    type Row = Position;

    fn cell_style(&self, cell: &CellStyleContext<'_, Position>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        match cell.column() {
            PositionColumn::Ticker => style.set_foreground(theme.yellow),
            PositionColumn::Quantity if row.quantity < 0 => style.set_foreground(theme.danger),
            PositionColumn::Pnl | PositionColumn::PnlPct => {
                style.set_foreground(signed(row.pnl(), theme))
            }
            _ => {}
        }
        if cell.column() != PositionColumn::Last {
            style.set_trend(None);
        }
        if cell.is_selected() && style.background().is_none() {
            style.set_background(theme.table_active_border);
        }
    }

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, Position>,
        style: &mut CellStyle,
        cx: &App,
    ) {
        let theme = cx.theme();
        style.set_trend(None);
        style.set_foreground(match (cell.column(), cell.value()) {
            (PositionColumn::Pnl, Some(CellValue::Float(value))) => signed(value, theme),
            _ => theme.cyan,
        });
    }

    /// "CORE · 8", "Technology · 3".
    fn group_caption(
        &self,
        caption: &GroupCaptionContext<'_, Position>,
        out: &mut String,
        _: &App,
    ) {
        out.push_str(caption.value_text());
        let _ = write!(out, " · {}", caption.row_count());
    }
}

fn schema() -> GridSchema<PositionColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(PositionColumn::Ticker, "Ticker").with_width(px(64.)))
        .column(GridColumn::new(PositionColumn::Quantity, "Qty").with_width(px(64.)))
        .column(GridColumn::new(PositionColumn::Average, "Avg").with_width(px(76.)))
        .column(GridColumn::new(PositionColumn::Last, "Last").with_width(px(76.)))
        .column(GridColumn::new(PositionColumn::Value, "Mkt Value").with_width(px(100.)))
        .column(GridColumn::new(PositionColumn::Pnl, "P&L").with_width(px(88.)))
        .column(GridColumn::new(PositionColumn::PnlPct, "P&L %").with_width(px(64.)))
}

/// Account, then sector: neither changes, so neither depends on a column.
fn group_by() -> [GroupBy<Position>; 2] {
    [
        GroupBy::key(
            GroupKey::new("account", ColumnSet::EMPTY, |row: &Position| {
                CellValue::Text(row.account)
            })
            .with_title("Account"),
        ),
        GroupBy::key(
            GroupKey::new("sector", ColumnSet::EMPTY, |row: &Position| {
                CellValue::Text(row.sector.label())
            })
            .with_title("Sector"),
        ),
    ]
}

pub struct PositionsPanel {
    grid: Entity<LiveGridState<Positions>>,
    _subscriptions: [Subscription; 2],
}

impl PositionsPanel {
    /// The positions over `store`, grouped by account and sector, with the
    /// summed market value and P&L; selecting a position sets the current
    /// instrument, and the current instrument is selected when held.
    pub fn new(
        store: GridStore<Position>,
        bus: &Entity<InstrumentBus>,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| {
            LiveGridState::new(schema(), Positions, store, cx)
                .with_view_id("positions")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(quiet_changes())
                .with_view(
                    ViewSpec::new()
                        .with_sort([(PositionColumn::Ticker, SortDirection::Ascending)])
                        .with_group_by(group_by())
                        .with_aggregates([
                            (PositionColumn::Value, Aggregate::Sum),
                            (PositionColumn::Pnl, Aggregate::Sum),
                        ]),
                )
                .with_totals(PinnedEdge::Bottom)
        });
        let sets_current = cx.subscribe(&grid, {
            let bus = bus.clone();
            move |_, _, event: &LiveGridEvent<InstrumentId, PositionColumn>, cx| {
                if let LiveGridEvent::SelectionChange { selection, .. } = event
                    && let Some(id) = selection.iter().next().copied()
                {
                    bus.update(cx, |bus, cx| bus.set_current(Some(id), cx));
                }
            }
        });
        let follows_current = cx.subscribe(bus, |panel, _, change: &InstrumentChange, cx| {
            panel.grid.update(cx, |grid, cx| {
                let held = change
                    .current
                    .filter(|id| grid.snapshot().row_id(id).is_some());
                match held {
                    Some(id) => {
                        if !grid.selection().contains(&id) {
                            grid.select_key(Some(id), cx);
                        }
                        grid.scroll_to_key(&id, cx);
                    }
                    None if !grid.selection().is_empty() => grid.select_key(None, cx),
                    None => {}
                }
            });
        });
        Self {
            grid,
            _subscriptions: [sets_current, follows_current],
        }
    }
}

dock_panel!(PositionsPanel, "desk.positions");

impl Panel for PositionsPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Positions"
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for PositionsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for PositionsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        grid_font(cx).child(
            LiveGrid::new(&self.grid)
                .with_size(Size::Size(ROW_HEIGHT))
                .bordered(false)
                .accessibility_label("Positions"),
        )
    }
}

#[cfg(all(test, feature = "test-support"))]
pub(crate) mod tests {
    use std::sync::Arc;

    use gpui_kit::{AnyWindowHandle, TestAppContext, WindowOptions, test::TestWindowExt as _};

    use super::*;
    use crate::trading::market::{Control, Market};

    /// A window with the view `build` makes, over a bus whose simulator
    /// does not run: the test writes the store.
    pub(crate) fn open<V: Render>(
        cx: &mut TestAppContext,
        build: impl FnOnce(&Entity<InstrumentBus>, &Arc<Control>, &mut Context<V>) -> V + 'static,
    ) -> (
        AnyWindowHandle,
        Entity<V>,
        Entity<InstrumentBus>,
        Arc<Control>,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
        });
        let control = Control::new(0);
        let bus = cx.new(|_| InstrumentBus::new(control.clone()));
        let (window, view) = cx
            .update(|cx| {
                let (bus, control) = (bus.clone(), control.clone());
                gpui_kit::open_window(WindowOptions::default(), cx, move |_, cx| {
                    cx.new(|cx| build(&bus, &control, cx))
                })
            })
            .expect("open window");
        (window, view, bus, control)
    }

    /// Lets the store publish and the window paint a few frames.
    pub(crate) fn settle(cx: &mut TestAppContext, window: AnyWindowHandle) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
        }
    }

    #[gpui_kit::test]
    fn test_positions_and_the_current_instrument_follow_each_other(cx: &mut TestAppContext) {
        let held = Market::new(Some(7)).positions().to_vec();
        let (window, panel, bus, control) = open(cx, {
            let held = held.clone();
            move |bus, _, cx| {
                let store = GridStore::new(store_options(), cx);
                store.writer().replace_all(held.into_iter().map(Arc::new));
                PositionsPanel::new(store, bus, cx)
            }
        });
        settle(cx, window);
        let grid = panel.read_with(cx, |panel, _| panel.grid.clone());
        let current = |cx: &mut TestAppContext| bus.read_with(cx, |bus, _| bus.current());

        // Selecting a position makes its instrument current.
        let first = held[0].id;
        grid.update(cx, |grid, cx| grid.select_key(Some(first), cx));
        settle(cx, window);
        assert_eq!(current(cx), Some(first));
        assert_eq!(control.current(), Some(first));

        // A held instrument made current elsewhere is selected.
        let second = held[1].id;
        bus.update(cx, |bus, cx| bus.set_current(Some(second), cx));
        settle(cx, window);
        assert!(grid.read_with(cx, |grid, _| grid.selection().contains(&second)));
        assert_eq!(current(cx), Some(second));

        // One not held clears the selection and stays current.
        let not_held = InstrumentId::all()
            .find(|id| held.iter().all(|position| position.id != *id))
            .expect("an instrument without a position");
        bus.update(cx, |bus, cx| bus.set_current(Some(not_held), cx));
        settle(cx, window);
        assert!(grid.read_with(cx, |grid, _| grid.selection().is_empty()));
        assert_eq!(current(cx), Some(not_held));
    }
}
