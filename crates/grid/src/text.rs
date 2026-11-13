//! Shaped-text cache and glyph painting for the grid body.
//!
//! GPUI's own line cache keeps two frames and looks up by hash with a linear
//! scan, and `ShapedLine` bakes the colour in. The grid keeps colour-free
//! `Arc<LineLayout>`s in a hash map instead and paints glyphs with the colour
//! of the current frame.

use std::{
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
};

use gpui_kit::{
    Font, FontId, FontWeight, GlyphId, Hsla, LineLayout, Pixels, Point, SharedString, TextRun,
    Window, black, point, px,
};
use rustc_hash::{FxHashMap, FxHasher};

/// Frames an unused entry survives before it is evicted.
const KEEP_FRAMES: u64 = 120;
/// How often (in frames) the cache is swept.
const SWEEP_EVERY: u64 = 60;

struct Entry {
    text: Box<str>,
    layout: Arc<LineLayout>,
    last_frame: u64,
}

/// Characters a number is made of. With tabular figures they have fixed
/// advances and no kerning, so numbers are laid out from a per-font table
/// instead of being shaped.
const NUMERIC: &str = "0123456789.,-+ ";
/// Characters the table takes if the font shapes each to one glyph: the
/// `:` of a time (`2026-11-04 12:34:56`). Without one of them the table
/// still lays out numbers; text with it is shaped.
const NUMERIC_OPTIONAL: &str = ":";

#[derive(Clone, Copy)]
struct NumericGlyph {
    font_id: FontId,
    glyph_id: GlyphId,
    advance: Pixels,
}

struct NumericTable {
    glyphs: [Option<NumericGlyph>; 128],
    ascent: Pixels,
    descent: Pixels,
}

impl NumericTable {
    /// Shapes each numeric character once. `None` if any of [`NUMERIC`]
    /// does not shape to exactly one glyph (then numbers are shaped
    /// normally); a character of [`NUMERIC_OPTIONAL`] that does not is left
    /// out of the table.
    fn build(shape: impl Fn(&str) -> Arc<LineLayout>) -> Option<Self> {
        let mut glyphs = [None; 128];
        let mut metrics = None;
        for ch in NUMERIC.chars() {
            let layout = shape(ch.encode_utf8(&mut [0; 4]));
            glyphs[ch as usize] = Some(Self::glyph(&layout)?);
            metrics.get_or_insert((layout.ascent, layout.descent));
        }
        for ch in NUMERIC_OPTIONAL.chars() {
            glyphs[ch as usize] = Self::glyph(&shape(ch.encode_utf8(&mut [0; 4])));
        }
        let (ascent, descent) = metrics?;
        Some(Self {
            glyphs,
            ascent,
            descent,
        })
    }

    /// The one glyph a character shaped to, if it shaped to exactly one.
    fn glyph(layout: &LineLayout) -> Option<NumericGlyph> {
        let [run] = layout.runs.as_slice() else {
            return None;
        };
        let [glyph] = run.glyphs.as_slice() else {
            return None;
        };
        Some(NumericGlyph {
            font_id: run.font_id,
            glyph_id: glyph.id,
            advance: layout.width,
        })
    }

    /// The layout of `text` if it only has numeric characters. Its glyphs go
    /// to the frame's arena, so a number allocates nothing of its own.
    fn layout(&self, text: &str, font_size: Pixels, arena: &mut GlyphArena) -> Option<NumericLine> {
        if !text
            .bytes()
            .all(|byte| self.glyphs.get(byte as usize).is_some_and(Option::is_some))
        {
            return None;
        }
        let start = arena.len();
        let mut x = px(0.);
        for byte in text.bytes() {
            let glyph = self.glyphs[byte as usize].expect("checked above");
            arena.push((glyph.font_id, glyph.glyph_id, x));
            x += glyph.advance;
        }
        Some(NumericLine {
            glyphs: start..arena.len(),
            width: x,
            ascent: self.ascent,
            descent: self.descent,
            font_size,
        })
    }
}

/// Positioned glyphs of every number in one frame: `(font, glyph, x)`.
pub(crate) type GlyphArena = Vec<(FontId, GlyphId, Pixels)>;

/// A number laid out from the glyph table; its glyphs are a range of the
/// frame's [`GlyphArena`].
pub(crate) struct NumericLine {
    glyphs: Range<usize>,
    width: Pixels,
    ascent: Pixels,
    descent: Pixels,
    font_size: Pixels,
}

/// The text of one cell: shaped and cached, or a number from the glyph table.
pub(crate) enum TextLayout {
    Shaped(Arc<LineLayout>),
    Numeric(NumericLine),
}

impl TextLayout {
    pub fn width(&self) -> Pixels {
        match self {
            Self::Shaped(layout) => layout.width,
            Self::Numeric(line) => line.width,
        }
    }

    /// The height above and below the baseline.
    pub fn ascent_descent(&self) -> (Pixels, Pixels) {
        match self {
            Self::Shaped(layout) => (layout.ascent, layout.descent),
            Self::Numeric(line) => (line.ascent, line.descent),
        }
    }
}

/// Hit and miss counters of the text caches: the grid's font and, apart,
/// each other weight a cell style asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextStats {
    hits: u64,
    misses: u64,
    numeric: u64,
    entries: usize,
    bold_entries: usize,
}

impl TextStats {
    /// Lookups served from the cache.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Lookups not in the cache: text shaped by the platform shaper.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Numbers laid out from the glyph table, without shaping or caching.
    pub fn numeric(&self) -> u64 {
        self.numeric
    }

    /// Entries currently cached, at every weight.
    pub fn entries(&self) -> usize {
        self.entries
    }

    /// Entries currently cached at a weight other than the grid font's
    /// (`CellStyle::set_font_weight`), such as bold totals.
    pub fn bold_entries(&self) -> usize {
        self.bold_entries
    }

    /// Adds the counters of another cache.
    fn add(&mut self, other: TextStats) {
        self.hits += other.hits;
        self.misses += other.misses;
        self.numeric += other.numeric;
        self.entries += other.entries;
        self.bold_entries += other.bold_entries;
    }
}

/// Colour-free shaped lines keyed by text, for one font and size.
pub(crate) struct TextCache {
    entries: FxHashMap<u64, Entry>,
    frame: u64,
    font: Option<Font>,
    font_size: Pixels,
    ellipsis: Option<Arc<LineLayout>>,
    numeric: Option<Option<NumericTable>>,
    stats: TextStats,
}

impl Default for TextCache {
    fn default() -> Self {
        Self {
            entries: FxHashMap::default(),
            frame: 0,
            font: None,
            font_size: px(0.),
            ellipsis: None,
            numeric: None,
            stats: TextStats::default(),
        }
    }
}

impl TextCache {
    /// Starts a frame. A new font or size drops every entry.
    pub fn begin_frame(&mut self, font: &Font, font_size: Pixels) {
        if self.font.as_ref() != Some(font) || self.font_size != font_size {
            self.entries.clear();
            self.ellipsis = None;
            self.numeric = None;
            self.font = Some(font.clone());
            self.font_size = font_size;
        }
        self.frame += 1;
        if self.frame % SWEEP_EVERY == 0 {
            let frame = self.frame;
            self.entries
                .retain(|_, entry| entry.last_frame + KEEP_FRAMES >= frame);
        }
    }

    fn shape(&self, text: &str, window: &Window) -> Arc<LineLayout> {
        let runs = [TextRun {
            len: text.len(),
            font: self.font.clone().expect("begin_frame sets the font"),
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        window
            .text_system()
            .layout_line(text, self.font_size, &runs, None)
    }

    /// The layout of `text`. Numbers put their glyphs into `arena`.
    pub fn layout(&mut self, text: &str, window: &Window, arena: &mut GlyphArena) -> TextLayout {
        // Numbers change on every update and would churn the cache (and its
        // rehashing) during fast scrolling; laying them out from the glyph
        // table is cheaper than caching them.
        if self.numeric.is_none() {
            self.numeric = Some(NumericTable::build(|text| self.shape(text, window)));
        }
        if let Some(layout) = self
            .numeric
            .as_ref()
            .and_then(|table| table.as_ref())
            .and_then(|table| table.layout(text, self.font_size, arena))
        {
            self.stats.numeric += 1;
            return TextLayout::Numeric(layout);
        }

        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        let key = hasher.finish();
        if let Some(entry) = self.entries.get_mut(&key)
            && &*entry.text == text
        {
            entry.last_frame = self.frame;
            self.stats.hits += 1;
            return TextLayout::Shaped(entry.layout.clone());
        }
        self.stats.misses += 1;
        let layout = self.shape(text, window);
        self.entries.insert(
            key,
            Entry {
                text: text.into(),
                layout: layout.clone(),
                last_frame: self.frame,
            },
        );
        TextLayout::Shaped(layout)
    }

    /// The shaped "…" used for truncated text.
    pub fn ellipsis(&mut self, window: &Window) -> Arc<LineLayout> {
        if self.ellipsis.is_none() {
            self.ellipsis = Some(self.shape("…", window));
        }
        self.ellipsis.clone().expect("set above")
    }

    /// Counters since the grid was created.
    pub fn stats(&self) -> TextStats {
        TextStats {
            entries: self.entries.len(),
            ..self.stats
        }
    }
}

/// The asset paths of the icons in cells (`CellValue::Icon`), as the shared
/// strings GPUI paints an SVG by: a path a row lends is copied once, when
/// first painted, not in every frame.
#[derive(Default)]
struct IconPaths {
    entries: FxHashMap<u64, IconPath>,
}

struct IconPath {
    path: SharedString,
    last_frame: u64,
}

impl IconPaths {
    fn get(&mut self, path: &str, frame: u64) -> SharedString {
        let mut hasher = FxHasher::default();
        path.hash(&mut hasher);
        let key = hasher.finish();
        match self.entries.get_mut(&key) {
            Some(entry) if entry.path.as_ref() == path => {
                entry.last_frame = frame;
                entry.path.clone()
            }
            _ => {
                let shared = SharedString::from(path.to_owned());
                self.entries.insert(
                    key,
                    IconPath {
                        path: shared.clone(),
                        last_frame: frame,
                    },
                );
                shared
            }
        }
    }

    /// Drops the paths no frame painted for [`KEEP_FRAMES`] frames.
    fn sweep(&mut self, frame: u64) {
        self.entries
            .retain(|_, entry| entry.last_frame + KEEP_FRAMES >= frame);
    }
}

/// A cache of another weight than the grid font's, and the last frame that
/// used it.
struct WeightCache {
    weight: FontWeight,
    cache: TextCache,
    last_frame: u64,
}

/// The text caches of a grid: the grid's font, and one cache per other
/// weight a cell style asks for, made on first use. A cache of its own per
/// weight, because a cache drops its entries when its font changes; each
/// has its own glyph table for numbers. A weight cache starts its frame
/// when a frame first uses it, and goes once no frame used it for
/// [`KEEP_FRAMES`] frames.
#[derive(Default)]
pub(crate) struct TextCaches {
    regular: TextCache,
    by_weight: Vec<WeightCache>,
    /// The paths of icons in cells.
    icon_paths: IconPaths,
    frame: u64,
}

impl TextCaches {
    /// Starts a frame in the grid's font.
    pub fn begin_frame(&mut self, font: &Font, font_size: Pixels) {
        self.frame += 1;
        self.regular.begin_frame(font, font_size);
        if self.frame % SWEEP_EVERY == 0 {
            let frame = self.frame;
            self.by_weight
                .retain(|cache| cache.last_frame + KEEP_FRAMES >= frame);
            self.icon_paths.sweep(frame);
        }
    }

    /// The shared path of an icon in a cell: made on the first frame that
    /// paints it, then looked up by hash.
    pub fn icon_path(&mut self, path: &str) -> SharedString {
        self.icon_paths.get(path, self.frame)
    }

    /// The cache of the grid's font.
    pub fn regular(&mut self) -> &mut TextCache {
        &mut self.regular
    }

    /// The cache for text of `weight`; `None`, or the grid font's own
    /// weight, is the grid's font.
    #[inline]
    pub fn for_weight(&mut self, weight: Option<FontWeight>) -> &mut TextCache {
        match weight {
            None => &mut self.regular,
            Some(weight) => self.weighted(weight),
        }
    }

    fn weighted(&mut self, weight: FontWeight) -> &mut TextCache {
        let Some(font) = self
            .regular
            .font
            .as_ref()
            .filter(|font| font.weight != weight)
        else {
            return &mut self.regular;
        };
        let ix = match self
            .by_weight
            .iter()
            .position(|cache| cache.weight == weight)
        {
            Some(ix) => ix,
            None => {
                self.by_weight.push(WeightCache {
                    weight,
                    cache: TextCache::default(),
                    last_frame: 0,
                });
                self.by_weight.len() - 1
            }
        };
        let cache = &mut self.by_weight[ix];
        if cache.last_frame != self.frame {
            cache.last_frame = self.frame;
            let font = Font {
                weight,
                ..font.clone()
            };
            cache.cache.begin_frame(&font, self.regular.font_size);
        }
        &mut cache.cache
    }

    /// Counters of every cache since the grid was created.
    pub fn stats(&self) -> TextStats {
        let mut stats = self.regular.stats();
        for cache in &self.by_weight {
            let weighted = cache.cache.stats();
            stats.add(TextStats {
                bold_entries: weighted.entries,
                ..weighted
            });
        }
        stats
    }
}

/// A line of text to paint in one cell.
pub(crate) struct TextItem {
    /// Top-left corner of the line box.
    pub origin: Point<Pixels>,
    pub layout: TextLayout,
    /// Text wider than this is cut and ends with an ellipsis.
    pub max_width: Pixels,
    pub color: Hsla,
    /// The ellipsis of text in another weight; `None`: the frame's.
    pub ellipsis: Option<Arc<LineLayout>>,
}

/// Paints a line glyph by glyph. Truncation cuts at a glyph boundary from
/// the laid-out positions, so it costs no extra shaping.
pub(crate) fn paint_text(
    item: &TextItem,
    arena: &GlyphArena,
    ellipsis: &LineLayout,
    line_height: Pixels,
    window: &mut Window,
) {
    match &item.layout {
        TextLayout::Shaped(layout) => paint_glyphs(
            item,
            layout
                .runs
                .iter()
                .flat_map(|run| run.glyphs.iter().map(move |glyph| (run.font_id, glyph)))
                .map(|(font_id, glyph)| Glyph {
                    font_id,
                    glyph_id: glyph.id,
                    x: glyph.position.x,
                    y: glyph.position.y,
                    is_emoji: glyph.is_emoji,
                }),
            Metrics {
                width: layout.width,
                ascent: layout.ascent,
                descent: layout.descent,
                font_size: layout.font_size,
            },
            ellipsis,
            line_height,
            window,
        ),
        TextLayout::Numeric(line) => paint_glyphs(
            item,
            arena[line.glyphs.clone()]
                .iter()
                .map(|(font_id, glyph_id, x)| Glyph {
                    font_id: *font_id,
                    glyph_id: *glyph_id,
                    x: *x,
                    y: px(0.),
                    is_emoji: false,
                }),
            Metrics {
                width: line.width,
                ascent: line.ascent,
                descent: line.descent,
                font_size: line.font_size,
            },
            ellipsis,
            line_height,
            window,
        ),
    }
}

struct Glyph {
    font_id: FontId,
    glyph_id: GlyphId,
    x: Pixels,
    y: Pixels,
    is_emoji: bool,
}

struct Metrics {
    width: Pixels,
    ascent: Pixels,
    descent: Pixels,
    font_size: Pixels,
}

fn paint_glyphs(
    item: &TextItem,
    glyphs: impl Iterator<Item = Glyph>,
    metrics: Metrics,
    ellipsis: &LineLayout,
    line_height: Pixels,
    window: &mut Window,
) {
    let padding_top = (line_height - metrics.ascent - metrics.descent) / 2.;
    let baseline_y = item.origin.y + padding_top + metrics.ascent;
    let truncate = metrics.width > item.max_width;
    // A cell narrower than the ellipsis shows nothing rather than spilling
    // into its neighbour.
    if truncate && item.max_width < ellipsis.width {
        return;
    }
    let limit = if truncate {
        item.max_width - ellipsis.width
    } else {
        metrics.width
    };

    let mut cut_x = metrics.width;
    let mut glyphs = glyphs.peekable();
    while let Some(glyph) = glyphs.next() {
        let end = glyphs.peek().map_or(metrics.width, |next| next.x);
        if truncate && end > limit {
            cut_x = glyph.x;
            break;
        }
        let origin = point(item.origin.x + glyph.x, baseline_y + glyph.y);
        if glyph.is_emoji {
            let _ = window.paint_emoji(origin, glyph.font_id, glyph.glyph_id, metrics.font_size);
        } else {
            let _ = window.paint_glyph(
                origin,
                glyph.font_id,
                glyph.glyph_id,
                metrics.font_size,
                item.color,
            );
        }
    }

    if truncate {
        for run in &ellipsis.runs {
            for glyph in &run.glyphs {
                let origin = point(
                    item.origin.x + cut_x + glyph.position.x,
                    baseline_y + glyph.position.y,
                );
                let _ = window.paint_glyph(
                    origin,
                    run.font_id,
                    glyph.id,
                    ellipsis.font_size,
                    item.color,
                );
            }
        }
    }
}
