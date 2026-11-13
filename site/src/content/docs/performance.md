---
title: Performance
description: Frame, rebuild, ingest and memory figures from the headless benchmark, and how to reproduce them.
---

The headless `spike_bench` example measures frames, background rebuilds, ingest and memory under scripted loads, up to 100k rows × 40 columns at 10k updates per second, and runs the same load through gpui-kit's `DataTable` for comparison. Each scenario has a limit of its own, and a run passes when every one holds.

## Method

- **The machine.** Apple M3 Pro, a release build, macOS, rendering headless through Metal at 2560×1440, scale 2×.
- **The load.** 100k rows × 40 columns unless a scenario says otherwise; updates come from a producer thread at a fixed rate, with the view sorted by a column that changes.
- **What is measured.** Frames are the main thread's time per frame, as percentiles over the scenario. Rebuilds, aggregate jobs and exports are measured on the background threads; their effect on the UI thread separately. Ingest is the share of a core the store's ingest task uses.
- **Repetition.** Each final run is made twice; ranges span both. The benchmark is not run in CI: it needs Metal, and shared runners are too noisy for p99.

## Stage 2 final run

Release build of `main` at `f198707`, the grid code of stage 2, two runs. Frames on the main thread, rebuilds on the background threads.

| Scenario | Result | Limit |
|---|---|---|
| Frame, 2k updates/s on visible rows (S1) | p99 1.10–1.12 ms | 8 ms |
| S1 with a direction arrow on every hot cell (S20) | p99 1.12–1.13 ms | 8 ms |
| Frame, 100k rows, 10k updates/s, sorted by a changing column (S2) | p99 1.34–1.43 ms | 8 ms |
| S2 plus 1k removals and 1k inserts per second (S7) | p99 1.41–1.50 ms | 8 ms |
| Fling: a new page every frame (S3) | p99 2.50–2.75 ms | 12 ms |
| S2 with pinned rows and a progress bar (S8) | p99 1.30–1.38 ms | 8 ms |
| S2 grouped by one column (8 groups), `Sum` and `Avg`, all expanded (S16) | frame p99 1.27–1.37 ms; rebuild p99 12.7–13.3 ms | 12 ms; 30 ms |
| S2 grouped by two columns, 600 groups, half collapsed (S16b) | frame p99 1.16–1.17 ms; rebuild p99 14.9–16.4 ms | 12 ms; 30 ms |
| S16 with a footer under every group and the totals row (S23) | frame p99 1.16–1.28 ms | 12 ms |
| Only aggregated columns change (S17) | aggregate job p99 3.8–5.1 ms, no rebuild | 10 ms |
| Expand and collapse a 50k-row group; expand all over 1000 groups (S18) | job p99 0.2 ms; the frames showing it at most 7.6 ms | 10 ms; 12 ms |
| Tree: 100k rows, depth 5, ~20k parents, 10k updates/s, quick filter (S19) | rebuild p99 4.9–7.0 ms; frame p99 1.26–1.27 ms | 40 ms; 12 ms |
| S16 with 1k removals/s fading out for 1 s and 1k inserts/s (S21) | frame p99 1.52–1.55 ms; rebuild p99 12.2–13.9 ms; at most 1053 fading rows | 12 ms; 30 ms; 1200 |
| A header dragged through the group panel and back (S22) | frame p99 1.22–1.28 ms | 12 ms |
| Accessibility tree plan, flat (S10) and grouped (S10b) | p99 0.23–0.24 ms and 0.19–0.20 ms | 1 ms |
| Quick filter over 100k rows, worst query (S11) | p99 5.6–8.4 ms | 30 ms |
| Select all, 100k `u64` keys (S14) | p99 6.1–8.6 ms | 16 ms |
| Copy all, 100k rows × 39 columns, 56 MB (S9); all expanded with 8 groups and totals (S9b) | background p50 181 ms and 181–185 ms; UI thread at most 0.04 ms | 500 ms; 50 ms |
| Ingest at 10k updates/s | 8.8–9.0% of a core | 20% |
| Highlight memory at 10k updates/s | 2.9 MB | 10 MB |
| Ingest at 100k updates/s, the store's own loop (S24) | 24.5–24.7% of a core; 8.2 MB of highlight | — |
| `DataTable` on the S1 load (S6) | p50 12.0–12.2 ms, p99 12.7–13.8 ms | baseline |

Every limit holds in both runs. The ingest and highlight memory rows come from the update rate sweep (S24) after its fixes: the store publishes no more often than a grid pulls, a row keeps only the stamps of its hot columns, and an update allocates once. Before them, ingest at 100k updates/s took 39.5–41.2% of a core, and highlight memory went over 10 MB from 15k updates/s; now only at 200k (10.2 MB).

### Against DataTable

On the S1 load (1500 visible cells, 2k updates a second) gpui-kit's `DataTable`, which builds an element per cell, takes 12–14 ms a frame; the grid, which paints glyphs from one element, about 1.1 ms.

## Stage 3 scenarios

Release build of the stage 3 code on an idle machine (load average 2.4–5.0), 100k rows × 40 columns. In the same run S1 and S2 stay at frame p99 1.09 ms and 1.33 ms.

| Scenario | Result | Limit |
|---|---|---|
| S1 with a status icon and a time on every row (S25) | frame p99 1.09 ms | 8 ms |
| S1 with a progress bar and a checkbox column, about 100 elements a frame (S26); the same at a fling (S26b) | frame p99 2.80 ms; 4.38 ms | 8 ms; 12 ms |
| S2 with a cell editor open for 10 s and 8 characters typed (S27) | frame p99 1.39 ms; the edited row keeps its screen line | 12 ms |
| S2 with 1,000 optimistic values, 100 confirmed a second (S28) | frame p99 1.17 ms; snapshot pull p99 0.22 ms | 12 ms and +5% over S2; 0.5 ms |
| S2 with an image on every row, 16 different (S29) | frame p99 1.23 ms; no image loads per frame after warm-up | 12 ms |
| CSV export, 100k rows × 39 columns, 56.5 MB (S35) | background 194 ms (to a file 205 ms); UI thread at most 0.01 ms; table 11.2 MB | 600 ms; 50 ms; 64 MB |
| `export_with` reading every cell, 8 groups expanded, totals, 1,000 waiting cells (S36) | background 39 ms; UI thread at most 0.01 ms | 400 ms; 50 ms |
| S2 with CSV exports running back to back (S37) | frame p99 1.39 ms, 100% of S2 in the same run; ingest 18% of a core | +10%; 20% |
| A row drag over a second grid under the S2 load (S38) | frame p99 1.27 ms; placing the drop target for one pointer move p50 0.0003 ms, p99 0.0008 ms | 12 ms; 0.1 ms |
| Starting a drag of 100k selected rows, `u64` keys (S39) | 3.69 ms on the UI thread (max 4.08 ms) | 16 ms |
| The column chooser over 128 columns under the S2 load (S40) | opening 7.5 ms; one search character 2.1 ms; frames while hiding columns p99 2.2 ms | 16 ms; 4 ms; 12 ms |
| `LiveGridHandle` over 128 columns (S41) | `columns()` 2.6 µs; counts 4 ns | 50 µs; 1 µs |

The same run measured the ingest under S24's sweep at 31.7% of a core at 200k updates/s (47–49% before writes were collapsed by key).

## Keyed storm (S42)

S42 sends events from a producer thread to keys drawn by a Zipf law (s = 1.1: a hot minority, as in an incident storm) or uniformly, with the view sorted by a changing column, and compares the two queue policies: `WriteCoalescing::ByKey` (the default: a later upsert of a key that is still queued takes its place) and `KeepEveryWrite`. Release build of `main` at `4f4c164`, 10 s per cell, on a machine that was not idle, so the two policies were measured back to back in the same run.

| Keys | Events/s | Peak queue, by key / every write | Ingest, % of a core | Write → publish p99, ms |
|---|---|---|---|---|
| Zipf, 10k | 100k | 761 / 3,343 | 9.6 / 14.4 | 12.8 / 15.1 |
| Zipf, 10k | 1M | 3,475 / 23,785 | 16.7 / 29.3 | 16.5 / 21.3 |
| Zipf, 10k | 2M | 5,582 / 65,011 | 14.3 / 38.9 | 14.5 / 30.6 |
| Uniform, 100k | 1M | 30,023 (by key) | 41.7 | 30.5 |
| Uniform, 100k | 2M | 83,170 / 1,143,086 | 74.1 / 98.7 | 128 / 1,041 |

With uniform keys and up to 1M events/s almost nothing merges, so only the default policy runs. At 2M/s over 100k uniform keys the ingest falls behind with either policy: by key slowly, keeping every write collapses. The frame budget holds in every cell, and the UI's snapshot pull stays under 0.25 ms. A stream of new keys (every event a new key) is the case the grid does not absorb: rows, rebuild time and memory grow with it, so such streams need aggregation before the grid.

## In the browser

On GPUI's web platform the rebuilds run on the main thread. [Web](../web/#figures) has frame times for each demo page in headless Chrome.

## Reproduce a run

On macOS, from a checkout of the repository:

```sh
mkdir -p target
cargo run --release -p gpui-kit-grid --features test-support --example spike_bench > target/bench-run.md
python3 -I script/spike_charts.py target/bench-run.md target/bench-charts
```

The first command runs every scenario and writes a Markdown report with each result against its limit; the second draws the charts from it. Close other applications first: the tails (p99) are sensitive to a busy machine.

For an application of your own, `MTL_HUD_ENABLED=1` shows the frame rate on macOS, and `samply record cargo run --release --example <name>` profiles it. `BLOTTER_SECONDS=10 ROWS=100000 RATE=10000 cargo run --release -p gpui-kit-grid --example blotter` runs the windowed `blotter` under the S2 load and prints its counters.
