//! The time and sales, the working orders and the fills: their hooks,
//! columns and store options. Colours are the theme's only; the hooks
//! take `&self` and never read the grid.

use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{App, Context, FontWeight, Hsla, px, transparent_black};
use gpui_kit_grid::{
    CellStyle, CellStyleContext, CellValue, GridColumn, GridSchema, GridStore, HighlightPolicy,
    LiveGridDelegate, LiveGridState, RowChange, SelectionMode, SortDirection, StoreOptions,
    ViewSpec,
};

use crate::{
    rows::{FillColumn, FillRow, OrderColumn, OrderRow, TradeColumn, TradeRow},
    trading::{
        paint::{TINT, quiet_changes},
        rows::Side,
    },
};

/// How long a new trade, order or fill tints its row.
const FLASH: Duration = Duration::from_millis(1_200);
/// How long a trade leaving the time and sales, or an order leaving the
/// working orders, takes to fade out.
const FADE_OUT: Duration = Duration::from_millis(1_500);

/// The aggressor's colour: a buyer took the ask, a seller hit the bid.
fn aggressor(side: Side, theme: &Theme) -> Hsla {
    match side {
        Side::Buy => theme.success,
        Side::Sell => theme.danger,
    }
}

/// The trader's own side: the ladder's colours for working orders.
fn own_side(side: Side, theme: &Theme) -> Hsla {
    match side {
        Side::Buy => theme.blue,
        Side::Sell => theme.magenta,
    }
}

/// A new row tinted in `color`; a leaving one faded to nothing. Returns
/// true for a leaving row, which keeps no other colour.
fn paint_change<R: gpui_kit_grid::GridRow>(
    cell: &CellStyleContext<'_, R>,
    color: Hsla,
    style: &mut CellStyle,
    theme: &Theme,
) -> bool {
    style.set_trend(None);
    match cell.change().map(|change| (change, change.change())) {
        Some((change, RowChange::Removed)) => {
            style.set_background(transparent_black());
            style.set_foreground(change.fade(theme.muted_foreground));
            true
        }
        Some((change, RowChange::Inserted)) => {
            style.set_background(color.opacity(TINT * change.intensity()));
            false
        }
        _ => false,
    }
}

/// A grid whose rows are a log, newest first by `column`: the selection
/// bar of a row clicked, otherwise read-only.
fn log_grid<D: LiveGridDelegate>(
    schema: GridSchema<<D::Row as gpui_kit_grid::GridRow>::Column>,
    delegate: D,
    store: GridStore<D::Row>,
    id: &'static str,
    newest_first: <D::Row as gpui_kit_grid::GridRow>::Column,
    cx: &mut Context<LiveGridState<D>>,
) -> LiveGridState<D> {
    LiveGridState::new(schema, delegate, store, cx)
        .with_view_id(id)
        .with_selection_mode(SelectionMode::RowSingle)
        .with_change_paint(quiet_changes())
        .with_view(ViewSpec::new().with_sort([(newest_first, SortDirection::Descending)]))
}

// The time and sales.

/// The time and sales store: a new trade tints, an old one fades out.
pub fn tape_options() -> StoreOptions {
    StoreOptions::default()
        .with_highlight(HighlightPolicy::new(FLASH, 6).with_removal_fade(FADE_OUT))
}

/// The time and sales' hooks: price, size and side in the aggressor's
/// colour, the time quiet, blocks bold, the trader's own trades on their
/// side's colour.
pub struct Tape;

impl LiveGridDelegate for Tape {
    type Row = TradeRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, TradeRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        let side = aggressor(row.side, theme);
        if paint_change(cell, side, style, theme) {
            return;
        }
        match cell.column() {
            TradeColumn::Time | TradeColumn::Seq => style.set_foreground(theme.muted_foreground),
            TradeColumn::Price | TradeColumn::Size | TradeColumn::Side => {
                style.set_foreground(side)
            }
        }
        if row.own {
            style.set_background(own_side(row.side, theme).opacity(0.3));
            style.set_foreground(theme.foreground);
        }
        if row.block || row.own {
            style.set_font_weight(FontWeight::BOLD);
        }
    }
}

fn tape_schema() -> GridSchema<TradeColumn> {
    GridSchema::new()
        .column(GridColumn::new(TradeColumn::Time, "Time").with_width(px(116.)))
        .column(GridColumn::new(TradeColumn::Price, "Price").with_width(px(78.)))
        .column(GridColumn::new(TradeColumn::Size, "Size").with_width(px(50.)))
        .column(GridColumn::new(TradeColumn::Side, "Side").with_width(px(44.)))
        .column(
            GridColumn::new(TradeColumn::Seq, "Seq")
                .with_width(px(60.))
                .with_visible(false),
        )
}

/// The time and sales over `store`, newest on top.
pub fn tape(
    store: GridStore<TradeRow>,
    cx: &mut Context<LiveGridState<Tape>>,
) -> LiveGridState<Tape> {
    log_grid(tape_schema(), Tape, store, "tape", TradeColumn::Seq, cx)
}

// The working orders.

/// The working orders store: a new order tints, a filled or cancelled one
/// fades out.
pub fn order_options() -> StoreOptions {
    StoreOptions::default()
        .with_highlight(HighlightPolicy::new(FLASH, 6).with_removal_fade(FADE_OUT))
}

/// The working orders' hooks: the side in the ladder's order colours, the
/// contract in `base.yellow`.
pub struct Orders;

impl LiveGridDelegate for Orders {
    type Row = OrderRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, OrderRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let side = own_side(cell.row().side, theme);
        if paint_change(cell, side, style, theme) {
            return;
        }
        match cell.column() {
            OrderColumn::Id | OrderColumn::Time => style.set_foreground(theme.muted_foreground),
            OrderColumn::Contract => style.set_foreground(theme.yellow),
            OrderColumn::Side | OrderColumn::Quantity | OrderColumn::Price => {
                style.set_foreground(side)
            }
        }
    }
}

fn order_schema() -> GridSchema<OrderColumn> {
    GridSchema::new()
        .column(GridColumn::new(OrderColumn::Id, "#").with_width(px(36.)))
        .column(GridColumn::new(OrderColumn::Time, "Time").with_width(px(80.)))
        .column(GridColumn::new(OrderColumn::Contract, "Contract").with_width(px(76.)))
        .column(GridColumn::new(OrderColumn::Side, "Side").with_width(px(44.)))
        .column(GridColumn::new(OrderColumn::Quantity, "Qty").with_width(px(40.)))
        .column(GridColumn::new(OrderColumn::Price, "Limit").with_width(px(84.)))
}

/// The working orders over `store`, newest on top.
pub fn orders(
    store: GridStore<OrderRow>,
    cx: &mut Context<LiveGridState<Orders>>,
) -> LiveGridState<Orders> {
    log_grid(order_schema(), Orders, store, "orders", OrderColumn::Id, cx)
}

// The fills.

/// The fills store: a new fill tints.
pub fn fill_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 6))
}

/// The fills' hooks: the side in the ladder's order colours, the contract
/// in `base.yellow`, the realised P&L by sign in `chart.bullish` and
/// `chart.bearish`.
pub struct Fills;

impl LiveGridDelegate for Fills {
    type Row = FillRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, FillRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let side = own_side(cell.row().side, theme);
        paint_change(cell, side, style, theme);
        match (cell.column(), cell.value()) {
            (FillColumn::Id | FillColumn::Time, _) => style.set_foreground(theme.muted_foreground),
            (FillColumn::Contract, _) => style.set_foreground(theme.yellow),
            (FillColumn::Side | FillColumn::Quantity | FillColumn::Price, _) => {
                style.set_foreground(side)
            }
            (FillColumn::Realized, CellValue::Float(pnl)) => style.set_foreground(if pnl >= 0. {
                theme.chart_bullish
            } else {
                theme.chart_bearish
            }),
            (FillColumn::Realized, _) => {}
        }
    }
}

fn fill_schema() -> GridSchema<FillColumn> {
    GridSchema::new()
        // The order of the fills: newest first.
        .column(
            GridColumn::new(FillColumn::Id, "#")
                .with_width(px(36.))
                .with_visible(false),
        )
        .column(GridColumn::new(FillColumn::Time, "Time").with_width(px(80.)))
        .column(GridColumn::new(FillColumn::Contract, "Contract").with_width(px(76.)))
        .column(GridColumn::new(FillColumn::Side, "Side").with_width(px(44.)))
        .column(GridColumn::new(FillColumn::Quantity, "Qty").with_width(px(40.)))
        .column(GridColumn::new(FillColumn::Price, "Price").with_width(px(84.)))
        .column(GridColumn::new(FillColumn::Realized, "Realized $").with_width(px(92.)))
}

/// The fills over `store`, newest on top.
pub fn fills(
    store: GridStore<FillRow>,
    cx: &mut Context<LiveGridState<Fills>>,
) -> LiveGridState<Fills> {
    log_grid(fill_schema(), Fills, store, "fills", FillColumn::Id, cx)
}
