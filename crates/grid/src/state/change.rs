//! How a highlighted change paints a cell: the `ChangePaint` seam. The
//! frame paints a hot cell's default style with it, then the delegate's
//! `cell_style` adjusts the result.

use std::{fmt, sync::Arc};

use gpui_kit::{Context, Hsla, component::Theme};

use super::{CellChange, CellStyle, LiveGridDelegate, LiveGridState};
use crate::{
    highlight::{ChangeDirection, HighlightPolicy, RowChange, RowStamps, Stamp},
    model::ColumnIx,
};

/// The opacity of a change's background at full intensity: the highlight
/// tints the row's own background instead of hiding it.
const CHANGE_OPACITY: f32 = 0.45;

/// The theme colours a [`ChangePainter`] works with, read once per frame.
/// No colours of its own: semantic tokens of the theme.
#[derive(Clone, Copy, Debug)]
pub struct ChangePalette {
    inserted: Hsla,
    updated: Hsla,
    up: Hsla,
    down: Hsla,
    removed: Hsla,
    removed_foreground: Hsla,
}

impl ChangePalette {
    /// The palette of a theme.
    pub(super) fn new(theme: &Theme) -> Self {
        Self {
            inserted: theme.success,
            updated: theme.warning,
            up: theme.success,
            down: theme.danger,
            removed: theme.danger,
            removed_foreground: theme.muted_foreground,
        }
    }

    /// An inserted row: the theme's `success`.
    pub fn inserted(&self) -> Hsla {
        self.inserted
    }

    /// An updated cell: the theme's `warning`.
    pub fn updated(&self) -> Hsla {
        self.updated
    }

    /// A value that rose: the theme's `success`.
    pub fn up(&self) -> Hsla {
        self.up
    }

    /// A value that fell: the theme's `danger`.
    pub fn down(&self) -> Hsla {
        self.down
    }

    /// A removed row while it fades out: the theme's `danger`.
    pub fn removed(&self) -> Hsla {
        self.removed
    }

    /// The text of a removed row while it fades out: the theme's
    /// `muted_foreground`.
    pub fn removed_foreground(&self) -> Hsla {
        self.removed_foreground
    }
}

/// Paints a highlighted change into a cell's default style, before the
/// delegate's `cell_style` adjusts it.
///
/// Called in prepaint for every hot visible cell, every frame: it must not
/// allocate or lock. [`CellChange::column_ix`] tells which column the cell is
/// in, so a painter can flash some columns and not others:
///
/// ```
/// use gpui_kit_grid::{
///     CellChange, CellStyle, ChangePainter, ChangePalette, ColumnIx, RowChange, UniformChanges,
/// };
///
/// /// Stage 1's colours on the price only; inserts and removals whole.
/// struct PriceOnly(ColumnIx);
///
/// impl ChangePainter for PriceOnly {
///     fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
///         if change.change() != RowChange::Updated || change.column_ix() == Some(self.0) {
///             UniformChanges.paint(change, palette, style);
///         }
///     }
/// }
/// ```
pub trait ChangePainter: 'static {
    /// Paints `change` into `style`.
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle);
}

/// The background of a change: `color` at the change's opacity.
fn change_background(change: &CellChange, color: Hsla) -> Hsla {
    color.opacity(CHANGE_OPACITY * change.intensity())
}

/// The highlighted change a stamp shows at the global tick `tick`, while
/// it is hot: a data cell's, an aggregate cell's or a caption's.
#[inline]
pub(super) fn stamp_change(
    stamp: Stamp,
    column: Option<ColumnIx>,
    policy: &HighlightPolicy,
    tick: u32,
) -> Option<CellChange> {
    policy.intensity(stamp, tick).map(|intensity| CellChange {
        intensity,
        change: stamp.change(),
        direction: stamp.direction(),
        column_ix: column,
    })
}

/// Where the cells of a planned row take their highlighted changes from.
#[derive(Clone, Copy, Debug)]
pub(super) enum RowChanges<'a> {
    /// A live row: its cells' stamps, if any cell is hot.
    Stamps(Option<RowStamps<'a>>),
    /// A removed row while it fades out: every cell shows its removal.
    Removed(CellChange),
}

impl RowChanges<'_> {
    /// The highlighted change of the cell of `column` at the global tick
    /// `tick`, if it is hot.
    #[inline]
    pub(super) fn cell(
        self,
        column: ColumnIx,
        policy: &HighlightPolicy,
        tick: u32,
    ) -> Option<CellChange> {
        match self {
            Self::Stamps(stamps) => stamps
                .and_then(|stamps| stamp_change(stamps.get(column), Some(column), policy, tick)),
            Self::Removed(change) => Some(CellChange {
                column_ix: Some(column),
                ..change
            }),
        }
    }
}

/// Paints a change by its kind alone: an inserted row in `success`, an
/// updated cell in `warning`, a removed row in `danger` with muted text, as
/// a background whose opacity fades with the change.
/// The text colour stays, so the text cache is not split by fade step. The
/// default.
#[derive(Clone, Copy, Debug, Default)]
pub struct UniformChanges;

impl ChangePainter for UniformChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        match change.change() {
            RowChange::Inserted => {
                style.set_background(change_background(change, palette.inserted()));
            }
            RowChange::Updated => {
                style.set_background(change_background(change, palette.updated()));
            }
            RowChange::Removed => {
                style.set_background(change_background(change, palette.removed()));
                style.set_foreground(palette.removed_foreground());
            }
        }
    }
}

/// Paints the direction of an updated number: a rise in `success`, a fall
/// in `danger`, each with an arrow
/// ([`CellStyle::set_trend`]); an update without a direction in `warning`
/// without one. Inserted and removed rows as [`UniformChanges`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DirectionalChanges;

impl ChangePainter for DirectionalChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        match (change.change(), change.direction()) {
            (RowChange::Updated, Some(direction)) => {
                let color = match direction {
                    ChangeDirection::Up => palette.up(),
                    ChangeDirection::Down => palette.down(),
                };
                style.set_background(change_background(change, color));
                style.set_trend(Some(direction));
            }
            _ => UniformChanges.paint(change, palette, style),
        }
    }
}

/// How a grid paints highlighted changes: the built-in painters,
/// dispatched without a virtual call on the hot path, or the
/// application's. The default is [`UniformChanges`].
#[derive(Clone)]
#[non_exhaustive]
pub enum ChangePaint {
    /// [`UniformChanges`]: one colour per kind of change.
    Uniform(UniformChanges),
    /// [`DirectionalChanges`]: rise and fall, with arrows.
    Directional(DirectionalChanges),
    /// The application's painter.
    Custom(Arc<dyn ChangePainter>),
}

impl Default for ChangePaint {
    fn default() -> Self {
        Self::Uniform(UniformChanges)
    }
}

impl fmt::Debug for ChangePaint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uniform(painter) => f.debug_tuple("Uniform").field(painter).finish(),
            Self::Directional(painter) => f.debug_tuple("Directional").field(painter).finish(),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

impl ChangePainter for ChangePaint {
    #[inline]
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        match self {
            Self::Uniform(painter) => painter.paint(change, palette, style),
            Self::Directional(painter) => painter.paint(change, palette, style),
            Self::Custom(painter) => painter.paint(change, palette, style),
        }
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets how highlighted changes paint cells (default
    /// [`ChangePaint::Uniform`]).
    pub fn with_change_paint(mut self, paint: ChangePaint) -> Self {
        self.change_paint = paint;
        self
    }

    /// Replaces how highlighted changes paint cells, as a "Show direction"
    /// switch does. The next frame paints with it; nothing is rebuilt.
    pub fn set_change_paint(&mut self, paint: ChangePaint, cx: &mut Context<Self>) {
        self.change_paint = paint;
        cx.notify();
    }

    /// How highlighted changes paint cells.
    pub fn change_paint(&self) -> &ChangePaint {
        &self.change_paint
    }
}
