//! Clicks: a press and a release of one button on the grid, with no drag
//! between them, reported as [`LiveGridEvent::Click`] for a data cell, a
//! pinned cell, a header or the empty area.
//!
//! The body and the header start a click when a button goes down, with the
//! target captured then, by key and column, as the context menu's. A press
//! the context menu gesture takes and a press that closes the menu start
//! none; a resize handle never does. The pointer moving past the drag
//! threshold, a column gesture or the next press anywhere in the window
//! drops it. The release decides at once, in the capture phase, after the
//! body's press has emitted its selection events; the event goes out after
//! the release's dispatch (`App::defer`), so it follows the `SortChange` of
//! a header click, which the header cell emits later in the same dispatch.

use gpui_kit::{Context, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point};

use super::{
    ColumnOf, KeyOf, LiveGridDelegate, LiveGridState, events::LiveGridEvent, menu::GridTarget,
    selection::is_past_drag_threshold,
};

/// A press that becomes a click if its button goes up before a drag.
pub(super) struct PendingClick<K, C> {
    target: GridTarget<K, C>,
    button: MouseButton,
    /// The modifiers held when the button went down.
    modifiers: Modifiers,
    click_count: usize,
    /// Where the button went down, in window coordinates.
    origin: Point<Pixels>,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// A press on the body or the header that may end in a click on
    /// `target`.
    pub(super) fn begin_click(
        &mut self,
        target: GridTarget<KeyOf<D>, ColumnOf<D>>,
        event: &MouseDownEvent,
    ) {
        self.click = Some(PendingClick {
            target,
            button: event.button,
            modifiers: event.modifiers,
            click_count: event.click_count,
            origin: event.position,
        });
    }

    /// The press is no click: a drag began, or a new press went down
    /// anywhere in the window (the body and the header start their own
    /// after it).
    pub(crate) fn cancel_click(&mut self) {
        self.click = None;
    }

    /// The pointer moved, with or without a button: past the drag threshold
    /// the press is a drag.
    pub(crate) fn on_click_pointer_move(&mut self, position: Point<Pixels>) {
        if self
            .click
            .as_ref()
            .is_some_and(|click| is_past_drag_threshold(click.origin, position))
        {
            self.click = None;
        }
    }

    /// A button went up anywhere in the window (capture phase). The release
    /// of the pending press, within the drag threshold, is a click: its
    /// event goes out after this dispatch.
    pub(crate) fn on_click_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(click) = self.click.take_if(|click| click.button == event.button) else {
            return;
        };
        if is_past_drag_threshold(click.origin, event.position) {
            return;
        }
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = this.update(cx, |state, cx| state.emit_click(click, cx));
        });
    }

    /// Emits the `Click`, unless the target's row left the store (or its
    /// band) since the press: a click names a row the grid shows.
    fn emit_click(&mut self, click: PendingClick<KeyOf<D>, ColumnOf<D>>, cx: &mut Context<Self>) {
        if self.is_target_gone(&click.target) {
            return;
        }
        self.open_clicked_link(
            &click.target,
            click.button,
            click.modifiers,
            click.click_count,
            click.origin,
            cx,
        );
        cx.emit(LiveGridEvent::Click {
            target: click.target,
            button: click.button,
            modifiers: click.modifiers,
            click_count: click.click_count,
        });
    }
}
