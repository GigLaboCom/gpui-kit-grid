//! The chart panel of the trading examples: one instrument's candles over
//! its volume, the session VWAP and, if asked for, an EMA across the
//! candles, a price axis on the right and a time axis below. The bars come
//! from a `ChartSource` (the simulator's `ChartFeed` for the listed
//! instruments: 5-second bars, republished every 100 ms), so the last
//! candle moves live; the timeframe switches between 5-second, 1-minute and
//! 5-minute candles aggregated from them. Prices show the decimals of the
//! instrument's tick.
//!
//! The candles, the VWAP and EMA lines and the volume bars are gpui-kit
//! charts. Its candlestick chart has no value axis, no y domain and no
//! second series, so the price axis, the grid lines and the lines are laid
//! over it where its scales put the candles: across, the band centres of a
//! `ScaleBand` with the chart's paddings; up, from the lowest low at the
//! bottom to the highest high `TOP_GAP` below the top. A line that runs
//! beyond the candles' range (the session VWAP over 5-second candles) is
//! cut at the pane's edge.

use std::{sync::Arc, time::Duration};

use gpui_kit::base::{
    h_flex,
    plot::scale::{Scale as _, ScaleBand},
    v_flex,
};
use gpui_kit::component::{
    ActiveTheme as _,
    chart::{BarChart, CandlestickChart, LineChart},
    dock::{BasePanel, Panel, PanelEvent},
};
use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, Focusable, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Task, Window, div, prelude::FluentBuilder as _, px, relative,
};

use super::{
    instruments::{InstrumentId, tick_decimals},
    market::{BAR_SECONDS, ChartFeed},
    rows::Bar,
};

/// How often the panel looks for new bars; the simulator publishes every
/// 100 ms.
const POLL: Duration = Duration::from_millis(100);
/// The newest candles shown, at most.
const MAX_CANDLES: usize = 120;
/// The room the candlestick chart keeps above the highest high.
const TOP_GAP: Pixels = px(10.);
/// The band paddings of gpui-kit's candlestick chart (fixed there), given
/// to the volume bars too so the bars stand under the candles.
const PADDING_INNER: f32 = 0.4;
const PADDING_OUTER: f32 = 0.2;
/// A band width no chart reaches: candles and bars always fill their bands,
/// so the overlays can place them by shares of the width.
const UNCAPPED: Pixels = px(100_000.);
/// Price levels on the axis, about.
const PRICE_LEVELS: usize = 10;
/// A level's label gives way to the last price's within this share of the
/// price pane's height (a label's height, about).
const LAST_PRICE_CLEARANCE: f32 = 0.04;
const HEADER_HEIGHT: Pixels = px(30.);
const AXIS_WIDTH: Pixels = px(68.);
const VOLUME_HEIGHT: Pixels = px(96.);
const TIME_AXIS_HEIGHT: Pixels = px(20.);
const LABEL_HEIGHT: Pixels = px(16.);
const TIME_LABEL_WIDTH: Pixels = px(48.);
/// How strongly a volume bar shows its candle's colour.
const VOLUME_OPACITY: f32 = 0.5;
/// The price step of the listed instruments' simulator (`market.rs`).
const LISTED_TICK: f64 = 0.01;

/// The instrument a chart shows: what its header names, the price its
/// change is against and its price step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartSubject {
    pub symbol: &'static str,
    pub name: &'static str,
    /// The simulated previous close: the header's change is against it.
    pub reference: f64,
    /// The price step: the price axis's finest level, and the decimals of
    /// every price shown.
    pub tick: f64,
}

impl ChartSubject {
    /// A listed instrument of `instruments.rs`, against its seed price.
    pub fn listed(id: InstrumentId) -> Self {
        let instrument = id.get();
        Self {
            symbol: instrument.ticker,
            name: instrument.name,
            reference: instrument.seed,
            tick: LISTED_TICK,
        }
    }

    fn format(&self, price: f64) -> String {
        format!("{price:.*}", tick_decimals(self.tick))
    }
}

/// The bars of one instrument, oldest first, `BAR_SECONDS` apart; the last
/// bar is still open. `start` is the first bar's start in Unix seconds;
/// `version` grows with every publication.
#[derive(Clone)]
pub struct ChartBars {
    pub subject: Option<ChartSubject>,
    pub bars: Arc<[Bar]>,
    pub start: u64,
    pub version: u64,
}

impl Default for ChartBars {
    fn default() -> Self {
        Self {
            subject: None,
            bars: Arc::from(Vec::new()),
            start: 0,
            version: 0,
        }
    }
}

/// Where a chart's bars come from. Asked every `POLL` on the UI thread, so
/// it must be cheap: a lock and a few `Arc` clones.
pub trait ChartSource: 'static {
    /// The bars as last published.
    fn bars(&self) -> ChartBars;
}

/// The simulator's bars of the current listed instrument.
impl ChartSource for ChartFeed {
    fn bars(&self) -> ChartBars {
        let snapshot = self.snapshot();
        ChartBars {
            subject: snapshot.instrument.map(ChartSubject::listed),
            bars: snapshot.bars,
            start: snapshot.start,
            version: snapshot.version,
        }
    }
}

/// The chart's candle length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Timeframe {
    FiveSeconds,
    #[default]
    OneMinute,
    FiveMinutes,
}

impl Timeframe {
    pub const ALL: [Self; 3] = [Self::FiveSeconds, Self::OneMinute, Self::FiveMinutes];

    /// The short name `TF` takes.
    pub fn label(self) -> &'static str {
        match self {
            Self::FiveSeconds => "5s",
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
        }
    }

    /// `5s`, `1M`: the timeframe of a label, ignoring case.
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|timeframe| timeframe.label().eq_ignore_ascii_case(label))
    }

    /// The next longer timeframe, after the longest the shortest (F5).
    pub fn next(self) -> Self {
        match self {
            Self::FiveSeconds => Self::OneMinute,
            Self::OneMinute => Self::FiveMinutes,
            Self::FiveMinutes => Self::FiveSeconds,
        }
    }

    fn seconds(self) -> u64 {
        match self {
            Self::FiveSeconds => 5,
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
        }
    }

    /// The time axis names the candles that start on a multiple of this,
    /// in seconds.
    fn label_every(self) -> u64 {
        match self {
            Self::FiveSeconds => 60,
            Self::OneMinute => 900,
            Self::FiveMinutes => 1_800,
        }
    }
}

/// One candle, and the session VWAP and the EMA of the closes at its close.
#[derive(Clone, Debug, PartialEq)]
struct Candle {
    /// The start, in Unix seconds.
    start: u64,
    /// The start as UTC `hh:mm:ss`: the x value, unique on the chart.
    time: SharedString,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    vwap: f64,
    ema: f64,
}

impl Candle {
    fn rising(&self) -> bool {
        self.close > self.open
    }
}

/// The candles of `bars` (`BAR_SECONDS` apart from `start`, in Unix
/// seconds) at `timeframe`, the newest `MAX_CANDLES` of them. The VWAP
/// runs over every bar, from the first; the EMA of `ema_period` candles
/// over every candle's close, from the first.
fn candles(bars: &[Bar], start: u64, timeframe: Timeframe, ema_period: usize) -> Vec<Candle> {
    let span = timeframe.seconds();
    let mut candles: Vec<Candle> = Vec::new();
    let (mut turnover, mut traded) = (0., 0.);
    for (ix, bar) in bars.iter().enumerate() {
        let time = start + ix as u64 * BAR_SECONDS;
        let volume = bar.volume;
        // The same VWAP as `rows::vwap`, running.
        turnover += bar.typical() * volume;
        traded += volume;
        let vwap = if traded > 0. {
            turnover / traded
        } else {
            bar.close
        };
        match candles.last_mut() {
            Some(candle) if candle.start == time - time % span => {
                candle.high = candle.high.max(bar.high);
                candle.low = candle.low.min(bar.low);
                candle.close = bar.close;
                candle.volume += volume;
                candle.vwap = vwap;
            }
            _ => candles.push(Candle {
                start: time - time % span,
                time: SharedString::default(),
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume,
                vwap,
                ema: bar.close,
            }),
        }
    }
    // The EMA of the closes, seeded with the first one.
    let alpha = 2. / (ema_period.max(1) as f64 + 1.);
    let mut ema = None;
    for candle in &mut candles {
        let value = ema.map_or(candle.close, |ema: f64| ema + alpha * (candle.close - ema));
        candle.ema = value;
        ema = Some(value);
    }
    candles.drain(..candles.len().saturating_sub(MAX_CANDLES));
    for candle in &mut candles {
        candle.time = clock(candle.start).into();
    }
    candles
}

/// Unix seconds as UTC `hh:mm:ss`.
fn clock(seconds: u64) -> String {
    let seconds = seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The centre of each of `count` candles (and volume bars) as a share of
/// the plot's width: gpui-kit's band scale with the charts' paddings,
/// uncapped.
fn band_centers(count: usize) -> Vec<f32> {
    let scale = ScaleBand::new(0..count, [0., 1.])
        .padding_inner(PADDING_INNER)
        .padding_outer(PADDING_OUTER);
    let half = scale.band_width() / 2.;
    (0..count)
        .filter_map(|ix| scale.tick(&ix).map(|left| left + half))
        .collect()
}

/// Round prices from `low` to `high`, about `PRICE_LEVELS` of them, a
/// whole 1, 2 or 5 times a power of ten (at least a `tick`) apart.
fn price_levels(low: f64, high: f64, tick: f64) -> Vec<f64> {
    let span = high - low;
    if span <= 0. || !span.is_finite() {
        return Vec::new();
    }
    let raw = span / PRICE_LEVELS as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1., 2., 5., 10.]
        .into_iter()
        .map(|factor| factor * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10. * magnitude)
        .max(tick);
    let (first, last) = ((low / step).ceil() as i64, (high / step).floor() as i64);
    (first..=last).map(|level| level as f64 * step).collect()
}

/// `1234567` as `1.23M`, `12345` as `12.3K`.
fn compact(volume: f64) -> String {
    if volume >= 1e6 {
        format!("{:.2}M", volume / 1e6)
    } else if volume >= 1e3 {
        format!("{:.1}K", volume / 1e3)
    } else {
        format!("{volume:.0}")
    }
}

/// What the chart shows: one instrument's candles at one timeframe.
struct Series {
    subject: ChartSubject,
    candles: Vec<Candle>,
    /// Each candle's centre, as a share of the plot's width.
    centers: Vec<f32>,
    /// The lowest low and the highest high: the price axis's range. A
    /// line beyond it is cut at the pane's edge.
    low: f64,
    high: f64,
    max_volume: f64,
}

impl Series {
    fn new(bars: &ChartBars, timeframe: Timeframe, ema: Option<usize>) -> Option<Self> {
        let subject = bars.subject?;
        let candles = candles(&bars.bars, bars.start, timeframe, ema.unwrap_or(1));
        if candles.is_empty() {
            return None;
        }
        let low = candles.iter().map(|c| c.low).fold(f64::MAX, f64::min);
        let high = candles.iter().map(|c| c.high).fold(f64::MIN, f64::max);
        let max_volume = candles.iter().map(|c| c.volume).fold(0., f64::max);
        Some(Self {
            subject,
            centers: band_centers(candles.len()),
            candles,
            low,
            high,
            max_volume,
        })
    }

    fn last(&self) -> &Candle {
        self.candles.last().expect("a series has candles")
    }

    /// The share of the price pane's height above `price`: 0 at the
    /// highest high, 1 at the lowest low.
    fn depth(&self, price: f64) -> f32 {
        ((self.high - price) / (self.high - self.low)) as f32
    }

    /// The centres and names of the candles the time axis names.
    fn time_marks(&self, timeframe: Timeframe) -> impl Iterator<Item = (f32, SharedString)> + '_ {
        let every = timeframe.label_every();
        self.candles
            .iter()
            .zip(&self.centers)
            .filter(move |(candle, center)| {
                candle.start % every == 0 && (0.02..=0.98).contains(*center)
            })
            .map(|(candle, center)| (*center, SharedString::from(candle.time[..5].to_owned())))
    }
}

/// The chart of one instrument, refreshed from its source's bars; the
/// timeframe is the panel's.
pub struct ChartPanel {
    source: Box<dyn ChartSource>,
    timeframe: Timeframe,
    /// The EMA's period in candles, if one is drawn.
    ema: Option<usize>,
    /// The source's version `series` was built from.
    version: u64,
    series: Option<Series>,
    focus_handle: FocusHandle,
    _poll: Task<()>,
}

impl ChartPanel {
    /// A chart of the bars `source` publishes, polled every 100 ms.
    pub fn new(source: impl ChartSource, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |panel, cx| panel.pull(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            source: Box::new(source),
            timeframe: Timeframe::default(),
            ema: None,
            version: 0,
            series: None,
            focus_handle: cx.focus_handle(),
            _poll: poll,
        }
    }

    /// Also draws the EMA of the closes over `period` candles, in
    /// `base.magenta`.
    pub fn with_ema(mut self, period: usize) -> Self {
        self.ema = Some(period.max(1));
        self
    }

    pub fn timeframe(&self) -> Timeframe {
        self.timeframe
    }

    pub fn set_timeframe(&mut self, timeframe: Timeframe, cx: &mut Context<Self>) {
        if self.timeframe == timeframe {
            return;
        }
        self.timeframe = timeframe;
        let bars = self.source.bars();
        self.version = bars.version;
        self.series = Series::new(&bars, timeframe, self.ema);
        cx.notify();
    }

    /// Rebuilds the series if the source published new bars.
    fn pull(&mut self, cx: &mut Context<Self>) {
        let bars = self.source.bars();
        if bars.version == self.version {
            return;
        }
        self.version = bars.version;
        self.series = Series::new(&bars, self.timeframe, self.ema);
        cx.notify();
    }

    /// Symbol, name, last, change and %change by direction, the VWAP and the
    /// EMA in their lines' colours and the timeframes.
    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let ema = self.ema;
        let quote = self.series.as_ref().map(|series| {
            let subject = series.subject;
            let last = series.last();
            let change = last.close - subject.reference;
            let color = if change >= 0. {
                theme.chart_bullish
            } else {
                theme.chart_bearish
            };
            (subject, last.clone(), change, color)
        });
        h_flex()
            .flex_none()
            .w_full()
            .h(HEADER_HEIGHT)
            .px_2()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .when_some(quote.clone(), |row, (subject, last, change, color)| {
                let change_text = if change >= 0. {
                    format!("+{}", subject.format(change))
                } else {
                    subject.format(change)
                };
                row.child(div().text_color(theme.yellow).child(subject.symbol))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(theme.muted_foreground)
                            .child(subject.name),
                    )
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(SharedString::from(subject.format(last.close))),
                    )
                    .child(
                        div()
                            .text_color(color)
                            .child(SharedString::from(change_text)),
                    )
                    .child(div().text_color(color).child(SharedString::from(format!(
                        "{:+.2}%",
                        change / subject.reference * 100.
                    ))))
            })
            .child(div().flex_1())
            .when_some(quote, |row, (subject, last, _, _)| {
                row.child(
                    div()
                        .flex_none()
                        .text_color(theme.cyan)
                        .child(SharedString::from(format!(
                            "VWAP {}",
                            subject.format(last.vwap)
                        ))),
                )
                .when_some(ema, |row, period| {
                    row.child(div().flex_none().text_color(theme.magenta).child(
                        SharedString::from(format!("EMA {period} {}", subject.format(last.ema))),
                    ))
                })
            })
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .children(Timeframe::ALL.map(|timeframe| {
                        let active = timeframe == self.timeframe;
                        div()
                            .id(timeframe.label())
                            .px_1()
                            .map(|label| {
                                if active {
                                    label.bg(theme.primary).text_color(theme.primary_foreground)
                                } else {
                                    label.text_color(theme.muted_foreground)
                                }
                            })
                            .child(timeframe.label())
                            .on_click(cx.listener(move |panel, _, _, cx| {
                                panel.set_timeframe(timeframe, cx)
                            }))
                    })),
            )
    }

    fn render_chart(&self, series: &Series, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (bullish, bearish) = (theme.chart_bullish, theme.chart_bearish);
        let last = series.last();
        let (first, last_center) = (series.centers[0], series.centers[series.centers.len() - 1]);
        let subject = series.subject;
        let decimals = tick_decimals(subject.tick);
        // By the change on the day, as the header.
        let last_color = if last.close >= subject.reference {
            bullish
        } else {
            bearish
        };
        let key = (subject.symbol, self.timeframe);
        let levels = price_levels(series.low, series.high, subject.tick);
        // A line across the candles, from the first candle's centre to the
        // last one's, cut at the pane's edges beyond the candles' range.
        let line = |id: &'static str, value: fn(&Candle) -> f64, color: Hsla| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(first))
                .w(relative(last_center - first))
                .child(
                    LineChart::new(series.candles.clone())
                        .id(id)
                        .x(|c: &Candle| c.time.clone())
                        .y(value)
                        .y_domain(series.low, series.high)
                        .stroke(color)
                        .linear()
                        .x_axis(false)
                        .grid(false)
                        .interactive(false)
                        .appear_key(key),
                )
        };
        let marks: Vec<(f32, SharedString)> = series.time_marks(self.timeframe).collect();
        // A vertical grid line at each named time.
        let time_lines = |marks: &[(f32, SharedString)], color: Hsla| {
            div()
                .absolute()
                .inset_0()
                .children(marks.iter().map(move |(center, _)| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(*center))
                        .w(px(1.))
                        .bg(color)
                }))
        };
        // The price pane's levels: from the highest high, `TOP_GAP` down,
        // to the lowest low at the bottom.
        let price_range = || div().absolute().top(TOP_GAP).bottom_0().left_0().right_0();

        let price_pane = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .child(time_lines(&marks, theme.chart_grid))
            .child(price_range().children(levels.iter().map(|level| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(relative(series.depth(*level)))
                    .h(px(1.))
                    .bg(theme.chart_grid)
            })))
            .child(
                div().absolute().inset_0().child(
                    CandlestickChart::new(series.candles.clone())
                        .id("chart-candles")
                        .x(|c: &Candle| c.time.clone())
                        .open(|c| c.open)
                        .high(|c| c.high)
                        .low(|c| c.low)
                        .close(|c| c.close)
                        .bullish(bullish)
                        .bearish(bearish)
                        .x_axis(false)
                        .grid(false)
                        .max_band_width(UNCAPPED)
                        .appear_key(key)
                        .tooltip_value(move |_, _, price| format!("{price:.decimals$}").into()),
                ),
            )
            .when(series.candles.len() > 1, |pane| {
                pane.child(line("chart-vwap", |c| c.vwap, theme.cyan))
                    .when(self.ema.is_some(), |pane| {
                        pane.child(line("chart-ema", |c| c.ema, theme.magenta))
                    })
            })
            .child(
                price_range().child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(relative(series.depth(last.close)))
                        .h(px(1.))
                        .bg(last_color.opacity(0.6)),
                ),
            );

        let price_label = |price: f64| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(series.depth(price)))
                .mt(-LABEL_HEIGHT / 2.)
                .h(LABEL_HEIGHT)
                .px_1()
                .flex()
                .items_center()
                .child(SharedString::from(format!("{price:.decimals$}")))
        };
        let price_axis = div()
            .relative()
            .flex_none()
            .w(AXIS_WIDTH)
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                price_range()
                    .children(
                        levels
                            .iter()
                            .filter(|level| {
                                (series.depth(**level) - series.depth(last.close)).abs()
                                    > LAST_PRICE_CLEARANCE
                            })
                            .map(|level| price_label(*level)),
                    )
                    .child(
                        price_label(last.close)
                            .bg(last_color)
                            .text_color(theme.background),
                    ),
            );

        let volume_pane = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(time_lines(&marks, theme.chart_grid))
            .child(
                div().absolute().inset_0().child(
                    BarChart::new(series.candles.clone())
                        .id("chart-volume")
                        .band(|c: &Candle| c.time.clone())
                        .value(|c| c.volume)
                        .fill(move |c, _, _, _| {
                            (if c.rising() { bullish } else { bearish }).opacity(VOLUME_OPACITY)
                        })
                        .padding_inner(PADDING_INNER)
                        .padding_outer(PADDING_OUTER)
                        .max_band_width(UNCAPPED)
                        .label_axis(false)
                        .grid(false)
                        .appear_key(key)
                        .name("Volume")
                        .tooltip_title(|c| c.time.clone())
                        .tooltip_value(|_, volume| compact(volume).into()),
                ),
            );
        let volume_axis = v_flex()
            .flex_none()
            .w(AXIS_WIDTH)
            .h_full()
            .px_1()
            .border_l_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(SharedString::from(compact(series.max_volume)))
            .child("VOL");

        let time_axis = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .children(marks.into_iter().map(|(center, label)| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(center))
                    .ml(-TIME_LABEL_WIDTH / 2.)
                    .w(TIME_LABEL_WIDTH)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(label)
            }));

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(price_pane)
                    .child(price_axis),
            )
            .child(
                h_flex()
                    .flex_none()
                    .h(VOLUME_HEIGHT)
                    .w_full()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(volume_pane)
                    .child(volume_axis),
            )
            .child(
                h_flex()
                    .flex_none()
                    .h(TIME_AXIS_HEIGHT)
                    .w_full()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(time_axis)
                    .child(
                        div()
                            .flex_none()
                            .w(AXIS_WIDTH)
                            .h_full()
                            .px_1()
                            .border_l_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .child("UTC"),
                    ),
            )
    }
}

impl BasePanel for ChartPanel {
    fn panel_name(&self) -> &'static str {
        "trading.chart"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for ChartPanel {}

impl Panel for ChartPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Chart"
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for ChartPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChartPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let waiting = match self.source.bars().subject {
            Some(subject) => format!("Waiting for the simulated bars of {}", subject.symbol),
            None => "No instrument".into(),
        };
        v_flex()
            .id("chart")
            .track_focus(&self.focus_handle)
            .size_full()
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .child(self.render_header(cx))
            .map(|panel| match &self.series {
                Some(series) => panel.child(self.render_chart(series, cx)),
                None => panel.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(waiting)),
                ),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(open: f64, close: f64, volume: f64) -> Bar {
        let mut bar = Bar::new(open);
        bar.trade(close, volume);
        bar
    }

    #[test]
    fn test_candles_aggregate_on_whole_minutes() {
        // 13 five-second bars from 00:00:55: one bar of the first minute,
        // twelve of the next.
        let bars: Vec<Bar> = (0..13)
            .map(|ix| bar(100. + ix as f64, 101. + ix as f64, 10.))
            .collect();
        let candles = candles(&bars, 55, Timeframe::OneMinute, 20);
        assert_eq!(candles.len(), 2);
        assert_eq!((candles[0].start, &*candles[0].time), (0, "00:00:00"));
        assert_eq!((candles[1].start, &*candles[1].time), (60, "00:01:00"));
        assert_eq!(candles[1].open, 101.);
        assert_eq!(candles[1].close, 113.);
        assert_eq!(candles[1].high, 113.);
        assert_eq!(candles[1].low, 101.);
        assert_eq!(candles[1].volume, 120.);
        // The VWAP of the typical prices, equal volumes: their mean.
        let typical: f64 = bars.iter().map(Bar::typical).sum();
        assert!((candles[1].vwap - typical / 13.).abs() < 1e-9);
        // Five-second candles are the bars, the newest `MAX_CANDLES`.
        let many: Vec<Bar> = (0..200).map(|_| bar(1., 2., 1.)).collect();
        assert_eq!(candles_len(&many, Timeframe::FiveSeconds), MAX_CANDLES);
        assert_eq!(candles_len(&many, Timeframe::FiveMinutes), 4);
    }

    fn candles_len(bars: &[Bar], timeframe: Timeframe) -> usize {
        candles(bars, 0, timeframe, 20).len()
    }

    #[test]
    fn test_band_centers_are_evenly_spaced_inside_the_plot() {
        let centers = band_centers(10);
        assert_eq!(centers.len(), 10);
        assert!(centers[0] > 0. && centers[9] < 1.);
        let step = centers[1] - centers[0];
        assert!(
            centers
                .windows(2)
                .all(|pair| (pair[1] - pair[0] - step).abs() < 1e-5)
        );
        let single = band_centers(1);
        assert!(single.len() == 1 && (single[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn test_price_levels_are_round_and_inside() {
        assert_eq!(
            price_levels(101.3, 104.9, 0.01),
            [101.5, 102., 102.5, 103., 103.5, 104., 104.5]
        );
        let levels = price_levels(179.91, 180.37, 0.01);
        // Five cents apart, from 179.95.
        assert!(levels.len() == 9 && (levels[0] - 179.95).abs() < 1e-9);
        assert!(levels.iter().all(|level| (179.91..=180.37).contains(level)));
        assert!(price_levels(5., 5., 0.01).is_empty());
        // Never finer than the tick: two ticks of 0.25 make one level apart.
        assert_eq!(price_levels(6800., 6800.5, 0.25), [6800., 6800.25, 6800.5]);
    }

    #[test]
    fn test_ema_follows_the_closes() {
        // Flat closes keep the EMA; a jump moves it a share `2 / (n + 1)`.
        let mut bars: Vec<Bar> = (0..10).map(|_| bar(100., 100., 1.)).collect();
        bars.push(bar(100., 110., 1.));
        let candles = candles(&bars, 0, Timeframe::FiveSeconds, 3);
        assert!(candles[..10].iter().all(|candle| candle.ema == 100.));
        assert!((candles[10].ema - 105.).abs() < 1e-9);
    }

    #[test]
    fn test_timeframes_parse_and_cycle() {
        assert_eq!(Timeframe::parse("1M"), Some(Timeframe::OneMinute));
        assert_eq!(Timeframe::parse("15m"), None);
        let mut timeframe = Timeframe::default();
        for _ in Timeframe::ALL {
            timeframe = timeframe.next();
        }
        assert_eq!(timeframe, Timeframe::default());
    }
}
