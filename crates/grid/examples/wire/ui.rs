//! What the wire's surfaces share: the spacing, the card every region sits
//! in, its header, and the frame a grid takes inside a card.

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonRounded, ButtonVariants as _},
};
use gpui_kit::{
    App, Div, ElementId, Entity, FontFeatures, FontWeight, IntoElement, ParentElement as _, Pixels,
    SharedString, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{GridRow, LiveGridDelegate, LiveGridState};

/// Around the main area: the genre is airier than a dense desk.
pub const PAGE_PADDING: Pixels = px(20.);
/// Between two cards.
pub const GAP: Pixels = px(16.);
/// The leading and trailing space inside a card.
pub const CARD_PADDING: Pixels = px(16.);
/// A grid's inset from the card's edges: its cells add their own padding,
/// so its text lines up with the card's header; and its square corners
/// stay inside the card's rounded ones.
pub const GRID_INSET: Pixels = px(8.);
/// A card's header: its title line.
pub const HEADER_HEIGHT: Pixels = px(48.);

/// A card: the theme's group box, a hairline border, the large radius.
pub fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .min_w_0()
        .min_h_0()
        .bg(theme.group_box)
        .text_color(theme.group_box_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .overflow_hidden()
}

/// A card's header: the title, and a muted detail after it.
pub fn card_header(title: impl Into<SharedString>, detail: Option<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_none()
        .w_full()
        .h(HEADER_HEIGHT)
        .px(CARD_PADDING)
        .gap(px(10.))
        .child(
            div()
                .flex_none()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(title.into()),
        )
        .when_some(detail, |header, detail| {
            header.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
        })
}

/// The frame of a grid in a card: the rest of the card's height, inset by
/// `GRID_INSET` at the sides and the bottom.
pub fn grid_frame() -> Div {
    div().flex_1().min_h_0().px(GRID_INSET).pb(GRID_INSET)
}

/// The words of `SIMULATED` labels and badges.
pub const SIMULATED: &str = "SIMULATED";

/// A muted label over a card's section.
pub fn section_label(label: &'static str, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_size(px(12.))
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

/// How strongly a selected chip shows the accent.
const CHIP_TINT: f32 = 0.3;

/// A chip: a small pill button, the accent's tint when `selected`, an
/// outline otherwise; a toggle to assistive technology.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &App,
) -> Button {
    let accent = cx.theme().primary;
    Button::new(id)
        .label(label)
        .small()
        .rounded(ButtonRounded::Large)
        .toggled(selected)
        .map(|chip| {
            if selected {
                chip.custom(
                    ButtonCustomVariant::new(cx)
                        .color(accent.opacity(CHIP_TINT))
                        .foreground(accent)
                        .hover(accent.opacity(CHIP_TINT * 1.5))
                        .active(accent.opacity(CHIP_TINT * 2.)),
                )
            } else {
                chip.outline()
            }
        })
}

/// Room kept right of a filled column: the grid's scrollbar.
const FILL_SLACK: Pixels = px(14.);

/// Gives `column` the width the grid's other visible columns leave of
/// `width`, the grid's frame, and at least `min`. The grid has no column
/// that takes the rest of its width; this stands in for one, called when
/// the window's size changes.
pub fn fill_column<D: LiveGridDelegate>(
    grid: &Entity<LiveGridState<D>>,
    column: <D::Row as GridRow>::Column,
    width: Pixels,
    min: Pixels,
    cx: &mut App,
) {
    grid.update(cx, |grid, cx| {
        let others = grid
            .visible_columns()
            .filter(|shown| *shown != column)
            .fold(px(0.), |sum, shown| sum + grid.column_width(shown));
        let fill = (width - others - FILL_SLACK).max(min).floor();
        grid.set_column_width(column, fill, cx);
    });
}

/// Inter's tabular figures, for numbers outside the grids (the clock, the
/// status bar, the chart); the grids add them to their cells. Not for
/// prose: Inter's tabular forms widen the hyphen too.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}
