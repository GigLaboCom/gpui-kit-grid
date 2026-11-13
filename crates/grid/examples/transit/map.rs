//! The map: every vehicle under way at its place over the network's links
//! and stations, painted on a canvas, north up. A view of its own, drawn
//! cached and notified only when a sweep publishes a new frame or the
//! selection or the zoom changes, so the table's flashes do not repaint
//! it. The selected run's stations are drawn over the rest, the part
//! already travelled quieter, and the vehicle ringed; a click on a vehicle
//! selects it (`MapEvent::Select`), a click on a station away from the
//! vehicles opens its board where the source has one
//! (`MapEvent::Station`), and the board's station is marked.
//!
//! The places are the timetable's stations: the feed has no shapes, so the
//! links are straight lines between stations and a vehicle moves along
//! them by time.

use std::{cell::Cell, f64::consts::PI, rc::Rc, sync::Arc};

use gpui_kit::base::h_flex;
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{
    BorderStyle, Bounds, Context, EventEmitter, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, ParentElement as _, PathBuilder, Pixels, Point,
    Render, SharedString, Styled as _, Window, canvas, div, point, prelude::FluentBuilder as _, px,
    quad, size,
};

use super::{
    producer::{Selected, TransitFrame},
    source::{GeoBounds, GeoPoint, Mode, Network, VehicleKey},
    table::{line_colors, mode_color},
};

/// A click selects the nearest vehicle this close, in pixels.
const PICK_RADIUS: f32 = 12.;
/// The empty margin around the stations, a share of the map's side.
const MARGIN: f64 = 0.04;
/// A vehicle's side by mode: trains larger than trams, trams than buses.
fn dot_side(mode: Mode) -> f32 {
    match mode {
        Mode::Tram => 5.,
        Mode::Bus | Mode::RegionalBus => 4.,
        _ => 6.,
    }
}
const STATION: f32 = 2.;
const RING: f32 = 16.;
/// The board's station: a square this wide.
const BOARD_MARK: f32 = 10.;

/// How much of the network the map shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MapZoom {
    /// The stations of the city's own lines: U-Bahn, tram and bus.
    #[default]
    City,
    /// Every station, the S-Bahn's ends included.
    Region,
}

impl MapZoom {
    pub const ALL: [Self; 2] = [Self::City, Self::Region];

    pub fn label(self) -> &'static str {
        match self {
            Self::City => "City",
            Self::Region => "Region",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::City => Self::Region,
            Self::Region => Self::City,
        }
    }

    fn bounds(self, network: &Network) -> Option<GeoBounds> {
        match self {
            Self::City => network.city.or(network.region),
            Self::Region => network.region,
        }
    }
}

/// Where a place falls on the map: the box shown, fitted into the canvas
/// with the same scale both ways (a degree of longitude shortened by the
/// latitude's cosine).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    center: GeoPoint,
    /// Pixels per degree of latitude.
    scale: f64,
    /// The length of a degree of longitude in degrees of latitude.
    squeeze: f64,
    origin: Point<Pixels>,
    size: (f64, f64),
}

impl Projection {
    /// `shown` fitted into `bounds`.
    pub fn fit(shown: GeoBounds, bounds: Bounds<Pixels>) -> Self {
        let center = shown.center();
        let squeeze = (center.lat * PI / 180.).cos();
        let width = f64::from(f32::from(bounds.size.width));
        let height = f64::from(f32::from(bounds.size.height));
        let span_x = ((shown.max.lon - shown.min.lon) * squeeze).max(1e-6);
        let span_y = (shown.max.lat - shown.min.lat).max(1e-6);
        let scale = (width / span_x).min(height / span_y) * (1. - 2. * MARGIN);
        Self {
            center,
            scale,
            squeeze,
            origin: bounds.origin,
            size: (width, height),
        }
    }

    /// The point of `at`.
    pub fn place(&self, at: GeoPoint) -> Point<Pixels> {
        let x = self.size.0 / 2. + (at.lon - self.center.lon) * self.squeeze * self.scale;
        let y = self.size.1 / 2. - (at.lat - self.center.lat) * self.scale;
        point(self.origin.x + px(x as f32), self.origin.y + px(y as f32))
    }
}

/// What the map tells its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapEvent {
    /// A click on a vehicle.
    Select(VehicleKey),
    /// A click on a station, away from the vehicles, where the source has
    /// boards.
    Station(u32),
}

pub struct TransitMap {
    frame: Arc<TransitFrame>,
    zoom: MapZoom,
    /// The canvas's bounds at the last paint, for clicks.
    painted: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl EventEmitter<MapEvent> for TransitMap {}

impl TransitMap {
    pub fn new() -> Self {
        Self {
            frame: Arc::default(),
            zoom: MapZoom::City,
            painted: Rc::default(),
        }
    }

    /// A new sweep's frame.
    pub fn set_frame(&mut self, frame: Arc<TransitFrame>, cx: &mut Context<Self>) {
        self.frame = frame;
        cx.notify();
    }

    pub fn zoom(&self) -> MapZoom {
        self.zoom
    }

    pub fn set_zoom(&mut self, zoom: MapZoom, cx: &mut Context<Self>) {
        if self.zoom != zoom {
            self.zoom = zoom;
            cx.notify();
        }
    }

    /// The vehicle nearest `position`, within the pick radius; else the
    /// nearest station, where the source has boards.
    fn pick(&self, position: Point<Pixels>) -> Option<MapEvent> {
        let bounds = self.painted.get()?;
        let info = self.frame.info.as_ref()?;
        let network = &info.network;
        let projection = Projection::fit(self.zoom.bounds(network)?, bounds);
        let distance = |at: GeoPoint| {
            let at = projection.place(at);
            let (dx, dy) = (f32::from(at.x - position.x), f32::from(at.y - position.y));
            dx * dx + dy * dy
        };
        let nearest = |points: &mut dyn Iterator<Item = (MapEvent, f32)>| {
            points
                .filter(|(_, distance)| *distance <= PICK_RADIUS * PICK_RADIUS)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(event, _)| event)
        };
        nearest(
            &mut self
                .frame
                .dots
                .iter()
                .map(|dot| (MapEvent::Select(dot.key), distance(dot.at))),
        )
        .or_else(|| {
            info.has_board.then_some(())?;
            nearest(
                &mut network
                    .stations
                    .iter()
                    .enumerate()
                    .map(|(ix, at)| (MapEvent::Station(ix as u32), distance(*at))),
            )
        })
    }
}

/// The colours of the map, from the theme and the lines' data.
struct Palette {
    land: Hsla,
    station: Hsla,
    links: [Hsla; Mode::COUNT],
    /// A live line the network does not know: its mode's colour.
    modes: [Hsla; Mode::COUNT],
    ring: Hsla,
    outline: Hsla,
    /// By line rank.
    lines: Vec<Hsla>,
}

impl Palette {
    fn new(network: &Network, theme: &Theme) -> Self {
        Self {
            land: theme.table,
            station: theme.muted_foreground.opacity(0.45),
            links: Mode::ALL.map(|mode| mode_color(mode, theme).opacity(0.22)),
            modes: Mode::ALL.map(|mode| mode_color(mode, theme)),
            ring: theme.foreground,
            outline: theme.table,
            lines: network
                .lines
                .iter()
                .map(|line| line_colors(line, theme).0)
                .collect(),
        }
    }
}

impl Render for TransitMap {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let frame = self.frame.clone();
        let zoom = self.zoom;
        let painted = self.painted.clone();
        let network = frame.info.as_ref().map(|info| info.network.clone());
        let palette = network.as_ref().map(|network| Palette::new(network, theme));
        let label = frame.selected.as_ref().map(|selected| {
            SharedString::from(format!("{} {}", selected.line.name, selected.headsign))
        });
        let label_color = frame
            .selected
            .as_ref()
            .map(|selected| line_colors(&selected.line, theme).0);
        let (background, foreground) = (theme.background, theme.foreground);
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.table)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|map, event: &MouseDownEvent, _, cx| {
                    if let Some(event) = map.pick(event.position) {
                        cx.emit(event);
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        painted.set(Some(bounds));
                        bounds
                    },
                    move |_, bounds, window, _| {
                        let (Some(network), Some(palette)) = (network, palette) else {
                            return;
                        };
                        let Some(shown) = zoom.bounds(&network) else {
                            return;
                        };
                        let projection = Projection::fit(shown, bounds);
                        paint(&projection, &network, &frame, &palette, bounds, window);
                    },
                )
                .size_full(),
            )
            .when_some(label.zip(label_color), |map, (label, color)| {
                map.child(
                    h_flex()
                        .absolute()
                        .top(px(8.))
                        .left(px(8.))
                        .px(px(6.))
                        .py(px(2.))
                        .gap(px(6.))
                        .rounded(px(3.))
                        .bg(background.opacity(0.9))
                        .child(div().size(px(8.)).rounded_full().bg(color))
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .text_color(foreground)
                                .child(label),
                        ),
                )
            })
    }
}

fn circle(center: Point<Pixels>, side: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(center.x - px(side / 2.), center.y - px(side / 2.)),
        size: size(px(side), px(side)),
    }
}

fn paint(
    projection: &Projection,
    network: &Network,
    frame: &TransitFrame,
    palette: &Palette,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    window.paint_quad(quad(
        bounds,
        px(0.),
        palette.land,
        px(0.),
        palette.land,
        BorderStyle::Solid,
    ));
    // The links, bus first and the U-Bahn on top: one path a mode.
    for mode in Mode::ALL.into_iter().rev() {
        let mut path = PathBuilder::stroke(px(if mode.is_rail() { 1.5 } else { 1. }));
        let mut any = false;
        for (a, b, link) in &network.links {
            if *link != mode {
                continue;
            }
            path.move_to(projection.place(network.stations[*a as usize]));
            path.line_to(projection.place(network.stations[*b as usize]));
            any = true;
        }
        if any && let Ok(path) = path.build() {
            window.paint_path(path, palette.links[mode.ix()]);
        }
    }
    for station in &network.stations {
        window.paint_quad(quad(
            circle(projection.place(*station), STATION),
            px(STATION / 2.),
            palette.station,
            px(0.),
            palette.station,
            BorderStyle::Solid,
        ));
    }
    if let Some(at) = frame
        .station
        .and_then(|station| network.stations.get(station as usize))
    {
        let mark = circle(projection.place(*at), BOARD_MARK);
        window.paint_quad(quad(
            mark,
            px(2.),
            palette.outline,
            px(2.),
            palette.ring,
            BorderStyle::Solid,
        ));
    }
    // Buses first, trains over them.
    for mode in Mode::ALL.into_iter().rev() {
        for dot in frame.dots.iter().filter(|dot| dot.mode == mode) {
            let color = palette
                .lines
                .get(dot.line as usize)
                .copied()
                .unwrap_or(palette.modes[mode.ix()]);
            let side = dot_side(mode);
            window.paint_quad(quad(
                circle(projection.place(dot.at), side + 1.),
                px((side + 1.) / 2.),
                color,
                px(0.5),
                palette.outline,
                BorderStyle::Solid,
            ));
        }
    }
    if let Some(selected) = &frame.selected {
        paint_run(projection, selected, palette, window);
        window.paint_quad(quad(
            circle(projection.place(selected.at), RING),
            px(RING / 2.),
            gpui_kit::transparent_black(),
            px(2.),
            palette.ring,
            BorderStyle::Solid,
        ));
    }
}

/// The selected run's stations over the other vehicles: the part travelled
/// quieter, the rest in its line's colour, each line on a halo of the
/// land's colour so it reads over the dots.
fn paint_run(projection: &Projection, selected: &Selected, palette: &Palette, window: &mut Window) {
    let color = palette
        .lines
        .get(selected.line.rank as usize)
        .copied()
        .unwrap_or(palette.ring);
    let leg = usize::from(selected.leg_ix).min(selected.path.len().saturating_sub(1));
    let stroke = |points: &mut dyn Iterator<Item = GeoPoint>, width: f32| {
        let mut path = PathBuilder::stroke(px(width));
        for (ix, at) in points.enumerate() {
            let at = projection.place(at);
            if ix == 0 {
                path.move_to(at);
            } else {
                path.line_to(at);
            }
        }
        path.build().ok()
    };
    let travelled = selected.path[..=leg]
        .iter()
        .copied()
        .chain(std::iter::once(selected.at));
    let ahead = || std::iter::once(selected.at).chain(selected.path[leg + 1..].iter().copied());
    let halo = selected.path[..=leg].iter().copied().chain(ahead());
    if let Some(path) = stroke(&mut halo.into_iter(), 6.) {
        window.paint_path(path, palette.outline.opacity(0.85));
    }
    if let Some(path) = stroke(&mut travelled.into_iter(), 2.) {
        window.paint_path(path, color.opacity(0.4));
    }
    if let Some(path) = stroke(&mut ahead(), 3.) {
        window.paint_path(path, color);
    }
    for (ix, at) in selected.path.iter().enumerate() {
        let side = if ix > leg { 6. } else { 4. };
        window.paint_quad(quad(
            circle(projection.place(*at), side),
            px(side / 2.),
            palette.outline,
            px(1.5),
            if ix > leg { color } else { color.opacity(0.4) },
            BorderStyle::Solid,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_projection_puts_north_up_and_keeps_the_scale() {
        let shown = GeoBounds {
            min: GeoPoint::new(48.0, 11.0),
            max: GeoPoint::new(48.2, 11.6),
        };
        let bounds = Bounds {
            origin: point(px(10.), px(20.)),
            size: size(px(400.), px(300.)),
        };
        let projection = Projection::fit(shown, bounds);
        let center = projection.place(shown.center());
        assert!((f32::from(center.x) - 210.).abs() < 0.01);
        assert!((f32::from(center.y) - 170.).abs() < 0.01);
        let north = projection.place(GeoPoint::new(48.2, 11.3));
        assert!(north.y < center.y);
        let east = projection.place(GeoPoint::new(48.1, 11.6));
        assert!(east.x > center.x);
        // Everything inside the canvas, with a margin.
        for corner in [shown.min, shown.max] {
            let at = projection.place(corner);
            assert!((10. ..=410.).contains(&f32::from(at.x)), "{at:?}");
            assert!((20. ..=320.).contains(&f32::from(at.y)), "{at:?}");
        }
        // A degree of longitude is shorter than one of latitude here.
        let lat = f32::from(projection.place(GeoPoint::new(48.1, 11.3)).y)
            - f32::from(projection.place(GeoPoint::new(48.2, 11.3)).y);
        let lon = f32::from(projection.place(GeoPoint::new(48.1, 11.4)).x)
            - f32::from(projection.place(GeoPoint::new(48.1, 11.3)).x);
        assert!((lon / lat - (48.1f32.to_radians().cos())).abs() < 0.01);
        assert_eq!(MapZoom::City.toggled(), MapZoom::Region);
    }
}
