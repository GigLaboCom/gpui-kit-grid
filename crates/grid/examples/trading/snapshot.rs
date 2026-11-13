//! An example drawn offscreen into PNG files, for a review without a window
//! on screen: headless Metal through gpui-kit's `test-support` (as
//! `spike_bench`'s `SPIKE_SCREENSHOT`), so it needs that feature.
//!
//! The convention: each example reads its own `<EXAMPLE>_SNAPSHOT=<dir>`
//! variable, draws its scenario with [`Snapshot`] into `<dir>` and quits,
//! never opening a window; without `--features test-support` it says so
//! and quits.
//!
//! ```sh
//! DESK_SNAPSHOT=target/desk cargo run --release -p gpui-kit-grid \
//!     --features test-support --example desk
//! ```

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, HeadlessAppContext, Pixels, Render,
    Size, Window, WindowBounds, WindowOptions,
};

/// One frame of real time between two paints.
const FRAME: Duration = Duration::from_millis(16);

/// A headless app with one window, drawn into PNG files.
pub struct Snapshot {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    dir: PathBuf,
}

impl Snapshot {
    /// A headless app with gpui-kit and the grid initialised, then `setup`
    /// (themes, key bindings), and one hidden window of `size` with the view
    /// `build` makes. `None`, with the reason on stderr, if `dir` cannot be
    /// made.
    pub fn open<V: Render>(
        dir: &Path,
        size: Size<Pixels>,
        setup: impl FnOnce(&mut App),
        build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Option<(Self, Entity<V>)> {
        if let Err(error) = std::fs::create_dir_all(dir) {
            err!("{}: {error}", dir.display());
            return None;
        }
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(gpui_kit::assets::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
            setup(cx);
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size,
            })),
            focus: false,
            show: false,
            ..Default::default()
        };
        let (window, view) = cx
            .update(|cx| gpui_kit::open_window(options, cx, build))
            .expect("open a headless window");
        let snapshot = Self {
            cx,
            window,
            dir: dir.to_owned(),
        };
        Some((snapshot, view))
    }

    /// Runs `update` with the app, as a command or a key would.
    pub fn update<R>(&mut self, update: impl FnOnce(&mut App) -> R) -> R {
        self.cx.update(update)
    }

    /// Reads `entity`.
    pub fn read<T: 'static, R>(
        &mut self,
        entity: &Entity<T>,
        read: impl FnOnce(&T, &App) -> R,
    ) -> R {
        self.cx.read_entity(entity, read)
    }

    /// Paints frames for `duration` of real time: the simulator writes on
    /// its own thread meanwhile, and the executor clock advances by the
    /// real time that passed, so flashes and fades last as long as on
    /// screen.
    pub fn run_for(&mut self, duration: Duration) {
        let started = Instant::now();
        let mut last = started;
        while last - started < duration {
            std::thread::sleep(FRAME);
            let now = Instant::now();
            self.cx.advance_clock(now - last);
            last = now;
            self.cx.run_until_parked();
        }
    }

    /// Paints `frames` frames as [`Self::run_for`] does, each one forced and
    /// timed whole (the window's layout, prepaint and paint), and prints
    /// their p50 and p99 with the most elements in cells (`hosts`, such as
    /// the sum of the grids' `GridStats::cell_hosts`) a frame had.
    pub fn measure(&mut self, name: &str, frames: usize, hosts: impl Fn(&App) -> usize) {
        use gpui_kit::test::TestWindowExt as _;

        let mut draws = Vec::with_capacity(frames);
        let mut elements = 0;
        let mut last = Instant::now();
        for _ in 0..frames {
            std::thread::sleep(FRAME);
            let now = Instant::now();
            self.cx.advance_clock(now - last);
            last = now;
            self.cx.run_until_parked();
            let started = Instant::now();
            self.cx
                .update_window(self.window, |_, window, cx| window.render_frame(cx))
                .expect("the window is open");
            draws.push(started.elapsed());
            elements = elements.max(self.cx.update(|cx| hosts(cx)));
        }
        draws.sort_unstable();
        let at = |p: f64| draws[((draws.len() - 1) as f64 * p).round() as usize];
        let ms = |duration: Duration| duration.as_secs_f64() * 1e3;
        out!(
            "measure {name}: {frames} frames, draw p50 {:.2} ms, p99 {:.2} ms, elements in cells {elements}",
            ms(at(0.5)),
            ms(at(0.99)),
        );
    }

    /// Saves the window as `<dir>/<name>.png` and says so on stdout.
    pub fn save(&mut self, name: &str) {
        let path = self.dir.join(format!("{name}.png"));
        let image = self
            .cx
            .capture_screenshot(self.window)
            .expect("the headless Metal renderer");
        match image.save(&path) {
            Ok(()) => out!("snapshot: {}", path.display()),
            Err(error) => err!("{}: {error}", path.display()),
        }
    }
}
