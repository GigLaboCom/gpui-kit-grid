//! The toolbar of the blotter and the choices it offers: the reorder
//! policy, Refresh order, Reload and Cancel reload, the group panel and the
//! Groups menu, Columns…, the row size, the selection mode, the change
//! arrows, the theme and the quick filter.
//!
//! The toolbar reaches the grid through its `LiveGridHandle`: the reorder
//! policy, Refresh order, the Groups menu, Columns… and the filter need no
//! delegate type. What the handle does not offer
//! (the selection mode, the change paint) goes through `Root`.
//!
//! Every control is a gpui-kit component in one `Toolbar`: Tab goes through
//! them left to right and then into the grid, and the arrow keys move along
//! the bar. The filter is last, as the toolbar asks of text inputs (they
//! keep the arrow keys for the caret).

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, Size, Theme,
    ThemeMode,
    button::{Button, ButtonGroup, Toggle},
    input::Input,
    menu::DropdownMenu as _,
    select::Select,
    separator::Separator,
    toolbar::{Toolbar, ToolbarGroup},
};
use gpui_kit::{App, Context, IntoElement, Styled as _, div};
use gpui_kit_grid::{
    ExpandDepth, HoldReorder, LiveGridHandle, LiveReorder, ManualReorder, SelectionMode,
    actions::{CollapseAll, ExpandAll, ShowCaptionsOnly},
};

use crate::{CopyExpanded, KEY_CONTEXT, Reload, Root, e2e::RecordBounds as _};

/// When a new order replaces the shown one: the three built-in reorder
/// policies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reorder {
    Live,
    Hold,
    Manual,
}

impl Reorder {
    pub const ALL: [Self; 3] = [Self::Live, Self::Hold, Self::Manual];

    /// `BLOTTER_REORDER`: `live`, `hold` or `manual`.
    pub fn from_env(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reorder| reorder.label().eq_ignore_ascii_case(value))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Hold => "Hold",
            Self::Manual => "Manual",
        }
    }

    /// Gives the grid this policy: the one place a choice becomes a
    /// `ReorderPolicy`. A held order is offered to the new policy at once.
    /// Through the handle: any grid, whatever its delegate.
    pub fn apply(self, grid: &LiveGridHandle, cx: &mut App) {
        match self {
            Self::Live => grid.set_reorder_policy(LiveReorder, cx),
            Self::Hold => grid.set_reorder_policy(HoldReorder::default(), cx),
            Self::Manual => grid.set_reorder_policy(ManualReorder, cx),
        }
    }
}

/// The row sizes of `LiveGrid` (26, 30, 32 and 40 px rows).
pub const SIZES: [(&str, Size); 4] = [
    ("XSmall", Size::XSmall),
    ("Small", Size::Small),
    ("Medium", Size::Medium),
    ("Large", Size::Large),
];

/// What a click or a key press selects.
pub const SELECTION_MODES: [(&str, SelectionMode); 4] = [
    ("Row", SelectionMode::RowSingle),
    ("Rows", SelectionMode::RowSet),
    ("Cell", SelectionMode::CellSingle),
    ("Cells", SelectionMode::CellSet),
];

/// The labels of a choice table, for a `Select`.
pub fn labels<T>(choices: &[(&'static str, T)]) -> Vec<&'static str> {
    choices.iter().map(|(label, _)| *label).collect()
}

/// The position of `value` in a choice table.
pub fn position<T: PartialEq>(choices: &[(&'static str, T)], value: &T) -> usize {
    choices
        .iter()
        .position(|(_, choice)| choice == value)
        .unwrap_or_default()
}

/// The value of the choice labelled `label`.
pub fn choice<T: Copy>(choices: &[(&'static str, T)], label: &str) -> Option<T> {
    choices
        .iter()
        .find(|(choice, _)| *choice == label)
        .map(|(_, value)| *value)
}

/// Returns true if the grid shows no groups: Expand all, Collapse all and
/// Captions only would do nothing. The blotter shows no tree.
pub fn is_flat(grid: &LiveGridHandle, cx: &App) -> bool {
    grid.group_by(cx).is_empty()
}

impl Root {
    pub(crate) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let reloading = self.reload.is_some();
        let dark = cx.theme().is_dark();
        Toolbar::new("blotter-toolbar")
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                ToolbarGroup::new("order")
                    .label("Order")
                    .gap_1()
                    .child(
                        ButtonGroup::new("reorder-policy")
                            .outline()
                            .children(Reorder::ALL.map(|reorder| {
                                Button::new(reorder.label())
                                    .label(reorder.label())
                                    .selected(self.reorder == reorder)
                                    .e2e_bounds(reorder.label())
                            }))
                            .on_click(cx.listener(|root, clicked: &Vec<usize>, _, cx| {
                                if let Some(reorder) =
                                    clicked.first().and_then(|ix| Reorder::ALL.get(*ix))
                                {
                                    root.set_reorder(*reorder, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("refresh-order")
                            .label("Refresh order")
                            .disabled(!self.status.order_held)
                            .e2e_bounds("refresh-order")
                            .on_click(cx.listener(|root, _, _, cx| {
                                root.handle.apply_held_order(cx);
                            })),
                    ),
            )
            .content(Separator::vertical().h_5())
            .child(
                ToolbarGroup::new("data")
                    .label("Data")
                    .gap_1()
                    .child(
                        Button::new("reload")
                            .label("Reload")
                            .loading(reloading)
                            .e2e_bounds("reload")
                            .tooltip_with_action("Reload trades", &Reload, Some(KEY_CONTEXT))
                            .on_click(cx.listener(|root, _, _, cx| root.reload(cx))),
                    )
                    .child(
                        Button::new("cancel-reload")
                            .label("Cancel reload")
                            .disabled(!reloading)
                            .e2e_bounds("cancel-reload")
                            .on_click(cx.listener(|root, _, _, cx| root.cancel_reload(cx))),
                    ),
            )
            .content(Separator::vertical().h_5())
            .child(
                ToolbarGroup::new("groups")
                    .label("Groups")
                    .gap_1()
                    .child(
                        Toggle::new("group-panel")
                            .label("Group panel")
                            .checked(self.group_panel)
                            .e2e_bounds("group-panel")
                            .on_click(cx.listener(|root, shown: &bool, _, cx| {
                                root.set_group_panel(*shown, cx)
                            })),
                    )
                    .content(self.render_groups_menu(cx))
                    // Every column in the grid's chooser.
                    .child(
                        Button::new("columns")
                            .label("Columns…")
                            .tooltip("Show, hide and reorder the columns")
                            .e2e_bounds("columns")
                            .on_click(cx.listener(|root, _, window, cx| {
                                root.handle.show_column_chooser(window, cx);
                            })),
                    ),
            )
            .content(Separator::vertical().h_5())
            .child(
                ToolbarGroup::new("view")
                    .label("View")
                    .gap_1()
                    .child(
                        Select::new(&self.size_select)
                            .title_prefix("Size: ")
                            .accessibility_label("Row size")
                            .w_32(),
                    )
                    .child(
                        Select::new(&self.selection_select)
                            .title_prefix("Select: ")
                            .accessibility_label("Selection mode")
                            .w_32(),
                    )
                    .child(
                        Toggle::new("arrows")
                            .label("Arrows")
                            .checked(self.arrows)
                            .tooltip("Rises and falls with arrows")
                            .e2e_bounds("arrows")
                            .on_click(cx.listener(|root, arrows: &bool, _, cx| {
                                root.set_arrows(*arrows, cx)
                            })),
                    )
                    .child(
                        Toggle::new("dark-theme")
                            .label("Dark theme")
                            .checked(dark)
                            .on_click(|dark, window, cx| {
                                let mode = if *dark {
                                    ThemeMode::Dark
                                } else {
                                    ThemeMode::Light
                                };
                                Theme::change(mode, Some(window), cx);
                            }),
                    ),
            )
            .content(div().flex_1())
            .child(
                Input::new(&self.filter)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .aria_label("Filter trades")
                    .w_48(),
            )
    }

    /// Groups ▾: the commands over every group, sent to the grid as its
    /// Actions (they are the grid's menu items too), and Copy all expanded.
    /// The items read the grid when the menu opens. A menu button, not a
    /// `DropdownButton`: that is a split button whose other half runs a
    /// command, and Groups has none. Content of the toolbar, at its size.
    fn render_groups_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let grid = self.handle.clone();
        let focus = grid.focus_handle(cx);
        Button::new("groups-menu")
            .label("Groups")
            .small()
            .dropdown_caret(true)
            .e2e_bounds("groups-menu")
            .dropdown_menu(move |menu, _, cx| {
                // Expand and collapse do nothing on a flat view.
                let flat = is_flat(&grid, cx);
                let captions = grid.expand_depth(cx) == ExpandDepth::Captions;
                menu.action_context(focus.clone())
                    .menu_with_disabled("Expand all", Box::new(ExpandAll), flat)
                    .menu_with_disabled("Collapse all", Box::new(CollapseAll), flat)
                    .menu_with_check_and_disabled(
                        "Captions only",
                        captions,
                        Box::new(ShowCaptionsOnly),
                        flat,
                    )
                    .separator()
                    .menu("Copy all expanded", Box::new(CopyExpanded))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reorder_choices() {
        for reorder in Reorder::ALL {
            assert_eq!(Reorder::from_env(reorder.label()), Some(reorder));
            assert_eq!(
                Reorder::from_env(&reorder.label().to_lowercase()),
                Some(reorder)
            );
        }
        assert_eq!(Reorder::from_env("later"), None);
    }

    #[test]
    fn test_choice_tables() {
        assert_eq!(labels(&SIZES), ["XSmall", "Small", "Medium", "Large"]);
        assert_eq!(position(&SIZES, &Size::Medium), 2);
        assert_eq!(
            choice(&SELECTION_MODES, "Cells"),
            Some(SelectionMode::CellSet)
        );
        assert_eq!(choice(&SELECTION_MODES, "Columns"), None);
    }
}
