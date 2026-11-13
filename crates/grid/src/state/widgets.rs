//! Elements in cells: what the delegate's `render_cell` knows about a cell
//! of an `Element` or `Control` column, the host that places its element in
//! the body, presses on it, and Space on a control.
//!
//! A host covers its cell for the mouse (`block_mouse_except_scroll`): the
//! body does not see a press on it. The host reports the press itself, in
//! two phases. In the capture phase it records the press ([`CellPress`]);
//! in the bubble phase, unless the element stopped the press, the press is
//! a press on the cell: selection, drag-selection, double click, menu and
//! `Click`, as on any cell. After the press is dispatched, a press the
//! element stopped moves only the grid's focus to the cell, and the
//! keyboard focus goes back to the grid's root: the grid is one Tab stop.

use std::{any::Any, cell::RefCell, rc::Rc, sync::Arc};

use gpui_kit::component::Size as ComponentSize;
use gpui_kit::{
    App, Context, InteractiveElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Role, SharedString, Size, StatefulInteractiveElement as _, Styled, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px, size,
};

use super::{
    CELL_PADDING, CellChange, ColumnAlign, LiveGridDelegate, LiveGridState,
    a11y::column_index,
    columns::CellContent,
    edit::EditState,
    editors::{CellCommit, CellEditHandle, GridCommit},
    frame::{HostCell, RowPlanner},
    selection::FocusedCell,
};
use crate::{
    actions::ActivateCell,
    element::{CellHost, HostLayer},
    model::{CellValue, GridColumns as _, GridRow, display_text_in},
    store::PinnedEdge,
};

/// What `ActivateCell` (Space on a focused `Control` cell) calls: the
/// handler the cell's element registered in the last frame.
pub(crate) type CellActivation = Rc<dyn Fn(&mut Window, &mut App)>;

/// The handler of Space on the focused cell of a `Control` column, from
/// the last frame, with the cell it belongs to: a key moved before the
/// next frame does not press another cell's element.
pub(super) struct ActiveCell<K, C> {
    key: K,
    column: C,
    handler: CellActivation,
}

/// A press on a cell host, recorded in the capture phase before the
/// element sees it.
pub(super) struct CellPress<K, C> {
    key: K,
    column: C,
    /// The band of a pinned row: a press there takes no focus.
    pinned: Option<PinnedEdge>,
    button: MouseButton,
    /// The element did not stop the press: it was a press on the cell.
    accepted: bool,
}

/// What a grid keeps for the elements in its cells: the press on a host
/// being dispatched, and the handler `ActivateCell` calls.
pub(super) struct WidgetSlot<K, C> {
    press: Option<CellPress<K, C>>,
    /// Set while the focused `Control` cell is planned; cleared when a
    /// frame begins.
    activation: RefCell<Option<ActiveCell<K, C>>>,
    /// The grid as every `CellEditHandle` commits to it: made once, shared.
    commit: RefCell<Option<Rc<dyn CellCommit>>>,
}

impl<K, C> Default for WidgetSlot<K, C> {
    fn default() -> Self {
        Self {
            press: None,
            activation: RefCell::new(None),
            commit: RefCell::new(None),
        }
    }
}

impl<K, C> WidgetSlot<K, C> {
    /// A frame begins: the last frame's handler of Space goes; the focused
    /// control registers its own again.
    pub(super) fn begin_frame(&self) {
        self.activation.borrow_mut().take();
    }

    /// The grid as cell elements commit to it: made on first use.
    fn commit<D: LiveGridDelegate>(
        &self,
        grid: &WeakEntity<LiveGridState<D>>,
    ) -> Rc<dyn CellCommit> {
        let mut commit = self.commit.borrow_mut();
        Rc::clone(commit.get_or_insert_with(|| {
            Rc::new(GridCommit { grid: grid.clone() }) as Rc<dyn CellCommit>
        }))
    }
}

/// What [`LiveGridDelegate::render_cell`] knows about a visible cell of an
/// `Element` or `Control` column.
pub struct CellElementContext<'a, R: GridRow> {
    row: &'a Arc<R>,
    column: R::Column,
    value: CellValue<'a>,
    text: &'a str,
    change: Option<CellChange>,
    size: ComponentSize,
    cell_size: Size<Pixels>,
    selected: bool,
    focused: bool,
    /// The cell's edits.
    edit_state: Option<&'a EditState>,
    /// Where `on_activate` keeps its handler: only for the focused cell of
    /// a `Control` column.
    activation: Option<&'a RefCell<Option<ActiveCell<R::Key, R::Column>>>>,
    /// The grid, for `edit`: only in a column with an editor, off the
    /// pinned rows.
    commit: Option<Rc<dyn CellCommit>>,
}

impl<'a, R: GridRow> CellElementContext<'a, R> {
    /// The row, as this frame shows it.
    pub fn row(&self) -> &'a R {
        self.row
    }

    /// The row, shared: for an element's handlers, which run after the
    /// frame and outlive it.
    pub fn shared_row(&self) -> &'a Arc<R> {
        self.row
    }

    /// The row's key.
    pub fn key(&self) -> &'a R::Key {
        self.row.key()
    }

    /// The column.
    pub fn column(&self) -> R::Column {
        self.column
    }

    /// The cell's value: the store's, or the value a commit of the cell
    /// still waits with.
    pub fn value(&self) -> CellValue<'a> {
        self.value
    }

    /// The text the body would paint for the value.
    pub fn text(&self) -> &'a str {
        self.text
    }

    /// The cell's highlighted change, if it is hot: for an element that
    /// shows the change itself. On a removed row that fades out it is
    /// [`RowChange::Removed`](crate::RowChange::Removed) at the fade's
    /// intensity: the element is drawn until the fade ends, and takes no
    /// press, Space or edit.
    pub fn change(&self) -> Option<CellChange> {
        self.change
    }

    /// The grid's size: gpui-kit elements take it through `Sizable` and fit
    /// the row.
    pub fn size(&self) -> ComponentSize {
        self.size
    }

    /// The room for the element: the cell without its padding.
    pub fn cell_size(&self) -> Size<Pixels> {
        self.cell_size
    }

    /// Returns true if the cell is selected.
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Returns true if the cell has the grid's focus.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// The cell's edits: a commit that waits for an answer (`Pending`) or
    /// for the store (`Accepted`) — [`Self::value`] is its value then — or a
    /// refused one (`Failed`). `None` for a cell with no edits.
    pub fn edit_state(&self) -> Option<&'a EditState> {
        self.edit_state
    }

    /// A handle that commits values of this cell as edits, for the
    /// element's handlers, such as a checkbox's `on_click` and
    /// `on_activate`: `EditCommit`, then the value shown until the commit
    /// settles. `None` if the column has no editor, or on a pinned row.
    /// Whether the delegate's `is_editable` takes the cell is asked at the
    /// commit, outside the frame.
    pub fn edit(&self) -> Option<CellEditHandle> {
        let commit = self.commit.as_ref()?;
        let row: Arc<dyn Any> = Arc::clone(self.row) as Arc<dyn Any>;
        Some(CellEditHandle::new(
            Rc::clone(commit),
            row,
            self.column.ix(),
        ))
    }

    /// Sets what Space does on the focused cell of a `Control` column
    /// (`ActivateCell`): what a press on the element does, such as
    /// toggling a checkbox. Kept for the focused cell only, until the next
    /// frame; on any other cell it is dropped without being stored. It runs
    /// after the key is handled, outside the grid's update, so it may
    /// update the grid.
    pub fn on_activate(&self, handler: impl Fn(&mut Window, &mut App) + 'static) {
        if let Some(activation) = self.activation {
            *activation.borrow_mut() = Some(ActiveCell {
                key: self.row.key().clone(),
                column: self.column,
                handler: Rc::new(handler),
            });
        }
    }
}

/// The component size of a grid with rows of `row_height`: what `LiveGrid`
/// sizes its rows from (`Size::table_row_height`).
fn component_size(row_height: Pixels) -> ComponentSize {
    [
        ComponentSize::XSmall,
        ComponentSize::Small,
        ComponentSize::Medium,
        ComponentSize::Large,
    ]
    .into_iter()
    .find(|size| size.table_row_height() == row_height)
    .unwrap_or(ComponentSize::Size(row_height))
}

/// What a host tells a screen reader about its cell, while the tree is
/// built: it is the cell's `GridCell` node.
struct HostA11y {
    /// The value's text for an `Element`; none for a `Control`, whose
    /// element names itself and tells its state.
    label: Option<SharedString>,
    /// The column title, as on a synthetic cell.
    description: SharedString,
    column_index: usize,
    /// Set when the selection is made of cells.
    selected: Option<bool>,
}

impl<D: LiveGridDelegate> RowPlanner<'_, D> {
    /// Plans the element of a cell of an `Element` or `Control` column
    /// `render_cell`, and the host around its element.
    /// Returns true if the cell is hosted: the body paints no text there;
    /// false if the delegate gave no element.
    ///
    /// A removed row that fades out keeps its elements, so they fade with
    /// its text (`CellElementContext::change` is its removal). They take no
    /// press: a cover over the element passes the press to the body, which
    /// finds no row there, as on the fading row's text cells.
    pub(super) fn plan_cell_host(
        &mut self,
        cell: &HostCell<'_, D::Row>,
        window: &mut Window,
    ) -> bool {
        let row = cell.row;
        let live = row.is_live();
        let bounds = cell.host_bounds();
        let control = cell.content == CellContent::Control;
        let widgets = self.widgets;
        // Only the focused control keeps a handler of Space; a pinned row
        // takes no focus.
        let activation = (live && control && cell.focused && row.pinned.is_none())
            .then_some(&widgets.activation);
        let delegate = self.delegate;
        self.host_text.clear();
        let text = display_text_in(cell.value, cell.number_format, &mut self.host_text);
        let context = CellElementContext {
            row: row.row,
            column: cell.column,
            value: cell.value,
            text,
            change: cell.change,
            size: component_size(self.row_height),
            cell_size: size(
                (bounds.size.width - CELL_PADDING * 2.).max(px(0.)),
                bounds.size.height,
            ),
            selected: cell.selected,
            focused: cell.focused,
            edit_state: cell.edit_state,
            activation,
            commit: (live && self.edit.has_editor(cell.column.ix()) && row.pinned.is_none())
                .then(|| widgets.commit(&self.grid)),
        };
        let Some(element) = delegate.render_cell(&context, window, self.app) else {
            return false;
        };
        let a11y = self.a11y.as_ref().map(|input| HostA11y {
            label: (!control).then(|| SharedString::from(text.to_owned())),
            description: cell.title.clone(),
            column_index: column_index(cell.view),
            selected: input.cell_selected(cell.selected),
        });
        let column = cell.column;
        if !live {
            let grid = self.grid.clone();
            self.hosts.push(CellHost {
                layer: HostLayer::Widget,
                column: column.ix(),
                zone: cell.zone,
                bounds,
                element: CellHost::host_div(HostLayer::Widget, column.id())
                    .relative()
                    .flex()
                    .items_center()
                    .px(CELL_PADDING)
                    .map(|this| align_host(this, cell.align))
                    .child(element)
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .block_mouse_except_scroll()
                            .on_any_mouse_down(move |event, window, cx| {
                                let _ = grid.update(cx, |state, cx| {
                                    state.on_body_mouse_down(event, window, cx);
                                });
                            }),
                    ),
            });
            return true;
        }
        let press = HostPress {
            grid: self.grid.clone(),
            row: Arc::clone(row.row),
            column,
            pinned: row.pinned,
        };
        let element = CellHost::host_div(HostLayer::Widget, column.id())
            .flex()
            .items_center()
            .px(CELL_PADDING)
            .map(|this| align_host(this, cell.align))
            .block_mouse_except_scroll()
            .capture_any_mouse_down({
                let press = press.clone();
                move |event, window, cx| press.record(event, window, cx)
            })
            .on_any_mouse_down(move |event, window, cx| press.accept(event, window, cx))
            .when_some(a11y, |this, a11y| {
                this.role(Role::GridCell)
                    .aria_column_index(a11y.column_index)
                    .aria_description(a11y.description)
                    .when_some(a11y.label, |this, label| this.aria_label(label))
                    .when_some(a11y.selected, |this, selected| this.aria_selected(selected))
            })
            .child(element);
        self.hosts.push(CellHost {
            layer: HostLayer::Widget,
            column: column.ix(),
            zone: cell.zone,
            bounds,
            element,
        });
        true
    }
}

/// Places a host's element by its column's alignment.
fn align_host<E: Styled>(host: E, align: ColumnAlign) -> E {
    match align {
        ColumnAlign::Start => host.justify_start(),
        ColumnAlign::End => host.justify_end(),
        // A column of icons or checkboxes reads as a column.
        ColumnAlign::Auto => host.justify_center(),
    }
}

/// What a host's mouse listeners know: the grid, and the cell by key and
/// column. Cloning it clones two handles.
struct HostPress<D: LiveGridDelegate> {
    grid: WeakEntity<LiveGridState<D>>,
    row: Arc<D::Row>,
    column: <D::Row as GridRow>::Column,
    pinned: Option<PinnedEdge>,
}

impl<D: LiveGridDelegate> Clone for HostPress<D> {
    fn clone(&self) -> Self {
        Self {
            grid: self.grid.clone(),
            row: Arc::clone(&self.row),
            column: self.column,
            pinned: self.pinned,
        }
    }
}

impl<D: LiveGridDelegate> HostPress<D> {
    /// The capture phase: records the press, and settles it once the press
    /// is dispatched.
    fn record(&self, event: &MouseDownEvent, window: &mut Window, cx: &mut App) {
        let press = CellPress {
            key: self.row.key().clone(),
            column: self.column,
            pinned: self.pinned,
            button: event.button,
            accepted: false,
        };
        if self
            .grid
            .update(cx, |state, _| state.widgets.press = Some(press))
            .is_err()
        {
            return;
        }
        let grid = self.grid.clone();
        window.defer(cx, move |window, cx| {
            let _ = grid.update(cx, |state, cx| state.settle_cell_press(window, cx));
        });
    }

    /// The bubble phase, reached when the element did not stop the press:
    /// a press on the cell.
    fn accept(&self, event: &MouseDownEvent, window: &mut Window, cx: &mut App) {
        let _ = self.grid.update(cx, |state, cx| {
            if let Some(press) = &mut state.widgets.press {
                press.accepted = true;
            }
            state.on_body_mouse_down(event, window, cx);
        });
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Binds `ActivateCell`.
    pub(super) fn bind_cell_actions<E: InteractiveElement>(root: E, cx: &Context<Self>) -> E {
        root.on_action(cx.listener(Self::on_activate_cell))
    }

    /// Space on a focused cell of a `Control` column: the handler its
    /// element registered in the last frame, after this update; without
    /// one, the key goes on to `ToggleSelection`.
    fn on_activate_cell(&mut self, _: &ActivateCell, window: &mut Window, cx: &mut Context<Self>) {
        let handler = self.selection.focus().and_then(|focus| {
            let key = focus.row()?;
            let column = focus.column();
            if self.columns.column(column.ix()).content != CellContent::Control {
                return None;
            }
            let activation = self.widgets.activation.borrow();
            activation
                .as_ref()
                .filter(|active| active.key == *key && active.column == column)
                .map(|active| Rc::clone(&active.handler))
        });
        match handler {
            Some(handler) => window.defer(cx, move |window, cx| handler(window, cx)),
            None => cx.propagate(),
        }
    }

    /// A press on a cell host has been dispatched. One the element stopped
    /// moves only the focus to the cell of a data row (no selection, no
    /// drag, no double click, no `Click`); its `FocusChange` comes on
    /// release, at once for another button. Then the keyboard focus goes
    /// back to the grid's root, wherever the element put it.
    fn settle_cell_press(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(press) = self.widgets.press.take() else {
            return;
        };
        if !press.accepted && press.pinned.is_none() {
            self.focus_press(FocusedCell::new(press.key, press.column), cx);
            if press.button != MouseButton::Left {
                self.on_body_mouse_up(cx);
            }
        }
        if !self.is_body_focused(window, cx) {
            #[cfg(debug_assertions)]
            self.warn_tab_stop_in_cell(window, cx);
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Warns once, in a debug build, when an element in a cell took the
    /// keyboard focus and is a Tab stop: Tab would stop on it.
    #[cfg(debug_assertions)]
    fn warn_tab_stop_in_cell(&self, window: &Window, cx: &App) {
        use std::sync::atomic::{AtomicBool, Ordering};

        static WARNED: AtomicBool = AtomicBool::new(false);
        let in_cell = window.focused(cx).is_some_and(|focused| {
            focused.tab_stop && self.focus_handle.contains(&focused, window)
        });
        if in_cell && !WARNED.swap(true, Ordering::Relaxed) {
            log::warn!(
                "an element in a grid cell took the keyboard focus as a Tab stop; \
                 give it `tab_stop(false)`: the grid is one Tab stop"
            );
        }
    }
}
