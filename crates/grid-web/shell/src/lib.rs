//! What every web example's page shares (`wasm32-unknown-unknown`, GPUI's
//! web platform: one canvas, single-threaded, WebGPU with a WebGL2
//! fallback). Each example is a crate of its own in `apps/`, built into its
//! own module and page (`www/<app>/`); it plugs in through one trait,
//! [`PageDelegate`], and [`register`]s its delegate when its module starts:
//!
//! ```ignore
//! #[wasm_bindgen(start)]
//! fn start() {
//!     gpui_kit_grid_web_shell::register(DeskPage::default());
//! }
//! ```
//!
//! The shell does the rest, the same on every page: the platform and its
//! graphics backend (`?backend=webgl` skips WebGPU), the panic hook and the
//! console log, the window and the view that waits for the canvas's size,
//! the fonts' intake (GPUI's default face, [`fonts::SYSTEM_UI`], before
//! every example), the icons, and the hooks the page's scripts call.
//!
//! The page (`www/shell.js`) asks [`font_files`] which faces to fetch,
//! hands each to [`add_font`] and calls [`run`] with its URL's parameters;
//! after the first rows it fetches the other themes' faces and calls
//! [`fonts_loaded`]. [`set_rate`] changes the example's update rate,
//! [`stats`] reports its main grid's counters and [`grid_state`] its
//! scroll, selection and sort with the example's own state. An example's
//! own hooks live in its crate, reaching its view with [`with_view`].
//! [`feed`] is the client of the quotes gateway (`?feed=`).
//!
//! Embedded in another page (`?embed`, which the `<live-grid>` element
//! passes too), the example does not take the keyboard when it opens: the
//! first click in it does. [`scroll_room`] tells the page which ways the
//! example can still scroll, so a wheel at an end scrolls the page around
//! it. [`set_theme`] lays a theme remix over the example's theme
//! ([`remix`] lists its keys), after the page has fetched the faces
//! [`theme_font_files`] names and handed them to [`add_theme_font`];
//! [`current_theme`] reports the remix keys of the theme shown.

use std::{cell::RefCell, rc::Rc, time::Duration};

use gpui_kit::web::{CanvasFontFallback, WebBackendPreference, WebPlatform};
use gpui_kit::{
    AnyView, AnyWindowHandle, App, AppContext as _, Application, ApplicationHandle, Context,
    Entity, IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window,
    WindowOptions, div, px,
};
use serde_json::Value;
use wasm_bindgen::prelude::*;
use web_time::Instant;

pub mod feed;
pub mod fonts;
mod icons;
mod params;
pub mod remix;
mod report;
mod scroll;

pub use fonts::FontFiles;
pub use icons::BundledIcon;
pub use params::PageParams;
pub use report::{grid_json, ms, sort_json, stats_json};
pub use scroll::ScrollRoom;

use icons::Icons;

/// One example's side of its page: the faces and themes it draws in, its
/// setup and its view, and what it reports to the page's scripts. The
/// shell calls it on the page's only thread, never while another of its
/// hooks runs.
pub trait PageDelegate: 'static {
    /// The example's view, the window's content.
    type View: Render;

    /// The faces to fetch for the start `theme` (the page's `?theme=`,
    /// empty without one; the example's default theme if empty or
    /// unknown). The shell adds GPUI's default face to those fetched
    /// before the example runs.
    fn font_files(&self, theme: &str) -> FontFiles;

    /// One face the page fetched (a `.ttf` file's bytes), to keep for the
    /// next theme installation: the start faces before [`setup`], the later
    /// ones before [`install_themes`].
    ///
    /// [`setup`]: Self::setup
    /// [`install_themes`]: Self::install_themes
    fn add_font(&self, face: Vec<u8>);

    /// Registers the themes whose faces arrived since the setup, once the
    /// page has fetched the later faces.
    fn install_themes(&self, cx: &mut App);

    /// Once, before the window opens: gpui-kit, the grid's keys, the
    /// themes with the start faces, the example's keys, and what its view
    /// is built from `params`.
    fn setup(&mut self, params: &PageParams, cx: &mut App);

    /// The example's view in `window`, the keyboard in it, once the browser
    /// has reported the canvas's size. GPUI's web window is 0×0 until then,
    /// and a view that sizes itself against the window when it is built
    /// (the desk's dock splits) would keep the proportions of that size.
    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Self::View>;

    /// The counters [`stats`] reports as JSON, usually [`stats_json`] and
    /// the example's own; `built` is how long [`open`](Self::open) took.
    fn stats(&self, view: &Entity<Self::View>, built: Duration, cx: &App) -> Value;

    /// What [`grid_state`] reports as JSON for input checks: the main
    /// grid's scroll, selection and sort ([`grid_json`]) and the example's
    /// own state.
    fn grid_state(&self, view: &Entity<Self::View>, window: &Window, cx: &App) -> Value;

    /// Sets the updates a second ([`set_rate`]) and returns the rate set;
    /// `None` for an example without one.
    fn set_rate(&self, view: &Entity<Self::View>, rate: u32, cx: &mut App) -> Option<u32> {
        let _ = (view, rate, cx);
        None
    }

    /// Which ways the example can still scroll ([`scroll_room`]): usually
    /// [`ScrollRoom::of_grid`] of its main grid. By default every wheel
    /// over the example is the example's.
    fn scroll_room(&self, view: &Entity<Self::View>, cx: &App) -> ScrollRoom {
        let _ = (view, cx);
        ScrollRoom::BOTH_WAYS
    }

    /// The gpui-kit icons the example draws besides those the shell
    /// bundles for the grid and gpui-kit's components.
    fn icons(&self) -> &'static [BundledIcon] {
        &[]
    }

    /// What the browser's canvas draws when no face the page loaded has a
    /// glyph: emoji (GPUI's default), or CJK too for an example that shows
    /// text in any script, such as page titles.
    fn font_fallback(&self) -> CanvasFontFallback {
        CanvasFontFallback::default()
    }
}

/// Makes `delegate` the page's example: call it once, when the module
/// starts (`#[wasm_bindgen(start)]`), before the page calls any hook.
pub fn register(delegate: impl PageDelegate) {
    PAGE.set(Some(Box::new(delegate)));
}

/// Runs `read` on the example's view in its window: `None` before the view
/// is open, if it is not a `V`, or when `read` has nothing. For the
/// example's own hooks.
pub fn with_view<V: 'static, R>(
    read: impl FnOnce(&Entity<V>, &mut Window, &mut App) -> Option<R>,
) -> Option<R> {
    with_opened(|opened, window, cx| {
        let view = opened.view.clone().downcast::<V>().ok()?;
        read(&view, window, cx)
    })
}

/// [`PageDelegate`] with its view's type erased, as the page keeps it.
trait Page {
    fn font_files(&self, theme: &str) -> FontFiles;
    fn add_font(&self, face: Vec<u8>);
    fn icons(&self) -> &'static [BundledIcon];
    fn font_fallback(&self) -> CanvasFontFallback;
    fn install_themes(&self, cx: &mut App);
    fn setup(&mut self, params: &PageParams, cx: &mut App);
    fn open(&mut self, window: &mut Window, cx: &mut App) -> AnyView;
    fn stats(&self, view: &AnyView, built: Duration, cx: &App) -> Option<Value>;
    fn grid_state(&self, view: &AnyView, window: &Window, cx: &App) -> Option<Value>;
    fn set_rate(&self, view: &AnyView, rate: u32, cx: &mut App) -> Option<u32>;
    fn scroll_room(&self, view: &AnyView, cx: &App) -> Option<ScrollRoom>;
}

impl<D: PageDelegate> Page for D {
    fn font_files(&self, theme: &str) -> FontFiles {
        PageDelegate::font_files(self, theme)
    }

    fn add_font(&self, face: Vec<u8>) {
        PageDelegate::add_font(self, face);
    }

    fn icons(&self) -> &'static [BundledIcon] {
        PageDelegate::icons(self)
    }

    fn font_fallback(&self) -> CanvasFontFallback {
        PageDelegate::font_fallback(self)
    }

    fn install_themes(&self, cx: &mut App) {
        PageDelegate::install_themes(self, cx);
    }

    fn setup(&mut self, params: &PageParams, cx: &mut App) {
        PageDelegate::setup(self, params, cx);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> AnyView {
        PageDelegate::open(self, window, cx).into()
    }

    fn stats(&self, view: &AnyView, built: Duration, cx: &App) -> Option<Value> {
        let view = view.clone().downcast::<D::View>().ok()?;
        Some(PageDelegate::stats(self, &view, built, cx))
    }

    fn grid_state(&self, view: &AnyView, window: &Window, cx: &App) -> Option<Value> {
        let view = view.clone().downcast::<D::View>().ok()?;
        Some(PageDelegate::grid_state(self, &view, window, cx))
    }

    fn set_rate(&self, view: &AnyView, rate: u32, cx: &mut App) -> Option<u32> {
        let view = view.clone().downcast::<D::View>().ok()?;
        PageDelegate::set_rate(self, &view, rate, cx)
    }

    fn scroll_room(&self, view: &AnyView, cx: &App) -> Option<ScrollRoom> {
        let view = view.clone().downcast::<D::View>().ok()?;
        Some(PageDelegate::scroll_room(self, &view, cx))
    }
}

thread_local! {
    static PAGE: RefCell<Option<Box<dyn Page>>> = const { RefCell::new(None) };
    static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
    static OPENED: RefCell<Option<Opened>> = const { RefCell::new(None) };
}

/// The example's view once its window is open, and how long building it
/// took (a simulated market included).
struct Opened {
    view: AnyView,
    window: AnyWindowHandle,
    built: Duration,
}

fn with_page<R>(call: impl FnOnce(&dyn Page) -> R) -> R {
    PAGE.with_borrow(|page| call(page.as_deref().expect(UNREGISTERED)))
}

fn with_page_mut<R>(call: impl FnOnce(&mut dyn Page) -> R) -> R {
    PAGE.with_borrow_mut(|page| call(page.as_deref_mut().expect(UNREGISTERED)))
}

const UNREGISTERED: &str = "no example registered: its module calls `register` when it starts";

/// The faces (paths under `fonts/`) the page fetches with `query` (the
/// page's `location.search`, its `?theme=` the start theme) as JSON:
/// `[[before run], [after the first rows]]`. GPUI's default face comes
/// before `run` on every page.
#[wasm_bindgen]
pub fn font_files(query: &str) -> String {
    let theme = PageParams::new(query).get("theme").unwrap_or_default();
    let files = with_page(|page| page.font_files(&theme));
    let mut start = files.start().to_vec();
    start.push(fonts::SYSTEM_UI);
    serde_json::json!([start, files.later()]).to_string()
}

/// One face (a `.ttf` file's bytes) the page fetched: before `run`, for
/// the start theme; after it, followed by `fonts_loaded`.
#[wasm_bindgen]
pub fn add_font(face: Vec<u8>) {
    with_page(|page| page.add_font(face));
}

/// Registers the themes whose faces arrived since `run`.
#[wasm_bindgen]
pub fn fonts_loaded() {
    with_opened(|_, _, cx| {
        with_page(|page| page.install_themes(cx));
        None::<()>
    });
}

/// Starts the example in the page's canvas with `query`, the page's
/// `location.search`: the example reads its parameters from it in its
/// setup, and `?backend=webgl` skips WebGPU, as a browser without it would.
#[wasm_bindgen]
pub fn run(query: &str) {
    gpui_kit::platform::web_init();
    let params = PageParams::new(query);
    let embedded = params.get("embed").is_some();
    // Without threads (`false`): the grid's ingest task, its projection
    // rebuilds and the example's simulator share the page's main thread
    // with painting.
    let backend = if params.get("backend").as_deref() == Some("webgl") {
        WebBackendPreference::WebGl
    } else {
        WebBackendPreference::Auto
    };
    let fallback = with_page(|page| page.font_fallback());
    let platform = WebPlatform::new_with_backend_and_font_fallback(false, backend, fallback);
    let icons = Icons::new(with_page(|page| page.icons()));
    let application = Application::with_platform(Rc::new(platform))
        .with_assets(icons)
        .run_embedded(move |cx| {
            with_page_mut(|page| page.setup(&params, cx));
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Shell::new(embedded, window, cx))
            })
            .expect("open the window");
            cx.activate(true);
        });
    APPLICATION.set(Some(application));
}

/// The window's root view: empty until the browser reports the canvas's
/// size, then the example's view.
struct Shell {
    view: Option<AnyView>,
    /// In another page (`?embed`): the example opens without the keyboard.
    embedded: bool,
    _resized: Subscription,
}

impl Shell {
    fn new(embedded: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            view: None,
            embedded,
            _resized: cx.observe_window_bounds(window, |shell, window, cx| shell.open(window, cx)),
        }
    }

    /// Opens the example's view, timed, once the window has a size.
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.viewport_size().width <= px(0.) || self.view.is_some() {
            return;
        }
        let started = Instant::now();
        let view = with_page_mut(|page| page.open(window, cx));
        if self.embedded {
            // An example focuses its main view when it opens; in another
            // page the keyboard stays where the reader left it until a
            // click in the example.
            window.blur(cx);
        }
        let built = started.elapsed();
        self.view = Some(view.clone());
        OPENED.set(Some(Opened {
            view,
            window: window.window_handle(),
            built,
        }));
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.view.clone())
    }
}

/// Sets the example's updates a second; returns the rate set (0 for an
/// example without one, such as orbit).
#[wasm_bindgen]
pub fn set_rate(rate: u32) -> u32 {
    with_opened(|opened, _, cx| with_page(|page| page.set_rate(&opened.view, rate, cx)))
        .unwrap_or(0)
}

/// The example's counters as JSON, its main grid's among them, `null`
/// before the window opens. Durations are in milliseconds; `build_ms` is
/// how long building the example's view took, a simulated market included.
#[wasm_bindgen]
pub fn stats() -> String {
    json_or_null(with_opened(|opened, _, cx| {
        with_page(|page| page.stats(&opened.view, opened.built, cx))
    }))
}

/// The example's main grid's scroll, selection and sort, with the
/// example's own state, as JSON; `null` before the window opens.
#[wasm_bindgen]
pub fn grid_state() -> String {
    json_or_null(with_opened(|opened, window, cx| {
        with_page(|page| page.grid_state(&opened.view, window, cx))
    }))
}

/// Which ways the example can still scroll: 1 up, 2 down, 3 both (and
/// before the window opens). An embedded page lets a wheel the example has
/// no room for scroll the page around it.
#[wasm_bindgen]
pub fn scroll_room() -> u32 {
    with_opened(|opened, _, cx| with_page(|page| page.scroll_room(&opened.view, cx)))
        .unwrap_or(ScrollRoom::BOTH_WAYS)
        .bits()
}

/// Lays the theme remix `json` over the example's theme (see [`remix`] for
/// its keys; `{}` shows the example's own again) and returns, as JSON, what
/// it applied, ignored and replaced, or `{"error": …}` for JSON that does
/// not parse or before the window opens.
#[wasm_bindgen]
pub fn set_theme(json: &str) -> String {
    match with_opened(|_, _, cx| Some(remix::apply(json, cx))) {
        Some(Ok(report)) => report.to_string(),
        Some(Err(error)) => serde_json::json!({ "error": error }).to_string(),
        None => serde_json::json!({ "error": "the example has not opened yet" }).to_string(),
    }
}

/// The faces (paths under `fonts/`, as JSON) of the shipped families the
/// remix `json` names that the page has not handed over yet.
#[wasm_bindgen]
pub fn theme_font_files(json: &str) -> String {
    let files = with_opened(|_, _, cx| Some(remix::missing_faces(json, cx))).unwrap_or_default();
    serde_json::json!(files).to_string()
}

/// One face (a `.ttf` file's bytes) for a remix's family, after `run`.
#[wasm_bindgen]
pub fn add_theme_font(face: Vec<u8>) {
    with_opened(|_, _, cx| {
        remix::add_font(face, cx);
        None::<()>
    });
}

/// As JSON, `{theme, families}`: the remix keys of the theme shown now, and
/// the font families a remix can name; `null` before the window opens.
#[wasm_bindgen]
pub fn current_theme() -> String {
    json_or_null(with_opened(|_, _, cx| {
        let families: Vec<&str> = fonts::shipped_families().collect();
        Some(serde_json::json!({ "theme": remix::current(cx), "families": families }))
    }))
}

fn json_or_null(value: Option<Value>) -> String {
    value.map_or_else(|| "null".into(), |value| value.to_string())
}

/// Runs `read` on the opened example in its window; `None` before the
/// window opens or when `read` has nothing.
fn with_opened<R>(read: impl FnOnce(&Opened, &mut Window, &mut App) -> Option<R>) -> Option<R> {
    APPLICATION.with_borrow(|application| {
        let application = application.as_ref()?;
        OPENED.with_borrow(|opened| {
            let opened = opened.as_ref()?;
            application.update(|cx| {
                cx.update_window(opened.window, |_, window, cx| read(opened, window, cx))
                    .ok()
                    .flatten()
            })
        })
    })
}
