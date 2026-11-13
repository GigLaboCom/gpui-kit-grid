//! The selected vessel's card: its name and identifiers, what it is and
//! what it is doing, its voyage as it reports it, and when it was last
//! heard. Values as published; nothing links a vessel to a person.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    px,
};

use super::{driver::VesselDetail, rows::Clock, theme::category_color, ui};

/// `47 s ago`, `12 min ago`, `2 h ago`.
pub fn ago(ms: f64) -> String {
    let seconds = (ms / 1e3).max(0.).round() as u64;
    match seconds {
        0..=89 => format!("{seconds} s ago"),
        90..=5_399 => format!("{} min ago", (seconds + 30) / 60),
        _ => format!("{} h ago", (seconds + 1_800) / 3_600),
    }
}

fn field(label: &'static str, value: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .gap(px(8.))
        .child(
            div()
                .flex_none()
                .w(px(68.))
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(ui::figures(cx).flex_1().min_w_0().child(value.into()))
}

/// The card of `detail` at the source's `clock_ms`, or a hint.
pub fn render_detail(detail: Option<&VesselDetail>, clock_ms: f64, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(detail) = detail else {
        return div()
            .p(ui::PANEL_PADDING)
            .text_color(theme.muted_foreground)
            .child("Select a vessel in the table or on the map.")
            .into_any_element();
    };
    let row = &detail.row;
    let dash = || "—".to_string();
    let speed = row
        .speed
        .map_or_else(dash, |speed| format!("{:.1} kn", speed));
    let course = row
        .course
        .map_or_else(dash, |course| format!("{course:.1}°"));
    let heading = row
        .heading
        .map_or_else(dash, |heading| format!("{heading}°"));
    let position = match (row.latitude, row.longitude) {
        (Some(lat), Some(lon)) => format!("{:.4}° N  {:.4}° E", lat, lon),
        _ => dash(),
    };
    let mut eta = String::new();
    row.eta.write(&mut eta);
    let metadata = detail.metadata.as_deref();
    let size = match metadata.map(|metadata| (metadata.length(), metadata.beam())) {
        Some((Some(length), Some(beam))) => format!("{length} × {beam} m"),
        Some((Some(length), None)) => format!("{length} m"),
        _ => dash(),
    };
    let draught = row
        .draught
        .map_or_else(dash, |draught| format!("{:.1} m", draught));
    let mut seen = String::new();
    Clock::write(detail.seen_ms, &mut seen);
    seen.push_str(&format!(" UTC · {}", ago(clock_ms - detail.seen_ms)));
    let identifiers = {
        let mut text = format!("MMSI {}", row.mmsi);
        if !row.call_sign.is_empty() {
            text.push_str(&format!(" · {}", row.call_sign));
        }
        if let Some(imo) = metadata.map(|metadata| metadata.imo).filter(|imo| *imo > 0) {
            text.push_str(&format!(" · IMO {imo}"));
        }
        text
    };
    let category = category_color(row.category, theme);
    v_flex()
        .px(ui::PANEL_PADDING)
        .py(px(10.))
        .gap(px(4.))
        .child(
            div()
                .text_size(px(19.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(row.title()),
        )
        .child(
            ui::figures(cx)
                .text_color(theme.muted_foreground)
                .child(identifiers),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .pb(px(2.))
                .child(div().size(px(8.)).rounded_full().bg(category))
                .child(div().text_color(category).child(row.category.label()))
                .child(div().text_color(theme.muted_foreground).child("·"))
                .child(
                    div()
                        .text_color(if row.status.is_alert() {
                            theme.danger
                        } else {
                            theme.foreground
                        })
                        .child(row.status.label()),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(16.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.))
                        .child(field("Speed", speed, cx))
                        .child(field("Course", course, cx))
                        .child(field("Heading", heading, cx))
                        .child(field("Size", size, cx)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.))
                        .child(field(
                            "To",
                            if row.destination.is_empty() {
                                dash()
                            } else {
                                row.destination.to_string()
                            },
                            cx,
                        ))
                        .child(field(
                            "ETA",
                            if eta.is_empty() { dash() } else { eta + " UTC" },
                            cx,
                        ))
                        .child(field("Draught", draught, cx))
                        .child(field("Heard", seen, cx)),
                ),
        )
        .child(field("Position", position, cx))
        .into_any_element()
}

/// The selected vessel in three lines, for a short window: name, type and
/// status; speed, course and voyage; identifiers and when it was heard.
pub fn render_strip(detail: Option<&VesselDetail>, clock_ms: f64, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(detail) = detail else {
        return div()
            .px(ui::PANEL_PADDING)
            .py(px(8.))
            .text_color(theme.muted_foreground)
            .child("Select a vessel in the table or on the map.")
            .into_any_element();
    };
    let row = &detail.row;
    let category = category_color(row.category, theme);
    let mut motion = row
        .speed
        .map_or_else(|| "—".to_string(), |speed| format!("{:.1} kn", speed));
    if let Some(course) = row.course {
        motion.push_str(&format!(" · {course:.0}°"));
    }
    if !row.destination.is_empty() {
        motion.push_str(&format!(" · to {}", row.destination));
    }
    if row.eta.is_known() {
        motion.push_str(" · ETA ");
        row.eta.write(&mut motion);
    }
    let mut heard = format!("MMSI {}", row.mmsi);
    if !row.call_sign.is_empty() {
        heard.push_str(&format!(" · {}", row.call_sign));
    }
    heard.push_str(" · heard ");
    Clock::write(detail.seen_ms, &mut heard);
    heard.push_str(&format!(" UTC, {}", ago(clock_ms - detail.seen_ms)));
    v_flex()
        .px(ui::PANEL_PADDING)
        .py(px(8.))
        .gap(px(3.))
        .child(
            h_flex()
                .gap(px(8.))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .child(row.title()),
                )
                .child(div().flex_none().size(px(7.)).rounded_full().bg(category))
                .child(
                    div()
                        .flex_none()
                        .text_color(category)
                        .child(row.category.label()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(row.status.label()),
                ),
        )
        .child(ui::figures(cx).text_color(theme.foreground).child(motion))
        .child(
            ui::figures(cx)
                .text_color(theme.muted_foreground)
                .child(heard),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ago_reads_naturally() {
        assert_eq!(ago(-5.), "0 s ago");
        assert_eq!(ago(47_000.), "47 s ago");
        assert_eq!(ago(90_000.), "2 min ago");
        assert_eq!(ago(12.4 * 60_000.), "12 min ago");
        assert_eq!(ago(2. * 3_600_000.), "2 h ago");
    }
}
