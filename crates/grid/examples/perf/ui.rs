//! What the monitor's panels share: the spacing, the flat panel, its
//! header, the frame of a grid inside it and the change paint, as in the
//! `ops` example.

use std::sync::Arc;

use gpui_kit::base::h_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Div, FontFeatures, FontWeight, Hsla, ParentElement as _, Pixels, SharedString,
    Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{CellChange, CellStyle, ChangePaint, ChangePainter, ChangePalette, RowChange};

pub const PAGE_PADDING: Pixels = px(10.);
pub const GAP: Pixels = px(8.);
pub const PANEL_PADDING: Pixels = px(12.);
pub const HEADER_HEIGHT: Pixels = px(38.);
/// The grid's rows: dense, a monitor.
pub const ROW_HEIGHT: Pixels = px(26.);

/// A panel: the theme's group box, a hairline border.
pub fn panel(cx: &App) -> Div {
    let theme = cx.theme();
    gpui_kit::base::v_flex()
        .min_w_0()
        .min_h_0()
        .bg(theme.group_box)
        .text_color(theme.group_box_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .overflow_hidden()
}

/// A panel's header: an accent tick, the title in capitals, a muted
/// detail. Controls go after it with `.child`.
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
                .text_size(px(15.))
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
                    .child(detail),
            )
        })
}

/// The frame of a grid in a panel: the rest of the panel, in the grids'
/// font.
pub fn grid_frame(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex_1()
        .min_h_0()
        .font_family(theme.mono_font_family.clone())
        .text_size(theme.mono_font_size)
}

/// Tabular figures, for numbers outside the grid.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

const INSERTED_TINT: f32 = 0.34;
const UPDATED_TINT: f32 = 0.18;

/// Paints changes in the accent, never in the heat colour: a new process
/// and a notable change are tinted, without arrows; an exited process's
/// text fades out, muted.
#[derive(Clone, Copy, Debug)]
pub struct AccentChanges {
    accent: Hsla,
}

impl ChangePainter for AccentChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        let tint = match change.change() {
            RowChange::Removed => {
                style.set_foreground(change.fade(palette.removed_foreground()));
                return;
            }
            RowChange::Inserted => INSERTED_TINT,
            _ => UPDATED_TINT,
        };
        style.set_background(self.accent.opacity(tint * change.intensity()));
        style.set_trend(None);
    }
}

pub fn accent_changes(cx: &App) -> ChangePaint {
    ChangePaint::Custom(Arc::new(AccentChanges {
        accent: cx.theme().primary,
    }))
}
