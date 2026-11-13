//! Service health: each service against its SLO, the lowest error budgets
//! first; the simulator pins the three lowest on top, where they stay in
//! view however the rows are sorted. The status is an image: a colour SVG
//! mark of its severity, drawn from the theme's status colours by the
//! grid's own image source (`SeverityMarks`).

use std::{cell::RefCell, fmt::Write as _, sync::Arc, time::Duration};

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{
    App, AppContext as _, Entity, FontWeight, Hsla, Image, ImageFormat, RenderImage, Rgba, Window,
    px, transparent_black,
};
use gpui_kit_grid::{
    CellImage, CellImageSource, CellImages, CellStyle, CellStyleContext, GridColumn, GridSchema,
    GridStore, GridTuning, HighlightPolicy, LiveGridDelegate, LiveGridState, RowChange,
    SelectionMode, SortDirection, StoreOptions, ViewSpec,
};

use super::{
    rows::{ServiceColumn, ServiceRow, Severity},
    theme::status_color,
    ui,
};

/// How long a changed status shows.
const FLASH: Duration = Duration::from_millis(900);
/// The order changes at most this often: a list to read.
const REBUILD: Duration = Duration::from_secs(1);

/// The service store's options: a short flash.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 4))
}

/// The error budget's status: what is left of it.
fn budget_status(budget: f64) -> Severity {
    match budget {
        budget if budget < 10. => Severity::Critical,
        budget if budget < 25. => Severity::Major,
        budget if budget < 50. => Severity::Warning,
        _ => Severity::Ok,
    }
}

/// The service grid's hooks, colours from the theme only: the status and
/// the budget in their status colours, latency and errors in the status
/// colour while the service is unhealthy, the pinned services bold. Only
/// the status flashes. They take `&self`, never read the grid and never
/// allocate.
pub struct Services;

impl LiveGridDelegate for Services {
    type Row = ServiceRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, ServiceRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let service = cell.row();
        let column = cell.column();
        if let Some(change) = cell.change()
            && change.change() != RowChange::Removed
            && column != ServiceColumn::Status
        {
            style.set_background(transparent_black());
        }
        let unhealthy = service.status != Severity::Ok;
        match column {
            ServiceColumn::Service if cell.pinned_edge().is_some() => {
                style.set_font_weight(FontWeight::BOLD)
            }
            ServiceColumn::Status => style.set_foreground(status_color(service.status, theme)),
            ServiceColumn::Budget => {
                let status = budget_status(service.budget);
                if status != Severity::Ok {
                    style.set_foreground(status_color(status, theme));
                }
            }
            ServiceColumn::P99 | ServiceColumn::Errors if unhealthy => {
                style.set_foreground(status_color(service.status, theme))
            }
            ServiceColumn::Slo | ServiceColumn::Hosts => {
                style.set_foreground(theme.muted_foreground)
            }
            _ => {}
        }
    }
}

/// The services grid's image source: each severity's mark, a two-colour
/// SVG in its status colour with the background's colour inside, drawn once
/// per colour (a new theme draws it again) and kept. A mark's id is
/// `Severity::mark`'s.
#[derive(Default)]
pub struct SeverityMarks {
    drawn: RefCell<Vec<(Severity, Hsla, Arc<RenderImage>)>>,
}

impl CellImageSource for SeverityMarks {
    fn image(&self, id: &str, _: &mut Window, cx: &mut App) -> CellImage {
        let Some(severity) = Severity::from_mark(id) else {
            return CellImage::Missing;
        };
        let theme = cx.theme();
        let color = status_color(severity, theme);
        let mut drawn = self.drawn.borrow_mut();
        if let Some((_, _, image)) = drawn
            .iter()
            .find(|(each, each_color, _)| *each == severity && *each_color == color)
        {
            return CellImage::Ready(image.clone());
        }
        let svg = mark_svg(severity, color, theme);
        match Image::from_bytes(ImageFormat::Svg, svg.into_bytes()).to_image_data(cx.svg_renderer())
        {
            Ok(image) => {
                drawn.retain(|(each, _, _)| *each != severity);
                drawn.push((severity, color, image.clone()));
                CellImage::Ready(image)
            }
            Err(_) => CellImage::Missing,
        }
    }
}

/// `#rrggbb` of a colour, opaque.
fn hex(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(rgba.r),
        channel(rgba.g),
        channel(rgba.b)
    )
}

/// The mark of `severity`, 32 × 32: its shape (square, triangle, diamond,
/// circle; a circle with a check for OK) in `color`, with an exclamation
/// mark or the check inside in the background's colour.
fn mark_svg(severity: Severity, color: Hsla, theme: &Theme) -> String {
    let (fill, inner) = (hex(color), hex(theme.background));
    let shape = match severity {
        Severity::Critical => r#"<rect x="3" y="3" width="26" height="26" rx="4"/>"#,
        Severity::Major => r#"<path d="M16 2 L31 29 L1 29 Z"/>"#,
        Severity::Minor => r#"<path d="M16 1 L31 16 L16 31 L1 16 Z"/>"#,
        Severity::Warning | Severity::Ok => r#"<circle cx="16" cy="16" r="14"/>"#,
    };
    let inside = match severity {
        Severity::Ok => format!(
            r#"<path d="M9 16.5 L14 21.5 L23 11" fill="none" stroke="{inner}" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/>"#
        ),
        Severity::Major => format!(
            r#"<rect x="14.5" y="11" width="3" height="9" fill="{inner}"/><rect x="14.5" y="22.5" width="3" height="3" fill="{inner}"/>"#
        ),
        _ => format!(
            r#"<rect x="14.5" y="8" width="3" height="11" fill="{inner}"/><rect x="14.5" y="21.5" width="3" height="3" fill="{inner}"/>"#
        ),
    };
    let mut svg = String::with_capacity(320);
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><g fill="{fill}">{shape}</g>{inside}</svg>"#
    );
    svg
}

fn column(column: ServiceColumn, title: &'static str, width: f32) -> GridColumn<ServiceColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

fn schema() -> GridSchema<ServiceColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(column(ServiceColumn::Service, "Service", 150.))
        // An image sorts by its text, which is not the rank: no sort here.
        .column(column(ServiceColumn::Status, "Status", 84.).with_sortable(false))
        .column(column(ServiceColumn::Slo, "SLO %", 66.))
        .column(column(ServiceColumn::Budget, "Budget left %", 124.))
        .column(column(ServiceColumn::P99, "p99 ms", 72.))
        .column(column(ServiceColumn::Errors, "Errors %", 76.))
        .column(column(ServiceColumn::Hosts, "Hosts", 62.))
}

pub fn service_grid(store: GridStore<ServiceRow>, cx: &mut App) -> Entity<LiveGridState<Services>> {
    cx.new(|cx| {
        LiveGridState::new(schema(), Services, store, cx)
            .with_view_id("services")
            .with_selection_mode(SelectionMode::RowSingle)
            .with_change_paint(ui::accent_changes(cx))
            .with_tuning(GridTuning::default().with_min_rebuild_interval(REBUILD))
            .with_image_source(CellImages::custom(SeverityMarks::default()))
            .with_view(
                ViewSpec::new().with_sort([(ServiceColumn::Budget, SortDirection::Ascending)]),
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marks_are_two_colour_svgs() {
        let theme = Theme::default();
        let color = status_color(Severity::Critical, &theme);
        let svg = mark_svg(Severity::Critical, color, &theme);
        assert!(svg.contains(&hex(color)) && svg.contains(&hex(theme.background)));
        assert_eq!(hex(gpui_kit::hsla(0., 1., 0.5, 1.)), "#ff0000");
    }

    #[test]
    fn test_budget_status_worsens_as_it_drains() {
        let levels = [80., 40., 20., 5.].map(budget_status);
        assert!(levels.windows(2).all(|pair| pair[0] > pair[1]));
    }
}
