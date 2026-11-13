// What the orbit example needs before it runs, for its page and for
// `<live-grid app="orbit">` inline (`embed/live-grid.js`): the orbital
// elements, `data/celestrak-active.json` (`site=lat,lon,alt_m`, `time=` a
// fixed UTC start, `chunks=` the steps of a sweep in `params`). On the
// public site (`pkg/site.json`, a `SITE=1` build) it fetches them from
// CelesTrak instead, on a click in a panel (`celestrak.js`);
// `data=celestrak` or `data=local` picks either.

import { orbitData } from './celestrak.js';

/// The example's start for `params`: resolves with `{ loading, beforeRun }`,
/// where `beforeRun(wasm)` hands the module the elements (without them the
/// example says how to get them) and returns the bytes fetched. From
/// CelesTrak the panel shows at once, in `root` (the page's body by
/// default) with its style in `styles` (the page's head), so the visitor
/// can choose while the module downloads.
export async function setup(params, { root, styles } = {}) {
  const fromSite = await fetch(new URL('../pkg/site.json', import.meta.url))
    .then((response) => (response.ok ? response.json() : {}))
    .catch(() => ({}));
  const source = params.get('data') ?? (fromSite.site ? 'celestrak' : 'local');
  const celestrak = source === 'celestrak' ? orbitData({ root, styles }) : null;

  async function beforeRun(wasm) {
    const [site, time] = [params.get('site') ?? undefined, params.get('time') ?? undefined];
    const chunks = params.has('chunks') ? Number(params.get('chunks')) : undefined;
    if (celestrak) {
      const { data, modified } = await celestrak;
      wasm.orbit_data(data, undefined, modified, site, time, chunks);
      return data.length;
    }
    const url = new URL('../data/celestrak-active.json', import.meta.url);
    const response = await fetch(url).catch((error) => error);
    if (!(response instanceof Response) || !response.ok) {
      const reason = response instanceof Response ? `HTTP ${response.status}` : String(response);
      const error = `data/celestrak-active.json: ${reason}. Run python3 -I script/fetch-orbit-data, then make build.`;
      wasm.orbit_data(undefined, error, undefined, site, time, chunks);
      return 0;
    }
    const modified = Date.parse(response.headers.get('Last-Modified') ?? '');
    const data = new Uint8Array(await response.arrayBuffer());
    wasm.orbit_data(data, undefined, Number.isNaN(modified) ? undefined : modified, site, time, chunks);
    return data.length;
  }

  return { loading: 'Loading orbital data', beforeRun };
}
