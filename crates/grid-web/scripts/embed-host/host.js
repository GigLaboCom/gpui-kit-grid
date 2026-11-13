// The host pages of `scripts/embed-check.mjs`: another site's page holding
// `<live-grid>` from the examples' www/ (the "CDN"), as a reader's page
// would. Parameters: `cdn` (the address of that www/), and the elements'
// `app`, `mode` and `query`, `count` of them (default 1) into `#grids`.
// A page with a `#field` focuses it and scrolls down a little, so that both
// the keyboard and the page's scroll have somewhere to come back to.
//
// `window.grids` holds the elements, and `window.events` their events as
// [element index, type, detail].

const params = new URLSearchParams(location.search);
const cdn = params.get('cdn');
const count = Number(params.get('count') ?? 1);

window.events = [];
window.grids = [];
for (let ix = 0; ix < count; ix++) {
  const grid = document.createElement('live-grid');
  grid.setAttribute('app', params.get('app') ?? 'quotes');
  grid.setAttribute('mode', params.get('mode') ?? 'inline');
  if (params.get('query')) grid.setAttribute('query', params.get('query'));
  grid.setAttribute('autoload', '');
  for (const type of ['ready', 'error']) {
    grid.addEventListener(type, (event) => events.push([ix, type, event.detail]));
  }
  grids.push(grid);
}
document.getElementById('grids').append(...grids);

const field = document.getElementById('field');
if (field) {
  field.focus();
  scrollTo(0, 120);
}

await import(new URL('embed/live-grid.js', cdn).href);
