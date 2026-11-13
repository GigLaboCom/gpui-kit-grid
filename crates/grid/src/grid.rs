//! `LiveGrid`: the element applications place in their layout, following
//! gpui-kit's `DataTable` pattern (a `RenderOnce` wrapper over the state).

use gpui_kit::{
    App, Entity, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window,
    component::{ActiveTheme as _, Sizable, Size, ThemeStyled as _},
    div,
    prelude::FluentBuilder as _,
};

use crate::state::{LiveGridDelegate, LiveGridState};

/// A live grid element over a [`LiveGridState`].
///
/// ```ignore
/// LiveGrid::new(&grid)
///     .accessibility_label("Trades")
///     .xsmall()
///     .bordered(false)
///     .group_panel(true)
/// ```
///
/// Sizes follow gpui-kit tables: `xsmall` 26 px rows, `small` 30, `medium`
/// (default) 32, `large` 40, or an explicit `Size::Size(px)`.
///
/// The cells take the font of the element the grid is placed in: its
/// family, size and OpenType features, and so do the ghosts its drags show
/// under the pointer (a header, a group level, rows), though GPUI draws them
/// outside that element. The grid adds tabular figures (`tnum`) so digits
/// line up, and keeps the rest, so a feature set on an ancestor reaches the
/// cells:
///
/// ```ignore
/// div()
///     .font_family("IBM Plex Mono")
///     // A slashed zero in every cell.
///     .font_features(FontFeatures(Arc::new(vec![("zero".into(), 1)])))
///     .child(LiveGrid::new(&grid))
/// ```
///
/// Give every grid an accessible name with [`Self::accessibility_label`]:
/// a screen reader announces the grid by it (Design Guides, "controls have
/// accessible names").
#[derive(IntoElement)]
pub struct LiveGrid<D: LiveGridDelegate> {
    state: Entity<LiveGridState<D>>,
    size: Size,
    bordered: bool,
    accessibility_label: Option<SharedString>,
    group_panel: bool,
}

impl<D: LiveGridDelegate> LiveGrid<D> {
    /// A grid element for `state`.
    pub fn new(state: &Entity<LiveGridState<D>>) -> Self {
        Self {
            state: state.clone(),
            size: Size::default(),
            bordered: true,
            accessibility_label: None,
            group_panel: false,
        }
    }

    /// The accessible name of the grid, e.g. "Trades": a screen reader
    /// names the table by it. No name by default.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Draws a border with the theme radius around the grid (default true).
    pub fn bordered(mut self, bordered: bool) -> Self {
        self.bordered = bordered;
        self
    }

    /// Shows the group panel above the header (default false): the
    /// grouping levels as chips, and a drop zone for column headers. The
    /// user drags a header into it to group by the column, drags a chip to
    /// reorder the levels or out onto the grid to remove one, clicks a chip
    /// to reverse its groups, and removes it with its button. The panel is
    /// one header row tall; the rows get that much less room.
    ///
    /// The application decides when it shows, as with [`Self::bordered`]:
    /// always, only while the grid is grouped, or behind a toolbar toggle.
    pub fn group_panel(mut self, shown: bool) -> Self {
        self.group_panel = shown;
        self
    }
}

impl<D: LiveGridDelegate> Sizable for LiveGrid<D> {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl<D: LiveGridDelegate> RenderOnce for LiveGrid<D> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let row_height = self.size.table_row_height();
        let label = self.accessibility_label;
        let (size, group_panel) = (self.size, self.group_panel);
        self.state.update(cx, |state, cx| {
            state.set_row_height(row_height);
            state.set_accessibility_label(label);
            state.set_group_panel_shown(group_panel, size, cx);
        });
        // The rows have the focus on the root or in an open editor.
        let is_body_focused = self.state.read(cx).is_body_focused(window, cx);
        let theme = cx.theme();
        // The grid view clips its own content; this wrapper does not clip,
        // so the focus ring outside its border stays visible.
        div()
            .size_full()
            .when(self.bordered, |this| {
                this.border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
            })
            // Keyboard focus only, as CSS `:focus-visible` (as gpui-kit's
            // `DataTable`): a click focuses the grid without a ring.
            .when(
                is_body_focused && window.last_input_was_keyboard(),
                |this| this.focus_ring_style(window, cx),
            )
            .child(self.state)
    }
}
