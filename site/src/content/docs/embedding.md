---
title: Embedding
description: Show a live example on any page with one script and a tag, and recolour it with a theme remix.
---

The examples on this site run on any page, a snippet site or your own
docs, from one module script and a tag:

```html
<script type="module" src="https://gkg.giglabo.com/embed/v1/live-grid.js"></script>
<live-grid app="quotes" autoload></live-grid>
```

[Play](../play/) writes this snippet for you, with your theme, and opens it
on CodePen, JSFiddle, StackBlitz or the MDN Playground. These are demos of
the Rust crate running in WebAssembly, not a JavaScript grid: each one is
about a 7 MB download.

## The element

| Attribute | What it does |
| --- | --- |
| `app` | The example: `quotes` (the default), `desk`, `broker`, `ops`, `orbit`, `transit`, `wiki` or `ships`. |
| `mode` | `inline` (the default) runs the example in your page, its canvas inside the element. `frame` shows the example's page in an iframe with `allow="clipboard-write; fullscreen"`: its keyboard shortcuts stay inside, and copying works there. |
| `theme` | A theme remix: JSON, or packed as `v1.…` the way Play and `#theme=` links carry it. Changing the attribute recolours the running example. |
| `query` | The example's own parameters, such as `seed=7&rate=20`. |
| `autoload` | Starts the example once the element is on screen. Without it, and on touch screens or with Save-Data, a button starts it. |

The element is 360 px high until your CSS sizes it. Its bottom 20 px are a
line that links the licences of what the example is built from, and for
`quotes`, `desk` and `broker` also says "Simulated prices, not market
data": they show real companies with made-up prices. For `ops`, a made-up
data center, it says "Simulated data".

From script:

- `element.start()` starts the example;
- `element.setTheme(theme)` lays a remix (an object, JSON or `v1.…`) over
  the example's theme and resolves with what it applied, ignored and
  replaced; it rejects for JSON that does not parse;
- `element.grid` is the module's own hooks (`stats()`, `set_rate(n)`,
  `current_theme()` and more) once an inline example is ready;
- events `ready` and `error` (`event.detail.message`);
- `customElements.get('live-grid').packTheme(theme)` and `unpackTheme(text)`
  convert between a remix and its `v1.…` form.

As a guest, the example keeps to itself:

- it takes no keyboard focus when it starts; the first click in it does;
- the mouse wheel scrolls the grid, and scrolls your page once the grid is
  at its top or bottom (within its first or last row);
- in a frame that may not write to the clipboard, a copy shows a short
  notice, "Copy needs clipboard permission on this site", instead of
  failing silently. Snippet sites differ: some allow it in their preview,
  others do not.

`live-grid.js` stores nothing in the browser and loads no analytics. It
fetches the example's module and fonts from this site.

### In an iframe

Any example's page takes `?embed`: the same manners and the same line,
without the list of examples. A page that frames it can send it a remix:

```js
frame.contentWindow.postMessage({ type: 'live-grid:set-theme', id: 1, theme }, '*');
// answers { type: 'live-grid:theme', id: 1, report } or { …, error },
// and says { type: 'live-grid:ready' } once the example is on screen
```

## Theme remix, version 1

A remix is a small part of a [gpui-kit theme file](https://gpui-kit.com),
spelled the same way, so a remix you like is also the start of a theme for
a native application. The keys below are the contract of version 1 (the
`v1.` of a packed remix): keys are only ever added. A key outside the list
is ignored with a warning on the console, and so is a value of the wrong
kind; JSON that does not parse is an error and changes nothing. `{}` shows
the example's own theme again.

```json
{
  "font.family": "Inter",
  "mono_font.family": "JetBrains Mono",
  "colors": {
    "table.background": "#0b0f0c",
    "foreground": "#33ff66",
    "success.background": "#00aa44"
  }
}
```

| Key | Value |
| --- | --- |
| `font.family` | The interface font: one of the families below. |
| `mono_font.family` | The font of numbers and code: one of the families below. |
| `font.size` | The interface size in pixels, 8 to 32. |
| `mono_font.size` | The size of numbers in pixels, 8 to 32. |
| `radius` | The corner radius of controls in whole pixels, 0 to 24. |

Colours go under `colors`, as `#rrggbb` or `#rrggbbaa`:

| Key | Paints |
| --- | --- |
| `background` | The page behind the example. |
| `foreground` | Text. |
| `border` | Borders and dividers. |
| `muted.foreground` | Secondary text. |
| `accent.background` | Hovered and accented controls. |
| `selection.background` | Selected text. |
| `scrollbar.thumb.background` | Scrollbar thumbs. |
| `table.background` | Grid rows. |
| `table.even.background` | Even grid rows. |
| `table.head.background` | Grid headers. |
| `table.head.foreground` | Grid header text. |
| `table.hover.background` | The row under the pointer. |
| `table.active.background` | Selected rows. |
| `table.active.border` | The selected cell's border. |
| `table.row.border` | Lines between rows. |
| `success.background` | The highlight of a rise and of a new row. |
| `danger.background` | The highlight of a fall and of a removed row. |
| `warning.background` | The highlight of any other change. |
| `chart.bullish` | Values that went up. |
| `chart.bearish` | Values that went down. |
| `base.yellow` | Tickers in the quotes and the desk. |
| `base.cyan` | Group totals in the quotes. |

**Fonts.** The browser has no system fonts here, so a remix names one of
the families the examples ship: Inter, IBM Plex Sans, IBM Plex Mono,
JetBrains Mono, Iosevka, Barlow Semi Condensed or Manrope. The element
fetches its faces first. Any other family falls back to the example's own,
with a warning; fonts from your own address are not supported in version 1.

**What the light or dark mode is.** A remix keeps the example's mode: it
recolours the example's theme, it does not replace it.

**How far each example follows.** Every grid follows the table keys and
the change highlights.

- `quotes`, `desk` and `ops` follow everywhere: all their colours are
  these tokens.
- `orbit` and `ships` follow on the grids, borders and headers; their
  panels and plots keep colours of their own.
- `broker`, `transit` and `wiki` have light themes. Their grids, text and
  page background follow; their cards and panels keep the example's
  colours, which the keys above do not cover.

**In a link.** A packed remix is the JSON deflated and written in
base64url after `v1.`; Play puts it in its address as `#theme=v1.…`, and
every example's page reads `#theme=` too. The part after `#` never reaches
the server.
