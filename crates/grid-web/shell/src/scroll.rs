//! Whether the example can scroll further, for an embedded page: a wheel
//! that would move nothing in the example scrolls the page around it
//! instead ([`scroll_room`](crate::scroll_room)).

use gpui_kit_grid::{LiveGridDelegate, LiveGridState};

/// Which ways the example's rows can still scroll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollRoom {
    up: bool,
    down: bool,
}

impl ScrollRoom {
    /// Room both ways: every wheel over the example is the example's. The
    /// default for an example that does not say.
    pub const BOTH_WAYS: Self = Self {
        up: true,
        down: true,
    };

    /// The room of `grid`'s rows. Within one row of either end the grid
    /// counts as there: its first or last row shows, if not whole.
    pub fn of_grid<D: LiveGridDelegate>(grid: &LiveGridState<D>) -> Self {
        let rows = grid.visible_rows();
        Self {
            up: rows.start > 0,
            down: rows.end < grid.projection().len(),
        }
    }

    /// The rows can scroll up (the wheel turned away from the reader).
    pub fn has_room_up(&self) -> bool {
        self.up
    }

    /// The rows can scroll down.
    pub fn has_room_down(&self) -> bool {
        self.down
    }

    /// As the page reads it: 1 for room up, 2 for room down, 3 for both.
    pub(crate) fn bits(self) -> u32 {
        u32::from(self.up) | (u32::from(self.down) << 1)
    }
}
