//! `DateEditor`: a date, or a date and a time, from a calendar under the
//! cell.
//!
//! The calendar is gpui-kit's public `Calendar` (and `TimeField` for a
//! time) in the editor's own popup, not its `DatePicker`: a picker has no
//! public way to open, and its calendar is a layer whose bounds the grid
//! cannot know, so a press on it would read as a press outside the editor
//! and end the edit.

use std::cell::{Cell, RefCell};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone as _, Utc};
use gpui_kit::component::calendar::{Calendar, CalendarEvent, CalendarState, Date};
use gpui_kit::component::time_field::{TimeField, TimeFieldState, TimePrecision};
use gpui_kit::component::{Sizable as _, Size as ComponentSize};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, FocusHandle, InteractiveElement as _,
    IntoElement as _, ParentElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::{
    actions::CommitEdit,
    model::{CellValue, GridRow, OwnedValue},
    state::edit::{
        CellEditor, EditError, EditInput, EditorCell, EditorField, EditorOpen, EditorStart,
    },
};

/// Edits a `Timestamp` cell with a calendar under it.
///
/// [`Self::date`] commits a date at 00:00 of its offset as soon as a day is
/// picked; [`Self::date_time`] also edits the time, and commits on Enter or
/// Tab. The offset is UTC unless [`Self::with_offset`] sets another: the
/// day a cell shows and the day picked are days of that offset. Escape
/// cancels. Backspace and Delete start with no date, which commits `Empty`;
/// typing takes no edit.
#[derive(Clone, Debug)]
pub struct DateEditor {
    with_time: bool,
    offset: FixedOffset,
}

impl DateEditor {
    /// A date: a day picked commits that day at 00:00 of the offset.
    pub fn date() -> Self {
        Self {
            with_time: false,
            offset: FixedOffset::east_opt(0).expect("UTC is an offset"),
        }
    }

    /// A date and a time of day, to the minute.
    pub fn date_time() -> Self {
        Self {
            with_time: true,
            ..Self::date()
        }
    }

    /// Sets the offset whose days and times the editor shows and commits
    /// (default UTC).
    pub fn with_offset(mut self, offset: FixedOffset) -> Self {
        self.offset = offset;
        self
    }

    /// The offset of the days and times.
    pub fn offset(&self) -> FixedOffset {
        self.offset
    }

    /// Returns true if the editor edits the time of day too.
    pub fn has_time(&self) -> bool {
        self.with_time
    }

    /// The day and time a start shows: the cell's, in the offset; no day for
    /// an empty cell or `Clear`. `None` for typing, or a value that is not a
    /// point in time.
    fn start_draft(
        &self,
        value: CellValue<'_>,
        input: &EditInput,
    ) -> Option<(Option<NaiveDate>, NaiveTime)> {
        match (input, value) {
            (EditInput::Typing(_), _) => None,
            (EditInput::Clear, CellValue::Timestamp(_) | CellValue::Empty) => {
                Some((None, NaiveTime::MIN))
            }
            (_, CellValue::Empty) => Some((None, NaiveTime::MIN)),
            (_, CellValue::Timestamp(at)) => {
                let local = at.with_timezone(&self.offset);
                let time = if self.with_time {
                    TimePrecision::Minute.truncate(local.time())
                } else {
                    NaiveTime::MIN
                };
                Some((Some(local.date_naive()), time))
            }
            _ => None,
        }
    }
}

/// The value of a day and a time of day in `offset`.
fn to_value(day: Option<NaiveDate>, time: NaiveTime, offset: FixedOffset) -> OwnedValue {
    let Some(day) = day else {
        return OwnedValue::Empty;
    };
    // A fixed offset maps every local time to exactly one instant.
    offset
        .from_local_datetime(&day.and_time(time))
        .single()
        .map_or(OwnedValue::Empty, |at| {
            OwnedValue::Timestamp(at.with_timezone(&Utc))
        })
}

impl<R: GridRow> CellEditor<R> for DateEditor {
    fn open(&self, start: &EditorStart<'_, R>, window: &mut Window, cx: &mut App) -> EditorOpen {
        let Some((day, time)) = self.start_draft(start.value(), start.input()) else {
            return EditorOpen::Refuse;
        };
        let calendar = cx.new(|cx| {
            let mut calendar = CalendarState::new(window, cx);
            calendar.set_date(Date::Single(day), window, cx);
            calendar
        });
        let time_field = self.with_time.then(|| {
            cx.new(|cx| {
                let mut field = TimeFieldState::new(window, cx).precision(TimePrecision::Minute);
                field.set_time(time, window, cx);
                field
            })
        });
        // A day picked commits a date, as the editor's Enter does.
        let with_time = self.with_time;
        let picked = window.subscribe(
            &calendar,
            cx,
            move |_, event: &CalendarEvent, window, cx| {
                let CalendarEvent::Selected(_) = event;
                if !with_time {
                    window.dispatch_action(Box::new(CommitEdit), cx);
                }
            },
        );
        EditorOpen::Field(Box::new(DateField {
            calendar,
            time_field,
            offset: self.offset,
            start: RefCell::new(OwnedValue::from_cell(start.value())),
            initial: Cell::new((day, time)),
            focus: cx.focus_handle(),
            _picked: picked,
        }))
    }
}

/// An open [`DateEditor`]: the draft's text in the cell, the calendar under
/// it.
struct DateField {
    calendar: Entity<CalendarState>,
    /// For [`DateEditor::date_time`].
    time_field: Option<Entity<TimeFieldState>>,
    offset: FixedOffset,
    /// The cell's value: the draft until a day or a time is picked, so an
    /// untouched editor keeps a time of day it does not show.
    start: RefCell<OwnedValue>,
    /// The day and time it opened with, or the store gave it since.
    initial: Cell<(Option<NaiveDate>, NaiveTime)>,
    /// The cell part's focus: the keys of the editor go there.
    focus: FocusHandle,
    _picked: Subscription,
}

impl DateField {
    /// The draft: the picked day and the time of day.
    fn draft(&self, cx: &App) -> (Option<NaiveDate>, NaiveTime) {
        let day = match self.calendar.read(cx).date() {
            Date::Single(day) => day,
            Date::Range(start, _) => start,
        };
        let time = self
            .time_field
            .as_ref()
            .map_or(NaiveTime::MIN, |field| field.read(cx).time());
        (day, time)
    }

    /// The draft as the cell shows it in the editor.
    fn text(&self, cx: &App) -> String {
        let (day, time) = self.draft(cx);
        let Some(day) = day else {
            return String::new();
        };
        let at: DateTime<FixedOffset> = match self.offset.from_local_datetime(&day.and_time(time)) {
            chrono::LocalResult::Single(at) => at,
            _ => return String::new(),
        };
        if self.time_field.is_some() {
            at.format("%Y-%m-%d %H:%M").to_string()
        } else {
            at.format("%Y-%m-%d").to_string()
        }
    }
}

impl EditorField for DateField {
    fn render(&self, cell: &EditorCell<'_>, _: &mut Window, cx: &mut App) -> AnyElement {
        let size = match cell.size() {
            ComponentSize::XSmall | ComponentSize::Small => ComponentSize::Small,
            size => size,
        };
        let width = if size == ComponentSize::Small {
            px(196.)
        } else {
            px(224.)
        };
        div()
            .flex_1()
            .min_w_0()
            .px(px(4.))
            .truncate()
            .track_focus(&self.focus)
            .child(self.text(cx))
            .child(
                cell.popup(
                    div()
                        .p(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            // As gpui-kit's `DatePicker` draws it: the popup is the
                            // frame, so the calendar's own border and padding go.
                            Calendar::new(&self.calendar)
                                .border_0()
                                .rounded_none()
                                .p_0()
                                .with_size(size)
                                .w(width),
                        )
                        .children(
                            self.time_field
                                .as_ref()
                                .map(|field| TimeField::new(field).with_size(size)),
                        ),
                    cx,
                ),
            )
            .into_any_element()
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn value(&self, cx: &App) -> Result<OwnedValue, EditError> {
        let draft = self.draft(cx);
        if draft == self.initial.get() {
            return Ok(self.start.borrow().clone());
        }
        Ok(to_value(draft.0, draft.1, self.offset))
    }

    fn set_value(&self, value: &OwnedValue, window: &mut Window, cx: &mut App) {
        let (day, time) = match value {
            OwnedValue::Timestamp(at) => {
                let local = at.with_timezone(&self.offset);
                let time = if self.time_field.is_some() {
                    TimePrecision::Minute.truncate(local.time())
                } else {
                    NaiveTime::MIN
                };
                (Some(local.date_naive()), time)
            }
            _ => (None, NaiveTime::MIN),
        };
        self.calendar.update(cx, |calendar, cx| {
            calendar.set_date(Date::Single(day), window, cx)
        });
        if let Some(field) = &self.time_field {
            field.update(cx, |field, cx| field.set_time(time, window, cx));
        }
        // The store's value is the new starting point: untouched again.
        *self.start.borrow_mut() = value.clone();
        self.initial.set((day, time));
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.draft(cx) != self.initial.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a time")
            .with_timezone(&Utc)
    }

    #[test]
    fn test_date_editor_values() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 4).expect("a day");
        let utc = DateEditor::date();
        assert!(!utc.has_time());
        assert_eq!(utc.offset().local_minus_utc(), 0);
        assert_eq!(
            to_value(Some(day), NaiveTime::MIN, utc.offset()),
            OwnedValue::Timestamp(at("2026-11-04T00:00:00Z"))
        );
        assert_eq!(
            to_value(None, NaiveTime::MIN, utc.offset()),
            OwnedValue::Empty
        );
        // 00:00 in Moscow is 21:00 UTC the day before.
        let moscow = DateEditor::date().with_offset(FixedOffset::east_opt(3 * 3600).expect("+03"));
        assert_eq!(
            to_value(Some(day), NaiveTime::MIN, moscow.offset()),
            OwnedValue::Timestamp(at("2026-11-03T21:00:00Z"))
        );
        // The cell's day is the offset's: 22:30 UTC is the next day there.
        let placed = at("2026-11-04T22:30:45Z");
        let start = |editor: &DateEditor, input: &EditInput| {
            editor.start_draft(CellValue::Timestamp(placed), input)
        };
        assert_eq!(
            start(&moscow, &EditInput::Command),
            Some((NaiveDate::from_ymd_opt(2026, 11, 5), NaiveTime::MIN))
        );
        let with_time = DateEditor::date_time();
        assert!(with_time.has_time());
        assert_eq!(
            start(&with_time, &EditInput::Command),
            Some((
                Some(day),
                NaiveTime::from_hms_opt(22, 30, 0).expect("a time")
            ))
        );
        assert_eq!(
            start(&with_time, &EditInput::Clear),
            Some((None, NaiveTime::MIN))
        );
        assert_eq!(start(&with_time, &EditInput::Typing("1".into())), None);
        assert_eq!(
            utc.start_draft(CellValue::Int(1), &EditInput::Command),
            None
        );
    }
}
