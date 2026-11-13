//! What the console's panels share: the spacing, the flat panel each
//! region sits in, its header, the frame of a grid inside it, the severity
//! chips and the change paint.

use std::sync::Arc;

use gpui_kit::base::h_flex;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    tag::Tag,
};
use gpui_kit::{
    App, Div, ElementId, FontFeatures, FontWeight, Hsla, ParentElement as _, Pixels, SharedString,
    Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{CellChange, CellStyle, ChangePaint, ChangePainter, ChangePalette, RowChange};

use super::{rows::Severity, theme::status_color};

/// Around the main area and between panels: a wall of panels, tight.
pub const PAGE_PADDING: Pixels = px(10.);
pub const GAP: Pixels = px(8.);
/// The leading space of a panel's header.
pub const PANEL_PADDING: Pixels = px(12.);
/// A panel's header: its title line.
pub const HEADER_HEIGHT: Pixels = px(38.);
/// The grids' rows: dense, a console.
pub const ROW_HEIGHT: Pixels = px(26.);
/// Below this window width the console keeps to its alerts, a phone's
/// screen among them: the side column (chart, service health, fleet) and
/// the status bar's counters and keys go, and the alert panel's controls
/// wrap under its title.
pub const NARROW: Pixels = px(1180.);

/// The words of `SIMULATED` labels and badges.
pub const SIMULATED: &str = "SIMULATED";

/// A panel: the theme's group box, a hairline border, square corners.
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

/// A panel's header: an accent tick, the title in capitals, and a muted
/// detail after it. Controls go after it with `.child`.
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

/// How strongly a selected chip shows its colour.
const CHIP_TINT: f32 = 0.2;

/// A severity filter chip: its icon, label and active count, tinted in its
/// status colour when shown, outlined and muted when hidden; a toggle to
/// assistive technology. A `compact` chip shows the icon and the count
/// alone, its label in its tooltip and to assistive technology.
pub fn severity_chip(
    severity: Severity,
    count: u32,
    shown: bool,
    compact: bool,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    let color = status_color(severity, theme);
    let label = format!("{} {}  {count}", severity.icon(), severity.label());
    Button::new(ElementId::Name(severity.label().into()))
        .map(|chip| {
            if compact {
                chip.label(format!("{} {count}", severity.icon()))
                    .accessibility_label(format!("{} {count}", severity.label()))
                    .tooltip(severity.label())
            } else {
                chip.label(label)
            }
        })
        .small()
        .toggled(shown)
        .map(|chip| {
            if shown {
                chip.custom(
                    ButtonCustomVariant::new(cx)
                        .color(color.opacity(CHIP_TINT))
                        .foreground(color)
                        .hover(color.opacity(CHIP_TINT * 1.5))
                        .active(color.opacity(CHIP_TINT * 2.)),
                )
            } else {
                chip.outline()
            }
        })
}

/// How strongly a severity pill shows its border.
const PILL_BORDER: f32 = 0.45;

/// A severity in a grid's cell: a pill with its shape and label in its
/// status colour on a tint of it, so it never rests on colour alone; at
/// `opacity`, such as a handled alert's.
pub fn severity_pill(severity: Severity, opacity: f32, size: Size, cx: &App) -> Tag {
    let color = status_color(severity, cx.theme()).opacity(opacity);
    Tag::custom(color.opacity(CHIP_TINT), color, color.opacity(PILL_BORDER))
        .with_size(size)
        .rounded_full()
        .child(severity.pill())
}

/// Iosevka's and Barlow's tabular figures, for numbers outside the grids
/// (the clock, the status bar); the grids add them to their cells.
pub fn tabular_figures() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// `5000` as `5,000`.
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

/// The opacity of a change's tint at full intensity: a new row, an
/// updated cell.
const INSERTED_TINT: f32 = 0.34;
const UPDATED_TINT: f32 = 0.18;

/// Paints changes in the accent, never in a status colour: those mean
/// status here. A new row and an updated cell are tinted, without arrows;
/// a removed row's text fades out, muted, with no background, so a
/// resolved alert leaves quietly.
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

/// [`AccentChanges`] in the theme's accent, as a grid's change paint.
pub fn accent_changes(cx: &App) -> ChangePaint {
    ChangePaint::Custom(Arc::new(AccentChanges {
        accent: cx.theme().primary,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
