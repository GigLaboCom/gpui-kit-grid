//! The order book of the current instrument: 20 asks above 20 bids around
//! a spread line, each level's size drawn over a bar of its depth (its size
//! against the book's largest level), in each theme's own way: an element
//! in the Size cell, the value still a number.
//!
//! The grid has no separator rows, so the spread line is a row of the book
//! (`SPREAD_LEVEL`, priced at the mid): sorted by Price descending it lies
//! between the asks and the bids, and `cell_style` paints it as a
//! separator in the header's colours.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size, Theme,
    dock::{BasePanel, Panel, PanelEvent},
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, IntoElement, ParentElement as _, Render, Rgba, SharedString, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, px, relative,
};
use gpui_kit_grid::{
    CellContent, CellElementContext, CellStyle, CellStyleContext, ColumnAlign, GridColumn,
    GridSchema, GridStore, HighlightPolicy, LiveGrid, LiveGridDelegate, LiveGridEvent,
    LiveGridState, SortDirection, StoreOptions, ViewSpec,
};

use super::{
    panels::{ROW_HEIGHT, dock_panel, grid_font, ticker},
    theme::DeskTheme,
};
use crate::trading::{
    bus::{InstrumentBus, InstrumentChange},
    market::BOOK_LEVELS,
    paint::quiet_changes,
    rows::{BookColumn, BookLevel, Side},
    theme::TradingTheme as _,
};

/// How long a change flashes: briefly, as sizes and prices move several
/// times a second.
const FLASH: Duration = Duration::from_millis(400);
/// The contrast a level's text needs against its shade (WCAG's ratio for
/// large text); below it, the text takes the theme's background or
/// foreground, whichever reads better.
const MIN_CONTRAST: f32 = 3.;

/// The book store's options: short flashes.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 4))
}

/// The book's hooks, colours from the theme only:
///
/// - the Size cell is a bar of the level's depth (its size over the
///   largest level's) with the size over it, an element
///   (`render_cell`): Depth with `button.success.background` for bids and
///   `button.danger.background` for asks, brightened towards `success` and
///   `danger`; Heat along the `chart.1`–`chart.4` scale; Amber with the
///   amber `primary`, deeper the larger the level;
/// - a change tints the cell faintly, with its arrow (`QuietChanges`),
///   except in Cum, which adds up the levels above and would flash with
///   each of them;
/// - prices in `success` (bids) and `danger` (asks); the best bid and ask
///   bold, their price inverted; a size too close to its bar (a red ask on
///   a deep amber or heat bar) in the theme's background or foreground;
/// - the spread line in the header's colours.
///
/// They take `&self` and never read the grid; the styles never allocate
/// (the bar, an element, does).
pub struct Book;

impl LiveGridDelegate for Book {
    type Row = BookLevel;

    fn cell_style(&self, cell: &CellStyleContext<'_, BookLevel>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        let column = cell.column();
        if row.is_spread() {
            style.set_background(theme.table_head);
            style.set_foreground(theme.table_head_foreground);
            style.set_trend(None);
            return;
        }
        if column == BookColumn::Total {
            style.set_trend(None);
            style.set_background(gpui_kit::transparent_black());
        }
        let background = style
            .background()
            .map_or(theme.table, |flash| theme.table.blend(flash));
        let side = match row.side() {
            Side::Buy => theme.success,
            Side::Sell => theme.danger,
        };
        match column {
            BookColumn::Price if row.is_best() => {
                style.set_background(side);
                style.set_foreground(theme.background);
            }
            BookColumn::Price => style.set_foreground(readable(side, background, theme)),
            _ => {
                let text = style.foreground().unwrap_or(theme.foreground);
                style.set_foreground(readable(text, background, theme));
            }
        }
        if row.is_best() {
            style.set_font_weight(FontWeight::BOLD);
        }
    }

    /// The Size cell of a level: its depth bar from the right, the size
    /// over it at the right edge; the spread line's label stays text.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, BookLevel>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let row = cell.row();
        if cell.column() != BookColumn::Size || row.is_spread() {
            return None;
        }
        let theme = cx.theme();
        let bar = depth_fill(DeskTheme::current(cx), row, theme);
        let depth = row.depth.clamp(0., 1.);
        // The text sits over the bar where the bar is long, over the table
        // where it is short.
        let under = if depth > 0.35 { bar } else { theme.table };
        let text = readable(theme.foreground, under, theme);
        Some(
            div()
                .relative()
                .size_full()
                .flex()
                .items_center()
                .justify_end()
                .child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .bottom(px(2.))
                        .right_0()
                        .w(relative(depth))
                        .bg(bar),
                )
                .child(
                    div()
                        .relative()
                        .text_color(text)
                        .when(row.is_best(), |this| this.font_weight(FontWeight::BOLD))
                        .child(SharedString::from(cell.text().to_owned())),
                )
                .into_any_element(),
        )
    }
}

/// The depth bar of a level, opaque, over the table's background.
fn depth_fill(desk: Option<DeskTheme>, row: &BookLevel, theme: &Theme) -> Hsla {
    let depth = row.depth.clamp(0., 1.);
    match desk {
        Some(DeskTheme::Heat) => {
            let heat = heat_scale(
                [theme.chart_1, theme.chart_2, theme.chart_3, theme.chart_4],
                depth,
            );
            theme.table.blend(heat.opacity(0.15 + 0.5 * depth))
        }
        Some(DeskTheme::Amber) => theme.table.blend(theme.primary.opacity(0.06 + 0.6 * depth)),
        Some(DeskTheme::Depth) | None => {
            let (token, bright) = match row.side() {
                Side::Buy => (theme.button_success, theme.success),
                Side::Sell => (theme.button_danger, theme.danger),
            };
            theme
                .table
                .blend(token.opacity(0.35 + 0.65 * depth))
                .blend(bright.opacity(0.25 * depth))
        }
    }
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

/// The colour at `t` (0 to 1) of a scale through four stops, evenly
/// spaced: the first at 0, the last at 1, blended between.
pub fn heat_scale(stops: [Hsla; 4], t: f32) -> Hsla {
    let at = t.clamp(0., 1.) * 3.;
    let ix = (at as usize).min(2);
    stops[ix].blend(stops[ix + 1].opacity(at - ix as f32))
}

fn schema() -> GridSchema<BookColumn> {
    // Not sortable: the asks stay above the bids.
    let column = |column, title: &'static str, width: f32| {
        GridColumn::new(column, title)
            .with_width(px(width))
            .with_sortable(false)
    };
    GridSchema::new()
        .column(column(BookColumn::Price, "Price", 80.))
        // The depth bar with the size over it; right-aligned, so the spread
        // line's label sits by its value.
        .column(
            column(BookColumn::Size, "Size", 80.)
                .with_align(ColumnAlign::End)
                .with_content(CellContent::Element),
        )
        .column(column(BookColumn::Total, "Cum", 64.))
        .column(column(BookColumn::Orders, "Ord", 44.))
        .column(column(BookColumn::Side, "Side", 48.).with_visible(false))
}

pub struct BookPanel {
    grid: Entity<LiveGridState<Book>>,
    bus: Entity<InstrumentBus>,
    /// The spread line waits to be centred: from a change of the
    /// instrument until the book's rows are shown.
    center_spread: bool,
    _subscriptions: [Subscription; 2],
}

impl BookPanel {
    /// The book over `store`, which the simulator fills with the current
    /// instrument's levels; it centres the spread line when the instrument
    /// changes.
    pub fn new(
        store: GridStore<BookLevel>,
        bus: &Entity<InstrumentBus>,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| {
            LiveGridState::new(schema(), Book, store, cx)
                .with_view_id("book")
                .with_change_paint(quiet_changes())
                .with_view(
                    ViewSpec::new().with_sort([(BookColumn::Price, SortDirection::Descending)]),
                )
        });
        let follows_current = cx.subscribe(bus, |panel, _, _: &InstrumentChange, cx| {
            panel.center_spread = !panel.try_center_spread(cx);
            cx.notify();
        });
        let rows_shown = cx.subscribe(
            &grid,
            |panel, _, event: &LiveGridEvent<i8, BookColumn>, cx| {
                if panel.center_spread && matches!(event, LiveGridEvent::ProjectionChange { .. }) {
                    panel.center_spread = !panel.try_center_spread(cx);
                }
            },
        );
        Self {
            grid,
            bus: bus.clone(),
            center_spread: true,
            _subscriptions: [follows_current, rows_shown],
        }
    }

    /// The book's grid.
    #[cfg(feature = "test-support")]
    pub fn grid(&self) -> &Entity<LiveGridState<Book>> {
        &self.grid
    }

    /// Scrolls the spread line to the middle of the rows; returns false
    /// while the book has no rows to scroll.
    fn try_center_spread(&self, cx: &mut Context<Self>) -> bool {
        self.grid.update(cx, |grid, cx| {
            if grid.projection().len() <= BOOK_LEVELS {
                return false;
            }
            let shown = grid.visible_rows().len();
            grid.scroll_to_row(BOOK_LEVELS.saturating_sub(shown / 2), cx);
            true
        })
    }
}

dock_panel!(BookPanel, "desk.book");

impl Panel for BookPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(format!("Book · {}", ticker(self.bus.read(cx).current())))
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for BookPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for BookPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        grid_font(cx).child(
            LiveGrid::new(&self.grid)
                .with_size(Size::Size(ROW_HEIGHT))
                .bordered(false)
                .accessibility_label("Order book"),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::hsla;

    use super::*;

    #[test]
    fn test_heat_scale_runs_through_its_stops() {
        let stops = [
            hsla(0.55, 1., 0.45, 1.),
            hsla(0.11, 1., 0.5, 1.),
            hsla(0.03, 1., 0.5, 1.),
            hsla(0., 0., 1., 1.),
        ];
        let close = |a: Hsla, b: Hsla| {
            let (a, b) = (gpui_kit::Rgba::from(a), gpui_kit::Rgba::from(b));
            (a.r - b.r).abs() < 1e-3 && (a.g - b.g).abs() < 1e-3 && (a.b - b.b).abs() < 1e-3
        };
        assert!(close(heat_scale(stops, 0.), stops[0]));
        assert!(close(heat_scale(stops, 1. / 3.), stops[1]));
        assert!(close(heat_scale(stops, 2. / 3.), stops[2]));
        assert!(close(heat_scale(stops, 1.), stops[3]));
        assert!(close(heat_scale(stops, 7.), stops[3]));
        // Halfway between the last two stops: lighter than the third.
        assert!(heat_scale(stops, 5. / 6.).l > stops[2].l);
    }

    #[test]
    fn test_text_too_close_to_its_shade_gives_way() {
        let (black, white) = (hsla(0., 0., 0., 1.), hsla(0., 0., 1., 1.));
        assert!((contrast(black, white) - 21.).abs() < 1e-3);
        let theme = Theme::default();
        let red = hsla(0., 0.65, 0.5, 1.);
        // Red reads on black, not on a deep orange shade.
        assert_eq!(readable(red, black, &theme), red);
        let orange = hsla(0.08, 1., 0.35, 1.);
        let text = readable(red, orange, &theme);
        assert_ne!(text, red);
        assert!(contrast(text, orange) >= contrast(red, orange));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod wiring {
    use std::sync::Arc;

    use gpui_kit::TestAppContext;

    use super::super::positions::tests::{open, settle};
    use super::*;
    use crate::trading::{instruments::InstrumentId, market::Market, rows::SPREAD_LEVEL};

    #[gpui_kit::test]
    fn test_book_shows_the_asks_over_the_spread_line_over_the_bids(cx: &mut TestAppContext) {
        let levels = Market::new(Some(7)).book(InstrumentId(3));
        let (window, panel, _, _) = open(cx, move |bus, _, cx| {
            let store = GridStore::new(store_options(), cx);
            store.writer().replace_all(levels.into_iter().map(Arc::new));
            BookPanel::new(store, bus, cx)
        });
        settle(cx, window);
        let grid = panel.read_with(cx, |panel, _| panel.grid.clone());
        let shown = grid.read_with(cx, |grid, _| {
            let snapshot = grid.snapshot();
            grid.projection()
                .rows()
                .iter()
                .filter_map(|id| snapshot.row(*id).map(|row| row.level))
                .collect::<Vec<_>>()
        });
        let levels = BOOK_LEVELS as i8;
        let expected: Vec<i8> = (1..=levels)
            .rev()
            .map(|depth| -depth)
            .chain([SPREAD_LEVEL])
            .chain(1..=levels)
            .collect();
        assert_eq!(shown, expected);

        // A depth bar per level on screen, in the Size cell only; the
        // spread line's label stays text.
        let (hosts, painted) = grid.read_with(cx, |grid, _| {
            (grid.stats().cell_hosts(), grid.visible_keys())
        });
        let painted_levels = painted
            .iter()
            .filter(|level| **level != SPREAD_LEVEL)
            .count();
        assert!(painted_levels > 0);
        assert_eq!(hosts, painted_levels);
    }
}
