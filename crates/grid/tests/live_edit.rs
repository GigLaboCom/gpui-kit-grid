//! Headless UI tests of editing under live updates: the draft and the conflict,
//! the order held while a field is open, an edited row removed or hidden, the
//! optimistic value of a commit until its answer and the store settle it, a
//! refused commit's rollback and mark, several commits at once.

mod common;

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{ActiveTheme as _, Sizable as _, checkbox::Checkbox};
use gpui_kit::{
    AnyElement, App, AppContext as _, Hsla, IntoElement as _, Modifiers, MouseButton,
    TestAppContext, Window, point, px,
};
use gpui_kit_grid::{
    AfterCommit, CellContent, CellElementContext, CellStyle, CellStyleContext, CellValue,
    CheckboxEditor, DraftAction, EditCancelReason, EditCommitKey, EditEnd, EditError, EditGesture,
    EditGestureContext, EditId, EditPolicy, EditPolicyContext, EditState, EditorMark, ExpandTarget,
    FollowLiveValue, GridColumn, GridColumns as _, GridSchema, GroupBy, GroupPath, HoldReorder,
    LiveGridDelegate, LiveGridEvent, LiveGridState, ManualReorder, OwnedValue, ReorderContext,
    ReorderDecision, ReorderPolicy, SortDirection, StandardEditPolicy, ViewSpec,
};

use crate::common::{
    EditFixture, ORDER_COLUMNS, Order, OrderColumn, ROW, open_orders_in, open_orders_with,
    order_row_id, orders, press_at, unthrottled, with_order_editors,
};

/// The look of a price cell as the delegate's `cell_style` saw it: the
/// edit state, the text colour the grid chose, and the error mark.
type StyleSeen = (u32, Option<EditState>, Option<Hsla>, bool);

/// What the element of an `Active` cell was built with: the value and the
/// edit state.
type WidgetSeen = (u32, OwnedValue, Option<EditState>);

/// Records the style of price cells with edits, and builds the checkboxes
/// of `Active` cells, which commit through `CellEditHandle`, recording what
/// each saw.
#[derive(Clone, Default)]
struct Watch {
    styles: Rc<RefCell<Vec<StyleSeen>>>,
    widgets: Rc<RefCell<Vec<WidgetSeen>>>,
}

impl LiveGridDelegate for Watch {
    type Row = Order;

    fn cell_style(&self, cell: &CellStyleContext<'_, Order>, style: &mut CellStyle, _: &App) {
        if cell.column() == OrderColumn::Price && cell.edit_state().is_some() {
            self.styles.borrow_mut().push((
                cell.row().id,
                cell.edit_state().cloned(),
                style.foreground(),
                style.has_error_mark(),
            ));
        }
    }

    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Order>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        self.widgets.borrow_mut().push((
            *cell.key(),
            OwnedValue::from_cell(cell.value()),
            cell.edit_state().cloned(),
        ));
        let (edit, checked) = (cell.edit(), matches!(cell.value(), CellValue::Bool(true)));
        Some(
            Checkbox::new("active")
                .checked(checked)
                .with_size(cell.size())
                .tab_stop(false)
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
fn schema_with_control() -> GridSchema<OrderColumn> {
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

type Fixture = EditFixture<Watch>;

fn open(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<Watch>) -> LiveGridState<Watch> + 'static,
) -> (Fixture, Watch) {
    let watch = Watch::default();
    let fixture = open_orders_with(cx, orders(60), watch.clone(), move |grid| {
        configure(with_order_editors(grid))
    });
    (fixture, watch)
}

/// Order `key`'s price in [`orders`].
fn price(key: u32) -> f64 {
    100. + f64::from(key) / 4.
}

/// Writes order `key` with `price`.
fn write_price(fixture: &Fixture, cx: &mut TestAppContext, key: u32, new_price: f64) {
    let mut order = orders(60)
        .into_iter()
        .find(|order| order.id == key)
        .expect("an order");
    order.price = new_price;
    fixture.writer(cx).upsert(Arc::new(order));
    fixture.settle(cx);
}

/// Edits the focused cell with `text`, starting from an empty field.
fn type_draft(fixture: &Fixture, cx: &mut TestAppContext, text: &str) {
    fixture.press_key(cx, "backspace");
    fixture.settle(cx);
    fixture.type_text(cx, text);
    fixture.settle(cx);
}

/// The commits among `events`: id, key, column, value, base, current.
fn commits(
    events: &[LiveGridEvent<u32, OrderColumn>],
) -> Vec<(EditId, u32, OrderColumn, OwnedValue, OwnedValue, OwnedValue)> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::EditCommit {
                edit,
                key,
                column,
                value,
                base,
                current,
            } => Some((
                *edit,
                *key,
                *column,
                value.clone(),
                base.clone(),
                current.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// The cancels among `events`: key, reason, draft.
fn cancels(
    events: &[LiveGridEvent<u32, OrderColumn>],
) -> Vec<(u32, EditCancelReason, Option<OwnedValue>)> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::EditCancel {
                key, reason, draft, ..
            } => Some((*key, *reason, draft.clone())),
            _ => None,
        })
        .collect()
}

fn message(fixture: &Fixture, cx: &mut TestAppContext) -> Option<(EditorMark, String)> {
    fixture.grid.read_with(cx, |grid, _| {
        grid.edit_message()
            .map(|(mark, message)| (mark, message.to_string()))
    })
}

fn resolve(
    fixture: &Fixture,
    cx: &mut TestAppContext,
    edit: EditId,
    result: Result<(), EditError>,
) -> bool {
    let resolved = fixture
        .grid
        .update(cx, |grid, cx| grid.resolve_edit(edit, result, cx));
    fixture.settle(cx);
    resolved
}

fn state(fixture: &Fixture, cx: &mut TestAppContext, key: u32) -> Option<EditState> {
    fixture.grid.read_with(cx, |grid, _| {
        grid.edit_state(&key, OrderColumn::Price).cloned()
    })
}

#[gpui_kit::test]
fn test_draft_survives_updates(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "101.5");
    fixture.take_events();

    // Another column of the row changes: nothing.
    let mut order = orders(60).remove(2);
    order.quantity = 999;
    fixture.writer(cx).upsert(Arc::new(order));
    fixture.settle(cx);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("101.5"));
    assert_eq!(message(&fixture, cx), None);

    // The price changes: the draft stays, the conflict shows.
    write_price(&fixture, cx, 3, 101.3);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("101.5"));
    assert_eq!(
        message(&fixture, cx),
        Some((
            EditorMark::Conflict,
            "Changed to 101.30 while you edited".to_string()
        ))
    );
    // Back at the starting value: no conflict.
    write_price(&fixture, cx, 3, price(3));
    assert_eq!(message(&fixture, cx), None);
    // Changed again and committed: the commit carries both values.
    write_price(&fixture, cx, 3, 101.3);
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let commits = commits(&fixture.take_events());
    assert_eq!(commits.len(), 1);
    let (_, key, column, value, base, current) = commits[0].clone();
    assert_eq!((key, column), (3, Price));
    assert_eq!(value, OwnedValue::Float(101.5));
    assert_eq!(base, OwnedValue::Float(price(3)));
    assert_eq!(current, OwnedValue::Float(101.3));
}

#[gpui_kit::test]
fn test_follow_live_value(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid.with_edit_policy(FollowLiveValue));
    fixture.focus_cell(cx, 3, Price);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    // Untouched: the draft follows the store.
    write_price(&fixture, cx, 3, 120.);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("120"));
    assert_eq!(message(&fixture, cx), None);
    // Touched: the draft stays, the conflict shows.
    fixture.type_text(cx, "5");
    fixture.settle(cx);
    write_price(&fixture, cx, 3, 121.);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("1205"));
    assert_eq!(
        message(&fixture, cx).map(|(mark, _)| mark),
        Some(EditorMark::Conflict)
    );
    fixture.take_events();
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let commits = commits(&fixture.take_events());
    assert_eq!(
        commits[0].4,
        OwnedValue::Float(120.),
        "the base followed the store"
    );
}

/// Cancels the edit when the store changes its cell, and commits a hidden
/// row's draft.
struct CancelOnChange;

impl EditPolicy for CancelOnChange {
    fn on_blur(&self, context: &EditPolicyContext<'_>) -> EditEnd {
        StandardEditPolicy.on_blur(context)
    }

    fn on_value_change(&self, _: &EditPolicyContext<'_>) -> DraftAction {
        DraftAction::Cancel
    }

    fn on_hidden(&self, _: &EditPolicyContext<'_>) -> EditEnd {
        EditEnd::Commit
    }
}

#[gpui_kit::test]
fn test_conflict_policy_cancel(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid.with_edit_policy(CancelOnChange));
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "7");
    fixture.take_events();
    write_price(&fixture, cx, 3, 120.);
    assert_eq!(fixture.editing(cx), None);
    assert_eq!(
        cancels(&fixture.take_events()),
        [(
            3,
            EditCancelReason::Programmatic,
            Some(OwnedValue::Float(7.))
        )]
    );
}

fn held(events: &[LiveGridEvent<u32, OrderColumn>]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::OrderHeldChange { held } => Some(*held),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn test_order_held_while_editing(cx: &mut TestAppContext) {
    use OrderColumn::{Note, Price};
    let (fixture, _) = open(cx, |grid| {
        grid.with_tuning(unthrottled())
            .with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
    });
    // The note of the third row is edited; the stream lifts the last row
    // to the top.
    let key = fixture.visible_keys(cx)[2];
    fixture.focus_cell(cx, key, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    fixture.take_events();
    write_price(&fixture, cx, 1, 1000.);
    assert_eq!(
        line_of(&fixture, cx, key),
        Some(2),
        "the row stays under the field"
    );
    assert_eq!(held(&fixture.take_events()), [true]);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    // The commit closes the field: the held order shows.
    fixture.type_text(cx, "x");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let events = fixture.take_events();
    assert_eq!(held(&events), [false]);
    assert_eq!(line_of(&fixture, cx, 1), Some(0));
    assert_eq!(line_of(&fixture, cx, key), Some(3));

    // `ManualReorder` holds after the field closes too, until applied.
    let (fixture, _) = open(cx, |grid| {
        grid.with_tuning(unthrottled())
            .with_reorder_policy(ManualReorder)
            .with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
    });
    let key = fixture.visible_keys(cx)[2];
    fixture.focus_cell(cx, key, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    write_price(&fixture, cx, 1, 1000.);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    assert_eq!(line_of(&fixture, cx, 1), Some(59));
    fixture
        .grid
        .update(cx, |grid, cx| grid.apply_held_order(cx));
    fixture.settle(cx);
    assert_eq!(line_of(&fixture, cx, 1), Some(0));
}

/// The line of the row of `key` in the grid's order.
fn line_of(fixture: &Fixture, cx: &mut TestAppContext, key: u32) -> Option<usize> {
    fixture.grid.read_with(cx, |grid, _| {
        grid.projection().display_ix(grid.snapshot().row_id(&key)?)
    })
}

#[gpui_kit::test]
fn test_user_action_while_editing(cx: &mut TestAppContext) {
    use OrderColumn::{Note, Price};
    let (fixture, _) = open(cx, |grid| grid.with_tuning(unthrottled()));
    fixture.focus_cell(cx, 3, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    // A call that keeps the row shown applies at once; the field lives.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("Order", cx));
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Note)));
    fixture.grid.update(cx, |grid, cx| {
        grid.set_sort([(Price, SortDirection::Descending)], cx)
    });
    fixture.settle(cx);
    assert_eq!(line_of(&fixture, cx, 60), Some(0), "the new order applies");
    assert_eq!(fixture.editing(cx), Some((3, Note)));
}

#[gpui_kit::test]
fn test_edited_row_removed(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid);
    // A commit of order 4 waits; order 3 is being edited.
    fixture.focus_cell(cx, 4, Price);
    type_draft(&fixture, cx, "9");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let pending = commits(&fixture.take_events());
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.pending_edit_count()),
        1
    );
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "8");
    fixture.take_events();
    fixture.writer(cx).remove(3);
    fixture.writer(cx).remove(4);
    fixture.settle(cx);
    assert_eq!(
        cancels(&fixture.take_events()),
        [(3, EditCancelReason::RowRemoved, Some(OwnedValue::Float(8.)))]
    );
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.pending_edit_count()),
        0
    );
    assert!(!resolve(&fixture, cx, pending[0].0, Ok(())));
}

#[gpui_kit::test]
fn test_edited_row_hidden(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid.with_tuning(unthrottled()));
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "8");
    fixture.take_events();
    // A quick filter by a call leaves the row out.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("Order 59", cx));
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
    assert_eq!(
        cancels(&fixture.take_events()),
        [(3, EditCancelReason::RowHidden, Some(OwnedValue::Float(8.)))]
    );

    // A policy that commits a hidden row's draft.
    let (fixture, _) = open(cx, |grid| {
        grid.with_tuning(unthrottled())
            .with_edit_policy(CancelOnChange)
    });
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "8");
    fixture.take_events();
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("Order 59", cx));
    fixture.settle(cx);
    let events = fixture.take_events();
    assert_eq!(commits(&events).len(), 1);
    assert_eq!(commits(&events)[0].3, OwnedValue::Float(8.));

    // A collapsed group: the field lives through the grouping, not the
    // collapse.
    let (fixture, _) = open(cx, |grid| grid.with_tuning(unthrottled()));
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "8");
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by([GroupBy::column(OrderColumn::Instrument)], cx);
    });
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    fixture.take_events();
    let gazp = GroupPath::new([(
        OrderColumn::Instrument.column_id(),
        OwnedValue::from_cell(CellValue::Text("GAZP")),
    )]);
    fixture.grid.update(cx, |grid, cx| {
        assert!(grid.set_expanded(ExpandTarget::Group(gazp), false, cx));
    });
    fixture.settle(cx);
    assert_eq!(
        cancels(&fixture.take_events()),
        [(3, EditCancelReason::RowHidden, Some(OwnedValue::Float(8.)))]
    );
}

#[gpui_kit::test]
fn test_optimistic_value(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, watch) = open(cx, |grid| grid);
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    let muted = cx.update(|cx| cx.theme().muted_foreground);
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "150");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let edit = commits(&fixture.take_events())[0].0;
    // The cell shows the new value, muted; the store keeps the old one.
    assert_eq!(state(&fixture, cx, 3), Some(EditState::Pending));
    let seen = watch.styles.borrow().last().cloned().expect("a style call");
    assert_eq!(seen, (3, Some(EditState::Pending), Some(muted), false));
    fixture.grid.read_with(cx, |grid, _| {
        let row = grid.snapshot().get(&3).expect("a row");
        assert_eq!(row.price, price(3));
    });
    // Back on the cell, a screen reader hears it saving.
    fixture.focus_cell(cx, 3, Price);
    let description = fixture.grid.read_with(cx, |grid, _| {
        grid.a11y_focus_description().map(str::to_owned)
    });
    assert_eq!(description.as_deref(), Some("price. saving. Saving…"));

    // Accepted: the usual colour; the store's own value settles it.
    assert!(resolve(&fixture, cx, edit, Ok(())));
    assert_eq!(state(&fixture, cx, 3), Some(EditState::Accepted));
    let seen = watch.styles.borrow().last().cloned().expect("a style call");
    assert_eq!(seen, (3, Some(EditState::Accepted), None, false));
    write_price(&fixture, cx, 3, 150.);
    assert_eq!(state(&fixture, cx, 3), None);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.pending_edit_count()),
        0
    );
}

#[gpui_kit::test]
fn test_settle_rules(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid);
    let commit = |key: u32, text: &str, cx: &mut TestAppContext| {
        fixture.focus_cell(cx, key, Price);
        type_draft(&fixture, cx, text);
        fixture.press_key(cx, "enter");
        fixture.settle(cx);
        commits(&fixture.take_events())[0].0
    };
    // The store says its own value after the answer: it settles.
    let edit = commit(3, "150.123", cx);
    assert!(resolve(&fixture, cx, edit, Ok(())));
    write_price(&fixture, cx, 3, 150.12);
    assert_eq!(state(&fixture, cx, 3), None);
    // No word from the store: the accepted value shows 5 s by its clock.
    let edit = commit(4, "160", cx);
    assert!(resolve(&fixture, cx, edit, Ok(())));
    fixture.frame(cx, Duration::from_secs(4));
    assert_eq!(state(&fixture, cx, 4), Some(EditState::Accepted));
    fixture.frame(cx, Duration::from_millis(1100));
    fixture.settle(cx);
    assert_eq!(state(&fixture, cx, 4), None);
}

#[gpui_kit::test]
fn test_rollback(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, watch) = open(cx, |grid| grid);
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "150");
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    let edit = commits(&fixture.take_events())[0].0;
    let error = EditError::new("Price out of band");
    assert!(resolve(&fixture, cx, edit, Err(error.clone())));
    // The store's value again, with the mark.
    assert_eq!(
        state(&fixture, cx, 3),
        Some(EditState::Failed(error.clone()))
    );
    let seen = watch.styles.borrow().last().cloned().expect("a style call");
    assert_eq!(seen, (3, Some(EditState::Failed(error)), None, true));
    fixture.focus_cell(cx, 3, Price);
    let description = fixture.grid.read_with(cx, |grid, _| {
        grid.a11y_focus_description().map(str::to_owned)
    });
    assert_eq!(
        description.as_deref(),
        Some("price. not saved. Price out of band. Not saved: Price out of band")
    );
    // F2 on the cell takes the mark off; so does `clear_edit_error`.
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    assert_eq!(state(&fixture, cx, 3), None);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    assert!(
        !fixture
            .grid
            .update(cx, |grid, cx| grid.clear_edit_error(&3, Price, cx))
    );
}

#[gpui_kit::test]
fn test_several_pending(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid);
    let commit = |key: u32, text: &str, cx: &mut TestAppContext| {
        fixture.focus_cell(cx, key, Price);
        type_draft(&fixture, cx, text);
        fixture.press_key(cx, "enter");
        fixture.settle(cx);
        commits(&fixture.take_events())[0].clone()
    };
    let other = commit(5, "500", cx);
    let older = commit(3, "300", cx);
    // The second commit of the cell starts from the first one's value.
    let newer = commit(3, "301", cx);
    assert_eq!(newer.4, OwnedValue::Float(300.));
    let count = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.pending_edit_count())
    };
    assert_eq!(count(cx), 3);
    // Answers in reverse order; the older refused while the newer waits.
    assert!(resolve(&fixture, cx, newer.0, Ok(())));
    assert!(resolve(&fixture, cx, older.0, Err(EditError::new("Stale"))));
    assert_eq!(state(&fixture, cx, 3), Some(EditState::Accepted));
    assert_eq!(count(cx), 2);
    assert!(resolve(&fixture, cx, other.0, Ok(())));
    assert!(!resolve(&fixture, cx, other.0, Ok(())), "answered already");
    assert_eq!(count(cx), 2);
}

/// Edits on a single click (AG Grid's `singleClickEdit`); otherwise Excel's.
struct SingleClick;

impl EditGesture for SingleClick {
    fn starts_edit(&self, _: &EditGestureContext) -> bool {
        true
    }

    fn after_commit(&self, _: EditCommitKey) -> AfterCommit {
        AfterCommit::Stay
    }
}

#[gpui_kit::test]
fn test_single_click_edit(cx: &mut TestAppContext) {
    use OrderColumn::{Price, Side};
    let (fixture, _) = open(cx, |grid| grid.with_edit_gesture(SingleClick));
    fixture.press_cell(cx, 2, Price, 1);
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    // A cell without an editor stays a click.
    fixture.press_cell(cx, 2, Side, 1);
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);
}

#[gpui_kit::test]
fn test_edit_a11y_real_cell(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid);
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.focus_cell(cx, 3, Price);
    let synthetic = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.a11y_row_cells(&order_row_id(3))
                .into_iter()
                .filter(|(_, description)| description.as_deref() == Some("price"))
                .count()
        })
    };
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    // The editor's host is the price cell's node: no synthetic twin.
    assert_eq!(synthetic(cx), 0);
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    fixture.focus_cell(cx, 4, Price);
    assert_eq!(synthetic(cx), 1, "a synthetic price cell again");
}

#[gpui_kit::test]
fn test_widget_optimistic(cx: &mut TestAppContext) {
    use OrderColumn::{Active, Price};
    let watch = Watch::default();
    let fixture = open_orders_in(
        cx,
        schema_with_control(),
        orders(60),
        watch.clone(),
        |grid| {
            with_order_editors(grid)
                .with_editor(Active, CheckboxEditor)
                .with_tuning(unthrottled())
                .with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
        },
    );
    // Order 60, on top, is not active; a click on its checkbox commits
    // through the element's handle, and the next frame's element shows the
    // value waiting.
    fixture.take_events();
    watch.widgets.take();
    fixture.press_cell(cx, 0, Active, 1);
    fixture.settle(cx);
    let events = fixture.take_events();
    assert_eq!(commits(&events)[0].3, OwnedValue::Bool(true));
    let seen = watch
        .widgets
        .borrow()
        .iter()
        .rfind(|seen| seen.0 == 60)
        .cloned();
    assert_eq!(
        seen,
        Some((60, OwnedValue::Bool(true), Some(EditState::Pending)))
    );
    // A commit with no field holds no order.
    write_price(&fixture, cx, 1, 1000.);
    assert_eq!(held(&fixture.take_events()), [] as [bool; 0]);
    assert_eq!(line_of(&fixture, cx, 1), Some(0));
}

#[gpui_kit::test]
fn test_reload_while_editing(cx: &mut TestAppContext) {
    use OrderColumn::Price;
    let (fixture, _) = open(cx, |grid| grid);
    fixture.focus_cell(cx, 3, Price);
    type_draft(&fixture, cx, "8");
    fixture.take_events();
    // The reload has the row with another price: a conflict.
    let mut rows = orders(60);
    rows[2].price = 101.3;
    fixture
        .writer(cx)
        .begin_reload()
        .finish(rows.into_iter().map(Arc::new));
    fixture.settle(cx);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("8"));
    assert_eq!(
        message(&fixture, cx).map(|(mark, _)| mark),
        Some(EditorMark::Conflict)
    );
    // The next one has no such row.
    fixture.writer(cx).begin_reload().finish(
        orders(60)
            .into_iter()
            .filter(|order| order.id != 3)
            .map(Arc::new),
    );
    fixture.settle(cx);
    assert_eq!(
        cancels(&fixture.take_events()),
        [(3, EditCancelReason::RowRemoved, Some(OwnedValue::Float(8.)))]
    );
}

#[gpui_kit::test]
fn test_header_click_while_editing(cx: &mut TestAppContext) {
    use OrderColumn::{Note, Price, Quantity};
    let (fixture, _) = open(cx, |grid| {
        grid.with_tuning(unthrottled())
            .with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
    });
    fixture.focus_cell(cx, 58, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    fixture.type_text(cx, "x");
    fixture.take_events();
    // A click on the quantity header: the field commits, then the sort
    // changes and its order shows at once.
    let x: f32 = ORDER_COLUMNS
        .iter()
        .take_while(|(column, _)| *column != Quantity)
        .map(|(_, width)| width)
        .sum::<f32>()
        + 35.;
    cx.update_window(fixture.window, |_, window, cx| {
        press_at(
            window,
            point(px(x), px(ROW / 2.)),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    })
    .expect("window is open");
    fixture.settle(cx);
    let events = fixture.take_events();
    let commit = events
        .iter()
        .position(|event| matches!(event, LiveGridEvent::EditCommit { .. }));
    let sort = events
        .iter()
        .position(|event| matches!(event, LiveGridEvent::SortChange { .. }));
    assert!(commit.is_some() && commit < sort, "{events:?}");
    assert_eq!(line_of(&fixture, cx, 1), Some(0));
}

/// Shows every new order at once, even under an open field.
struct ApplyAlways;

impl ReorderPolicy for ApplyAlways {
    fn decide(&self, _: &ReorderContext) -> ReorderDecision {
        ReorderDecision::Apply
    }
}

#[gpui_kit::test]
fn test_editor_follows_row(cx: &mut TestAppContext) {
    use OrderColumn::{Note, Price};
    let (fixture, _) = open(cx, |grid| {
        grid.with_tuning(unthrottled())
            .with_reorder_policy(ApplyAlways)
            .with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
    });
    let key = fixture.visible_keys(cx)[2];
    fixture.focus_cell(cx, key, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    let before = fixture.editor_input(cx).expect("a field").bounds();
    // The stream moves the edited row down, between orders 31 and 30.
    write_price(&fixture, cx, key, price(30) + 0.125);
    assert_eq!(line_of(&fixture, cx, key), Some(29));
    assert_eq!(fixture.editing(cx), Some((key, Note)));
    let field = fixture
        .editor_input(cx)
        .expect("the field moved with its row");
    assert_eq!(field.focused(), Some(true));
    assert_eq!(field.bounds(), before, "the row keeps its screen line");
}

#[gpui_kit::test]
fn test_hold_reorder_while_editing(cx: &mut TestAppContext) {
    use OrderColumn::{Note, Price};
    let (fixture, _) = open(cx, |grid| {
        grid.with_tuning(unthrottled())
            .with_reorder_policy(HoldReorder::new(Duration::from_millis(100)))
            .with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
    });
    fixture.focus_cell(cx, 58, Note);
    fixture.press_key(cx, "f2");
    fixture.settle(cx);
    write_price(&fixture, cx, 1, 1000.);
    // Past `max`: the field still holds the order.
    fixture.frame(cx, Duration::from_secs(1));
    fixture.settle(cx);
    assert_eq!(line_of(&fixture, cx, 1), Some(59));
    fixture.press_key(cx, "escape");
    fixture.settle(cx);
    assert_eq!(line_of(&fixture, cx, 1), Some(0));
}
