//! The sea as its ships draw it: no coastline, only every vessel at its
//! position, an arrow along its heading while it moves and a point while
//! it lies still, in its type's colour, each with a faint wake of where it
//! has been. Within minutes the wakes trace the fairways and the points
//! the ports. A graticule and a few ports' names give the bearings; the
//! selected vessel is ringed in the accent with its whole wake.
//!
//! The projection is equirectangular with the longitude scaled by the
//! cosine of the area's middle latitude: true to shape at these latitudes
//! over a few hundred kilometres.

use std::{
    cell::{Cell, RefCell},
    f32::consts::PI,
    rc::Rc,
    sync::Arc,
};

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{
    App, BorderStyle, Bounds, ContentMask, Hsla, IntoElement, ParentElement as _, Path,
    PathBuilder, Pixels, Point, SharedString, Styled as _, TextAlign, TextRun, Window, canvas, div,
    point, px, quad, size,
};

use super::{
    ais::ShipCategory,
    driver::{FleetFrame, VesselDetail},
    fleet::Dot,
    theme::category_color,
};

/// A part of the sea the map shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub label: &'static str,
    /// The same in a word, for a phone.
    pub short_label: &'static str,
    west: f32,
    east: f32,
    south: f32,
    north: f32,
}

/// The areas, the whole first: every Finnish water and the northern
/// Baltic, the Gulf of Finland, and the Archipelago Sea with Åland.
pub const AREAS: [Area; 3] = [
    Area {
        label: "All waters",
        short_label: "All",
        west: 16.8,
        east: 31.2,
        south: 58.3,
        north: 66.0,
    },
    Area {
        label: "Gulf of Finland",
        short_label: "Gulf",
        west: 22.4,
        east: 30.6,
        south: 59.1,
        north: 60.9,
    },
    Area {
        label: "Archipelago",
        short_label: "Åland",
        west: 19.0,
        east: 23.4,
        south: 59.4,
        north: 60.95,
    },
];

/// Ports and capitals, for the bearings: their names and positions.
const PLACES: [(&str, f32, f32); 16] = [
    ("Helsinki", 24.95, 60.16),
    ("Tallinn", 24.75, 59.44),
    ("Turku", 22.25, 60.44),
    ("Stockholm", 18.07, 59.33),
    ("Mariehamn", 19.94, 60.10),
    ("Hanko", 22.95, 59.82),
    ("Kotka", 26.95, 60.46),
    ("St Petersburg", 30.30, 59.93),
    ("Pori", 21.46, 61.60),
    ("Rauma", 21.46, 61.13),
    ("Vaasa", 21.57, 63.09),
    ("Umeå", 20.26, 63.82),
    ("Oulu", 25.43, 65.01),
    ("Kemi", 24.52, 65.72),
    ("Paldiski", 24.05, 59.35),
    ("Kokkola", 23.03, 63.84),
];

/// Where a longitude and latitude fall in a box of pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    origin: Point<Pixels>,
    /// Pixels a degree of longitude and of latitude.
    x_scale: f32,
    y_scale: f32,
    west: f32,
    north: f32,
}

impl Projection {
    /// `area` fitted into `bounds`, centred, true to shape.
    pub fn fit(area: Area, bounds: Bounds<Pixels>) -> Self {
        let middle = ((area.south + area.north) / 2.).to_radians().cos();
        let width = (area.east - area.west) * middle;
        let height = area.north - area.south;
        let scale = (f32::from(bounds.size.width) / width)
            .min(f32::from(bounds.size.height) / height)
            .max(0.);
        let (x_scale, y_scale) = (scale * middle, scale);
        let used_width = (area.east - area.west) * x_scale;
        let used_height = height * y_scale;
        Self {
            origin: point(
                bounds.origin.x + (bounds.size.width - px(used_width)) / 2.,
                bounds.origin.y + (bounds.size.height - px(used_height)) / 2.,
            ),
            x_scale,
            y_scale,
            west: area.west,
            north: area.north,
        }
    }

    pub fn point(&self, lon: f32, lat: f32) -> Point<Pixels> {
        point(
            self.origin.x + px((lon - self.west) * self.x_scale),
            self.origin.y + px((self.north - lat) * self.y_scale),
        )
    }
}

/// The colours of the map, from the theme.
#[derive(Clone)]
struct Colors {
    sea: Hsla,
    grid: Hsla,
    place: Hsla,
    selected: Hsla,
    label_background: Hsla,
    label: Hsla,
    categories: [Hsla; ShipCategory::ALL.len()],
    font: SharedString,
}

impl Colors {
    fn new(theme: &Theme) -> Self {
        Self {
            sea: theme.table,
            grid: theme.chart_grid,
            place: theme.muted_foreground,
            selected: theme.primary,
            label_background: theme.background.opacity(0.85),
            label: theme.foreground,
            categories: ShipCategory::ALL.map(|category| category_color(category, theme)),
            font: theme.mono_font_family.clone(),
        }
    }
}

/// The map's tessellated paths, kept from frame to frame while nothing
/// they are drawn from changes: tessellating every wake on every frame
/// would cost more than the rest of the window.
#[derive(Default)]
pub struct MapCache {
    key: Option<(u64, Bounds<Pixels>, &'static str, Option<u32>)>,
    layers: Layers,
}

/// What the map paints from paths, in order, and the graticule's labels.
#[derive(Default)]
struct Layers {
    graticule: Vec<(Path<Pixels>, Hsla)>,
    labels: Vec<(String, Point<Pixels>)>,
    wakes: Vec<(Path<Pixels>, Hsla)>,
    /// The quiet vessels' arrows, then the others'.
    arrows: [Vec<(Path<Pixels>, Hsla)>; 2],
    selected: Vec<(Path<Pixels>, Hsla)>,
}

/// The map of `frame` (the `version`th) over `area`, `selected` ringed.
/// Its painted bounds go to `painted`, for the clicks that select the
/// nearest vessel; its paths stay in `cache` until they change.
pub fn map(
    frame: Arc<FleetFrame>,
    version: u64,
    area: Area,
    selected: Option<u32>,
    painted: Rc<Cell<Option<Bounds<Pixels>>>>,
    cache: Rc<RefCell<MapCache>>,
    cx: &App,
) -> impl IntoElement {
    let colors = Colors::new(cx.theme());
    div().size_full().child(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                painted.set(Some(bounds));
                let mut cache = cache.borrow_mut();
                let key = Some((version, bounds, area.label, selected));
                if cache.key != key {
                    cache.key = key;
                    cache.layers = build_layers(bounds, &frame, area, selected, &colors);
                }
                paint(
                    bounds,
                    &frame,
                    area,
                    selected,
                    &cache.layers,
                    &colors,
                    window,
                    cx,
                );
            },
        )
        .size_full(),
    )
}

/// The vessel nearest `position` within `reach` pixels on a map of `area`
/// painted in `bounds`.
pub fn nearest(
    dots: &[Dot],
    area: Area,
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    reach: f32,
) -> Option<u32> {
    let projection = Projection::fit(area, bounds);
    dots.iter()
        .map(|dot| {
            let at = projection.point(dot.lon, dot.lat);
            let (dx, dy) = (f32::from(at.x - position.x), f32::from(at.y - position.y));
            (dot.mmsi, dx.hypot(dy))
        })
        .filter(|(_, distance)| *distance <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(mmsi, _)| mmsi)
}

fn circle(center: Point<Pixels>, diameter: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(center.x - px(diameter / 2.), center.y - px(diameter / 2.)),
        size: size(px(diameter), px(diameter)),
    }
}

/// An arrow at `at` pointing along `bearing` (degrees from north), into
/// `path`: a slim triangle with a notch at its tail.
fn arrow(path: &mut PathBuilder, at: Point<Pixels>, bearing: f32, length: f32) {
    let angle = bearing * PI / 180.;
    let (sin, cos) = angle.sin_cos();
    let along = |forward: f32, side: f32| {
        point(
            at.x + px(forward * sin + side * cos),
            at.y + px(-forward * cos + side * sin),
        )
    };
    let half = length / 2.;
    path.move_to(along(half, 0.));
    path.line_to(along(-half, length * 0.36));
    path.line_to(along(-half * 0.45, 0.));
    path.line_to(along(-half, -length * 0.36));
    path.close();
}

/// Text at `at` in the map's font, unless it would overlap a label in
/// `taken`; the labels painted go there.
#[allow(clippy::too_many_arguments)]
fn label(
    text: &str,
    at: Point<Pixels>,
    color: Hsla,
    font: &SharedString,
    font_size: Pixels,
    taken: &mut Vec<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) {
    let run = TextRun {
        len: text.len(),
        font: gpui_kit::font(font.clone()),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(text.to_owned()),
        font_size,
        &[run],
        None,
    );
    let line_height = font_size * 1.3;
    let room = Bounds {
        origin: point(at.x - px(2.), at.y),
        size: size(line.width + px(4.), line_height),
    };
    if taken.iter().any(|other| other.intersects(&room)) {
        return;
    }
    taken.push(room);
    let _ = line.paint(at, line_height, TextAlign::Left, None, window, cx);
}

/// The first of `steps` (degrees) that puts lines at least `min_gap`
/// pixels apart at `scale` pixels a degree.
fn step(steps: &[f32], scale: f32, min_gap: f32) -> f32 {
    steps
        .iter()
        .copied()
        .find(|step| step * scale >= min_gap)
        .unwrap_or(steps[steps.len() - 1])
}

fn paths(builders: Vec<PathBuilder>, colors: impl Fn(usize) -> Hsla) -> Vec<(Path<Pixels>, Hsla)> {
    builders
        .into_iter()
        .enumerate()
        .filter_map(|(ix, builder)| Some((builder.build().ok()?, colors(ix))))
        .collect()
}

/// Tessellates what the map draws as paths.
fn build_layers(
    bounds: Bounds<Pixels>,
    frame: &FleetFrame,
    area: Area,
    selected: Option<u32>,
    colors: &Colors,
) -> Layers {
    let projection = Projection::fit(area, bounds);
    let mut layers = Layers::default();

    // Meridians and parallels at round degrees, far enough apart to read,
    // labelled along the top and the left edge.
    let lon_step = step(&[0.5, 1., 2., 5.], projection.x_scale, 64.);
    let lat_step = step(&[0.25, 0.5, 1., 2.], projection.y_scale, 30.);
    let decimals = |step: f32| usize::from(step < 1.) + usize::from(step < 0.5);
    let mut grid = PathBuilder::stroke(px(1.));
    let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
    let (left, right) = (bounds.origin.x, bounds.origin.x + bounds.size.width);
    let mut lon = (area.west / lon_step).ceil() * lon_step;
    while lon <= area.east {
        let x = projection.point(lon, area.north).x;
        grid.move_to(point(x, top));
        grid.line_to(point(x, bottom));
        let text = format!("{:.*}°E", decimals(lon_step), lon);
        layers.labels.push((text, point(x + px(3.), top + px(3.))));
        lon += lon_step;
    }
    let mut lat = (area.south / lat_step).ceil() * lat_step;
    while lat <= area.north {
        let y = projection.point(area.west, lat).y;
        grid.move_to(point(left, y));
        grid.line_to(point(right, y));
        let text = format!("{:.*}°N", decimals(lat_step), lat);
        layers.labels.push((text, point(left + px(4.), y + px(2.))));
        lat += lat_step;
    }
    layers.graticule = paths(vec![grid], |_| colors.grid);

    // The wakes, a path a ship type.
    let mut wakes: Vec<PathBuilder> = (0..ShipCategory::ALL.len())
        .map(|_| PathBuilder::stroke(px(1.)))
        .collect();
    for trail in &frame.trails {
        polyline(&mut wakes[trail.category.ix()], &trail.points, &projection);
    }
    layers.wakes = paths(wakes, |ix| colors.categories[ix].opacity(0.32));

    // The moving vessels' arrows, the quiet ones faint.
    let arrow_length = (f32::from(bounds.size.width) / 90.).clamp(7., 12.);
    for (layer, stale) in [true, false].into_iter().enumerate() {
        let mut arrows: Vec<PathBuilder> = (0..ShipCategory::ALL.len())
            .map(|_| PathBuilder::fill())
            .collect();
        for dot in frame.dots.iter().filter(|dot| dot.stale == stale) {
            if let Some(bearing) = dot.bearing.filter(|_| dot.moving) {
                let at = projection.point(dot.lon, dot.lat);
                arrow(&mut arrows[dot.category.ix()], at, bearing, arrow_length);
            }
        }
        layers.arrows[layer] = paths(arrows, |ix| {
            let color = colors.categories[ix];
            if stale { color.opacity(0.35) } else { color }
        });
    }

    // The selected vessel's whole wake.
    if let Some(detail) = frame
        .selected
        .as_ref()
        .filter(|detail| Some(detail.row.mmsi) == selected && detail.trail.len() > 1)
    {
        let mut wake = PathBuilder::stroke(px(2.));
        polyline(&mut wake, &detail.trail, &projection);
        layers.selected = paths(vec![wake], |_| colors.selected.opacity(0.8));
    }
    layers
}

/// `points` as one line of `path`.
fn polyline(path: &mut PathBuilder, points: &[(f32, f32)], projection: &Projection) {
    for (ix, (lon, lat)) in points.iter().enumerate() {
        let at = projection.point(*lon, *lat);
        if ix == 0 {
            path.move_to(at);
        } else {
            path.line_to(at);
        }
    }
}

fn paint_paths(paths: &[(Path<Pixels>, Hsla)], window: &mut Window) {
    for (path, color) in paths {
        window.paint_path(path.clone(), *color);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    bounds: Bounds<Pixels>,
    frame: &FleetFrame,
    area: Area,
    selected: Option<u32>,
    layers: &Layers,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) {
    let projection = Projection::fit(area, bounds);
    window.paint_quad(quad(
        bounds,
        px(0.),
        colors.sea,
        px(0.),
        colors.sea,
        BorderStyle::Solid,
    ));
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let mut taken = Vec::new();
        paint_paths(&layers.graticule, window);
        for (text, at) in &layers.labels {
            let color = colors.place.opacity(0.7);
            label(
                text,
                *at,
                color,
                &colors.font,
                px(10.),
                &mut taken,
                window,
                cx,
            );
        }
        paint_paths(&layers.wakes, window);
        paint_places(&projection, colors, &mut taken, window, cx);
        // Still vessels as points, moving ones as arrows; the quiet ones
        // faint, under the others.
        for (layer, stale) in [true, false].into_iter().enumerate() {
            for dot in frame
                .dots
                .iter()
                .filter(|dot| dot.stale == stale && !(dot.moving && dot.bearing.is_some()))
            {
                let color = colors.categories[dot.category.ix()];
                let color = if stale {
                    color.opacity(0.35)
                } else {
                    color.opacity(0.85)
                };
                let side = 3.;
                window.paint_quad(quad(
                    circle(projection.point(dot.lon, dot.lat), side),
                    px(side / 2.),
                    color,
                    px(0.),
                    color,
                    BorderStyle::Solid,
                ));
            }
            paint_paths(&layers.arrows[layer], window);
        }
        paint_paths(&layers.selected, window);
        if let Some(detail) = frame
            .selected
            .as_ref()
            .filter(|detail| Some(detail.row.mmsi) == selected)
        {
            paint_selected(detail, &projection, colors, window, cx);
        }
    });
}

/// The ports, the most travelled first, each named where its name fits.
fn paint_places(
    projection: &Projection,
    colors: &Colors,
    taken: &mut Vec<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) {
    for (name, lon, lat) in PLACES {
        let at = projection.point(lon, lat);
        window.paint_quad(quad(
            circle(at, 4.),
            px(2.),
            gpui_kit::transparent_black(),
            px(1.),
            colors.place.opacity(0.8),
            BorderStyle::Solid,
        ));
        let at = point(at.x + px(5.), at.y - px(7.));
        label(
            name,
            at,
            colors.place,
            &colors.font,
            px(10.5),
            taken,
            window,
            cx,
        );
    }
}

/// The selected vessel's rings and its name and speed on a backdrop.
fn paint_selected(
    detail: &VesselDetail,
    projection: &Projection,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) {
    let (Some(lat), Some(lon)) = (detail.row.latitude, detail.row.longitude) else {
        return;
    };
    let at = projection.point(lon as f32, lat as f32);
    for (diameter, width, opacity) in [(18., 2., 1.), (30., 1., 0.45)] {
        window.paint_quad(quad(
            circle(at, diameter),
            px(diameter / 2.),
            gpui_kit::transparent_black(),
            px(width),
            colors.selected.opacity(opacity),
            BorderStyle::Solid,
        ));
    }
    let mut text = detail.row.title();
    if let Some(speed) = detail.row.speed {
        text.push_str(&format!("  {:.1} kn", speed));
    }
    let origin = point(at.x + px(17.), at.y - px(9.));
    let font_size = px(11.5);
    let run = TextRun {
        len: text.len(),
        font: gpui_kit::font(colors.font.clone()),
        color: colors.label,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(text), font_size, &[run], None);
    window.paint_quad(quad(
        Bounds {
            origin: point(origin.x - px(4.), origin.y - px(1.)),
            size: size(line.width + px(8.), font_size * 1.3 + px(2.)),
        },
        px(2.),
        colors.label_background,
        px(1.),
        colors.selected.opacity(0.6),
        BorderStyle::Solid,
    ));
    let _ = line.paint(origin, font_size * 1.3, TextAlign::Left, None, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(10.), px(20.)),
            size: size(px(width), px(height)),
        }
    }

    #[test]
    fn test_projection_puts_north_up_east_right_and_fits() {
        let area = AREAS[0];
        let projection = Projection::fit(area, bounds(400., 600.));
        let north_west = projection.point(area.west, area.north);
        let south_east = projection.point(area.east, area.south);
        assert!(south_east.x > north_west.x && south_east.y > north_west.y);
        // Inside the bounds, touching two opposite sides.
        assert!(north_west.x >= px(10.) - px(0.01) && south_east.x <= px(410.) + px(0.01));
        assert!(north_west.y >= px(20.) - px(0.01) && south_east.y <= px(620.) + px(0.01));
        let touches_width = (f32::from(south_east.x - north_west.x) - 400.).abs() < 0.1;
        let touches_height = (f32::from(south_east.y - north_west.y) - 600.).abs() < 0.1;
        assert!(touches_width || touches_height);
        // A degree of longitude is about half a degree of latitude here.
        let one_east = projection.point(area.west + 1., area.north);
        let one_south = projection.point(area.west, area.north - 1.);
        let ratio = f32::from(one_east.x - north_west.x) / f32::from(one_south.y - north_west.y);
        assert!((0.4..0.55).contains(&ratio), "{ratio}");
    }

    #[test]
    fn test_nearest_vessel_within_reach() {
        let area = AREAS[1];
        let bounds = bounds(800., 400.);
        let dot = |mmsi, lon, lat| Dot {
            mmsi,
            lon,
            lat,
            bearing: None,
            moving: false,
            category: ShipCategory::Cargo,
            stale: false,
        };
        let dots = [dot(1, 24.95, 60.10), dot(2, 25.40, 60.00)];
        let projection = Projection::fit(area, bounds);
        let near_one = projection.point(24.96, 60.10);
        assert_eq!(nearest(&dots, area, bounds, near_one, 12.), Some(1));
        let far = projection.point(27.0, 59.5);
        assert_eq!(nearest(&dots, area, bounds, far, 12.), None);
    }
}
