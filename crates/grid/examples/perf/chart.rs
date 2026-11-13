//! The machine's CPU and GPU over the last samples, a gpui-kit area chart
//! beside the grid, as the `ops` example's: two series, 0 to 100 %, a
//! legend with each one's last value. It polls the sampler's history; a
//! new sample bumps its version.

use std::{sync::Arc, time::Duration};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, chart::AreaChart};
use gpui_kit::{
    Context, Hsla, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Task,
    Window, div, prelude::FluentBuilder as _, px,
};

use super::{
    sampler::{Control, HISTORY_LEN},
    ui,
};

const POLL: Duration = Duration::from_millis(250);
const FILL_OPACITY: f32 = 0.14;

#[derive(Clone, Debug, PartialEq)]
struct Point {
    label: SharedString,
    cpu: f64,
    gpu: f64,
}

pub struct UsageChart {
    control: Arc<Control>,
    version: u64,
    points: Vec<Point>,
    has_gpu: bool,
    _poll: Task<()>,
}

impl UsageChart {
    pub fn new(control: Arc<Control>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |chart, cx| chart.pull(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            control,
            version: 0,
            points: Vec::new(),
            has_gpu: false,
            _poll: poll,
        }
    }

    /// At most `HISTORY_LEN` points, once per sample.
    fn pull(&mut self, cx: &mut Context<Self>) {
        if self.control.history_version() == self.version {
            return;
        }
        let (version, history) = self.control.history();
        self.version = version;
        self.has_gpu = history.iter().any(|point| point.gpu.is_some());
        self.points = history
            .iter()
            .map(|point| Point {
                label: format!("#{}", point.sample).into(),
                cpu: point.cpu,
                gpu: point.gpu.unwrap_or(0.),
            })
            .collect();
        cx.notify();
    }

    fn colors(cx: &Context<Self>) -> (Hsla, Hsla) {
        let theme = cx.theme();
        (theme.primary, theme.magenta)
    }
}

impl Render for UsageChart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (cpu_color, gpu_color) = Self::colors(cx);
        let last = self.points.last().cloned();
        let has_gpu = self.has_gpu;
        let legend = |color: Hsla, name: &'static str, value: Option<f64>| {
            h_flex()
                .gap(px(6.))
                .child(div().size(px(8.)).bg(color))
                .child(div().text_color(theme.muted_foreground).child(name))
                .child(div().text_color(theme.foreground).child(SharedString::from(
                    value.map_or("—".into(), |value| format!("{value:.0} %")),
                )))
        };
        v_flex()
            .size_full()
            .gap(px(6.))
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(14.))
                    .font_features(ui::tabular_figures())
                    .child(legend(
                        cpu_color,
                        "CPU",
                        last.as_ref().map(|point| point.cpu),
                    ))
                    .child(legend(
                        gpu_color,
                        "GPU",
                        last.as_ref().filter(|_| has_gpu).map(|point| point.gpu),
                    )),
            )
            .child(div().flex_1().min_h_0().text_size(px(12.)).when(
                !self.points.is_empty(),
                |pane| {
                    let chart = AreaChart::new(self.points.iter().cloned())
                        .id("perf-usage")
                        .x(|point: &Point| point.label.clone())
                        .linear()
                        .appear(false)
                        .point_count(HISTORY_LEN)
                        .y_domain(0., 100.)
                        .y_axis(true)
                        .y_tick_count(3)
                        .y_tick_format(|value| format!("{value:.0}"))
                        .x_tick_count(0)
                        .y(|point: &Point| point.cpu)
                        .stroke(cpu_color)
                        .fill(cpu_color.opacity(FILL_OPACITY))
                        .name("CPU");
                    pane.child(chart.when(has_gpu, |chart| {
                        chart
                            .y(|point: &Point| point.gpu)
                            .stroke(gpu_color)
                            .fill(gpu_color.opacity(FILL_OPACITY))
                            .name("GPU")
                    }))
                },
            ))
    }
}
