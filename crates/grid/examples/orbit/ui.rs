//! What the panels share: the spacing, the flat panel each region sits in,
//! its header, the frame of the grid and the figures' font.

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Div, FontFeatures, FontWeight, ParentElement as _, Pixels, SharedString, Styled as _, div,
    prelude::FluentBuilder as _, px,
};

/// Around the main area and between panels.
pub const PAGE_PADDING: Pixels = px(10.);
pub const GAP: Pixels = px(8.);
/// The leading space of a panel's header.
pub const PANEL_PADDING: Pixels = px(12.);
pub const HEADER_HEIGHT: Pixels = px(36.);
/// The sky table's rows: dense.
pub const ROW_HEIGHT: Pixels = px(24.);

/// A panel: the theme's group box, a hairline border.
pub fn panel(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .min_w_0()
        .min_h_0()
        .bg(theme.group_box)
        .text_color(theme.group_box_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .overflow_hidden()
}

/// A panel's header: an accent tick, the title, and a muted detail after
/// it. Controls go after it with `.child`.
pub fn panel_header(title: &'static str, detail: Option<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_none()
        .w_full()
        .h(HEADER_HEIGHT)
        .pl(PANEL_PADDING)
        .pr(px(8.))
        .gap(px(10.))
        .border_b_1()
        .border_color(theme.border)
        .child(div().flex_none().w(px(3.)).h(px(14.)).bg(theme.primary))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(title),
        )
        .when_some(detail, |header, detail| {
            header.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme.muted_foreground)
                    .font_features(tabular_figures())
                    .child(detail),
            )
        })
}

/// The frame of the grid in a panel: the rest of the panel, in the grid's
/// font.
pub fn grid_frame(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex_1()
        .min_h_0()
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
}

/// Text in the grid's font with tabular figures, on one line.
pub fn figures(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
        .font_features(tabular_figures())
}

/// Tabular figures, for numbers outside the grid (the clock, the status
/// bar); the grid adds them to its cells.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}
