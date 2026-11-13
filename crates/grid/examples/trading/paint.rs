//! A quiet change paint for dense trading screens.
//!
//! The grid's `DirectionalChanges` lays a change's colour under the cell at
//! up to 45 % opacity. On a desk where Chg is green or red, Bid green and
//! Ask red, that is coloured text on a flash of its own colour, and a
//! screen of a few hundred updates a second is mostly flash. `QuietChanges`
//! keeps the arrow and only tints: the text keeps the colour the delegate
//! gives it, readable in every theme.

use std::sync::Arc;

use gpui_kit_grid::{
    CellChange, CellStyle, ChangeDirection, ChangePaint, ChangePainter, ChangePalette, RowChange,
};

/// The opacity of a change's tint at full intensity; it fades with the
/// change. Low enough that text in the change's own colour stays legible
/// over it on the dark themes.
pub const TINT: f32 = 0.16;

/// Paints changes as a faint tint of the theme's colours: a rise in
/// `success` and a fall in `danger`, each with its arrow; an update without
/// a direction in `warning`, a new row in `success`; a removed row's text
/// fading out, muted. Never sets the text colour of a live row.
#[derive(Clone, Copy, Debug, Default)]
pub struct QuietChanges;

impl ChangePainter for QuietChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        let color = match (change.change(), change.direction()) {
            (RowChange::Removed, _) => {
                style.set_foreground(change.fade(palette.removed_foreground()));
                return;
            }
            (RowChange::Inserted, _) => palette.inserted(),
            (_, Some(ChangeDirection::Up)) => palette.up(),
            (_, Some(ChangeDirection::Down)) => palette.down(),
            (_, None) => palette.updated(),
        };
        style.set_background(color.opacity(TINT * change.intensity()));
        style.set_trend(change.direction());
    }
}

/// [`QuietChanges`] as a grid's change paint.
pub fn quiet_changes() -> ChangePaint {
    ChangePaint::Custom(Arc::new(QuietChanges))
}
