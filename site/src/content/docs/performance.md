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

### Against DataTable under the S2 load, sorted and scrolled

On S1 nothing moves, so it compares painting only. S6c puts `DataTable` under the S2 load: 100k rows × 40 columns, 166 updates a frame on random keys (10k a second), shown sorted by the column the updates change, descending. S6d also scrolls to a new page every frame. `DataTable` addresses rows by index and calls its delegate on the UI thread, so the delegate keeps the order there, in one of three ways: it sorts the whole order every frame (what an application writes first), removes and reinserts each updated row by binary search, or removes the changed rows in one pass and merges them back, redrawing only when the rows on screen changed. Every delegate stores rows by key and formats every row once at setup, as the fast delegate of S6b does, so a cell only clones a string. S2c and S3c run the grid under the same load and scrolling.

The figure is the UI thread's time per frame. For `DataTable` it is the update, the upkeep of the order and the draw; for the grid, the draw and the pull of the store's snapshot, while the grid's rebuilds run on a background thread. When the test platform draws more than once in a 16 ms interval, only the longest draw counts, as a display draws once. Release build of `472ff88` (`SPIKE_STORM_ONLY=1`), two runs, on a machine that was not idle (load average 7–28).

| Scenario | p50, ms | p99, ms | max, ms |
|---|---|---|---|
| Grid, S2 load (S2c) | 0.01 | 1.38–1.51 | 1.51–2.14 |
| The same, the 66 frames of 300 that draw | 1.20–1.27 | 1.45–1.90 | 1.51–2.14 |
| `DataTable`, sort every frame (S6c) | 13.5–13.6 | 16.4–16.5 | 19.8–42.5 |
| `DataTable`, binary search per update (S6c) | 13.1–13.5 | 14.2–17.8 | 15.7–33.8 |
| `DataTable`, one-pass merge, redrawn on change (S6c) | 0.33–0.34 | 12.7–13.0 | 13.2–13.7 |
| The same, the 22 frames of 300 that draw | 12.4–12.7 | 13.2–13.7 | 13.2–13.7 |
| Grid, S2 load and a new page every frame (S3c) | 1.21–1.22 | 2.85–3.14 | 4.41–4.48 |
| `DataTable`, the same, sort every frame (S6d) | 36.4–36.8 | 38.6–50.9 | 39.1–72.3 |
| `DataTable`, the same, binary search per update (S6d) | 36.4–38.0 | 41.4–44.9 | 42.4–56.6 |
| `DataTable`, the same, one-pass merge (S6d) | 34.2–35.4 | 37.2–38.1 | 37.9–38.9 |

Keeping the order costs `DataTable` little: 1.5 ms a frame to sort everything (p99 3.0–3.5 ms), 0.3–0.4 ms to merge. The draw is the cost: about 12 ms for a page that changes in place, 34–37 ms for a new page every frame. While the view scrolls under the load, the grid's UI thread works 28–29 times less than with the best `DataTable` delegate at the median and 12–13 times less at p99. With the view still, both skip the frames in which nothing on screen changed, and a frame that draws costs the grid about 10 times less. The grid's rebuilds took p50 3.6–3.8 ms and p99 4.0–6.6 ms on the background thread, one every 100 ms.

The comparison leaves out three things. The grid shows a new order at most every 100 ms (its rebuild interval), the `DataTable` delegates every frame. Outside the measured subtree GPUI spends 0.4 ms a frame with the grid and 1.6–1.9 ms with `DataTable`. The grid's ingest runs on a background thread, which in the headless test is the main thread: counting it, installing each rebuilt order and every extra draw, the grid's main thread with the view scrolling takes at most p50 3.2 ms and p99 8.6–8.8 ms, against 36–37 ms and 39–40 ms for the best `DataTable` delegate.

### Every visible cell changing every frame (S43)

S43 writes a new value into every column of every painted row, every frame: about 1,450 cells on screen, 1,400 of them changed (all but the Id, which is the row's key), so 84k cell changes a second at 60 fps. Each new value reads differently from the last: prices move by 0.02 to 1 and show two decimals, sizes move by 1 to 100, and each text column takes another text. With "texts from the pools" the texts come from the benchmark's pools of a few hundred, so they repeat and a text cache can keep them; with "every text new" each carries a counter and has never been shown before, so every changed text has to be shaped. Each runs with the view still (sorted by the key) and with the S2 load on top (166 more updates a frame on random keys, sorted by the column the updates change). `DataTable` runs the same through the fast delegate of S6c: cells formatted once per change, rows kept by key, and under the S2 load the order kept by the one-pass merge.

The figure is the UI thread's time per frame, as in S2c and S6c: for the grid the draw and the snapshot pull, for `DataTable` the update, the upkeep of the order and the draw. Release build of `7b9fa2b` (`SPIKE_EVERY_CELL_ONLY=1`), two runs, load average 3.4–4.4.

| Scenario | p50, ms | p99, ms | max, ms |
|---|---|---|---|
| Grid, view still, texts from the pools | 1.08–1.09 | 2.42–2.45 | 4.72–4.74 |
| `DataTable`, the same | 32.6–33.1 | 36.1–36.9 | 36.8–60.1 |
| Grid, view still, every text new | 14.5 | 15.6–17.6 | 19.6–20.2 |
| `DataTable`, the same | 41.2–41.4 | 43.9–46.8 | 50.3–80.2 |
| Grid, S2 load, texts from the pools | 1.21–1.23 | 2.54–2.66 | 4.50–4.67 |
| `DataTable`, the same | 33.4–33.5 | 35.6–36.3 | 36.7–42.8 |
| Grid, S2 load, every text new | 14.6 | 15.7–15.9 | 17.8–18.9 |
| `DataTable`, the same | 41.7–41.9 | 44.6–51.3 | 46.4–84.1 |

The text cache does hide a cost, and the two variants show how much. The grid lays numbers out from a table of digit glyphs and never shapes them: 70% of its cells. Texts it shapes once and keeps; with pooled texts 0.1% of the painted cells needed shaping. When every text is new, about 420 texts a frame (29% of the cells) go to the platform's shaper, and the frame takes 14.5 ms instead of 1.1 ms: over the 8 ms limit of S1, though inside a 16 ms frame at p99 in three of the four runs. `DataTable` shapes every text that was not on screen in the frame before (GPUI's line cache keeps two frames): an estimated 75% of its cells with pooled texts, 96% with every text new. Free text that changes in every visible cell every frame is the one load here under which the grid's frame is not small; numbers, and texts from a bounded set, keep it near 1 ms.

Under the S2 load the grid's main thread also runs the ingest of the updates, a background task that the headless test runs on it: counting that and every extra draw, p50 3.3 ms and p99 7.6–7.8 ms with pooled texts, 16.8–16.9 ms and 22.4–22.6 ms with every text new.

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

## From a write to the picture (S44)

S44 measures how long a value takes from the producer's write call to the screen. A producer thread writes to a store whose ingest and rebuilds run on real background threads, and every millisecond it also writes a probe: a row at the top of the view whose painted column carries a sequence number. A watcher thread subscribed to the store notes when each probe is published. The harness ticks the headless window every 16 ms of the wall clock, as a display would, and each tick runs what the main thread does: the grid's pull of the snapshot and the draw. Each draw notes which probe it painted.

For every probe written while the view stays (about 10,000 per cell), the time runs from the write call to the end of the first draw that shows it or a later value. The end of the draw is the end of the grid's paint on the main thread. GPUI's work after it (0.2–0.6 ms, measured apart), the GPU, the compositor and the display are not in the figure; on a real screen they add about a frame. The figure splits into write → publish (the queue and the store's 8 ms pause between publishes), publish → pull (mostly the wait for the harness's next tick: a display's frame interval costs the same wait, and the grid pulls at most every 8 ms), and pull → end of draw. The grid's own part is write → publish plus pull → end of draw. Release build of `7b9fa2b` (`SPIKE_PICTURE_ONLY=1`), 10 s per cell, two runs, load average 2.8–4.0. Figures are p50 / p99 / max in ms; ranges span both runs.

| Load | Write → picture | Write → publish | Publish → pull | Pull → end of draw | Grid's own part |
|---|---|---|---|---|---|
| S2: 100k rows, 10k updates/s on random keys, sorted by a changing column | 12.6–12.7 / 25.4–26.0 / 34–54 | 6.0 / 11.9–12.2 / 31–33 | 4.7 / 15.1–16.2 / 18–20 | 1.2–1.3 / 2.2–2.6 / 3.1–8.5 | 7.3–7.4 / 13.6–13.7 / 34–36 |
| S42 storm: Zipf over 10k keys, 100k events/s, grouped | 12.4–12.6 / 24.5–25.2 / 29–31 | 5.4–5.6 / 11.0–11.2 / 13–14 | 5.4–5.6 / 15.7–16.6 / 17–18 | 1.2–1.3 / 2.2–2.4 / 4.8–4.9 | 6.6–6.9 / 12.7–12.8 / 16–17 |
| S42 storm: Zipf over 10k keys, 1M events/s, grouped | 13.0–13.2 / 25.6–25.9 / 34 | 6.1–6.4 / 12.2–13.0 / 16–17 | 5.2–5.6 / 15.8–16.4 / 19 | 1.0 / 1.6–1.8 / 2.1–2.7 | 7.0–7.5 / 13.4–14.4 / 18 |

A value reaches the end of a draw in about 13 ms at the median and 25–26 ms at p99, at 10k, 100k and 1M events a second alike. Half of that is the harness's frame: the wait for the next tick. Of the grid's own 7 ms, most is the store's pause between publishes, which batches writes (`StoreOptions::with_publish_interval`); the pull and the draw take 1–1.3 ms. The frame on the UI thread stayed at p99 1.8–2.9 ms in every cell. The largest values, 34–54 ms in the S2 cell, come from rare write → publish spikes of 31–33 ms, seen in both runs and not in the storm cells; their cause is not yet known.

## Thirty minutes of the S2 load (S45)

The other scenarios run for seconds. S45 runs the S2 cell of S44 for 30 minutes: 100k rows × 40 columns, 10k updates a second on random keys from a producer thread, sorted by a changing column, the probe every millisecond, the window ticked every 16 ms of the wall clock. For the first 10 s of every minute the view scrolls to a new page every tick, then returns to the top. The frame is the UI thread's time per tick (the draw and the snapshot pull); write → picture counts only while the view stays. Resident memory is sampled every 2 s, highlight memory at the end of each minute. Release build of `7b9fa2b` (`SPIKE_SOAK_MINUTES=30`, not part of the full run), one run, load average 2.9 at the start and 3.9 at the end.

| Over the whole run | Result |
|---|---|
| Frames (ticks) | 112,290 |
| Frame p50 / p99 / p99.9 / max | 1.29 / 2.28 / 4.16 / 230 ms |
| Frames over 8 ms; over 16 ms | 17; 6 |
| Write → picture p50 / p99 | 13.0 / 28.9 ms, over 1.5 million probes |
| Rebuilds (background) p50 / p99 | 4.6 / 7.7 ms, 17,520 jobs |

| Minute | Frame p99, ms | Write → picture p99, ms | Queue max | Highlight memory, MB | Resident memory, MB |
|---|---|---|---|---|---|
| 1 | 2.21 | 24.9 | 125 | 2.29 | 296 |
| 10 | 2.00 | 25.9 | 132 | 2.79 | 245 |
| 20 | 1.83 | 25.4 | 139 | 2.47 | 255 |
| 30 | 2.27 | 34.0 | 191 | 2.79 | 244 |

Nothing drifts. Frame p99 stays between 1.8 and 2.9 ms in every minute, highlight memory between 2.3 and 2.8 MB, the grid's text cache at about 190 entries, and resident memory falls from about 300 MB in the first minutes to 240–270 MB, where it stays (237 MB at the end). The queue between the producer and the ingest holds at most 120–190 writes in a typical minute. Six frames went over 16 ms, four of them in one minute, the longest at 230 ms, with the queue at 519 writes and one rebuild at 456 ms in the same minute: a stall of the whole process rather than of one frame. Two stretches of three minutes (minutes 12–14 and 28–30) had about 120 late ticks a minute instead of 10–20, and write → picture p99 rose from about 25.5 ms to 33.5 ms there while the frame did not change: the harness's sleeps overran, which points at the machine. The benchmark ran on a desktop that was not otherwise idle, and neither cause was confirmed.

## In the browser

On GPUI's web platform the rebuilds run on the main thread. [Web](../web/#figures) has frame times for each demo page in headless Chrome.

## Reproduce a run

On macOS, from a checkout of the repository:

```sh
mkdir -p target
cargo run --release -p gpui-kit-grid --features test-support --example spike_bench > target/bench-run.md
python3 -I script/spike_charts.py target/bench-run.md target/bench-charts
```

The first command runs every scenario and writes a Markdown report with each result against its limit; the second draws the charts from it. `SPIKE_EVERY_CELL_ONLY=1` and `SPIKE_PICTURE_ONLY=1` run S43 or S44 alone, and `SPIKE_SOAK_MINUTES=30` runs the soak (S45) and nothing else. Close other applications first: the tails (p99) are sensitive to a busy machine.

For an application of your own, `MTL_HUD_ENABLED=1` shows the frame rate on macOS, and `samply record cargo run --release --example <name>` profiles it. `BLOTTER_SECONDS=10 ROWS=100000 RATE=10000 cargo run --release -p gpui-kit-grid --example blotter` runs the windowed `blotter` under the S2 load and prints its counters.
