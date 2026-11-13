//! The instrument open in the centre: its symbol and name, the large last
//! price and the day's change, a gpui-kit area chart of its simulated
//! session over a range the tabs pick, and a small card of session stats.
//!
//! The bars come from the trading examples' `ChartSource` (`chart.rs`; the
//! simulator's `ChartFeed`: 5-second bars of the current instrument over
//! the last two simulated hours, republished every 100 ms), so the last
//! point moves live. The ranges are cuts of that simulated session. The
//! line, its fill and the change take the colour of their direction
//! (`PriceColors`).

use std::{sync::Arc, time::Duration};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    chart::AreaChart,
    tab::{Tab, TabBar},
    tag::Tag,
};
use gpui_kit::{
    Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::GridStore;

use super::{
    colors::PriceColors,
    ui::{self, SIMULATED},
};
use crate::trading::{
    bus::InstrumentBus,
    chart::{ChartBars, ChartSource},
    market::BAR_SECONDS,
    rows::{self, Bar, Quote},
};

/// How often the view looks for new bars and a new quote.
const POLL: Duration = Duration::from_millis(200);
/// Points on the chart, about, whatever the range.
const POINTS: u64 = 60;
const PRICE_TICKS: usize = 4;
const TIME_TICKS: usize = 5;
/// How strongly the area under the line shows its colour, at the line.
const FILL_OPACITY: f32 = 0.22;
/// The headline's last price.
const PRICE_SIZE: f32 = 40.;

/// How much of the simulated session the chart shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChartRange {
    FifteenMinutes,
    ThirtyMinutes,
    #[default]
    OneHour,
    TwoHours,
}

impl ChartRange {
    pub const ALL: [Self; 4] = [
        Self::FifteenMinutes,
        Self::ThirtyMinutes,
        Self::OneHour,
        Self::TwoHours,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::FifteenMinutes => "15m",
            Self::ThirtyMinutes => "30m",
            Self::OneHour => "1h",
            Self::TwoHours => "2h",
        }
    }

    fn seconds(self) -> u64 {
        match self {
            Self::FifteenMinutes => 15 * 60,
            Self::ThirtyMinutes => 30 * 60,
            Self::OneHour => 60 * 60,
            Self::TwoHours => 2 * 60 * 60,
        }
    }

    /// Seconds between two points: a whole number of bars.
    fn step(self) -> u64 {
        (self.seconds() / POINTS).div_ceil(BAR_SECONDS) * BAR_SECONDS
    }

    fn ix(self) -> usize {
        self as usize
    }
}

/// One point: the last close of a step.
#[derive(Clone, Debug, PartialEq)]
struct Point {
    /// The step's start as UTC `hh:mm` (or `hh:mm:ss` for steps under a
    /// minute): the x value, unique on the chart.
    time: SharedString,
    close: f64,
}

/// What the chart and the stats card show of one instrument's bars.
struct Series {
    symbol: &'static str,
    /// The simulated previous close.
    reference: f64,
    points: Vec<Point>,
    low: f64,
    high: f64,
    session: Session,
}

/// The simulated session so far: every bar the feed keeps.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Session {
    open: f64,
    high: f64,
    low: f64,
    vwap: Option<f64>,
}

impl Session {
    fn new(bars: &[Bar]) -> Option<Self> {
        let first = bars.first()?;
        let (low, high) = bars
            .iter()
            .fold((first.low, first.high), |(low, high), bar| {
                (low.min(bar.low), high.max(bar.high))
            });
        Some(Self {
            open: first.open,
            high,
            low,
            vwap: rows::vwap(bars),
        })
    }
}

impl Series {
    /// The points of `range` in `bars`; `None` without an instrument or
    /// bars.
    fn new(bars: &ChartBars, range: ChartRange) -> Option<Self> {
        let subject = bars.subject?;
        let session = Session::new(&bars.bars)?;
        let step = range.step();
        let end = bars.start + bars.bars.len() as u64 * BAR_SECONDS;
        let from = end.saturating_sub(range.seconds());
        let mut points: Vec<(u64, Point)> = Vec::with_capacity(POINTS as usize + 1);
        for (ix, bar) in bars.bars.iter().enumerate() {
            let time = bars.start + ix as u64 * BAR_SECONDS;
            if time < from {
                continue;
            }
            let bucket = time / step;
            match points.last_mut() {
                Some((last, point)) if *last == bucket => point.close = bar.close,
                _ => points.push((
                    bucket,
                    Point {
                        time: clock(bucket * step, step < 60).into(),
                        close: bar.close,
                    },
                )),
            }
        }
        let points: Vec<Point> = points.into_iter().map(|(_, point)| point).collect();
        // The previous close is in range, so its line shows.
        let reference = subject.reference;
        let (low, high) = points
            .iter()
            .fold((reference, reference), |(low, high), point| {
                (low.min(point.close), high.max(point.close))
            });
        (!points.is_empty()).then_some(Self {
            symbol: subject.symbol,
            reference,
            points,
            low,
            high,
            session,
        })
    }

    fn last(&self) -> f64 {
        self.points
            .last()
            .map_or(self.reference, |point| point.close)
    }
}

/// `seconds` (Unix) as UTC `hh:mm`, or `hh:mm:ss`.
fn clock(seconds: u64, with_seconds: bool) -> String {
    let day = seconds % 86_400;
    let (hours, minutes) = (day / 3_600, day / 60 % 60);
    if with_seconds {
        format!("{hours:02}:{minutes:02}:{:02}", day % 60)
    } else {
        format!("{hours:02}:{minutes:02}")
    }
}

/// The open instrument.
pub struct InstrumentView<S: ChartSource> {
    bus: Entity<InstrumentBus>,
    quotes: GridStore<Quote>,
    source: S,
    range: ChartRange,
    bars: ChartBars,
    series: Option<Series>,
    quote: Option<Arc<Quote>>,
    _poll: Task<()>,
    _subscriptions: [Subscription; 1],
}

impl<S: ChartSource> InstrumentView<S> {
    pub fn new(
        bus: Entity<InstrumentBus>,
        quotes: GridStore<Quote>,
        source: S,
        cx: &mut Context<Self>,
    ) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |view, cx| view.pull(cx)).is_err() {
                    break;
                }
            }
        });
        let subscriptions = [cx.observe(&bus, |view, _, cx| view.pull(cx))];
        Self {
            bus,
            quotes,
            source,
            range: ChartRange::default(),
            bars: ChartBars::default(),
            series: None,
            quote: None,
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_range(&mut self, range: ChartRange, cx: &mut Context<Self>) {
        if self.range == range {
            return;
        }
        self.range = range;
        self.series = Series::new(&self.bars, range);
        cx.notify();
    }

    /// Takes new bars and the current quote, if either changed.
    fn pull(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let bars = self.source.bars();
        if bars.version != self.bars.version {
            self.series = Series::new(&bars, self.range);
            self.bars = bars;
            changed = true;
        }
        let quote = self
            .bus
            .read(cx)
            .current()
            .and_then(|id| self.quotes.snapshot().get(&id).cloned());
        let same = match (&quote, &self.quote) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.quote = quote;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// The series, if it is of the open instrument: the feed follows the
    /// bus a moment later.
    fn series(&self) -> Option<&Series> {
        let quote = self.quote.as_ref()?;
        self.series
            .as_ref()
            .filter(|series| series.symbol == quote.ticker)
    }

    /// Symbol, name and the SIMULATED tag; the large last price and the
    /// change; the range tabs.
    fn render_headline(&self, cx: &mut Context<Self>) -> impl IntoElement + use<S> {
        let theme = cx.theme();
        let colors = PriceColors::current(cx);
        let quote = self.quote.clone();
        v_flex()
            .flex_none()
            .gap(px(6.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(quote.as_ref().map_or("—", |quote| quote.ticker)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(theme.muted_foreground)
                            .child(quote.as_ref().map_or("No instrument", |quote| quote.name)),
                    )
                    .child(Tag::secondary().small().rounded_full().child(SIMULATED)),
            )
            .child(
                h_flex()
                    .items_end()
                    .gap(px(14.))
                    .when_some(quote, |row, quote| {
                        let change = quote.change();
                        let color = colors.of(change, theme);
                        row.child(
                            div()
                                .flex_none()
                                .text_size(px(PRICE_SIZE))
                                .line_height(px(PRICE_SIZE + 4.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.foreground)
                                .child(SharedString::from(format!("{:.2}", quote.last))),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .pb(px(6.))
                                .gap(px(8.))
                                .text_size(px(16.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(color)
                                .child(SharedString::from(format!(
                                    "{} {change:+.2}",
                                    ui::arrow(change)
                                )))
                                .child(
                                    div()
                                        .px(px(8.))
                                        .py(px(2.))
                                        .rounded_full()
                                        .bg(color.opacity(0.12))
                                        .child(SharedString::from(ui::signed_pct(
                                            quote.change_pct(),
                                        ))),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .pb(px(8.))
                                .text_size(px(13.))
                                .text_color(theme.muted_foreground)
                                .child("today"),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        TabBar::new("chart-range")
                            .segmented()
                            .small()
                            .selected_index(self.range.ix())
                            .children(ChartRange::ALL.map(|range| Tab::new().label(range.label())))
                            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                                if let Some(&range) = ChartRange::ALL.get(*ix) {
                                    view.set_range(range, cx);
                                }
                            })),
                    ),
            )
    }

    fn render_chart(&self, cx: &mut Context<Self>) -> impl IntoElement + use<S> {
        let theme = cx.theme();
        let colors = PriceColors::current(cx);
        let series = self.series();
        div()
            .flex_1()
            .min_h_0()
            .when_some(series, |pane, series| {
                // The line's colour is the day's direction.
                let color = if series.last() >= series.reference {
                    colors.rise(theme)
                } else {
                    colors.fall(theme)
                };
                pane.child(
                    AreaChart::new(series.points.iter().cloned())
                        .id("broker-chart")
                        .x(|point: &Point| point.time.clone())
                        .y(|point| point.close)
                        .stroke(color)
                        .fill(linear_gradient(
                            180.,
                            linear_color_stop(color.opacity(FILL_OPACITY), 0.),
                            linear_color_stop(color.opacity(0.), 1.),
                        ))
                        .linear()
                        .y_domain(series.low, series.high)
                        .y_axis(true)
                        .y_tick_count(PRICE_TICKS)
                        .y_tick_format(|price| format!("{price:.2}"))
                        .x_tick_count(TIME_TICKS)
                        .reference_line(series.reference)
                        .appear_key((series.symbol, self.range.ix())),
                )
            })
            .when_none(&series, |pane| {
                pane.flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child("Loading the simulated chart…")
            })
    }

    /// The session's open, high, low, the previous close, the volume and
    /// the VWAP, in a soft inner card.
    fn render_stats(&self, cx: &mut Context<Self>) -> impl IntoElement + use<S> {
        let theme = cx.theme();
        let session = self.series().map(|series| series.session);
        let price = |value: Option<f64>| -> SharedString {
            value.map_or("—".into(), |value| format!("{value:.2}").into())
        };
        let stats: [(&'static str, SharedString); 6] = [
            ("Open", price(session.map(|session| session.open))),
            ("High", price(session.map(|session| session.high))),
            ("Low", price(session.map(|session| session.low))),
            (
                "Prev close",
                price(self.quote.as_ref().map(|quote| quote.close)),
            ),
            (
                "Volume",
                self.quote.as_ref().map_or("—".into(), |quote| {
                    ui::compact(quote.volume as f64).into()
                }),
            ),
            ("VWAP", price(session.and_then(|session| session.vwap))),
        ];
        h_flex()
            .flex_none()
            .w_full()
            .px(px(16.))
            .py(px(12.))
            .gap(px(12.))
            .rounded(theme.radius_lg)
            .bg(theme.muted)
            .children(
                stats
                    .into_iter()
                    .map(|(label, value)| div().flex_1().child(ui::stat(label, value, None, cx))),
            )
    }
}

impl<S: ChartSource> Render for InstrumentView<S> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let caption = format!("{SIMULATED} session · previous close dashed · UTC");
        v_flex()
            .size_full()
            .p(ui::CARD_PADDING)
            .gap(px(12.))
            .font_features(ui::tabular_figures())
            .child(self.render_headline(cx))
            .child(self.render_chart(cx))
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(caption)),
            )
            .child(self.render_stats(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::chart::ChartSubject;

    fn bars(count: usize) -> ChartBars {
        let id = crate::trading::instruments::find("KO").expect("KO");
        let bars: Vec<Bar> = (0..count)
            .map(|ix| {
                let mut bar = Bar::new(70. + ix as f64 * 0.01);
                bar.trade(70.5 + ix as f64 * 0.01, 10.);
                bar
            })
            .collect();
        ChartBars {
            subject: Some(ChartSubject::listed(id)),
            bars: Arc::from(bars),
            // 10:00:00 UTC on a whole hour.
            start: 36_000,
            version: 1,
        }
    }

    #[test]
    fn test_steps_are_whole_bars_and_ranges_keep_their_points() {
        for range in ChartRange::ALL {
            assert_eq!(range.step() % BAR_SECONDS, 0, "{range:?}");
            let series = Series::new(&bars(1_440), range).expect("a series");
            let expected = range.seconds().div_ceil(range.step()) as usize;
            assert!(
                series.points.len().abs_diff(expected) <= 1,
                "{range:?}: {} points",
                series.points.len()
            );
            assert!(series.low <= series.reference && series.reference <= series.high);
        }
    }

    #[test]
    fn test_the_session_spans_every_bar() {
        let series = Series::new(&bars(10), ChartRange::FifteenMinutes).expect("a series");
        assert_eq!(series.session.open, 70.);
        assert!((series.session.high - 70.59).abs() < 1e-9);
        assert_eq!(series.session.low, 70.);
        assert!(series.session.vwap.is_some());
        assert_eq!(series.points[0].time.as_ref(), "10:00:00");
        assert!(Series::new(&bars(0), ChartRange::OneHour).is_none());
    }
}
