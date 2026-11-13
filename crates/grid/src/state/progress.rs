//! Progress: the progress shown over the body and its overlay.
//!
//! Two sources: the view ([`LiveGridState::set_progress`], UI thread) and
//! the store (`GridWriter::set_progress`, any thread, in order with the
//! data). The view's progress wins; an application that wants another rule
//! combines both itself. The store's progress is ordered with the data:
//! when the store clears it at version V while the order on screen is still
//! built from an older version, the view keeps showing it until the order
//! covers V (the coverage `flush` waits for). So the bar does not leave
//! before the rows it announced are shown.
//!
//! The overlay is absolute over the body, so it moves no row when it comes
//! or goes, and it has no hitbox: presses and the wheel reach the body.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, progress::Progress};
use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use rust_i18n::t;

use super::{LiveGridDelegate, LiveGridState};
use crate::store::GridProgress;

/// The element id of the overlay's container.
const OVERLAY_ID: &str = "live-grid-progress";
/// The element id of the default overlay's label on an empty grid.
const LABEL_ID: &str = "live-grid-progress-label";

/// What [`LiveGridDelegate::render_progress`] knows about the grid.
///
/// [`LiveGridDelegate::render_progress`]: crate::LiveGridDelegate::render_progress
pub struct ProgressContext {
    empty: bool,
}

impl ProgressContext {
    /// Returns true if the grid shows no data rows, as during a first load.
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// The default presentation: a 4 px bar (gpui-kit's `Progress`,
    /// `xsmall`) across the grid right under the header, over the pinned
    /// rows and the data rows; indeterminate when the length of the work is
    /// unknown. On an empty grid the progress label, or "Loading…", is
    /// centered in the body as muted text. With data shown there is no
    /// label: the previous rows stay readable while they refresh (Design
    /// Guides, "Loading: preserve context").
    ///
    /// The label is also the bar's accessible name. gpui-kit's `Progress`
    /// honors reduced motion: an indeterminate bar does not animate then.
    pub fn default_overlay(
        &self,
        progress: &GridProgress,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let label = progress_label(progress);
        div()
            .relative()
            .size_full()
            .child(
                Progress::new("live-grid-progress-bar")
                    .xsmall()
                    .loading(progress.is_indeterminate())
                    .value(progress.fraction().unwrap_or_default() * 100.)
                    .accessibility_label(label.clone())
                    // Edge to edge under the header's line.
                    .rounded_none()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0(),
            )
            .when(self.empty, |this| {
                this.child(
                    div()
                        .id(LABEL_ID)
                        .test_support()
                        .absolute()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
            })
            .into_any_element()
    }
}

/// The text of a progress: its label, or "Loading…".
fn progress_label(progress: &GridProgress) -> SharedString {
    match progress.label() {
        Some(label) => SharedString::from(label.to_owned()),
        None => SharedString::from(t!("live_grid.progress.loading").into_owned()),
    }
}

/// The progress of the view and the store's progress as shown.
#[derive(Default)]
pub(super) struct ProgressSlot {
    /// [`LiveGridState::set_progress`].
    view: Option<GridProgress>,
    /// The store's progress as shown: kept after the store cleared it,
    /// until the order on screen covers the clearing.
    store: Option<GridProgress>,
    /// The store version that cleared `store`, while it is still shown.
    cleared_at: Option<u64>,
}

impl ProgressSlot {
    /// A slot that shows the store's progress `store` from the start.
    pub(super) fn new(store: Option<&GridProgress>) -> Self {
        Self {
            view: None,
            store: store.cloned(),
            cleared_at: None,
        }
    }

    /// The progress shown: the view's, else the store's.
    fn shown(&self) -> Option<&GridProgress> {
        self.view.as_ref().or(self.store.as_ref())
    }

    /// A snapshot of `version` changed the store's progress to `progress`.
    /// New progress shows at once; cleared progress stays until
    /// [`Self::release`] sees the order cover `version`.
    pub(super) fn follow_store(&mut self, progress: Option<&GridProgress>, version: u64) {
        match progress {
            Some(progress) => {
                if self.store.as_ref() != Some(progress) {
                    self.store = Some(progress.clone());
                }
                self.cleared_at = None;
            }
            None if self.store.is_some() && self.cleared_at.is_none() => {
                self.cleared_at = Some(version);
            }
            None => {}
        }
    }

    /// The order on screen covers `coverage`. Returns true if a cleared
    /// store progress left with it.
    pub(super) fn release(&mut self, coverage: u64) -> bool {
        match self.cleared_at {
            Some(version) if coverage >= version => {
                self.store = None;
                self.cleared_at = None;
                true
            }
            _ => false,
        }
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Shows the view's own progress over the store's; `None` shows the
    /// store's again. For work the view waits for itself; a producer writes
    /// `GridWriter::set_progress` instead, in order with its data.
    pub fn set_progress(&mut self, progress: Option<GridProgress>, cx: &mut Context<Self>) {
        if self.progress.view != progress {
            self.progress.view = progress;
            cx.notify();
        }
    }

    /// The progress shown now: the view's ([`Self::set_progress`]), else the
    /// store's. Progress the store cleared stays shown until the order on
    /// screen reflects every write before the clearing, so the bar does not
    /// leave before the new rows show.
    pub fn progress(&self) -> Option<&GridProgress> {
        self.progress.shown()
    }

    /// The overlay of the shown progress, from the delegate's
    /// `render_progress`: called in `render`, only while progress is shown.
    /// Absolute over the body, below the header.
    pub(super) fn render_progress_overlay(
        &self,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<impl IntoElement + use<D>> {
        let progress = self.progress()?;
        let context = ProgressContext {
            empty: self.projection.row_count() == 0,
        };
        let overlay = self
            .delegate
            .render_progress(progress, &context, window, cx);
        Some(
            div()
                .id(OVERLAY_ID)
                .test_support()
                .absolute()
                // Below the group panel too.
                .top(self.body_top())
                .left_0()
                .right_0()
                .bottom_0()
                .child(overlay),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{GridProgress, ProgressSlot, progress_label};

    #[test]
    fn test_progress_slot() {
        let loading = GridProgress::indeterminate().with_label("Loading trades");
        let mut slot = ProgressSlot::new(None);
        assert_eq!(slot.shown(), None);

        // The store's progress shows at once.
        slot.follow_store(Some(&loading), 3);
        assert_eq!(slot.shown(), Some(&loading));
        // Cleared at version 5: shown until the order covers 5.
        slot.follow_store(None, 5);
        assert!(!slot.release(4));
        assert_eq!(slot.shown(), Some(&loading));
        assert!(slot.release(5));
        assert_eq!(slot.shown(), None);
        assert!(!slot.release(6));

        // New progress replaces a cleared one waiting for the order.
        let half = GridProgress::new(0.5);
        slot.follow_store(Some(&loading), 7);
        slot.follow_store(None, 8);
        slot.follow_store(Some(&half), 9);
        assert!(!slot.release(9));
        assert_eq!(slot.shown(), Some(&half));

        // The view's progress wins over the store's.
        slot.view = Some(loading.clone());
        assert_eq!(slot.shown(), Some(&loading));
        slot.view = None;
        assert_eq!(slot.shown(), Some(&half));

        assert_eq!(ProgressSlot::new(Some(&half)).shown(), Some(&half));
        assert_eq!(progress_label(&loading), "Loading trades");
        assert_eq!(
            &*progress_label(&half),
            &*rust_i18n::t!("live_grid.progress.loading")
        );
    }
}
