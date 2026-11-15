# gpui-kit-grid live demos for `<live-grid>`

These packages are **demo modules, not a JavaScript library**. They carry the
`<live-grid>` custom element and the WebAssembly builds of the
[gpui-kit-grid](https://github.com/GigLaboCom/gpui-kit-grid) examples, so that
a CDN that mirrors npm (jsDelivr, unpkg) can serve them to any page at a fixed,
immutable version. There is nothing to `import` from them in an application,
and no API beyond what the element documents. The grid itself is a Rust crate:
see the repository.

| Package | What it holds |
|---|---|
| `@giglabo/live-grid` | the element (`embed/live-grid.js` and the two modules it imports) and the fonts every example draws in |
| `@giglabo/live-grid-<app>` | one example: its module (`pkg/<app>/<app>_bg.wasm`) and wasm-bindgen glue, the script it runs before it starts and its data, if any, and the third-party notices of what the module compiles |

All the packages are released together with one version number. The element
loads an example from the package of the same name and version next to its
own on the CDN.

## Use

```html
<script type="module" src="https://cdn.jsdelivr.net/npm/@giglabo/live-grid@1.0.0/embed/live-grid.js"></script>
<live-grid app="quotes" autoload></live-grid>
```

The attributes, events and theme remix are described on
<https://gkg.giglabo.com/embedding/>. The same element is served, always at its
latest version 1, from `https://gkg.giglabo.com/embed/v1/live-grid.js`.

Pin an exact version. The element loads the examples at its own version, so a
version range (`@1`) still loads one consistent set of files, but the range
itself is cached by the CDN for up to seven days.

`mode="frame"` shows the example's own page from gkg.giglabo.com: CDNs serve
HTML from npm packages as text, so the pages are not in these packages.

Each example is a 6 to 8 MB compressed download; the element shows a button
instead of starting on touch screens and with Save-Data.

## Licences

The element and the examples are licensed under Apache-2.0 (`LICENSE`).
Each example's module compiles third-party Rust crates under their own
permissive licences, listed with their full texts in `licenses/<app>.html` in
that example's package. The fonts are under the SIL Open Font License 1.1, its
text next to each family in `fonts/<family>/OFL.txt`. The data an example
replays carries its licence and attribution next to it (`data/<file>.license`).
