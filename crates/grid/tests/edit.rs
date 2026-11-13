//! Headless UI tests of editing: how an edit starts, commits, cancels and moves
//! the focus; the field's keys, its message and its host; a press or a focus
//! change outside the field; the edit API; the list, the calendar, the checkbox
//! and commits from an element in a cell.

mod common;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use chrono::{DateTime, Utc};
use gpui_kit::component::{Sizable as _, checkbox::Checkbox};
use gpui_kit::{
    AnyElement, App, AppContext as _, Focusable as _, InputEvent as _, IntoElement as _, Modifiers,
    MouseButton, Role, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, Window, point,
    px, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    AfterCommit, CellContent, CellEditHandle, CellElementContext, CellMove, CellValue,
    CheckboxEditor, DateEditor, DraftAction, EditCancelReason, EditCommitKey, EditEnd, EditError,
    EditGesture, EditGestureContext, EditInput, EditOrigin, EditPolicy, EditPolicyContext,
    EditValidateContext, EditableContext, EditorMark, GridColumn, GridColumns as _, GridSchema,
    LiveGridDelegate, LiveGridEvent, NumberEditor, OwnedValue, SelectEditor, SelectOption,
    SortDirection,
};

use crate::common::{
    EditFixture, EditHooks, ORDER_COLUMNS, Order, OrderColumn, ROW, cell_host_id, open_editable,
    open_orders_in, open_orders_with, order_cell, order_row_id, orders, press_at,
    with_order_editors,
};

/// What an edit test watches: the edit events, the selection and focus
/// changes, clicks, sorts and confirmations, in order.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Start(u32, OrderColumn, EditOrigin),
    Commit(u32, OrderColumn, OwnedValue),
    Cancel(u32, OrderColumn, EditCancelReason, Option<OwnedValue>),
    Selection,
    Focus,
    Click,
    Sort,
    Confirm,
}

fn seen(fixture: &EditFixture<EditHooks>) -> Vec<Seen> {
    fixture
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::EditStart {
                key,
                column,
                origin,
            } => Some(Seen::Start(key, column, origin)),
            LiveGridEvent::EditCommit {
                key, column, value, ..
            } => Some(Seen::Commit(key, column, value)),
            LiveGridEvent::EditCancel {
                key,
                column,
                reason,
                draft,
            } => Some(Seen::Cancel(key, column, reason, draft)),
            LiveGridEvent::SelectionChange { .. } => Some(Seen::Selection),
            LiveGridEvent::FocusChange { .. } => Some(Seen::Focus),
            LiveGridEvent::Click { .. } => Some(Seen::Click),
            LiveGridEvent::SortChange { .. } => Some(Seen::Sort),
            LiveGridEvent::Confirm { .. } => Some(Seen::Confirm),
            _ => None,
        })
        .collect()
}

/// Order `key`'s price: `100 + key / 4`.
fn price(key: u32) -> f64 {
    100. + f64::from(key) / 4.
}

#[gpui_kit::test]
fn test_start_edit_inputs(cx: &mut TestAppContext) {
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, OrderColumn::Price);
    seen(&fixture);
    use OrderColumn::Price;

    // F2 starts from the exact value, Backspace and Delete from nothing,
    // typing from the typed text.
    for (keys, text) in [
        (&["f2"][..], "100.75"),
        (&["backspace"][..], ""),
        (&["delete"][..], ""),
    ] {
        for key in keys {
            fixture.press_key(cx, key);
        }
        fixture.settle(cx);
        assert_eq!(fixture.editing(cx), Some((3, Price)), "{keys:?}");
        assert_eq!(fixture.editor_text(cx).as_deref(), Some(text), "{keys:?}");
        assert_eq!(seen(&fixture), [Seen::Start(3, Price, EditOrigin::User)]);
        fixture.press_key(cx, "escape");
        fixture.settle(cx);
        assert_eq!(fixture.editing(cx), None);
        seen(&fixture);
    }
    fixture.type_text(cx, "5");
    fixture.settle(cx);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("5"));
    assert_eq!(seen(&fixture), [Seen::Start(3, Price, EditOrigin::User)]);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    seen(&fixture);

    // A double click on a cell with an editor edits it, with no `Confirm`.
    fixture.press_cell(cx, 4, Price, 1);
    fixture.press_cell(cx, 4, Price, 2);
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((5, Price)));
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("101.25"));
    let events = seen(&fixture);
    assert!(events.contains(&Seen::Start(5, Price, EditOrigin::User)));
    assert!(!events.contains(&Seen::Confirm), "{events:?}");
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    seen(&fixture);

    // A column without an editor, or a filled order: nothing; F2 goes on,
    // a double click confirms, typing does nothing.
    fixture.focus_cell(cx, 3, OrderColumn::Side);
    fixture.press_key(cx, "f2");
    fixture.type_text(cx, "5");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
    fixture.press_cell(cx, 2, OrderColumn::Side, 1);
    fixture.press_cell(cx, 2, OrderColumn::Side, 2);
    assert!(seen(&fixture).contains(&Seen::Confirm));
}

#[gpui_kit::test]
fn test_filled_order_is_not_editable(cx: &mut TestAppContext) {
    let hooks = EditHooks::default();
    hooks.filled.borrow_mut().push(3);
    let fixture = open_orders_with(cx, orders(60), hooks.clone(), with_order_editors);
    fixture.focus_cell(cx, 3, OrderColumn::Price);
    fixture.take_events();
    for key in ["f2", "backspace"] {
        fixture.press_key(cx, key);
    }
    fixture.type_text(cx, "5");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
    assert!(hooks.calls.borrow().contains(&"is_editable"));
    // Another order of the column takes the edit.
    fixture.focus_cell(cx, 4, OrderColumn::Price);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((4, OrderColumn::Price)));
}

/// Types into the open field: clears it first, with Backspace through the
/// grid's `StartEditEmpty` when no field is open.
fn edit_with(fixture: &EditFixture<EditHooks>, cx: &mut TestAppContext, text: &str) {
    fixture.press_key(cx, "backspace");
    fixture.settle(cx);
    fixture.type_text(cx, text);
    fixture.settle(cx);
}

#[gpui_kit::test]
fn test_commit_moves_focus(cx: &mut TestAppContext) {
    let fixture = open_editable(cx, |grid| grid);
    use OrderColumn::{Price, Quantity, Status};
    fixture.focus_cell(cx, 3, Price);
    seen(&fixture);

    // Enter: `EditCommit` with the value, the value the edit started from
    // and the store's value; the focus goes down.
    edit_with(&fixture, cx, "101.5");
    fixture.take_events();
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let events = fixture.take_events();
    let commit = events
        .iter()
        .find_map(|event| match event {
            LiveGridEvent::EditCommit {
                key,
                column,
                value,
                base,
                current,
                ..
            } => Some((*key, *column, value.clone(), base.clone(), current.clone())),
            _ => None,
        })
        .expect("a commit");
    assert_eq!(
        commit,
        (
            3,
            Price,
            OwnedValue::Float(101.5),
            OwnedValue::Float(price(3)),
            OwnedValue::Float(price(3)),
        )
    );
    assert_eq!(fixture.editing(cx), None);
    assert_eq!(fixture.focused(cx), Some((4, Price)));

    // The other keys of the field, each after a change.
    for (key, focus) in [
        ("shift-enter", (3, Price)),
        ("up", (2, Price)),
        ("down", (3, Price)),
        ("shift-tab", (3, Quantity)),
        ("tab", (3, Price)),
    ] {
        let before = fixture.focused(cx).expect("a focused cell");
        fixture.press_key(cx, "f2");
        fixture.settle(cx);
        assert_eq!(fixture.editing(cx), Some(before), "{key}");
        fixture.type_text(cx, "1");
        fixture.press_key(cx, key);
        fixture.settle(cx);
        assert_eq!(fixture.editing(cx), None, "{key}");
        assert_eq!(fixture.focused(cx), Some(focus), "{key}");
        assert!(
            seen(&fixture)
                .iter()
                .any(|event| matches!(event, Seen::Commit(..))),
            "{key}"
        );
    }
    // Tab right of the price: the status, which has no editor.
    fixture.press_key(cx, "f2");
    fixture.type_text(cx, "2");
    fixture.press_key(cx, "tab");
    fixture.settle(cx);
    assert_eq!(fixture.focused(cx), Some((3, Status)));
    // Tab on the last column keeps the focus.
    fixture.focus_cell(cx, 3, OrderColumn::Note);
    fixture.press_key(cx, "f2");
    fixture.type_text(cx, "x");
    fixture.press_key(cx, "tab");
    fixture.settle(cx);
    assert_eq!(fixture.focused(cx), Some((3, OrderColumn::Note)));
}

/// Enter stays; Tab edits the next editable cell (AG Grid).
struct StayAndEditNext;

impl EditGesture for StayAndEditNext {
    fn starts_edit(&self, context: &EditGestureContext) -> bool {
        !matches!(context.input(), EditInput::Press { click_count: 1 })
    }

    fn after_commit(&self, key: EditCommitKey) -> AfterCommit {
        match key {
            EditCommitKey::Next => AfterCommit::MoveAndEdit(CellMove::NextEditable),
            EditCommitKey::Prev => AfterCommit::MoveAndEdit(CellMove::PrevEditable),
            _ => AfterCommit::Stay,
        }
    }
}

#[gpui_kit::test]
fn test_custom_edit_gesture(cx: &mut TestAppContext) {
    let fixture = open_editable(cx, |grid| grid.with_edit_gesture(StayAndEditNext));
    use OrderColumn::{Instrument, Quantity};
    fixture.focus_cell(cx, 3, Instrument);
    fixture.press_key(cx, "f2");
    fixture.type_text(cx, "X");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(fixture.focused(cx), Some((3, Instrument)), "Enter stays");
    // Tab skips the side, which has no editor, and edits the quantity.
    fixture.press_key(cx, "f2");
    fixture.press_key(cx, "tab");
    fixture.settle(cx);
    assert_eq!(fixture.focused(cx), Some((3, Quantity)));
    assert_eq!(fixture.editing(cx), Some((3, Quantity)));
    fixture.press_key(cx, "shift-tab");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Instrument)));
}

#[gpui_kit::test]
fn test_cancel_edit(cx: &mut TestAppContext) {
    let fixture = open_editable(cx, |grid| grid);
    use OrderColumn::Price;
    fixture.focus_cell(cx, 3, Price);
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(3), cx));
    seen(&fixture);
    edit_with(&fixture, cx, "7");
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture),
        [
            Seen::Start(3, Price, EditOrigin::User),
            Seen::Cancel(
                3,
                Price,
                EditCancelReason::Escape,
                Some(OwnedValue::Float(7.))
            ),
        ]
    );
    // The grid has the focus back: the next Escape clears the selection.
    assert_eq!(fixture.selected_keys(cx), [3]);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    assert!(fixture.selected_keys(cx).is_empty());
}

/// The standard policy, but a commit equal to its start still goes out.
struct CommitUnchanged;

impl EditPolicy for CommitUnchanged {
    fn on_blur(&self, context: &EditPolicyContext<'_>) -> EditEnd {
        gpui_kit_grid::StandardEditPolicy.on_blur(context)
    }

    fn commit_unchanged(&self) -> bool {
        true
    }

    fn on_value_change(&self, _: &EditPolicyContext<'_>) -> DraftAction {
        DraftAction::Keep
    }

    fn on_hidden(&self, _: &EditPolicyContext<'_>) -> EditEnd {
        EditEnd::Cancel
    }
}

#[gpui_kit::test]
fn test_unchanged_commit(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    seen(&fixture);
    fixture.press_key(cx, "f2");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    // The editor closes as unchanged; Enter still moves down.
    let edits: Vec<Seen> = seen(&fixture)
        .into_iter()
        .filter(|event| matches!(event, Seen::Start(..) | Seen::Commit(..) | Seen::Cancel(..)))
        .collect();
    assert_eq!(
        edits,
        [
            Seen::Start(3, Price, EditOrigin::User),
            Seen::Cancel(3, Price, EditCancelReason::Unchanged, None),
        ]
    );
    assert_eq!(fixture.focused(cx), Some((4, Price)));

    let fixture = open_editable(cx, |grid| grid.with_edit_policy(CommitUnchanged));
    fixture.focus_cell(cx, 3, Price);
    seen(&fixture);
    fixture.press_key(cx, "f2");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert!(
        seen(&fixture).contains(&Seen::Commit(3, Price, OwnedValue::Float(price(3)))),
        "the policy commits an unchanged value"
    );
}

#[gpui_kit::test]
fn test_number_editor_parse(cx: &mut TestAppContext) {
    use OrderColumn::{Price, Quantity};
    let fixture = open_editable(cx, |grid| {
        grid.with_editor(Quantity, NumberEditor::new().with_min(OwnedValue::Int(0)))
    });
    fixture.focus_cell(cx, 3, Quantity);
    seen(&fixture);
    let message = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.edit_message()
                .map(|(mark, message)| (mark, message.to_string()))
        })
    };
    // A fraction in a whole-number column: refused, the field stays open
    // with the catalog's message.
    edit_with(&fixture, cx, "1.5");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Quantity)));
    assert_eq!(
        message(cx),
        Some((EditorMark::Invalid, "Enter a whole number".to_string()))
    );
    // Below the minimum.
    fixture.press_key(cx, "secondary-a");
    fixture.type_text(cx, "-1");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(
        message(cx),
        Some((EditorMark::Invalid, "Enter 0 or more".to_string()))
    );
    // Letters do not get in; a whole number commits as `Int`.
    fixture.press_key(cx, "secondary-a");
    fixture.press_key(cx, "backspace");
    fixture.type_text(cx, "abc");
    fixture.settle(cx);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some(""));
    fixture.type_text(cx, "15");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
    let events = seen(&fixture);
    assert!(events.contains(&Seen::Commit(3, Quantity, OwnedValue::Int(15))));

    // A price commits as `Float`, its sign kept.
    fixture.focus_cell(cx, 3, Price);
    edit_with(&fixture, cx, "-0.5");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert!(seen(&fixture).contains(&Seen::Commit(3, Price, OwnedValue::Float(-0.5))));
}

#[gpui_kit::test]
fn test_validate_edit_hook(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    // The hooks of the fixture refuse a price above 200 when told.
    let hooks = EditHooks::default();
    hooks.max_price.set(Some(200.));
    let fixture = open_orders_with(cx, orders(60), hooks, with_order_editors);
    fixture.focus_cell(cx, 3, Price);
    seen(&fixture);
    edit_with(&fixture, cx, "250");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    let message = fixture.grid.read_with(cx, |grid, _| {
        grid.edit_message()
            .map(|(mark, message)| (mark, message.to_string()))
    });
    assert_eq!(
        message,
        Some((EditorMark::Invalid, "Enter a price up to 200".to_string()))
    );
    fixture.press_key(cx, "secondary-a");
    fixture.type_text(cx, "150");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert!(seen(&fixture).contains(&Seen::Commit(3, Price, OwnedValue::Float(150.))));
}

#[gpui_kit::test]
fn test_press_outside_commits(cx: &mut TestAppContext) {
    use OrderColumn::{Price, Quantity};
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    seen(&fixture);

    // A press on another cell: the commit first, then the press's events.
    edit_with(&fixture, cx, "99");
    seen(&fixture);
    fixture.press_cell(cx, 5, Quantity, 1);
    fixture.settle(cx);
    let events = seen(&fixture);
    assert_eq!(
        events.first(),
        Some(&Seen::Commit(3, Price, OwnedValue::Float(99.)))
    );
    assert!(events.contains(&Seen::Click), "{events:?}");
    assert_eq!(fixture.editing(cx), None);
    assert_eq!(fixture.focused(cx), Some((6, Quantity)));

    // A press on a header: the commit, then the sort.
    fixture.focus_cell(cx, 3, Price);
    edit_with(&fixture, cx, "98");
    seen(&fixture);
    cx.update_window(fixture.window, |_, window, cx| {
        let at = point(order_cell(0, Price).x, px(ROW / 2.));
        press_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
    })
    .expect("window is open");
    fixture.settle(cx);
    let events = seen(&fixture);
    assert_eq!(
        events.first(),
        Some(&Seen::Commit(3, Price, OwnedValue::Float(98.)))
    );
    assert!(events.contains(&Seen::Sort), "{events:?}");

    // An invalid draft and a press outside the grid: cancelled.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_sort([(OrderColumn::Id, SortDirection::Ascending)], cx)
    });
    fixture.settle(cx);
    fixture.focus_cell(cx, 3, Quantity);
    edit_with(&fixture, cx, "1.5");
    seen(&fixture);
    cx.update_window(fixture.window, |_, window, cx| window.click("away", cx))
        .expect("window is open");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Cancel(3, Quantity, EditCancelReason::Blur, None))
    );
    assert_eq!(fixture.editing(cx), None);
}

#[gpui_kit::test]
fn test_focus_out_commits(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let fixture = open_editable(cx, |grid| grid);
    // GPUI reports focus changes of an active window only.
    cx.update_window(fixture.window, |_, window, _| window.activate_window())
        .expect("window is open");
    cx.run_until_parked();
    fixture.focus_cell(cx, 3, Price);
    edit_with(&fixture, cx, "77");
    seen(&fixture);
    // Another window becomes active: the edit stays, as in Excel.
    let other = cx.update(|cx| {
        gpui_kit::open_window(Default::default(), cx, |_, cx| cx.new(|_| gpui_kit::Empty))
            .expect("open window")
    });
    cx.update_window(other.0, |_, window, _| window.activate_window())
        .expect("window is open");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    assert!(seen(&fixture).is_empty());
    cx.update_window(fixture.window, |_, window, _| window.activate_window())
        .expect("window is open");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    // The focus moves to the grid's root by a call: the field loses it.
    cx.update_window(fixture.window, |_, window, cx| {
        let handle = fixture.grid.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .expect("window is open");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture),
        [Seen::Commit(3, Price, OwnedValue::Float(77.))]
    );
    assert_eq!(fixture.editing(cx), None);
}

#[gpui_kit::test]
fn test_editor_keys_stay_in_editor(cx: &mut TestAppContext) {
    use OrderColumn::Note;
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Note);
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(3), cx));
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    let first = fixture.visible_keys(cx)[0];
    for key in [
        "pagedown",
        "secondary-a",
        "shift-f10",
        "left",
        "right",
        "home",
    ] {
        fixture.press_key(cx, key);
    }
    fixture.type_text(cx, "- +");
    fixture.settle(cx);
    // No scroll, no selection, no menu: the keys stayed in the field,
    // whose caret went home; `-`, ` ` and `+` are its text.
    assert_eq!(fixture.visible_keys(cx)[0], first);
    assert_eq!(fixture.selected_keys(cx), [3]);
    assert_eq!(fixture.editing(cx), Some((3, Note)));
    assert!(seen(&fixture).is_empty());
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("- +Order 3"));
}

#[gpui_kit::test]
fn test_editor_host(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    // The field of an `xsmall` grid is an `XSmall` input, in the cell.
    let input = fixture.editor_input(cx).expect("the field");
    let path = input.path();
    assert!(path.contains(&order_row_id(3)), "{path:?}");
    assert!(path.contains(&cell_host_id("live-grid-editor", Price)));
    assert_eq!(input.bounds().size.height, px(20.));
    let cell = order_cell(2, Price);
    assert!(input.bounds().origin.y >= cell.y - px(ROW / 2.));
    assert!(input.focused() == Some(true));

    // Scrolled away by the wheel: the field keeps the focus, and typing
    // brings its cell back.
    cx.update_window(fixture.window, |_, window, cx| {
        let wheel = ScrollWheelEvent {
            position: order_cell(5, OrderColumn::Id),
            delta: ScrollDelta::Pixels(point(px(0.), px(-ROW * 20.))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        };
        window.dispatch_event(wheel.to_platform_input(), cx);
    })
    .expect("window is open");
    fixture.settle(cx);
    assert!(!fixture.visible_keys(cx).contains(&3));
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    fixture.type_text(cx, "5");
    fixture.settle(cx);
    assert!(fixture.visible_keys(cx).contains(&3));
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("100.755"));
}

#[gpui_kit::test]
fn test_edit_api(cx: &mut TestAppContext) {
    use OrderColumn::{Price, Quantity, Side};
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    seen(&fixture);
    let start = |key: u32, column: OrderColumn, cx: &mut TestAppContext| {
        cx.update_window(fixture.window, |_, window, cx| {
            fixture
                .grid
                .update(cx, |grid, cx| grid.start_edit(&key, column, window, cx))
        })
        .expect("window is open")
    };
    assert!(start(4, Price, cx));
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((4, Price)));
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_editing()));
    let events = seen(&fixture);
    assert_eq!(
        events.last(),
        Some(&Seen::Start(4, Price, EditOrigin::Programmatic))
    );
    // A second start ends the first: unchanged, it cancels.
    fixture.type_text(cx, "1");
    fixture.settle(cx);
    assert!(start(5, Quantity, cx));
    fixture.settle(cx);
    let events = seen(&fixture);
    assert_eq!(
        events.first(),
        Some(&Seen::Commit(4, Price, OwnedValue::Float(1011.))),
        "{events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&Seen::Start(5, Quantity, EditOrigin::Programmatic))
    );
    // Commit and cancel by call.
    let committed = cx
        .update_window(fixture.window, |_, window, cx| {
            fixture
                .grid
                .update(cx, |grid, cx| grid.commit_edit(window, cx))
        })
        .expect("window is open");
    assert!(committed);
    assert_eq!(fixture.editing(cx), None);
    assert!(start(6, Price, cx));
    let cancelled = cx
        .update_window(fixture.window, |_, window, cx| {
            fixture
                .grid
                .update(cx, |grid, cx| grid.cancel_edit(window, cx))
        })
        .expect("window is open");
    assert!(cancelled);
    assert!(seen(&fixture).contains(&Seen::Cancel(
        6,
        Price,
        EditCancelReason::Programmatic,
        None
    )));
    // Refused: a column without an editor, a row not shown.
    assert!(!start(6, Side, cx));
    assert!(!start(1000, Price, cx));
    assert_eq!(fixture.editing(cx), None);
}

#[gpui_kit::test]
fn test_edit_row_removed(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let fixture = open_editable(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    edit_with(&fixture, cx, "5");
    seen(&fixture);
    fixture.writer(cx).remove(3);
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture)
            .into_iter()
            .filter(|event| matches!(event, Seen::Cancel(..)))
            .collect::<Vec<_>>(),
        [Seen::Cancel(
            3,
            Price,
            EditCancelReason::RowRemoved,
            Some(OwnedValue::Float(5.))
        )]
    );
    assert_eq!(fixture.editing(cx), None);
    let focused = cx
        .update_window(fixture.window, |_, window, cx| {
            fixture.grid.focus_handle(cx).is_focused(window)
        })
        .expect("window is open");
    assert!(focused, "the grid has the focus");
}

#[gpui_kit::test]
fn test_edit_a11y(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let fixture = open_editable(cx, |grid| grid);
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.focus_cell(cx, 3, Price);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    // The field is a text input named after its column, in a `GridCell`.
    let input = fixture.editor_input(cx).expect("the field");
    assert_eq!(input.label(), Some("price"));
    let host_role = cx
        .update_window(fixture.window, |_, window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|element| {
                    element.path().last() == Some(&cell_host_id("live-grid-editor", Price))
                })
                .and_then(|element| element.role())
        })
        .expect("window is open");
    assert_eq!(host_role, Some(Role::GridCell));
    // After the commit, the focused cell is the active descendant again.
    fixture.type_text(cx, "1");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let focus_element = cx
        .update_window(fixture.window, |_, window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .iter()
                .any(|element| {
                    element.path().last() == Some(&cell_host_id("live-grid-focus", Price))
                })
        })
        .expect("window is open");
    assert!(focus_element);
}

// The list, the calendar, the checkbox, commits from elements.

/// Presses the element a screen reader names `label`, such as an item of
/// the open list or a day of the open calendar.
fn press_label<D: LiveGridDelegate<Row = Order>>(
    fixture: &EditFixture<D>,
    cx: &mut TestAppContext,
    label: &str,
) {
    cx.update_window(fixture.window, |_, window, cx| {
        let bounds = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|element| element.label() == Some(label) && element.visible())
            .unwrap_or_else(|| panic!("{label} is shown"))
            .bounds();
        press_at(
            window,
            bounds.center(),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    })
    .expect("window is open");
}

/// Returns true if an element named `label` is shown.
fn shows_label<D: LiveGridDelegate<Row = Order>>(
    fixture: &EditFixture<D>,
    cx: &mut TestAppContext,
    label: &str,
) -> bool {
    cx.update_window(fixture.window, |_, window, _| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|element| element.label() == Some(label) && element.visible())
    })
    .expect("window is open")
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::Text(value.into())
}

fn sides() -> SelectEditor {
    SelectEditor::new([
        SelectOption::new(text("Buy"), "Buy"),
        SelectOption::new(text("Sell"), "Sell"),
    ])
}

#[gpui_kit::test]
fn test_select_editor(cx: &mut TestAppContext) {
    use OrderColumn::{Quantity, Side};
    let fixture = open_editable(cx, |grid| grid.with_editor(Side, sides()));
    // F2: the list opens at once with the cell's value picked.
    fixture.focus_cell(cx, 3, Side);
    seen(&fixture);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Side)));
    assert!(shows_label(&fixture, cx, "Buy") && shows_label(&fixture, cx, "Sell"));
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Start(3, Side, EditOrigin::User))
    );
    // Up picks Buy; Enter commits it once, and the grid confirms nothing.
    fixture.focus_cell(cx, 3, Side);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    fixture.press_key(cx, "up");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let events = seen(&fixture);
    let commits: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, Seen::Commit(..)))
        .collect();
    assert_eq!(commits, [&Seen::Commit(3, Side, text("Buy"))]);
    assert!(!events.contains(&Seen::Confirm), "{events:?}");
    assert_eq!(fixture.editing(cx), None);

    // Typing searches: "S" leaves Sell, which Enter commits.
    fixture.focus_cell(cx, 4, Side);
    seen(&fixture);
    fixture.type_text(cx, "S");
    fixture.settle(cx);
    assert!(!shows_label(&fixture, cx, "Buy"));
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert!(seen(&fixture).contains(&Seen::Commit(4, Side, text("Sell"))));

    // A click on an item commits it.
    fixture.focus_cell(cx, 5, Side);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    press_label(&fixture, cx, "Buy");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Commit(5, Side, text("Buy")))
    );

    // Escape cancels.
    fixture.focus_cell(cx, 7, Side);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    let events = seen(&fixture);
    assert_eq!(
        events.first(),
        Some(&Seen::Cancel(7, Side, EditCancelReason::Escape, None)),
        "{events:?}"
    );
    assert_eq!(fixture.editing(cx), None);

    // Tab commits the picked item and moves right.
    fixture.focus_cell(cx, 9, Side);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    fixture.press_key(cx, "up");
    fixture.press_key(cx, "tab");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Commit(9, Side, text("Buy")))
    );
    assert_eq!(fixture.focused(cx), Some((9, Quantity)));

    // Backspace takes no edit without an empty item.
    fixture.focus_cell(cx, 11, Side);
    fixture.press_key(cx, "backspace");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
}

#[gpui_kit::test]
fn test_select_empty_item(cx: &mut TestAppContext) {
    use OrderColumn::Side;
    let fixture = open_editable(cx, |grid| {
        grid.with_editor(Side, sides().with_empty("None"))
    });
    fixture.focus_cell(cx, 3, Side);
    seen(&fixture);
    // Backspace picks the empty item.
    fixture.press_key(cx, "backspace");
    fixture.settle(cx);
    assert!(shows_label(&fixture, cx, "None"));
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert!(seen(&fixture).contains(&Seen::Commit(3, Side, OwnedValue::Empty)));
}

fn at(text: &str) -> OwnedValue {
    OwnedValue::Timestamp(
        DateTime::parse_from_rfc3339(text)
            .expect("a time")
            .with_timezone(&Utc),
    )
}

#[gpui_kit::test]
fn test_date_editor(cx: &mut TestAppContext) {
    use OrderColumn::{Active, Placed};
    let fixture = open_editable(cx, |grid| grid.with_editor(Placed, DateEditor::date()));
    // F2: the calendar opens at once on the cell's day; a day picked
    // commits it at 00:00 UTC.
    fixture.focus_cell(cx, 3, Placed);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    assert!(shows_label(&fixture, cx, "2026-11-04"));
    press_label(&fixture, cx, "2026-11-06");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Commit(3, Placed, at("2026-11-06T00:00:00Z")))
    );
    assert_eq!(fixture.editing(cx), None);

    // Escape cancels.
    fixture.focus_cell(cx, 5, Placed);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Cancel(5, Placed, EditCancelReason::Escape, None))
    );

    // Tab with the day untouched: unchanged, the focus moves right.
    fixture.focus_cell(cx, 5, Placed);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    fixture.press_key(cx, "tab");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Cancel(5, Placed, EditCancelReason::Unchanged, None))
    );
    assert_eq!(fixture.focused(cx), Some((5, Active)));

    // Typing takes no edit.
    fixture.focus_cell(cx, 5, Placed);
    fixture.type_text(cx, "1");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
}

#[gpui_kit::test]
fn test_date_time_editor(cx: &mut TestAppContext) {
    use OrderColumn::Placed;
    let moscow = chrono::FixedOffset::east_opt(3 * 3600).expect("+03:00");
    let fixture = open_editable(cx, move |grid| {
        grid.with_editor(Placed, DateEditor::date_time().with_offset(moscow))
    });
    // Order 3 was placed 09:03 UTC, 12:03 in Moscow; a day picked keeps
    // the field open with the time; Enter commits both.
    fixture.focus_cell(cx, 3, Placed);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    seen(&fixture);
    press_label(&fixture, cx, "2026-11-06");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Placed)));
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture).first(),
        Some(&Seen::Commit(3, Placed, at("2026-11-06T09:03:00Z")))
    );
}

/// Checkboxes in a `Control` column of `Active`, committing through
/// [`CellEditHandle`]: a click and Space toggle. Orders in `filled` are not
/// editable; `refuse` makes `validate_edit` refuse every value.
#[derive(Clone, Default)]
struct Flags {
    filled: Rc<RefCell<Vec<u32>>>,
    refuse: Rc<Cell<bool>>,
    /// The handle each `render_cell` got, by order.
    handles: Rc<RefCell<Vec<(u32, Option<CellEditHandle>)>>>,
}

impl Flags {
    /// The handle the last frame gave the cell of `key`.
    fn handle(&self, key: u32) -> Option<CellEditHandle> {
        self.handles
            .borrow()
            .iter()
            .rev()
            .find(|(each, _)| *each == key)
            .and_then(|(_, handle)| handle.clone())
    }
}

impl LiveGridDelegate for Flags {
    type Row = Order;

    fn is_editable(&self, cell: &EditableContext<'_, Order>, _: &App) -> bool {
        !self.filled.borrow().contains(cell.key())
    }

    fn validate_edit(&self, _: &EditValidateContext<'_, Order>, _: &App) -> Result<(), EditError> {
        if self.refuse.get() {
            Err(EditError::new("Locked"))
        } else {
            Ok(())
        }
    }

    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Order>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        let edit = cell.edit();
        self.handles.borrow_mut().push((*cell.key(), edit.clone()));
        let checked = matches!(cell.value(), CellValue::Bool(true));
        if let Some(edit) = edit.clone() {
            cell.on_activate(move |window, cx| {
                edit.commit(OwnedValue::Bool(!checked), window, cx);
            });
        }
        Some(
            Checkbox::new("active")
                .checked(checked)
                .with_size(cell.size())
                .tab_stop(false)
                .accessibility_label("Active")
                .on_click(move |_, window, cx| {
                    if let Some(edit) = &edit {
                        edit.commit(OwnedValue::Bool(!checked), window, cx);
                    }
                })
                .into_any_element(),
        )
    }
}

/// The order schema with `Active` as a `Control` column.
fn control_schema() -> GridSchema<OrderColumn> {
    ORDER_COLUMNS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, width)| {
            let content = if column == OrderColumn::Active {
                CellContent::Control
            } else {
                CellContent::Text
            };
            schema.column(
                GridColumn::new(column, column.id())
                    .with_width(px(width))
                    .with_content(content),
            )
        })
}

/// The edit events among `events`: commits and cancels.
fn edits(events: Vec<LiveGridEvent<u32, OrderColumn>>) -> Vec<Seen> {
    events
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::EditStart {
                key,
                column,
                origin,
            } => Some(Seen::Start(key, column, origin)),
            LiveGridEvent::EditCommit {
                key, column, value, ..
            } => Some(Seen::Commit(key, column, value)),
            LiveGridEvent::EditCancel {
                key,
                column,
                reason,
                draft,
            } => Some(Seen::Cancel(key, column, reason, draft)),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn test_checkbox_editor(cx: &mut TestAppContext) {
    use OrderColumn::Active;
    // A `Text` column: F2 and a double click toggle, with no field.
    let fixture = open_editable(cx, |grid| grid.with_editor(Active, CheckboxEditor));
    fixture.focus_cell(cx, 3, Active);
    seen(&fixture);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    assert_eq!(
        seen(&fixture),
        [Seen::Commit(3, Active, OwnedValue::Bool(true))]
    );
    assert_eq!(fixture.editing(cx), None);
    fixture.press_cell(cx, 3, Active, 1);
    fixture.press_cell(cx, 3, Active, 2);
    fixture.settle(cx);
    let events = seen(&fixture);
    assert!(
        events.contains(&Seen::Commit(4, Active, OwnedValue::Bool(false))),
        "{events:?}"
    );
    assert!(!events.iter().any(|event| matches!(event, Seen::Start(..))));
    // Space selects, as on any `Text` cell.
    fixture.focus_cell(cx, 6, Active);
    seen(&fixture);
    fixture.press_key(cx, "space");
    fixture.settle(cx);
    assert!(
        !seen(&fixture)
            .iter()
            .any(|event| matches!(event, Seen::Commit(..)))
    );

    // A `Control` column: the checkbox toggles itself, Space included; a
    // double click on it is two toggles of the element, not three.
    let flags = Flags::default();
    let fixture = open_orders_in(cx, control_schema(), orders(60), flags, |grid| {
        with_order_editors(grid).with_editor(Active, CheckboxEditor)
    });
    fixture.focus_cell(cx, 3, Active);
    fixture.take_events();
    fixture.press_key(cx, "space");
    fixture.settle(cx);
    assert_eq!(
        edits(fixture.take_events()),
        [Seen::Commit(3, Active, OwnedValue::Bool(true))]
    );
    fixture.press_cell(cx, 4, Active, 1);
    fixture.settle(cx);
    fixture.press_cell(cx, 4, Active, 2);
    fixture.settle(cx);
    assert_eq!(
        edits(fixture.take_events()),
        [
            Seen::Commit(5, Active, OwnedValue::Bool(false)),
            Seen::Commit(5, Active, OwnedValue::Bool(true)),
        ]
    );
}

#[gpui_kit::test]
fn test_cell_edit_handle(cx: &mut TestAppContext) {
    use OrderColumn::Active;
    // No editor: no handle.
    let flags = Flags::default();
    let fixture = open_orders_in(cx, control_schema(), orders(60), flags.clone(), |grid| grid);
    assert!(
        flags
            .handles
            .borrow()
            .iter()
            .all(|(_, handle)| handle.is_none())
    );
    drop(fixture);

    let flags = Flags::default();
    let fixture = open_orders_in(cx, control_schema(), orders(60), flags.clone(), |grid| {
        grid.with_editor(Active, CheckboxEditor)
    });
    let commit = |key: u32, value: bool, cx: &mut TestAppContext| {
        let handle = flags.handle(key).expect("a handle");
        cx.update_window(fixture.window, |_, window, cx| {
            handle.commit(OwnedValue::Bool(value), window, cx)
        })
        .expect("window is open")
    };
    // Not editable at the commit: refused, no events.
    fixture.take_events();
    flags.filled.borrow_mut().push(3);
    assert!(!commit(3, true, cx));
    // A refused value: no events either.
    flags.refuse.set(true);
    assert!(!commit(4, false, cx));
    fixture.settle(cx);
    assert!(edits(fixture.take_events()).is_empty());
    // Taken: `EditCommit`, and the element shows the value.
    flags.refuse.set(false);
    assert!(commit(4, false, cx));
    fixture.settle(cx);
    assert_eq!(
        edits(fixture.take_events()),
        [Seen::Commit(4, Active, OwnedValue::Bool(false))]
    );
    // Unchanged: closes as an editor's commit does.
    assert!(!commit(4, false, cx));
    assert_eq!(
        edits(fixture.take_events()),
        [Seen::Cancel(4, Active, EditCancelReason::Unchanged, None)]
    );
}
