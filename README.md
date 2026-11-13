# gpui-kit-grid

A live data grid for [gpui-kit](https://gpui-kit.com) (GPUI) applications: hundreds of thousands of rows, thousands of updates per second from any thread, change highlight, sorting, filtering, grouping and trees in the background.

It is a separate crate on top of gpui-kit. Rows are addressed by key and written from any thread; a background ingest collapses updates by key and publishes snapshots; the order on screen is rebuilt in the background; one custom element paints the visible cells. The ideas come from the LiveGrid grid (WinForms), rethought for GPUI.

**Documentation: https://gkg.giglabo.com/** · [Getting started](https://gkg.giglabo.com/start/) · [Examples](https://gkg.giglabo.com/examples/) · [Live demos](https://gkg.giglabo.com/demo/) · [API](https://gkg.giglabo.com/api/)

> **Status:** stages 1–3 are done: the live core, grouping, aggregates and trees, then editing, elements in cells, export and drag and drop. The API will still change. What may come next is in [ROADMAP.md](ROADMAP.md).

## Why a new grid when gpui-kit has DataTable

`DataTable` builds an element for each cell, addresses rows by index and calls its delegate through `&mut` on the UI thread. That is enough for static tables, not for live data: with 1500 visible cells under 2k updates a second a frame takes 12–14 ms, and after a re-sort the selection points at a different row.

|  | `DataTable` | `gpui-kit-grid` |
|---|---|---|
| Painting | An element per cell | One custom element, glyphs painted directly |
| Frame, 1500 cells, 2k updates/s | 12–14 ms | ~1.1 ms |
| Rows and selection | By index | By key: survive re-sorting and removal |
| Data | `&mut` delegate on the UI thread | `GridWriter` from any thread, background ingest |
| Sorting and filtering | Synchronous, on click | In the background, automatic when data changes |
| Change highlight | No | Yes, with direction arrows and fading removed rows |
| Structure | Flat rows | Grouping, aggregates, totals and trees, built in the background |
| Editing and export | No | Editors under live updates; copy as TSV, CSV and custom formats in the background |

Measured with the headless `spike_bench` example (release, macOS); the [Performance](https://gkg.giglabo.com/performance/) page has every scenario.

## Quick start

```sh
cargo run --release -p gpui-kit-grid --example quickstart
```

A row is an immutable value with a key; `value` borrows from it and must not allocate:

<!-- region crates/grid/examples/quickstart.rs#row -->
```rust
// 2. Rows: an immutable value with a key. `value` must not allocate: it runs
//    for every visible cell on every frame and for every row when sorting.
#[derive(Clone)]
struct Trade {
    id: u64,
    instrument: Arc<str>,
    sell: bool,
    quantity: i64,
    price: f64,
}

impl GridRow for Trade {
    type Key = u64;
    type Column = TradeColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: TradeColumn) -> CellValue<'_> {
        match column {
            TradeColumn::Id => CellValue::Int(self.id as i64),
            TradeColumn::Instrument => CellValue::Text(&self.instrument),
            TradeColumn::Side => CellValue::Text(if self.sell { "Sell" } else { "Buy" }),
            TradeColumn::Quantity => CellValue::Int(self.quantity),
            TradeColumn::Price => CellValue::Float(self.price),
        }
    }
}
```

The grid state gets a schema, a delegate for styling, a store of its own and a sort:

<!-- region crates/grid/examples/quickstart.rs#state -->
```rust
// 5. The grid state: schema, delegate, its own store, a sort.
let schema = GridSchema::new()
    .with_frozen(1)
    .column(GridColumn::new(TradeColumn::Id, "Id").with_width(px(80.)))
    .column(GridColumn::new(TradeColumn::Instrument, "Instrument").with_width(px(140.)))
    .column(GridColumn::new(TradeColumn::Side, "Side").with_width(px(80.)))
    .column(GridColumn::new(TradeColumn::Quantity, "Quantity"))
    .column(GridColumn::new(TradeColumn::Price, "Price"));
let grid = cx.new(|cx| {
    LiveGridState::new_standalone(schema, Trades, StoreOptions::default(), cx)
        .with_view(ViewSpec::new().with_sort([(TradeColumn::Price, SortDirection::Descending)]))
});
```

Any thread writes rows by key:

<!-- region crates/grid/examples/quickstart.rs#writer -->
```rust
// 7. Data from any thread: replace everything, then upsert by key.
let writer = grid.read(cx).writer();
std::thread::spawn(move || {
    let instruments: Vec<Arc<str>> = ["SBER", "GAZP", "LKOH", "YNDX", "VTBR"]
        .map(Arc::from)
        .to_vec();
    let mut rng = fastrand::Rng::new();
    let mut trades: Vec<Trade> = (0..1_000)
        .map(|id| Trade {
            id,
            instrument: instruments[rng.usize(..instruments.len())].clone(),
            sell: rng.bool(),
            quantity: rng.i64(1..10_000),
            price: rng.f64() * 300.,
        })
        .collect();
    writer.replace_all(trades.iter().cloned().map(Arc::new));
    while !writer.is_closed() {
        let trade = &mut trades[rng.usize(..1_000)];
        trade.price *= 1. + (rng.f64() - 0.5) / 100.;
        writer.upsert(trade.clone());
        std::thread::sleep(Duration::from_millis(5));
    }
});
```

The window shows `LiveGrid::new(&grid)`, and the application calls `gpui_kit_grid::init(cx)` after `gpui_kit::init(cx)` for the grid's keys. The whole program is [`crates/grid/examples/quickstart.rs`](crates/grid/examples/quickstart.rs); [Getting started](https://gkg.giglabo.com/start/) walks through it, with installation.

## Examples

| Example | What it shows |
|---|---|
| `quickstart` | The smallest application: 1000 trades from a producer thread |
| `cookbook` | Recipes: a reload with progress, a held order, a saved layout, a custom export, a WebSocket feed |
| `blotter` | Every feature of stages 1 and 2 on a trade blotter: grouping, totals, change arrows, a held order, a status bar on `LiveGridHandle` |
| `portfolio` | A live tree of funds, desks, books and positions |
| `dock_layout` | Grids in a dock, with layouts saved and restored |
| `orders` | Orders edited in place under live updates: every editor, elements in cells, CSV export, drag and drop |
| `desk`, `ladder`, `wire`, `broker` | Trading screens over a simulated market |
| `ops` | A data-centre console in a simulated alert storm |
| `orbit` | About 17,000 satellites from CelesTrak's public data |
| `transit` | Munich by the timetable, and Helsinki live |
| `wiki` | Wikimedia's live edits, a row per wiki |
| `ships` | Every vessel around Finland, live from AIS |
| `perf` | This machine's processes and GPU, live (macOS and Linux) |
| `spike_bench` | The headless benchmark (macOS) |

Run one with `cargo run --release -p gpui-kit-grid --example <name>` (`spike_bench` also needs `--features test-support`). Each example lists its environment variables at the top of its `main.rs`. The showcase examples' fonts are downloaded with `python3 -I script/fetch-example-fonts`, and `orbit`'s and `transit`'s data with `script/fetch-orbit-data` and `script/fetch-transit-data`. `desk`, `broker`, `ops`, `orbit`, `transit`, `wiki` and `ships` also run in the browser, with a quotes page beside them (`crates/grid-web`; see [Web](https://gkg.giglabo.com/web/)).

## Documentation

- [Concepts](https://gkg.giglabo.com/concepts/): rows by key, ingest and snapshots, background rebuilds, and what the grid is for.
- [Replaceable behaviors](https://gkg.giglabo.com/concepts/replaceable-behaviors/): every default is a strategy you can replace.
- Guides: [grouping](https://gkg.giglabo.com/guides/grouping/), [tree](https://gkg.giglabo.com/guides/tree/), [live changes](https://gkg.giglabo.com/guides/live-changes/), [cells](https://gkg.giglabo.com/guides/cells/), [editing](https://gkg.giglabo.com/guides/editing/), [export](https://gkg.giglabo.com/guides/export/), [drag and drop](https://gkg.giglabo.com/guides/drag-and-drop/), [columns](https://gkg.giglabo.com/guides/columns/), [keyboard](https://gkg.giglabo.com/guides/keyboard/), [settings](https://gkg.giglabo.com/guides/settings/).
- [Cookbook](https://gkg.giglabo.com/cookbook/), [Performance](https://gkg.giglabo.com/performance/), [Web](https://gkg.giglabo.com/web/), [Embedding](https://gkg.giglabo.com/embedding/).

The site is built from `site/` (Astro Starlight): `bun install --cwd site --frozen-lockfile`, then `bun run --cwd site build` and `bun test --cwd site`. Code on its pages comes from `// #region` blocks of compiled files, and `bun test` checks that this README's snippets match them. The site also ships as a container image, `ghcr.io/giglabocom/gpui-kit-grid-site` (nginx, `docker/site/`), released on its own tags; see "Releases" in [CONTRIBUTING.md](CONTRIBUTING.md).

## Development

Requires Rust 1.95 (`rust-toolchain.toml`). gpui-kit comes from GitHub, pinned to one revision of its `next` branch; to build against a local gpui-kit checkout instead, see "A local gpui-kit checkout" in [CONTRIBUTING.md](CONTRIBUTING.md).

| Path | Contents |
|---|---|
| `crates/grid` | The grid (`gpui-kit-grid`): `model`, `store`, `engine`, `policy`, `state`, `element`; property and headless UI tests; the examples |
| `crates/grid-macros` | `#[derive(GridColumns)]`, behind the `derive` feature |
| `crates/grid-web` | The browser pages, a separate workspace built for `wasm32-unknown-unknown` |
| `crates/grid-feed`, `crates/grid-gateway` | The quotes feed protocol and its WebSocket gateway |
| `site` | The documentation site |
| `script`, `docker/linux` | Gate checks, data downloads, the benchmark charts; Linux builds and the end-to-end suite |

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --features gpui-kit-grid/test-support -- --deny warnings
cargo test --workspace --features gpui-kit-grid/test-support
RUSTC_BOOTSTRAP=1 cargo check -p gpui-kit-grid --lib --target wasm32-unknown-unknown --features decimal,derive
typos && cargo machete && bun script/check-gpui-kit-pin.ts
```

Linux in Docker: `docker/linux/test.sh`, `docker/linux/smoke.sh` and `docker/linux/e2e.sh`. Rules: [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

## Data in the examples

The examples download their data with the scripts above; nothing is committed.

| Example | Source | Licence |
|---|---|---|
| `orbit` | [CelesTrak](https://celestrak.org), derived from the US Space Force's public catalog | No redistribution granted; fetched at most once per update |
| `transit` (Munich) | MVV's GTFS feed "Soll-Fahrplandaten MVV-Gesamtnetz", © Münchner Verkehrs- und Tarifverbund GmbH, via [opendata.muenchen.de](https://opendata.muenchen.de) | CC BY 4.0; scheduled, not real-time |
| `transit` (Helsinki) | © HSL, [high-frequency positioning](https://digitransit.fi/en/developers/apis/5-realtime-api/vehicle-positions/high-frequency-positioning/) and GTFS | CC BY 4.0 |
| `wiki` | [Wikimedia EventStreams](https://stream.wikimedia.org/v2/stream/recentchange); titles by Wikimedia contributors | CC BY-SA 4.0 |
| `ships` | Source: Fintraffic / [digitraffic.fi](https://www.digitraffic.fi/en/marine-traffic/) | CC BY 4.0 |
| `quotes`, `desk`, `ladder`, `wire`, `broker` | Simulated prices, not market data | — |
| `ops` | Simulated: a made-up data center | — |
| `perf` | This machine's processes and GPU, read locally; nothing leaves the machine | — |

The live examples keep their sources' rules: one connection each, backoff with jitter on reconnect, and no personal data kept. [Licenses](https://gkg.giglabo.com/licenses/) lists what the web demos serve and their third-party notices.

## Sponsor

If the grid is useful to you, consider [sponsoring its development](https://gkg.giglabo.com/sponsor/).

## License

Software source and examples: [Apache-2.0](LICENSE-APACHE), as in gpui-kit.

- Built on [gpui-kit](https://github.com/longbridge/gpui-kit) by Longbridge, also Apache-2.0. gpui-kit in turn is built on GPUI, the UI framework from Zed Industries, also Apache-2.0.
- Contribution rules, gates and agent guidance (`AGENTS.md`, `CONTRIBUTING.md`, `.agents/`) are adapted from gpui-kit and extended.
