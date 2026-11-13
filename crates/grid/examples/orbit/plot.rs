//! The sky plot: every object above the horizon at its azimuth and
//! elevation, painted on a canvas. The horizon is the outer circle and the
//! zenith the centre, north up and east to the right as on a map; rings
//! at 30° and 60°. Sunlit objects in `warning`, those in the Earth's
//! shadow in `info` (their status colours), the stations larger; the
//! selected object ringed in the accent with its pass's track.

use std::{f32::consts::PI, sync::Arc};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, BorderStyle, Bounds, Hsla, IntoElement, ParentElement as _, PathBuilder, Pixels, Point,
    Styled as _, canvas, div, point, prelude::FluentBuilder as _, px, quad, size,
};

use super::{
    producer::{Dot, SkyFrame},
    sky::PassSearch,
};

/// The plot's side; the horizon circle sits `MARGIN` inside it, room for
/// the compass letters.
pub const PLOT_SIZE: Pixels = px(372.);
const MARGIN: f32 = 22.;
/// Dots' sides: an object, a station, the selected object's ring.
const DOT: f32 = 3.;
const STATION: f32 = 7.;
const RING: f32 = 15.;
const LABEL_WIDTH: f32 = 30.;

/// The colours of the plot, from the theme.
#[derive(Clone, Copy)]
struct Colors {
    disk: Hsla,
    line: Hsla,
    horizon: Hsla,
    sunlit: Hsla,
    shadow: Hsla,
    station: Hsla,
    selected: Hsla,
}

/// Where an azimuth and elevation fall, relative to the plot's origin.
fn place(azimuth: f32, elevation: f32) -> (f32, f32) {
    let center = f32::from(PLOT_SIZE) / 2.;
    on_circle(
        azimuth,
        (center - MARGIN) * (90. - elevation.clamp(0., 90.)) / 90.,
    )
}

/// The point at `azimuth` on the circle of `radius` around the centre.
fn on_circle(azimuth: f32, radius: f32) -> (f32, f32) {
    let center = f32::from(PLOT_SIZE) / 2.;
    let angle = azimuth * PI / 180.;
    (center + radius * angle.sin(), center - radius * angle.cos())
}

/// The plot of `frame`, `selected` ringed.
pub fn sky_plot(frame: Arc<SkyFrame>, selected: Option<u32>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let colors = Colors {
        disk: theme.table_even,
        line: theme.border,
        horizon: theme.muted_foreground.opacity(0.6),
        sunlit: theme.warning,
        shadow: theme.info,
        station: theme.foreground,
        selected: theme.primary,
    };
    let center = f32::from(PLOT_SIZE) / 2.;
    let compass = [("N", 0.), ("E", 90.), ("S", 180.), ("W", 270.)].map(|(letter, azimuth)| {
        let (x, y) = on_circle(azimuth, center - MARGIN / 2.);
        div()
            .absolute()
            .left(px(x - LABEL_WIDTH / 2.))
            .top(px(y - 9.))
            .w(px(LABEL_WIDTH))
            .flex()
            .justify_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(letter)
    });
    let rings = [30., 60.].map(|elevation: f32| {
        let (_, y) = place(0., elevation);
        div()
            .absolute()
            .left(px(center + 3.))
            .top(px(y + 1.))
            .text_xs()
            .text_color(theme.muted_foreground.opacity(0.7))
            .child(format!("{elevation:.0}°"))
    });
    let label = frame
        .detail
        .as_ref()
        .filter(|detail| Some(detail.norad) == selected)
        .and_then(|detail| {
            let look = detail.position?.look;
            look.is_up().then(|| {
                let (x, y) = place(look.azimuth as f32, look.elevation as f32);
                (x, y, detail.name.clone())
            })
        });
    div()
        .relative()
        .flex_none()
        .size(PLOT_SIZE)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| paint(bounds, &frame, selected, colors, window),
            )
            .size_full(),
        )
        .children(compass)
        .children(rings)
        .when_some(label, |plot, (x, y, name)| {
            plot.child(
                div()
                    .absolute()
                    .left(px(x + RING / 2. + 3.))
                    .top(px(y - 9.))
                    .px(px(4.))
                    .bg(theme.background.opacity(0.8))
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(name.to_string()),
            )
        })
}

fn circle(center: Point<Pixels>, diameter: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(center.x - px(diameter / 2.), center.y - px(diameter / 2.)),
        size: size(px(diameter), px(diameter)),
    }
}

fn paint(
    bounds: Bounds<Pixels>,
    frame: &SkyFrame,
    selected: Option<u32>,
    colors: Colors,
    window: &mut gpui_kit::Window,
) {
    let at = |azimuth: f32, elevation: f32| {
        let (x, y) = place(azimuth, elevation);
        point(bounds.origin.x + px(x), bounds.origin.y + px(y))
    };
    let center = at(0., 90.);
    let horizon = f32::from(PLOT_SIZE) - 2. * MARGIN;
    // The sky below the horizon is not drawn: the disk is all there is.
    window.paint_quad(quad(
        circle(center, horizon),
        px(horizon / 2.),
        colors.disk,
        px(1.),
        colors.horizon,
        BorderStyle::Solid,
    ));
    for elevation in [30., 60.] {
        let diameter = horizon * (90. - elevation) / 90.;
        window.paint_quad(quad(
            circle(center, diameter),
            px(diameter / 2.),
            gpui_kit::transparent_black(),
            px(1.),
            colors.line,
            BorderStyle::Dashed,
        ));
    }
    let line = |from: Point<Pixels>, to: Point<Pixels>| {
        let mut path = PathBuilder::stroke(px(1.));
        path.move_to(from);
        path.line_to(to);
        path.build().ok()
    };
    for (from, to) in [((0., 0.), (180., 0.)), ((90., 0.), (270., 0.))] {
        if let Some(path) = line(at(from.0, from.1), at(to.0, to.1)) {
            window.paint_path(path, colors.line);
        }
    }

    let dot = |dot: &Dot, window: &mut gpui_kit::Window| {
        let (side, color) = if dot.station {
            (STATION, colors.station)
        } else if dot.sunlit {
            (DOT, colors.sunlit)
        } else {
            (DOT, colors.shadow)
        };
        window.paint_quad(quad(
            circle(at(dot.azimuth, dot.elevation), side),
            px(side / 2.),
            color,
            px(0.),
            color,
            BorderStyle::Solid,
        ));
    };
    // The shadowed first, the sunlit over them, the stations on top.
    for pass in 0..3 {
        for each in frame.dots.iter().filter(|dot| {
            let layer = if dot.station {
                2
            } else {
                usize::from(dot.sunlit)
            };
            layer == pass
        }) {
            dot(each, window);
        }
    }

    let Some(detail) = frame
        .detail
        .as_ref()
        .filter(|detail| Some(detail.norad) == selected)
    else {
        return;
    };
    if let PassSearch::Pass(pass) = &detail.pass {
        let mut path = PathBuilder::stroke(px(1.5));
        for (ix, (azimuth, elevation)) in pass.track.iter().enumerate() {
            let to = at(*azimuth, *elevation);
            if ix == 0 {
                path.move_to(to);
            } else {
                path.line_to(to);
            }
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, colors.selected.opacity(0.55));
        }
    }
    if let Some(look) = detail.position.map(|position| position.look)
        && look.is_up()
    {
        window.paint_quad(quad(
            circle(at(look.azimuth as f32, look.elevation as f32), RING),
            px(RING / 2.),
            gpui_kit::transparent_black(),
            px(2.),
            colors.selected,
            BorderStyle::Solid,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_place_puts_north_up_and_east_right() {
        let center = f32::from(PLOT_SIZE) / 2.;
        let zenith = place(123., 90.);
        assert!((zenith.0 - center).abs() < 1e-3 && (zenith.1 - center).abs() < 1e-3);
        let north = place(0., 0.);
        assert!((north.0 - center).abs() < 1e-3 && north.1 < center);
        let east = place(90., 0.);
        assert!(east.0 > center && (east.1 - center).abs() < 1e-3);
        // Below the horizon is pinned to it.
        assert_eq!(place(45., -10.), place(45., 0.));
    }
}
