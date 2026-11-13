//! The text style of drag ghosts: a header's, a group level's, a column
//! chooser row's and the rows'.
//!
//! GPUI draws the view of the active drag outside the window's root
//! element, as it does a tooltip, so a ghost inherits no text style from
//! where its drag started. Its text would take GPUI's default
//! `.SystemUIFont` instead of the grid's font, and on GPUI's web platform,
//! where only the faces a page loads exist, laying it out panics. Each ghost
//! therefore carries the text style of its source, captured while the
//! source renders, and sets it on its root.

use gpui_kit::{Div, Font, Pixels, Styled as _, Window, div};

/// The font and size a drag ghost draws its text in: those of the element
/// its drag starts from.
#[derive(Clone)]
pub(crate) struct GhostTextStyle {
    font: Font,
    size: Pixels,
}

impl GhostTextStyle {
    /// The text style at this point of the element tree. Captured while the
    /// drag's source renders, it is the style the source inherits: the font
    /// of the element the application placed the grid in, as for the cells.
    pub(crate) fn capture(window: &Window) -> Self {
        let style = window.text_style();
        Self {
            font: style.font(),
            size: style.font_size.to_pixels(window.rem_size()),
        }
    }

    /// The root of a ghost's view: its content inherits this style, as it
    /// does in place.
    pub(crate) fn root(&self) -> Div {
        div().font(self.font.clone()).text_size(self.size)
    }
}
