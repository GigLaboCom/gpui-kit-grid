//! Keeping the display order in step with the store: the view (sort, filter,
//! quick filter, grouping, expansion), snapshot pulls, throttled background
//! jobs, the reorder hold, and what removals and new orders do to the
//! selection, the focus and the scroll position.
//!
//! The pending work is a set: a rebuild takes every other work; otherwise
//! one job runs `compact` → `aggregate` → `expand` over the shown
//! projection. Data-driven work waits for the `min_rebuild_interval`
//! throttle, and only a data-driven rebuild meets the reorder policy; a
//! change of the expansion is a user action and is laid out at once, ahead
//! of a data-driven rebuild that still waits. One job runs at a time.
//!
//! Every job that gives a projection stamps what changed in its summaries
//! against the projection before it: a rebuild against the held order,
//! else the shown one, unless it is for a new view or data set; a refresh
//! against the projection it ran over. The stamps are carried in the job,
//! off the UI thread (`Projection::carry_summary_stamps`).
//!
//! The coverage is the newest store version up to which every change is
//! reflected in the order shown or held. A pulled snapshot whose changes
//! need no work is covered at once; a finished job built from snapshot S
//! that took all the data work covers the current snapshot if no work came
//! after S, else S.
//! [`LiveGridState::flush`] waits for it, and so does store progress that
//! was cleared (`state/progress.rs`).

use std::{sync::Arc, time::Duration};

use async_channel::Sender;
use gpui_kit::{AppContext as _, Context, SharedString, Task};
use web_time::Instant;

use super::{
    ColumnOf, LiveGridDelegate, LiveGridEvent, LiveGridState, SelectionOrigin, SortOrigin,
    events::GroupOrigin,
    frame::PaintedLine,
    grouping::{ExpansionRevision, same_structure},
    scroll::ScrollReset,
    stats::{profile_elapsed, profile_start},
};
use crate::{
    engine::{Projection, ProjectionEngine, SortDirection, ViewSpec},
    highlight::HighlightPolicy,
    model::GridRow,
    policy::{ReorderContext, ReorderDecision, ReorderPolicy},
    quick_filter::QuickFilter,
    store::{ChangeSet, StoreSnapshot},
};

/// What the projection needs before it matches the latest snapshot and the
/// view's expansion: a set of work in which a rebuild takes every other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PendingWork {
    #[default]
    None,
    /// Work over the shown projection, in one job.
    Refresh(Refresh),
    /// Rows were inserted, values the order depends on changed, or the view
    /// changed. `expand`: the expansion changed while a data-driven rebuild
    /// waits; the lines are laid out first, as a user action.
    Rebuild { expand: bool },
}

impl PendingWork {
    const REBUILD: Self = Self::Rebuild { expand: false };
    const COMPACT: Self = Self::Refresh(Refresh {
        compact: true,
        aggregate: false,
        expand: false,
    });
    const AGGREGATE: Self = Self::Refresh(Refresh {
        compact: false,
        aggregate: true,
        expand: false,
    });
    pub(super) const EXPAND: Self = Self::Refresh(Refresh::EXPAND);

    /// This work and `other`: a rebuild takes the rest, but keeps a change
    /// of the expansion apart.
    pub(super) fn with(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, work) | (work, Self::None) => work,
            (Self::Rebuild { expand: a }, Self::Rebuild { expand: b }) => {
                Self::Rebuild { expand: a || b }
            }
            (Self::Rebuild { expand }, Self::Refresh(refresh))
            | (Self::Refresh(refresh), Self::Rebuild { expand }) => Self::Rebuild {
                expand: expand || refresh.expand,
            },
            (Self::Refresh(a), Self::Refresh(b)) => Self::Refresh(Refresh {
                compact: a.compact || b.compact,
                aggregate: a.aggregate || b.aggregate,
                expand: a.expand || b.expand,
            }),
        }
    }
}

/// Jobs over the shown projection, run in this order by one job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Refresh {
    /// Rows the projection holds were removed: drop them, recount groups.
    compact: bool,
    /// Values aggregates depend on changed, or rows were removed under
    /// aggregates: recount them, in the held order too. Values only: no
    /// reorder, no `ProjectionChange`.
    aggregate: bool,
    /// The expansion changed: lay the lines out again.
    expand: bool,
}

impl Refresh {
    const EXPAND: Self = Self {
        compact: false,
        aggregate: false,
        expand: true,
    };

    /// Returns true if the job does work the data asked for, which the
    /// throttle paces.
    fn has_data_work(self) -> bool {
        self.compact || self.aggregate
    }

    /// Returns true if the job changes the shown lines (an order is shown).
    fn changes_lines(self) -> bool {
        self.compact || self.expand
    }

    /// Runs the jobs over `shown`, in order, and stamps what changed in
    /// the summaries against `shown` at `stamped`.
    fn run<R: GridRow>(
        self,
        engine: &dyn ProjectionEngine<R>,
        shown: &Projection,
        snapshot: &StoreSnapshot<R>,
        view: &ViewSpec<R>,
        stamped: SummaryClock,
    ) -> Projection {
        let mut refreshed = self.compact.then(|| engine.compact(shown, snapshot));
        if self.aggregate {
            let aggregated = engine.aggregate(refreshed.as_ref().unwrap_or(shown), snapshot, view);
            refreshed = Some(aggregated);
        }
        if self.expand {
            let expanded = engine.expand(refreshed.as_ref().unwrap_or(shown), snapshot, view);
            refreshed = Some(expanded);
        }
        let mut refreshed = refreshed.unwrap_or_else(|| shown.duplicate());
        stamped.carry(&mut refreshed, shown);
        refreshed
    }
}

/// When and how a job stamps the summaries of its projection: the store
/// clock at the job's start, in milliseconds since the snapshot's epoch,
/// and the store's highlight policy, which drops stamps that expired.
#[derive(Clone, Copy, Debug)]
struct SummaryClock {
    now_ms: u32,
    policy: HighlightPolicy,
}

impl SummaryClock {
    /// Stamps `projection`'s aggregates and captions that differ from
    /// `previous`, and carries `previous`'s stamps that are still hot.
    fn carry(self, projection: &mut Projection, previous: &Projection) {
        projection.carry_summary_stamps(previous, self.now_ms, self.policy);
    }
}

/// A job to start.
#[derive(Clone, Copy, Debug)]
enum Job {
    Rebuild,
    Refresh(Refresh),
}

/// What the pending work allows now.
enum NextJob {
    /// Nothing is pending.
    Idle,
    /// Data-driven work waits this long for the throttle.
    Wait(Duration),
    /// `job` may start; `rest` stays pending after it.
    Start { job: Job, rest: PendingWork },
}

/// What [`LiveGridState::flush`] waited for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlushOutcome {
    store_version: u64,
    row_count: usize,
    order_held: bool,
}

impl FlushOutcome {
    /// The store version every write before the flush is part of.
    pub fn store_version(&self) -> u64 {
        self.store_version
    }

    /// Data rows of the order shown when the flush resolved
    /// (`Projection::row_count`): rows of collapsed groups count, pinned
    /// rows do not.
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Returns true if the order that reflects the writes is held by the
    /// [`ReorderPolicy`](crate::ReorderPolicy) and not shown yet: the
    /// shown order, and [`Self::row_count`], are the old ones.
    /// [`LiveGridState::apply_held_order`] shows it.
    pub fn is_order_held(&self) -> bool {
        self.order_held
    }
}

/// The coverage of the order shown or held, and the flushes waiting for it.
#[derive(Default)]
pub(super) struct Coverage {
    /// Every store change up to this version is reflected in the order
    /// shown or held.
    version: u64,
    /// The version of the latest pulled snapshot whose changes needed work
    /// (a rebuild, a compaction or a recount of aggregates).
    last_work: u64,
    /// `flush` calls waiting for `version` to reach theirs.
    waiting: Vec<(u64, Sender<FlushOutcome>)>,
}

impl Coverage {
    /// Returns true while a flush waits.
    fn is_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// Returns true if a flush waits for a version the view already holds:
    /// the work for it starts without the throttle, once.
    fn lifts_throttle(&self, held_version: u64) -> bool {
        self.waiting
            .iter()
            .any(|(version, _)| *version <= held_version)
    }
}

/// A newer display order kept back by the reorder policy.
pub(super) struct HeldOrder {
    projection: Arc<Projection>,
    /// The expansion its lines are laid out for: shown with an older one,
    /// it is laid out again (`expand`).
    revision: ExpansionRevision,
    since: Instant,
    _timer: Option<Task<()>>,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Returns true if a newer order is ready but held by the reorder policy.
    pub fn is_order_held(&self) -> bool {
        self.held.is_some()
    }

    /// Shows the held order now (for a "refresh order" control).
    pub fn apply_held_order(&mut self, cx: &mut Context<Self>) {
        // `install` releases the held order and emits the event.
        if let Some((projection, revision)) = self
            .held
            .as_ref()
            .map(|held| (held.projection.clone(), held.revision))
        {
            self.install(projection, ScrollReset::Keep, revision, cx);
        }
    }

    /// Replaces the reorder policy at run time, as a "Live / Hold / Manual"
    /// switch does; [`Self::with_reorder_policy`] sets it when the grid is
    /// built. A held order is offered to the new policy at once, as when the
    /// pointer leaves the grid, with the hold's start kept: switching to
    /// [`LiveReorder`](crate::LiveReorder) shows it and emits
    /// `OrderHeldChange { held: false }`. While the context menu is open the
    /// built-in policies keep holding. With no order held only the policy
    /// changes.
    ///
    /// ```
    /// use gpui_kit::{App, Entity};
    /// use gpui_kit_grid::{LiveGridDelegate, LiveGridState, LiveReorder, ManualReorder};
    ///
    /// /// A "Live / Manual" switch in a toolbar.
    /// fn set_live<D: LiveGridDelegate>(grid: &Entity<LiveGridState<D>>, live: bool, cx: &mut App) {
    ///     grid.update(cx, |grid, cx| {
    ///         if live {
    ///             grid.set_reorder_policy(LiveReorder, cx);
    ///         } else {
    ///             grid.set_reorder_policy(ManualReorder, cx);
    ///         }
    ///     });
    /// }
    /// ```
    pub fn set_reorder_policy(&mut self, policy: impl ReorderPolicy, cx: &mut Context<Self>) {
        self.reorder = Box::new(policy);
        self.reconsider_held(cx);
    }

    /// Resolves once the order shown, or held by the reorder policy,
    /// reflects every operation written to the store before the call: with
    /// what it waited for, or `None` if the grid or the store is gone.
    ///
    /// For tests and for "select the first row after loading":
    ///
    /// ```ignore
    /// writer.replace_all(rows);
    /// let flushed = grid.update(cx, |grid, cx| grid.flush(cx));
    /// cx.spawn(async move |_, cx| {
    ///     if flushed.await.is_some() {
    ///         grid.update(cx, |grid, cx| {
    ///             let first = grid
    ///                 .projection()
    ///                 .rows()
    ///                 .first()
    ///                 .and_then(|id| grid.snapshot().row(*id))
    ///                 .map(|row| row.key().clone());
    ///             grid.select_key(first, cx);
    ///         })
    ///         .ok();
    ///     }
    /// })
    /// .detach();
    /// ```
    ///
    /// Call it after the writes it should wait for: a producer on another
    /// thread awaits `GridWriter::flush` itself or tells the UI thread it
    /// wrote. Operations buffered by an unfinished reload are not waited
    /// for (`GridWriter::flush`). An order held by the reorder policy
    /// resolves the flush with [`FlushOutcome::is_order_held`]; a new data
    /// set (`replace_all`, a finished reload) is shown under any policy.
    /// `ProjectionChange` of the order comes before the flush resolves.
    ///
    /// The work the flush waits for starts at once: without the
    /// `min_rebuild_interval` throttle, and from the flush itself when the
    /// grid is not painted (a hidden tab, a grid not in a window). So a
    /// flush is not something to call on every tick: each call lifts the
    /// throttle for the rebuild it waits for.
    pub fn flush(&mut self, cx: &mut Context<Self>) -> Task<Option<FlushOutcome>> {
        let written = self.store.writer().flush();
        cx.spawn(async move |this, cx| {
            let version = written.await?;
            let (done, outcome) = async_channel::bounded(1);
            this.update(cx, |state, cx| state.wait_for_coverage(version, done, cx))
                .ok()?;
            outcome.recv().await.ok()
        })
    }

    /// Registers a flush of the store version `version` and starts the work
    /// it waits for.
    fn wait_for_coverage(
        &mut self,
        version: u64,
        done: Sender<FlushOutcome>,
        cx: &mut Context<Self>,
    ) {
        self.coverage.waiting.push((version, done));
        self.resolve_flushes();
        if self.coverage.is_waiting() {
            self.ensure_projection(cx);
        }
    }

    /// The order shown or held now covers `version`: flushes up to it
    /// resolve, and store progress cleared up to it leaves.
    fn cover(&mut self, version: u64, cx: &mut Context<Self>) {
        self.coverage.version = self.coverage.version.max(version);
        self.resolve_flushes();
        if self.progress.release(self.coverage.version) {
            cx.notify();
        }
    }

    /// Resolves the flushes the coverage reached; drops the ones nobody
    /// awaits any more.
    fn resolve_flushes(&mut self) {
        if !self.coverage.is_waiting() {
            return;
        }
        let covered = self.coverage.version;
        let row_count = self.projection.row_count();
        let order_held = self.held.is_some();
        self.coverage.waiting.retain(|(version, done)| {
            if *version > covered {
                return !done.is_closed();
            }
            let _ = done.try_send(FlushOutcome {
                store_version: *version,
                row_count,
                order_held,
            });
            false
        });
    }

    /// A job built from the snapshot of `version` finished. `covers`: its
    /// order is shown or held and it took all the data work pending when it
    /// started; not when it was dropped as stale or was laid out ahead of a
    /// rebuild that still waits. With no work since, the order covers the
    /// current snapshot; else that version. Pending work starts at once
    /// while a flush waits, else from `render`.
    fn job_done(&mut self, version: u64, covers: bool, cx: &mut Context<Self>) {
        if self.pending == PendingWork::None && self.coverage.last_work <= version {
            self.cover(self.snapshot.version(), cx);
        } else if covers {
            self.cover(version, cx);
        }
        if self.pending != PendingWork::None {
            if self.coverage.is_waiting() {
                self.ensure_projection(cx);
            }
            cx.notify();
        }
    }

    /// Tracks the pointer over the body; leaving may release a held order.
    pub(crate) fn set_pointer_inside(&mut self, inside: bool, cx: &mut Context<Self>) {
        if self.pointer_inside == inside {
            return;
        }
        self.pointer_inside = inside;
        if !inside {
            self.reconsider_held(cx);
        }
    }

    /// Replaces the view (sort, filter, grouping or tree, aggregates) and
    /// rebuilds at once. The new order is shown from the top, unless the
    /// focused line (a row or a group caption) is on screen and still shown:
    /// it keeps its screen line. The grid owns the quick filter
    /// and the expansion: they stay, and those of `view` are replaced by
    /// them ([`Self::set_quick_filter`], [`Self::set_expanded`]). Emits
    /// `SortChange` (`SortOrigin::Programmatic`) if the sort differs, then
    /// `GroupChange` (`GroupOrigin::Programmatic`) if the grouping or the
    /// tree does.
    pub fn set_view(&mut self, view: ViewSpec<D::Row>, cx: &mut Context<Self>) {
        let sort_changed = view.sort() != self.view.sort();
        let structure_changed = !same_structure(&view, &self.view);
        let quick_filter = self.view.quick_filter().cloned();
        let expansion = self.view.expansion().clone();
        self.view = view
            .with_quick_filter(quick_filter)
            .with_expansion(expansion);
        self.view_changed(
            sort_changed.then_some(SortOrigin::Programmatic),
            structure_changed.then_some(GroupOrigin::Programmatic),
            cx,
        );
    }

    /// The sort, most significant column first; empty for the insertion
    /// order.
    pub fn sort(&self) -> &[(ColumnOf<D>, SortDirection)] {
        self.view.sort()
    }

    /// Replaces the sort and rebuilds at once (`SortChange` with
    /// `SortOrigin::Programmatic`), shown as by [`Self::set_view`]: from the
    /// top, or with the focused line on its screen line. Repeated columns are
    /// dropped, the first one stays; the same sort again does nothing. Works
    /// for columns that a header click does not sort.
    pub fn set_sort(
        &mut self,
        sort: impl IntoIterator<Item = (ColumnOf<D>, SortDirection)>,
        cx: &mut Context<Self>,
    ) {
        self.apply_sort(sort, SortOrigin::Programmatic, cx);
    }

    /// The one path of every sort change: header clicks, `set_sort`, and the
    /// menu and layouts of later steps. Drops repeated columns (the first one
    /// stays) and returns false, doing nothing, if the sort is unchanged.
    /// Otherwise releases a held order (`OrderHeldChange { held: false }`),
    /// emits `SortChange { sort, origin }` and rebuilds at once.
    pub(crate) fn apply_sort(
        &mut self,
        sort: impl IntoIterator<Item = (ColumnOf<D>, SortDirection)>,
        origin: SortOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut normalized: Vec<(ColumnOf<D>, SortDirection)> = Vec::new();
        for (column, direction) in sort {
            if !normalized.iter().any(|(sorted, _)| *sorted == column) {
                normalized.push((column, direction));
            }
        }
        if normalized == self.view.sort() {
            return false;
        }
        self.view = std::mem::take(&mut self.view).with_sort(normalized);
        self.view_changed(Some(origin), None, cx);
        true
    }

    /// The quick filter text, trimmed; empty when the filter is off.
    pub fn quick_filter(&self) -> &SharedString {
        &self.quick_filter_text
    }

    /// Shows only rows whose display text contains the text, as the
    /// [`QuickFilterMatcher`] decides (default [`ContainsAllTerms`]: every
    /// whitespace-separated term, ignoring case, in some quick filter
    /// column). Rebuilds at once, like any user action, and shows the new
    /// order as [`Self::set_view`] does.
    /// The text is trimmed; empty or blank text turns the filter off, and the
    /// same trimmed text again does nothing.
    ///
    /// [`QuickFilterMatcher`]: crate::QuickFilterMatcher
    /// [`ContainsAllTerms`]: crate::ContainsAllTerms
    pub fn set_quick_filter(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        let trimmed = text.trim();
        if trimmed == &*self.quick_filter_text {
            return;
        }
        self.quick_filter_text = if trimmed.len() == text.len() {
            text.clone()
        } else {
            SharedString::from(trimmed.to_owned())
        };
        self.recompile_quick_filter();
        self.view_changed(None, None, cx);
    }

    /// Compiles the quick filter text again into the view (new text, matcher
    /// or columns). Does not rebuild.
    pub(super) fn recompile_quick_filter(&mut self) {
        let columns = self
            .quick_filter_columns
            .unwrap_or_else(|| self.columns.visible_set());
        let filter = QuickFilter::new(
            &self.quick_filter_text,
            columns,
            &*self.quick_filter_matcher,
        )
        .map(|filter| filter.with_number_formats(self.columns.number_formats()));
        self.view = std::mem::take(&mut self.view).with_quick_filter(filter);
    }

    /// The visible columns changed: a quick filter over the default columns
    /// now searches others, which is a user action. The scroll position
    /// stays: the user changed the columns, not the view.
    pub(super) fn quick_filter_columns_changed(&mut self, cx: &mut Context<Self>) {
        if self.follow_visible_columns() {
            self.restart_projection(cx);
            cx.notify();
        }
    }

    /// Compiles a quick filter over the default columns again for the
    /// visible columns. Returns true if the view changed and needs a
    /// rebuild; does not rebuild.
    pub(super) fn follow_visible_columns(&mut self) -> bool {
        if self.quick_filter_columns.is_some() || self.view.quick_filter().is_none() {
            return false;
        }
        self.recompile_quick_filter();
        true
    }

    /// A new view: rebuild at once and show it from the top, or with a
    /// painted focused line on its screen line (`restore_line_anchor`).
    /// Events in this order: `OrderHeldChange { held: false }` (if held),
    /// then `SortChange` (with a sort change), then `GroupChange` (with a
    /// change of the grouping or the tree), then later `ProjectionChange`.
    pub(super) fn view_changed(
        &mut self,
        sort_changed: Option<SortOrigin>,
        structure_changed: Option<GroupOrigin>,
        cx: &mut Context<Self>,
    ) {
        self.restart_projection(cx);
        // A new grouping or tree shows its first caption at the top unless
        // the user had scrolled (`restore_line_anchor`).
        self.pending_scroll_reset = self
            .pending_scroll_reset
            .max(if structure_changed.is_some() {
                ScrollReset::NewStructure
            } else {
                ScrollReset::NewView
            });
        if let Some(origin) = sort_changed {
            cx.emit(LiveGridEvent::SortChange {
                sort: self.view.sort().into(),
                origin,
            });
        }
        if let Some(origin) = structure_changed {
            cx.emit(LiveGridEvent::GroupChange { origin });
        }
        cx.notify();
    }

    /// Schedules a rebuild that starts in the next `render` regardless of the
    /// throttle, skips the reorder hold, takes every other pending work (a
    /// change of the expansion among it) and drops work for the old view and
    /// a held order of it.
    pub(super) fn restart_projection(&mut self, cx: &mut Context<Self>) {
        self.pending = PendingWork::REBUILD;
        self.pending_user_action = true;
        self.last_rebuild_started = None;
        // Work for the old view is useless now: dropping cancels it.
        self.rebuild_task = None;
        self.rebuild_timer = None;
        if self.held.take().is_some() {
            cx.emit(LiveGridEvent::OrderHeldChange { held: false });
        }
    }

    pub(super) fn apply_snapshot(
        &mut self,
        snapshot: Arc<StoreSnapshot<D::Row>>,
        changes: ChangeSet,
        cx: &mut Context<Self>,
    ) {
        if changes.is_empty() && snapshot.version() == self.snapshot.version() {
            return;
        }
        let started = profile_start();
        let work = self.work_for(&changes);
        let version = snapshot.version();
        if work != PendingWork::None {
            self.coverage.last_work = version;
        }
        if changes.columns().intersects(self.view.aggregate_columns()) {
            self.aggregates_changed = true;
        }
        self.pending = self.pending.with(work);
        // A new data set (or the first rows of an empty grid) is not a
        // stream of changes: show it at once instead of after the throttle,
        // and drop a rebuild of the old data still in flight.
        if changes.is_reset() || (self.projection.is_empty() && changes.inserted() > 0) {
            self.last_rebuild_started = None;
            self.rebuild_task = None;
            self.pending_user_action = true;
        }
        // A visible change: a painted row changed or was removed, or a
        // painted fading row left the snapshot.
        let visible = changes
            .updated()
            .iter()
            .chain(changes.removed())
            .any(|id| self.painted.iter().any(|line| line.row() == Some(*id)))
            || changes
                .faded()
                .iter()
                .any(|id| self.painted.contains(&PaintedLine::Ghost(*id)));

        // The view often holds the last reference to the old snapshot:
        // free its retired pages and rows off the UI thread. A projection
        // built before a removal, or before a fading row left, may still
        // hold its id: a compaction drops it when shown.
        if !changes.removed().is_empty() || !changes.faded().is_empty() {
            self.last_removal_version = version;
        }
        if changes.has_progress_change() {
            self.progress.follow_store(snapshot.progress(), version);
        }
        let retired = std::mem::replace(&mut self.snapshot, snapshot);
        self.stats.snapshots += 1;

        // Removed keys leave the selection, read through the retired
        // snapshot; a removed focus moves to its successor. A new data set
        // keeps only the keys it has. O(removed) per pull.
        let mark = self.selection.mark();
        self.selection.remove_rows(changes.removed(), &retired);
        if changes.is_reset() {
            self.selection.retain_present(&self.snapshot);
        } else if !changes.removed().is_empty() {
            self.selection
                .follow_removed(&self.projection, &retired, &self.snapshot);
        }
        let selection_changed = self.finish_change(mark, SelectionOrigin::DataRemoved, cx);
        cx.background_spawn(async move { drop(retired) }).detach();
        // The edited row may have changed or left the store.
        self.follow_edit_rows(&changes, cx);

        // Pinned rows are always on screen, and the progress may show or
        // leave: any change of them repaints. A band change resizes the
        // data rows' area.
        if changes.has_pinned_change() {
            self.clamp_scroll();
        }
        // Off-screen updates do not repaint: a grid fed 10k updates/s across
        // 100k rows mostly changes rows nobody sees. Pending work starts in
        // `render`, so it needs a render only once it may start; until the
        // throttle allows it, one timer waits instead of a render per pull.
        let mut notify = visible
            || selection_changed
            || changes.has_pinned_change()
            || changes.has_progress_change();
        if self.rebuild_task.is_none() {
            match self.next_job(self.store.now()) {
                NextJob::Idle => {}
                NextJob::Start { .. } => notify = true,
                NextJob::Wait(wait) => self.ensure_rebuild_timer(wait, cx),
            }
        }
        // Changes that need no work are covered as soon as they are taken.
        if self.pending == PendingWork::None && self.rebuild_task.is_none() {
            self.cover(version, cx);
        }
        // While a flush waits, pending work starts here and not only from
        // `render`: the grid may not be painted at all.
        if self.coverage.is_waiting() {
            self.ensure_projection(cx);
        }
        if notify {
            cx.notify();
        }
        self.stats.last_pull = profile_elapsed(started);
    }

    /// The work a pulled change set needs. O(1) by column sets, plus
    /// O(removed) to find removed rows the projection holds.
    fn work_for(&self, changes: &ChangeSet) -> PendingWork {
        let columns = changes.columns();
        let removed = changes.removed();
        let mut work = PendingWork::None;
        // Inserted rows and changes of what orders, filters or groups rows
        // can move any row.
        if changes.has_insertions_or_reset() || columns.intersects(self.view.dependencies()) {
            work = PendingWork::REBUILD;
        }
        // A new parent moves a tree row with its subtree, though no column
        // may have changed.
        if self.view.tree().is_some() && changes.has_parent_change() {
            work = PendingWork::REBUILD;
        }
        // A removed tree row with children moves them to another parent.
        if self.projection.is_tree()
            && removed
                .iter()
                .any(|id| self.projection.branch(*id).is_some())
        {
            work = PendingWork::REBUILD;
        }
        // Removing rows the projection does not hold needs no work at all;
        // removing rows it holds (shown, or in a collapsed group) needs a
        // compaction, not a sort. A job in flight was built from an older
        // snapshot and may still hold them.
        if removed.iter().any(|id| self.projection.contains(*id))
            || (self.rebuild_task.is_some() && !removed.is_empty())
        {
            work = work.with(self.removal_work());
        }
        // A fading row whose fade ended left the snapshot: the projection
        // still holds its id, and a compaction drops it. It counted in no
        // group and no aggregate, so nothing is recounted.
        let faded = changes.faded();
        if faded.iter().any(|id| self.projection.contains(*id))
            || (self.rebuild_task.is_some() && !faded.is_empty())
        {
            work = work.with(PendingWork::COMPACT);
        }
        // Values of aggregates change, not the order.
        if columns.intersects(self.view.aggregate_columns()) {
            work = work.with(PendingWork::AGGREGATE);
        }
        work
    }

    /// The stamp clock of a job starting at `now` (store clock).
    fn summary_clock(&self, now: Instant) -> SummaryClock {
        SummaryClock {
            now_ms: now
                .saturating_duration_since(self.snapshot.epoch())
                .as_millis() as u32,
            policy: self.store.options().highlight(),
        }
    }

    /// What removing rows the projection holds needs: a compaction, with the
    /// aggregates recounted if the view has any; a rebuild if groups are
    /// ordered by aggregates, which removals change.
    fn removal_work(&self) -> PendingWork {
        let view = &self.view;
        if !view.group_by().is_empty() && !view.group_order().depends_on().is_empty() {
            PendingWork::REBUILD
        } else if view.aggregates().is_empty() {
            PendingWork::COMPACT
        } else {
            PendingWork::COMPACT.with(PendingWork::AGGREGATE)
        }
    }

    /// Returns true if the view groups rows or builds a tree: only then has
    /// it lines to lay out by the expansion.
    pub(super) fn has_structure(&self) -> bool {
        !self.view.group_by().is_empty() || self.view.tree().is_some()
    }

    /// What the pending work allows at `now`. A rebuild for a user action
    /// starts at once (`restart_projection` cleared the throttle) and takes
    /// everything; a change of the expansion starts at once, with the other
    /// work over the shown projection, or alone ahead of a data-driven
    /// rebuild, which may be throttled and held; other data-driven work
    /// waits for the throttle.
    fn next_job(&self, now: Instant) -> NextJob {
        let start = |job, rest| NextJob::Start { job, rest };
        let throttled = |job| match self.throttle_wait(now) {
            Some(wait) => NextJob::Wait(wait),
            None => start(job, PendingWork::None),
        };
        match self.pending {
            PendingWork::None => NextJob::Idle,
            PendingWork::Rebuild { expand: true } if !self.pending_user_action => {
                start(Job::Refresh(Refresh::EXPAND), PendingWork::REBUILD)
            }
            PendingWork::Rebuild { .. } => throttled(Job::Rebuild),
            PendingWork::Refresh(work) if work.expand => {
                start(Job::Refresh(work), PendingWork::None)
            }
            PendingWork::Refresh(work) => throttled(Job::Refresh(work)),
        }
    }

    /// How long data-driven work must still wait, or `None` if it may start.
    /// A flush waiting for a snapshot the view holds lifts the throttle.
    fn throttle_wait(&self, now: Instant) -> Option<Duration> {
        if self.coverage.lifts_throttle(self.snapshot.version()) {
            return None;
        }
        let started = self.last_rebuild_started?;
        let elapsed = now.saturating_duration_since(started);
        (elapsed < self.tuning.min_rebuild_interval)
            .then(|| self.tuning.min_rebuild_interval - elapsed)
    }

    fn ensure_rebuild_timer(&mut self, wait: Duration, cx: &mut Context<Self>) {
        if self.rebuild_timer.is_some() {
            return;
        }
        self.rebuild_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |state, cx| {
                state.rebuild_timer = None;
                if state.coverage.is_waiting() {
                    state.ensure_projection(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Starts the pending work in the background. Called from `render`, so a
    /// grid that is not painted does no sorting at all; and while a flush
    /// waits, from the pull, the timer and the flush.
    pub(super) fn ensure_projection(&mut self, cx: &mut Context<Self>) {
        if self.pending == PendingWork::None || self.rebuild_task.is_some() {
            return;
        }
        let now = self.store.now();
        match self.next_job(now) {
            NextJob::Idle => {}
            NextJob::Wait(wait) => self.ensure_rebuild_timer(wait, cx),
            NextJob::Start { job, rest } => {
                self.pending = rest;
                match job {
                    Job::Rebuild => self.start_rebuild(now, cx),
                    Job::Refresh(work) => {
                        self.start_refresh(work, rest == PendingWork::None, now, cx)
                    }
                }
            }
        }
    }

    /// Rebuilds the view from the snapshot; the reorder policy decides on
    /// the result unless it is for a user action.
    fn start_rebuild(&mut self, now: Instant, cx: &mut Context<Self>) {
        let user_action = std::mem::take(&mut self.pending_user_action);
        // A rebuild takes in an aggregate job; if its order is held, the
        // shown order still needs one (`offer`).
        let aggregates_changed = std::mem::take(&mut self.aggregates_changed);
        // Copied, not taken: a rebuild cancelled by newer work must leave the
        // scroll reset to the rebuild that replaces it. A view change always
        // cancels the rebuild in flight, so a finished one never predates it.
        let scroll_reset = self.pending_scroll_reset;
        self.last_rebuild_started = Some(now);
        let snapshot = self.snapshot.clone();
        let job_version = snapshot.version();
        let revision = self.expansion_revision;
        let view = self.view.clone();
        let engine = self.engine.clone();
        // Stamps against the newest projection of the view; none for a new
        // view or data set: the view changed, not the data.
        let previous = (!user_action).then(|| {
            self.held
                .as_ref()
                .map_or_else(|| self.projection.clone(), |held| held.projection.clone())
        });
        let stamped = self.summary_clock(now);
        let (done, result) = async_channel::bounded(1);
        let job = self.runner.run(
            Box::new(move || {
                let started = profile_start();
                let mut projection = engine.rebuild(&snapshot, &view);
                if let Some(previous) = previous {
                    stamped.carry(&mut projection, &previous);
                }
                let _ = done.try_send((projection, profile_elapsed(started)));
            }),
            cx,
        );
        self.rebuild_task = Some(cx.spawn(async move |this, cx| {
            job.await;
            let Ok((projection, elapsed)) = result.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| {
                state.rebuild_task = None;
                state.stats.rebuilds += 1;
                state.stats.last_rebuild = elapsed;
                state.stats.max_rebuild = state.stats.max_rebuild.max(elapsed);
                state.offer(
                    Arc::new(projection),
                    user_action,
                    scroll_reset,
                    revision,
                    cx,
                );
                // Values are not held: with the order held, the shown
                // order's aggregates are recounted by a job of their own.
                if aggregates_changed && state.held.is_some() {
                    state.pending = state.pending.with(PendingWork::AGGREGATE);
                    cx.notify();
                }
                state.job_done(job_version, true, cx);
            });
        }));
    }

    /// Runs `work` over the shown projection, and recounts the aggregates of
    /// a held order in the same job. Nothing here reorders, so the reorder
    /// policy is not asked: a held order stays held. `covers`: the job takes
    /// all the data work pending (see [`Self::job_done`]).
    fn start_refresh(&mut self, work: Refresh, covers: bool, now: Instant, cx: &mut Context<Self>) {
        if work.has_data_work() {
            self.last_rebuild_started = Some(now);
        }
        if work.aggregate {
            self.aggregates_changed = false;
        }
        let snapshot = self.snapshot.clone();
        let job_version = snapshot.version();
        // The lines are laid out for the view's expansion if the job lays
        // them out, else they stay those of the shown projection.
        let revision = if work.expand {
            self.expansion_revision
        } else {
            self.shown_revision
        };
        let shown = self.projection.clone();
        let held = self
            .held
            .as_ref()
            .filter(|_| work.aggregate)
            .map(|held| held.projection.clone());
        let (shown_base, held_base) = (shown.clone(), held.clone());
        let view = self.view.clone();
        let engine = self.engine.clone();
        let stamped = self.summary_clock(now);
        let (done, result) = async_channel::bounded(1);
        let job = self.runner.run(
            Box::new(move || {
                let refreshed = work.run(&*engine, &shown, &snapshot, &view, stamped);
                // The held order keeps its store version: a compaction it may
                // need when shown is found by that version.
                let held = held.map(|held| {
                    let mut aggregated = engine
                        .aggregate(&held, &snapshot, &view)
                        .with_store_version(held.store_version());
                    stamped.carry(&mut aggregated, &held);
                    aggregated
                });
                let _ = done.try_send((refreshed, held));
            }),
            cx,
        );
        self.rebuild_task = Some(cx.spawn(async move |this, cx| {
            job.await;
            let Ok((refreshed, held)) = result.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| {
                state.rebuild_task = None;
                if let (Some(projection), Some(base)) = (held, held_base) {
                    state.refresh_held(projection, &base);
                }
                let applied = Arc::ptr_eq(&state.projection, &shown_base);
                if applied {
                    state.finish_refresh(Arc::new(refreshed), work, revision, cx);
                } else {
                    // A held order was applied meanwhile: this is the old
                    // order. The work stays pending for the shown one
                    // (`show` scheduled a compaction it needs), and `render`
                    // starts it.
                    state.pending = state.pending.with(PendingWork::Refresh(work));
                    cx.notify();
                }
                state.job_done(job_version, applied && covers, cx);
            });
        }));
    }

    /// Shows the result of `work` over the shown projection.
    fn finish_refresh(
        &mut self,
        projection: Arc<Projection>,
        work: Refresh,
        revision: ExpansionRevision,
        cx: &mut Context<Self>,
    ) {
        if work.compact {
            self.stats.compactions += 1;
        }
        if work.changes_lines() {
            // Removals and a new expansion never reorder: no reason to hold,
            // and a newer order held by the reorder policy stays held.
            self.show(projection, ScrollReset::Keep, revision, cx);
        } else {
            // Fresh aggregates over the same lines: values, not an order, so
            // no `ProjectionChange`.
            self.projection = projection;
            cx.notify();
        }
    }

    /// Puts recounted aggregates into the held order, if it is still the
    /// one they were recounted from.
    fn refresh_held(&mut self, projection: Projection, base: &Arc<Projection>) {
        if let Some(held) = &mut self.held
            && Arc::ptr_eq(&held.projection, base)
        {
            held.projection = Arc::new(projection);
        }
    }

    /// Asks the reorder policy whether a new order may replace the shown one.
    /// The order of a new view is shown from the top (`scroll_reset`), or
    /// with a painted focused row on its line.
    fn offer(
        &mut self,
        projection: Arc<Projection>,
        user_action: bool,
        scroll_reset: ScrollReset,
        revision: ExpansionRevision,
        cx: &mut Context<Self>,
    ) {
        let now = self.store.now();
        let since = self.held.as_ref().map_or(now, |held| held.since);
        let context = self.reorder_context(now, since, user_action);
        match self.reorder.decide(&context) {
            ReorderDecision::Apply => {
                if scroll_reset.is_reset() {
                    self.pending_scroll_reset = ScrollReset::Keep;
                }
                self.install(projection, scroll_reset, revision, cx)
            }
            ReorderDecision::Hold => self.hold(projection, revision, since, None, cx),
            ReorderDecision::HoldUntil(deadline) => {
                let wait = deadline.saturating_duration_since(now);
                let timer = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |state, cx| state.reconsider_held(cx));
                });
                self.hold(projection, revision, since, Some(timer), cx);
            }
        }
    }

    fn hold(
        &mut self,
        projection: Arc<Projection>,
        revision: ExpansionRevision,
        since: Instant,
        timer: Option<Task<()>>,
        cx: &mut Context<Self>,
    ) {
        let was_held = self.held.is_some();
        self.held = Some(HeldOrder {
            projection,
            revision,
            since,
            _timer: timer,
        });
        if !was_held {
            cx.emit(LiveGridEvent::OrderHeldChange { held: true });
            // The header shows that a newer order is waiting.
            cx.notify();
        }
    }

    /// What the reorder policy is told: the clock, the start of the hold,
    /// the pointer over the rows, the source of the order, the open menu
    /// and the shown tooltip.
    fn reorder_context(&self, now: Instant, since: Instant, user_action: bool) -> ReorderContext {
        ReorderContext::new(now, since, self.is_pointer_over_rows(), user_action)
            .with_menu_open(self.is_menu_open())
            .with_tooltip_shown(self.tooltip.is_shown())
            // The built-in policies hold while editing; a policy may hold
            // while commits wait.
            .with_editing(self.is_editing())
            .with_pending_edits(self.pending_edit_count() > 0)
            // A policy may hold while the chooser is open.
            .with_column_chooser_open(self.is_column_chooser_open())
            // A policy may hold while a drag is over the rows.
            .with_drag_over(self.drag.is_over())
    }

    /// Whether the pointer hovers the rows, as the reorder policy sees it:
    /// never during a column gesture, or dragging a header over the body
    /// would hold the order.
    fn is_pointer_over_rows(&self) -> bool {
        self.pointer_inside && !self.is_column_gesture_active()
    }

    /// Re-asks the policy about a held order (pointer left, deadline passed,
    /// a column gesture started, the context menu closed).
    pub(super) fn reconsider_held(&mut self, cx: &mut Context<Self>) {
        if let Some(held) = self.held.take() {
            let since = held.since;
            // Keep `since` so a hold is bounded from its start, not restarted.
            self.held = Some(HeldOrder {
                projection: held.projection.clone(),
                revision: held.revision,
                since,
                _timer: None,
            });
            let now = self.store.now();
            let context = self.reorder_context(now, since, false);
            match self.reorder.decide(&context) {
                ReorderDecision::Apply => {
                    self.install(held.projection, ScrollReset::Keep, held.revision, cx)
                }
                ReorderDecision::Hold => {}
                ReorderDecision::HoldUntil(deadline) => {
                    let wait = deadline.saturating_duration_since(now);
                    let timer = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(wait).await;
                        let _ = this.update(cx, |state, cx| state.reconsider_held(cx));
                    });
                    if let Some(held) = &mut self.held {
                        held._timer = Some(timer);
                    }
                }
            }
        }
    }

    /// Shows a new display order in place of the shown one and of a held one.
    fn install(
        &mut self,
        projection: Arc<Projection>,
        scroll_reset: ScrollReset,
        revision: ExpansionRevision,
        cx: &mut Context<Self>,
    ) {
        if self.held.take().is_some() {
            cx.emit(LiveGridEvent::OrderHeldChange { held: false });
        }
        self.show(projection, scroll_reset, revision, cx);
    }

    /// Shows `projection`: the one place `ProjectionChange` is emitted, once
    /// per order shown, never for a held one. A projection built from an
    /// older snapshot may hold rows removed since: a compaction follows to
    /// drop them. Lines laid out for an older expansion (`revision`, a held
    /// order or a rebuild that started before the change) are laid out again.
    ///
    /// The scroll anchor keeps its screen line: a toggled caption or tree
    /// row when the lines of its newest expansion are shown, else the
    /// painted focused line, else the first painted line. A focus or an
    /// anchor on a row the new order leaves out moves to its successor; a
    /// focused caption whose group is gone moves to the line at its place,
    /// and a focused line a collapsed group or tree row now hides moves to
    /// its caption or row (`FocusChange` with `Filtered`, after
    /// `ProjectionChange`).
    fn show(
        &mut self,
        projection: Arc<Projection>,
        scroll_reset: ScrollReset,
        revision: ExpansionRevision,
        cx: &mut Context<Self>,
    ) {
        let row_count = projection.row_count();
        let store_version = projection.store_version();
        if store_version < self.last_removal_version {
            // Which removed rows it holds is not known here: in a tree, one
            // may have had children, which move.
            let work = if projection.is_tree() {
                PendingWork::REBUILD
            } else {
                self.removal_work()
            };
            self.pending = self.pending.with(work);
        }
        self.shown_revision = revision;
        if revision != self.expansion_revision && self.has_structure() {
            self.pending = self.pending.with(PendingWork::EXPAND);
        }
        let toggled = if revision == self.expansion_revision {
            self.expand_anchor.take()
        } else {
            None
        };
        let anchor = toggled.or_else(|| self.line_anchor());
        let previous = std::mem::replace(&mut self.projection, projection);
        let mark = self.selection.mark();
        self.selection
            .follow_filtered(&previous, &self.projection, &self.snapshot);
        self.follow_shown_lines(&previous);
        self.restore_line_anchor(anchor, &previous, scroll_reset);
        self.clamp_scroll();
        cx.emit(LiveGridEvent::ProjectionChange {
            row_count,
            store_version,
        });
        self.finish_change(mark, SelectionOrigin::Filtered, cx);
        // An edited row the new order hides.
        self.follow_edit_shown(cx);
        cx.notify();
    }
}
