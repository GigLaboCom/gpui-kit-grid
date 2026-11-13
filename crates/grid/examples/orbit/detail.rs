//! The detail panel: the selected object now, its orbit's mean elements
//! and its next pass (rise, highest point, set).

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Div, FontWeight, ParentElement as _, SharedString, Styled as _, div, px};

use super::{
    producer::Detail,
    sky::{PassPoint, PassSearch, format_span, format_time, format_utc, thousands},
    ui,
};

/// A label's column in the detail's two-column lists.
const LABEL_WIDTH: f32 = 88.;
const LINE_HEIGHT: f32 = 18.;

/// One label and its value, the value in the grid's figures.
fn line(label: &'static str, value: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .h(px(LINE_HEIGHT))
        .gap(px(8.))
        .child(
            div()
                .flex_none()
                .w(px(LABEL_WIDTH))
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(ui::figures(cx).min_w_0().child(value.into()))
}

/// A section's title.
fn heading(title: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .pt(px(8.))
        .pb(px(2.))
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(cx.theme().muted_foreground)
        .child(title.into())
}

/// `51.48° N`, `0.12° W`.
fn degrees(value: f64, positive: char, negative: char) -> String {
    let hemisphere = if value >= 0. { positive } else { negative };
    format!("{:.2}° {hemisphere}", value.abs())
}

/// `in 4 min`, `2 min ago`.
fn relative(unix_ms: f64, now_ms: f64) -> String {
    if unix_ms >= now_ms {
        format!("in {}", format_span(unix_ms - now_ms))
    } else {
        format!("{} ago", format_span(now_ms - unix_ms))
    }
}

/// One moment of the pass: its name, the time, how far from now and
/// where.
fn moment(title: &'static str, point: &PassPoint, now_ms: f64, high: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let place = if high {
        format!("{:.0}° high · az {:.0}°", point.elevation, point.azimuth)
    } else {
        format!("az {:.0}°", point.azimuth)
    };
    v_flex()
        .flex_1()
        .min_w_0()
        .child(div().text_color(theme.muted_foreground).child(title))
        .child(
            ui::figures(cx)
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(format_time(point.unix_ms)),
        )
        .child(ui::figures(cx).child(relative(point.unix_ms, now_ms)))
        .child(ui::figures(cx).child(place))
}

/// The detail of `detail` at simulated time `now_ms`, or how to choose an
/// object.
pub fn render_detail(detail: Option<&Detail>, now_ms: f64, cx: &App) -> Div {
    let theme = cx.theme();
    let Some(detail) = detail else {
        return v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_color(theme.muted_foreground)
            .child("Select an object in the table");
    };
    let now = match &detail.position {
        Some(position) => {
            let look = position.look;
            let motion = if look.range_rate < 0. {
                "closing"
            } else {
                "away"
            };
            v_flex()
                .child(line("Azimuth", format!("{:.2}°", look.azimuth), cx))
                .child(line(
                    "Elevation",
                    format!(
                        "{:.2}° {}",
                        look.elevation,
                        if look.is_up() { "up" } else { "down" }
                    ),
                    cx,
                ))
                .child(line(
                    "Range",
                    format!("{} km", thousands(look.range.round() as u64)),
                    cx,
                ))
                .child(line(
                    "Range rate",
                    format!("{:+.2} km/s {motion}", look.range_rate),
                    cx,
                ))
                .child(line("Altitude", format!("{:.0} km", position.altitude), cx))
                .child(line("Speed", format!("{:.2} km/s", position.speed), cx))
                .child(line(
                    "Light",
                    if position.sunlit {
                        "sunlit"
                    } else {
                        "in shadow"
                    },
                    cx,
                ))
                .child(line(
                    "Ground",
                    format!(
                        "{} {}",
                        degrees(position.latitude, 'N', 'S'),
                        degrees(position.longitude, 'E', 'W')
                    ),
                    cx,
                ))
        }
        None => v_flex().child(line("Position", "not computable now", cx)),
    };
    let elements = &detail.elements;
    let orbit = v_flex()
        .child(line(
            "Epoch",
            format!("{} UTC", &format_utc(elements.epoch_ms)[5..16]),
            cx,
        ))
        .child(line("Perigee", format!("{:.0} km", elements.perigee), cx))
        .child(line("Apogee", format!("{:.0} km", elements.apogee), cx))
        .child(line("Period", format!("{:.1} min", elements.period), cx))
        .child(line(
            "Inclination",
            format!("{:.3}°", elements.inclination),
            cx,
        ))
        .child(line(
            "Eccentricity",
            format!("{:.5}", elements.eccentricity),
            cx,
        ))
        .child(line(
            "Motion",
            format!("{:.4} rev/d", elements.mean_motion),
            cx,
        ))
        .child(line("Drag B*", format!("{:.3e}", elements.drag_term), cx));
    let pass = match &detail.pass {
        PassSearch::Pass(pass) => {
            let title = match detail.pass_skip {
                _ if pass.rise.unix_ms <= now_ms => "Pass in progress".to_string(),
                0 => "Next pass".to_string(),
                skip => format!("Pass {} from now", skip + 1),
            };
            v_flex()
                .child(heading(
                    format!(
                        "{title} · {}",
                        format_span(pass.set.unix_ms - pass.rise.unix_ms)
                    ),
                    cx,
                ))
                .child(
                    h_flex()
                        .items_start()
                        .gap(px(16.))
                        .child(moment("Rise", &pass.rise, now_ms, false, cx))
                        .child(moment("Highest", &pass.culmination, now_ms, true, cx))
                        .child(moment("Set", &pass.set, now_ms, false, cx)),
                )
        }
        PassSearch::AlwaysUp => v_flex()
            .child(heading("Next pass", cx))
            .child(div().child("Always above the horizon")),
        PassSearch::NoPass => v_flex()
            .child(heading("Next pass", cx))
            .child(div().child("None in the next two days")),
    };

    let mut subtitle = format!("NORAD {}", detail.norad);
    if !detail.designator.is_empty() {
        subtitle.push_str(" · ");
        subtitle.push_str(&detail.designator);
    }
    subtitle.push_str(" · ");
    subtitle.push_str(detail.constellation.label());
    subtitle.push_str(" · ");
    subtitle.push_str(&detail.shell);
    v_flex()
        .size_full()
        .px(ui::PANEL_PADDING)
        .py(px(8.))
        .child(
            div()
                .text_size(px(17.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(detail.name.to_string()),
        )
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(theme.muted_foreground)
                .child(subtitle),
        )
        .child(
            h_flex()
                .items_start()
                .gap(px(16.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(heading("Now", cx))
                        .child(now),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(heading("Orbit", cx))
                        .child(orbit),
                ),
        )
        .child(pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_degrees_name_the_hemisphere() {
        assert_eq!(degrees(51.4769, 'N', 'S'), "51.48° N");
        assert_eq!(degrees(-0.0005, 'E', 'W'), "0.00° W");
    }

    #[test]
    fn test_relative_times() {
        assert_eq!(relative(4. * 60_000., 0.), "in 4 min");
        assert_eq!(relative(0., 2. * 60_000.), "2 min ago");
    }
}
