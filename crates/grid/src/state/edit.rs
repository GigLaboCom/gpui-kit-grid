//! Editing cells: the types of an edit, the editor of a column, what the
//! delegate's edit hooks are told, where the grid keeps the editors and the
//! edit seams, and the edit session.
//!
//! A column takes edits once the application gives it an editor
//! (`with_editor`). An edit starts with F2, Backspace or Delete, typing, a
//! double click or `start_edit`; the editor opens a field in the cell (a
//! host in the body's row, `element.rs`), commits at once, or refuses. One
//! path commits every value (`commit_value`): the unchanged check,
//! `is_editable`, `validate_edit`, then `EditCommit`. A press outside the
//! field and a focus leaving it end the edit as the edit policy decides.
//! The keys of the field are Actions bound in `LiveGridEditor`. The session
//! follows live data too (`follow_edit_rows`, `end_hidden_edit`).

use std::{cell::Cell, rc::Rc};

use gpui_kit::base::{Align, POPUP_PRIORITY, Placement, Positioner, actions::Cancel};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Size as ComponentSize};
use gpui_kit::{
    Anchor, AnyElement, AnyWindowHandle, App, Bounds, Context, Div, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, Role, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored, deferred, div,
    point, prelude::FluentBuilder as _, px, size,
};

use rust_i18n::t;

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridEvent, LiveGridState, ViewColumnIx,
    a11y::data_row_id_name,
    columns::column_of,
    editors::editor_size,
    frame::{HostCell, RowPlanner},
    key_hash,
    selection::{Navigation, is_past_drag_threshold},
};
use crate::{
    actions::{
        CancelEdit, CommitEdit, CommitEditDown, CommitEditNext, CommitEditPrev, CommitEditUp,
        EDITOR_KEY_CONTEXT, StartEdit, StartEditEmpty,
    },
    element::{CellHost, HostLayer, HostPlan},
    model::{CellValue, ColumnIx, ColumnSet, GridColumns, GridRow, OwnedValue, display_text},
    policy::{
        AfterCommit, CellMove, DraftAction, EditCommitKey, EditEnd, EditGesture,
        EditGestureContext, EditPolicy, EditPolicyContext, StandardEditGesture, StandardEditPolicy,
    },
    store::ChangeSet,
};

/// One commit, until `LiveGridState::resolve_edit` answers it. Opaque,
/// unique within its grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EditId(u64);

impl EditId {
    #[cfg_attr(not(test), expect(dead_code, reason = "the edit slot numbers commits"))]
    pub(crate) const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The id as a number, unique within its grid.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Why a value is refused: the editor could not parse it, the delegate's
/// `validate_edit` refused it, or the application rejected the commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditError {
    message: SharedString,
}

impl EditError {
    /// A refusal with the message shown to the user: a sentence that says
    /// what is wrong and how to fix it.
    pub fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The message.
    pub fn message(&self) -> &SharedString {
        &self.message
    }
}

/// Who started an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditOrigin {
    /// A key, a press or a menu command.
    User,
    /// A call of the grid API.
    Programmatic,
}

/// What started an edit, as [`EditGesture`] and [`CellEditor`] see it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditInput {
    /// `StartEdit` (F2) or a menu command: the editor starts from the
    /// value.
    Command,
    /// `StartEditEmpty` (Backspace, Delete): the editor starts empty.
    Clear,
    /// Typing on a focused cell: the editor starts from the typed text.
    Typing(SharedString),
    /// A press on the cell; `click_count` 2 for a double click.
    Press {
        /// The press's place in a series.
        click_count: usize,
    },
    /// `LiveGridState::start_edit`, or a commit through a
    /// `CellEditHandle`.
    Programmatic,
}

/// Why an editor closed without `EditCommit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditCancelReason {
    /// `CancelEdit` (Escape).
    Escape,
    /// The focus left with a draft [`EditPolicy::on_blur`] cancels.
    Blur,
    /// The value is the one the edit started from
    /// ([`EditPolicy::commit_unchanged`]).
    Unchanged,
    /// The row left the store.
    RowRemoved,
    /// The row left the shown lines without a user action
    /// ([`EditPolicy::on_hidden`]).
    RowHidden,
    /// `LiveGridState::cancel_edit`, or another edit replaced it.
    Programmatic,
}

/// The mark of an open editor: what its frame and its message show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditorMark {
    /// Nothing to show.
    #[default]
    None,
    /// The draft is refused: it does not parse, or `validate_edit` refused
    /// it.
    Invalid,
    /// The store changed the cell while its draft is open.
    Conflict,
}

/// What a cell's commits look like now.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditState {
    /// The commit went out; no answer yet.
    Pending,
    /// The answer was `Ok`; the cell waits for the store to have the value.
    Accepted,
    /// The answer was `Err`: the cell shows the store's value and this
    /// error until it is edited again.
    Failed(EditError),
}

/// Edits the cells of a column ([`LiveGridState::with_editor`]). Asked
/// once per edit start, on the UI thread, outside prepaint.
pub trait CellEditor<R: GridRow>: 'static {
    /// What to do with a start: open a field, commit at once, or refuse.
    fn open(&self, start: &EditorStart<'_, R>, window: &mut Window, cx: &mut App) -> EditorOpen;
}

/// What an editor does with a start.
#[non_exhaustive]
pub enum EditorOpen {
    /// A field in the cell until a commit or a cancel.
    Field(Box<dyn EditorField>),
    /// Commits this value at once, with no field: a checkbox toggles.
    Commit(OwnedValue),
    /// Takes no edit from this input: a select editor ignores typing. The
    /// input goes on as if the column had no editor.
    Refuse,
}

/// One open editor: its element in the cell, its draft and its focus.
pub trait EditorField: 'static {
    /// The element in the cell, built in prepaint every frame while the
    /// editor is open. Must not read, update or notify the grid entity.
    fn render(&self, cell: &EditorCell<'_>, window: &mut Window, cx: &mut App) -> AnyElement;

    /// Moves the keyboard focus into the editor; once, when it opens.
    fn focus(&self, window: &mut Window, cx: &mut App);

    /// The draft as a value of the column; `Err` if it is not one: the
    /// editor stays open and shows the message.
    fn value(&self, cx: &App) -> Result<OwnedValue, EditError>;

    /// Replaces the draft (`DraftAction::Replace`).
    fn set_value(&self, value: &OwnedValue, window: &mut Window, cx: &mut App);

    /// Returns true if the user changed the draft since the editor opened.
    fn is_dirty(&self, cx: &App) -> bool;
}

/// What a [`CellEditor`] knows about a start.
pub struct EditorStart<'a, R: GridRow> {
    row: &'a R,
    column: R::Column,
    value: CellValue<'a>,
    input: &'a EditInput,
    size: ComponentSize,
    title: &'a SharedString,
}

impl<'a, R: GridRow> EditorStart<'a, R> {
    /// The row.
    pub fn row(&self) -> &'a R {
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

    /// What started the edit.
    pub fn input(&self) -> &'a EditInput {
        self.input
    }

    /// The size of a gpui-kit field that fits the grid's rows.
    pub fn size(&self) -> ComponentSize {
        self.size
    }

    /// The column's title: the field's accessible name.
    pub fn title(&self) -> &'a SharedString {
        self.title
    }
}

/// What an [`EditorField`] knows about its cell when it renders.
pub struct EditorCell<'a> {
    bounds: Bounds<Pixels>,
    size: ComponentSize,
    mark: EditorMark,
    message: Option<&'a SharedString>,
    title: &'a SharedString,
    popup: &'a FieldPopup,
}

impl<'a> EditorCell<'a> {
    /// The cell in window coordinates, without a tree row's indent.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// The size of a gpui-kit field that fits the grid's rows.
    pub fn size(&self) -> ComponentSize {
        self.size
    }

    /// The editor's mark.
    pub fn mark(&self) -> EditorMark {
        self.mark
    }

    /// The message of the mark, if any.
    pub fn message(&self) -> Option<&'a SharedString> {
        self.message
    }

    /// The column's title: the field's accessible name.
    pub fn title(&self) -> &'a SharedString {
        self.title
    }

    /// Places `content` under the cell on a popover surface, as the
    /// built-in list and calendar are: above the grid, on the other side of
    /// the cell when there is no room below, and the field's own for
    /// presses, which do not end the edit. The message of the mark moves
    /// above the cell meanwhile. One per frame.
    pub fn popup(&self, content: impl IntoElement, cx: &App) -> AnyElement {
        use gpui_kit::component::ThemeStyled as _;

        self.popup.shown.set(true);
        let bounds = self.popup.bounds.clone();
        deferred(
            Positioner::side(self.bounds)
                .placement(Placement::Bottom)
                .align(Align::Start)
                .offset(px(2.))
                .margin(px(8.))
                .on_position(move |resolved| bounds.set(Some(resolved.bounds)))
                .child(div().occlude().popover_style(cx).child(content)),
        )
        .with_priority(POPUP_PRIORITY)
        .into_any_element()
    }
}

/// The layer a field shows under its cell ([`EditorCell::popup`]): whether
/// it is shown this frame, and where it was in the last one.
#[derive(Default)]
struct FieldPopup {
    shown: Cell<bool>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// What [`LiveGridDelegate::is_editable`] knows about a cell.
pub struct EditableContext<'a, R: GridRow> {
    row: &'a R,
    column: R::Column,
    value: CellValue<'a>,
    input: &'a EditInput,
}

impl<'a, R: GridRow> EditableContext<'a, R> {
    /// The row.
    pub fn row(&self) -> &'a R {
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

    /// The cell's value.
    pub fn value(&self) -> CellValue<'a> {
        self.value
    }

    /// What would start the edit.
    pub fn input(&self) -> &'a EditInput {
        self.input
    }
}

/// What [`LiveGridDelegate::validate_edit`] knows about a commit.
pub struct EditValidateContext<'a, R: GridRow> {
    row: &'a R,
    column: R::Column,
    value: &'a OwnedValue,
    base: &'a OwnedValue,
    current: &'a OwnedValue,
}

impl<'a, R: GridRow> EditValidateContext<'a, R> {
    /// The row, as the store has it now.
    pub fn row(&self) -> &'a R {
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

    /// The value to commit.
    pub fn value(&self) -> &'a OwnedValue {
        self.value
    }

    /// The cell's value when the edit started.
    pub fn base(&self) -> &'a OwnedValue {
        self.base
    }

    /// The cell's value in the store now: it differs from
    /// [`Self::base`] after a concurrent change.
    pub fn current(&self) -> &'a OwnedValue {
        self.current
    }
}

/// What [`LiveGridDelegate::render_edit_message`] knows about the open
/// editor's message.
pub struct EditMessageContext<'a> {
    mark: EditorMark,
    message: &'a SharedString,
    cell_bounds: Bounds<Pixels>,
    size: ComponentSize,
}

impl<'a> EditMessageContext<'a> {
    /// What the message is about: a refused draft or a conflict.
    pub fn mark(&self) -> EditorMark {
        self.mark
    }

    /// The message.
    pub fn message(&self) -> &'a SharedString {
        self.message
    }

    /// The edited cell in window coordinates.
    pub fn cell_bounds(&self) -> Bounds<Pixels> {
        self.cell_bounds
    }

    /// The grid's size.
    pub fn size(&self) -> ComponentSize {
        self.size
    }

    /// The default presentation: one line with an icon, `circle-alert` in
    /// the danger colour for a refused draft, `triangle-alert` in the
    /// warning colour for a conflict, on a popover surface. The grid places
    /// it under the cell.
    pub fn default_popup(&self, _window: &mut Window, cx: &mut App) -> AnyElement {
        use gpui_kit::component::ThemeStyled as _;

        let theme = cx.theme();
        let (icon, color) = match self.mark {
            EditorMark::Conflict => (IconName::TriangleAlert, theme.warning),
            _ => (IconName::CircleAlert, theme.danger),
        };
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .py(px(2.))
            .text_xs()
            .popover_style(cx)
            .child(Icon::new(icon).size_3().text_color(color))
            .child(self.message.clone())
            .into_any_element()
    }
}

/// An open edit: the cell, the value it started from, the editor's field
/// and its mark, and the focus handle of the field's host.
struct EditSession<K, C> {
    key: K,
    column: C,
    /// The value the edit started from.
    base: OwnedValue,
    /// The store's value of the cell at the last snapshot: a change asks
    /// the edit policy once.
    seen: OwnedValue,
    /// What started it: `is_editable` is asked again with it at the commit.
    input: EditInput,
    field: Box<dyn EditorField>,
    /// The column's title: the field's accessible name.
    title: SharedString,
    mark: EditorMark,
    message: Option<SharedString>,
    /// The handle the host tracks: the field inside is its descendant.
    editor_focus: FocusHandle,
    /// The host's visible part in the last frame: a press inside it is the
    /// field's own.
    host_bounds: Cell<Option<Bounds<Pixels>>>,
    /// The message's layer in the last frame: a press on it is the field's
    /// too.
    popup_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The field's own layer, such as a list.
    field_popup: FieldPopup,
    /// Brings the edited cell back on screen; any key in the host calls it.
    reveal: Rc<dyn Fn(&mut App)>,
    /// The window of the field, to give the grid the focus back when the
    /// edited row leaves the store.
    window: AnyWindowHandle,
    _focus_out: Subscription,
}

/// The open edit of a grid, and the number of the next commit.
pub(super) struct EditSlot<K, C> {
    session: Option<EditSession<K, C>>,
    next_edit: u64,
    /// A single press on a data cell, and where it went down: released
    /// there, it is offered to the edit gesture (`Press { click_count: 1 }`).
    single_press: Option<(K, C, Point<Pixels>)>,
    /// The columns with an editor: their elements may commit (`CellEditHandle`).
    edited_columns: ColumnSet,
}

impl<K, C> Default for EditSlot<K, C> {
    fn default() -> Self {
        Self {
            session: None,
            next_edit: 0,
            single_press: None,
            edited_columns: ColumnSet::EMPTY,
        }
    }
}

impl<K: PartialEq, C: Copy> EditSlot<K, C> {
    /// Returns true if `column` has an editor.
    pub(super) fn has_editor(&self, column: ColumnIx) -> bool {
        self.edited_columns.contains(column)
    }

    /// Returns true while an editor is open.
    pub(super) fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// The edited column, if the open editor is in the row of `key`.
    pub(super) fn column_in(&self, key: &K) -> Option<C> {
        self.session
            .as_ref()
            .and_then(|session| (session.key == *key).then_some(session.column))
    }

    /// Returns true if the keyboard focus is in the open editor.
    pub(super) fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.editor_focus.contains_focused(window, cx))
    }

    /// A new commit's id.
    fn next_id(&mut self) -> EditId {
        self.next_edit += 1;
        EditId(self.next_edit)
    }
}

/// How an attempt to commit a value ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitOutcome {
    /// `EditCommit` went out.
    Committed,
    /// The editor closed without a commit (`EditCancel`): the value was
    /// unchanged, the cell is no longer editable, or the row is gone.
    Closed,
    /// The value was refused: the field stays open with the message.
    Refused,
}

/// The editors of a grid's columns and its edit seams.
pub(super) struct EditSetup<R: GridRow> {
    /// By schema index; `None`: the column is read-only.
    editors: Vec<Option<Rc<dyn CellEditor<R>>>>,
    gesture: Box<dyn EditGesture>,
    policy: Box<dyn EditPolicy>,
}

impl<R: GridRow> Default for EditSetup<R> {
    fn default() -> Self {
        Self {
            editors: Vec::new(),
            gesture: Box::new(StandardEditGesture),
            policy: Box::new(StandardEditPolicy),
        }
    }
}

impl<R: GridRow> EditSetup<R> {
    fn editor(&self, column: ColumnIx) -> Option<&Rc<dyn CellEditor<R>>> {
        self.editors.get(column.get())?.as_ref()
    }

    fn set_editor(&mut self, column: ColumnIx, editor: Option<Rc<dyn CellEditor<R>>>) {
        if self.editors.len() <= column.get() {
            self.editors.resize_with(column.get() + 1, || None);
        }
        self.editors[column.get()] = editor;
    }

    /// The columns with an editor.
    fn edited_columns(&self) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        for (ix, editor) in self.editors.iter().enumerate() {
            if editor.is_some() {
                columns.insert(ColumnIx::new(ix as u16));
            }
        }
        columns
    }

    /// Which input opens an editor, and where the focus goes after a commit.
    pub(super) fn gesture(&self) -> &dyn EditGesture {
        &*self.gesture
    }

    /// How an edit behaves around its commit and under live data.
    pub(super) fn policy(&self) -> &dyn EditPolicy {
        &*self.policy
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Gives `column`'s cells an editor; a column without one is read-only.
    /// The delegate's `is_editable` can still refuse a cell.
    pub fn with_editor(mut self, column: ColumnOf<D>, editor: impl CellEditor<D::Row>) -> Self {
        self.edit_setup
            .set_editor(column.ix(), Some(Rc::new(editor)));
        self.edit.edited_columns = self.edit_setup.edited_columns();
        self
    }

    /// Sets or removes the editor of `column`'s cells.
    pub fn set_editor(
        &mut self,
        column: ColumnOf<D>,
        editor: Option<Rc<dyn CellEditor<D::Row>>>,
        cx: &mut Context<Self>,
    ) {
        self.edit_setup.set_editor(column.ix(), editor);
        self.edit.edited_columns = self.edit_setup.edited_columns();
        cx.notify();
    }

    /// Returns true if `column`'s cells have an editor.
    pub fn has_editor(&self, column: ColumnOf<D>) -> bool {
        self.edit_setup.editor(column.ix()).is_some()
    }

    /// Sets which input opens an editor and where the focus goes after a
    /// commit (default [`StandardEditGesture`], Excel's).
    pub fn with_edit_gesture(mut self, gesture: impl EditGesture) -> Self {
        self.edit_setup.gesture = Box::new(gesture);
        self
    }

    /// Sets how an edit behaves around its commit and under live data
    /// (default [`StandardEditPolicy`]).
    pub fn with_edit_policy(mut self, policy: impl EditPolicy) -> Self {
        self.edit_setup.policy = Box::new(policy);
        self
    }

    /// Opens the editor of a cell (`EditInput::Programmatic`,
    /// `EditOrigin::Programmatic`): the cell takes the focus and scrolls
    /// into view. An open editor elsewhere ends first, as a press outside
    /// it would end it. Returns false, doing nothing, if the cell is not
    /// shown, has no editor, or `is_editable` refuses it.
    pub fn start_edit(
        &mut self,
        key: &KeyOf<D>,
        column: ColumnOf<D>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.begin_edit(
            key.clone(),
            column,
            EditInput::Programmatic,
            EditOrigin::Programmatic,
            Modifiers::default(),
            window,
            cx,
        )
    }

    /// Commits the open editor as Enter would, without moving the focus.
    /// Returns false if no editor is open or its draft is refused: it stays
    /// open with the message.
    pub fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.edit.is_open() && self.commit_field(Some(window), cx) != CommitOutcome::Refused
    }

    /// Closes the open editor without a commit
    /// (`EditCancelReason::Programmatic`). Returns false if none is open.
    pub fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.cancel_field(EditCancelReason::Programmatic, Some(window), cx)
    }

    /// The cell being edited: its key and column.
    pub fn editing_cell(&self) -> Option<(&KeyOf<D>, ColumnOf<D>)> {
        let session = self.edit.session.as_ref()?;
        Some((&session.key, session.column))
    }

    /// Returns true while an editor is open.
    pub fn is_editing(&self) -> bool {
        self.edit.is_open()
    }

    /// The open editor's message and its mark: a refused draft
    /// (`EditorMark::Invalid`), for an application that shows it itself
    /// (`LiveGridDelegate::render_edit_message` returning `None`).
    pub fn edit_message(&self) -> Option<(EditorMark, &SharedString)> {
        let session = self.edit.session.as_ref()?;
        Some((session.mark, session.message.as_ref()?))
    }

    /// Returns true if the keyboard focus is on the grid's rows: on the
    /// root, or in the open editor's host. The root's focus ring and the
    /// focused cell's outline follow it. The group panel is outside.
    pub(crate) fn is_body_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.is_focused(window) || self.edit.contains_focus(window, cx)
    }

    /// The focused data cell: its key and column.
    fn focused_data_cell(&self) -> Option<(KeyOf<D>, ColumnOf<D>)> {
        let focus = self.selection.focus()?;
        Some((focus.row()?.clone(), focus.column()))
    }

    /// Opens the editor of a cell for `input`. The one path of every start:
    /// the cell must be a shown cell of a data row in a visible column with
    /// an editor; the gesture must agree to typing and presses
    /// (`EditGesture::starts_edit`) and the delegate to the cell
    /// (`is_editable`). The editor then opens a field, commits at once, or
    /// refuses. Returns true if it did not refuse.
    #[allow(clippy::too_many_arguments)]
    fn begin_edit(
        &mut self,
        key: KeyOf<D>,
        column: ColumnOf<D>,
        input: EditInput,
        origin: EditOrigin,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let snapshot = self.snapshot.clone();
        let Some(id) = snapshot.row_id(&key) else {
            return false;
        };
        let Some(row) = snapshot.row(id).cloned() else {
            return false;
        };
        if self.projection.display_ix(id).is_none()
            || self.columns.view_column_ix(column.ix()).is_none()
        {
            return false;
        }
        let Some(editor) = self.edit_setup.editor(column.ix()).cloned() else {
            return false;
        };
        let content = self.columns.column(column.ix()).content;
        if matches!(input, EditInput::Typing(_) | EditInput::Press { .. })
            && !self
                .edit_setup
                .gesture()
                .starts_edit(&EditGestureContext::new(input.clone(), modifiers, content))
        {
            return false;
        }
        // The value the cell shows: a waiting commit's, or the store's.
        let shown = self.pending_edits.overlay(&key, column).cloned();
        let value = shown
            .as_ref()
            .map_or_else(|| row.value(column), OwnedValue::as_cell);
        let editable = self.delegate.is_editable(
            &EditableContext {
                row: &*row,
                column,
                value,
                input: &input,
            },
            cx,
        );
        if !editable {
            return false;
        }
        // An open editor elsewhere ends first, as a press outside it.
        if self.edit.is_open() {
            self.end_edit_on_blur(window, cx);
        }
        let base = OwnedValue::from_cell(value);
        let title = self.columns.column(column.ix()).title.clone();
        let opened = editor.open(
            &EditorStart {
                row: &*row,
                column,
                value,
                input: &input,
                size: editor_size(self.row_height),
                title: &title,
            },
            window,
            cx,
        );
        match opened {
            EditorOpen::Refuse => false,
            EditorOpen::Commit(value) => {
                self.focus_edited_cell(&key, column, cx);
                self.pending_edits.clear_failed(&key, column);
                self.commit_value(key, column, value, base, &input, Some(window), cx);
                true
            }
            EditorOpen::Field(field) => {
                self.focus_edited_cell(&key, column, cx);
                let editor_focus = cx.focus_handle();
                let focus_out = cx.on_focus_out(&editor_focus, window, |this, _, window, cx| {
                    this.on_editor_focus_out(window, cx);
                });
                let reveal: Rc<dyn Fn(&mut App)> = {
                    let grid = cx.weak_entity();
                    Rc::new(move |cx: &mut App| {
                        let _ = grid.update(cx, |this, cx| {
                            if this.reveal_edited_cell() {
                                cx.notify();
                            }
                        });
                    })
                };
                field.focus(window, cx);
                let (grid, refocus) = (cx.weak_entity(), editor_focus.clone());
                window.defer(cx, move |window, cx| {
                    let _ = grid.update(cx, |this, cx| this.refocus_field(&refocus, window, cx));
                });
                self.pending_edits.clear_failed(&key, column);
                self.edit.session = Some(EditSession {
                    key: key.clone(),
                    column,
                    seen: OwnedValue::from_cell(row.value(column)),
                    base,
                    input,
                    field,
                    title,
                    mark: EditorMark::None,
                    message: None,
                    editor_focus,
                    host_bounds: Cell::new(None),
                    popup_bounds: Rc::default(),
                    field_popup: FieldPopup::default(),
                    reveal,
                    window: window.window_handle(),
                    _focus_out: focus_out,
                });
                cx.emit(LiveGridEvent::EditStart {
                    key,
                    column,
                    origin,
                });
                cx.notify();
                true
            }
        }
    }

    /// Gives the edited cell the focus, if it lacks it, and scrolls it into
    /// view.
    fn focus_edited_cell(&mut self, key: &KeyOf<D>, column: ColumnOf<D>, cx: &mut Context<Self>) {
        if self.focused_data_cell().as_ref() != Some(&(key.clone(), column)) {
            self.set_focused_cell(key, Some(column), cx);
        }
        self.reveal_focus();
    }

    /// Scrolls the edited cell into view. Returns true if it scrolled.
    fn reveal_edited_cell(&mut self) -> bool {
        let Some(session) = &self.edit.session else {
            return false;
        };
        let Some(display_ix) = self
            .snapshot
            .row_id(&session.key)
            .and_then(|id| self.projection.display_ix(id))
        else {
            return false;
        };
        let column = session.column.ix();
        let before = (self.scroll_row, self.scroll_offset, self.scroll_x);
        self.reveal_row(display_ix);
        let mut columns = ColumnSet::EMPTY;
        columns.insert(column);
        self.reveal_columns(columns);
        self.clamp_scroll();
        before != (self.scroll_row, self.scroll_offset, self.scroll_x)
    }

    /// The store's value of a cell now; `None` if its row is gone.
    fn store_value(&self, key: &KeyOf<D>, column: ColumnOf<D>) -> Option<OwnedValue> {
        let row = self.snapshot.row(self.snapshot.row_id(key)?)?;
        Some(OwnedValue::from_cell(row.value(column)))
    }

    /// Closes the open field. The grid's root takes the keyboard focus back
    /// if the field had it; without a window at hand (a snapshot, a new
    /// order), once the update is done. The order held for the field goes
    /// to the reorder policy again, as when the context menu closes.
    fn close_field(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<EditSession<KeyOf<D>, ColumnOf<D>>> {
        let session = self.edit.session.take()?;
        match window {
            Some(window) => {
                if session.editor_focus.contains_focused(window, cx) {
                    window.focus(&self.focus_handle, cx);
                }
            }
            None => {
                let (root, window) = (self.focus_handle.clone(), session.window);
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        if window.focused(cx).is_none() || root.contains_focused(window, cx) {
                            window.focus(&root, cx);
                        }
                    });
                });
            }
        }
        // After the close's own events.
        let grid = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = grid.update(cx, |this, cx| this.reconsider_held(cx));
        });
        cx.notify();
        Some(session)
    }

    /// The one path of every commit: a field's draft and a commit with no
    /// field. The value is checked against the value the edit started from
    /// (`EditPolicy::commit_unchanged`), the cell against `is_editable`, the
    /// value against `validate_edit`; then the field closes and
    /// `EditCommit` goes out.
    #[allow(clippy::too_many_arguments)]
    fn commit_value(
        &mut self,
        key: KeyOf<D>,
        column: ColumnOf<D>,
        value: OwnedValue,
        base: OwnedValue,
        input: &EditInput,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> CommitOutcome {
        let close = |this: &mut Self,
                     reason: EditCancelReason,
                     draft: Option<OwnedValue>,
                     window: Option<&mut Window>,
                     cx: &mut Context<Self>| {
            this.close_field(window, cx);
            cx.emit(LiveGridEvent::EditCancel {
                key: key.clone(),
                column,
                reason,
                draft,
            });
            CommitOutcome::Closed
        };
        if value.cmp_total(&base).is_eq() && !self.edit_setup.policy().commit_unchanged() {
            return close(self, EditCancelReason::Unchanged, None, window, cx);
        }
        let snapshot = self.snapshot.clone();
        let Some(row) = snapshot.row_id(&key).and_then(|id| snapshot.row(id)) else {
            return close(self, EditCancelReason::RowRemoved, Some(value), window, cx);
        };
        let current = OwnedValue::from_cell(row.value(column));
        let editable = self.delegate.is_editable(
            &EditableContext {
                row: &**row,
                column,
                value: current.as_cell(),
                input,
            },
            cx,
        );
        if !editable {
            return close(
                self,
                EditCancelReason::Programmatic,
                Some(value),
                window,
                cx,
            );
        }
        let valid = self.delegate.validate_edit(
            &EditValidateContext {
                row: &**row,
                column,
                value: &value,
                base: &base,
                current: &current,
            },
            cx,
        );
        if let Err(error) = valid {
            if let Some(session) = &mut self.edit.session {
                session.mark = EditorMark::Invalid;
                session.message = Some(error.message().clone());
                cx.notify();
            }
            return CommitOutcome::Refused;
        }
        let edit = self.edit.next_id();
        self.close_field(window, cx);
        cx.emit(LiveGridEvent::EditCommit {
            edit,
            key: key.clone(),
            column,
            value: value.clone(),
            base,
            current,
        });
        // The cell shows the value until the commit settles.
        self.begin_pending_edit(edit, key, column, value, cx);
        CommitOutcome::Committed
    }

    /// A commit from a cell's element (`CellEditHandle::commit`): as an
    /// editor's commit with no field, started by a call. The row must be in
    /// the store, the column must have an editor, and `is_editable` must
    /// take the cell now; a refused cell or value emits nothing. An editor
    /// open elsewhere ends first. Returns true if `EditCommit` went out.
    pub(super) fn commit_from_element(
        &mut self,
        key: &KeyOf<D>,
        column: ColumnOf<D>,
        value: OwnedValue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.edit_setup.editor(column.ix()).is_none() {
            return false;
        }
        let snapshot = self.snapshot.clone();
        let Some(row) = snapshot.get(key) else {
            return false;
        };
        let input = EditInput::Programmatic;
        let shown = self.pending_edits.overlay(key, column).cloned();
        let now = shown
            .as_ref()
            .map_or_else(|| row.value(column), OwnedValue::as_cell);
        let editable = self.delegate.is_editable(
            &EditableContext {
                row: &**row,
                column,
                value: now,
                input: &input,
            },
            cx,
        );
        if !editable {
            return false;
        }
        if self.edit.is_open() {
            self.end_edit_on_blur(window, cx);
        }
        let base = OwnedValue::from_cell(now);
        self.pending_edits.clear_failed(key, column);
        self.commit_value(key.clone(), column, value, base, &input, Some(window), cx)
            == CommitOutcome::Committed
    }

    /// Commits the open field's draft: a draft that does not parse keeps
    /// the field open with its message.
    fn commit_field(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> CommitOutcome {
        let Some(session) = &mut self.edit.session else {
            return CommitOutcome::Closed;
        };
        let draft = session.field.value(cx);
        let value = match draft {
            Ok(value) => value,
            Err(error) => {
                session.mark = EditorMark::Invalid;
                session.message = Some(error.message().clone());
                cx.notify();
                return CommitOutcome::Refused;
            }
        };
        let (key, column, base, input) = (
            session.key.clone(),
            session.column,
            session.base.clone(),
            session.input.clone(),
        );
        self.commit_value(key, column, value, base, &input, window, cx)
    }

    /// Closes the open field without a commit. Returns false if none is
    /// open.
    fn cancel_field(
        &mut self,
        reason: EditCancelReason,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = &self.edit.session else {
            return false;
        };
        let draft = session
            .field
            .value(cx)
            .ok()
            .filter(|draft| !draft.cmp_total(&session.base).is_eq());
        let Some(session) = self.close_field(window, cx) else {
            return false;
        };
        cx.emit(LiveGridEvent::EditCancel {
            key: session.key,
            column: session.column,
            reason,
            draft,
        });
        true
    }

    /// The focus left the open field, or a press landed outside it: it
    /// ends as [`EditPolicy::on_blur`] decides, with no move. A commit the
    /// delegate refuses then cancels: the press goes on elsewhere.
    fn end_edit_on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.edit.session else {
            return;
        };
        let draft = session.field.value(cx).ok();
        let current = self
            .store_value(&session.key, session.column)
            .unwrap_or_else(|| session.base.clone());
        let end = self.edit_setup.policy().on_blur(&EditPolicyContext::new(
            &session.base,
            &current,
            draft.as_ref(),
            session.field.is_dirty(cx),
        ));
        if end == EditEnd::Commit
            && self.commit_field(Some(&mut *window), cx) != CommitOutcome::Refused
        {
            return;
        }
        self.cancel_field(EditCancelReason::Blur, Some(window), cx);
    }

    /// The focus left the editor's host without a press: to another element
    /// of the window. Leaving the window keeps the edit, as Excel does: GPUI
    /// reports a focus-out when the window turns inactive, which is ignored.
    fn on_editor_focus_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_window_active() && self.edit.is_open() && !self.edit.contains_focus(window, cx)
        {
            self.end_edit_on_blur(window, cx);
        }
    }

    /// Gives the open field the keyboard focus again after the event that
    /// opened it: a press focuses the grid's root once its own handlers
    /// ran.
    fn refocus_field(&mut self, editor_focus: &FocusHandle, window: &mut Window, cx: &mut App) {
        if let Some(session) = &self.edit.session
            && session.editor_focus == *editor_focus
            && !session.editor_focus.contains_focused(window, cx)
        {
            session.field.focus(window, cx);
        }
    }

    /// A commit Action: commits the field and moves the focus as the edit
    /// gesture says for `key`.
    fn commit_and_move(&mut self, key: EditCommitKey, window: &mut Window, cx: &mut Context<Self>) {
        if !self.edit.is_open()
            || self.commit_field(Some(&mut *window), cx) == CommitOutcome::Refused
        {
            return;
        }
        match self.edit_setup.gesture().after_commit(key) {
            AfterCommit::Move(to) => {
                self.move_focus(to, cx);
            }
            AfterCommit::MoveAndEdit(to) => {
                if self.move_focus(to, cx)
                    && let Some((key, column)) = self.focused_data_cell()
                {
                    self.begin_edit(
                        key,
                        column,
                        EditInput::Command,
                        EditOrigin::User,
                        Modifiers::default(),
                        window,
                        cx,
                    );
                }
            }
            _ => {}
        }
    }

    /// Moves the focus after a commit; returns true if it moved. The
    /// arrows' moves are the arrows' navigation; `NextEditable` and
    /// `PrevEditable` stay in the row.
    fn move_focus(&mut self, to: CellMove, cx: &mut Context<Self>) -> bool {
        let before = self.focused_data_cell();
        let navigation = match to {
            CellMove::Up => Navigation::Up,
            CellMove::Down => Navigation::Down,
            CellMove::Left => Navigation::PrevColumn,
            CellMove::Right => Navigation::NextColumn,
            CellMove::NextEditable | CellMove::PrevEditable => {
                return self.focus_editable_neighbour(to == CellMove::NextEditable, cx);
            }
        };
        self.navigate(navigation, false, cx);
        self.focused_data_cell() != before
    }

    /// Focuses the next (`forward`) or previous visible cell of the focused
    /// row that takes an edit. Returns true if there is one.
    fn focus_editable_neighbour(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some((key, column)) = self.focused_data_cell() else {
            return false;
        };
        let Some(view) = self.columns.view_column_ix(column.ix()) else {
            return false;
        };
        let snapshot = self.snapshot.clone();
        let Some(row) = snapshot.row_id(&key).and_then(|id| snapshot.row(id)) else {
            return false;
        };
        let count = self.columns.visible_set().len();
        let views: Vec<usize> = if forward {
            (view.get() + 1..count).collect()
        } else {
            (0..view.get()).rev().collect()
        };
        let next = views.into_iter().find_map(|at| {
            let ix = self.columns.column_at(ViewColumnIx::new(at as u16))?;
            let column: ColumnOf<D> = column_of(ix);
            let editable = self.edit_setup.editor(ix).is_some()
                && self.delegate.is_editable(
                    &EditableContext {
                        row: &**row,
                        column,
                        value: row.value(column),
                        input: &EditInput::Command,
                    },
                    cx,
                );
            editable.then_some(column)
        });
        match next {
            Some(column) => self.set_focused_cell(&key, Some(column), cx),
            None => false,
        }
    }

    // The call sites in the grid's other files.

    /// A button went down anywhere in the window, in the capture phase:
    /// before the press does anything, an open editor outside it ends as
    /// [`EditPolicy::on_blur`] decides.
    pub(super) fn commit_edit_on_press(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.edit.session else {
            return;
        };
        let inside = |bounds: Option<Bounds<Pixels>>| bounds.is_some_and(|b| b.contains(&position));
        if inside(session.host_bounds.get())
            || inside(session.popup_bounds.get())
            || inside(session.field_popup.bounds.get())
        {
            return;
        }
        self.end_edit_on_blur(window, cx);
    }

    /// The second press of a series on a data cell: opens its editor if
    /// the column has one and the gesture agrees. Returns true if an editor
    /// opened; false keeps the double click a `Confirm`.
    pub(super) fn start_edit_by_press(
        &mut self,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pointer) = self
            .pointer_target
            .as_ref()
            .filter(|pointer| pointer.pinned().is_none())
        else {
            return false;
        };
        let (Some(key), Some(column)) = (pointer.row().cloned(), pointer.column()) else {
            return false;
        };
        self.begin_edit(
            key,
            column_of(column),
            EditInput::Press { click_count },
            EditOrigin::User,
            Modifiers::default(),
            window,
            cx,
        )
    }

    /// A new snapshot was taken: the waiting commits settle what the store
    /// decided; an edited row that left the store ends its edit
    /// (`EditCancel { RowRemoved }`); a change of the edited cell's value
    /// asks `EditPolicy::on_value_change` once. O(1) for the edit: one key
    /// lookup and one value.
    pub(super) fn follow_edit_rows(&mut self, _changes: &ChangeSet, cx: &mut Context<Self>) {
        self.follow_pending_edits(cx);
        let Some(session) = &self.edit.session else {
            return;
        };
        let Some(current) = self.store_value(&session.key, session.column) else {
            self.cancel_field(EditCancelReason::RowRemoved, None, cx);
            return;
        };
        if current.cmp_total(&session.base).is_eq() {
            // Back at the value the edit started from: no conflict.
            if let Some(session) = &mut self.edit.session
                && session.mark == EditorMark::Conflict
            {
                session.mark = EditorMark::None;
                session.message = None;
                session.seen = current;
                cx.notify();
            }
            return;
        }
        if current.cmp_total(&session.seen).is_eq() {
            return;
        }
        let draft = session.field.value(cx).ok();
        let action = self
            .edit_setup
            .policy()
            .on_value_change(&EditPolicyContext::new(
                &session.base,
                &current,
                draft.as_ref(),
                session.field.is_dirty(cx),
            ));
        match action {
            DraftAction::Replace => {
                let Some(session) = &mut self.edit.session else {
                    return;
                };
                session.seen = current.clone();
                session.base = current.clone();
                session.mark = EditorMark::None;
                session.message = None;
                // The field takes the value once a window is at hand.
                let (grid, window, editor_focus) = (
                    cx.weak_entity(),
                    session.window,
                    session.editor_focus.clone(),
                );
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        let _ = grid.update(cx, |this, cx| {
                            if let Some(session) = &this.edit.session
                                && session.editor_focus == editor_focus
                            {
                                session.field.set_value(&current, window, cx);
                                cx.notify();
                            }
                        });
                    });
                });
                cx.notify();
            }
            DraftAction::Cancel => {
                self.cancel_field(EditCancelReason::Programmatic, None, cx);
            }
            _ => {
                let mut text = String::new();
                let shown = display_text(current.as_cell(), &mut text).to_owned();
                let Some(session) = &mut self.edit.session else {
                    return;
                };
                session.mark = EditorMark::Conflict;
                session.message = Some(t!("live_grid.edit.conflict", value = shown).into());
                session.seen = current;
                cx.notify();
            }
        }
    }

    /// The edited row or column is no longer shown, though no press closed
    /// the field: a filter, a collapsed group, a hidden column. The edit
    /// ends as `EditPolicy::on_hidden` decides: a commit, or a cancel
    /// (`EditCancel { RowHidden }`), which a refused commit becomes too.
    pub(super) fn end_hidden_edit(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.edit.session else {
            return;
        };
        let shown = self
            .snapshot
            .row_id(&session.key)
            .and_then(|id| self.projection.display_ix(id))
            .is_some()
            && self.columns.view_column_ix(session.column.ix()).is_some();
        if shown {
            return;
        }
        let draft = session.field.value(cx).ok();
        let current = self
            .store_value(&session.key, session.column)
            .unwrap_or_else(|| session.base.clone());
        let end = self.edit_setup.policy().on_hidden(&EditPolicyContext::new(
            &session.base,
            &current,
            draft.as_ref(),
            session.field.is_dirty(cx),
        ));
        if end == EditEnd::Commit && self.commit_field(None, cx) != CommitOutcome::Refused {
            return;
        }
        self.cancel_field(EditCancelReason::RowHidden, None, cx);
    }

    /// Plans the open editor's host when its row is not painted this frame
    /// (scrolled away): it keeps the focus and the keys, with a mask of no
    /// area, at the row's place by the projection.
    pub(super) fn plan_hidden_editor_host(
        &mut self,
        hosts: &mut HostPlan,
        painted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.edit.session else {
            return;
        };
        // A hidden column takes the field out of every row: the edit ends
        // after this frame.
        if self.columns.view_column_ix(session.column.ix()).is_none() {
            let grid = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = grid.update(cx, |this, cx| this.end_hidden_edit(cx));
            });
        }
        if painted {
            return;
        }
        session.host_bounds.set(None);
        let rows = self.body_bands().rows();
        let line = self
            .snapshot
            .row_id(&session.key)
            .and_then(|id| self.projection.display_ix(id))
            .map_or(rows.origin.y, |display_ix| {
                rows.origin.y + self.row_height * (display_ix as f32 - self.scroll_row as f32)
                    - self.scroll_offset
            });
        let line = Bounds::new(
            point(rows.origin.x, line),
            size(rows.size.width, self.row_height),
        );
        let width = self.columns.column(session.column.ix()).width;
        let bounds = Bounds::new(line.origin, size(width, self.row_height));
        let first = hosts.len();
        let element = editor_host(
            session,
            bounds,
            editor_size(self.row_height),
            false,
            None,
            window,
            cx,
        );
        hosts.push(CellHost {
            layer: HostLayer::Editor,
            column: session.column.ix(),
            zone: self.columns.column(session.column.ix()).zone,
            bounds,
            element,
        });
        hosts.end_row(
            first,
            data_row_id_name(None),
            key_hash(&session.key),
            line,
            Bounds::default(),
        );
    }

    /// Binds the editing Actions (`StartEdit`, `StartEditEmpty`, the
    /// commits and `CancelEdit`) and typing on the root.
    pub(super) fn bind_edit_actions<E: InteractiveElement>(root: E, cx: &Context<Self>) -> E {
        root.on_action(cx.listener(|this, _: &StartEdit, window, cx| {
            this.start_edit_by_key(EditInput::Command, window, cx)
        }))
        .on_action(cx.listener(|this, _: &StartEditEmpty, window, cx| {
            this.start_edit_by_key(EditInput::Clear, window, cx)
        }))
        .on_action(cx.listener(|this, _: &CommitEdit, window, cx| {
            this.commit_and_move(EditCommitKey::Enter, window, cx)
        }))
        .on_action(cx.listener(|this, _: &CommitEditUp, window, cx| {
            this.commit_and_move(EditCommitKey::Up, window, cx)
        }))
        .on_action(cx.listener(|this, _: &CommitEditDown, window, cx| {
            this.commit_and_move(EditCommitKey::Down, window, cx)
        }))
        .on_action(cx.listener(|this, _: &CommitEditNext, window, cx| {
            this.commit_and_move(EditCommitKey::Next, window, cx)
        }))
        .on_action(cx.listener(|this, _: &CommitEditPrev, window, cx| {
            this.commit_and_move(EditCommitKey::Prev, window, cx)
        }))
        .on_action(cx.listener(|this, _: &CancelEdit, window, cx| {
            this.cancel_field(EditCancelReason::Escape, Some(window), cx);
        }))
        .on_key_down(cx.listener(Self::type_to_edit))
        .on_mouse_down(MouseButton::Left, cx.listener(Self::note_single_press))
        .on_mouse_up(MouseButton::Left, cx.listener(Self::edit_on_release))
    }

    /// A left press bubbled up to the root: if the body took it as the
    /// first press on a data cell, its release may edit the cell.
    fn note_single_press(&mut self, event: &MouseDownEvent, _: &mut Window, _: &mut Context<Self>) {
        self.edit.single_press = self.pointer_target.as_ref().and_then(|pointer| {
            let fresh = pointer.position() == event.position && event.click_count == 1;
            let cell = (pointer.row().cloned()?, column_of(pointer.column()?));
            (fresh && pointer.pinned().is_none()).then_some((cell.0, cell.1, event.position))
        });
    }

    /// A left release on the root after a single press on a data cell,
    /// without a drag: a gesture of the application may edit on a single
    /// click (`Press { click_count: 1 }`, AG Grid's `singleClickEdit`). The
    /// standard gesture does not.
    fn edit_on_release(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, column, origin)) = self.edit.single_press.take() else {
            return;
        };
        if self.edit.is_open() || is_past_drag_threshold(origin, event.position) {
            return;
        }
        self.begin_edit(
            key,
            column,
            EditInput::Press { click_count: 1 },
            EditOrigin::User,
            event.modifiers,
            window,
            cx,
        );
    }

    /// `StartEdit` and `StartEditEmpty` on the focused cell; a cell that
    /// takes no edit lets the key go on.
    fn start_edit_by_key(&mut self, input: EditInput, window: &mut Window, cx: &mut Context<Self>) {
        let started = self.focus_handle.is_focused(window)
            && self.focused_data_cell().is_some_and(|(key, column)| {
                self.begin_edit(
                    key,
                    column,
                    input,
                    EditOrigin::User,
                    Modifiers::default(),
                    window,
                    cx,
                )
            });
        if !started {
            cx.propagate();
        }
    }

    /// Typing on the focused cell opens its editor with the typed text: a
    /// key with a character, without Ctrl, Cmd or Fn, that no binding took,
    /// while the root itself has the focus. An input method's text does not
    /// start an edit: the root has no text input handler.
    fn type_to_edit(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if self.edit.is_open()
            || !self.focus_handle.is_focused(window)
            || modifiers.control
            || modifiers.platform
            || modifiers.function
        {
            return;
        }
        let Some(text) = keystroke
            .key_char
            .as_ref()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        else {
            return;
        };
        let Some((key, column)) = self.focused_data_cell() else {
            return;
        };
        let input = EditInput::Typing(text.clone().into());
        if self.begin_edit(key, column, input, EditOrigin::User, modifiers, window, cx) {
            cx.stop_propagation();
        }
    }

    /// Returns true if typing `text` on the focused cell would edit it: a
    /// data cell with an editor that the delegate and the gesture let take
    /// typing. `ExpandRow` and `CollapseRow` then let `+` and `-`
    /// go on to typing.
    pub(super) fn would_type_into_focus(&self, text: &str, cx: &App) -> bool {
        let Some((key, column)) = self.focused_data_cell() else {
            return false;
        };
        let Some(row) = self
            .snapshot
            .row_id(&key)
            .and_then(|id| self.snapshot.row(id))
        else {
            return false;
        };
        let input = EditInput::Typing(SharedString::new(text));
        self.edit_setup.editor(column.ix()).is_some()
            && self.columns.view_column_ix(column.ix()).is_some()
            && self
                .edit_setup
                .gesture()
                .starts_edit(&EditGestureContext::new(
                    input.clone(),
                    Modifiers::default(),
                    self.columns.column(column.ix()).content,
                ))
            && self.delegate.is_editable(
                &EditableContext {
                    row: &**row,
                    column,
                    value: row.value(column),
                    input: &input,
                },
                cx,
            )
    }
}

/// The open editor's host over `bounds`: the field, a frame inside the cell
/// in the colour of its mark, the editor's key context and its focus, and
/// the message's layer under the cell. A press on it is the field's, not
/// the body's; any key brings the cell back on screen.
fn editor_host<K, C>(
    session: &EditSession<K, C>,
    bounds: Bounds<Pixels>,
    size: ComponentSize,
    a11y: bool,
    message: Option<AnyElement>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div>
where
    C: GridColumns,
{
    session.field_popup.shown.set(false);
    session.field_popup.bounds.set(None);
    let field = session.field.render(
        &EditorCell {
            bounds,
            size,
            mark: session.mark,
            message: session.message.as_ref(),
            title: &session.title,
            popup: &session.field_popup,
        },
        window,
        cx,
    );
    // Over a field's own layer under the cell, the message goes above it.
    let (message_anchor, message_at) = if session.field_popup.shown.get() {
        (Anchor::BottomLeft, point(bounds.left(), bounds.top()))
    } else {
        (Anchor::TopLeft, point(bounds.left(), bounds.bottom()))
    };
    let theme = cx.theme();
    let frame = match session.mark {
        EditorMark::Invalid => theme.danger,
        EditorMark::Conflict => theme.warning,
        _ => theme.ring,
    };
    let reveal = session.reveal.clone();
    let popup_bounds = session.popup_bounds.clone();
    popup_bounds.set(None);
    CellHost::host_div(HostLayer::Editor, session.column.id())
        .track_focus(&session.editor_focus)
        .key_context(EDITOR_KEY_CONTEXT)
        .capture_key_down(move |_, _, cx| reveal(cx))
        // A field's own Escape that it lets go (gpui-kit's `Cancel`, as a
        // list does) cancels the edit, not the grid's selection.
        .on_action(|_: &Cancel, window, cx| {
            window.dispatch_action(Box::new(CancelEdit), cx);
        })
        .block_mouse_except_scroll()
        .flex()
        .items_center()
        .px(px(2.))
        .bg(theme.background)
        .border_2()
        .border_color(frame)
        .when(a11y, |this| this.role(Role::GridCell))
        .when_some(session.message.clone(), |this, message| {
            this.aria_description(message)
        })
        .child(field)
        .when_some(message, |this, message| {
            this.child(
                deferred(
                    anchored()
                        .anchor(message_anchor)
                        .position(message_at)
                        .snap_to_window_with_margin(px(4.))
                        .child(
                            div()
                                .on_children_prepainted(move |children, _, _| {
                                    popup_bounds.set(children.first().copied());
                                })
                                .child(div().occlude().child(message)),
                        ),
                )
                .with_priority(1),
            )
        })
}

impl<D: LiveGridDelegate> RowPlanner<'_, D> {
    /// Plans the open editor's host in its cell: the cell without a tree
    /// row's indent, the field over it, the message under it. Returns true:
    /// the body paints no text there.
    pub(super) fn plan_editor_host(
        &mut self,
        cell: &HostCell<'_, D::Row>,
        window: &mut Window,
    ) -> bool {
        let Some(session) = &self.edit.session else {
            return false;
        };
        let bounds = Bounds::new(
            point(cell.bounds.origin.x + cell.indent, cell.bounds.origin.y),
            size(
                (cell.bounds.size.width - cell.indent).max(px(0.)),
                cell.bounds.size.height,
            ),
        );
        let clip = self.band_clips[1];
        session.host_bounds.set(Some(bounds.intersect(&clip)));
        let size = editor_size(self.row_height);
        let message = session.message.as_ref().and_then(|message| {
            self.delegate.render_edit_message(
                &EditMessageContext {
                    mark: session.mark,
                    message,
                    cell_bounds: bounds,
                    size: grid_size(self.row_height),
                },
                window,
                self.app,
            )
        });
        let element = editor_host(
            session,
            bounds,
            size,
            self.a11y.is_some(),
            message,
            window,
            self.app,
        );
        self.hosts.push(CellHost {
            layer: HostLayer::Editor,
            column: cell.column.ix(),
            zone: cell.zone,
            bounds,
            element,
        });
        true
    }
}

/// The gpui-kit size of a grid with rows `row_height` high: 26, 30, 32 and
/// 40 px are `xsmall`, `small`, `medium` and `large`.
fn grid_size(row_height: Pixels) -> ComponentSize {
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::Modifiers;

    use super::*;
    use crate::{
        CellContent,
        policy::{
            AfterCommit, CellMove, DraftAction, EditCommitKey, EditEnd, EditGestureContext,
            EditPolicyContext, ReorderContext,
        },
    };

    #[test]
    fn test_edit_types() {
        // Ids are unique and read back.
        let ids: HashSet<EditId> = (0..4).map(EditId::new).collect();
        assert_eq!(ids.len(), 4);
        assert_eq!(EditId::new(7).get(), 7);
        let error = EditError::new("Enter a number");
        assert_eq!(error.message().as_ref(), "Enter a number");
        assert_eq!(EditorMark::default(), EditorMark::None);

        // Excel's gesture: every input but a single press, and no press on
        // a control; Enter down, Tab right.
        let gesture = StandardEditGesture;
        let starts = |input: EditInput, content: CellContent| {
            gesture.starts_edit(&EditGestureContext::new(
                input,
                Modifiers::default(),
                content,
            ))
        };
        for input in [
            EditInput::Command,
            EditInput::Clear,
            EditInput::Typing("4".into()),
            EditInput::Press { click_count: 2 },
            EditInput::Programmatic,
        ] {
            assert!(starts(input.clone(), CellContent::Text), "{input:?}");
            assert!(starts(input.clone(), CellContent::Element), "{input:?}");
        }
        assert!(!starts(
            EditInput::Press { click_count: 1 },
            CellContent::Text
        ));
        assert!(!starts(
            EditInput::Press { click_count: 2 },
            CellContent::Control
        ));
        assert!(starts(EditInput::Typing("x".into()), CellContent::Control));
        for (key, expected) in [
            (EditCommitKey::Enter, CellMove::Down),
            (EditCommitKey::Down, CellMove::Down),
            (EditCommitKey::Up, CellMove::Up),
            (EditCommitKey::Next, CellMove::Right),
            (EditCommitKey::Prev, CellMove::Left),
        ] {
            assert_eq!(gesture.after_commit(key), AfterCommit::Move(expected));
        }

        // The standard policy: a valid draft commits on blur, an invalid one
        // cancels; the draft is kept under a store change; a hidden row
        // cancels; 5 s to settle; the error mark stays.
        let policy = StandardEditPolicy;
        let (base, current, draft) = (OwnedValue::Int(1), OwnedValue::Int(2), OwnedValue::Int(3));
        let valid = EditPolicyContext::new(&base, &current, Some(&draft), true);
        let invalid = EditPolicyContext::new(&base, &current, None, true);
        assert_eq!(policy.on_blur(&valid), EditEnd::Commit);
        assert_eq!(policy.on_blur(&invalid), EditEnd::Cancel);
        assert!(!policy.commit_unchanged());
        assert_eq!(policy.on_value_change(&valid), DraftAction::Keep);
        assert_eq!(policy.on_hidden(&valid), EditEnd::Cancel);
        assert_eq!(policy.settle_timeout(), std::time::Duration::from_secs(5));
        assert_eq!(policy.error_mark_timeout(), None);
        assert_eq!(
            (valid.base(), valid.current(), valid.is_dirty()),
            (&base, &current, true)
        );

        // A new context sets none of the edit, drag and chooser inputs.
        let now = web_time::Instant::now();
        let context = ReorderContext::new(now, now, false, false);
        assert!(!context.is_editing());
        assert!(!context.has_pending_edits());
        assert!(!context.is_drag_over());
        assert!(!context.is_column_chooser_open());
        assert!(context.with_editing(true).is_editing());

        // Nothing is open; the setup reads back what it was given.
        let slot = EditSlot::<u32, u16>::default();
        assert!(!slot.is_open());
        assert_eq!(slot.column_in(&1), None);
        let setup = EditSetup::<TestRow>::default();
        assert!(setup.gesture().starts_edit(&EditGestureContext::new(
            EditInput::Command,
            Modifiers::default(),
            CellContent::Text,
        )));
        assert_eq!(setup.policy().on_blur(&invalid), EditEnd::Cancel);
    }

    struct TestRow;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct TestColumn;

    impl crate::model::GridColumns for TestColumn {
        fn all() -> &'static [Self] {
            &[Self]
        }

        fn id(self) -> &'static str {
            "test"
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(0)
        }
    }

    impl GridRow for TestRow {
        type Key = u32;
        type Column = TestColumn;

        fn key(&self) -> &u32 {
            &0
        }

        fn value(&self, _: TestColumn) -> CellValue<'_> {
            CellValue::Empty
        }
    }
}
