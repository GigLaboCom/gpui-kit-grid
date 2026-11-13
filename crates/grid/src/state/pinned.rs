//! Pinned rows on screen: where the bands sit in the body, what the last
//! frame painted in them, and their hit tests.
//!
//! The rows live in the store (`GridWriter::set_pinned_rows`), one band per
//! edge, each in a key space of its own. The body splits top to bottom into
//! the top band, the data rows and the bottom band. The top band takes
//! `min(lines × row height, body)`, the bottom band `min(lines × row height,
//! body − top band)`, and the data rows the rest, possibly nothing: the top
//! band wins when the body is too short (one place to change). Bands do not
//! scroll vertically; their columns scroll sideways with the data rows', in
//! the same column zones.
//!
//! A band's lines are its application rows and, in the band of
//! `LiveGridState::with_totals`, the totals row as its outermost line: first
//! in the top band, last in the bottom one. Each band is laid out from the
//! body's edge, so a band cut short loses the lines next to the data rows
//! and keeps the totals at the edge.

use std::{hash::Hash, sync::Arc};

use gpui_kit::{Bounds, Pixels, Point, point, px, size};

use super::{KeyOf, LiveGridDelegate, LiveGridState, key_hash};
use crate::{model::GridRow, store::PinnedEdge};

/// Both edges, top first: the order bands are planned in.
pub(super) const EDGES: [PinnedEdge; 2] = [PinnedEdge::Top, PinnedEdge::Bottom];

/// The slot of an edge in per-band arrays.
pub(super) const fn band_slot(edge: PinnedEdge) -> usize {
    match edge {
        PinnedEdge::Top => 0,
        PinnedEdge::Bottom => 1,
    }
}

/// The name of a band in element ids; the band's accessibility rows use
/// the same name.
pub(crate) const fn band_name(edge: PinnedEdge) -> &'static str {
    match edge {
        PinnedEdge::Top => "live-grid-pinned-top",
        PinnedEdge::Bottom => "live-grid-pinned-bottom",
    }
}

/// The hash of a pinned row's key for element ids, such as the tooltip div
/// of a hovered pinned cell. A band's keys are its own, so the band's name
/// is hashed with the key: a pinned row never gets the id of a data row or
/// of the other band's row with the same key. (Not `(edge, key)`:
/// `FxHasher` turns the zero discriminant of `PinnedEdge::Top` into
/// nothing, so the top band would hash as the data rows.)
pub(crate) fn pinned_key_hash<K: Hash + ?Sized>(edge: PinnedEdge, key: &K) -> u64 {
    key_hash(&(band_name(edge), key))
}

/// The body split into the top band, the data rows and the bottom band, in
/// window coordinates, and the lines of each band.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BodyBands {
    top: Bounds<Pixels>,
    rows: Bounds<Pixels>,
    bottom: Bounds<Pixels>,
    row_height: Pixels,
    /// The application rows of each band, by band slot.
    pinned: [usize; 2],
    /// The band with the totals row.
    totals: Option<PinnedEdge>,
}

impl BodyBands {
    /// Splits `body` for `top` and `bottom` pinned rows of `row_height`,
    /// and the totals row in the band at `totals`.
    pub(super) fn new(
        body: Bounds<Pixels>,
        row_height: Pixels,
        top: usize,
        bottom: usize,
        totals: Option<PinnedEdge>,
    ) -> Self {
        let mut bands = Self {
            row_height: row_height.max(px(0.)),
            pinned: [top, bottom],
            totals,
            ..Self::default()
        };
        let height = body.size.height.max(px(0.));
        let line_height = bands.row_height;
        let top_height = (line_height * bands.lines(PinnedEdge::Top) as f32).min(height);
        let bottom_height =
            (line_height * bands.lines(PinnedEdge::Bottom) as f32).min(height - top_height);
        let (x, y, width) = (body.origin.x, body.origin.y, body.size.width);
        bands.top = Bounds::new(point(x, y), size(width, top_height));
        bands.rows = Bounds::new(
            point(x, y + top_height),
            size(width, height - top_height - bottom_height),
        );
        bands.bottom = Bounds::new(
            point(x, y + height - bottom_height),
            size(width, bottom_height),
        );
        bands
    }

    /// The data rows' area: the body without the bands.
    pub(crate) fn rows(&self) -> Bounds<Pixels> {
        self.rows
    }

    /// The area of the band at `edge` (zero high without lines).
    pub(crate) fn band(&self, edge: PinnedEdge) -> Bounds<Pixels> {
        match edge {
            PinnedEdge::Top => self.top,
            PinnedEdge::Bottom => self.bottom,
        }
    }

    /// The lines of the band at `edge`: its application rows, and the
    /// totals row if it is in this band.
    pub(super) fn lines(&self, edge: PinnedEdge) -> usize {
        self.pinned[band_slot(edge)] + usize::from(self.totals == Some(edge))
    }

    /// What line `line` of the band at `edge` holds: the totals row as the
    /// outermost line, or an application row by its index in the band.
    pub(super) fn band_line(&self, edge: PinnedEdge, line: usize) -> BodyLine {
        match (self.totals == Some(edge), edge) {
            (false, _) => BodyLine::Pinned(edge, line),
            (true, PinnedEdge::Top) => match line.checked_sub(1) {
                Some(ix) => BodyLine::Pinned(edge, ix),
                None => BodyLine::Totals(edge),
            },
            (true, PinnedEdge::Bottom) if line + 1 == self.lines(edge) => BodyLine::Totals(edge),
            (true, PinnedEdge::Bottom) => BodyLine::Pinned(edge, line),
        }
    }

    /// The top edge of line `line` of the band at `edge` in window
    /// coordinates, if any of it shows: the top band is laid out from its
    /// top edge, the bottom band from its bottom edge.
    pub(super) fn line_y(&self, edge: PinnedEdge, line: usize) -> Option<Pixels> {
        let band = self.band(edge);
        let y = match edge {
            PinnedEdge::Top => band.origin.y + self.row_height * line as f32,
            PinnedEdge::Bottom => {
                band.bottom() - self.row_height * (self.lines(edge).checked_sub(line)?) as f32
            }
        };
        (y < band.bottom() && y + self.row_height > band.origin.y).then_some(y)
    }

    /// The lines of the band at `edge` that show, top to bottom, with their
    /// top edges.
    pub(super) fn visible_lines(
        &self,
        edge: PinnedEdge,
    ) -> impl Iterator<Item = (BodyLine, Pixels)> + '_ {
        (0..self.lines(edge))
            .filter_map(move |line| Some((self.band_line(edge, line), self.line_y(edge, line)?)))
    }

    /// The line of the band at `edge` under window y `y`, if `y` is in the
    /// band.
    fn line_at(&self, edge: PinnedEdge, y: Pixels) -> Option<BodyLine> {
        let band = self.band(edge);
        if y < band.origin.y || y >= band.bottom() || self.row_height <= px(0.) {
            return None;
        }
        let line = match edge {
            PinnedEdge::Top => ((y - band.origin.y) / self.row_height).floor() as usize,
            PinnedEdge::Bottom => {
                let from_edge = ((band.bottom() - y) / self.row_height).ceil() as usize;
                self.lines(edge).checked_sub(from_edge.max(1))?
            }
        };
        (line < self.lines(edge)).then(|| self.band_line(edge, line))
    }
}

/// A line of the body: a data row's screen line from the top of the data
/// rows, a pinned row's index in its band, or a band's totals row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BodyLine {
    Data(usize),
    Pinned(PinnedEdge, usize),
    /// The totals row.
    Totals(PinnedEdge),
}

/// The pinned rows the last frame painted, band by band, top to bottom. Hit
/// tests and [`LiveGridState::pinned_keys`] read what is on screen, as they
/// read `painted` for the data rows. Cleared and refilled every frame; the
/// vectors keep their capacity, so a frame does not allocate for them.
pub(super) struct PaintedBands<R> {
    rows: [Vec<Arc<R>>; 2],
    /// The band index of each band's first painted row: a bottom band cut
    /// short paints its last rows.
    first: [usize; 2],
}

impl<R> Default for PaintedBands<R> {
    fn default() -> Self {
        Self {
            rows: [Vec::new(), Vec::new()],
            first: [0; 2],
        }
    }
}

impl<R> PaintedBands<R> {
    /// The rows painted at `edge`.
    pub(super) fn rows(&self, edge: PinnedEdge) -> &[Arc<R>] {
        &self.rows[band_slot(edge)]
    }

    /// The painted row of index `ix` in the band at `edge`.
    fn row(&self, edge: PinnedEdge, ix: usize) -> Option<&Arc<R>> {
        let slot = band_slot(edge);
        self.rows[slot].get(ix.checked_sub(self.first[slot])?)
    }

    /// Empties the band at `edge` for a new frame.
    pub(super) fn begin(&mut self, edge: PinnedEdge) {
        let slot = band_slot(edge);
        self.rows[slot].clear();
        self.first[slot] = 0;
    }

    /// The frame painted the row of index `ix` in the band at `edge`; rows
    /// come top to bottom, without gaps.
    pub(super) fn push(&mut self, edge: PinnedEdge, ix: usize, row: Arc<R>) {
        let slot = band_slot(edge);
        if self.rows[slot].is_empty() {
            self.first[slot] = ix;
        }
        debug_assert_eq!(self.first[slot] + self.rows[slot].len(), ix, "no gaps");
        self.rows[slot].push(row);
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Keys of the rows pinned at `edge` that the last frame painted, top
    /// to bottom. A band taller than the body is cut next to the data rows,
    /// so the last rows of the top band, or the first rows of the bottom
    /// band, may be missing.
    pub fn pinned_keys(&self, edge: PinnedEdge) -> Vec<KeyOf<D>> {
        self.painted_pinned
            .rows(edge)
            .iter()
            .map(|row| row.key().clone())
            .collect()
    }

    /// The body split into bands for the bands of the current snapshot and
    /// the totals row. O(1).
    pub(crate) fn body_bands(&self) -> BodyBands {
        BodyBands::new(
            self.body_bounds,
            self.row_height,
            self.snapshot.pinned_rows(PinnedEdge::Top).len(),
            self.snapshot.pinned_rows(PinnedEdge::Bottom).len(),
            self.totals,
        )
    }

    /// The data rows' area of the body, in window coordinates.
    pub(super) fn rows_bounds(&self) -> Bounds<Pixels> {
        self.body_bands().rows()
    }

    /// The line of the body under a window position: a band's line first,
    /// then a data row's screen line.
    pub(super) fn body_line_at(&self, position: Point<Pixels>) -> Option<BodyLine> {
        let bands = self.body_bands();
        for edge in EDGES {
            if bands.band(edge).contains(&position) {
                return bands.line_at(edge, position.y);
            }
        }
        self.line_at(position).map(BodyLine::Data)
    }

    /// The painted pinned row under a window position: its band and key.
    pub(super) fn pinned_row_at(&self, position: Point<Pixels>) -> Option<(PinnedEdge, KeyOf<D>)> {
        let Some(BodyLine::Pinned(edge, ix)) = self.body_line_at(position) else {
            return None;
        };
        let row = self.painted_pinned.row(edge, ix)?;
        Some((edge, row.key().clone()))
    }

    /// Returns true if the band at `edge` of the current snapshot holds
    /// `key`: a menu on a pinned row closes when its row leaves the band.
    pub(super) fn is_pinned_key(&self, edge: PinnedEdge, key: &KeyOf<D>) -> bool {
        self.snapshot
            .pinned_rows(edge)
            .iter()
            .any(|row| row.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, point, px, size};

    use super::{BodyBands, BodyLine, band_slot, pinned_key_hash};
    use crate::{state::key_hash, store::PinnedEdge};

    #[test]
    fn test_pinned_key_hash() {
        for key in [0u32, 1, 42] {
            let top = pinned_key_hash(PinnedEdge::Top, &key);
            let bottom = pinned_key_hash(PinnedEdge::Bottom, &key);
            assert_ne!(top, key_hash(&key), "{key}");
            assert_ne!(bottom, key_hash(&key), "{key}");
            assert_ne!(top, bottom, "{key}");
        }
        assert_eq!(
            pinned_key_hash(PinnedEdge::Top, "total"),
            pinned_key_hash(PinnedEdge::Top, &String::from("total"))
        );
        // What the band name avoids: `FxHasher` hashes the top edge to
        // nothing.
        assert_eq!(key_hash(&(PinnedEdge::Top, 7u32)), key_hash(&7u32));
    }

    #[test]
    fn test_body_bands_overflow_rule() {
        let body = Bounds::new(point(px(10.), px(40.)), size(px(300.), px(260.)));
        let row = px(26.);

        // No pinned rows: the data rows take the whole body.
        let bands = BodyBands::new(body, row, 0, 0, None);
        assert_eq!(bands.rows(), body);
        assert_eq!(bands.band(PinnedEdge::Top).size.height, px(0.));
        assert_eq!(bands.band(PinnedEdge::Bottom).size.height, px(0.));
        assert_eq!(bands.band(PinnedEdge::Bottom).origin.y, px(300.));

        // One row each: the bands at the edges, the data rows between.
        let bands = BodyBands::new(body, row, 1, 1, None);
        assert_eq!(
            bands.band(PinnedEdge::Top),
            Bounds::new(point(px(10.), px(40.)), size(px(300.), px(26.)))
        );
        assert_eq!(
            bands.rows(),
            Bounds::new(point(px(10.), px(66.)), size(px(300.), px(208.)))
        );
        assert_eq!(
            bands.band(PinnedEdge::Bottom),
            Bounds::new(point(px(10.), px(274.)), size(px(300.), px(26.)))
        );

        // Too many rows: the top band first, then the bottom band, then
        // the data rows get what is left, possibly nothing.
        let bands = BodyBands::new(body, row, 6, 6, None);
        assert_eq!(bands.band(PinnedEdge::Top).size.height, px(156.));
        assert_eq!(bands.band(PinnedEdge::Bottom).size.height, px(104.));
        assert_eq!(bands.rows().size.height, px(0.));
        let bands = BodyBands::new(body, row, 20, 1, None);
        assert_eq!(bands.band(PinnedEdge::Top).size.height, px(260.));
        assert_eq!(bands.band(PinnedEdge::Bottom).size.height, px(0.));
        assert_eq!(bands.rows().size.height, px(0.));

        // Before the first frame the body is empty.
        let bands = BodyBands::new(Bounds::default(), row, 2, 1, None);
        assert_eq!(bands.rows().size.height, px(0.));
        assert_eq!(bands.band(PinnedEdge::Top).size.height, px(0.));

        assert_eq!(band_slot(PinnedEdge::Top), 0);
        assert_eq!(band_slot(PinnedEdge::Bottom), 1);
    }

    #[test]
    fn test_body_bands_with_totals() {
        let body = Bounds::new(point(px(10.), px(40.)), size(px(300.), px(260.)));
        let row = px(26.);
        let lines = |bands: &BodyBands, edge| -> Vec<(BodyLine, f32)> {
            bands
                .visible_lines(edge)
                .map(|(line, y)| (line, y.as_f32()))
                .collect()
        };
        use BodyLine::{Pinned, Totals};
        use PinnedEdge::{Bottom, Top};

        // Totals at the bottom: the band is one line higher, the totals
        // last, at the body's bottom edge.
        let bands = BodyBands::new(body, row, 1, 1, Some(Bottom));
        assert_eq!(bands.band(Bottom).size.height, px(52.));
        assert_eq!(bands.rows().size.height, px(182.));
        assert_eq!(
            lines(&bands, Bottom),
            [(Pinned(Bottom, 0), 248.), (Totals(Bottom), 274.)]
        );
        assert_eq!(lines(&bands, Top), [(Pinned(Top, 0), 40.)]);
        // Without application rows the band holds the totals alone.
        let bands = BodyBands::new(body, row, 0, 0, Some(Bottom));
        assert_eq!(lines(&bands, Bottom), [(Totals(Bottom), 274.)]);

        // Totals at the top: the first line, under the header.
        let bands = BodyBands::new(body, row, 2, 0, Some(Top));
        assert_eq!(bands.band(Top).size.height, px(78.));
        assert_eq!(
            lines(&bands, Top),
            [
                (Totals(Top), 40.),
                (Pinned(Top, 0), 66.),
                (Pinned(Top, 1), 92.)
            ]
        );
        assert_eq!(bands.line_at(Top, px(40.)), Some(Totals(Top)));
        assert_eq!(bands.line_at(Top, px(91.)), Some(Pinned(Top, 0)));
        assert_eq!(bands.line_at(Top, px(118.)), None);

        // A bottom band cut short loses the lines next to the data rows:
        // 8 lines on top leave 52 px, two lines laid out from the body's
        // bottom edge.
        let bands = BodyBands::new(body, row, 8, 3, Some(Bottom));
        assert_eq!(bands.band(Bottom).size.height, px(52.));
        assert_eq!(
            lines(&bands, Bottom),
            [(Pinned(Bottom, 2), 248.), (Totals(Bottom), 274.)]
        );
        assert_eq!(bands.line_at(Bottom, px(299.)), Some(Totals(Bottom)));
        assert_eq!(bands.line_at(Bottom, px(274.)), Some(Totals(Bottom)));
        assert_eq!(bands.line_at(Bottom, px(273.)), Some(Pinned(Bottom, 2)));
        assert_eq!(bands.line_at(Bottom, px(300.)), None);
        // A line partly shown: half of row 2 above the totals; rows 0 and 1
        // are cut.
        let short = Bounds::new(body.origin, size(px(300.), px(26. * 3. + 13.)));
        let bands = BodyBands::new(short, row, 2, 3, Some(Bottom));
        assert_eq!(bands.band(Top).size.height, px(52.));
        assert_eq!(bands.band(Bottom).size.height, px(39.));
        assert_eq!(
            lines(&bands, Bottom),
            [(Pinned(Bottom, 2), 79.), (Totals(Bottom), 105.)]
        );
        assert_eq!(bands.line_at(Bottom, px(92.)), Some(Pinned(Bottom, 2)));
        // A top band cut short loses its last lines, as without totals; the
        // totals stay under the header.
        let bands = BodyBands::new(short, row, 5, 0, Some(Top));
        assert_eq!(bands.rows().size.height, px(0.));
        assert_eq!(
            lines(&bands, Top),
            [
                (Totals(Top), 40.),
                (Pinned(Top, 0), 66.),
                (Pinned(Top, 1), 92.),
                (Pinned(Top, 2), 118.)
            ]
        );
    }
}
