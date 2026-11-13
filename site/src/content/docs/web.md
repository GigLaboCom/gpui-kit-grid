---
title: In the browser
description: Build the grid for wasm32 and run it on GPUI's web platform, and what changes there.
sidebar:
  label: Web
---

The grid builds for `wasm32-unknown-unknown` and runs on GPUI's web platform: single-threaded, on WebGPU with a WebGL2 fallback. The same application code runs natively and in a page; the demos on this site are the repository's native examples, compiled for the browser.

## What changes in the browser

- **One thread.** The page has no background threads, so the jobs that run on the background executor natively (rebuilds of the view, aggregates, exports) run on the main thread, between frames. A rebuild stays within a frame up to about 10k rows that change order on every update, and drops frames at 50k. Simulators and live sources run on the executor's timer instead of a thread.
- **Download size.** Each example's module carries GPUI, gpui-kit and the grid: 18–22 MB, 6.4–7.1 MB gzip. Browsers share no code between modules, so each demo is its own download.
- **Fonts.** The browser has no system fonts for GPUI: a page fetches the faces its theme uses, and IBM Plex Sans for GPUI's default family.
- **Clipboard.** A browser offers its clipboard API only in a secure context: https, `localhost` or `127.0.0.1`. Served over plain http on another address, a page puts a stand-in in its place: copying works inside the page, and pasting from the system clipboard does not. Serve over https, or from `localhost`, for the full clipboard.
- **Files.** `export` writes to a `Vec<u8>`; there is no `export_to_path`.
- **Keys.** The `secondary` modifier is Ctrl in the browser. A browser on macOS reports Option with a letter as a character (Option-B arrives as `∫`), so an application that binds Option keys binds those characters as well.
- **Text.** GPUI's glyph fallback in the browser covers emoji and CJK only: Arabic, Hebrew, Indic and Thai text shows as placeholders.
- **The clock.** The grid reads only its store's clock, never `std::time::Instant::now()`, which panics on wasm.

## Build the demos

`crates/grid-web` is a separate workspace with one crate, module and page per example, plugged into a shared shell (the platform and its graphics backend, the window, the fonts, the icons, and the hooks a page's scripts call) through one trait, `PageDelegate`. The desk, broker, orbit, transit, wiki, ships and ops pages run their examples' own `app.rs`, the same code as natively. A new example is a new crate, a new page and its name in `scripts/build-wasm.sh`.

It needs the `wasm32-unknown-unknown` target, `wasm-bindgen-cli` 0.2.129, and the example fonts:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129
python3 -I script/fetch-example-fonts
make -C crates/grid-web build      # release build of every example; APP=desk for one
make -C crates/grid-web serve      # http://127.0.0.1:8080; PORT=8091 for another port
```

GPUI's web platform needs one nightly feature gate even single-threaded, so the build script sets `RUSTC_BOOTSTRAP=1` itself; `wasm-opt` runs when it is installed. The pages use relative paths only, so they work under any prefix.

A release build of every example takes about 160 s from scratch; a change to one example rebuilds only its own module, in under 40 s.

| Command | Does |
|---|---|
| `make measure` | Load and frame times in headless Chrome (`QUERY=app=desk` for the desk) |
| `make examples` | The desk's, the broker's and orbit's themes and keys in headless Chrome |
| `make survive` | Drags and copies that once stopped a page, in headless Chrome |
| `make embed-check` | `<live-grid>` inline and in a frame on another site's page |
| `make licenses` | The licences and third-party notices of what the pages serve |
| `make site` | The release build for a public site: notices required, only servable data |

## Figures

Headless Chrome on an Apple M3 Pro, the modules served locally:

| Page | Module (gzip) | What it does | Frames |
|---|---|---|---|
| quotes | 18.2 MB (6.4) | 150 simulated quotes grouped by sector | — |
| desk | 22.0 MB (7.1) | A keyboard-first terminal, several grids | 3–4 ms on average |
| broker | 20.3 MB (6.8) | A retail broker, light and dark | 3–4 ms on average |
| orbit | 18.7 MB (6.5) | About 17,000 satellites reordering every second | about 5 ms; a rebuild 2.5–3.4 ms |
| transit | 18.5 MB (6.4) | About 650 vehicles by the timetable | about 5 ms at ×1 and ×60 |
| wiki | 18.6 MB (6.5) | Wikimedia's live edits, about 30 events a second | 2–6 ms |
| ships | 18.6 MB (6.5) | About 680 vessels, 42 AIS messages a second | p50 3.1 ms, p99 6.1 ms |
| ops | 20.7 MB (7.0) | 5,760 hosts, an alert storm of about 1,550 updates a second | 3.3–5.9 ms on average, p99 at most 12.1 ms |

orbit is the browser's limit case: one SGP4 sweep over 17,000 objects takes 9–18 ms in wasm, so it is spread over four steps of about 2.4 ms.

## Live sources

The live demos connect the visitor's browser straight to their public source, with no server of ours in between:

- **wiki** opens Wikimedia's EventStreams with `EventSource` only when the visitor presses Start, closes it when the tab has been hidden for a minute or after five minutes without an interaction, and falls back to a recording when the stream cannot be reached.
- **ships** fetches Digitraffic's latest positions, then speaks MQTT over the browser's WebSocket through the same client as natively; `?source=replay` plays a recording.
- **transit** in Helsinki takes HSL's high-frequency positioning over MQTT on a WebSocket; Munich runs on the published timetable, served with the page.
- **orbit** downloads CelesTrak's elements only on a click, keeps them in the browser's Cache Storage for two hours, and stops asking on any answer but success.

[Licenses](../licenses/) lists the sources and their attributions, and [Privacy](../privacy/) what those connections reveal.

## Embedding

Any page can show a demo with one module script and a `<live-grid>` tag, inline or in a frame, and recolour it with a theme remix. [Embedding](../embedding/) has the element's attributes and the remix keys, and [Play](../play/) writes the snippet for you.

## Live quotes from your own gateway

The demos on this site run on a simulator inside the page: no server
sends them anything. The quotes demo can also take its quotes from a
server, the WebSocket gateway in `crates/grid-gateway`. It sends a
snapshot, then deltas conflated per client to the latest value per key
every 20 ms, and a heartbeat every second; a sequence gap asks for a
resync, and a lost connection reconnects with backoff.

Run the gateway on your machine, from a checkout of the repository:

```sh
cargo run --release -p gpui-kit-grid-gateway
```

It listens on `ws://127.0.0.1:9001` (`--port`, `--rate`, `--send-ms`,
`--drop-every` and `--stall-ms` change that). Then build and serve the
demos locally (see [Build the demos](#build-the-demos) for what the build
needs) and open the quotes page with the gateway's address:

```sh
make -C crates/grid-web build serve
# http://127.0.0.1:8080/quotes/?feed=ws://127.0.0.1:9001
```

Until the gateway answers, the page's status line says so and names the
command above. `make -C crates/grid-web measure-feed` reports latency,
conflation, resyncs and reconnect time. A native application can use the
same protocol: the [Cookbook](../cookbook/#feed-a-grid-from-a-websocket)
has a client.
