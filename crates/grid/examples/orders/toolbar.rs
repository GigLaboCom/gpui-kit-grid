//! The toolbar of the orders example: the reorder policy and Refresh
//! order, Keep draft / Follow, New order, Columns… (the grid's column
//! chooser, through its `LiveGridHandle`), Export…, the row size and the
//! theme. Every control is a gpui-kit component in one `Toolbar`: Tab goes
//! through them left to right and then into the grid, past the checkboxes
//! and buttons of its rows (they take no Tab stop).

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Size, Theme, ThemeMode,
    button::{Button, ButtonGroup, Toggle},
    separator::Separator,
    toolbar::{Toolbar, ToolbarGroup},
};
use gpui_kit::{Context, IntoElement, Styled as _};
use gpui_kit_grid::{HoldReorder, LiveGridDelegate, LiveGridState, LiveReorder, ManualReorder};

use crate::{Export, KEY_CONTEXT, NewOrder, Root, e2e::RecordBounds as _};

/// When a new order replaces the shown one: the three built-in reorder
/// policies. Each holds the order while a cell is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reorder {
    Live,
    Hold,
    Manual,
}

impl Reorder {
    pub const ALL: [Self; 3] = [Self::Live, Self::Hold, Self::Manual];

    /// `ORDERS_REORDER`: `live`, `hold` or `manual`.
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

    /// Gives the grid this policy.
    pub fn apply<D: LiveGridDelegate>(
        self,
        grid: &mut LiveGridState<D>,
        cx: &mut Context<LiveGridState<D>>,
    ) {
        match self {
            Self::Live => grid.set_reorder_policy(LiveReorder, cx),
            Self::Hold => grid.set_reorder_policy(HoldReorder::default(), cx),
            Self::Manual => grid.set_reorder_policy(ManualReorder, cx),
        }
    }
}

/// The row sizes of `LiveGrid`.
const SIZES: [(&str, Size); 4] = [
    ("XS", Size::XSmall),
    ("S", Size::Small),
    ("M", Size::Medium),
    ("L", Size::Large),
];

impl Root {
    pub(crate) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let dark = cx.theme().is_dark();
        Toolbar::new("orders-toolbar")
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
                            .disabled(!self.is_order_held())
                            .e2e_bounds("refresh-order")
                            .on_click(cx.listener(|root, _, _, cx| {
                                root.grid.update(cx, |grid, cx| grid.apply_held_order(cx));
                            })),
                    ),
            )
            .content(Separator::vertical().h_5())
            .child(
                ToolbarGroup::new("edit")
                    .label("Edit")
                    .gap_1()
                    .child(
                        Toggle::new("follow")
                            .label(if self.follow { "Follow" } else { "Keep draft" })
                            .checked(self.follow)
                            .tooltip("A changed value replaces an untouched draft")
                            .e2e_bounds("follow")
                            .on_click(cx.listener(|root, follow: &bool, window, cx| {
                                root.set_follow(*follow, window, cx)
                            })),
                    )
                    .child(
                        Button::new("new-order")
                            .label("New order")
                            .tooltip_with_action("New order", &NewOrder, Some(KEY_CONTEXT))
                            .e2e_bounds("new-order")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NewOrder), cx)
                            }),
                    )
                    .child(
                        Button::new("columns")
                            .label("Columns…")
                            .tooltip("Show, hide and reorder the columns")
                            .e2e_bounds("columns")
                            .on_click(cx.listener(|root, _, window, cx| {
                                root.handle.show_column_chooser(window, cx);
                            })),
                    )
                    .child(
                        Button::new("export")
                            .label("Export…")
                            .loading(self.is_exporting())
                            .e2e_bounds("export")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(Export), cx)),
                    ),
            )
            .content(Separator::vertical().h_5())
            .child(
                ToolbarGroup::new("view")
                    .label("View")
                    .gap_1()
                    .child(
                        ButtonGroup::new("size")
                            .outline()
                            .children(SIZES.map(|(label, size)| {
                                Button::new(label).label(label).selected(self.size == size)
                            }))
                            .on_click(cx.listener(|root, clicked: &Vec<usize>, _, cx| {
                                if let Some((_, size)) =
                                    clicked.first().and_then(|ix| SIZES.get(*ix))
                                {
                                    root.size = *size;
                                    cx.notify();
                                }
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
    }
}
