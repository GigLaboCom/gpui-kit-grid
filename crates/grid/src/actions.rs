//! Commands of the grid, as GPUI Actions in the `live_grid` namespace, and
//! its default key bindings.
//!
//! The grid handles them under the key context [`KEY_CONTEXT`], on the
//! focused grid, together with the shared ones of `gpui_kit::base::actions`
//! (`SelectUp`, `SelectPageDown`, `Confirm`, `Cancel` and the rest).
//! [`init`] binds the default keymap of [`key_bindings`]: Excel's, with Cmd
//! on macOS. An application rebinds a key in the same context after `init`
//! (the later binding wins), unbinds one with `NoAction`, or binds a
//! filtered [`key_bindings`] itself instead of calling `init`.
//!
//! Column commands act on [`LiveGridState::current_column`]: the header
//! cell or band pressed last, or the focused cell's column. With no current
//! column they do nothing. They have no default binding: Excel and Windows
//! ListView bind none of them, and Ctrl+Plus is the application's zoom.
//! Nor have the sort commands, `ClearAllSorting` included: the
//! application binds them.
//!
//! Tab is not bound: the grid is one Tab stop and Tab leaves it, as in the
//! WAI-ARIA grid pattern, so the grid never traps the keyboard focus of the
//! window around it. In an open cell editor Tab commits and moves right,
//! Shift+Tab left.
//!
//! F2 opens the focused cell's editor, Backspace and Delete open it with an
//! empty draft; in the editor ([`EDITOR_KEY_CONTEXT`], inside
//! [`KEY_CONTEXT`]) Enter commits and moves down, Shift+Enter and ↑ up, ↓
//! down, Escape cancels, and every other key of the grid is `NoAction`, so
//! the grid's navigation and commands stay out of the way while the field
//! has the focus. Space on a focused cell of a `Control` column
//! ([`CELL_CONTENT_KEY`] `== control`) presses its element; unhandled, it
//! goes on to `ToggleSelection`.
//!
//! Group captions and tree rows expand with `+` (Shift+= on a US layout)
//! and collapse with `-`; ← and → on a caption collapse, expand and move
//! along the groups as in the WAI-ARIA treegrid pattern (they stay
//! `SelectPrevColumn` and `SelectNextColumn`). Expand all, collapse all and
//! captions only have no default binding.
//!
//! [`LiveGridState::current_column`]: crate::LiveGridState::current_column

use gpui_kit::base::actions::{
    Cancel, Confirm, SelectDown, SelectFirst, SelectLast, SelectNextColumn, SelectPageDown,
    SelectPageUp, SelectPrevColumn, SelectUp,
};
use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate, NoAction, OperatingSystem};

/// The key context of the grid root.
pub const KEY_CONTEXT: &str = "LiveGrid";

/// The key context of the group panel, inside [`KEY_CONTEXT`]: its keys act
/// on the focused chip.
pub const GROUP_PANEL_KEY_CONTEXT: &str = "LiveGridGroupPanel";

/// The key context of an open cell editor's host, inside [`KEY_CONTEXT`]:
/// its bindings are deeper than the grid's, and every key of the grid
/// without one of its own is `NoAction` there.
pub const EDITOR_KEY_CONTEXT: &str = "LiveGridEditor";

/// The attribute of [`KEY_CONTEXT`] that names what the focused data cell's
/// column paints: `text`, `element` or `control` (`CellContent`), or `link`
/// for a cell whose value is a link; absent without a focused data cell.
/// Space presses a control: `LiveGrid && cell == control`; Enter opens a
/// link: `LiveGrid && cell == link`.
pub const CELL_CONTENT_KEY: &str = "cell";

/// The key context of the column chooser: a layer next to the grid root,
/// outside [`KEY_CONTEXT`].
pub const COLUMN_CHOOSER_KEY_CONTEXT: &str = "LiveGridColumnChooser";

/// The predicate of a binding on a focused `Control` cell.
const CONTROL_CELL_CONTEXT: &str = "LiveGrid && cell == control";

/// The predicate of a binding on a focused link.
const LINK_CELL_CONTEXT: &str = "LiveGrid && cell == link";

gpui_kit::actions!(
    live_grid,
    [
        // Column commands.
        /// Fits every visible column to its header and painted rows.
        AutofitColumns,
        /// Fits the current column to its header and painted rows.
        AutofitColumn,
        /// Hides the current column (never the last visible one).
        HideColumn,
        /// Shows every hidden column at its place.
        ShowAllColumns,
        /// Freezes the current column at the left edge.
        FreezeColumnLeft,
        /// Freezes the current column at the right edge.
        FreezeColumnRight,
        /// Moves the current column back to the scrolling columns.
        UnfreezeColumn,
        /// Moves the current column one visible place left within its zone,
        /// if the column move policy allows it.
        MoveColumnLeft,
        /// Moves the current column one visible place right within its
        /// zone, if the column move policy allows it.
        MoveColumnRight,
        /// Restores the default layout of the schema
        /// (`LiveGridState::reset_layout`, with the `User` origin).
        ResetLayout,
        // Selection and focus.
        /// Focuses the first row, in the same column.
        SelectFirstRow,
        /// Focuses the last row, in the same column.
        SelectLastRow,
        /// Focuses the first row and the first visible column.
        SelectFirstCell,
        /// Focuses the last row and the last visible column.
        SelectLastCell,
        /// Extends the selection to the row above.
        ExtendSelectionUp,
        /// Extends the selection to the row below.
        ExtendSelectionDown,
        /// Extends the selection to the visible column on the left.
        ExtendSelectionPrevColumn,
        /// Extends the selection to the visible column on the right.
        ExtendSelectionNextColumn,
        /// Extends the selection a page up.
        ExtendSelectionPageUp,
        /// Extends the selection a page down.
        ExtendSelectionPageDown,
        /// Extends the selection to the first visible column.
        ExtendSelectionToFirst,
        /// Extends the selection to the last visible column.
        ExtendSelectionToLast,
        /// Extends the selection to the first row.
        ExtendSelectionToFirstRow,
        /// Extends the selection to the last row.
        ExtendSelectionToLastRow,
        /// Extends the selection to the first cell.
        ExtendSelectionToFirstCell,
        /// Extends the selection to the last cell.
        ExtendSelectionToLastCell,
        /// Selects every row on screen (in the cell modes, every visible
        /// cell); nothing in the single modes.
        SelectAll,
        /// Toggles the focused row or cell; selects it in the single modes.
        ToggleSelection,
        // Copying, the context menu and sorting.
        /// Copies the selected rows or cells as tab-separated text.
        Copy,
        /// Copies every shown row, with column headers, as tab-separated text.
        CopyAll,
        /// Opens the context menu at the focused cell
        /// (`LiveGridState::show_context_menu`).
        ShowContextMenu,
        /// Sorts by the current column alone, ascending (`SortOrigin::User`),
        /// if the column is sortable.
        SortAscending,
        /// Sorts by the current column alone, descending (`SortOrigin::User`),
        /// if the column is sortable.
        SortDescending,
        /// Takes the current column out of the sort; the other sorted
        /// columns stay.
        ClearSort,
        // The whole sort.
        /// Clears the whole sort, every column (`SortOrigin::User`); does
        /// nothing when nothing is sorted.
        ClearAllSorting,
        // Grouping, `GroupOrigin::User`.
        /// Groups by the current column: a new last level, ascending. Does
        /// nothing for a column already grouped by, one that is not
        /// groupable, or in a tree view.
        GroupByColumn,
        /// Takes the current column's level out of the grouping; on a chip
        /// of the group panel, the chip's level.
        UngroupColumn,
        /// Takes every level out of the grouping; does nothing when not
        /// grouped.
        ClearGrouping,
        // Expanding and collapsing.
        /// Expands the focused group caption, or the focused tree row with
        /// children (`GroupOrigin::User`).
        ExpandRow,
        /// Collapses the focused group caption or tree row with children; on
        /// a data row of a grouped view, its group, and the focus goes to
        /// the group's caption.
        CollapseRow,
        /// Expands every group and tree row: the default depth `All`, every
        /// toggle cleared.
        ExpandAll,
        /// Collapses every group and tree row: the default depth
        /// `Levels(0)`, every toggle cleared.
        CollapseAll,
        /// Shows the captions of every group level but no data rows: the
        /// default depth `Captions`, every toggle cleared.
        ShowCaptionsOnly,
        // Editing, and pressing a cell's control.
        /// Opens the editor of the focused cell, starting from its value.
        StartEdit,
        /// Opens the editor of the focused cell with an empty draft.
        StartEditEmpty,
        /// Commits the open editor; the focus moves as the edit gesture says
        /// for Enter (down by default).
        CommitEdit,
        /// Commits the open editor and moves up.
        CommitEditUp,
        /// Commits the open editor and moves down.
        CommitEditDown,
        /// Commits the open editor and moves right.
        CommitEditNext,
        /// Commits the open editor and moves left.
        CommitEditPrev,
        /// Closes the open editor without a commit.
        CancelEdit,
        /// Presses the element of the focused cell of a `Control` column,
        /// through the handler it registered; without one, the key goes on.
        ActivateCell,
        /// Opens the link of the focused cell (`CellValue::Link`); on any
        /// other cell, the key goes on.
        OpenLink,
        // Columns.
        /// Opens the column chooser.
        ShowColumnChooser,
    ]
);

/// The default keymap in [`KEY_CONTEXT`]: Excel's, with Cmd on macOS.
/// `secondary-` is Cmd on a macOS host, in a browser there too, and Ctrl
/// elsewhere, as `cx.platform_info()` reports the host, so one entry serves
/// every platform.
///
/// The context menu opens with Shift+F10 and the Menu key (`menu`; Mac
/// keyboards have none). Not bound: `tab` (leaves the grid).
///
/// In [`GROUP_PANEL_KEY_CONTEXT`], on a chip, Delete and Backspace remove
/// its level, and Enter and Space press the focused chip or button as in
/// any toolbar: the grid's own Enter and Space are unbound there.
///
/// In [`EDITOR_KEY_CONTEXT`] the editor's keys commit and cancel; every
/// other key of [`KEY_CONTEXT`] is `NoAction` there, built from the list
/// itself, so a new key of the grid is silenced in the editor too. An
/// application unsilences one with its own binding in that context after
/// `init`.
///
/// In [`COLUMN_CHOOSER_KEY_CONTEXT`], Alt+↑ and Alt+↓ move the highlighted
/// column up and down its zone, and Space shows or hides it.
pub fn key_bindings(cx: &App) -> Vec<KeyBinding> {
    let os = cx.platform_info().os;
    let mut bindings = grid_key_bindings(cx);
    let editor = editor_key_bindings(cx);
    // Every key of the grid without a binding of its own in the editor.
    let own: Vec<String> = editor
        .iter()
        .filter_map(|binding| binding_key(binding, os))
        .collect();
    let mut silenced: Vec<String> = Vec::new();
    for key in bindings
        .iter()
        .filter_map(|binding| binding_key(binding, os))
    {
        if !own.contains(&key) && !silenced.contains(&key) {
            silenced.push(key);
        }
    }
    let silenced = silenced
        .iter()
        .map(|key| bind(key, NoAction, Some(EDITOR_KEY_CONTEXT), cx));
    bindings.extend(group_panel_key_bindings());
    bindings.extend(editor);
    bindings.extend(silenced);
    bindings.extend(column_chooser_key_bindings());
    bindings
}

/// A binding of `keystrokes` in `context`, `secondary-` resolved for the
/// host (`KeyBinding::new` takes explicit modifiers only).
fn bind(keystrokes: &str, action: impl Action, context: Option<&str>, cx: &App) -> KeyBinding {
    let predicate = context.map(|context| {
        KeyBindingContextPredicate::parse(context)
            .expect("valid key context")
            .into()
    });
    KeyBinding::load(keystrokes, Box::new(action), predicate, false, None, cx)
        .expect("valid keystroke")
}

/// The keystroke of a binding, as text that parses back to it on `os`.
fn binding_key(binding: &KeyBinding, os: OperatingSystem) -> Option<String> {
    match binding.keystrokes() {
        [keystroke] => Some(keystroke.inner().unparse(os)),
        _ => None,
    }
}

/// The bindings of [`KEY_CONTEXT`], `ActivateCell` on a control included.
fn grid_key_bindings(cx: &App) -> Vec<KeyBinding> {
    let context = Some(KEY_CONTEXT);
    vec![
        bind("up", SelectUp, context, cx),
        bind("down", SelectDown, context, cx),
        bind("left", SelectPrevColumn, context, cx),
        bind("right", SelectNextColumn, context, cx),
        bind("pageup", SelectPageUp, context, cx),
        bind("pagedown", SelectPageDown, context, cx),
        bind("home", SelectFirst, context, cx),
        bind("end", SelectLast, context, cx),
        bind("secondary-left", SelectFirst, context, cx),
        bind("secondary-right", SelectLast, context, cx),
        bind("secondary-up", SelectFirstRow, context, cx),
        bind("secondary-down", SelectLastRow, context, cx),
        bind("secondary-home", SelectFirstCell, context, cx),
        bind("secondary-end", SelectLastCell, context, cx),
        bind("shift-up", ExtendSelectionUp, context, cx),
        bind("shift-down", ExtendSelectionDown, context, cx),
        bind("shift-left", ExtendSelectionPrevColumn, context, cx),
        bind("shift-right", ExtendSelectionNextColumn, context, cx),
        bind("shift-pageup", ExtendSelectionPageUp, context, cx),
        bind("shift-pagedown", ExtendSelectionPageDown, context, cx),
        bind("shift-home", ExtendSelectionToFirst, context, cx),
        bind("shift-end", ExtendSelectionToLast, context, cx),
        bind("shift-secondary-left", ExtendSelectionToFirst, context, cx),
        bind("shift-secondary-right", ExtendSelectionToLast, context, cx),
        bind("shift-secondary-up", ExtendSelectionToFirstRow, context, cx),
        bind(
            "shift-secondary-down",
            ExtendSelectionToLastRow,
            context,
            cx,
        ),
        bind(
            "shift-secondary-home",
            ExtendSelectionToFirstCell,
            context,
            cx,
        ),
        bind(
            "shift-secondary-end",
            ExtendSelectionToLastCell,
            context,
            cx,
        ),
        bind("secondary-a", SelectAll, context, cx),
        bind("space", ToggleSelection, context, cx),
        bind("enter", Confirm { secondary: false }, context, cx),
        bind("secondary-enter", Confirm { secondary: true }, context, cx),
        bind("escape", Cancel, context, cx),
        // Copy and the context menu.
        bind("secondary-c", Copy, context, cx),
        bind("shift-f10", ShowContextMenu, context, cx),
        bind("menu", ShowContextMenu, context, cx),
        // Expanding and collapsing. `+` is typed as Shift+= on a US layout,
        // which matches both bindings.
        bind("+", ExpandRow, context, cx),
        bind("shift-=", ExpandRow, context, cx),
        bind("-", CollapseRow, context, cx),
        // Editing.
        bind("f2", StartEdit, context, cx),
        bind("backspace", StartEditEmpty, context, cx),
        bind("delete", StartEditEmpty, context, cx),
        // After `ToggleSelection`, so it comes first on a control (same depth,
        // later binding); unhandled, Space goes on to `ToggleSelection`.
        bind("space", ActivateCell, Some(CONTROL_CELL_CONTEXT), cx),
        // Enter opens a focused link; unhandled, it goes on to `Confirm`.
        bind("enter", OpenLink, Some(LINK_CELL_CONTEXT), cx),
    ]
}

/// The bindings of [`GROUP_PANEL_KEY_CONTEXT`].
fn group_panel_key_bindings() -> [KeyBinding; 4] {
    let context = Some(GROUP_PANEL_KEY_CONTEXT);
    [
        KeyBinding::new("delete", UngroupColumn, context),
        KeyBinding::new("backspace", UngroupColumn, context),
        KeyBinding::new("enter", NoAction, context),
        KeyBinding::new("space", NoAction, context),
    ]
}

/// The editor's own bindings in [`EDITOR_KEY_CONTEXT`].
fn editor_key_bindings(cx: &App) -> Vec<KeyBinding> {
    let context = Some(EDITOR_KEY_CONTEXT);
    vec![
        bind("enter", CommitEdit, context, cx),
        bind("secondary-enter", CommitEdit, context, cx),
        bind("shift-enter", CommitEditUp, context, cx),
        bind("up", CommitEditUp, context, cx),
        bind("down", CommitEditDown, context, cx),
        bind("tab", CommitEditNext, context, cx),
        bind("shift-tab", CommitEditPrev, context, cx),
        bind("escape", CancelEdit, context, cx),
    ]
}

/// The bindings of [`COLUMN_CHOOSER_KEY_CONTEXT`]: move the highlighted
/// column, show or hide it.
fn column_chooser_key_bindings() -> [KeyBinding; 3] {
    let context = Some(COLUMN_CHOOSER_KEY_CONTEXT);
    [
        KeyBinding::new("alt-up", MoveColumnLeft, context),
        KeyBinding::new("alt-down", MoveColumnRight, context),
        KeyBinding::new("space", Confirm { secondary: false }, context),
    ]
}

/// Binds [`key_bindings`]. Call once, after `gpui_kit::init`. Without it the
/// mouse works and the keys do not.
pub fn init(cx: &mut App) {
    let bindings = key_bindings(cx);
    cx.bind_keys(bindings);
}

// `key_bindings` reads the host from an `App`: the test platform's.
#[cfg(all(test, feature = "test-support"))]
mod tests {
    use std::collections::{HashMap, HashSet};

    use gpui_kit::{Keystroke, PlatformInfo, PlatformTarget, TestAppContext};

    use super::*;

    /// A binding as "keystroke → action name", the keystroke in its parsed
    /// form on `os`, so `secondary-a` and the host's `cmd-a` agree.
    fn binding(keystroke: &str, action: &str, os: OperatingSystem) -> (String, String) {
        let keystroke = Keystroke::parse_with_os(keystroke, os).expect("valid keystroke");
        (keystroke.unparse(os), action.to_string())
    }

    #[gpui_kit::test]
    fn test_key_bindings(cx: &mut TestAppContext) {
        for (os, select_all) in [
            (OperatingSystem::Mac, "cmd-a"),
            (OperatingSystem::Linux, "ctrl-a"),
            (OperatingSystem::Windows, "ctrl-a"),
        ] {
            cx.set_platform_info(PlatformInfo {
                os,
                target: PlatformTarget::Native,
            });
            let bindings = cx.update(|cx| key_bindings(cx));
            // `secondary-` is the host's modifier.
            assert!(
                bindings.iter().any(|binding| {
                    binding.action().as_any().is::<SelectAll>()
                        && binding_key(binding, os).as_deref() == Some(select_all)
                }),
                "{select_all} on {os:?}"
            );
            assert_key_bindings(bindings, os);
        }
    }

    fn assert_key_bindings(bindings: Vec<KeyBinding>, os: OperatingSystem) {
        // Bindings by context, each key once per context.
        let mut by_context: HashMap<String, HashSet<(String, String)>> = HashMap::new();
        let mut keys_by_context: HashMap<String, HashSet<String>> = HashMap::new();
        for binding in &bindings {
            let [keystroke] = binding.keystrokes() else {
                panic!("one keystroke per binding");
            };
            let key = keystroke.inner().unparse(os);
            let context = binding
                .predicate()
                .expect("every binding has a context")
                .to_string();
            assert!(
                keys_by_context
                    .entry(context.clone())
                    .or_default()
                    .insert(key.clone()),
                "{key} is bound twice in {context}"
            );
            by_context
                .entry(context)
                .or_default()
                .insert((key, binding.action().name().to_string()));
        }
        let set = |table: &[(&str, &str)]| -> HashSet<(String, String)> {
            let set: HashSet<_> = table
                .iter()
                .map(|(keystroke, action)| binding(keystroke, action, os))
                .collect();
            assert_eq!(set.len(), table.len());
            set
        };
        // Every default binding of the grid, once.
        let grid = set(&[
            ("up", "ui::SelectUp"),
            ("down", "ui::SelectDown"),
            ("left", "ui::SelectPrevColumn"),
            ("right", "ui::SelectNextColumn"),
            ("pageup", "ui::SelectPageUp"),
            ("pagedown", "ui::SelectPageDown"),
            ("home", "ui::SelectFirst"),
            ("end", "ui::SelectLast"),
            ("secondary-left", "ui::SelectFirst"),
            ("secondary-right", "ui::SelectLast"),
            ("secondary-up", "live_grid::SelectFirstRow"),
            ("secondary-down", "live_grid::SelectLastRow"),
            ("secondary-home", "live_grid::SelectFirstCell"),
            ("secondary-end", "live_grid::SelectLastCell"),
            ("shift-up", "live_grid::ExtendSelectionUp"),
            ("shift-down", "live_grid::ExtendSelectionDown"),
            ("shift-left", "live_grid::ExtendSelectionPrevColumn"),
            ("shift-right", "live_grid::ExtendSelectionNextColumn"),
            ("shift-pageup", "live_grid::ExtendSelectionPageUp"),
            ("shift-pagedown", "live_grid::ExtendSelectionPageDown"),
            ("shift-home", "live_grid::ExtendSelectionToFirst"),
            ("shift-end", "live_grid::ExtendSelectionToLast"),
            ("shift-secondary-left", "live_grid::ExtendSelectionToFirst"),
            ("shift-secondary-right", "live_grid::ExtendSelectionToLast"),
            ("shift-secondary-up", "live_grid::ExtendSelectionToFirstRow"),
            (
                "shift-secondary-down",
                "live_grid::ExtendSelectionToLastRow",
            ),
            (
                "shift-secondary-home",
                "live_grid::ExtendSelectionToFirstCell",
            ),
            (
                "shift-secondary-end",
                "live_grid::ExtendSelectionToLastCell",
            ),
            ("secondary-a", "live_grid::SelectAll"),
            ("space", "live_grid::ToggleSelection"),
            ("enter", "ui::Confirm"),
            ("secondary-enter", "ui::Confirm"),
            ("escape", "ui::Cancel"),
            // Copy and the context menu.
            ("secondary-c", "live_grid::Copy"),
            ("shift-f10", "live_grid::ShowContextMenu"),
            ("menu", "live_grid::ShowContextMenu"),
            // Expanding and collapsing.
            ("+", "live_grid::ExpandRow"),
            ("shift-=", "live_grid::ExpandRow"),
            ("-", "live_grid::CollapseRow"),
            // Editing.
            ("f2", "live_grid::StartEdit"),
            ("backspace", "live_grid::StartEditEmpty"),
            ("delete", "live_grid::StartEditEmpty"),
        ]);
        // On a chip of the group panel.
        let panel = set(&[
            ("delete", "live_grid::UngroupColumn"),
            ("backspace", "live_grid::UngroupColumn"),
            ("enter", "zed::NoAction"),
            ("space", "zed::NoAction"),
        ]);
        // Space on a focused control; the editor's own keys; the column
        // chooser's.
        let control = set(&[("space", "live_grid::ActivateCell")]);
        let link = set(&[("enter", "live_grid::OpenLink")]);
        let editor_own = set(&[
            ("enter", "live_grid::CommitEdit"),
            ("secondary-enter", "live_grid::CommitEdit"),
            ("shift-enter", "live_grid::CommitEditUp"),
            ("up", "live_grid::CommitEditUp"),
            ("down", "live_grid::CommitEditDown"),
            ("tab", "live_grid::CommitEditNext"),
            ("shift-tab", "live_grid::CommitEditPrev"),
            ("escape", "live_grid::CancelEdit"),
        ]);
        let chooser = set(&[
            ("alt-up", "live_grid::MoveColumnLeft"),
            ("alt-down", "live_grid::MoveColumnRight"),
            ("space", "ui::Confirm"),
        ]);
        // In the editor, every key of the grid is either the editor's own or
        // `NoAction`.
        let own_keys: HashSet<String> = editor_own.iter().map(|(key, _)| key.clone()).collect();
        let mut editor = editor_own.clone();
        for (key, _) in grid.iter().chain(&control).chain(&link) {
            if !own_keys.contains(key) {
                editor.insert((key.clone(), "zed::NoAction".to_string()));
            }
        }
        let expected = HashMap::from([
            (KEY_CONTEXT.to_string(), grid),
            (GROUP_PANEL_KEY_CONTEXT.to_string(), panel),
            (CONTROL_CELL_CONTEXT.to_string(), control),
            (LINK_CELL_CONTEXT.to_string(), link),
            (EDITOR_KEY_CONTEXT.to_string(), editor),
            (COLUMN_CHOOSER_KEY_CONTEXT.to_string(), chooser),
        ]);
        assert_eq!(by_context, expected);
        assert!(
            by_context[EDITOR_KEY_CONTEXT].contains(&binding("left", "zed::NoAction", os))
                && by_context[EDITOR_KEY_CONTEXT].contains(&binding("f2", "zed::NoAction", os))
                && by_context[EDITOR_KEY_CONTEXT].contains(&binding("space", "zed::NoAction", os)),
            "the grid's navigation and commands are silenced in the editor"
        );

        // `ActivateCell` comes after `ToggleSelection`: at one depth the
        // later binding wins.
        let position = |action: &str| {
            bindings
                .iter()
                .position(|binding| binding.action().name() == action)
                .expect("bound")
        };
        assert!(position("live_grid::ActivateCell") > position("live_grid::ToggleSelection"));
        // Enter and Cmd/Ctrl+Enter differ by `secondary`.
        let confirms: Vec<_> = bindings
            .iter()
            .filter(|binding| {
                binding
                    .predicate()
                    .is_some_and(|predicate| predicate.to_string() == KEY_CONTEXT)
            })
            .filter_map(|binding| binding.action().as_any().downcast_ref::<Confirm>())
            .map(|confirm| confirm.secondary)
            .collect();
        assert_eq!(confirms, [false, true]);
        // The sort commands are the application's to bind, as are expand
        // all, collapse all, captions only, the grouping commands and the
        // column chooser.
        assert!(
            bindings.iter().all(|binding| {
                let action = binding.action().as_any();
                !action.is::<ClearAllSorting>()
                    && !action.is::<ClearSort>()
                    && !action.is::<ExpandAll>()
                    && !action.is::<CollapseAll>()
                    && !action.is::<ShowCaptionsOnly>()
                    && !action.is::<GroupByColumn>()
                    && !action.is::<ClearGrouping>()
                    && !action.is::<ShowColumnChooser>()
            }),
            "no default key for the application's commands"
        );
        // Tab leaves the grid: free in the grid, bound in the editor.
        for free in ["tab", "shift-tab"] {
            let key = Keystroke::parse_with_os(free, os)
                .expect("valid keystroke")
                .unparse(os);
            assert!(
                !keys_by_context[KEY_CONTEXT].contains(&key),
                "{free} is bound"
            );
            assert!(keys_by_context[EDITOR_KEY_CONTEXT].contains(&key), "{free}");
        }
    }
}
