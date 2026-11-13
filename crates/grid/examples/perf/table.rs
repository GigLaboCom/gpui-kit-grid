//! The process grid: its columns, its four arrangements (a tree by parent
//! process, a flat list, grouped by user, grouped by app) and its hooks.
//! CPU and GPU are heat-coloured; the accent tints a new process whole and
//! a changed cell only when the change is notable (`sampler.rs`).

use std::{sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, AppContext as _, Entity, FontWeight, px, transparent_black};
use gpui_kit_grid::{
    Aggregate, AggregateOrder, CellStyle, CellStyleContext, ColumnAlign, ColumnZone, ExpandDepth,
    GridColumn, GridSchema, GridStore, GroupBy, HighlightPolicy, HoldReorder, LiveGridDelegate,
    LiveGridState, NumberFormat, RowChange, SelectionMode, SortDirection, StoreOptions, TreeSpec,
    TreeTotals, ViewSpec,
};

use super::{
    rows::{ByteSum, PerfColumn, ProcRow},
    theme::heat,
    ui,
};

/// How long a notable change shows; a little under the default interval,
/// so a value that keeps moving does not stay tinted.
const FLASH: Duration = Duration::from_millis(900);
/// How long an exited process fades out.
const REMOVAL_FADE: Duration = Duration::from_millis(2_500);
/// The heat ranges: no tint up to the first, full at the second. CPU is
/// % of one core, so a busy process passes 100.
const CPU_HEAT: (f64, f64) = (15., 100.);
const GPU_HEAT: (f64, f64) = (3., 60.);
/// How long the order holds while the pointer rests on the grid.
pub const HOLD: Duration = Duration::from_secs(4);

/// The store's options: short flashes, exited processes fading out.
pub fn store_options() -> StoreOptions {
    StoreOptions::default()
        .with_highlight(HighlightPolicy::new(FLASH, 4).with_removal_fade(REMOVAL_FADE))
}

/// How the grid arranges the processes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrangement {
    /// A tree by parent process.
    #[default]
    Tree,
    Flat,
    /// Grouped by user, the busiest user first.
    User,
    /// Grouped by app (the bundle, or the process name), the busiest first.
    App,
}

impl Arrangement {
    pub const ALL: [Self; 4] = [Self::Tree, Self::Flat, Self::User, Self::App];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::Flat => "Flat",
            Self::User => "By user",
            Self::App => "By app",
        }
    }

    /// The column the captions name, hidden while grouped by it.
    pub fn group_column(self) -> Option<PerfColumn> {
        match self {
            Self::User => Some(PerfColumn::User),
            Self::App => Some(PerfColumn::App),
            Self::Tree | Self::Flat => None,
        }
    }
}

/// The order unless the user sorts: the busiest first.
pub const SORT: [(PerfColumn, SortDirection); 1] = [(PerfColumn::Cpu, SortDirection::Descending)];

/// The view of `arrangement` with `sort`. Groups sum CPU, GPU, memory,
/// disk and threads; a tree has no roll-ups (a parent shows its own use).
pub fn view(arrangement: Arrangement, sort: Vec<(PerfColumn, SortDirection)>) -> ViewSpec<ProcRow> {
    let view = ViewSpec::new().with_sort(sort);
    match arrangement {
        Arrangement::Tree => view.with_tree(
            TreeSpec::new()
                .with_column(PerfColumn::Name)
                .with_totals(TreeTotals::AllRows),
        ),
        Arrangement::Flat => view,
        Arrangement::User | Arrangement::App => {
            let column = arrangement.group_column().unwrap_or(PerfColumn::User);
            let bytes = Arc::new(ByteSum);
            view.with_group_by([GroupBy::column(column)])
                .with_group_order(AggregateOrder::new(
                    PerfColumn::Cpu,
                    SortDirection::Descending,
                ))
                .with_aggregates([
                    (PerfColumn::Cpu, Aggregate::Sum),
                    (PerfColumn::Gpu, Aggregate::Sum),
                    (PerfColumn::GpuMemory, Aggregate::Custom(bytes.clone())),
                    (PerfColumn::Memory, Aggregate::Custom(bytes.clone())),
                    (PerfColumn::DiskRead, Aggregate::Custom(bytes.clone())),
                    (PerfColumn::DiskWrite, Aggregate::Custom(bytes)),
                    (PerfColumn::Threads, Aggregate::Sum),
                ])
        }
    }
}

/// The grid's hooks, colours from the theme only: the machine's rows bold,
/// heat as a `base.magenta` tint, ids and times muted, and the change tint
/// taken off a cell whose change is not notable. They take `&self`, never
/// read the grid and never allocate.
pub struct Processes;

impl LiveGridDelegate for Processes {
    type Row = ProcRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, ProcRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        let column = cell.column();
        if let Some(change) = cell.change()
            && change.change() == RowChange::Updated
            && !row.notable.contains(gpui_kit_grid::GridColumns::ix(column))
        {
            style.set_background(transparent_black());
        }
        let tint = |style: &mut CellStyle, value: Option<f64>, (cool, hot): (f64, f64)| {
            if let Some(color) = value.and_then(|value| heat(value, cool, hot, theme)) {
                style.set_background(color);
            }
        };
        match column {
            PerfColumn::Name if row.is_machine() => style.set_font_weight(FontWeight::BOLD),
            PerfColumn::Cpu => tint(style, row.cpu, CPU_HEAT),
            PerfColumn::Gpu => tint(style, row.gpu, GPU_HEAT),
            PerfColumn::Pid | PerfColumn::Threads | PerfColumn::User | PerfColumn::Uptime => {
                style.set_foreground(theme.muted_foreground)
            }
            PerfColumn::Name
            | PerfColumn::GpuMemory
            | PerfColumn::Memory
            | PerfColumn::DiskRead
            | PerfColumn::DiskWrite
            | PerfColumn::App => {}
        }
    }
}

fn column(column: PerfColumn, title: &'static str, width: f32) -> GridColumn<PerfColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// One decimal for the percentages.
const PERCENT: NumberFormat = NumberFormat::new().with_decimals(1);

fn schema() -> GridSchema<PerfColumn> {
    GridSchema::new()
        .column(column(PerfColumn::Name, "Name", 300.).with_zone(ColumnZone::FrozenLeft))
        .column(column(PerfColumn::Pid, "PID", 64.))
        .column(column(PerfColumn::Cpu, "CPU %", 70.).with_number_format(PERCENT))
        .column(column(PerfColumn::Gpu, "GPU %", 70.).with_number_format(PERCENT))
        // Per process only where the GPU source gives it (Linux fdinfo).
        .column(
            column(PerfColumn::GpuMemory, "GPU mem", 92.)
                .with_align(ColumnAlign::End)
                .with_visible(cfg!(target_os = "linux")),
        )
        .column(column(PerfColumn::Memory, "Memory", 92.).with_align(ColumnAlign::End))
        .column(column(PerfColumn::DiskRead, "Read/s", 92.).with_align(ColumnAlign::End))
        .column(column(PerfColumn::DiskWrite, "Write/s", 92.).with_align(ColumnAlign::End))
        .column(column(PerfColumn::Threads, "Threads", 70.))
        .column(column(PerfColumn::User, "User", 120.))
        .column(column(PerfColumn::Uptime, "Uptime", 84.).with_align(ColumnAlign::End))
        .column(column(PerfColumn::App, "App", 200.))
}

pub fn process_grid(store: GridStore<ProcRow>, cx: &mut App) -> Entity<LiveGridState<Processes>> {
    cx.new(|cx| {
        LiveGridState::new(schema(), Processes, store, cx)
            .with_view_id("processes")
            .with_selection_mode(SelectionMode::RowSingle)
            .with_change_paint(ui::accent_changes(cx))
            .with_reorder_policy(HoldReorder::new(HOLD))
            .with_quick_filter_columns([PerfColumn::Name, PerfColumn::User, PerfColumn::App])
            .with_view(view(Arrangement::default(), SORT.to_vec()))
            .with_expand_depth(ExpandDepth::All)
    })
}
