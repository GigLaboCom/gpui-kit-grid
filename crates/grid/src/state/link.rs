//! Links in cells: a [`CellValue::Link`] painted by the body as underlined
//! text in the theme's link colour, the pointer cursor over its text, and
//! what opens it: a click on its text, Enter on the focused cell
//! (`OpenLink`, bound in `LiveGrid && cell == link`). What opening means is
//! the [`LinkOpener`] seam; the default opens the address in the system's
//! browser, or a new tab on the web.
//!
//! A link is the one place in the grid where the pointer cursor is right
//! (gpui-kit's rule: buttons keep the default cursor, links do not). The
//! frame records where each link's text was painted; the pointer is over
//! a link when it is over one of those, as of the last frame.

use std::rc::Rc;

use gpui_kit::{App, Bounds, Context, Modifiers, MouseButton, Pixels, Point, Window};

use super::{ColumnOf, KeyOf, LiveGridDelegate, LiveGridState, menu::GridTarget};
use crate::{
    actions::OpenLink,
    model::{CellValue, GridRow},
};

/// Opens a link of the grid: a click on a [`CellValue::Link`]'s text, or
/// Enter on its focused cell.
///
/// Called once per gesture, after the grid's update (`App::defer`): it may
/// read and update the grid.
pub trait LinkOpener: 'static {
    /// Opens `url`, the address of the link that was activated.
    fn open(&self, url: &str, cx: &mut App);
}

/// Opens the address with `App::open_url`: natively in the system's
/// browser, on the web in a new tab. The default.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenInBrowser;

impl LinkOpener for OpenInBrowser {
    fn open(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }
}

/// What a grid keeps for its links: how it opens them, where the last
/// frame painted their texts, and whether the pointer is over one.
pub(super) struct LinkSlot {
    opener: Rc<dyn LinkOpener>,
    /// The painted texts of links in window coordinates, the cell's height.
    pub(super) rects: Vec<Bounds<Pixels>>,
    /// The pointer is over a link's text: the body shows the pointer.
    pub(super) hovered: bool,
    /// The press series began on a link's text: its second press is no
    /// `Confirm`, the first opened the link.
    pub(super) series_on_link: bool,
}

impl Default for LinkSlot {
    fn default() -> Self {
        Self {
            opener: Rc::new(OpenInBrowser),
            rects: Vec::new(),
            hovered: false,
            series_on_link: false,
        }
    }
}

impl LinkSlot {
    /// Returns true if `position` is over a painted link's text.
    pub(super) fn is_over(&self, position: Point<Pixels>) -> bool {
        self.rects.iter().any(|rect| rect.contains(&position))
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets what opening a link does (default [`OpenInBrowser`]): an
    /// application may open it in a view of its own, or ask first.
    pub fn with_link_opener(mut self, opener: impl LinkOpener) -> Self {
        self.links.opener = Rc::new(opener);
        self
    }

    /// Returns true if the pointer is over a link's text, as of the last
    /// frame: the body shows the pointer cursor.
    pub fn is_link_hovered(&self) -> bool {
        self.links.hovered
    }

    /// Returns true if the pointer is over a link's text now.
    pub(super) fn is_pointer_on_link(&self) -> bool {
        self.tooltip
            .position
            .is_some_and(|position| self.links.is_over(position))
    }

    /// The pointer moved to `position` (`None`: it left the body), or a
    /// frame painted links anew: the pointer cursor follows.
    pub(super) fn update_link_hover(
        &mut self,
        position: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let hovered = position.is_some_and(|position| self.links.is_over(position));
        if hovered != self.links.hovered {
            self.links.hovered = hovered;
            cx.notify();
        }
    }

    /// The address of the link in the cell `target` names, if its value is
    /// a link now.
    fn link_url(&self, target: &GridTarget<KeyOf<D>, ColumnOf<D>>) -> Option<String> {
        let (row, column) = match target {
            GridTarget::Cell { key, column } => (self.snapshot.get(key)?.clone(), *column),
            GridTarget::Pinned { edge, key, column } => (
                self.snapshot
                    .pinned_rows(*edge)
                    .iter()
                    .find(|row| row.key() == key)?
                    .clone(),
                *column,
            ),
            _ => return None,
        };
        match row.value(column) {
            CellValue::Link(link) => Some(link.url().to_owned()),
            _ => None,
        }
    }

    /// Opens the link of `target` after this update, if it has one.
    fn open_link_of(
        &self,
        target: &GridTarget<KeyOf<D>, ColumnOf<D>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(url) = self.link_url(target) else {
            return false;
        };
        let opener = self.links.opener.clone();
        cx.defer(move |cx| opener.open(&url, cx));
        true
    }

    /// A click (`LiveGridEvent::Click`) that pressed and released the left
    /// button once, without modifiers, on a link's painted text opens it.
    pub(super) fn open_clicked_link(
        &self,
        target: &GridTarget<KeyOf<D>, ColumnOf<D>>,
        button: MouseButton,
        modifiers: Modifiers,
        click_count: usize,
        origin: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if button == MouseButton::Left
            && click_count == 1
            && !modifiers.modified()
            && self.links.is_over(origin)
        {
            self.open_link_of(target, cx);
        }
    }

    /// Returns true if the focused cell of a data or pinned row holds a
    /// link now: the key context's `cell` is `link`.
    pub(super) fn is_link_focused(&self) -> bool {
        self.focused_link_target()
            .is_some_and(|target| self.link_url(&target).is_some())
    }

    fn focused_link_target(&self) -> Option<GridTarget<KeyOf<D>, ColumnOf<D>>> {
        let focus = self.selection.focus()?;
        Some(GridTarget::Cell {
            key: focus.row()?.clone(),
            column: focus.column(),
        })
    }

    /// Binds `OpenLink` (Enter on a focused link).
    pub(super) fn bind_link_actions<E: gpui_kit::InteractiveElement>(
        root: E,
        cx: &Context<Self>,
    ) -> E {
        root.on_action(cx.listener(Self::on_open_link))
    }

    /// Enter on a focused link opens it; on any other cell the key goes on
    /// to `Confirm`.
    fn on_open_link(&mut self, _: &OpenLink, _: &mut Window, cx: &mut Context<Self>) {
        let opened = self
            .focused_link_target()
            .is_some_and(|target| self.open_link_of(&target, cx));
        if !opened {
            cx.propagate();
        }
    }
}
