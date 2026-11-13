//! A live performance monitor for this machine: every process with its
//! CPU, GPU, memory, disk and threads, sampled every second on a thread of
//! its own and written to the grid's store, the machine's totals pinned on
//! top. Nothing is simulated. macOS and Linux; on Windows the GPU
//! columns stay blank (`gpu.rs` says where NVML and PDH would go).
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example perf
//! PERF_INTERVAL_MS=2000 cargo run --release -p gpui-kit-grid --example perf
//! # No window: the monitor drawn offscreen to PNG files (`snapshot.rs`)
//! PERF_SNAPSHOT=target/perf cargo run --release -p gpui-kit-grid --features test-support --example perf
//! # Linux in Docker (Mesa, Xvfb): the snapshots and a windowed run
//! docker/linux/perf.sh
//! ```
//!
//! - **Processes** (`table.rs`): name, PID, CPU % (of one core, as
//!   Activity Monitor counts), GPU %, GPU memory (Linux), memory (the
//!   footprint where macOS gives it, else the resident size), disk reads
//!   and writes a second,
//!   threads, user, uptime and app (the bundle a helper is in). Keyed by
//!   PID and start time, since PIDs are reused. Arranged as a tree by
//!   parent process, a flat list, or grouped by user or by app with sums
//!   in each caption; sorted by CPU, live, with the order held while the
//!   pointer rests on the grid and paused with F9. A new process flashes,
//!   an exited one fades out; a value flashes only when it moved far
//!   enough (`sampler.rs`), so the grid does not shimmer.
//! - **Machine rows**, pinned: CPU (% of all cores), memory used of the total
//!   with the pressure (macOS's level, Linux's PSI stall), GPU utilization
//!   (renderer and tiler on macOS, the busiest engine on Linux) and GPU
//!   memory.
//! - **Chart** (`chart.rs`): the machine's CPU and GPU, last 120 samples.
//! - **GPU** (`gpu.rs`): a `GpuSampler` behind a factory; on macOS
//!   `ioreg`'s accelerator statistics and each process's accumulated GPU
//!   time, no root needed; on Linux the DRM clients' `fdinfo` and the
//!   cards in sysfs (`drm.rs`), for any DRM driver, "none found" without
//!   one.
//! - **Processes on each system** (`task.rs`): macOS adds threads, the
//!   footprint and other users' identity from libproc and sysctl; Linux
//!   adds threads and the memory pressure from `/proc`.
//!
//! **Privacy.** Process names and user names are personal data: they stay
//! on screen and in snapshot files the run writes, never in the
//! repository. Command lines and environments are never read.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `PERF_INTERVAL_MS` | 1000 | The sampling interval, 250 to 10000 ms |
//! | `PERF_GPU` | `auto` | `auto`: the platform's GPU source; `none`: no GPU columns |
//! | `PERF_SECONDS` | none | Print the status every second and quit after N seconds |
//! | `PERF_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use std::time::Duration;

use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the example's own modules are its children here.
#[path = "app.rs"]
mod app;

/// The settings the environment sets (see the table above).
fn settings_from_env() -> app::Settings {
    let mut settings = app::Settings::default();
    if let Ok(value) = std::env::var("PERF_INTERVAL_MS") {
        match value.parse::<u64>() {
            Ok(ms) => settings = settings.with_interval(Duration::from_millis(ms)),
            Err(_) => err!("PERF_INTERVAL_MS={value}: not a number; ignored"),
        }
    }
    if let Ok(value) = std::env::var("PERF_SECONDS") {
        match value.parse::<u64>() {
            Ok(seconds) => settings = settings.with_quit_after(seconds),
            Err(_) => err!("PERF_SECONDS={value}: not a number; ignored"),
        }
    }
    match std::env::var("PERF_GPU").as_deref() {
        Ok("none") => settings = settings.with_gpu_sampler(app::gpu::NoGpu),
        Ok("auto") | Err(_) => {}
        Ok(other) => err!("PERF_GPU={other}: auto or none; using auto"),
    }
    settings
}

fn main() {
    let settings = settings_from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("PERF_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        app::snapshot::run(&settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "PERF_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons (search, sort arrows, chevrons).
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
            app::configure(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1600.), px(960.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let perf = cx.new(|cx| app::Perf::new(&settings, window, cx));
                let focus = gpui_kit::Focusable::focus_handle(perf.read(cx).grid(), cx);
                window.focus(&focus, cx);
                // Closing the window ends the sampler and the monitor.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                perf
            })
            .expect("open window");
            cx.activate(true);
        });
}
