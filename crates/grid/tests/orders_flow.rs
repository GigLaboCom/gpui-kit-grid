//! The orders example's workflow end to end on 40 orders: what
//! `examples/orders` does, in a headless window, with the example's own model
//! (`examples/orders/model.rs`) and a "server" the test answers itself. Editing
//! under a stream, the hold, the optimistic value and its settling; a refused
//! limit, a conflict and a fill during an edit; the checkbox, the Cancel button
//! and the tag of Side; the CSV export of a waiting edit.

mod common;

#[allow(dead_code)]
#[path = "../examples/orders/model.rs"]
mod model;

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{Sizable as _, Size, button::Button, checkbox::Checkbox};
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement as _, Modifiers, MouseButton, ParentElement as _, Pixels,
    Point, Render, Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, point,
    px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellElementContext, CellValue, CheckboxEditor, CsvFormat, EditCancelReason, EditError, EditId,
    EditState, EditValidateContext, EditableContext, EditorMark, ExportRequest, ExportScope,
    GridWriter, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, LiveReorder,
    NumberEditor, OwnedValue, SelectEditor, SelectOption, SelectionMode, SortDirection,
    StoreOptions, ViewSpec,
};

use crate::common::{press_at, type_text, unthrottled};
use crate::model::{Order, OrderColumn, Status};

const ROW: f32 = 26.;
const WIDTH: f32 = 1500.;
const HEIGHT: f32 = ROW * 13.;

/// The example's hooks, as `examples/orders/main.rs` has them: closed
/// orders take no edit, quantities in lots of 100; the checkbox commits
/// through its handle, the Cancel button stops its press and records the
/// order it cancels.
#[derive(Clone, Default)]
struct Orders {
    cancelled: Rc<RefCell<Vec<u64>>>,
}

impl LiveGridDelegate for Orders {
    type Row = Order;

    fn is_editable(&self, cell: &EditableContext<'_, Order>, _: &App) -> bool {
        !cell.row().status.is_final()
    }

    fn validate_edit(
        &self,
        edit: &EditValidateContext<'_, Order>,
        _: &App,
    ) -> Result<(), EditError> {
        match model::check_edit(edit.column(), edit.value(), model::trading_day()) {
            Some(message) => Err(EditError::new(message)),
            None => Ok(()),
        }
    }

    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Order>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        let key = cell.row().id;
        Some(match cell.column() {
            OrderColumn::Active => {
                let checked = matches!(cell.value(), CellValue::Bool(true));
                let edit = cell.edit();
                if let Some(edit) = edit.clone() {
                    cell.on_activate(move |window, cx| {
                        edit.commit(OwnedValue::Bool(!checked), window, cx);
                    });
                }
                Checkbox::new("active")
                    .checked(checked)
                    .with_size(cell.size())
                    .tab_stop(false)
                    .on_click(move |_, window, cx| {
                        if let Some(edit) = &edit {
                            edit.commit(OwnedValue::Bool(!checked), window, cx);
                        }
                    })
                    .into_any_element()
            }
            OrderColumn::Cancel => {
                let cancelled = self.cancelled.clone();
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .xsmall()
                            .tab_stop(false)
                            .on_click(move |_, _, _| cancelled.borrow_mut().push(key)),
                    )
                    .into_any_element()
            }
            OrderColumn::Side | OrderColumn::Filled => {
                div().child(cell.text().to_owned()).into_any_element()
            }
            _ => return None,
        })
    }
}

struct Root {
    grid: Entity<LiveGridState<Orders>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui_kit::IntoElement {
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .with_size(Size::XSmall)
                .bordered(false),
        )
    }
}

type Events = Rc<RefCell<Vec<LiveGridEvent<u64, OrderColumn>>>>;

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Orders>>,
    hooks: Orders,
    events: Events,
}

/// 40 orders of the example's model (seed 7), with every editor of the
/// example, sorted by `sort`.
fn open(cx: &mut TestAppContext, sort: Option<(OrderColumn, SortDirection)>) -> Fixture {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let hooks = Orders::default();
    let delegate = hooks.clone();
    let options = |labels: &[&'static str]| {
        labels
            .iter()
            .map(|label| SelectOption::new(OwnedValue::Text((*label).into()), *label))
            .collect::<Vec<_>>()
    };
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(WIDTH), px(HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            LiveGridState::new_standalone(
                                model::schema(),
                                delegate,
                                StoreOptions::default(),
                                cx,
                            )
                            .with_selection_mode(SelectionMode::RowSet)
                            .with_tuning(unthrottled())
                            .with_reorder_policy(LiveReorder)
                            .with_view(ViewSpec::new().with_sort(sort))
                            .with_editor(
                                OrderColumn::Side,
                                SelectEditor::new(options(&model::SIDES)),
                            )
                            .with_editor(
                                OrderColumn::Quantity,
                                NumberEditor::new().with_min(OwnedValue::Int(1)),
                            )
                            .with_editor(OrderColumn::Limit, NumberEditor::new().with_scale(2))
                            .with_editor(OrderColumn::Active, CheckboxEditor)
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events: Events = Rc::default();
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<_, _>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let fixture = Fixture {
        window,
        grid,
        hooks,
        events,
    };
    let rows = model::orders(40, 7)
        .into_iter()
        .map(|order| Order {
            // Every order working, so every one takes an edit.
            status: Status::Working,
            filled: 0.,
            ..order
        })
        .map(Arc::new);
    fixture.writer(cx).replace_all(rows);
    fixture.settle(cx);
    cx.update_window(window, |_, window, _| window.activate_window())
        .expect("window is open");
    fixture.settle(cx);
    fixture
}

impl Fixture {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
        }
    }

    fn writer(&self, cx: &mut TestAppContext) -> GridWriter<Order> {
        self.grid.read_with(cx, |grid, _| grid.writer())
    }

    fn order(&self, cx: &mut TestAppContext, key: u64) -> Order {
        self.grid.read_with(cx, |grid, _| {
            (**grid.snapshot().get(&key).expect("an order")).clone()
        })
    }

    /// Writes `order` changed by `change`, as the stream or the server.
    fn write(&self, cx: &mut TestAppContext, key: u64, change: impl FnOnce(&mut Order)) {
        let mut order = self.order(cx, key);
        change(&mut order);
        self.writer(cx).upsert(order);
        self.settle(cx);
    }

    fn take_events(&self) -> Vec<LiveGridEvent<u64, OrderColumn>> {
        self.events.take()
    }

    fn focus_cell(&self, cx: &mut TestAppContext, key: u64, column: OrderColumn) {
        cx.update_window(self.window, |_, window, cx| {
            let handle = self.grid.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .expect("window is open");
        self.grid.update(cx, |grid, cx| {
            assert!(
                grid.set_focused_cell(&key, Some(column), cx),
                "{key} is shown"
            );
        });
        self.settle(cx);
    }

    fn key(&self, cx: &mut TestAppContext, keystroke: &str) {
        cx.update_window(self.window, |_, window, cx| window.press(keystroke, cx))
            .expect("window is open");
        self.settle(cx);
    }

    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        cx.update_window(self.window, |_, window, cx| type_text(window, text, cx))
            .expect("window is open");
        self.settle(cx);
    }

    /// The window position of a cell: `line` counts the shown rows.
    fn cell(&self, line: usize, column: OrderColumn) -> Point<Pixels> {
        // Cancel is frozen at the right edge.
        if column == OrderColumn::Cancel {
            let width = model::COLUMN_WIDTHS[14].2;
            return point(px(WIDTH - width / 2.), px(ROW * (line as f32 + 1.5)));
        }
        let mut x = 0.;
        for (each, _, width) in model::COLUMN_WIDTHS {
            if each == column {
                return point(px(x + width / 2.), px(ROW * (line as f32 + 1.5)));
            }
            x += width;
        }
        unreachable!("every column is in the schema")
    }

    fn press(&self, cx: &mut TestAppContext, at: Point<Pixels>, click_count: usize) {
        cx.update_window(self.window, |_, window, cx| {
            press_at(
                window,
                at,
                MouseButton::Left,
                Modifiers::default(),
                click_count,
                cx,
            )
        })
        .expect("window is open");
        self.settle(cx);
    }

    fn line_of(&self, cx: &mut TestAppContext, key: u64) -> Option<usize> {
        self.grid.read_with(cx, |grid, _| {
            grid.projection().display_ix(grid.snapshot().row_id(&key)?)
        })
    }

    fn edit_state(
        &self,
        cx: &mut TestAppContext,
        key: u64,
        column: OrderColumn,
    ) -> Option<EditState> {
        self.grid
            .read_with(cx, |grid, _| grid.edit_state(&key, column).cloned())
    }

    /// The server's answer to `edit`: the store first, then `resolve_edit`.
    fn answer(&self, cx: &mut TestAppContext, edit: EditId, result: Result<(), &str>) {
        self.grid.update(cx, |grid, cx| {
            grid.resolve_edit(edit, result.map_err(EditError::new), cx);
        });
        self.settle(cx);
    }
}

/// The commits among `events`: id, key, column, value, base, current.
fn commits(
    events: &[LiveGridEvent<u64, OrderColumn>],
) -> Vec<(EditId, u64, OrderColumn, OwnedValue, OwnedValue, OwnedValue)> {
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

#[gpui_kit::test]
fn test_orders_edit_flow(cx: &mut TestAppContext) {
    use OrderColumn::{Last, Limit};
    let fixture = open(cx, Some((Last, SortDirection::Descending)));
    let key = fixture.grid.read_with(cx, |grid, _| grid.visible_keys()[2]);
    let limit = fixture.order(cx, key).last.round();
    // Typing on the focused limit opens the field; the stream moves the
    // row's Last to the top meanwhile: the order is held under the field.
    fixture.focus_cell(cx, key, Limit);
    fixture.type_text(cx, &format!("{limit}"));
    fixture.take_events();
    fixture.write(cx, key, |order| order.last *= 3.);
    assert_eq!(fixture.line_of(cx, key), Some(2), "held under the field");
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    // Enter: the commit, the new order, the value waiting.
    fixture.key(cx, "enter");
    let commit = commits(&fixture.take_events()).pop().expect("a commit");
    assert_eq!(commit.3, OwnedValue::Float(limit));
    assert_eq!(
        fixture.line_of(cx, key),
        Some(0),
        "the order shows after the field"
    );
    assert_eq!(fixture.edit_state(cx, key, Limit), Some(EditState::Pending));
    // The server writes the store, rounded to the step, then answers.
    fixture.write(cx, key, |order| {
        order.limit = model::round_to(limit, model::STEP)
    });
    fixture.answer(cx, commit.0, Ok(()));
    assert_eq!(
        fixture.edit_state(cx, key, Limit),
        None,
        "the store's value settled it"
    );
    assert_eq!(
        fixture.order(cx, key).limit,
        model::round_to(limit, model::STEP)
    );
}

#[gpui_kit::test]
fn test_orders_reject_and_conflict(cx: &mut TestAppContext) {
    use OrderColumn::Limit;
    let fixture = open(cx, None);
    // A limit out of the band: the server refuses, the store's value stays
    // with the error mark.
    fixture.focus_cell(cx, 3, Limit);
    fixture.type_text(cx, "999");
    fixture.key(cx, "enter");
    let commit = commits(&fixture.take_events()).pop().expect("a commit");
    let order = fixture.order(cx, 3);
    let refused = model::server_rule(&order, Limit, &commit.3);
    assert_eq!(refused, Err("Price out of band"));
    fixture.answer(cx, commit.0, refused.map(|_| ()));
    assert_eq!(
        fixture.edit_state(cx, 3, Limit),
        Some(EditState::Failed(EditError::new("Price out of band")))
    );
    assert_eq!(fixture.order(cx, 3).limit, order.limit);

    // Another trader moves the limit of an open draft: the conflict, then
    // a commit over the newer value.
    fixture.focus_cell(cx, 5, Limit);
    fixture.key(cx, "backspace");
    fixture.type_text(cx, "123.5");
    let base = fixture.order(cx, 5).limit;
    fixture.write(cx, 5, |order| order.limit += 0.1);
    let mark = fixture
        .grid
        .read_with(cx, |grid, _| grid.edit_message().map(|(mark, _)| mark));
    assert_eq!(mark, Some(EditorMark::Conflict));
    fixture.take_events();
    fixture.key(cx, "enter");
    let commit = commits(&fixture.take_events()).pop().expect("a commit");
    assert_eq!(commit.4, OwnedValue::Float(base));
    assert_ne!(commit.4, commit.5, "base != current");

    // The order fills and leaves during an edit: the edit closes.
    fixture.focus_cell(cx, 7, Limit);
    fixture.key(cx, "f2");
    fixture.take_events();
    fixture.writer(cx).remove(7);
    fixture.settle(cx);
    let cancelled = fixture.take_events().into_iter().any(|event| {
        matches!(
            event,
            LiveGridEvent::EditCancel {
                key: 7,
                reason: EditCancelReason::RowRemoved,
                ..
            }
        )
    });
    assert!(cancelled);
}

#[gpui_kit::test]
fn test_orders_widgets(cx: &mut TestAppContext) {
    use OrderColumn::{Active, Cancel, Side};
    let fixture = open(cx, None);
    // The checkbox: a click commits and selects the row.
    let active = fixture.order(cx, 3).active;
    fixture.press(cx, fixture.cell(2, Active), 1);
    let events = fixture.take_events();
    assert_eq!(
        commits(&events).pop().map(|commit| commit.3),
        Some(OwnedValue::Bool(!active))
    );
    let selected: Vec<u64> = fixture
        .grid
        .read_with(cx, |grid, _| grid.selection().iter().copied().collect());
    assert_eq!(selected, [3]);
    // Space on the focused checkbox commits too.
    fixture.focus_cell(cx, 4, Active);
    let active = fixture.order(cx, 4).active;
    fixture.key(cx, "space");
    assert_eq!(
        commits(&fixture.take_events()).pop().map(|commit| commit.3),
        Some(OwnedValue::Bool(!active))
    );
    // The Cancel button stops its press: no selection change.
    fixture.take_events();
    fixture.press(cx, fixture.cell(5, Cancel), 1);
    assert_eq!(fixture.hooks.cancelled.borrow().as_slice(), [6]);
    assert!(
        !fixture
            .take_events()
            .iter()
            .any(|event| matches!(event, LiveGridEvent::SelectionChange { .. }))
    );
    // A double click on Side opens its list.
    fixture.press(cx, fixture.cell(1, Side), 1);
    fixture.press(cx, fixture.cell(1, Side), 2);
    let editing = fixture.grid.read_with(cx, |grid, _| {
        grid.editing_cell().map(|(key, column)| (*key, column))
    });
    assert_eq!(editing, Some((2, Side)));
}

#[gpui_kit::test]
fn test_orders_export(cx: &mut TestAppContext) {
    use OrderColumn::Limit;
    let fixture = open(cx, None);
    // Backspace opens an empty field; the new limit, Enter.
    fixture.focus_cell(cx, 2, Limit);
    fixture.key(cx, "backspace");
    let limit = fixture.order(cx, 2).limit;
    let typed = format!("{:.2}", limit + 1.);
    fixture.type_text(cx, &typed);
    fixture.key(cx, "enter");
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.pending_edit_count()),
        1
    );
    // The export writes what the screen shows: the waiting value.
    let path = std::env::temp_dir().join(format!("orders-flow-{}.csv", std::process::id()));
    let job = fixture.grid.update(cx, |grid, cx| {
        grid.export_to_path(
            ExportRequest::new(ExportScope::Visible),
            CsvFormat::default(),
            path.clone(),
            cx,
        )
    });
    let job = job.expect("an export");
    cx.run_until_parked();
    assert!(job.is_finished(), "the job ran");
    let outcome = smol::block_on(job).expect("exported");
    assert_eq!(outcome.data_rows(), 40);
    let text = std::fs::read_to_string(&path).expect("the file");
    let _ = std::fs::remove_file(&path);
    let header = text
        .lines()
        .next()
        .expect("a header")
        .trim_start_matches('\u{feff}');
    assert!(header.starts_with("Id,Account,Instrument"), "{header}");
    let row = text
        .lines()
        .find(|line| line.starts_with("2,"))
        .expect("order 2");
    assert!(row.contains(&typed), "{row}");
    assert!(text.contains("\r\n"), "CRLF line ends");
    assert!(row.contains("Working"), "the status as text: {row}");
}
