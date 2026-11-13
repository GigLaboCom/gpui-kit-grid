//! The current instrument's chart: its simulated closes over the last two
//! hours, a minute apart, as a gpui-kit area chart in the accent, with the
//! simulated previous close as a reference line. The bars come from the
//! simulator (`ChartFeed`: 5-second bars of the current instrument,
//! republished every 100 ms), so the last point moves live.

use std::time::Duration;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, chart::AreaChart};
use gpui_kit::{
    Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Task,
    Window, div, linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px,
};

use crate::{
    trading::{
        instruments::InstrumentId,
        market::{BAR_SECONDS, ChartFeed, ChartSnapshot},
    },
    ui::{self, SIMULATED},
};

/// How often the chart looks for new bars; the simulator publishes every
/// 100 ms.
const POLL: Duration = Duration::from_millis(250);
/// Seconds between two points.
const STEP: u64 = 60;
/// Price labels on the axis.
const PRICE_TICKS: usize = 4;
/// Time labels under the chart.
const TIME_TICKS: usize = 5;
/// How strongly the area under the line shows the accent, at the line.
const FILL_OPACITY: f32 = 0.28;

/// One point: a minute's last close.
#[derive(Clone, Debug, PartialEq)]
struct Point {
    /// The minute as UTC `hh:mm`: the x value, unique on the chart.
    time: SharedString,
    close: f64,
}

/// The points of one instrument and what the chart and its header show.
struct Series {
    instrument: InstrumentId,
    points: Vec<Point>,
    low: f64,
    high: f64,
}

impl Series {
    /// The minutes of `snapshot`'s bars, each with its last close; `None`
    /// without an instrument or bars.
    fn new(snapshot: &ChartSnapshot) -> Option<Self> {
        let instrument = snapshot.instrument?;
        let mut points: Vec<(u64, Point)> = Vec::with_capacity(snapshot.bars.len() / 12 + 1);
        for (ix, bar) in snapshot.bars.iter().enumerate() {
            let minute = (snapshot.start + ix as u64 * BAR_SECONDS) / STEP;
            match points.last_mut() {
                Some((last, point)) if *last == minute => point.close = bar.close,
                _ => {
                    let seconds = minute * STEP % 86_400;
                    let time = format!("{:02}:{:02}", seconds / 3_600, seconds / 60 % 60);
                    points.push((
                        minute,
                        Point {
                            time: time.into(),
                            close: bar.close,
                        },
                    ));
                }
            }
        }
        let points: Vec<Point> = points.into_iter().map(|(_, point)| point).collect();
        // The previous close is in range, so its line shows.
        let close = instrument.get().seed;
        let (low, high) = points.iter().fold((close, close), |(low, high), point| {
            (low.min(point.close), high.max(point.close))
        });
        (!points.is_empty()).then_some(Self {
            instrument,
            points,
            low,
            high,
        })
    }

    fn last(&self) -> f64 {
        self.points.last().map_or(0., |point| point.close)
    }
}

/// The chart of the current instrument, as the simulator publishes it.
pub struct InstrumentChart {
    feed: ChartFeed,
    /// The feed's version `series` was built from.
    version: u64,
    series: Option<Series>,
    _poll: Task<()>,
}

impl InstrumentChart {
    pub fn new(feed: ChartFeed, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |chart, cx| chart.pull(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            feed,
            version: 0,
            series: None,
            _poll: poll,
        }
    }

    /// Rebuilds the series if the simulator published new bars.
    fn pull(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.feed.snapshot();
        if snapshot.version == self.version {
            return;
        }
        self.version = snapshot.version;
        self.series = Series::new(&snapshot);
        cx.notify();
    }

    /// Ticker, name, last, and the change against the simulated previous
    /// close by direction.
    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let quote = self.series.as_ref().map(|series| {
            let instrument = series.instrument.get();
            let last = series.last();
            let change = last - instrument.seed;
            let color = if change >= 0. {
                theme.chart_bullish
            } else {
                theme.chart_bearish
            };
            (instrument, last, change, color)
        });
        h_flex()
            .flex_none()
            .w_full()
            .gap(px(10.))
            .when_some(quote, |row, (instrument, last, change, color)| {
                row.child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .child(instrument.ticker),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(theme.muted_foreground)
                        .child(instrument.name),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.foreground)
                        .child(SharedString::from(format!("{last:.2}"))),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(color)
                        .child(SharedString::from(format!(
                            "{change:+.2} ({:+.2}%)",
                            change / instrument.seed * 100.
                        ))),
                )
            })
            .when_none(&quote, |row| {
                row.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("No instrument"),
                )
            })
    }
}

impl Render for InstrumentChart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let accent = theme.primary;
        let caption =
            format!("{SIMULATED} · 1-minute closes, last 2 hours · previous close dashed");
        v_flex()
            .size_full()
            .gap(px(8.))
            .font_features(ui::tabular_figures())
            .child(self.render_header(cx))
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(caption)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(self.series.as_ref(), |pane, series| {
                        let close = series.instrument.get().seed;
                        pane.child(
                            AreaChart::new(series.points.iter().cloned())
                                .id("wire-chart")
                                .x(|point: &Point| point.time.clone())
                                .y(|point| point.close)
                                .stroke(accent)
                                .fill(linear_gradient(
                                    180.,
                                    linear_color_stop(accent.opacity(FILL_OPACITY), 0.),
                                    linear_color_stop(accent.opacity(0.), 1.),
                                ))
                                .linear()
                                .y_domain(series.low, series.high)
                                .y_axis(true)
                                .y_tick_count(PRICE_TICKS)
                                .y_tick_format(|price| format!("{price:.2}"))
                                .x_tick_count(TIME_TICKS)
                                .reference_line(close)
                                .appear_key(series.instrument.0),
                        )
                    }),
            )
    }
}

/// The chart in a card, under its title.
pub fn card(
    chart: &gpui_kit::Entity<InstrumentChart>,
    cx: &gpui_kit::App,
) -> impl IntoElement + use<> {
    ui::card(cx)
        .size_full()
        .child(ui::card_header("Chart", None, cx))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .px(ui::CARD_PADDING)
                .pb(ui::CARD_PADDING)
                .child(chart.clone()),
        )
}
