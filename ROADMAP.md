# Roadmap

Where gpui-kit-grid stands and what may come next.

| Stage | Contents | Status |
|---|---|---|
| 0. Spike | Ingest, snapshots, the custom body element, background rebuilds, highlight on a global tick | Done |
| 1. Core | Keys, sorting, quick filter, selection by key, frozen columns, resize, reorder, autofit, bands, styles, tooltips, menus, Actions, copy, layout, accessibility | Done |
| 2. Structure | Grouping, group panel, aggregates, footers, totals, tree, direction arrows, fading removed rows, grouping in the layout, copy with groups | Done |
| 3. Input and output | Time, icon, image and custom cell values, elements in cells, editing under live updates, export to files, row drag and drop, column chooser, `LiveGridHandle`, `#[derive(GridColumns)]` | Done |
| 4. Polish | See below | Not scheduled |

## Stage 3: input and output

Done. The final benchmark run is on the documentation site's Performance page. There was no separate manual pass in real windows: VoiceOver, input methods, CSV files in spreadsheet applications and the two editing defaults below are covered by the headless tests and the Linux end-to-end suite only, so problems found in use are fixed as they are reported.

| Item | What it adds |
|---|---|
| Cell values | `Timestamp`, `Icon`, `Image` (raster images and colour SVGs through a replaceable image source) and `Custom` values of the application's own type, sorted and exported by their own rules |
| Elements in cells | Real gpui-kit elements (`Element` and `Control` columns, `render_cell`) for visible rows only, keyed by row key and column; the keyboard focus stays with the grid |
| Cell editing | An editor per column, Excel's gestures (`EditGesture`), validation with a message under the cell, built-in text, number, select, date and checkbox editors |
| Editing under live updates | The field is bound to the row key; the draft survives updates and shows conflicts; the order is held while a field is open; asynchronous commits (`EditCommit` → `resolve_edit`) with an optimistic value and an error mark (`EditPolicy`); no undo |
| Export to files | CSV built in; export in the background from one snapshot to a `Write` sink, a file or a callback, with progress and cancel; table metadata for an application's own xlsx. No xlsx dependency in the grid |
| Row drag and drop | A two-phase gesture (`RowDragGesture`), application data on the drag (`drag_payload`), drops onto rows, between rows or onto group captions (`RowDropPolicy`), `LiveGridEvent::Drop`. Off by default |
| Columns | Icon headers, columns that cannot be hidden, a column chooser popover, and `LiveGridHandle`, so a toolbar can drive a grid without knowing its delegate type |
| `#[derive(GridColumns)]` | A derive macro in `crates/grid-macros`, behind the `derive` feature; the manual implementation stays |
| Example `orders` | Live orders edited in place while updates stream in |

Every default ships as the first implementation of a replaceable behavior. Two of them change earlier behavior: a double click on an editable cell edits it instead of running the primary command, and `+` and `-` on an editable cell type into the draft instead of expanding or collapsing. A custom `EditGesture` restores either in one line.

**Left open by stage 3:**
- whether cells with elements need row and column indices for screen readers on Windows;

**Gaps in gpui-kit and GPUI, worked around:**
- `Select` and `DatePicker` cannot be opened from code, and `Select` passes Enter on: the grid's list and calendar editors are its own, on gpui-kit's public `List` and `Calendar`;
- `Input` has no error state, and GPUI has no `aria_invalid`: the editor draws the red frame, and the error goes into the accessible description;
- gpui-kit's number mask knows only `.`: with another decimal separator the field takes any text, and the parser refuses what is not a number;
- `Checkbox` and `Button` are Tab stops with their own focus: elements in cells set `tab_stop(false)`, and the grid takes the focus back after a press;
- `Checkbox` has no mixed state: a band in the column chooser shows "N of M" as text;
- `Popover` needs a trigger: the column chooser is the grid's own layer, as the context menu is;
- GPUI has no drag-end callback and no Escape cancel for drags, and it drops only onto the element under the pointer: the grid clears its drop target itself, cancels on Escape, and an element in a cell or an open editor takes no drop;
- typing through an IME does not start an edit, because the grid's root has no text input handler; after F2 the IME works in the field.

## Stage 4: polish (not scheduled)

- Documentation for application authors.
- More languages in the catalog (`en`, `zh-CN`, `zh-HK` today; `ru` waits for a decision).
- Whether to offer the grid upstream to gpui-kit, and whether to split it into a Base and a
  Component part as gpui-kit's dock is.

## Replaceable behaviour follow-ups (not scheduled)

The repository rule is that every accepted default ships as the first implementation of a
replaceable behaviour (a trait, a default and a `with_*` builder; AGENTS.md). Three follow-ups
are recorded:

- **Accessibility as a seam.** The accessibility tree is planned inside the grid. Today only
  the grid's name (`LiveGrid::accessibility_label`) and the text sources (cell text, tooltips,
  the catalog) can be changed. The planned seam is `AccessibilityPolicy`, injected with
  `with_accessibility_policy`. It would have three variants:
  - a built-in `StandardAccessibility`, today's behaviour;
  - a built-in `NoAccessibility`, the root node only;
  - `Custom(Arc<dyn …>)`.

  Its hooks would cover the root role and name, row and cell labels and descriptions
  (captions, footers, totals, trend, aggregate function), the level as text, header row
  numbering, and whether changes are announced. The planner runs only while a screen reader
  listens, so the cost stays zero otherwise.
- **Audit of stage 1–2 defaults.** Check every accepted default in
  the decision log against the code. Small gaps (a hard-coded branch, a
  `bool` that could grow into modes) become seams or plain builder parameters. Larger ones are
  listed with a proposed seam.
- **Edit policy at run time.** `with_edit_policy` is a builder only; switching between keeping and
  following drafts means building the grid again with its layout. A `set_edit_policy` would match
  `set_reorder_policy`.

## Look and feel follow-ups (not scheduled)

Today the grid's look comes from three places:
- **Colours** come from gpui-kit theme tokens, such as `table`, `table_head`, `table_foot`, `table_even`, `table_active`, `table_row_border`, `border`, `foreground`, `muted_foreground`, `success`, `warning`, `danger` and `ring`.
- **Sizes:**
  - row height is set by `Sizable` (`xsmall` 26, `small` 30, `medium` 32, `large` 40 px, or any `Size::Size(px)`);
  - `bordered(false)` drops the frame;
  - column widths are set per column in the schema.
- **Font family and size** are inherited from GPUI's text style around the grid, with tabular figures.

Per cell and row, `cell_style`, `row_style` and `aggregate_style` set the background, the text colour, the weight and the trend arrow. `ChangePaint::Custom` paints changes in any colours.

Not replaceable yet. Each would become a seam or a builder parameter under the repository rule:

| Item | Today | Planned |
|---|---|---|
| State colours (`GridPalette`) | Theme tokens only. The change colours come from `success`, `warning` and `danger`; other colours need a custom `ChangePainter` | A palette for change, selection, focus, caption, footer and totals colours, defaulting to the theme tokens |
| Font per column or cell | One family and size for the whole body; only the weight per cell (`CellStyle::set_font_weight`) | Family, size and style (italic) per column and through `cell_style`, cached per font in the text cache |
| Header font and look | Follows gpui-kit's table header | A header font and weight separate from the body; a header cell renderer |
| Cell padding | Fixed, as in gpui-kit tables; an element in a cell gets the same fixed padding, so a bar cannot fill the whole cell | A padding parameter per grid and per column, for text and elements |
| Header height, group indent, chevron lane | Fixed | Parameters per grid |
| Grid lines | Row and column lines and zone separators, always on, in `table_row_border` and `border` | A policy for horizontal, vertical, both or none, and their colours |

### Found by the showcase examples

The showcase examples (`desk`, `ladder`, `wire`, `broker`, `ops`, `orbit`, `transit`, `wiki` and `ships`)
and `orders` work around these gaps with the public API today. Three of them extend rows of the table above: the change colours
(`broker`, `ops`), the header font (`desk`) and the grid lines (`broker`).

| Gap | Where it bit | What the example does instead |
|---|---|---|
| A number format has decimals only: no digit grouping, no prefix or suffix, no locale | `desk`, `ladder`, `wire`, `ops`, `orbit` | Writes formatted text, or a `Custom` value with its own text and order |
| No two-line cells, and one row height per grid | `broker` | One line per row |
| Tabular figures are applied to text too, which widens hyphens in some fonts | `wire` | Nothing |
| No column that fills the remaining width | `desk`, `wire` | Recomputes the width on window resize; a manual resize of another column breaks it |
| No rounded rows, no row hover and no empty state | `broker` | Square rows, no hover, an empty grid |
| Grid lines come only from the theme, and `table_row_border` also removes the header lines | `broker` | Accepts both |
| The selection colour is capped at 20% opacity, and there is no mode without selection | `desk`, `ladder` | Draws a selection border through `cell_style`; ignores the selection |
| `cell_style` cannot clear the change highlight | `wire`, `ops` | Paints a transparent background over it, or a `ChangePainter` that reads `CellChange::column_ix` flashes only some columns |
| Change paint knows only the success and danger tokens, which status colours must not share | `broker`, `ops` | A custom `ChangePainter` |
| A group caption is one colour, and its label band hides the aggregates of the columns under it | `wire`, `ops` | Hidden counter columns, summed and read in `group_caption` |
| Pinned rows belong to the store and sit only at the edges: two grids over one store cannot pin differently, and pinned rows cannot follow a rule such as top N | `desk`, `wire`, `broker`, `ops` | A second store, or the producer moves rows in and out of the pinned band |
| No newest-first insertion order | `desk` | A hidden sequence column sorted descending |
| `clear` and `replace_all` drop rows that are still fading out | `desk` | Nothing |
| Tree parent rows have no roll-ups | `ops`, `portfolio` | The producer writes the sums into the branch rows |
| The context menu gesture does not know the clicked column | `ladder` | The menu opens from the header only |
| The command scope of a focused group caption is not public | `ops` | Dispatches the grid's `Confirm` |
| A change written without a value change (`mark_changed`) carries no direction, and pinned rows always highlight their changed columns | `orbit` | Writes rises and sets as highlighted upserts with a direction override, everything else silently |
| Pinned rows cannot be selected: a click selects the data row with the same key | `orbit` | Accepts it |
| `select_key` ignores a row that is not published yet | `orbit` | Retries until the row arrives |
| Elements in cells do not inherit the body's tabular figures | `desk`, `broker` | Nothing |
| An `Image` value sorts and filters by its text only, so a status image cannot sort by rank | `ops` | The service status sorts by its label text |
| gpui-kit's `Tag` brings hover and a hitbox of its own, which makes pills the largest part of a frame | `broker` | Accepts it (frame p50 1.77 → 2.54 ms, noisy) |
| A group caption shows text and a tag, not an element | `transit` | The line's pill is a `CaptionTag` |
| In the column chooser, ↓ in the search field does not move the highlight, Tab does not cycle inside the popover, a drag to reorder shows no "not allowed" cursor, and a row's checkbox only shows the state (a click on the row toggles it) | `blotter`, `orders` | Nothing; the manual check decides |

Fixed in the grid since: a fading row keeps its elements in cells (`transit`); internal columns,
never offered by the column chooser (`GridColumn::with_internal`; `transit`, `ships`); groups of a
`Custom` key keep the value itself (`OwnedValue::custom`), and a caption can start with a tag
(`group_caption_tag`; `transit`); link values painted by the grid, opened by a click or Enter
(`CellValue::Link`, `LinkOpener`; `wiki`); a change painter knows the cell's column
(`CellChange::column_ix`; `wiki`, `transit`); a zone scrolled to its start stays there when a column is
shown before it, and `scroll_x` reads the offset (`wiki`, `orders`); a new grouping shows its first
caption (`ships`); decimals per column, and copies and CSV files with the decimals shown
(`NumberFormat`, `NumberPrecision`; `ships` and every price column).

Outside the grid, in gpui-kit and GPUI:
- the charts have no y-domain, no y-axis and no label format, and every live update rebuilds all
  their data;
- a button with a custom colour draws it at 20%;
- the theme has four status colours, so a fifth severity level needs a remapped token;
- in the browser, GPUI's glyph fallback covers emoji and CJK only: Arabic, Hebrew, Indic and Thai
  text shows as placeholders (`wiki` names the script instead), and `ships` draws `●` and `▲` as
  shapes;
- on the web, `App::open_url` calls `window.open` without `noopener`;
- GPUI has no retained paths, so a canvas tessellates its paths again every frame (`ships` caches
  its own);
- the text of gpui-kit's `StatusBar` does not shrink on a phone;
- offscreen snapshots need Metal, so the examples' PNG snapshots run on macOS only.

## Gaps for new kinds of applications (not scheduled)

A survey of applications the grid could serve next found these gaps. The candidates included a shared task board, a live test run, election night, an internet routing table, a CSV viewer and cluster or database monitors. The survey also raises the priority of three rows above: number formats (units such as bytes, percent and durations), elements in cells (a cheap bar or sparkline that fills the cell), and roll-ups on tree parent rows.

| Gap | Today | Who needs it |
|---|---|---|
| Columns known only at run time | The column set is a static list in code (`GridColumns::all`), and a grid has at most 128 columns | A CSV or Parquet viewer, an arbitrary database query, Kubernetes resources |
| A filter per column in the interface | Only the quick text filter, driven through the API | Any grid a user explores by hand: a CSV viewer, a task board, a cluster view |
| Chunked rebuilds in the browser | The single-threaded web platform rebuilds on the main thread. 100k rows are measured, a million are not | A CSV viewer, large live sets on the web |
| Row retention by a rule | The store keeps the last state per key. Keeping only the last N rows, or rows no older than T, is left to the producer (as in `wire`) | Event streams: routing updates, traces, test output |
| CSV download in the browser | `export_to_path` is native only; on the web `export_with` hands the bytes to a callback, and the page has no recipe to download them | A task board, a CSV viewer, any web demo with export |
| Another person's presence in a cell | No mark for "someone else is editing this cell"; row locks are deferred | A shared task board, collaborative editing |
| Dimming rows that went quiet | No time-based cell state: a row that has not updated for N seconds looks the same as a fresh one | Polled feeds: bike stations, flights, sensors |

## Open outside the stages

- **Windows check.** The suite targets Windows, but no run has been made there yet.
- **Continuous integration.** The workflows pin gpui-kit by git revision; their first run on
  GitHub, Windows included, is still to come.
- **Benchmark tails.** A few p99 tails of stage 1 scenarios grew by more than 5% under load and
  wait for a re-measure on an idle machine.
- **Stage 3 final run.** The stage 3 benchmark numbers come from runs on a busy machine; the final
  run, with S1–S24 against stage 2, waits for an idle one.
