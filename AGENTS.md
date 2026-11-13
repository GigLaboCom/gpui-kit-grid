# AGENTS.md

This file provides shared guidance for coding agents working in this repository.
Keep project rules here; `CLAUDE.md` imports this file for Claude Code.

The rules are adopted from gpui-kit (`AGENTS.md`, `CONTRIBUTING.md`,
`.agents/COMPONENT_TEST_RULES.md`, CI) and extended with rules specific to a
live-updating grid. Where this file and gpui-kit disagree, gpui-kit wins for
general component rules, and this file wins for grid-specific rules.

## Canonical Design and Coding Guides

Before changing UI, interaction, interface language, layout, styling,
components, or application architecture, read and follow gpui-kit's canonical
guides:

- Design Guides: `../gpui-kit/website/docs/design-guides.md`
  (<https://gpui-kit.com/docs/design-guides.md>)
- Coding Guides: `../gpui-kit/website/docs/coding-guides.md`
  (<https://gpui-kit.com/docs/coding-guides.md>). Sections most relevant here:
  "Lists, tables, and large data", "Public API design", "Performance rules",
  "Rules for coding agents", "Implementation checklist".

Reference the canonical `website/docs/` originals, not the vendored copies under
`skills/`: on gpui-kit `next` the copies have drifted.

These guides are requirements, not optional inspiration. Review the finished
work against both guides before considering it complete.

The maintainers keep the grid's design notes and decision log outside the
repository (an untracked `docs/` folder in their checkouts). No rule here
depends on them: the rules below, the rustdoc and the code comments carry
their own reasons. Where a checkout has the notes, a change to a recorded
decision updates them too.

## Replaceable Behavior: Defaults Are Not Hard-Coded

Most of the grid's behaviors were settled by accepting a default, not by a
final product decision. Treat every such default as **the first implementation
of a replaceable behavior**:

- **When it applies.** A behavior gets a seam if it was settled by accepting a
  default, or if a different behavior is plausible or more precise (a reviewer
  can name a second reasonable variant).
- **What the seam looks like.** A trait (strategy) and a default implementation
  next to it. The default is created by a factory: a `Default` impl, or a
  `…Factory` trait when construction needs context. It is injected through the
  builder (`with_<seam>(…)`).
- **What is forbidden instead:**
  - hard-coded branches;
  - boolean flags that grow into modes;
  - `match` on a mode enum spread across modules.
- **Built-in vs. pluggable.** Industry standard behavior is built in.
  Alternatives ship as more implementations of the same trait, or live in the
  application.
- **Patterns.**
  - Strategy and Factory for behaviors.
  - Dependency injection through builders.
  - Composition over inheritance; open/closed.
  - Events (`EventEmitter`) for observers; GPUI Actions for commands.
  - One seam per behavior. Do not build one "plugin registry" that knows everything.
- **Hot path stays fast.** Strategies called per cell or per frame use static
  dispatch: a generic parameter, or an enum of built-ins plus `Custom(Arc<dyn …>)`.
  Alternatively they are invoked once per batch or frame. Never allocate a
  `Box<dyn …>` per cell. Strategies used by ingest or the engine are
  `Send + Sync`.
- **Documented and tested.** Every seam is documented on its trait and on the
  `with_` builder that injects it; the view's seams are also listed in the
  `policy` module docs, and adding or changing one updates that list. The
  default implementation has tests. Engine strategies all pass the same
  reference-model property tests.
- **Follows gpui-kit API rules.** Private fields, `with_` builders, no `Kind`
  suffix, names after the role (`ReorderPolicy`, `ProjectionEngine`,
  `CellFormatter`).

## Project Overview

`gpui-kit-grid` is a live-updating data grid for gpui-kit applications:
key-addressed rows written from any thread, background sorting, filtering,
grouping and tree projection, per-cell change highlight, per-cell style hooks,
key-based selection, frozen columns, layout persistence and export.

It is a separate crate that depends on `gpui-kit` alone (`use gpui_kit::*;`,
`gpui_kit::component`, `gpui_kit::base`). It never depends on GPUI directly;
`script/check-gpui-kit-pin.ts` enforces that.

Layout:

- `crates/grid` - the component (package `gpui-kit-grid`), with its example
  applications (`crates/grid/examples`: blotter, editable orders, benchmarks)
- `crates/grid-macros` - `#[derive(GridColumns)]` (the grid's `derive` feature)
- `crates/grid-feed`, `crates/grid-gateway` - the web example's quotes feed
- `crates/grid-web` - the examples in the browser, a separate workspace

## Common Commands

```bash
# Lint check (CI: macOS); again with the off-by-default `decimal` feature
cargo clippy --workspace --all-targets --features gpui-kit-grid/test-support -- --deny warnings
cargo clippy --workspace --all-targets --features gpui-kit-grid/test-support,gpui-kit-grid/decimal -- --deny warnings
cargo clippy --workspace --all-targets --features gpui-kit-grid/test-support,gpui-kit-grid/derive -- --deny warnings

# The library alone, without features, and for the browser
cargo check -p gpui-kit-grid
RUSTC_BOOTSTRAP=1 cargo check -p gpui-kit-grid --lib --target wasm32-unknown-unknown --features decimal,derive

# The web examples: a separate workspace (crates/grid-web), the shell every
# page shares (shell/) and one crate, module and page per example
# (apps/<app>/, www/<app>/); a new example is also one more name in
# scripts/build-wasm.sh
(cd crates/grid-web && RUSTC_BOOTSTRAP=1 cargo clippy --workspace --target wasm32-unknown-unknown -- --deny warnings)
(cd crates/grid-web && cargo fmt --all --check)
make -C crates/grid-web build      # release build of every example; APP=desk for one;
                                   # hb-subset (HarfBuzz) cuts Iosevka down for the pages
make -C crates/grid-web serve      # http://127.0.0.1:8080; PORT=8091 for another port
make -C crates/grid-web survive    # headless Chrome; also examples, input, measure
make -C crates/grid-web embed-check  # <live-grid> on another site; on every gpui-kit/GPUI upgrade
make -C crates/grid-web embed-check APPS=all  # and every example inline and in a frame, on recordings
make -C crates/grid-web site-dist API=target/doc  # the site build as files in site/dist/demo/ (after bun run build)
make -C crates/grid-web posters      # the site's posters (site/public/posters/), offline inputs only
make -C crates/grid-web measure QUERY='app=ops&storm' RATES=1,1,1  # ops in a storm

# Format check (CI: enforced)
cargo fmt --all --check

# Spell check (installed locally: cargo install --root target/tools typos-cli)
typos

# Unused dependencies (cargo install --root target/tools cargo-machete@0.9.1)
cargo machete

# Git hooks, once per clone: pre-commit refuses secrets (gitleaks), local-only
# files and private data; commit-msg refuses agent attribution; pre-push
# repeats the secret and private-data scan on what leaves the machine. Both
# refuse to run without gitleaks (on PATH or target/tools/bin/gitleaks).
# Private patterns go to <git common dir>/info/privacy-patterns, never
# committed. The custom-docs submodule, when checked out, gets the same hooks.
script/install-hooks

# Minimal example (the site's Getting started page walks through it)
cargo run --release -p gpui-kit-grid --example quickstart

# Recipes the site's Cookbook page shows (COOKBOOK_FEED=ws://127.0.0.1:9001
# feeds it from the quotes gateway)
cargo run --release -p gpui-kit-grid --example cookbook

# The documentation site (site/, Astro Starlight): build, type check, and
# the checks that code on its pages and in the README comes from regions
# (`// #region name`) of compiled files, and that internal links resolve
bun install --cwd site --frozen-lockfile
bun run --cwd site build
bun run --cwd site check
bun test --cwd site

# The site as a container image (nginx, docker/site/), from the site/dist
# CI assembles (the pages, the demos under /demo/, rustdoc under /api/): the
# context, the image, then a read-only run against Cloudflare's behavior
docker/site/stage.sh target/site-image
docker build -t gpui-kit-grid-site target/site-image
docker/site/smoke.sh gpui-kit-grid-site

# A release of the site (tags site-vX.Y.Z; vX.Y.Z is the crate's): what
# reached the site since the last one, then the tag that publishes the image,
# deploys to Cloudflare and opens the GitHub release
script/release-site
script/release-site 0.2.0 --push

# Tree example: a live portfolio
cargo run --release -p gpui-kit-grid --example portfolio

# Editing under live updates, widgets, export and drag and drop (stage 3)
cargo run --release -p gpui-kit-grid --example orders

# Munich by the timetable: MVV's open GTFS feed, downloaded and compiled
# once (a newer feed is taken and logged; --pin refuses it; never in a
# loop), then the example and its PNG snapshots; TRANSIT_REGIONAL=1 with the
# regional trains and buses
python3 -I script/fetch-transit-data
cargo run --release -p gpui-kit-grid --example transit
TRANSIT_SNAPSHOT=target/transit cargo run --release -p gpui-kit-grid --features test-support --example transit

# Helsinki live from HSL's HFP (one connection to mqtt.hsl.fi; keep live
# sessions short and few): the network once, then the example;
# TRANSIT_SOURCE=replay plays the recording made with TRANSIT_RECORD=<file>
python3 -I script/fetch-transit-data --helsinki
TRANSIT_CITY=helsinki cargo run --release -p gpui-kit-grid --example transit
TRANSIT_SNAPSHOT=target/helsinki TRANSIT_CITY=helsinki TRANSIT_SOURCE=replay cargo run --release -p gpui-kit-grid --features test-support --example transit

# Wikimedia's live edits (one connection to stream.wikimedia.org; keep
# development runs short); WIKI_SOURCE=replay plays a recording made with
# WIKI_RECORD=<seconds>
cargo run --release -p gpui-kit-grid --example wiki

# Every vessel around Finland, live from Digitraffic's AIS (one connection;
# keep live sessions short and few); SHIPS_RECORD=<file> records a session,
# SHIPS_SOURCE=replay plays the recording, SHIPS_SNAPSHOT=<dir> draws PNG files
cargo run --release -p gpui-kit-grid --example ships
SHIPS_SNAPSHOT=target/ships SHIPS_SOURCE=replay cargo run --release -p gpui-kit-grid --features test-support --example ships

# This machine's processes and GPU, live (macOS: ioreg; Linux: DRM fdinfo;
# the GPU columns stay blank elsewhere); PERF_SNAPSHOT=<dir> draws PNG files,
# which show this machine's processes and users: never commit them;
# docker/linux/perf.sh runs both on Linux (Mesa, Xvfb) into out/linux/perf/
cargo run --release -p gpui-kit-grid --example perf
PERF_SNAPSHOT=target/perf cargo run --release -p gpui-kit-grid --features test-support --example perf
docker/linux/perf.sh

# Spike benchmark and windowed example (macOS)
cargo run --release -p gpui-kit-grid --features test-support --example spike_bench
BLOTTER_SECONDS=10 ROWS=100000 RATE=10000 cargo run --release -p gpui-kit-grid --example blotter

# GPUI only through a pinned gpui-kit
bun script/check-gpui-kit-pin.ts

# Tests, including headless UI tests; again with `decimal`
cargo test --workspace --features gpui-kit-grid/test-support
cargo test --workspace --features gpui-kit-grid/test-support,gpui-kit-grid/decimal
cargo test --workspace --features gpui-kit-grid/test-support,gpui-kit-grid/derive

# Doc tests
cargo test --workspace --features gpui-kit-grid/test-support --doc
```

System dependencies: `script/bootstrap` (Linux packages via
`script/install-linux.sh`), `script/install-window.ps1` on Windows.

### Performance Profiling

```bash
# View FPS on macOS (using Metal HUD)
MTL_HUD_ENABLED=1 cargo run -p <example>

# Profile performance using samply
samply record cargo run -p <example>
```

## Architecture Constraints

- Do not modify gpui-kit from this repository. If gpui-kit lacks something,
  say so in the pull request (or an issue upstream) and work around it with
  public API only.
- Use only public gpui-kit API. `pub(crate)` parts of gpui-kit (column resize
  handles, `TableOptions`, the component's i18n catalog and actions module) are
  reimplemented here, not copied with visibility changes.
- Keep the `model` and `engine` modules free of GPUI types, so they can be
  promoted to `gpui-base` later as a Base/Component split like dock.
- Theme APIs expose semantic tokens; do not add an ever-growing set of
  component-specific styling fields. Reuse `table_*` and semantic tokens.
- `DataTable`, `TableState` and `TableDelegate` are not wrapped or extended;
  this grid is independent of them.

## Component Design Principles

Copied from gpui-kit `AGENTS.md`:

1. **Stateless design**: Use `RenderOnce` trait, components should be stateless when possible
2. **Size system**: Supports `xs`, `sm`, `md` (default), `lg` sizes via `Sizable` trait.
3. **Mouse cursor**: Buttons use `default` cursor not `pointer` (desktop app convention), unless it's a link button
4. **Style system**: Provides CSS-like styling API via `Styled` trait and `ElementExt` extensions
5. **Base controls are no-style**: Base controls and parts do not install layout,
   positioning, colors, sizing, gaps, radius, borders, shadows, variants, or animation.
   Complete presentation belongs to the styled layer or the application.
6. **GPUI builder style**: Keep element construction as one fluent builder chain. Express
   conditions with `when`, `when_some`, `when_none`, and `map`; do not split a chain into a
   mutable temporary element followed by imperative reassignment when the builder API can
   express the same operation.
7. **No `pub` fields on public data types**: A public struct keeps its fields private, is
   constructed with a builder, and is read through methods. Setters and readers must not
   collide: an all-boolean type names setters after the field and readers
   `is_<adjective>`/`has_<noun>`, never `can_`; a type with non-boolean fields prefixes
   every setter with `with_` and keeps the field name for readers. Value types whose fields
   are the definition and cannot grow (`Point`, `Selection`, `Edges`) are exempt.
8. **Spell `Context` out**: Name a context type `CellStyleContext`, never `…Ctx`.
   `cx` is reserved for GPUI's `App`, `Context<T>`, and `AsyncApp`.

## Grid Rules

These are not optional:

- **Identity by key, never by position.** Rows, selection, focus, anchors,
  element ids, animation ids and persisted layout are keyed by row key and
  `ColumnId`. Positions (`ViewColumnIx`, display row index) are transient.
- **Two column indices are two types.** `ColumnIx` (schema) and `ViewColumnIx`
  (on-screen position) are distinct newtypes; never convert through `usize`.
- **Hot-path hooks do not allocate or lock.** `GridRow::value`, `cell_style`,
  `row_style` and cell renderers run for every visible cell on every frame.
- **Delegate hooks take `&self` and never read or notify the grid entity**
  (re-entrant borrow panics). Per-frame hooks (`GridRow::value`,
  `cell_style`, `row_style`, cell renderers, `render_cell` for `Element` and
  `Control` columns, and `render_edit_message` while a field shows a message)
  run only in prepaint. Command and UI hooks may run outside prepaint:
  `context_menu` and `primary_command` in `window.defer`, `header_tooltip` and
  `render_progress` in `render`, and `is_editable`, `validate_edit` and
  `drag_payload` once per gesture. Commands leave the grid as
  `LiveGridEvent`s.
- **`LiveGridState` is a view** (`impl Render`, embedded with `.child(state)`);
  otherwise a cached dock panel never repaints it.
- **Applications place the `LiveGrid` element** (`LiveGrid::new(&state)`, a
  `RenderOnce` wrapper like gpui-kit's `DataTable`), not the state entity. Its
  size comes from `Sizable` (`xsmall`/`small`/`medium`/`large`: 26/30/32/40 px
  rows, as gpui-kit tables); `bordered(false)` drops the frame.
- **Rows are owned by the background ingest task.** The UI thread only swaps
  snapshots: one `update` and one `notify` per pull, at most once per 8–16 ms.
- **No `request_animation_frame` for highlights.** Fades step on one global
  tick with a single deadline timer; honor reduced motion.
- **Store clock only** (`GridStore::now()`, the store's `Clock`; by default the
  executor clock `cx.background_executor().now()`), never wall-clock time.
  `web_time::Instant::now()` is allowed only for profiling durations. Never
  `std::time::Instant::now()`: it panics on wasm, where `web_time` reads the
  browser clock (on native targets the two are the same type).
- **The library builds for `wasm32-unknown-unknown`.** No dependency that
  does not build there (`smol` and `async-io` are dev-dependencies only;
  channels come from `async-channel`). On the single-threaded web platform
  background jobs run on the main thread.
- **No work on the UI thread that grows with the total row count** in a
  frame; sorting, filtering, grouping and aggregation run on a snapshot in the
  background (or chunked when no background executor exists).
- Every behavior change to ingestion, the engine or painting carries a
  benchmark or profile in the PR (`## Performance`).

## No Agent Attribution

Applies to every agent (Claude, Codex, Copilot or any other), and overrides
any default attribution a tool adds:

- Commits, pull requests, tags and release notes carry no `Co-Authored-By:`
  trailer for any model or agent.
- They carry no "Generated with …" line, badge or link either.
- The author is the repository's own git identity (local `user.name` /
  `user.email`). Do not change it, do not commit with `--author`, and do not
  enable commit signing with another key.

## Code Style

Copied from gpui-kit `AGENTS.md`:

- Follow naming and organization patterns from existing code
- Reference macOS/Windows control API design for naming
- AI-generated code must be refactored to match project style
- Mark AI-generated portions when submitting PRs
- When creating a PR, inspect previous PR titles in the repository and match
  that style (gpui-kit uses `area: Sentence summary`). Do not blindly use
  conventional prefixes like `fix:` or `feat:`.
- When a PR adds, changes or removes public API, list every item under a
  `## Public API` section of the description, with its signature and one line on
  its purpose. Changes to existing items also go under `## Breaking Changes`
  with `diff` blocks showing the old and new usage.
- Avoid `Kind` as a type-name suffix. Name the type after what its variants
  _are_ instead.
- Vocabulary (gpui-kit Coding Guides): `<Control>State`, `<Control>Event`,
  `<Role>Delegate`; `render_<part>`, `on_<event>`, `set_` for `&mut` mutation;
  `_ix` for indices, `Id` for identifiers; no new `can_` readers.

## Internationalization

Uses `rust-i18n`, with this crate's own catalog (gpui-kit's catalog is
crate-local). Only add `en`, `zh-CN`, `zh-HK` (gpui-kit rule); add `ru` only
once the project decides to ship it. Keys describe intent.

## Documentation

- `README.md`, `ROADMAP.md` and the documentation site's pages
  (`site/src/content/docs/`) are written in English: they are the crate's
  public face, as in gpui-kit, and the only documents in the repository
  besides the rules (`AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`). The
  README is the front page and points to the site, where the guides live;
  the site's roadmap page is `ROADMAP.md`, read at build time. Code on the
  pages and in the README comes only from `// #region name` blocks of
  compiled files (the examples, `examples/cookbook.rs` for recipes), and
  `bun test --cwd site` checks it.
- The maintainers' design notes (`docs/`) are kept **locally only**: the
  folder is in `.gitignore` and is never committed or pushed; never
  `git add -f` it. The repository stands on its own without them: no README,
  ROADMAP, site page, rule, rustdoc or code comment links into them or cites
  their section numbers or decision ids. A comment states its reason in place.
- Code comments, rustdoc, commit messages, PR and issue text are in English
  (gpui-kit rule: PR descriptions are in English).
- Working documents written by or for agents (step specs, drafts, notes) go to
  `custom-docs/`. Nobody reads them as project documentation. The folder is
  an optional submodule backed by a private repository
  (`GigLaboCom/gpui-kit-grid-docs`): `.gitmodules` sets `update = none`, so a
  clone, even `--recursive`, skips it; maintainers fetch it with
  `git submodule update --init --checkout custom-docs`. Its files are
  committed and pushed in the submodule only, never in this repository (the
  pre-commit hook refuses them), and they are not moved into `docs/`.
  Anything that has to outlive a working document goes into the design
  notes, or into rustdoc and code comments when a reader of the code needs
  it.

## Platform Support

- macOS (aarch64, x86_64)
- Linux (x86_64)
- Windows (x86_64)

CI runs the full test suite on each platform.

## Testing Guidelines

See `.agents/COMPONENT_TEST_RULES.md`.

Per-change verification is automated (gates, unit and property tests,
headless UI tests with `#[gpui_kit::test]`, the headless benchmark, Linux
e2e); windowed and manual UI checks are batched into one final manual pass
at the end of each roadmap stage.
