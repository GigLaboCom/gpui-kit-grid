//! What the panels share: the spacing, the flat panel each region sits in,
//! its header, and figures outside the grid.

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
/// The table's rows.
pub const ROW_HEIGHT: Pixels = px(26.);

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

/// A panel's header: an accent tick, the title and a muted detail after
/// it. Controls go after it with `.child`.
pub fn panel_header(title: SharedString, detail: Option<SharedString>, cx: &App) -> Div {
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

/// Text in the theme's monospace font with tabular figures, on one line.
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

/// Tabular figures, for numbers that change in place.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// `12,345`.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `+1,204` or `−87` (a minus sign), bytes.
pub fn signed_bytes(delta: i64) -> String {
    let sign = if delta < 0 { "−" } else { "+" };
    format!("{sign}{}", thousands(delta.unsigned_abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_numbers_read_with_separators_and_signs() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(signed_bytes(1_204), "+1,204");
        assert_eq!(signed_bytes(-87), "−87");
        assert_eq!(signed_bytes(0), "+0");
    }
}
