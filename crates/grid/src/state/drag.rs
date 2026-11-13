//! Dragging rows within the application: what a row drag carries, what a
//! drop names, and what the delegate's `drag_payload` hook knows.
//!
//! The source: a press a [`RowDragGesture`] takes starts no drag-selection;
//! past the gesture's threshold the grid takes the pressed row's command
//! scope and the delegate's payload and arms the rows' wrapper
//! (`"live-grid-rows"`) with `on_drag` for the next frame, whose next move
//! starts GPUI's drag. The wrapper carries an empty `on_click` while the
//! gesture is enabled, so GPUI records the press it needs. The armed drag
//! goes on release, or in a frame with no drag and no press.
//!
//! The target: the wrapper follows every row drag (`on_drag_move`) and the
//! application's types it registered: over the rows the [`RowDropPolicy`]
//! places the drag by the line painted under the pointer, the frame draws
//! the target by its key, the rows scroll near an edge, and the cursor says
//! whether the grid takes it. A drop places it again where it lands and
//! emits `LiveGridEvent::Drop`. GPUI has no "drag ended" callback: the
//! target goes when the pointer leaves, on any release, and in a frame with
//! no drag.

use std::{any::Any, fmt, rc::Rc, sync::Arc};

use gpui_kit::base::AutoScroll;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, CursorStyle, Div, DragMoveEvent, EntityId,
    Hsla, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Pixels, Point,
    Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div,
    point, prelude::FluentBuilder as _, px, relative, size,
};
use rust_i18n::t;

use super::{
    CELL_PADDING, KeyOf, LiveGridDelegate, LiveGridEvent, LiveGridState,
    frame::PaintedLine,
    ghost::GhostTextStyle,
    pinned::BodyLine,
    selection::LineTarget,
    stats::{profile_elapsed, profile_start},
};
use crate::{
    engine::GroupPath,
    model::GridRow,
    policy::{
        DropLine, NoRowDrag, NoRowDrop, RowDragGesture, RowDragPressContext, RowDropContext,
        RowDropPolicy, SelectableLine,
    },
    store::StoreSnapshot,
};

/// The payload of a row drag: what GPUI carries while rows are dragged out
/// of a grid. A drop target outside grids takes it with
/// `on_drop::<RowDrag<K>>`.
#[derive(Clone)]
pub struct RowDrag<K> {
    grid: EntityId,
    keys: Arc<[K]>,
    payload: DragPayload,
}

impl<K> RowDrag<K> {
    /// The grid the rows come from.
    pub fn grid(&self) -> EntityId {
        self.grid
    }

    /// The dragged rows in the shown order: the command scope of the press,
    /// taken when the drag started.
    pub fn keys(&self) -> &Arc<[K]> {
        &self.keys
    }

    /// What the application added (`LiveGridDelegate::drag_payload`).
    pub fn payload(&self) -> &DragPayload {
        &self.payload
    }
}

impl<K> fmt::Debug for RowDrag<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RowDrag")
            .field("grid", &self.grid)
            .field("keys", &self.keys.len())
            .field("payload", &self.payload)
            .finish()
    }
}

/// Application data riding along a drag, and the label of its ghost.
#[derive(Clone, Default)]
pub struct DragPayload {
    data: Option<Arc<dyn Any>>,
    label: Option<SharedString>,
}

impl DragPayload {
    /// An empty payload: the keys alone are dragged.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds data for the drop target, read back with [`Self::data`].
    pub fn with_data<T: 'static>(mut self, data: T) -> Self {
        self.data = Some(Arc::new(data));
        self
    }

    /// The data, if it is a `T`.
    pub fn data<T: 'static>(&self) -> Option<&T> {
        self.data.as_deref()?.downcast_ref()
    }

    /// Sets the text of the drag's ghost ("3 orders"); without one the
    /// ghost says how many rows ("3 rows").
    pub fn with_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The ghost's text, if set.
    pub fn label(&self) -> Option<&SharedString> {
        self.label.as_ref()
    }
}

impl fmt::Debug for DragPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DragPayload")
            .field("label", &self.label)
            .field("has_data", &self.data.is_some())
            .finish()
    }
}

/// What was dropped on a grid.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DropSource<K> {
    /// Rows of a grid with this key type: this grid or another.
    Rows(RowDrag<K>),
    /// A value of a type the grid takes (`with_drop_payload::<T>()`), as its
    /// data ([`DragPayload::data`]).
    Payload(DragPayload),
}

/// Where a drop lands, by identity.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropTarget<K> {
    /// A data row (a tree row too), and where relative to it.
    Row {
        /// The row.
        key: K,
        /// Before, after or onto it.
        place: DropPlace,
    },
    /// A group caption.
    Group(GroupPath),
    /// The empty area below the rows, or an empty grid.
    Empty,
}

/// Where a drop lands relative to a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DropPlace {
    /// Above the row.
    Before,
    /// Below the row.
    After,
    /// Onto the row.
    Onto,
}

/// What [`LiveGridDelegate::drag_payload`](super::LiveGridDelegate::drag_payload)
/// knows: the rows about to be dragged.
pub struct RowDragContext<'a, R: GridRow> {
    keys: &'a [R::Key],
    pressed: &'a R::Key,
    snapshot: &'a StoreSnapshot<R>,
    modifiers: Modifiers,
}

impl<'a, R: GridRow> RowDragContext<'a, R> {
    /// The rows to drag, in the shown order (the command scope).
    pub fn keys(&self) -> &'a [R::Key] {
        self.keys
    }

    /// The row the press was on.
    pub fn pressed(&self) -> &'a R::Key {
        self.pressed
    }

    /// A row of the snapshot shown when the drag started.
    pub fn row(&self, key: &R::Key) -> Option<&'a Arc<R>> {
        self.snapshot.row(self.snapshot.row_id(key)?)
    }

    /// The modifiers held.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
}

/// Adds the listeners of one of the application's drop types to the rows'
/// wrapper (`with_drop_payload`).
type DropRegistrar<D> = Rc<dyn Fn(Stateful<Div>, &mut Context<LiveGridState<D>>) -> Stateful<Div>>;

/// What a grid keeps for row drags: the gesture and the drag armed past its
/// threshold; the drop policy, the application's drop types, and the drag
/// over the rows with its target.
pub(super) struct DragSlot<D: LiveGridDelegate> {
    gesture: Box<dyn RowDragGesture<D::Row>>,
    /// Set past the gesture's threshold, until the release: the rows'
    /// wrapper carries it as `on_drag`.
    armed: Option<RowDrag<KeyOf<D>>>,
    drop_policy: Box<dyn RowDropPolicy<D::Row>>,
    payloads: Vec<DropRegistrar<D>>,
    /// The drag over the rows, from its first move inside them until it
    /// leaves, drops or ends.
    hover: Option<DropHover<KeyOf<D>>>,
    /// The target of the hovering drag wants placing again from the lines a
    /// frame paints: the rows scrolled under the pointer.
    retarget: bool,
    auto_scroll: AutoScroll,
}

impl<D: LiveGridDelegate> Default for DragSlot<D> {
    fn default() -> Self {
        Self {
            gesture: Box::new(NoRowDrag),
            armed: None,
            drop_policy: Box::new(NoRowDrop),
            payloads: Vec::new(),
            hover: None,
            retarget: false,
            auto_scroll: AutoScroll::default(),
        }
    }
}

impl<D: LiveGridDelegate> DragSlot<D> {
    /// Returns true if a press may drag rows: the wrapper then records
    /// presses.
    pub(super) fn is_enabled(&self) -> bool {
        self.gesture.is_enabled()
    }

    /// Returns true if `press` may become a row drag.
    pub(super) fn is_drag_press(&self, press: &RowDragPressContext<'_, D::Row>) -> bool {
        self.gesture.is_enabled() && self.gesture.is_drag_press(press)
    }

    /// How far the pointer travels from the press before the drag starts.
    pub(super) fn threshold(&self) -> Pixels {
        self.gesture.threshold()
    }

    /// Drops the armed drag: the press ended.
    pub(super) fn unarm(&mut self) {
        self.armed = None;
    }

    /// Returns true while a drag is over the rows (`ReorderContext::
    /// is_drag_over`).
    pub(super) fn is_over(&self) -> bool {
        self.hover.is_some()
    }
}

/// What is dragged over the rows: rows of a grid with this key type, or a
/// value of a registered type, cloned once when it came over the rows.
#[derive(Clone)]
enum Dragged<K> {
    Rows(RowDrag<K>),
    Value(Rc<dyn Any>),
}

impl<K> Dragged<K> {
    fn as_ref(&self) -> DraggedRef<'_, K> {
        match self {
            Self::Rows(drag) => DraggedRef::Rows(drag),
            Self::Value(value) => DraggedRef::Value(value.as_ref()),
        }
    }
}

/// [`Dragged`], borrowed: what a drop hands over.
#[derive(Clone, Copy)]
enum DraggedRef<'a, K> {
    Rows(&'a RowDrag<K>),
    Value(&'a dyn Any),
}

/// A drag over the rows: what it carries, where it was last, and where it
/// would land.
struct DropHover<K> {
    dragged: Dragged<K>,
    own: bool,
    position: Point<Pixels>,
    modifiers: Modifiers,
    target: Option<DropTarget<K>>,
}

/// Returns true if `modifiers` ask to copy rather than move: Option on
/// macOS, Ctrl elsewhere, as the platforms' file managers do. Both count,
/// so the code is the same everywhere.
/// The ghost of a row drag: a chip with the payload's label, or "3 rows",
/// right of and below the pointer, in the rows' font. GPUI draws the
/// ghost's view at the pointer less `offset`, the press's offset in the
/// rows' wrapper, so the chip moves by it.
struct RowDragGhost {
    label: SharedString,
    height: Pixels,
    offset: Point<Pixels>,
    text: GhostTextStyle,
}

impl Render for RowDragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.text
            .root()
            .pl(self.offset.x + px(12.))
            .pt(self.offset.y + px(12.))
            .child(
                div()
                    .h(self.height)
                    .max_w(px(320.))
                    .px(CELL_PADDING)
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .bg(theme.table_head)
                    .text_color(theme.table_head_foreground)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_md()
                    .opacity(0.9)
                    .child(div().min_w(px(0.)).truncate().child(self.label.clone())),
            )
    }
}

/// The ghost's text: the payload's label, else how many rows.
fn ghost_label<K>(drag: &RowDrag<K>) -> SharedString {
    if let Some(label) = drag.payload().label() {
        return label.clone();
    }
    match drag.keys().len() {
        1 => t!("live_grid.drag.row").into(),
        count => t!("live_grid.drag.rows", count = count).into(),
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets which presses drag rows (default [`NoRowDrag`]: none;
    /// [`SelectedRowsDrag`](crate::SelectedRowsDrag) is LiveGrid's gesture).
    pub fn with_row_drag_gesture(mut self, gesture: impl RowDragGesture<D::Row>) -> Self {
        self.drag.gesture = Box::new(gesture);
        self
    }

    /// Sets where drops on the rows land (default [`NoRowDrop`]: the grid
    /// takes none; [`DropOnRows`](crate::DropOnRows) and
    /// [`DropBetweenRows`](crate::DropBetweenRows) are built in).
    pub fn with_row_drop_policy(mut self, policy: impl RowDropPolicy<D::Row>) -> Self {
        self.drag.drop_policy = Box::new(policy);
        self
    }

    /// Takes drops of `T`, a drag payload of the application's own, besides
    /// the rows of grids with this key type: an element with
    /// `on_drag(T)` dropped on the rows arrives as `DropSource::Payload`,
    /// the value cloned as its data. The drop policy places it, reading the
    /// value with `RowDropContext::payload`.
    pub fn with_drop_payload<T: Clone + 'static>(mut self) -> Self {
        self.drag.payloads.push(Rc::new(|this, cx| {
            this.on_drag_move(cx.listener(Self::on_payload_drag_move::<T>))
                .on_drop(cx.listener(Self::on_payload_drop::<T>))
        }));
        self
    }

    /// Returns true while a row drag from this grid is in progress: from the
    /// gesture's threshold to the release.
    pub fn is_dragging_rows(&self) -> bool {
        self.drag.armed.is_some()
    }

    /// The row drag from this grid in progress, if any. For tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn row_drag(&self) -> Option<&RowDrag<KeyOf<D>>> {
        self.drag.armed.as_ref()
    }

    /// Asks the gesture whether the primary press on the data row of `key`,
    /// at `column`, may drag rows.
    pub(super) fn is_row_drag_press(
        &self,
        key: &KeyOf<D>,
        column: super::ColumnOf<D>,
        was_selected: bool,
        modifiers: Modifiers,
        click_count: usize,
    ) -> bool {
        if !self.drag.is_enabled() {
            return false;
        }
        let Some(id) = self.snapshot.row_id(key) else {
            return false;
        };
        let Some(row) = self.snapshot.row(id) else {
            return false;
        };
        let line = SelectableLine::Row {
            row: &**row,
            branch: self.projection.branch(id),
        };
        self.drag.is_drag_press(&RowDragPressContext::new(
            line,
            column,
            was_selected,
            modifiers,
            self.selection.mode(),
            click_count,
        ))
    }

    /// The pointer passed the gesture's threshold from a press on the row
    /// of `pressed`: the pressed row's command scope and the delegate's
    /// payload arm the drag for the next frame. Returns false, arming
    /// nothing, if the row left the store or the delegate refused.
    pub(super) fn arm_row_drag(
        &mut self,
        pressed: &KeyOf<D>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.snapshot.row_id(pressed).is_none() {
            return false;
        }
        let keys = self
            .command_scope(Some(&LineTarget::Row(pressed.clone())))
            .shared_keys();
        let payload = self.delegate.drag_payload(
            &RowDragContext {
                keys: &keys,
                pressed,
                snapshot: &self.snapshot,
                modifiers,
            },
            cx,
        );
        let Some(payload) = payload else {
            return false;
        };
        self.drag.armed = Some(RowDrag {
            grid: cx.entity_id(),
            keys,
            payload,
        });
        cx.notify();
        true
    }

    /// Drops the armed drag when the press is over.
    pub(super) fn unarm_row_drag(&mut self) {
        self.drag.unarm();
    }

    /// A frame of a grid whose armed drag GPUI no longer carries and whose
    /// press is over (a release the grid did not see): the drag goes.
    pub(super) fn settle_row_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.armed.is_some() && self.press.is_none() && !cx.has_active_drag() {
            self.drag.unarm();
        }
        if self.drag.hover.is_some() && !cx.has_active_drag() {
            self.end_drop_hover(cx);
        }
    }

    /// The wrapper of the rows (`"live-grid-rows"`) around
    /// `body`: it takes the body's place in the layout. While the drag
    /// gesture is enabled it carries an empty `on_click`, so GPUI records
    /// a press on the rows; armed, its `on_drag` starts the drag on the next
    /// move, and the ghost draws in `ghost_text`. A div without a role: no
    /// accessibility node between the grid and its rows. It takes no wheel:
    /// the body scrolls as before.
    pub(super) fn render_rows(
        &self,
        body: impl IntoElement,
        ghost_text: GhostTextStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_height = self.row_height;
        div()
            .id("live-grid-rows")
            .flex()
            .flex_col()
            .flex_grow(1.)
            .flex_shrink(1.)
            .flex_basis(relative(0.))
            .w_full()
            .when(self.drag.is_enabled(), |this| this.on_click(|_, _, _| {}))
            // Every row drag: a target shows where it lands, or that it
            // does not, and the source its own cursor.
            .on_drag_move(cx.listener(Self::on_row_drag_move))
            .when(self.drag.drop_policy.is_enabled(), |this| {
                this.on_drop(cx.listener(Self::on_rows_drop))
            })
            .map(|this| {
                self.drag
                    .payloads
                    .iter()
                    .fold(this, |this, register| register(this, cx))
            })
            // Armed, the drag starts with the closed hand (GPUI takes the
            // cursor of the element it starts from).
            .when_some(self.drag.armed.clone(), |this, drag| {
                this.cursor(CursorStyle::ClosedHand).on_drag(
                    drag,
                    move |drag: &RowDrag<KeyOf<D>>, offset, _: &mut Window, cx: &mut App| {
                        let label = ghost_label(drag);
                        cx.new(|_| RowDragGhost {
                            label,
                            height: row_height,
                            offset,
                            text: ghost_text.clone(),
                        })
                    },
                )
            })
            .child(body)
            .into_any_element()
    }

    /// A row drag moved anywhere in the window.
    fn on_row_drag_move(
        &mut self,
        event: &DragMoveEvent<RowDrag<KeyOf<D>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (position, modifiers) = (event.event.position, event.event.modifiers);
        let inside = event.bounds.contains(&position);
        if !inside && self.drag.hover.is_none() {
            return;
        }
        let drag = event.drag(cx).clone();
        let own = drag.grid() == cx.entity_id();
        self.follow_drag(
            Dragged::Rows(drag),
            own,
            position,
            modifiers,
            inside,
            window,
            cx,
        );
    }

    /// A drag of the application's type `T` moved anywhere in the window.
    /// The value is cloned once, when it comes over the rows.
    fn on_payload_drag_move<T: Clone + 'static>(
        &mut self,
        event: &DragMoveEvent<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (position, modifiers) = (event.event.position, event.event.modifiers);
        let inside = event.bounds.contains(&position);
        if !inside && self.drag.hover.is_none() {
            return;
        }
        let value = match &self.drag.hover {
            Some(DropHover {
                dragged: Dragged::Value(value),
                ..
            }) if value.is::<T>() => value.clone(),
            _ => Rc::new(event.drag(cx).clone()) as Rc<dyn Any>,
        };
        self.follow_drag(
            Dragged::Value(value),
            false,
            position,
            modifiers,
            inside,
            window,
            cx,
        );
    }

    /// A drag moved: over the rows its target and the cursor follow, and
    /// near an edge the rows scroll; off them the target goes.
    #[allow(clippy::too_many_arguments)]
    fn follow_drag(
        &mut self,
        dragged: Dragged<KeyOf<D>>,
        own: bool,
        position: Point<Pixels>,
        modifiers: Modifiers,
        inside: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !inside {
            // Off this grid: the drag shows the closed hand again until
            // another target says otherwise.
            self.end_drop_hover(cx);
            cx.set_active_drag_cursor_style(CursorStyle::ClosedHand, window);
            return;
        }
        let started = profile_start();
        let target = self.row_drop_target_at(dragged.as_ref(), own, position, modifiers);
        self.stats.last_drop_target = profile_elapsed(started);
        let cursor = match &target {
            Some(_) if self.drag.drop_policy.is_copy(modifiers) => CursorStyle::DragCopy,
            Some(_) => CursorStyle::ClosedHand,
            // Over its own rows the source keeps the closed hand.
            None if own => CursorStyle::ClosedHand,
            None => CursorStyle::OperationNotAllowed,
        };
        // After every grid saw this move: a grid the pointer left resets
        // the cursor at once, and the grid under it has the last word,
        // whatever their order.
        window.defer(cx, move |window, cx| {
            if cx.active_drag_cursor_style() != Some(cursor) {
                cx.set_active_drag_cursor_style(cursor, window);
            }
        });
        let changed = self.drag.hover.as_ref().map(|hover| &hover.target) != Some(&target);
        let entered = self.drag.hover.is_none();
        self.drag.hover = Some(DropHover {
            dragged,
            own,
            position,
            modifiers,
            target,
        });
        if self.drag.drop_policy.is_enabled() {
            let delta = AutoScroll::compute_delta(position.y, self.rows_bounds());
            self.drag.auto_scroll.last_drag_position = Some(position);
            self.drag.auto_scroll.set(delta, cx, |delta, state, cx| {
                state.auto_scroll_drop(delta, cx)
            });
        }
        if changed || entered {
            cx.notify();
        }
    }

    /// One auto-scroll step of a drag over the rows: the next frame places
    /// the drag again on the lines it paints.
    fn auto_scroll_drop(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        if self.drag.hover.is_none() {
            self.drag.auto_scroll.stop();
            return;
        }
        let before = (self.scroll_row, self.scroll_offset);
        self.scroll_rows_by(delta);
        if (self.scroll_row, self.scroll_offset) != before {
            self.drag.retarget = true;
            cx.notify();
        }
    }

    /// Places the hovering drag again, after the rows scrolled under it:
    /// called by the frame once its lines are painted.
    pub(super) fn retarget_drop(&mut self) {
        if !std::mem::take(&mut self.drag.retarget) {
            return;
        }
        let Some(hover) = &self.drag.hover else {
            return;
        };
        let target = self.row_drop_target_at(
            hover.dragged.as_ref(),
            hover.own,
            hover.position,
            hover.modifiers,
        );
        if let Some(hover) = &mut self.drag.hover {
            hover.target = target;
        }
    }

    /// The drag left the rows, dropped or ended: its target and the
    /// auto-scroll go, and a held order is asked about again.
    pub(crate) fn end_drop_hover(&mut self, cx: &mut Context<Self>) {
        self.drag.auto_scroll.stop();
        self.drag.retarget = false;
        if self.drag.hover.take().is_some() {
            self.reconsider_held(cx);
            cx.notify();
        }
    }

    /// Where a drag at a window position lands, as the drop policy places
    /// it; `None` refuses it. Pinned bands, group footers and totals refuse
    /// without asking the policy; a row fading out and the area below the
    /// rows are `DropLine::Empty`. O(1).
    fn row_drop_target_at(
        &self,
        dragged: DraggedRef<'_, KeyOf<D>>,
        own: bool,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Option<DropTarget<KeyOf<D>>> {
        let policy = &self.drag.drop_policy;
        if !policy.is_enabled() {
            return None;
        }
        let BodyLine::Data(line) = self.body_line_at(position)? else {
            return None;
        };
        let rows = self.rows_bounds();
        let y = position.y - rows.origin.y + self.scroll_offset;
        let offset = ((y - self.row_height * line as f32) / self.row_height).clamp(0., 1.);
        let (snapshot, projection) = (&self.snapshot, &self.projection);
        let drop_line = match self.painted.get(line) {
            Some(PaintedLine::Row(id)) => match snapshot.row(*id) {
                Some(row) => DropLine::Row {
                    key: row.key(),
                    branch: projection.branch(*id),
                },
                None => DropLine::Empty,
            },
            Some(PaintedLine::Caption(path)) => DropLine::Caption(projection.groups().find(path)?),
            Some(PaintedLine::Footer(_)) => return None,
            Some(PaintedLine::Ghost(_)) | None => DropLine::Empty,
        };
        // The last data row shown, for a drop below the rows.
        let last_row = matches!(drop_line, DropLine::Empty)
            .then(|| {
                (0..projection.len())
                    .rev()
                    .filter_map(|ix| projection.row(ix))
                    .find_map(|id| snapshot.row(id))
                    .map(|row| row.key())
            })
            .flatten();
        let (rows_drag, value) = match dragged {
            DraggedRef::Rows(drag) => (Some(drag), None),
            DraggedRef::Value(value) => (None, Some(value)),
        };
        policy.place(&RowDropContext::new(
            drop_line,
            offset,
            rows_drag,
            value,
            own,
            modifiers,
            (
                !self.view.sort().is_empty(),
                projection.is_grouped(),
                projection.is_tree(),
            ),
            last_row,
            snapshot,
        ))
    }

    /// Rows were dropped on the rows: placed again where they landed, one
    /// `Drop` if the policy takes them.
    fn on_rows_drop(
        &mut self,
        drag: &RowDrag<KeyOf<D>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let own = drag.grid() == cx.entity_id();
        let source = DropSource::Rows(drag.clone());
        self.drop_on_rows(DraggedRef::Rows(drag), own, source, window, cx);
    }

    /// A value of the application's type `T` was dropped on the rows.
    fn on_payload_drop<T: Clone + 'static>(
        &mut self,
        value: &T,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = DropSource::Payload(DragPayload::new().with_data(value.clone()));
        self.drop_on_rows(DraggedRef::Value(value), false, source, window, cx);
    }

    fn drop_on_rows(
        &mut self,
        dragged: DraggedRef<'_, KeyOf<D>>,
        own: bool,
        source: DropSource<KeyOf<D>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (position, modifiers) = (window.mouse_position(), window.modifiers());
        let target = self.row_drop_target_at(dragged, own, position, modifiers);
        self.end_drop_hover(cx);
        if let Some(target) = target {
            cx.emit(LiveGridEvent::Drop {
                source,
                target,
                modifiers,
            });
        }
    }

    /// The indicator of the hovering drag's target, by its key on the lines
    /// this frame painted: a row or a caption filled and framed, a 2 px line
    /// between rows, under the last row for the empty area, the rows' frame
    /// in an empty grid. Across every zone, within `rows`.
    pub(super) fn drop_indicator(
        &self,
        rows: Bounds<Pixels>,
        fill_color: Hsla,
        border: Hsla,
    ) -> Vec<(Bounds<Pixels>, Hsla)> {
        let Some(target) = self
            .drag
            .hover
            .as_ref()
            .and_then(|hover| hover.target.as_ref())
        else {
            return Vec::new();
        };
        let top = |line: usize| rows.origin.y + self.row_height * line as f32 - self.scroll_offset;
        let line_of =
            |painted: &dyn Fn(&PaintedLine) -> bool| self.painted.iter().position(painted);
        let mut quads = Vec::new();
        let bar = |y: Pixels, quads: &mut Vec<_>| {
            quads.push((
                Bounds::new(
                    point(rows.origin.x, y - px(1.)),
                    size(rows.size.width, px(2.)),
                ),
                border,
            ));
        };
        let frame = |area: Bounds<Pixels>, fill: Option<Hsla>, quads: &mut Vec<_>| {
            if let Some(color) = fill {
                quads.push((area, color));
            }
            let (w, h, b) = (area.size.width, area.size.height, px(2.));
            let origin = area.origin;
            quads.extend([
                (Bounds::new(origin, size(w, b)), border),
                (
                    Bounds::new(point(origin.x, origin.y + h - b), size(w, b)),
                    border,
                ),
                (Bounds::new(origin, size(b, h)), border),
                (
                    Bounds::new(point(origin.x + w - b, origin.y), size(b, h)),
                    border,
                ),
            ]);
        };
        let line_area = |line: usize| {
            Bounds::new(
                point(rows.origin.x, top(line)),
                size(rows.size.width, self.row_height),
            )
        };
        match target {
            DropTarget::Row { key, place } => {
                let Some(id) = self.snapshot.row_id(key) else {
                    return quads;
                };
                let Some(line) = line_of(&|painted| painted.row() == Some(id)) else {
                    return quads;
                };
                match place {
                    DropPlace::Before => bar(top(line), &mut quads),
                    DropPlace::After => bar(top(line + 1), &mut quads),
                    _ => frame(line_area(line), Some(fill_color), &mut quads),
                }
            }
            DropTarget::Group(path) => {
                if let Some(line) = line_of(&|painted| painted.group() == Some(path)) {
                    frame(line_area(line), Some(fill_color), &mut quads);
                }
            }
            _ => {
                let shown = self
                    .painted
                    .iter()
                    .rposition(|painted| !matches!(painted, PaintedLine::Ghost(_)));
                match shown {
                    Some(last) => bar(top(last + 1), &mut quads),
                    None => frame(rows, None, &mut quads),
                }
            }
        }
        // Within the rows' area: a line scrolled half out is cut.
        quads
            .into_iter()
            .filter_map(|(bounds, color)| {
                let bounds = bounds.intersect(&rows);
                (bounds.size.height > px(0.) && bounds.size.width > px(0.))
                    .then_some((bounds, color))
            })
            .collect()
    }

    /// Escape during a row drag from this grid: GPUI drops it, no `Drop`
    /// comes, and the selection stays. Returns false without one.
    pub(super) fn cancel_row_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.drag.armed.is_none() {
            return false;
        }
        self.drag.unarm();
        cx.stop_active_drag(window);
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drag_payload() {
        let payload = DragPayload::new();
        assert!(payload.label().is_none() && payload.data::<u32>().is_none());
        let payload = payload.with_data(7u32).with_label("3 orders");
        assert_eq!(payload.data::<u32>(), Some(&7));
        assert_eq!(payload.data::<i64>(), None);
        assert_eq!(payload.label().map(AsRef::as_ref), Some("3 orders"));
        let debug = format!("{payload:?}");
        assert!(debug.contains("3 orders") && debug.contains("has_data: true"));
        assert_ne!(
            DropTarget::Row {
                key: 1,
                place: DropPlace::Before
            },
            DropTarget::Empty
        );
    }
}
