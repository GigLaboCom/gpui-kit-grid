//! Messages a second over the last two minutes: a bar a second, locations
//! at the bottom and metadata stacked on them, the newest on the right.
//! The scale follows the busiest second shown, from 10 up.

use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, BorderStyle, Bounds, IntoElement, Pixels, Styled as _, canvas, point, px, quad, size,
};

use super::{driver::FleetFrame, fleet::HISTORY};

/// The chart's height, and on a short window.
pub const HEIGHT: Pixels = px(56.);
pub const SHORT_HEIGHT: Pixels = px(30.);

/// The scale's top: the busiest second, rounded up to a multiple of ten.
pub fn scale(history: &[(u32, u32)]) -> u32 {
    let busiest = history
        .iter()
        .map(|(locations, metadata)| locations + metadata)
        .max()
        .unwrap_or(0);
    busiest.div_ceil(10).max(1) * 10
}

/// The chart of `frame`'s history, `height` tall.
pub fn rate_chart(frame: Arc<FleetFrame>, height: Pixels, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let (locations, metadata, base) = (theme.chart_1, theme.chart_3, theme.chart_grid);
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, (), window, _| {
            let history = &frame.history;
            let top = scale(history) as f32;
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let slot = width / HISTORY as f32;
            let bar = (slot - 1.).max(1.);
            // The baseline and a line at half the scale.
            for (y, alpha) in [(height - 1., 1.), (height / 2., 0.5)] {
                window.paint_quad(quad(
                    Bounds {
                        origin: point(bounds.origin.x, bounds.origin.y + px(y)),
                        size: size(bounds.size.width, px(1.)),
                    },
                    px(0.),
                    base.opacity(alpha),
                    px(0.),
                    base,
                    BorderStyle::Solid,
                ));
            }
            let first = HISTORY.saturating_sub(history.len());
            for (ix, (located, described)) in history.iter().enumerate() {
                let x = bounds.origin.x + px((first + ix) as f32 * slot);
                let mut y = height;
                for (count, color) in [(*located, locations), (*described, metadata)] {
                    let h = count as f32 / top * (height - 2.);
                    if h <= 0. {
                        continue;
                    }
                    y -= h;
                    window.paint_quad(quad(
                        Bounds {
                            origin: point(x, bounds.origin.y + px(y)),
                            size: size(px(bar), px(h)),
                        },
                        px(0.),
                        color,
                        px(0.),
                        color,
                        BorderStyle::Solid,
                    ));
                }
            }
        },
    )
    .w_full()
    .h(height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_rounds_the_busiest_second_up() {
        assert_eq!(scale(&[]), 10);
        assert_eq!(scale(&[(3, 2)]), 10);
        assert_eq!(scale(&[(30, 5), (10, 0)]), 40);
        assert_eq!(scale(&[(40, 0)]), 40);
    }
}
