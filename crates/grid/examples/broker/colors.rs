//! The price colour convention: which colour a rise takes and which a fall.
//! "Green up, red down" is the convention of most Western markets; "red
//! up, green down" that of several East Asian ones.
//!
//! It is a setting, not a constant: `PriceColors` is the one place that
//! decides. The grids' text (`cell_style`) and change paint
//! (`PriceChanges`), the price headline, the chart, the account's P&L and
//! the ticket's Buy and Sell all ask it, so switching it (the top bar's
//! control or a key) repaints everything at once.

use std::sync::Arc;

use gpui_kit::component::Theme;
use gpui_kit::{App, Global, Hsla};
use gpui_kit_grid::{
    CellChange, CellStyle, ChangeDirection, ChangePaint, ChangePainter, ChangePalette, RowChange,
};

use crate::trading::paint::TINT;

/// Which colour a rise takes: the app's setting, a GPUI global.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PriceColors {
    /// A rise in green, a fall in red.
    #[default]
    GreenUp,
    /// A rise in red, a fall in green.
    RedUp,
}

impl Global for PriceColors {}

impl PriceColors {
    pub const ALL: [Self; 2] = [Self::GreenUp, Self::RedUp];

    /// The control's word for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::GreenUp => "Green up",
            Self::RedUp => "Red up",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// The convention in force; green up until one is set.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    /// Makes this the convention in force. The caller repaints: a new
    /// change paint for each grid, and the windows.
    pub fn install(self, cx: &mut App) {
        cx.set_global(self);
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::GreenUp => Self::RedUp,
            Self::RedUp => Self::GreenUp,
        }
    }

    /// Of a `green` and a `red`, the one a rise takes, then the one a fall
    /// takes.
    pub fn pick<T>(self, green: T, red: T) -> (T, T) {
        match self {
            Self::GreenUp => (green, red),
            Self::RedUp => (red, green),
        }
    }

    /// The colour of a rise: the theme's `base.green` or `base.red`.
    pub fn rise(self, theme: &Theme) -> Hsla {
        self.pick(theme.green, theme.red).0
    }

    /// The colour of a fall.
    pub fn fall(self, theme: &Theme) -> Hsla {
        self.pick(theme.green, theme.red).1
    }

    /// The colour of a signed change: a rise's, a fall's, or the muted
    /// text of no change.
    pub fn of(self, change: f64, theme: &Theme) -> Hsla {
        if change > 0. {
            self.rise(theme)
        } else if change < 0. {
            self.fall(theme)
        } else {
            theme.muted_foreground
        }
    }

    /// The grids' change paint in this convention.
    pub fn change_paint(self) -> ChangePaint {
        ChangePaint::Custom(Arc::new(PriceChanges(self)))
    }
}

/// Paints a change as the trading examples' `QuietChanges` does, a faint
/// tint and the arrow, in the convention's colours. The palette's `up` is
/// the theme's `success` green and its `down` the `danger` red (the theme
/// files keep them equal to `base.green` and `base.red`); the convention
/// decides which a rise takes.
#[derive(Clone, Copy, Debug)]
pub struct PriceChanges(PriceColors);

impl ChangePainter for PriceChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        let (rise, fall) = self.0.pick(palette.up(), palette.down());
        let color = match (change.change(), change.direction()) {
            (RowChange::Removed, _) => {
                style.set_foreground(change.fade(palette.removed_foreground()));
                return;
            }
            (RowChange::Inserted, _) => palette.inserted(),
            (_, Some(ChangeDirection::Up)) => rise,
            (_, Some(ChangeDirection::Down)) => fall,
            (_, None) => palette.updated(),
        };
        style.set_background(color.opacity(TINT * change.intensity()));
        style.set_trend(change.direction());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_the_convention_decides_the_colour_of_a_rise() {
        assert_eq!(PriceColors::GreenUp.pick("green", "red"), ("green", "red"));
        assert_eq!(PriceColors::RedUp.pick("green", "red"), ("red", "green"));
        for colors in PriceColors::ALL {
            assert_eq!(colors.toggled().toggled(), colors);
            assert_ne!(colors.toggled(), colors);
            assert_eq!(PriceColors::ALL[colors.ix()], colors);
        }
        assert_eq!(PriceColors::default(), PriceColors::GreenUp);
    }
}
