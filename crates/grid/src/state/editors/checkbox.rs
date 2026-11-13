//! `CheckboxEditor`: a flag toggled at once, with no field.

use gpui_kit::{App, Window};

use crate::{
    model::{CellValue, GridRow, OwnedValue},
    state::edit::{CellEditor, EditInput, EditorOpen, EditorStart},
};

/// Toggles a `Bool` cell at once, with no field: F2, a double click on a
/// `Text` column and a call commit the opposite value (`Bool(true)` for an
/// empty cell) as `EditorOpen::Commit`, with no `EditStart`. Typing,
/// Backspace and Delete take no edit. On a `Control` column the checkbox
/// element toggles itself through
/// [`CellElementContext::edit`](crate::CellElementContext::edit), Space
/// included (`on_activate`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CheckboxEditor;

impl CheckboxEditor {
    /// The value a start commits: the opposite of a flag, `true` for an
    /// empty cell; `None` for another value or an input that takes no edit.
    fn toggled(value: CellValue<'_>, input: &EditInput) -> Option<OwnedValue> {
        if matches!(input, EditInput::Typing(_) | EditInput::Clear) {
            return None;
        }
        match value {
            CellValue::Bool(on) => Some(OwnedValue::Bool(!on)),
            CellValue::Empty => Some(OwnedValue::Bool(true)),
            _ => None,
        }
    }
}

impl<R: GridRow> CellEditor<R> for CheckboxEditor {
    fn open(&self, start: &EditorStart<'_, R>, _: &mut Window, _: &mut App) -> EditorOpen {
        Self::toggled(start.value(), start.input()).map_or(EditorOpen::Refuse, EditorOpen::Commit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkbox_toggles() {
        let toggled = CheckboxEditor::toggled;
        assert_eq!(
            toggled(CellValue::Bool(false), &EditInput::Command),
            Some(OwnedValue::Bool(true))
        );
        assert_eq!(
            toggled(CellValue::Bool(true), &EditInput::Press { click_count: 2 }),
            Some(OwnedValue::Bool(false))
        );
        assert_eq!(
            toggled(CellValue::Empty, &EditInput::Programmatic),
            Some(OwnedValue::Bool(true))
        );
        assert_eq!(toggled(CellValue::Bool(true), &EditInput::Clear), None);
        assert_eq!(
            toggled(CellValue::Bool(true), &EditInput::Typing("x".into())),
            None
        );
        assert_eq!(toggled(CellValue::Int(1), &EditInput::Command), None);
    }
}
