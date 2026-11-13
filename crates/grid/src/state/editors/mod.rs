//! The built-in cell editors: a text field and a number field on gpui-kit's
//! single-line `Input`, the size of a field that fits a row, and the raw
//! text a field starts from; a list of values, a calendar, a checkbox with
//! no field, and the handle that commits from an element in a cell.

mod checkbox;
mod date;
mod number;
mod select;
mod text;

use std::{any::Any, cell::RefCell, fmt::Write as _, rc::Rc, sync::Arc};

use gpui_kit::component::Size as ComponentSize;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, IntoElement as _, ParentElement as _, Pixels,
    SharedString, Styled as _, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};

pub use self::{
    checkbox::CheckboxEditor,
    date::DateEditor,
    number::{NumberEditor, NumberKind},
    select::{SelectEditor, SelectOption},
    text::TextEditor,
};
use super::{
    ColumnOf, LiveGridDelegate, LiveGridState,
    columns::column_of,
    edit::{EditError, EditorCell, EditorField},
};
use crate::model::{CellValue, ColumnIx, GridRow, OwnedValue, push_timestamp};

/// Commits values of one cell from a handler of its element, such as a
/// checkbox's `on_click` or `on_activate`: the same path as an editor's
/// commit with no field. From
/// [`CellElementContext::edit`](super::CellElementContext::edit). Cheap to
/// clone. Call it from an event handler, outside the grid's update, never
/// from `render_cell`.
#[derive(Clone)]
pub struct CellEditHandle {
    grid: Rc<dyn CellCommit>,
    /// The row as the frame showed it: its key finds it in the store.
    row: Arc<dyn Any>,
    column: ColumnIx,
}

impl CellEditHandle {
    pub(super) fn new(grid: Rc<dyn CellCommit>, row: Arc<dyn Any>, column: ColumnIx) -> Self {
        Self { grid, row, column }
    }

    /// Commits `value` as an edit of the cell: `is_editable` must take the
    /// cell now, an unchanged value closes as an editor's does
    /// (`EditCancel { Unchanged }`), `validate_edit` must take the value;
    /// then `EditCommit` goes out and the cell shows the value until the
    /// commit settles. Returns true if it was committed; a refused cell or
    /// value emits nothing.
    pub fn commit(&self, value: OwnedValue, window: &mut Window, cx: &mut App) -> bool {
        self.grid
            .commit(self.row.as_ref(), self.column, value, window, cx)
    }
}

impl std::fmt::Debug for CellEditHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellEditHandle")
            .field("column", &self.column)
            .finish_non_exhaustive()
    }
}

/// A grid that takes commits from its cells' elements, whatever its
/// delegate: one per grid, shared by every handle.
pub(super) trait CellCommit {
    fn commit(
        &self,
        row: &dyn Any,
        column: ColumnIx,
        value: OwnedValue,
        window: &mut Window,
        cx: &mut App,
    ) -> bool;
}

/// [`CellCommit`] of a grid entity.
pub(super) struct GridCommit<D: LiveGridDelegate> {
    pub(super) grid: WeakEntity<LiveGridState<D>>,
}

impl<D: LiveGridDelegate> CellCommit for GridCommit<D> {
    fn commit(
        &self,
        row: &dyn Any,
        column: ColumnIx,
        value: OwnedValue,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(row) = row.downcast_ref::<D::Row>() else {
            return false;
        };
        let column: ColumnOf<D> = column_of(column);
        self.grid
            .update(cx, |grid, cx| {
                grid.commit_from_element(row.key(), column, value, window, cx)
            })
            .unwrap_or(false)
    }
}

/// The largest gpui-kit size whose field fits a row `row_height` high with
/// 2 px of room above and below: `xsmall` rows (26 px) take an `XSmall`
/// field (20 px), `small` and `medium` rows (30, 32) a `Small` one (24),
/// `large` rows (40) a `Medium` one (32).
pub(crate) fn editor_size(row_height: Pixels) -> ComponentSize {
    let room = row_height - px(4.);
    [
        (ComponentSize::Medium, px(32.)),
        (ComponentSize::Small, px(24.)),
    ]
    .into_iter()
    .find(|(_, height)| *height <= room)
    .map_or(ComponentSize::XSmall, |(size, _)| size)
}

/// Appends the exact text of `value` a field starts from: as a raw copy
/// writes it, not rounded as the cell shows it, with `decimal_separator`
/// between the whole and fractional digits. A float that is not finite
/// starts empty; `Bool` as `TRUE` or `FALSE`.
pub(super) fn push_raw_text(value: CellValue<'_>, decimal_separator: char, out: &mut String) {
    let start = out.len();
    match value {
        CellValue::Empty => {}
        CellValue::Bool(value) => out.push_str(if value { "TRUE" } else { "FALSE" }),
        CellValue::Int(value) => {
            let _ = write!(out, "{value}");
        }
        CellValue::Float(value) if value.is_finite() => {
            let _ = write!(out, "{value}");
        }
        CellValue::Float(_) => {}
        #[cfg(feature = "decimal")]
        CellValue::Decimal(value) => {
            let _ = write!(out, "{value}");
        }
        CellValue::Text(text) => out.push_str(text),
        CellValue::Timestamp(value) => push_timestamp(value, out),
        CellValue::Icon(icon) => out.push_str(icon.text()),
        CellValue::Image(image) => out.push_str(image.text()),
        CellValue::Link(link) => out.push_str(link.text()),
        CellValue::Custom(value) => value.write_text(out),
    }
    if value.is_number()
        && decimal_separator != '.'
        && let Some(dot) = out[start..].find('.')
    {
        let dot = start + dot;
        let mut separator = [0; 4];
        out.replace_range(dot..dot + 1, decimal_separator.encode_utf8(&mut separator));
    }
}

/// The text a built-in field starts from: the cell's exact text for a
/// command, a press or a call; empty for `Clear`; the typed text for
/// `Typing`.
pub(super) fn initial_text<R: crate::model::GridRow>(
    start: &super::edit::EditorStart<'_, R>,
    decimal_separator: char,
) -> SharedString {
    match start.input() {
        super::edit::EditInput::Clear => SharedString::default(),
        super::edit::EditInput::Typing(text) => text.clone(),
        _ => {
            let mut text = String::new();
            push_raw_text(start.value(), decimal_separator, &mut text);
            text.into()
        }
    }
}

/// The draft of a built-in editor: its text parsed as a value of the
/// column.
type Parse = Box<dyn Fn(&str) -> Result<OwnedValue, EditError>>;

/// The field of the built-in editors: gpui-kit's single-line `Input`
/// without its own frame (the host draws the cell's), named after the
/// column, its text right-aligned for numbers.
///
/// The field opens without laying its text out. An `InputState` made
/// outside a frame (an editor opens from a key or a click) takes the
/// window's default text style, GPUI's `.SystemUIFont`, until its element
/// first draws in the cell; laying text out before then resolves that
/// family, which GPUI's web platform may lack. So the text is the state's
/// default value, laid out in the first frame in the cell's font.
pub(super) struct InputField {
    state: Entity<InputState>,
    /// The text the field opened with, or the store gave it since: a draft
    /// that differs is dirty.
    initial: RefCell<SharedString>,
    align_right: bool,
    /// What a value given to the field writes between the whole and
    /// fractional digits.
    decimal_separator: char,
    parse: Parse,
}

impl InputField {
    /// A field over `state`, which holds `initial`, parsing its drafts with
    /// `parse`.
    pub(super) fn new(
        state: Entity<InputState>,
        initial: SharedString,
        align_right: bool,
        parse: Parse,
    ) -> Self {
        Self {
            state,
            initial: RefCell::new(initial),
            align_right,
            decimal_separator: '.',
            parse,
        }
    }

    /// Sets the separator a value given to the field is written with, as
    /// its editor parses it (default `.`).
    pub(super) fn with_decimal_separator(mut self, separator: char) -> Self {
        self.decimal_separator = separator;
        self
    }

    /// The state of a single-line input that holds `text` with the caret
    /// at its end, configured by `configure` (a mask, a placeholder). The
    /// text is its default value, laid out in its first frame.
    pub(super) fn input_state(
        text: &SharedString,
        configure: impl FnOnce(InputState) -> InputState,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<InputState> {
        let state = cx.new(|cx| configure(InputState::new(window, cx)).default_value(text.clone()));
        state.update(cx, |state, cx| {
            let end = state.value().len();
            state.set_selected_range(end..end, cx);
        });
        state
    }
}

impl EditorField for InputField {
    fn render(&self, cell: &EditorCell<'_>, _: &mut Window, _: &mut App) -> AnyElement {
        use gpui_kit::component::Sizable as _;

        div()
            .flex_1()
            .min_w_0()
            .child(
                Input::new(&self.state)
                    .appearance(false)
                    .with_size(cell.size())
                    .aria_label(cell.title().clone())
                    .when(self.align_right, |this| this.text_right()),
            )
            .into_any_element()
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    fn value(&self, cx: &App) -> Result<OwnedValue, EditError> {
        (self.parse)(&self.state.read(cx).value())
    }

    fn set_value(&self, value: &OwnedValue, window: &mut Window, cx: &mut App) {
        let mut text = String::new();
        push_raw_text(value.as_cell(), self.decimal_separator, &mut text);
        let text = SharedString::from(text);
        self.state
            .update(cx, |state, cx| state.set_value(text.clone(), window, cx));
        // The store's value is the new starting point: untouched again.
        *self.initial.borrow_mut() = text;
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.state.read(cx).value() != *self.initial.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_editor_size() {
        for (row, size) in [
            (26., ComponentSize::XSmall),
            (30., ComponentSize::Small),
            (32., ComponentSize::Small),
            (40., ComponentSize::Medium),
            (20., ComponentSize::XSmall),
            (60., ComponentSize::Medium),
        ] {
            assert_eq!(editor_size(px(row)), size, "{row}");
        }
    }

    #[test]
    fn test_raw_text() {
        let text = |value: CellValue, separator: char| {
            let mut out = String::from("x");
            push_raw_text(value, separator, &mut out);
            out[1..].to_string()
        };
        // Exact, not as the cell rounds it.
        assert_eq!(text(CellValue::Float(101.2534), '.'), "101.2534");
        assert_eq!(text(CellValue::Float(101.2534), ','), "101,2534");
        assert_eq!(text(CellValue::Float(f64::NAN), '.'), "");
        assert_eq!(text(CellValue::Int(-12), ','), "-12");
        assert_eq!(text(CellValue::Text("a.b"), ','), "a.b");
        assert_eq!(text(CellValue::Empty, '.'), "");
        let placed = chrono::DateTime::from_timestamp(0, 0).expect("the epoch");
        assert_eq!(
            text(CellValue::Timestamp(placed), '.'),
            "1970-01-01 00:00:00"
        );
    }
}
