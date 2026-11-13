//! `NumberEditor`: a number field.

use gpui_kit::component::input::MaskPattern;
use gpui_kit::{App, Window};
use rust_i18n::t;

use super::{InputField, initial_text, push_raw_text};
use crate::{
    model::{CellValue, GridRow, OwnedValue},
    state::edit::{CellEditor, EditError, EditInput, EditorOpen, EditorStart},
};

/// The kind of number a [`NumberEditor`] commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NumberKind {
    /// `OwnedValue::Int`: a whole number.
    Int,
    /// `OwnedValue::Float`.
    Float,
    /// `OwnedValue::Decimal`: an exact decimal (the `decimal` feature).
    #[cfg(feature = "decimal")]
    Decimal,
}

/// Edits a cell as a number, on gpui-kit's `Input` with a number mask. The
/// kind follows the cell's value (`Int`, `Float`, `Decimal`; `Float` for an
/// empty cell) unless [`Self::with_kind`] sets it; an empty draft is
/// `Empty`.
///
/// The field starts from the exact value, not the two decimals the cell
/// shows. A draft is parsed with
/// [`Self::with_decimal_separator`] between the whole and fractional
/// digits; spaces and the other separator, a thousands one, are dropped.
/// Typing starts an edit only with a digit, a sign or a separator.
#[derive(Clone, Debug)]
pub struct NumberEditor {
    kind: Option<NumberKind>,
    min: Option<OwnedValue>,
    max: Option<OwnedValue>,
    scale: Option<u32>,
    decimal_separator: char,
}

impl Default for NumberEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl NumberEditor {
    /// A number editor of the cell's kind, without limits, with `.` between
    /// the whole and fractional digits.
    pub fn new() -> Self {
        Self {
            kind: None,
            min: None,
            max: None,
            scale: None,
            decimal_separator: '.',
        }
    }

    /// Commits numbers of `kind`, whatever the cell holds.
    pub fn with_kind(mut self, kind: NumberKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Refuses a number below `min`.
    pub fn with_min(mut self, min: impl Into<OwnedValue>) -> Self {
        self.min = Some(min.into());
        self
    }

    /// Refuses a number above `max`.
    pub fn with_max(mut self, max: impl Into<OwnedValue>) -> Self {
        self.max = Some(max.into());
        self
    }

    /// Refuses a number with more than `scale` decimals (trailing zeros do
    /// not count).
    pub fn with_scale(mut self, scale: u32) -> Self {
        self.scale = Some(scale);
        self
    }

    /// Sets the character between the whole and fractional digits (default
    /// `.`; `,` for a Russian-language product).
    /// gpui-kit's number mask knows only `.`: with another separator the
    /// field takes any text and the parser refuses what is not a number.
    pub fn with_decimal_separator(mut self, separator: char) -> Self {
        self.decimal_separator = separator;
        self
    }

    /// The kind set with [`Self::with_kind`].
    pub fn kind(&self) -> Option<NumberKind> {
        self.kind
    }

    /// The smallest number taken.
    pub fn min(&self) -> Option<&OwnedValue> {
        self.min.as_ref()
    }

    /// The largest number taken.
    pub fn max(&self) -> Option<&OwnedValue> {
        self.max.as_ref()
    }

    /// The most decimals taken.
    pub fn scale(&self) -> Option<u32> {
        self.scale
    }

    /// The character between the whole and fractional digits.
    pub fn decimal_separator(&self) -> char {
        self.decimal_separator
    }

    /// The kind a cell of `value` is edited as.
    fn kind_for(&self, value: CellValue<'_>) -> NumberKind {
        self.kind.unwrap_or(match value {
            CellValue::Int(_) => NumberKind::Int,
            #[cfg(feature = "decimal")]
            CellValue::Decimal(_) => NumberKind::Decimal,
            _ => NumberKind::Float,
        })
    }

    /// Returns true if `text` may start a number: digits, a sign, a
    /// separator.
    fn may_start(&self, text: &str) -> bool {
        text.chars().all(|ch| {
            ch.is_ascii_digit()
                || matches!(ch, '-' | '+' | '.' | ',')
                || ch == self.decimal_separator
        })
    }

    /// Parses a draft as a number of `kind`.
    fn parse(&self, draft: &str, kind: NumberKind) -> Result<OwnedValue, EditError> {
        let separator = self.decimal_separator;
        // Spaces and the thousands separator go; the decimal one becomes `.`.
        let thousands = if separator == '.' { ',' } else { '.' };
        let text: String = draft
            .chars()
            .filter(|ch| !ch.is_whitespace() && *ch != thousands && *ch != '\'')
            .map(|ch| if ch == separator { '.' } else { ch })
            .collect();
        if text.is_empty() {
            return Ok(OwnedValue::Empty);
        }
        let not_a_number = || EditError::new(t!("live_grid.edit.not_a_number"));
        // `parse::<f64>` takes "inf", "NaN" and exponents: a number of the
        // field has digits, a sign and one point alone.
        let is_plain = text
            .trim_start_matches(['-', '+'])
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '.')
            && text.matches('.').count() <= 1
            && text.chars().any(|ch| ch.is_ascii_digit());
        if !is_plain {
            return Err(not_a_number());
        }
        let decimals = text
            .split_once('.')
            .map_or(0, |(_, fraction)| fraction.trim_end_matches('0').len());
        if let Some(scale) = self.scale
            && decimals > scale as usize
        {
            return Err(EditError::new(t!(
                "live_grid.edit.too_many_decimals",
                scale = scale
            )));
        }
        let value = match kind {
            NumberKind::Int if decimals > 0 => {
                return Err(EditError::new(t!("live_grid.edit.not_a_whole_number")));
            }
            NumberKind::Int => {
                let whole = text.split('.').next().unwrap_or_default();
                OwnedValue::Int(whole.parse().map_err(|_| not_a_number())?)
            }
            NumberKind::Float => {
                let value: f64 = text.parse().map_err(|_| not_a_number())?;
                if !value.is_finite() {
                    return Err(not_a_number());
                }
                OwnedValue::Float(value)
            }
            #[cfg(feature = "decimal")]
            NumberKind::Decimal => OwnedValue::Decimal(
                rust_decimal::Decimal::from_str_exact(text.trim_start_matches('+'))
                    .map_err(|_| not_a_number())?,
            ),
        };
        let text_of = |limit: &OwnedValue| {
            let mut out = String::new();
            push_raw_text(limit.as_cell(), separator, &mut out);
            out
        };
        if let Some(min) = &self.min
            && value.cmp_total(min).is_lt()
        {
            return Err(EditError::new(t!(
                "live_grid.edit.below_min",
                min = text_of(min)
            )));
        }
        if let Some(max) = &self.max
            && value.cmp_total(max).is_gt()
        {
            return Err(EditError::new(t!(
                "live_grid.edit.above_max",
                max = text_of(max)
            )));
        }
        Ok(value)
    }
}

impl<R: GridRow> CellEditor<R> for NumberEditor {
    fn open(&self, start: &EditorStart<'_, R>, window: &mut Window, cx: &mut App) -> EditorOpen {
        if let EditInput::Typing(text) = start.input()
            && !self.may_start(text)
        {
            return EditorOpen::Refuse;
        }
        let kind = self.kind_for(start.value());
        let text = initial_text(start, self.decimal_separator);
        let mask = (self.decimal_separator == '.').then_some(MaskPattern::Number {
            separator: None,
            fraction: None,
        });
        let state = InputField::input_state(
            &text,
            |state| match mask {
                Some(mask) => state.mask_pattern(mask),
                None => state,
            },
            window,
            cx,
        );
        let editor = self.clone();
        EditorOpen::Field(Box::new(
            InputField::new(
                state,
                text,
                true,
                Box::new(move |draft| editor.parse(draft, kind)),
            )
            .with_decimal_separator(self.decimal_separator),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(result: Result<OwnedValue, EditError>) -> String {
        result.expect_err("refused").message().to_string()
    }

    #[test]
    fn test_number_editor_parse() {
        let editor = NumberEditor::new();
        // The kind decides the value; an empty draft is `Empty`.
        assert_eq!(
            editor.parse("-12", NumberKind::Int),
            Ok(OwnedValue::Int(-12))
        );
        assert_eq!(
            editor.parse("-0.5", NumberKind::Float),
            Ok(OwnedValue::Float(-0.5))
        );
        assert_eq!(editor.parse("  ", NumberKind::Float), Ok(OwnedValue::Empty));
        // Spaces and thousands separators go.
        assert_eq!(
            editor.parse("1 234.50", NumberKind::Float),
            Ok(OwnedValue::Float(1234.5))
        );
        assert_eq!(
            editor.parse("1,234", NumberKind::Int),
            Ok(OwnedValue::Int(1234))
        );
        // A whole number has no decimals; trailing zeros are none.
        assert_eq!(
            error(editor.parse("1.5", NumberKind::Int)),
            "Enter a whole number"
        );
        assert_eq!(
            editor.parse("2.00", NumberKind::Int),
            Ok(OwnedValue::Int(2))
        );
        for text in ["abc", "1e3", "inf", "NaN", "1.2.3", "-", "."] {
            assert_eq!(
                error(editor.parse(text, NumberKind::Float)),
                "Enter a number",
                "{text}"
            );
        }
        // Limits and scale, with their texts.
        let limited = NumberEditor::new()
            .with_min(OwnedValue::Int(0))
            .with_max(OwnedValue::Float(200.5))
            .with_scale(2);
        assert_eq!(
            error(limited.parse("-1", NumberKind::Float)),
            "Enter 0 or more"
        );
        assert_eq!(
            error(limited.parse("201", NumberKind::Float)),
            "Enter 200.5 or less"
        );
        assert_eq!(
            error(limited.parse("1.234", NumberKind::Float)),
            "Use at most 2 decimals"
        );
        assert_eq!(
            limited.parse("1.230", NumberKind::Float),
            Ok(OwnedValue::Float(1.23))
        );
        // Another decimal separator: `.` is a thousands one then.
        let comma = NumberEditor::new().with_decimal_separator(',');
        assert_eq!(
            comma.parse("1.234,5", NumberKind::Float),
            Ok(OwnedValue::Float(1234.5))
        );
        assert_eq!(comma.decimal_separator(), ',');
        // Typing starts only a number.
        assert!(editor.may_start("5") && editor.may_start("-") && editor.may_start("."));
        assert!(!editor.may_start("a") && !editor.may_start("+a"));
        // The kind follows the cell unless set.
        assert_eq!(editor.kind_for(CellValue::Int(1)), NumberKind::Int);
        assert_eq!(editor.kind_for(CellValue::Empty), NumberKind::Float);
        assert_eq!(
            editor
                .clone()
                .with_kind(NumberKind::Int)
                .kind_for(CellValue::Float(1.)),
            NumberKind::Int
        );
        assert_eq!(limited.scale(), Some(2));
        assert_eq!(limited.min(), Some(&OwnedValue::Int(0)));
        assert_eq!(limited.max(), Some(&OwnedValue::Float(200.5)));
        assert_eq!(editor.kind(), None);
    }

    #[cfg(feature = "decimal")]
    #[test]
    fn test_number_editor_decimal() {
        let editor = NumberEditor::new().with_scale(2);
        let value = editor.parse("12.30", NumberKind::Decimal);
        assert!(
            matches!(&value, Ok(OwnedValue::Decimal(value)) if value.to_string() == "12.30"),
            "{value:?}"
        );
        assert_eq!(
            error(editor.parse("1.234", NumberKind::Decimal)),
            "Use at most 2 decimals"
        );
    }
}
