//! Alert updates per second by severity over the last two minutes, a
//! gpui-kit area chart: one series per alert severity in its status
//! colour, always in the same order, with a legend that names each one
//! with its icon and its last second.

use std::time::Duration;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, chart::AreaChart};
use gpui_kit::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _, px,
};

use super::{
    rows::Severity,
    sim::{HISTORY_SECONDS, RateFeed, RateHistory, clock_text},
    theme::status_color,
    ui,
};

/// How often the chart looks for a new second.
const POLL: Duration = Duration::from_millis(500);
/// Labels on each axis.
const Y_TICKS: usize = 4;
const X_TICKS: usize = 5;
/// How strongly an area shows its colour.
const FILL_OPACITY: f32 = 0.14;
/// The least top of the scale, so a quiet minute is not all noise.
const MIN_PEAK: f64 = 20.;

/// One second: its clock and its updates by severity, in `Severity::ALERTS`
/// order.
#[derive(Clone, Debug, PartialEq)]
struct RatePoint {
    time: SharedString,
    counts: [f64; 4],
}

pub struct AlertRateChart {
    feed: RateFeed,
    version: u64,
    points: Vec<RatePoint>,
    peak: f64,
    _poll: Task<()>,
}

impl AlertRateChart {
    pub fn new(feed: RateFeed, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |chart, cx| chart.pull(cx)).is_err() {
                    break;
                }
            }
        });
        let mut chart = Self {
            feed,
            version: u64::MAX,
            points: Vec::new(),
            peak: MIN_PEAK,
            _poll: poll,
        };
        chart.pull(cx);
        chart
    }

    fn pull(&mut self, cx: &mut Context<Self>) {
        if self.feed.version() == self.version {
            return;
        }
        let RateHistory { version, samples } = self.feed.snapshot();
        self.version = version;
        self.points = samples
            .iter()
            .map(|sample| RatePoint {
                time: clock_text(sample.second * 1_000).into(),
                counts: sample.counts.map(f64::from),
            })
            .collect();
        let top = self
            .points
            .iter()
            .flat_map(|point| point.counts)
            .fold(0., f64::max);
        self.peak = nice_peak(top);
        cx.notify();
    }

    /// Each severity's swatch, icon, name and last second.
    fn render_legend(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let last = self.points.last().map(|point| point.counts);
        h_flex()
            .flex_none()
            .gap(px(14.))
            .font_features(ui::tabular_figures())
            .children(Severity::ALERTS.map(|severity| {
                let color = status_color(severity, theme);
                let value = last.map_or(0., |counts| counts[severity.ix()]);
                h_flex()
                    .gap(px(6.))
                    .child(div().text_color(color).child(format!(
                        "{} {}",
                        severity.icon(),
                        severity.label()
                    )))
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(SharedString::from(format!("{value:.0}"))),
                    )
            }))
    }
}

/// The top of the scale above `top`: `Y_TICKS - 1` round steps of 1, 2,
/// 2.5 or 5 times a power of ten, at least `MIN_PEAK`.
fn nice_peak(top: f64) -> f64 {
    let steps = (Y_TICKS - 1) as f64;
    let raw = top.max(MIN_PEAK) / steps;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1., 2., 2.5, 5., 10.]
        .into_iter()
        .map(|factor| factor * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10. * magnitude);
    step * steps
}

impl Render for AlertRateChart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // Drawn warning first, so the critical line lies on top; the
        // colours follow the severities, not the drawing order.
        let series: Vec<(usize, gpui_kit::Hsla, &'static str)> = Severity::ALERTS
            .iter()
            .rev()
            .map(|severity| {
                (
                    severity.ix(),
                    status_color(*severity, theme),
                    severity.label(),
                )
            })
            .collect();
        let peak = self.peak;
        v_flex()
            .size_full()
            .gap(px(6.))
            .child(self.render_legend(cx))
            .child(div().flex_1().min_h_0().text_size(px(12.)).when(
                !self.points.is_empty(),
                |pane| {
                    let chart = AreaChart::new(self.points.iter().cloned())
                        .id("ops-alert-rates")
                        .x(|point: &RatePoint| point.time.clone())
                        .linear()
                        .appear(false)
                        .point_count(HISTORY_SECONDS)
                        .y_domain(0., peak)
                        .y_axis(true)
                        .y_tick_count(Y_TICKS)
                        .y_tick_format(|value| format!("{value:.0}"))
                        .x_tick_count(X_TICKS);
                    let chart = series.into_iter().fold(chart, |chart, (ix, color, name)| {
                        chart
                            .y(move |point: &RatePoint| point.counts[ix])
                            .stroke(color)
                            .fill(color.opacity(FILL_OPACITY))
                            .name(name)
                    });
                    pane.child(chart)
                },
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nice_peak_is_round_and_above() {
        for (top, peak) in [
            (0., 30.),
            (7., 30.),
            (548., 600.),
            (803., 1_500.),
            (1_350., 1_500.),
        ] {
            assert_eq!(nice_peak(top), peak, "{top}");
        }
    }
}
