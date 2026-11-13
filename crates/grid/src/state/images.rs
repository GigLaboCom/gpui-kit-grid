//! Images in cells: where the image of a [`CellValue::Image`] comes from.
//!
//! A source finds an image by the id the row gives. The default,
//! [`AssetImageSource`], loads it through GPUI's image loader (assets,
//! files, URIs; rasters and colour SVGs, decoded in the background) and
//! keeps what it loaded; an application with its own store of thumbnails
//! gives the grid a source of its own ([`CellImages::custom`]).
//!
//! [`CellValue::Image`]: crate::CellValue::Image

use std::{
    cell::RefCell,
    hash::{Hash as _, Hasher as _},
    path::Path,
    rc::Rc,
    sync::Arc,
};

use gpui_kit::{
    App, ImgResourceLoader, ObjectFit, RenderImage, Resource, SharedString, SharedUri, Window,
};
use rustc_hash::{FxHashMap, FxHasher};

use super::{LiveGridDelegate, LiveGridState};

/// How an image fits the room of its cell (the cell without its padding),
/// as CSS `object-fit`. A column's parameter:
/// `GridColumn::with_image_fit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFit {
    /// Scaled to fit inside, keeping its proportions (the default).
    #[default]
    Contain,
    /// Scaled to cover the room, keeping its proportions; the cell crops
    /// the rest.
    Cover,
    /// Stretched to the room.
    Fill,
    /// As `Contain` if larger than the room, else at its own size.
    ScaleDown,
    /// At its own size, cropped by the cell.
    None,
}

impl ImageFit {
    /// GPUI's fit of the same name.
    pub(crate) fn object_fit(self) -> ObjectFit {
        match self {
            Self::Contain => ObjectFit::Contain,
            Self::Cover => ObjectFit::Cover,
            Self::Fill => ObjectFit::Fill,
            Self::ScaleDown => ObjectFit::ScaleDown,
            Self::None => ObjectFit::None,
        }
    }
}

/// What a [`CellImageSource`] has for an id now.
#[derive(Clone)]
#[non_exhaustive]
pub enum CellImage {
    /// The image, decoded: the body paints it.
    Ready(Arc<RenderImage>),
    /// Still loading: the cell stays empty until the source has it, and
    /// the source has the grid repainted then.
    Loading,
    /// No image for the id: the cell stays empty; its text is still in the
    /// tooltip and for a screen reader.
    Missing,
}

/// Finds the images of [`CellValue::Image`] cells by their ids.
///
/// Called in prepaint for every visible image cell, every frame, with the
/// grid borrowed: it must not read, update or notify the grid entity. Look
/// an id up by its hash and keep what was found: after the first frame an
/// id costs no allocation. An image that becomes ready later must repaint
/// the grid itself, as `Window::use_asset` does for the view being drawn.
///
/// [`CellValue::Image`]: crate::CellValue::Image
pub trait CellImageSource: 'static {
    /// The image for `id` now.
    fn image(&self, id: &str, window: &mut Window, cx: &mut App) -> CellImage;
}

/// The image source of a grid: the built-in one, or the application's own.
/// Static dispatch for the built-in source on the hot path.
#[derive(Clone)]
#[non_exhaustive]
pub enum CellImages {
    /// GPUI's image loader (the default).
    Assets(AssetImageSource),
    /// The application's own source.
    Custom(Arc<dyn CellImageSource>),
}

impl Default for CellImages {
    fn default() -> Self {
        Self::Assets(AssetImageSource::default())
    }
}

impl From<AssetImageSource> for CellImages {
    fn from(source: AssetImageSource) -> Self {
        Self::Assets(source)
    }
}

impl From<Arc<dyn CellImageSource>> for CellImages {
    fn from(source: Arc<dyn CellImageSource>) -> Self {
        Self::Custom(source)
    }
}

impl CellImages {
    /// The application's own source.
    pub fn custom(source: impl CellImageSource) -> Self {
        Self::Custom(Arc::new(source))
    }

    /// A frame begins: the built-in source keeps the images it asks for
    /// until the next one.
    pub(crate) fn begin_frame(&self) {
        if let Self::Assets(source) = self {
            source.begin_frame();
        }
    }

    /// The image for `id` now.
    #[inline]
    pub(crate) fn image(&self, id: &str, window: &mut Window, cx: &mut App) -> CellImage {
        match self {
            Self::Assets(source) => source.image(id, window, cx),
            Self::Custom(source) => source.image(id, window, cx),
        }
    }
}

/// The default image source: GPUI's image loader
/// (`Window::use_asset::<ImgResourceLoader>`), which reads and decodes in
/// the background, renders an SVG in its own colours, and repaints the grid
/// when an image is ready. An animated GIF shows its first frame.
///
/// The id says where the image is:
///
/// | Id | Image | In a browser |
/// |---|---|---|
/// | `http://…`, `https://…` | A URI, fetched by the application's HTTP client (`App::set_http_client`); without one, missing | With `gpui_web`'s `FetchHttpClient` installed (and the server allowing the page's origin); without it, missing |
/// | An absolute path | A file | Missing: no file system |
/// | Anything else | An asset of the application's `AssetSource` | Works |
///
/// A missing image is never an error of the frame: the cell is empty and
/// its text stays in the tooltip and for a screen reader.
///
/// Every image loaded is kept, as GPUI keeps its assets: memory grows with
/// the number of different images shown. [`Self::with_capacity`] keeps the
/// most recently used ones only. A clone shares the cache, so an
/// application can keep one to read [`Self::asset_requests`].
#[derive(Clone, Default)]
pub struct AssetImageSource {
    cache: Rc<RefCell<ImageCache>>,
}

#[derive(Default)]
struct ImageCache {
    entries: FxHashMap<u64, ImageEntry>,
    /// The most entries kept; `None` keeps every one.
    capacity: Option<usize>,
    /// Lookups so far: an entry's last use, for the least recently used.
    lookups: u64,
    /// Frames begun so far: the images of the frame being planned are
    /// never dropped for one another.
    frame: u64,
    /// Calls of `Window::use_asset`: one per frame for each image while it
    /// loads, none once it is ready or missing.
    asset_requests: u64,
}

struct ImageEntry {
    id: Box<str>,
    resource: Resource,
    image: CellImage,
    last_used: u64,
    /// The frame that last used it.
    frame: u64,
}

impl AssetImageSource {
    /// Keeps at most `capacity` images (at least one), dropping the least
    /// recently used one, and GPUI's copy of it, when another one comes.
    /// The images of one frame are never dropped for one another: while
    /// more different images than `capacity` are on screen, it keeps them
    /// all.
    pub fn with_capacity(self, capacity: usize) -> Self {
        self.cache.borrow_mut().capacity = Some(capacity.max(1));
        self
    }

    /// How many times this source asked GPUI's asset cache for an image: an
    /// image asks once a frame while it loads, and never once it is ready or
    /// missing. For benchmarks and tests.
    pub fn asset_requests(&self) -> u64 {
        self.cache.borrow().asset_requests
    }

    /// The images kept now, loaded or not.
    pub fn len(&self) -> usize {
        self.cache.borrow().entries.len()
    }

    /// Returns true if no image is kept.
    pub fn is_empty(&self) -> bool {
        self.cache.borrow().entries.is_empty()
    }

    /// A frame begins: the images it asks for stay until the next one.
    fn begin_frame(&self) {
        self.cache.borrow_mut().frame += 1;
    }
}

/// Where an image id points: a URI, a file or an asset.
fn resource(id: &str) -> Resource {
    if id.starts_with("http://") || id.starts_with("https://") {
        Resource::Uri(SharedUri::from(id.to_owned()))
    } else if Path::new(id).is_absolute() {
        Resource::Path(Path::new(id).into())
    } else {
        Resource::Embedded(SharedString::from(id.to_owned()))
    }
}

impl CellImageSource for AssetImageSource {
    fn image(&self, id: &str, window: &mut Window, cx: &mut App) -> CellImage {
        let mut hasher = FxHasher::default();
        id.hash(&mut hasher);
        let key = hasher.finish();
        let mut cache = self.cache.borrow_mut();
        cache.lookups += 1;
        let (lookup, frame) = (cache.lookups, cache.frame);
        let known = cache
            .entries
            .get(&key)
            .is_some_and(|entry| &*entry.id == id);
        if !known {
            cache.make_room(window, cx);
            cache.entries.insert(
                key,
                ImageEntry {
                    id: id.into(),
                    resource: resource(id),
                    image: CellImage::Loading,
                    last_used: lookup,
                    frame,
                },
            );
        }
        let ImageCache {
            entries,
            asset_requests,
            ..
        } = &mut *cache;
        let entry = entries.get_mut(&key).expect("inserted above");
        entry.last_used = lookup;
        entry.frame = frame;
        if let CellImage::Loading = entry.image {
            *asset_requests += 1;
            entry.image = match window.use_asset::<ImgResourceLoader>(&entry.resource, cx) {
                None => CellImage::Loading,
                Some(Ok(image)) => CellImage::Ready(image),
                Some(Err(_)) => CellImage::Missing,
            };
        }
        entry.image.clone()
    }
}

impl ImageCache {
    /// Drops the least recently used entries while the cache is full, with
    /// GPUI's copy of their images and their textures; never one the frame
    /// being planned uses, so images on screen do not push each other out
    /// and load again and again.
    fn make_room(&mut self, window: &mut Window, cx: &mut App) {
        let Some(capacity) = self.capacity else {
            return;
        };
        while self.entries.len() >= capacity {
            let frame = self.frame;
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.frame != frame)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                return;
            };
            let entry = self.entries.remove(&oldest).expect("found above");
            cx.remove_asset::<ImgResourceLoader>(&entry.resource);
            if let CellImage::Ready(image) = entry.image {
                let _ = window.drop_image(image);
            }
        }
    }
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets where the images of `CellValue::Image` cells come from (default
    /// [`AssetImageSource`]: GPUI's image loader, every image kept).
    pub fn with_image_source(mut self, source: impl Into<CellImages>) -> Self {
        self.images = source.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_resource() {
        assert!(matches!(
            resource("https://example.com/a.png"),
            Resource::Uri(uri) if uri.as_ref() == "https://example.com/a.png"
        ));
        assert!(matches!(
            resource("http://localhost/a.png"),
            Resource::Uri(_)
        ));
        #[cfg(not(windows))]
        assert!(matches!(
            resource("/tmp/a.png"),
            Resource::Path(path) if path.as_ref() == Path::new("/tmp/a.png")
        ));
        assert!(matches!(
            resource("images/a.png"),
            Resource::Embedded(path) if path.as_ref() == "images/a.png"
        ));
    }
}
