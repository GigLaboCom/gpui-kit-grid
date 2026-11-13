//! The price ladder: one row per price around the market, highest on top,
//! keyed by tick. Sizes change in place; the rows never move under the
//! pointer. A left click in Buy or Sell places a limit order there, a
//! right click cancels the orders there (`click_request`).
//!
//! Every colour is the theme's: bid depth and ask depth shaded by size,
//! the best bid and ask solid, the last trade's price inverted, the volume
//! traded at each price shaded by its share of the session's largest, in
//! the delta's colour, and the point of control in `warning`.

use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{App, Context, FontWeight, Hsla, MouseButton, Rgba, px};
use gpui_kit_grid::{
    CellStyle, CellStyleContext, CellValue, ColumnAlign, ContextMenuGesture,
    ContextMenuGestureContext, GestureInput, GridArea, GridColumn, GridSchema, HighlightPolicy,
    LiveGridDelegate, LiveGridState, SelectionEdit, SelectionGesture, SelectionGestureContext,
    SortDirection, StandardContextMenuGesture, StandardSelectionGesture, StoreOptions, ViewSpec,
};

use crate::{
    exchange::Request,
    rows::{LadderColumn, LadderRow},
    trading::{instruments::ContractId, rows::Side},
};

/// How long a size's change lights its cell: briefly, as sizes move
/// several times a second.
const FLASH: Duration = Duration::from_millis(400);
/// How strongly a change lights its cell at first, in the theme's
/// foreground: neutral, so it reads on green, red and grey alike.
const FLASH_TINT: f32 = 0.16;
/// The contrast text needs against its cell (WCAG's ratio for large
/// text); below it, the theme's background or foreground, whichever reads.
const MIN_CONTRAST: f32 = 3.;
/// Rows kept between the last price and the ladder's top or bottom edge
/// before the ladder recentres by itself.
pub const EDGE_ROWS: usize = 2;

/// The ladder store's options: short flashes.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 4))
}

/// The ladder's hooks. They read the row and the theme only, take `&self`
/// and never read the grid.
pub struct Ladder;

impl LiveGridDelegate for Ladder {
    type Row = LadderRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, LadderRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        // A size or a volume that changed lights its cell, whichever way it
        // went; a cell that emptied does not. No arrow: the digits keep the
        // room.
        let flash = cell
            .change()
            .filter(|_| cell.value() != CellValue::Empty)
            .map(|change| theme.foreground.opacity(FLASH_TINT * change.intensity()));
        style.set_trend(None);
        let (fill, text, bold) = match cell.column() {
            LadderColumn::Buys => order_cell(row.buys, theme.blue, theme),
            LadderColumn::Sells => order_cell(row.sells, theme.magenta, theme),
            LadderColumn::Bid => depth_cell(row.bid, row.best_bid, row.depth, Side::Buy, theme),
            LadderColumn::Ask => depth_cell(row.ask, row.best_ask, row.depth, Side::Sell, theme),
            LadderColumn::Price => price_cell(row, theme),
            LadderColumn::Volume => volume_cell(row, theme),
            LadderColumn::Tick => (theme.table, theme.muted_foreground, false),
        };
        let background = flash.map_or(fill, |flash| fill.blend(flash));
        style.set_background(background);
        style.set_foreground(readable(text, background, theme));
        if bold {
            style.set_font_weight(FontWeight::BOLD);
        }
    }
}

/// A Buy or Sell cell: the trader's working size on its side's colour, or
/// a quiet click target.
fn order_cell(quantity: i64, color: Hsla, theme: &Theme) -> (Hsla, Hsla, bool) {
    if quantity > 0 {
        (color, theme.primary_foreground, true)
    } else {
        (theme.muted, theme.muted_foreground, false)
    }
}

/// A Bid or Ask cell: the best one solid, the others shaded by size.
fn depth_cell(size: i64, best: bool, depth: f32, side: Side, theme: &Theme) -> (Hsla, Hsla, bool) {
    let (shade, solid, on_solid) = match side {
        Side::Buy => (
            theme.button_success,
            theme.success,
            theme.success_foreground,
        ),
        Side::Sell => (theme.button_danger, theme.danger, theme.danger_foreground),
    };
    if best {
        (solid, on_solid, true)
    } else if size > 0 {
        let fill = theme
            .table
            .blend(shade.opacity(0.45 + 0.55 * depth))
            .blend(solid.opacity(0.18 * depth));
        (fill, theme.foreground, false)
    } else {
        (theme.table, theme.muted_foreground, false)
    }
}

/// The Price cell: the last trade's inverted, the best bid's and ask's in
/// their colours, the rest quiet.
fn price_cell(row: &LadderRow, theme: &Theme) -> (Hsla, Hsla, bool) {
    if row.last {
        (theme.chart_4, theme.background, true)
    } else if row.best_bid {
        (theme.muted, theme.success, true)
    } else if row.best_ask {
        (theme.muted, theme.danger, true)
    } else {
        (theme.muted, theme.foreground, false)
    }
}

/// The Volume cell: the point of control in `warning`; the others shaded
/// by their share of it, squared so the busy prices stand out of a flat
/// profile, the number in the delta's colour.
fn volume_cell(row: &LadderRow, theme: &Theme) -> (Hsla, Hsla, bool) {
    if row.poc {
        return (theme.warning, theme.warning_foreground, true);
    }
    if row.volume == 0 {
        return (theme.table, theme.muted_foreground, false);
    }
    let fill = theme
        .table
        .blend(theme.chart_1.opacity(0.1 + 0.9 * row.profile * row.profile));
    let text = match row.delta() {
        delta if delta > 0 => theme.chart_bullish,
        delta if delta < 0 => theme.chart_bearish,
        _ => theme.foreground,
    };
    (fill, text, false)
}

/// `text` if it reads on the opaque `background`, else the theme's
/// background or foreground, whichever reads better there.
fn readable(text: Hsla, background: Hsla, theme: &Theme) -> Hsla {
    if contrast(text, background) >= MIN_CONTRAST {
        text
    } else if contrast(theme.background, background) >= contrast(theme.foreground, background) {
        theme.background
    } else {
        theme.foreground
    }
}

/// WCAG's contrast ratio of two opaque colours, from 1 to 21.
fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// WCAG's relative luminance of an opaque colour.
fn luminance(color: Hsla) -> f32 {
    let rgba = Rgba::from(color);
    let linear = |channel: f32| {
        if channel <= 0.039_28 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
}

/// A ladder is read in place: no column sorts, moves or hides; numbers and
/// prices align right. Tick, the order, is hidden.
fn schema() -> GridSchema<LadderColumn> {
    let column = |column, title: &'static str, width: f32| {
        GridColumn::new(column, title)
            .with_width(px(width))
            .with_align(ColumnAlign::End)
            .with_sortable(false)
            .with_movable(false)
    };
    GridSchema::new()
        .column(column(LadderColumn::Buys, "Buy", 52.))
        .column(column(LadderColumn::Bid, "Bid", 64.))
        .column(column(LadderColumn::Price, "Price", 86.))
        .column(column(LadderColumn::Ask, "Ask", 64.))
        .column(column(LadderColumn::Sells, "Sell", 52.))
        .column(column(LadderColumn::Volume, "Volume", 112.))
        .column(column(LadderColumn::Tick, "Tick", 60.).with_visible(false))
}

/// The ladder's grid over `store`: highest price on top, a press that
/// trades instead of selecting, and the context menu on the header only.
pub fn grid(
    store: gpui_kit_grid::GridStore<LadderRow>,
    cx: &mut Context<LiveGridState<Ladder>>,
) -> LiveGridState<Ladder> {
    LiveGridState::new(schema(), Ladder, store, cx)
        .with_view_id("ladder")
        .with_change_paint(crate::trading::paint::quiet_changes())
        .with_selection_gesture(PressTrades)
        .with_context_menu_gesture(HeaderMenu)
        .with_view(ViewSpec::new().with_sort([(LadderColumn::Tick, SortDirection::Descending)]))
}

/// On the ladder a press is an order, not a selection: the keyboard
/// selects as usual, a press leaves the selection as it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct PressTrades;

impl SelectionGesture for PressTrades {
    fn edit(&self, context: &SelectionGestureContext) -> SelectionEdit {
        match context.input() {
            GestureInput::Keyboard => StandardSelectionGesture.edit(context),
            _ => SelectionEdit::Keep,
        }
    }
}

/// The platform's menu gesture on the header only: on the body a right
/// click cancels orders.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeaderMenu;

impl ContextMenuGesture for HeaderMenu {
    fn opens_menu(&self, context: &ContextMenuGestureContext) -> bool {
        context.area() == GridArea::Header && StandardContextMenuGesture.opens_menu(context)
    }
}

/// The request of a click on the ladder at `tick`: a left click in Buy or
/// Sell places a limit order of `quantity` there, a right click there
/// cancels that side's orders at the price. Other columns trade nothing.
pub fn click_request(
    contract: ContractId,
    tick: i64,
    column: LadderColumn,
    button: MouseButton,
    quantity: i64,
) -> Option<Request> {
    let side = match column {
        LadderColumn::Buys => Side::Buy,
        LadderColumn::Sells => Side::Sell,
        _ => return None,
    };
    match button {
        MouseButton::Left => Some(Request::Limit {
            contract,
            side,
            tick,
            quantity,
        }),
        MouseButton::Right => Some(Request::Cancel {
            contract,
            side,
            tick,
        }),
        _ => None,
    }
}

/// The display row of `tick`, if shown.
fn display_ix(grid: &LiveGridState<Ladder>, tick: i64) -> Option<usize> {
    grid.snapshot()
        .row_id(&tick)
        .and_then(|id| grid.projection().display_ix(id))
}

/// Scrolls `tick`'s row to the middle of the ladder. False while the
/// ladder cannot: the row is not shown yet, or the grid not laid out.
pub fn center(
    grid: &mut LiveGridState<Ladder>,
    tick: i64,
    cx: &mut Context<LiveGridState<Ladder>>,
) -> bool {
    let shown = grid.visible_rows().len();
    let Some(ix) = display_ix(grid, tick).filter(|_| shown > 2 * EDGE_ROWS) else {
        return false;
    };
    grid.scroll_to_row(ix.saturating_sub(shown / 2), cx);
    true
}

/// Whether `tick`'s row is in view, `EDGE_ROWS` or more from the edges;
/// `None` while it is not shown.
pub fn in_view(grid: &LiveGridState<Ladder>, tick: i64) -> Option<bool> {
    let ix = display_ix(grid, tick)?;
    let shown = grid.visible_rows();
    Some(ix >= shown.start + EDGE_ROWS && ix + EDGE_ROWS < shown.end)
}

#[cfg(test)]
mod tests {
    use gpui_kit::hsla;

    use super::*;

    #[test]
    fn test_clicks_in_the_order_columns_place_and_cancel() {
        let es = ContractId(0);
        let click = |column, button| click_request(es, 100, column, button, 5);
        assert_eq!(
            click(LadderColumn::Buys, MouseButton::Left),
            Some(Request::Limit {
                contract: es,
                side: Side::Buy,
                tick: 100,
                quantity: 5
            })
        );
        assert_eq!(
            click(LadderColumn::Sells, MouseButton::Right),
            Some(Request::Cancel {
                contract: es,
                side: Side::Sell,
                tick: 100
            })
        );
        assert_eq!(click(LadderColumn::Price, MouseButton::Left), None);
        assert_eq!(click(LadderColumn::Buys, MouseButton::Middle), None);
    }

    #[test]
    fn test_text_too_close_to_its_cell_gives_way() {
        let theme = Theme::default();
        let (black, white) = (hsla(0., 0., 0., 1.), hsla(0., 0., 1., 1.));
        assert!((contrast(black, white) - 21.).abs() < 1e-3);
        let yellow = hsla(0.15, 1., 0.5, 1.);
        let text = readable(yellow, yellow, &theme);
        assert_ne!(text, yellow);
        assert!(contrast(text, yellow) > 1.5);
    }
}
