//! What the broker's surfaces share: the spacing, the card every region
//! sits in, its header, the frame a grid takes inside a card, an empty
//! state, and the stat a card shows (a muted label over a figure).

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, Div, FontFeatures, FontWeight, Hsla, IntoElement, ParentElement as _, Pixels,
    SharedString, Styled as _, div, prelude::FluentBuilder as _, px,
};

/// Around the main area: a friendly app, not a dense desk.
pub const PAGE_PADDING: Pixels = px(20.);
/// Between two cards.
pub const GAP: Pixels = px(16.);
/// The space inside a card.
pub const CARD_PADDING: Pixels = px(20.);
/// A grid's inset from the card's edges: its cells add their own padding,
/// and its square corners stay inside the card's rounded ones.
pub const GRID_INSET: Pixels = px(10.);
/// A card's title line.
pub const HEADER_HEIGHT: Pixels = px(52.);

/// The words of the SIMULATED labels.
pub const SIMULATED: &str = "SIMULATED";

/// A card: the theme's group box, a hairline border, the large radius, a
/// soft shadow where the theme has shadows.
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
        .when(theme.shadow, |card| card.shadow_xs())
        .overflow_hidden()
}

/// A card's title line: the title, a muted detail after it, and `trailing`
/// at the right.
pub fn card_header(
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> Div {
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
                .text_size(px(16.))
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
                    .text_size(px(13.))
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
        })
        .child(div().flex_1())
        .children(trailing)
}

/// The frame of a grid in a card: the rest of the card's height, inset at
/// the sides and the bottom; relative, so an empty state can lie over it.
pub fn grid_frame() -> Div {
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .px(GRID_INSET)
        .pb(GRID_INSET)
}

/// What an empty grid says, over its body: a title and a muted hint,
/// centred. It takes no clicks: they reach the grid under it.
pub fn empty_state(title: SharedString, hint: SharedString, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    v_flex()
        .absolute()
        .top(px(40.))
        .left_0()
        .right_0()
        .bottom_0()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .child(
            div()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(title),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme.muted_foreground)
                .child(hint),
        )
}

/// A figure under a muted label, as the account and session cards show
/// them.
pub fn stat(
    label: &'static str,
    value: SharedString,
    color: Option<Hsla>,
    cx: &App,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    v_flex()
        .min_w_0()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .text_color(color.unwrap_or(theme.foreground))
                .whitespace_nowrap()
                .child(value),
        )
}

/// Tabular figures, for numbers outside the grids (the headline, the
/// cards, the ticket, the chart); the grids add them to their cells.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// `+1.23%` or `-0.40%`.
pub fn signed_pct(value: f64) -> String {
    format!("{value:+.2}%")
}

/// A volume: `12.4M`, `830.5K`, `950`.
pub fn compact(value: f64) -> String {
    let value = value.max(0.);
    if value >= 1e9 {
        format!("{:.2}B", value / 1e9)
    } else if value >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.1}K", value / 1e3)
    } else {
        format!("{value:.0}")
    }
}

/// An arrow for the direction of a change: `▲`, `▼`, or none.
pub fn arrow(change: f64) -> &'static str {
    if change > 0. {
        "▲"
    } else if change < 0. {
        "▼"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_figures() {
        assert_eq!(signed_pct(1.234), "+1.23%");
        assert_eq!(signed_pct(-0.4), "-0.40%");
        assert_eq!(compact(12_400_000.), "12.4M");
        assert_eq!(compact(830_500.), "830.5K");
        assert_eq!(compact(950.), "950");
        assert_eq!(compact(2_500_000_000.), "2.50B");
        assert_eq!(arrow(-1.), "▼");
        assert_eq!(arrow(0.), "");
    }
}
