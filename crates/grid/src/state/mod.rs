//! `LiveGridState`: the grid view. It owns the view settings, pulls store
//! snapshots, runs projection rebuilds in the background and plans what the
//! body element paints.

mod a11y;
// How a highlighted change paints a cell.
mod change;
// Clicks.
mod click;
mod columns;
// The column chooser.
mod chooser;
// Copy.
mod copy;
// Exporting to a stream or a file.
mod export;
// Expanding and collapsing on screen.
mod disclosure;
// Dragging rows.
mod drag;
// Editing cells.
mod edit;
// The built-in cell editors.
mod editors;
mod events;
// Removed rows fading out on screen.
mod fade;
mod frame;
// The text style of drag ghosts, which GPUI draws outside the root.
mod ghost;
// The group panel.
mod group_panel;
// The grid without its delegate type.
mod handle;
// Links in cells.
mod link;
// Grouping and expansion.
mod grouping;
// Group captions on screen.
mod groups;
mod header;
// Images in cells.
mod images;
mod layout;
mod menu;
// Commits waiting for an answer.
mod pending;
mod pinned;
mod progress;
mod projection;
mod scroll;
mod selection;
mod stats;
mod style;
// Totals and group footers.
mod summary;
mod tooltip;
// The tree on screen.
mod tree;
// Elements in cells.
mod widgets;

use std::{
    hash::{Hash, Hasher as _},
    sync::Arc,
};

use gpui_kit::base::{AutoScroll, Scrollbar, TestSupportExt as _};
use gpui_kit::component::{ActiveTheme as _, Size as ComponentSize};
use gpui_kit::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyContext, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _,
    px,
};
use web_time::Instant;

pub(crate) use self::{
    a11y::{A11yPlan, FOCUS_CELL_ID},
    tooltip::CellHover,
};
use self::{
    a11y::{RowNumbering, root_role},
    click::PendingClick,
    columns::ColumnLayout,
    frame::PaintedLine,
    ghost::GhostTextStyle,
    grouping::ExpansionRevision,
    groups::CaptionTexts,
    header::HeaderState,
    menu::OpenMenu,
    pinned::PaintedBands,
    progress::ProgressSlot,
    projection::{Coverage, HeldOrder, PendingWork},
    scroll::{GridScrollHandle, LineAnchor, ScrollReset},
    selection::{PointerTarget, Press, SelectionModel},
    summary::SummaryTexts,
    tooltip::TooltipState,
};
pub use self::{
    change::{ChangePaint, ChangePainter, ChangePalette, DirectionalChanges, UniformChanges},
    chooser::{
        ColumnChooserContext, ColumnChooserFactory, ColumnChooserPanel, PopoverColumnChooser,
    },
    columns::{
        CellContent, ColumnAlign, ColumnBand, ColumnZone, GridColumn, GridSchema, HeaderLabel,
        MIN_COLUMN_WIDTH, ViewColumnIx,
    },
    drag::{DragPayload, DropPlace, DropSource, DropTarget, RowDrag, RowDragContext},
    edit::{
        CellEditor, EditCancelReason, EditError, EditId, EditInput, EditMessageContext, EditOrigin,
        EditState, EditValidateContext, EditableContext, EditorCell, EditorField, EditorMark,
        EditorOpen, EditorStart,
    },
    editors::{
        CellEditHandle, CheckboxEditor, DateEditor, NumberEditor, NumberKind, SelectEditor,
        SelectOption, TextEditor,
    },
    events::{
        ExpandTarget, GroupOrigin, LayoutOrigin, LayoutPart, LayoutParts, LiveGridEvent,
        SelectionOrigin, SortOrigin,
    },
    export::{EmptyExport, ExportHeader, ExportJob, ExportOutcome, ExportProgress, ExportRequest},
    groups::{CaptionTag, GroupCaptionContext},
    handle::{BandInfo, ColumnInfo, GridHandleEvent, LiveGridHandle, WeakLiveGridHandle},
    images::{AssetImageSource, CellImage, CellImageSource, CellImages, ImageFit},
    link::{LinkOpener, OpenInBrowser},
    menu::{GridMenuContext, GridTarget},
    progress::ProgressContext,
    projection::FlushOutcome,
    selection::{FocusedCell, GridSelection, ScopedSelection, SelectionMode},
    stats::{GridStats, GridTuning},
    style::{
        AggregateLine, AggregateStyleContext, CellChange, CellStyle, CellStyleContext,
        LiveGridDelegate,
    },
    tooltip::{CellTooltip, CellTooltipContext, HeaderTooltipContext},
    widgets::CellElementContext,
};
use crate::{
    actions::{CELL_CONTENT_KEY, KEY_CONTEXT},
    element::GridBody,
    engine::{GroupPath, Projection, ProjectionEngine, Rebuild, ViewSpec},
    model::{ColumnIx, ColumnSet, GridColumns as _, GridRow},
    policy::{
        AutofitPolicy, BackgroundRunner, BandedMove, ColumnMovePolicy, ContextMenuGesture,
        ExpandGesture, FocusCaptions, LiveReorder, ProjectionScope, ReorderPolicy,
        SelectionGesture, SelectionPolicy, SelectionScope, SortCycle, StandardContextMenuGesture,
        StandardExpandGesture, StandardSelectionGesture, StandardSortCycle, TaskRunner,
        VisibleRowsAutofit,
    },
    quick_filter::{ContainsAllTerms, QuickFilterMatcher},
    store::{GridStore, GridWriter, PinnedEdge, StoreOptions, StoreSnapshot},
    text::{TextCaches, TextStats},
};

type ColumnOf<D> = <<D as LiveGridDelegate>::Row as GridRow>::Column;
type KeyOf<D> = <<D as LiveGridDelegate>::Row as GridRow>::Key;

const CELL_PADDING: Pixels = px(6.);

/// The hash of a row key: the integer of element ids made from a row's
/// identity, such as the tooltip div of a hovered cell
/// (`NamedInteger(column id, key_hash)`) and the accessibility row of a
/// data row (`NamedInteger("live-grid-row", key_hash)`). A pinned row's key
/// is of its band's key space: `pinned::pinned_key_hash` hashes the band
/// with it. `FxHasher`: fast, and stable within the process, which is all
/// an element id needs.
pub(crate) fn key_hash<K: Hash + ?Sized>(key: &K) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    key.hash(&mut hasher);
    hasher.finish()
}

/// The hash of a group's path in element ids: a caption's accessibility row
/// (`NamedInteger("live-grid-group-row", ..)`), its tooltip divs and its
/// focus. Tagged, so it never equals a data row's `key_hash`, as
/// `pinned_key_hash` is.
pub(crate) fn group_hash(path: &GroupPath) -> u64 {
    key_hash(&("live-grid-group", path))
}

/// The hash of a group footer's path in element ids: its accessibility row
/// (`NamedInteger("live-grid-group-footer-row", ..)`) and its tooltip divs.
pub(crate) fn footer_hash(path: &GroupPath) -> u64 {
    key_hash(&("live-grid-group-footer", path))
}

/// The hash of the totals row of a band in element ids: its accessibility
/// row (`NamedInteger("live-grid-totals-row", ..)`) and its tooltip divs.
pub(crate) fn totals_hash(edge: PinnedEdge) -> u64 {
    key_hash(&(
        "live-grid-totals",
        match edge {
            PinnedEdge::Top => "top",
            PinnedEdge::Bottom => "bottom",
        },
    ))
}

/// The state and view of a live grid.
pub struct LiveGridState<D: LiveGridDelegate> {
    delegate: D,
    store: GridStore<D::Row>,
    snapshot: Arc<StoreSnapshot<D::Row>>,
    projection: Arc<Projection>,
    view: ViewSpec<D::Row>,
    engine: Arc<dyn ProjectionEngine<D::Row>>,
    tuning: GridTuning,
    pending: PendingWork,
    rebuild_task: Option<Task<()>>,
    rebuild_timer: Option<Task<()>>,
    last_rebuild_started: Option<Instant>,
    /// The pending work comes from a user action (a new view, a new data
    /// set): its result skips the reorder hold.
    pending_user_action: bool,
    /// The pending work comes from a new view (sort, filter, quick filter,
    /// grouping): its result is shown from the top, or with a painted
    /// focused line on its screen line.
    pending_scroll_reset: ScrollReset,
    /// Counts the edits of the view's expansion.
    expansion_revision: ExpansionRevision,
    /// The expansion the shown projection's lines are laid out for.
    shown_revision: ExpansionRevision,
    /// Values aggregates read changed since the last job that recounted
    /// them; a held order does not hold them.
    aggregates_changed: bool,
    /// The caption or tree row the user toggled last: it keeps its screen
    /// line when the lines of the new expansion are shown.
    expand_anchor: Option<LineAnchor>,
    /// How a press expands or collapses a caption or a tree row.
    expand_gesture: Box<dyn ExpandGesture>,
    /// The texts of captions and aggregates, by locale.
    caption_texts: CaptionTexts,
    /// The band of the totals row, if shown, and its label by locale.
    totals: Option<PinnedEdge>,
    summary_texts: SummaryTexts,
    reorder: Box<dyn ReorderPolicy>,
    runner: Box<dyn TaskRunner>,
    sort_cycle: Box<dyn SortCycle>,
    quick_filter_matcher: Arc<dyn QuickFilterMatcher>,
    /// The columns the quick filter searches; `None`: the visible ones.
    quick_filter_columns: Option<ColumnSet>,
    /// The quick filter text, trimmed; empty when the filter is off.
    quick_filter_text: SharedString,
    held: Option<HeldOrder>,
    pointer_inside: bool,
    /// The store version of the latest removal: a projection built before it
    /// may still hold removed rows.
    last_removal_version: u64,
    columns: ColumnLayout<ColumnOf<D>>,
    /// Saved layouts: view id, codec, merge, schema defaults.
    layout: layout::LayoutSlot<D::Row>,
    column_move: Box<dyn ColumnMovePolicy>,
    autofit: Box<dyn AutofitPolicy>,
    /// Columns to autofit in the next painted frame, and who asked.
    pending_autofit: Option<(ColumnSet, LayoutOrigin)>,
    /// The column that column commands act on.
    current_column: Option<ColumnIx>,
    header: HeaderState,
    scroll_row: usize,
    scroll_offset: Pixels,
    scroll_x: Pixels,
    row_height: Pixels,
    body_bounds: Bounds<Pixels>,
    /// What the last frame painted, one entry per line of the data rows:
    /// hit tests use what is on screen, not the projection, which may still
    /// hold removed rows.
    painted: Vec<PaintedLine>,
    /// The pinned rows painted in the last frame, band by band.
    painted_pinned: PaintedBands<D::Row>,
    /// The coverage of the order shown or held, and the flushes waiting
    /// for it.
    coverage: Coverage,
    /// The view's progress and the store's progress as shown.
    progress: ProgressSlot,
    /// The scroll position as the scrollbars see it.
    scroll_handle: GridScrollHandle,
    /// The selection, the focused cell and the anchor.
    selection: SelectionModel<KeyOf<D>, ColumnOf<D>>,
    selection_scope: Box<dyn SelectionScope<D::Row>>,
    /// Which lines take the selection; what a command over a line acts on.
    selection_policy: Box<dyn SelectionPolicy<D::Row>>,
    selection_gesture: Box<dyn SelectionGesture>,
    context_menu_gesture: Box<dyn ContextMenuGesture>,
    /// What the last mouse-down on the body hit: the context menu's target.
    pointer_target: Option<PointerTarget<KeyOf<D>>>,
    /// The primary-button press in progress.
    press: Option<Press<KeyOf<D>, ColumnOf<D>>>,
    /// The row of the first press of a click series: a double click
    /// confirms it.
    press_series: Option<KeyOf<D>>,
    /// The press on the grid that ends in a `Click` if it is released
    /// without a drag.
    click: Option<PendingClick<KeyOf<D>, ColumnOf<D>>>,
    /// Scrolls the rows while a drag-selection is above or below them.
    drag_scroll: AutoScroll,
    /// The open context menu.
    menu: Option<OpenMenu<KeyOf<D>, ColumnOf<D>>>,
    /// The pointer over the body and the header, and the hovered cell's
    /// tooltip.
    tooltip: TooltipState,
    /// The last frame showed keyboard focus (focus-visible).
    #[cfg(any(test, feature = "test-support"))]
    focus_visible: bool,
    /// The grid's accessible name, from `LiveGrid::accessibility_label`.
    accessibility_label: Option<SharedString>,
    /// Plan the accessibility tree while the window's accessibility is off
    /// ([`Self::set_a11y_forced`]).
    #[cfg(any(test, feature = "test-support"))]
    a11y_forced: bool,
    /// The accessibility plan of the last frame, for tests.
    #[cfg(any(test, feature = "test-support"))]
    last_a11y: Option<std::rc::Rc<A11yPlan>>,
    text_cache: TextCaches,
    /// How highlighted changes paint cells.
    change_paint: ChangePaint,
    /// A test's start of cells: the column, the indent and an icon.
    #[cfg(any(test, feature = "test-support"))]
    test_cell_lead: Option<(ColumnIx, Pixels, Option<SharedString>)>,
    /// A test's cell host on every cell of a column.
    #[cfg(any(test, feature = "test-support"))]
    test_cell_host: Option<frame::TestCellHost>,
    /// What the last frame painted, in order.
    #[cfg(any(test, feature = "test-support"))]
    last_paint_order: Vec<&'static str>,
    /// The icons the body painted in the last frame.
    #[cfg(any(test, feature = "test-support"))]
    painted_icons: Vec<(Bounds<Pixels>, SharedString, gpui_kit::Hsla)>,
    /// The images the body painted in the last frame.
    #[cfg(any(test, feature = "test-support"))]
    painted_images: Vec<frame::PaintedImage>,
    /// The caption tags the body painted in the last frame.
    #[cfg(any(test, feature = "test-support"))]
    painted_pills: Vec<(Bounds<Pixels>, gpui_kit::Hsla)>,
    fade: Option<(u32, Task<()>)>,
    stats: GridStats,
    focus_handle: FocusHandle,
    // Copy: the exporters and the copy being written.
    copy: copy::CopySlot<D::Row>,
    // The column chooser.
    column_chooser: chooser::ChooserSlot,
    // The group panel.
    group_panel: group_panel::GroupPanelState,
    // Editing: the editors and edit seams, the open edit, the commits
    // waiting, and the elements in cells.
    edit_setup: edit::EditSetup<D::Row>,
    edit: edit::EditSlot<KeyOf<D>, ColumnOf<D>>,
    pending_edits: pending::PendingEdits<KeyOf<D>, ColumnOf<D>>,
    widgets: widgets::WidgetSlot<KeyOf<D>, ColumnOf<D>>,
    links: link::LinkSlot,
    // Where the images of image cells come from.
    images: images::CellImages,
    // Row drags.
    drag: drag::DragSlot<D>,
    _pull: Task<()>,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// A grid over `store`. Several grids may share one store.
    pub fn new(
        schema: GridSchema<ColumnOf<D>>,
        delegate: D,
        store: GridStore<D::Row>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = store.subscribe();
        let snapshot = store.snapshot();
        let pull = cx.spawn(async move |this, cx| {
            while subscription.changed().await {
                let (snapshot, changes) = subscription.take();
                let Ok(interval) = this.update(cx, |state, cx| {
                    state.apply_snapshot(snapshot, changes, cx);
                    state.tuning.pull_interval
                }) else {
                    break;
                };
                cx.background_executor().timer(interval).await;
            }
        });
        Self {
            delegate,
            progress: ProgressSlot::new(snapshot.progress()),
            snapshot,
            store,
            projection: Arc::new(Projection::default()),
            view: ViewSpec::new(),
            engine: Arc::new(Rebuild::default()),
            tuning: GridTuning::default(),
            pending: PendingWork::Rebuild { expand: false },
            rebuild_task: None,
            rebuild_timer: None,
            last_rebuild_started: None,
            pending_user_action: true,
            pending_scroll_reset: ScrollReset::Keep,
            expansion_revision: ExpansionRevision::default(),
            shown_revision: ExpansionRevision::default(),
            aggregates_changed: false,
            expand_anchor: None,
            expand_gesture: Box::new(StandardExpandGesture),
            caption_texts: CaptionTexts::default(),
            totals: None,
            summary_texts: SummaryTexts::default(),
            reorder: Box::new(LiveReorder),
            runner: Box::new(BackgroundRunner),
            sort_cycle: Box::new(StandardSortCycle),
            quick_filter_matcher: Arc::new(ContainsAllTerms),
            quick_filter_columns: None,
            quick_filter_text: SharedString::default(),
            held: None,
            pointer_inside: false,
            last_removal_version: 0,
            // Reads the schema before `columns` takes it.
            layout: layout::LayoutSlot::new(&schema),
            columns: {
                let mut columns = ColumnLayout::new(schema);
                columns.normalize(&BandedMove);
                columns
            },
            column_move: Box::new(BandedMove),
            autofit: Box::new(VisibleRowsAutofit),
            pending_autofit: None,
            current_column: None,
            header: HeaderState::default(),
            scroll_row: 0,
            scroll_offset: px(0.),
            scroll_x: px(0.),
            // gpui-kit default size (md); `LiveGrid` sets the size it is given.
            row_height: ComponentSize::default().table_row_height(),
            body_bounds: Bounds::default(),
            painted: Vec::new(),
            painted_pinned: PaintedBands::default(),
            coverage: Coverage::default(),
            scroll_handle: GridScrollHandle::default(),
            selection: SelectionModel::new(SelectionMode::default()),
            selection_scope: Box::new(ProjectionScope),
            selection_policy: Box::new(FocusCaptions),
            selection_gesture: Box::new(StandardSelectionGesture),
            context_menu_gesture: Box::new(StandardContextMenuGesture),
            pointer_target: None,
            press: None,
            press_series: None,
            click: None,
            drag_scroll: AutoScroll::default(),
            menu: None,
            tooltip: TooltipState::default(),
            #[cfg(any(test, feature = "test-support"))]
            focus_visible: false,
            accessibility_label: None,
            #[cfg(any(test, feature = "test-support"))]
            a11y_forced: false,
            #[cfg(any(test, feature = "test-support"))]
            last_a11y: None,
            text_cache: TextCaches::default(),
            change_paint: ChangePaint::default(),
            #[cfg(any(test, feature = "test-support"))]
            test_cell_lead: None,
            #[cfg(any(test, feature = "test-support"))]
            test_cell_host: None,
            #[cfg(any(test, feature = "test-support"))]
            last_paint_order: Vec::new(),
            #[cfg(any(test, feature = "test-support"))]
            painted_icons: Vec::new(),
            #[cfg(any(test, feature = "test-support"))]
            painted_images: Vec::new(),
            #[cfg(any(test, feature = "test-support"))]
            painted_pills: Vec::new(),
            fade: None,
            stats: GridStats::default(),
            // The grid is one Tab stop; Tab leaves it.
            focus_handle: cx.focus_handle().tab_stop(true),
            copy: copy::CopySlot::default(),
            column_chooser: chooser::ChooserSlot::default(),
            group_panel: group_panel::GroupPanelState::default(),
            edit_setup: edit::EditSetup::default(),
            edit: edit::EditSlot::default(),
            pending_edits: pending::PendingEdits::default(),
            widgets: widgets::WidgetSlot::default(),
            links: link::LinkSlot::default(),
            images: images::CellImages::default(),
            drag: drag::DragSlot::default(),
            _pull: pull,
        }
    }

    /// A grid with its own store.
    pub fn new_standalone(
        schema: GridSchema<ColumnOf<D>>,
        delegate: D,
        options: StoreOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = GridStore::new(options, cx);
        Self::new(schema, delegate, store, cx)
    }

    /// Sets the view (filter, sort, grouping or tree, aggregates). The grid
    /// owns the quick filter and the expansion: those of `view` are replaced
    /// by the grid's own ([`Self::set_quick_filter`],
    /// [`Self::with_expand_depth`]). The sort and the grouping of a layout
    /// given to [`Self::with_layout`], before or after, replace those of
    /// `view`.
    pub fn with_view(mut self, view: ViewSpec<D::Row>) -> Self {
        let quick_filter = self.view.quick_filter().cloned();
        let expansion = self.view.expansion().clone();
        self.view = view
            .with_quick_filter(quick_filter)
            .with_expansion(expansion);
        // The default layout keeps this sort and grouping.
        self.layout.set_view(&self.view);
        self.apply_initial_layout()
    }

    /// Sets the projection engine (default [`Rebuild`]).
    pub fn with_engine(mut self, engine: impl ProjectionEngine<D::Row>) -> Self {
        self.engine = Arc::new(engine);
        self
    }

    /// Sets the timing knobs.
    pub fn with_tuning(mut self, tuning: GridTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// Sets when a new display order replaces the shown one (default
    /// [`LiveReorder`]; see also `HoldReorder` and `ManualReorder`).
    pub fn with_reorder_policy(mut self, policy: impl ReorderPolicy) -> Self {
        self.reorder = Box::new(policy);
        self
    }

    /// Sets where background jobs run (default [`BackgroundRunner`]).
    pub fn with_task_runner(mut self, runner: impl TaskRunner) -> Self {
        self.runner = Box::new(runner);
        self
    }

    /// Sets how a header click changes the sort (default
    /// [`StandardSortCycle`]).
    pub fn with_sort_cycle(mut self, cycle: impl SortCycle) -> Self {
        self.sort_cycle = Box::new(cycle);
        self
    }

    /// Sets how quick filter text matches rows (default
    /// [`ContainsAllTerms`]).
    pub fn with_quick_filter_matcher(mut self, matcher: impl QuickFilterMatcher) -> Self {
        self.quick_filter_matcher = Arc::new(matcher);
        self.recompile_quick_filter();
        self
    }

    /// Sets the columns the quick filter searches (default: the visible
    /// columns, frozen and scrolled out of view included). Hiding a column
    /// does not change an explicit set.
    pub fn with_quick_filter_columns(
        mut self,
        columns: impl IntoIterator<Item = ColumnOf<D>>,
    ) -> Self {
        let set = columns
            .into_iter()
            .fold(ColumnSet::EMPTY, |mut set, column| {
                set.insert(column.ix());
                set
            });
        self.quick_filter_columns = Some(set);
        self.recompile_quick_filter();
        self
    }

    /// Sets the row height (from the [`crate::LiveGrid`] size).
    pub(crate) fn set_row_height(&mut self, row_height: Pixels) {
        if self.row_height != row_height {
            self.row_height = row_height;
            self.clamp_scroll();
        }
    }

    /// Sets the accessible name (from the [`crate::LiveGrid`] label).
    pub(crate) fn set_accessibility_label(&mut self, label: Option<SharedString>) {
        self.accessibility_label = label;
    }

    /// Plans the accessibility tree even when the window's accessibility is
    /// off, so headless tests can check the planned elements: the frame
    /// plans the rows and the focused cell, prepaints their elements and
    /// builds the nodes GPUI would publish (it publishes none: the window's
    /// accessibility is off). Takes effect with the next frame.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_a11y_forced(&mut self, forced: bool) {
        self.a11y_forced = forced;
    }

    /// The description the last planned accessibility tree gives the
    /// focused cell: the column title, then the expansion state, the trend
    /// ("rising", "falling") and the tooltip text. `None` without a plan or
    /// a painted focused cell. For tests, with [`Self::set_a11y_forced`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn a11y_focus_description(&self) -> Option<&str> {
        self.last_a11y.as_ref()?.focus_description()
    }

    /// Gives every cell of `column` in the data rows a start, as group
    /// captions and tree rows have: the text begins `indent` further in
    /// with that much less room, and `icon` (an icon path) sits in the
    /// indent's last 20 px. For tests of the frame. Takes effect with the
    /// next frame.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_cell_lead(
        &mut self,
        column: ColumnOf<D>,
        indent: Pixels,
        icon: Option<SharedString>,
    ) {
        self.test_cell_lead = Some((column.ix(), indent, icon));
    }

    /// Gives every cell of `column` in the painted data and pinned rows a
    /// cell host: a real element over the cell, after a tree row's indent,
    /// made by `build` from the host's div, whose id is
    /// `NamedChild("live-grid-cell", column id)` in the id scope of the row's
    /// element (`NamedInteger("live-grid-row", key hash)`). The body paints
    /// no text in those cells. For tests of the cell host mechanism; `None`
    /// removes it. Takes effect with the next frame.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_cell_host(
        &mut self,
        column: Option<ColumnOf<D>>,
        build: impl Fn(gpui_kit::Stateful<gpui_kit::Div>) -> gpui_kit::Stateful<gpui_kit::Div> + 'static,
    ) {
        self.test_cell_host = column.map(|column| {
            let build: std::rc::Rc<
                dyn Fn(gpui_kit::Stateful<gpui_kit::Div>) -> gpui_kit::Stateful<gpui_kit::Div>,
            > = std::rc::Rc::new(build);
            (column.ix(), build)
        });
    }

    /// What the body painted in the last frame, in order: `regions` (the
    /// cells' backgrounds, lines, text and icons), `dividers`, `tooltip`
    /// (the hovered cell's div), `rows` (row elements: accessibility rows
    /// and cell hosts) and `outlines` (the focused cell's), each when there
    /// was one. For tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn last_paint_order(&self) -> &[&'static str] {
        &self.last_paint_order
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn set_last_paint_order(&mut self, order: Vec<&'static str>) {
        self.last_paint_order = order;
    }

    /// Records how long the last frame's row elements took to lay out and
    /// prepaint, when it had cell hosts.
    pub(crate) fn set_last_hosts_prepaint(&mut self, elapsed: std::time::Duration) {
        self.stats.last_hosts_prepaint = elapsed;
    }

    /// Whether this frame builds the accessibility tree of the body: only
    /// while a screen reader or another client is listening (or forced in
    /// tests), so the grid costs nothing for accessibility otherwise.
    fn is_a11y_planned(&self, window: &Window) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if self.a11y_forced {
            return true;
        }
        window.is_a11y_active()
    }

    /// The root's key context: [`KEY_CONTEXT`], with
    /// [`CELL_CONTENT_KEY`] set to what the focused data cell's column
    /// paints, so a key binding can tell a control from text.
    fn root_key_context(&self) -> KeyContext {
        let mut context = KeyContext::default();
        context.add(KEY_CONTEXT);
        let content = self
            .selection
            .focus()
            .filter(|focus| focus.row().is_some())
            .map(|focus| self.columns.column(focus.column().ix()).content);
        if self.is_link_focused() {
            context.set(CELL_CONTENT_KEY, "link");
        } else if let Some(content) = content {
            context.set(CELL_CONTENT_KEY, content.key_value());
        }
        context
    }

    /// How the accessibility tree numbers rows now: the header, the top
    /// band, the rows of the shown projection, the bottom band, with
    /// the totals row as the outermost row of its band.
    fn a11y_numbering(&self) -> RowNumbering {
        RowNumbering::new(
            self.snapshot.pinned_rows(PinnedEdge::Top).len(),
            self.projection.len(),
            self.snapshot.pinned_rows(PinnedEdge::Bottom).len(),
        )
        .with_totals(self.totals)
    }

    /// The store.
    pub fn store(&self) -> &GridStore<D::Row> {
        &self.store
    }

    /// A writer for producers on any thread.
    pub fn writer(&self) -> GridWriter<D::Row> {
        self.store.writer()
    }

    /// The snapshot currently shown.
    pub fn snapshot(&self) -> &Arc<StoreSnapshot<D::Row>> {
        &self.snapshot
    }

    /// The display order currently shown.
    pub fn projection(&self) -> &Arc<Projection> {
        &self.projection
    }

    /// Profiling counters.
    pub fn stats(&self) -> GridStats {
        // The cycles of the tree shown.
        self.stats.with_tree_cycles(self.projection.tree_cycles())
    }

    /// Text cache counters.
    pub fn text_stats(&self) -> TextStats {
        self.text_cache.stats()
    }
}

impl<D: LiveGridDelegate> EventEmitter<LiveGridEvent<KeyOf<D>, ColumnOf<D>>> for LiveGridState<D> {}

impl<D: LiveGridDelegate> Focusable for LiveGridState<D> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<D: LiveGridDelegate> Render for LiveGridState<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.close_stale_menu(window, cx);
        // A row drag whose release the grid did not see.
        self.settle_row_drag(cx);
        self.apply_scroll_request();
        self.ensure_projection(cx);
        let a11y = self.is_a11y_planned(window);
        let header_tooltip = self.header_tooltip(window, cx);
        // The drag ghosts' text style: the one the grid inherits here.
        let ghost_text = GhostTextStyle::capture(window);
        let header = self.render_header(header_tooltip, &ghost_text, a11y, cx);
        // The group panel, if `LiveGrid` shows it.
        let group_panel = self.render_group_panel(window, cx);
        let scrollbars = self.render_scrollbars();
        let progress = self.render_progress_overlay(window, cx);
        let menu = self.render_menu(window);
        // The column chooser's layer, as the menu's.
        let column_chooser = self.render_column_chooser(window, cx);
        // The root is the `Grid` node, a `TreeGrid` for a grouped view or a
        // tree, and keeps the focus; the focused cell claims the active
        // descendant under it. The counts are of the whole grid, not of what
        // is painted.
        let role = root_role(self.projection.is_grouped() || self.projection.is_tree());
        let row_count = self.a11y_numbering().row_count();
        let column_count = self.columns.visible_set().len();
        let multiselectable = !self.selection.mode().is_single();
        let theme = cx.theme();
        let root = div()
            .id("live-grid")
            .test_support()
            .track_focus(&self.focus_handle)
            .role(role)
            .when_some(self.accessibility_label.clone(), |this, label| {
                this.aria_label(label)
            })
            .aria_row_count(row_count)
            .aria_column_count(column_count)
            // A div has no `aria_*` for it: set on the node itself, and only
            // while the tree is built (the closure is a boxed allocation).
            .when(a11y && multiselectable, |this| {
                this.a11y_synthetic_children(|builder| builder.parent_node().set_multiselectable())
            })
            .key_context(self.root_key_context())
            .on_action(cx.listener(Self::on_autofit_columns))
            .on_action(cx.listener(Self::on_autofit_column))
            .on_action(cx.listener(Self::on_hide_column))
            .on_action(cx.listener(Self::on_show_all_columns))
            .on_action(cx.listener(Self::on_freeze_column_left))
            .on_action(cx.listener(Self::on_freeze_column_right))
            .on_action(cx.listener(Self::on_unfreeze_column))
            .on_action(cx.listener(Self::on_move_column_left))
            .on_action(cx.listener(Self::on_move_column_right))
            .on_action(cx.listener(Self::on_reset_layout))
            .on_action(cx.listener(Self::on_show_context_menu))
            .on_action(cx.listener(Self::on_show_column_chooser))
            .on_action(cx.listener(Self::on_sort_ascending))
            .on_action(cx.listener(Self::on_sort_descending))
            .on_action(cx.listener(Self::on_clear_sort))
            .on_action(cx.listener(Self::on_clear_all_sorting))
            .map(|this| Self::bind_selection_actions(this, cx))
            // Expanding and collapsing.
            .map(|this| Self::bind_disclosure_actions(this, cx))
            // Copy.
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_copy_all))
            // The grouping Actions and chip drags.
            .map(|this| Self::bind_group_panel(this, cx))
            // Editing and elements in cells.
            .map(|this| Self::bind_edit_actions(this, cx))
            .map(|this| Self::bind_cell_actions(this, cx))
            .map(|this| Self::bind_link_actions(this, cx))
            // Header drags drop anywhere on the grid, header or body.
            .on_drag_move(cx.listener(Self::on_columns_drag_move))
            .on_drag_move(cx.listener(Self::on_resize_drag_move))
            .on_drop(cx.listener(Self::on_columns_drop))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.table)
            .text_color(theme.foreground)
            // The group panel, above the header.
            .children(group_panel)
            .child(header)
            // The rows' wrapper, the source of row drags.
            .child(self.render_rows(GridBody::new(cx.entity()), ghost_text, cx))
            .child(scrollbars)
            // Over the body, under the header; no hitbox of its own.
            .children(progress);
        // The menu layer is a sibling of the grid root, outside its key
        // context: keys pressed in the menu (Space, Page Down, arrows a menu
        // passes on) must not move the grid's selection. Its items dispatch
        // to the grid's focus all the same (`action_context`).
        div()
            .size_full()
            .child(root)
            .children(menu)
            .children(column_chooser)
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// The scrollbars over the data rows (`gpui_kit::base::Scrollbar`, as
    /// gpui-kit's `DataTable`): vertical at their right edge, between the
    /// pinned bands, which do not scroll; horizontal at their bottom edge,
    /// under the scrolling zone. An overlay scrollbar takes the presses on
    /// it, so the bands stay clear of both and a pinned row is clickable
    /// across its height. Rendered by this view, so a thumb drag repaints
    /// it.
    fn render_scrollbars(&self) -> impl IntoElement {
        let geometry = self
            .columns
            .geometry(self.body_bounds.size.width, self.scroll_x);
        let scrolling = geometry.zone(ColumnZone::Scrolling);
        let (left, width) = (scrolling.x, scrolling.width);
        let bands = self.body_bands();
        let top_band = bands.band(PinnedEdge::Top).size.height;
        let bottom_band = bands.band(PinnedEdge::Bottom).size.height;
        div()
            .absolute()
            // Below the group panel and header.
            .top(self.body_top())
            .left_0()
            .right_0()
            .bottom_0()
            .child(
                div()
                    .absolute()
                    .top(top_band)
                    .right_0()
                    .bottom(bottom_band)
                    .w(Scrollbar::width())
                    .child(
                        Scrollbar::vertical(&self.scroll_handle)
                            .viewport_from_layout()
                            .max_fps(60),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(left)
                    .bottom(bottom_band)
                    .w(width)
                    .h(Scrollbar::width())
                    .child(Scrollbar::horizontal(&self.scroll_handle).viewport_from_layout()),
            )
    }
}
