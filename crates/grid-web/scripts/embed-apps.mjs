// What the embed checks (`embed-check.mjs`, `embed-sites.mjs`) share: the
// examples they run and on what, the mocked CelesTrak, and what a console
// line or notice they look for says.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const TIME = '2026-11-06T17:30:00Z';

// Each check's example and `query`: a fixed seed, its recording, a fixed
// time, or (orbit) CelesTrak's elements, which the checks answer
// themselves. Never a live source.
export const EXAMPLES = {
  quotes: ['quotes', 'seed=7&rate=20'],
  desk: ['desk', 'seed=7'],
  broker: ['broker', 'seed=7'],
  orbit: ['orbit', `data=celestrak&time=${TIME}`],
  transit: ['transit', `time=${TIME}`],
  helsinki: ['transit', 'city=helsinki&source=replay'],
  wiki: ['wiki', 'replay'],
  ships: ['ships', 'source=replay'],
  ops: ['ops', 'seed=7'],
};

// The names in APPS (default quotes, `all` for every one), or exits when
// one is not an example.
export function appsFromEnv() {
  const apps =
    process.env.APPS === 'all'
      ? Object.keys(EXAMPLES)
      : (process.env.APPS ?? 'quotes').split(',').filter(Boolean);
  for (const app of apps) {
    if (!(app in EXAMPLES)) {
      console.error(`APPS=${app}: not one of ${Object.keys(EXAMPLES).join(', ')}`);
      process.exit(1);
    }
  }
  return apps;
}

// The mocked CelesTrak's elements: the first few hundred objects of the
// local `data/celestrak-active.json` (script/fetch-orbit-data), never
// served by the examples' site build. Exits when there are none.
const ELEMENTS = fileURLToPath(new URL('../../grid/examples/data/celestrak-active.json', import.meta.url));
const ORBIT_OBJECTS = 300;

export function celestrakElements() {
  try {
    const all = JSON.parse(readFileSync(ELEMENTS, 'utf8'));
    return Buffer.from(JSON.stringify(all.slice(0, ORBIT_OBJECTS)));
  } catch (error) {
    console.error(`APPS=orbit: no local elements for the mocked CelesTrak (${error.message}): run script/fetch-orbit-data once.`);
    process.exit(1);
  }
}

// Chrome never reaches celestrak.org: with this flag its name does not
// resolve, and `answerCelestrak` answers the page and its frames instead.
export const NO_CELESTRAK = '--host-resolver-rules=MAP celestrak.org ~NOTFOUND';

// Answers each request to celestrak.org from the page of `chrome` (a
// `launch()`) and its frames with `elements`, recording its address in
// `asked`. Call it before navigating.
export async function answerCelestrak(chrome, elements, asked) {
  chrome.onBrowser(({ method, params }) => {
    if (method !== 'Fetch.requestPaused') return;
    asked.push(params.request.url);
    chrome
      .browser('Fetch.fulfillRequest', {
        requestId: params.requestId,
        responseCode: 200,
        responseHeaders: [
          { name: 'Access-Control-Allow-Origin', value: '*' },
          { name: 'Content-Type', value: 'application/json' },
          { name: 'Last-Modified', value: 'Fri, 06 Nov 2026 16:00:00 GMT' },
        ],
        body: elements.toString('base64'),
      })
      .catch(() => {});
  });
  // The browser's, so that requests from the page and its frames pause.
  await chrome.browser('Fetch.enable', { patterns: [{ urlPattern: '*://celestrak.org/*' }] });
}

export const COPY_NOTICE = 'Copy needs clipboard permission on this site';
// A console line that means the example broke.
export const FATAL = /panicked|RuntimeError|already borrowed|unreachable|TypeError|Uncaught|GRID_ERROR/i;
