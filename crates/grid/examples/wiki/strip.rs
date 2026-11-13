//! The pulse: the last minute of edits as a seismograph, one lane per
//! project family, busiest on top. Each edit is a tick at its time, now at
//! the right edge: up by the bytes it added, down by those it removed, on
//! a log scale; a bot's tick is pale, a new page has a dot. Painted on a
//! canvas from the pump's last frame, a quad per edit (a few thousand at
//! most); the view moves the clock between frames, so the ticks drift
//! left smoothly.

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{
    App, Bounds, FontWeight, Hsla, IntoElement, ParentElement as _, Pixels, SharedString,
    Styled as _, canvas, div, fill, point, px, size,
};

use super::{
    edit::Family,
    pulse::{Lane, WINDOW_MS},
    pump::PulseFrame,
    ui::{figures, thousands},
};

/// One family's lane.
pub const LANE_HEIGHT: f32 = 34.;
const LABEL_WIDTH: f32 = 136.;
/// A tick's width.
const TICK: f32 = 2.;
const NEW_PAGE_DOT: f32 = 3.;
/// A change of this many bytes reaches the lane's edge.
const FULL_BYTES: f64 = 100_000.;
/// The shortest tick, as a share of the half lane: a one-byte edit shows.
const MIN_TICK: f64 = 0.08;
const BOT_OPACITY: f32 = 0.35;

/// A family's colour, from the theme's chart and base colours, the same in
/// the lanes, the legend and nowhere else.
pub fn family_color(family: Family, theme: &Theme) -> Hsla {
    match family {
        Family::Wikipedia => theme.chart_1,
        Family::Wikidata => theme.chart_2,
        Family::Commons => theme.chart_3,
        Family::Wiktionary => theme.chart_4,
        Family::Wikisource => theme.cyan,
        Family::Wikibooks => theme.magenta,
        Family::Wikiquote => theme.chart_5,
        Family::Wikinews => theme.blue,
        Family::Wikiversity => theme.yellow_light,
        Family::Wikivoyage => theme.green_light,
        Family::Wikispecies => theme.magenta_light,
        Family::Wikifunctions => theme.blue_light,
        Family::MediaWiki => theme.cyan_light,
        Family::Meta => theme.red_light,
        Family::Other => theme.muted_foreground,
    }
}

/// The share of a half lane a change of `delta` bytes takes.
fn tick_height(delta: i32) -> f64 {
    let scaled = (f64::from(delta.unsigned_abs()) + 1.).log10() / (FULL_BYTES + 1.).log10();
    scaled.clamp(MIN_TICK, 1.)
}

/// The lanes of `frame`, at most `max`, the clock at `now_ms` (the
/// source's, ms).
pub fn pulse_strip(frame: Arc<PulseFrame>, now_ms: f64, max: usize, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let lanes: Vec<Lane> = frame.lanes.iter().take(max).copied().collect();
    if lanes.is_empty() {
        return div()
            .h(px(LANE_HEIGHT * 2.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.muted_foreground)
            .child("Edits appear here as they happen")
            .into_any_element();
    }
    let colors: Vec<(Family, Hsla)> = lanes
        .iter()
        .map(|lane| (lane.family, family_color(lane.family, theme)))
        .collect();
    let line = theme.border;
    let new_page = theme.success;
    let height = px(LANE_HEIGHT * lanes.len() as f32);
    let labels = v_flex()
        .flex_none()
        .w(px(LABEL_WIDTH))
        .children(lanes.iter().zip(&colors).map(|(lane, (_, color))| {
            h_flex()
                .h(px(LANE_HEIGHT))
                .gap(px(8.))
                .pl(px(12.))
                .child(div().flex_none().w(px(3.)).h(px(20.)).bg(*color))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.foreground)
                                .child(lane.family.label()),
                        )
                        .child(
                            figures(cx)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(format!(
                                    "{}/min · {}",
                                    thousands(u64::from(lane.per_minute)),
                                    thousands(u64::from(lane.wikis)),
                                ))),
                        ),
                )
        }));
    let axis = h_flex()
        .pl(px(LABEL_WIDTH))
        .pr(px(8.))
        .justify_between()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child("60 s ago")
        .child("30 s")
        .child("now");
    v_flex()
        .w_full()
        .py(px(6.))
        .gap(px(2.))
        .child(
            h_flex().w_full().child(labels).child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        paint(bounds, &frame, &colors, line, new_page, now_ms, window)
                    },
                )
                .flex_1()
                .h(height)
                .mr(px(8.)),
            ),
        )
        .child(axis)
        .into_any_element()
}

fn paint(
    bounds: Bounds<Pixels>,
    frame: &PulseFrame,
    lanes: &[(Family, Hsla)],
    line: Hsla,
    new_page: Hsla,
    now_ms: f64,
    window: &mut gpui_kit::Window,
) {
    let width = f32::from(bounds.size.width);
    let (left, top) = (bounds.origin.x, bounds.origin.y);
    let mut lane_of = [None; Family::ALL.len()];
    for (ix, (family, _)) in lanes.iter().enumerate() {
        lane_of[family.ix()] = Some(ix);
    }
    let half = LANE_HEIGHT / 2. - 3.;
    for ix in 0..lanes.len() {
        let middle = top + px(LANE_HEIGHT * ix as f32 + LANE_HEIGHT / 2.);
        window.paint_quad(fill(
            Bounds::new(point(left, middle), size(bounds.size.width, px(1.))),
            line.opacity(0.7),
        ));
        if ix > 0 {
            window.paint_quad(fill(
                Bounds::new(
                    point(left, top + px(LANE_HEIGHT * ix as f32)),
                    size(bounds.size.width, px(1.)),
                ),
                line.opacity(0.35),
            ));
        }
    }
    for tick in &frame.ticks {
        let Some(lane) = lane_of[tick.family.ix()] else {
            continue;
        };
        let age = (now_ms - tick.at_ms as f64).max(0.);
        if age > WINDOW_MS as f64 {
            continue;
        }
        let x = width - TICK - (age / WINDOW_MS as f64) as f32 * (width - TICK);
        let height = (tick_height(tick.delta) * f64::from(half)) as f32;
        let middle = LANE_HEIGHT * lane as f32 + LANE_HEIGHT / 2.;
        let y = if tick.delta >= 0 {
            middle - height
        } else {
            middle + 1.
        };
        let color = lanes[lane].1;
        let color = if tick.bot {
            color.opacity(BOT_OPACITY)
        } else {
            color
        };
        window.paint_quad(fill(
            Bounds::new(point(left + px(x), top + px(y)), size(px(TICK), px(height))),
            color,
        ));
        if tick.new_page {
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        left + px(x + TICK / 2. - NEW_PAGE_DOT / 2.),
                        top + px(middle - height - NEW_PAGE_DOT - 1.),
                    ),
                    size(px(NEW_PAGE_DOT), px(NEW_PAGE_DOT)),
                ),
                new_page,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_heights_grow_on_a_log_scale_within_the_lane() {
        assert_eq!(tick_height(0), MIN_TICK);
        assert!(tick_height(10) < tick_height(1_000));
        assert_eq!(tick_height(-1_000), tick_height(1_000));
        assert_eq!(tick_height(i32::MAX), 1.);
        assert!((tick_height(100_000) - 1.).abs() < 1e-9);
    }
}
