//! `SelectEditor`: one value of a closed set, picked from a searchable list
//! under the cell.
//!
//! The list is gpui-kit's public `List` in the editor's own popup, not its
//! `Select`: a `Select` has no public way to open, and its Enter opens the
//! list and goes on to the grid, which would commit before the pick.

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, Size as ComponentSize};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _,
    SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use rust_i18n::t;

use crate::{
    actions::CommitEdit,
    model::{CellValue, GridRow, OwnedValue, display_text},
    state::edit::{
        CellEditor, EditError, EditInput, EditorCell, EditorField, EditorOpen, EditorStart,
    },
};

/// One value a [`SelectEditor`] offers, with the text the list shows.
#[derive(Clone, Debug)]
pub struct SelectOption {
    value: OwnedValue,
    label: SharedString,
}

impl SelectOption {
    /// An option that commits `value`, shown as `label`.
    pub fn new(value: OwnedValue, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }

    /// The value it commits.
    pub fn value(&self) -> &OwnedValue {
        &self.value
    }

    /// The text the list shows.
    pub fn label(&self) -> &SharedString {
        &self.label
    }
}

/// Edits a cell with one value of a closed set, picked from a searchable
/// list under the cell.
///
/// The list opens at once with the cell's value picked. Typing searches,
/// ↑ and ↓ pick, Enter and a click commit the picked value, Tab commits it
/// and moves on, Escape cancels. Typing on the cell starts the search with
/// the typed text; Backspace and Delete pick the empty item
/// ([`Self::with_empty`]), or take no edit without one.
#[derive(Clone, Debug)]
pub struct SelectEditor {
    /// The empty item first, if any, then the options.
    items: Arc<[SelectOption]>,
    has_empty: bool,
}

impl SelectEditor {
    /// A list of `options`, in their order.
    pub fn new(options: impl IntoIterator<Item = SelectOption>) -> Self {
        Self {
            items: options.into_iter().collect(),
            has_empty: false,
        }
    }

    /// Adds an item on top that commits `Empty`, shown as `label`.
    pub fn with_empty(self, label: impl Into<SharedString>) -> Self {
        let empty = SelectOption::new(OwnedValue::Empty, label);
        let options = if self.has_empty {
            &self.items[1..]
        } else {
            &self.items[..]
        };
        Self {
            items: std::iter::once(empty)
                .chain(options.iter().cloned())
                .collect(),
            has_empty: true,
        }
    }

    /// The options, without the empty item.
    pub fn options(&self) -> &[SelectOption] {
        if self.has_empty {
            &self.items[1..]
        } else {
            &self.items
        }
    }

    /// The label of the empty item, if there is one.
    pub fn empty_label(&self) -> Option<&SharedString> {
        self.has_empty.then(|| self.items[0].label())
    }

    /// The item a start picks: the empty one for `Clear` (`None`, no edit,
    /// without it), the cell's value otherwise (`Some(None)` if it is none
    /// of the items).
    fn picked(&self, value: CellValue<'_>, input: &EditInput) -> Option<Option<usize>> {
        match input {
            EditInput::Clear => self.has_empty.then_some(Some(0)),
            _ => {
                let value = OwnedValue::from_cell(value);
                Some(item_of(&self.items, &value))
            }
        }
    }
}

/// The item of `items` with `value`.
fn item_of(items: &[SelectOption], value: &OwnedValue) -> Option<usize> {
    items
        .iter()
        .position(|item| item.value.cmp_total(value).is_eq())
}

impl<R: GridRow> CellEditor<R> for SelectEditor {
    fn open(&self, start: &EditorStart<'_, R>, window: &mut Window, cx: &mut App) -> EditorOpen {
        let Some(picked) = self.picked(start.value(), start.input()) else {
            return EditorOpen::Refuse;
        };
        let options = OptionList {
            matches: (0..self.items.len()).collect(),
            items: self.items.clone(),
        };
        let list = cx.new(|cx| ListState::new(options, window, cx).searchable(true));
        // Typed text filters the list now; the list's search field shows it
        // from the editor's second frame (`SelectField::render`), as that
        // field lays text out in the window's default font until its first
        // frame.
        let query = match start.input() {
            EditInput::Typing(text) => Some(text.clone()),
            _ => None,
        };
        list.update(cx, |list, cx| match start.input() {
            EditInput::Typing(text) => {
                list.delegate_mut().filter(text);
                let first = (!list.delegate().matches.is_empty()).then(|| IndexPath::new(0));
                list.set_selected_index(first, window, cx);
            }
            _ => {
                let row = picked.map(IndexPath::new);
                list.set_selected_index(row, window, cx);
                if let Some(row) = row {
                    list.scroll_to_item(row, gpui_kit::ScrollStrategy::Center, window, cx);
                }
            }
        });
        // A click on an item or Enter commits it, as the editor's Enter
        // does.
        let confirm = window.subscribe(&list, cx, |_, event: &ListEvent, window, cx| {
            if let ListEvent::Confirm(_) = event {
                window.dispatch_action(Box::new(CommitEdit), cx);
            }
        });
        EditorOpen::Field(Box::new(SelectField {
            list,
            start: RefCell::new(OwnedValue::from_cell(start.value())),
            initial: Cell::new(picked),
            query: RefCell::new(query),
            drawn: Cell::new(false),
            _confirm: confirm,
        }))
    }
}

/// The items of an open list, and those its search shows.
struct OptionList {
    items: Arc<[SelectOption]>,
    /// The shown items, by index into `items`.
    matches: Vec<usize>,
}

impl OptionList {
    /// Shows the items whose label contains `query`, ignoring case; every
    /// item for an empty query.
    fn filter(&mut self, query: &str) {
        let query = query.to_lowercase();
        self.matches = (0..self.items.len())
            .filter(|ix| query.is_empty() || self.items[*ix].label.to_lowercase().contains(&query))
            .collect();
    }
}

impl ListDelegate for OptionList {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.filter(query);
        Task::ready(())
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matches.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let item = &self.items[*self.matches.get(ix.row)?];
        Some(
            ListItem::new(ix.row)
                .accessibility_label(item.label.clone())
                .child(item.label.clone()),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(6.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(t!("live_grid.edit.no_matches")))
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
    }
}

/// An open [`SelectEditor`]: the picked item's text in the cell, the list
/// under it.
struct SelectField {
    list: Entity<ListState<OptionList>>,
    /// The cell's value: the draft while nothing is picked.
    start: RefCell<OwnedValue>,
    /// The item picked when it opened, or the store's value picked since.
    initial: Cell<Option<usize>>,
    /// The text typed to open it, for the search field once the field has
    /// drawn.
    query: RefCell<Option<SharedString>>,
    /// The field has drawn a frame: the list's search field is laid out.
    drawn: Cell<bool>,
    _confirm: Subscription,
}

impl SelectField {
    /// The picked item, by index into the items.
    fn picked(&self, cx: &App) -> Option<usize> {
        let list = self.list.read(cx);
        let row = list.selected_index()?.row;
        list.delegate().matches.get(row).copied()
    }

    /// The picked item's value and label.
    fn picked_item(&self, cx: &App) -> Option<SelectOption> {
        let ix = self.picked(cx)?;
        Some(self.list.read(cx).delegate().items[ix].clone())
    }
}

impl EditorField for SelectField {
    fn render(&self, cell: &EditorCell<'_>, window: &mut Window, cx: &mut App) -> AnyElement {
        if self.query.borrow().is_some() {
            if self.drawn.replace(true) {
                // The search finds what the filter found: the same first
                // match is picked.
                let query = self.query.take().unwrap_or_default();
                self.list
                    .update(cx, |list, cx| list.set_query(&query, window, cx));
            } else {
                // Its first frame: the next one shows the text.
                self.list.update(cx, |_, cx| cx.notify());
            }
        }
        let text = self.picked_item(cx).map_or_else(
            || {
                let mut text = String::new();
                display_text(self.start.borrow().as_cell(), &mut text).to_owned()
            },
            |item| item.label.to_string(),
        );
        let size = popup_size(cell.size());
        let width = cell.bounds().size.width.max(px(160.));
        div()
            .flex_1()
            .min_w_0()
            .px(px(4.))
            .truncate()
            .child(text)
            .child(
                cell.popup(
                    div().w(width).child(
                        List::new(&self.list)
                            .search_placeholder(SharedString::from(t!("live_grid.edit.search")))
                            .with_size(size)
                            .max_h(px(240.))
                            .p(px(4.)),
                    ),
                    cx,
                ),
            )
            .into_any_element()
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        self.list.update(cx, |list, cx| list.focus(window, cx));
    }

    fn value(&self, cx: &App) -> Result<OwnedValue, EditError> {
        Ok(self
            .picked_item(cx)
            .map_or_else(|| self.start.borrow().clone(), |item| item.value))
    }

    fn set_value(&self, value: &OwnedValue, window: &mut Window, cx: &mut App) {
        self.list.update(cx, |list, cx| {
            let row = list
                .delegate()
                .matches
                .iter()
                .position(|ix| list.delegate().items[*ix].value.cmp_total(value).is_eq());
            list.set_selected_index(row.map(IndexPath::new), window, cx);
            cx.notify();
        });
        // The store's value is the new starting point: untouched again.
        *self.start.borrow_mut() = value.clone();
        self.initial.set(self.picked(cx));
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.picked(cx) != self.initial.get()
    }
}

/// The size of the list's search field and items: as the cell's field,
/// but never smaller than `Small`, so the list reads.
fn popup_size(size: ComponentSize) -> ComponentSize {
    match size {
        ComponentSize::XSmall => ComponentSize::Small,
        size => size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sides() -> SelectEditor {
        SelectEditor::new([
            SelectOption::new(OwnedValue::Text("Buy".into()), "Buy"),
            SelectOption::new(OwnedValue::Text("Sell".into()), "Sell"),
        ])
    }

    #[test]
    fn test_select_picked() {
        let editor = sides();
        assert_eq!(editor.options().len(), 2);
        assert_eq!(editor.empty_label(), None);
        assert_eq!(
            editor.picked(CellValue::Text("Sell"), &EditInput::Command),
            Some(Some(1))
        );
        assert_eq!(
            editor.picked(CellValue::Text("Hold"), &EditInput::Command),
            Some(None)
        );
        // Backspace without an empty item takes no edit.
        assert_eq!(
            editor.picked(CellValue::Text("Sell"), &EditInput::Clear),
            None
        );

        let editor = sides().with_empty("None").with_empty("—");
        assert_eq!(editor.options().len(), 2, "one empty item");
        assert_eq!(editor.empty_label().map(AsRef::as_ref), Some("—"));
        assert_eq!(
            editor.picked(CellValue::Text("Sell"), &EditInput::Clear),
            Some(Some(0))
        );
        assert_eq!(
            editor.picked(CellValue::Empty, &EditInput::Command),
            Some(Some(0))
        );
        assert_eq!(
            editor.picked(CellValue::Text("Buy"), &EditInput::Press { click_count: 2 }),
            Some(Some(1))
        );
    }
}
