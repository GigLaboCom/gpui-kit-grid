//! The fleet as a tree: regions, data centers, halls, racks and hosts in
//! one Name column, frozen on the left. Each branch shows its hosts'
//! roll-ups (the simulator writes them); inlet temperature, CPU and errors
//! are heat-coloured; the totals row adds up the hosts.

use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, FontWeight, IntoElement as _, Window, px,
    transparent_black,
};
use gpui_kit_grid::{
    Aggregate, CellContent, CellElementContext, CellStyle, CellStyleContext, ColumnAlign,
    ColumnZone, ExpandDepth, ExpandTarget, GridColumn, GridMenuContext, GridSchema, GridStore,
    HighlightPolicy, LiveGridDelegate, LiveGridState, PinnedEdge, RowChange, SelectionMode,
    SortDirection, StoreOptions, TreeSpec, ViewSpec,
};

use super::{
    rows::{FleetColumn, FleetNode, Level},
    theme::{heat, status_color},
    ui,
};

/// How long a changed status shows.
const FLASH: Duration = Duration::from_millis(900);
/// The heat ranges: no tint below the first, full at the second.
const TEMP_HEAT: (f64, f64) = (28., 44.);
const CPU_HEAT: (f64, f64) = (70., 100.);
const ERRORS_HEAT: (f64, f64) = (1., 8.);
/// Inlet temperatures from this one up are bold.
const HOT: i64 = 36;
/// Regions, data centers and halls start expanded; racks collapsed.
pub const EXPAND_DEPTH: ExpandDepth = ExpandDepth::Levels(2);
/// Collapsed down to the data centers.
pub const COLLAPSED_DEPTH: ExpandDepth = ExpandDepth::Levels(1);

/// The fleet store's options: a short flash.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 4))
}

/// The fleet grid's hooks, colours from the theme only: regions and data
/// centers bold, the status a pill in its status colour with its shape
/// (`render_cell`), the hosts in alert in their status colour, heat as a
/// `base.magenta` tint. Metrics move every second, so only the status and
/// the hosts in alert flash. They take `&self` and never read the grid;
/// the styles never allocate (the pill, an element, does).
pub struct Fleet;

impl LiveGridDelegate for Fleet {
    type Row = FleetNode;

    fn cell_style(&self, cell: &CellStyleContext<'_, FleetNode>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let node = cell.row();
        let column = cell.column();
        if let Some(change) = cell.change()
            && change.change() != RowChange::Removed
            && !matches!(column, FleetColumn::Status | FleetColumn::InAlert)
        {
            style.set_background(transparent_black());
        }
        let tint = |style: &mut CellStyle, value: f64, (cool, hot): (f64, f64)| {
            if let Some(color) = heat(value, cool, hot, theme) {
                style.set_background(color);
            }
        };
        match column {
            FleetColumn::Name => match node.level {
                Level::Region | Level::Dc => style.set_font_weight(FontWeight::BOLD),
                Level::Hall => style.set_font_weight(FontWeight::SEMIBOLD),
                Level::Rack | Level::Host => {}
            },
            FleetColumn::Status => style.set_foreground(status_color(node.status, theme)),
            FleetColumn::InAlert => style.set_foreground(status_color(node.status, theme)),
            FleetColumn::Temp => {
                tint(style, node.temp as f64, TEMP_HEAT);
                if node.temp >= HOT {
                    style.set_font_weight(FontWeight::BOLD);
                }
            }
            FleetColumn::Cpu => tint(style, node.cpu as f64, CPU_HEAT),
            FleetColumn::Errors => tint(
                style,
                node.errors / f64::from(node.hosts.max(1)),
                ERRORS_HEAT,
            ),
            FleetColumn::Hosts if node.is_host() => {
                style.set_foreground(theme.muted_foreground.opacity(0.5))
            }
            FleetColumn::Hosts | FleetColumn::Mem | FleetColumn::Net | FleetColumn::Service => {
                style.set_foreground(theme.muted_foreground)
            }
            FleetColumn::Power => {}
        }
    }

    /// The status's pill.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, FleetNode>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        (cell.column() == FleetColumn::Status)
            .then(|| ui::severity_pill(cell.row().status, 1., cell.size(), cx).into_any_element())
    }

    /// "Show alerts": the console searches for the node's name.
    fn primary_command(
        &self,
        menu: &GridMenuContext<'_, FleetNode>,
        _: &App,
    ) -> Option<gpui_kit::SharedString> {
        (menu.selection().len() == 1).then(|| "Show alerts".into())
    }
}

fn column(column: FleetColumn, title: &'static str, width: f32) -> GridColumn<FleetColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

fn schema() -> GridSchema<FleetColumn> {
    GridSchema::new()
        .column(column(FleetColumn::Name, "Name", 170.).with_zone(ColumnZone::FrozenLeft))
        .column(
            column(FleetColumn::Status, "Status", 100.)
                .with_align(ColumnAlign::Start)
                .with_content(CellContent::Element),
        )
        .column(column(FleetColumn::Hosts, "Hosts", 56.))
        .column(column(FleetColumn::InAlert, "Alerting", 78.))
        .column(column(FleetColumn::Temp, "Inlet °C", 80.))
        .column(column(FleetColumn::Power, "kW", 72.))
        .column(column(FleetColumn::Cpu, "CPU %", 58.))
        .column(column(FleetColumn::Mem, "Mem %", 58.))
        .column(column(FleetColumn::Net, "Gb/s", 84.))
        .column(column(FleetColumn::Errors, "Err/s", 62.))
        .column(column(FleetColumn::Service, "Service", 92.))
}

/// The tree by name; the totals over the hosts (the leaves): how many,
/// how many in alert, the hottest and the sums.
fn view() -> ViewSpec<FleetNode> {
    ViewSpec::new()
        .with_sort([(FleetColumn::Name, SortDirection::Ascending)])
        .with_tree(TreeSpec::new().with_column(FleetColumn::Name))
        .with_aggregates([
            (FleetColumn::Hosts, Aggregate::Sum),
            (FleetColumn::InAlert, Aggregate::Sum),
            (FleetColumn::Temp, Aggregate::Max),
            (FleetColumn::Power, Aggregate::Sum),
            (FleetColumn::Net, Aggregate::Sum),
            (FleetColumn::Errors, Aggregate::Sum),
        ])
}

pub fn fleet_grid(store: GridStore<FleetNode>, cx: &mut App) -> Entity<LiveGridState<Fleet>> {
    cx.new(|cx| {
        LiveGridState::new(schema(), Fleet, store, cx)
            .with_view_id("fleet")
            .with_selection_mode(SelectionMode::RowSingle)
            .with_change_paint(ui::accent_changes(cx))
            .with_view(view())
            .with_totals(PinnedEdge::Bottom)
            .with_expand_depth(EXPAND_DEPTH)
    })
}

/// Opens the path to `node` and its children, selects it and scrolls to
/// it: the console follows an incident to its hall.
pub fn reveal(grid: &Entity<LiveGridState<Fleet>>, node: u32, cx: &mut App) {
    grid.update(cx, |grid, cx| {
        let snapshot = grid.snapshot().clone();
        let mut ancestor = Some(node);
        while let Some(id) = ancestor {
            grid.set_expanded(ExpandTarget::Row(id), true, cx);
            ancestor = snapshot.get(&id).and_then(|row| row.parent);
        }
        grid.select_key(Some(node), cx);
        grid.scroll_to_key(&node, cx);
    });
}
