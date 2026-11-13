//! `TextEditor`: a single-line text field.

use gpui_kit::{App, SharedString, Window};
use rust_i18n::t;

use super::{InputField, initial_text};
use crate::{
    model::{GridRow, OwnedValue},
    state::edit::{CellEditor, EditError, EditorOpen, EditorStart},
};

/// Edits a cell as one line of text, on gpui-kit's `Input`. Values: `Text`;
/// an empty draft is [`Self::with_empty_value`]'s value, `Empty` by default:
/// an empty cell has no value, as in Excel.
///
/// F2 starts from the cell's text with the caret at its end, Backspace and
/// Delete from an empty field, typing from the typed text.
#[derive(Clone, Debug)]
pub struct TextEditor {
    placeholder: Option<SharedString>,
    max_length: Option<usize>,
    empty_value: OwnedValue,
}

impl Default for TextEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEditor {
    /// A text editor without a placeholder or a length limit.
    pub fn new() -> Self {
        Self {
            placeholder: None,
            max_length: None,
            empty_value: OwnedValue::Empty,
        }
    }

    /// Sets the text an empty field shows.
    pub fn with_placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = Some(text.into());
        self
    }

    /// Refuses a draft longer than `chars` characters.
    pub fn with_max_length(mut self, chars: usize) -> Self {
        self.max_length = Some(chars);
        self
    }

    /// Sets what an empty draft commits (default `Empty`), such as
    /// `OwnedValue::Text("".into())`.
    pub fn with_empty_value(mut self, value: OwnedValue) -> Self {
        self.empty_value = value;
        self
    }

    /// The text an empty field shows.
    pub fn placeholder(&self) -> Option<&SharedString> {
        self.placeholder.as_ref()
    }

    /// The longest draft, in characters.
    pub fn max_length(&self) -> Option<usize> {
        self.max_length
    }

    /// What an empty draft commits.
    pub fn empty_value(&self) -> &OwnedValue {
        &self.empty_value
    }

    /// Parses a draft.
    fn parse(
        draft: &str,
        max_length: Option<usize>,
        empty: &OwnedValue,
    ) -> Result<OwnedValue, EditError> {
        if draft.is_empty() {
            return Ok(empty.clone());
        }
        if let Some(max) = max_length
            && draft.chars().count() > max
        {
            return Err(EditError::new(t!("live_grid.edit.too_long", count = max)));
        }
        Ok(OwnedValue::Text(draft.into()))
    }
}

impl<R: GridRow> CellEditor<R> for TextEditor {
    fn open(&self, start: &EditorStart<'_, R>, window: &mut Window, cx: &mut App) -> EditorOpen {
        let text = initial_text(start, '.');
        let placeholder = self.placeholder.clone();
        let state = InputField::input_state(
            &text,
            |state| match placeholder {
                Some(placeholder) => state.placeholder(placeholder),
                None => state,
            },
            window,
            cx,
        );
        let (max_length, empty) = (self.max_length, self.empty_value.clone());
        EditorOpen::Field(Box::new(InputField::new(
            state,
            text,
            false,
            Box::new(move |draft| Self::parse(draft, max_length, &empty)),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_editor_parse() {
        let parse = |draft: &str, max: Option<usize>, empty: &OwnedValue| {
            TextEditor::parse(draft, max, empty)
        };
        assert_eq!(parse("", None, &OwnedValue::Empty), Ok(OwnedValue::Empty));
        let blank = OwnedValue::Text("".into());
        assert_eq!(parse("", None, &blank), Ok(blank.clone()));
        assert_eq!(
            parse("Сбер", Some(4), &OwnedValue::Empty),
            Ok(OwnedValue::Text("Сбер".into()))
        );
        let error = parse("Сбербанк", Some(4), &OwnedValue::Empty).unwrap_err();
        assert_eq!(error.message().as_ref(), "Use at most 4 characters");
        let editor = TextEditor::new()
            .with_placeholder("Note")
            .with_max_length(10)
            .with_empty_value(blank.clone());
        assert_eq!(editor.placeholder().map(AsRef::as_ref), Some("Note"));
        assert_eq!(editor.max_length(), Some(10));
        assert_eq!(editor.empty_value(), &blank);
    }
}
