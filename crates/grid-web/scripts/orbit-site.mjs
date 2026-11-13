// The orbit page in its public-site mode (`?data=celestrak`), in headless
// Chrome, against a mocked CelesTrak: every request for celestrak.org is
// answered here through the DevTools protocol, and the browser cannot
// resolve the name besides, so the check never reaches CelesTrak. Checks:
//
// 1. the page asks nothing before the click, then exactly once, and starts
//    with the elements;
// 2. after a reload it starts from the browser's copy, asking nothing;
// 3. without a copy and with CelesTrak answering 403, one click asks once,
//    the page stops and says why, and a reload asks nothing and offers no
//    download.
//
// The mock's elements are the first few hundred objects of the local
// `data/celestrak-active.json` (script/fetch-orbit-data). Prints each step
// and exits 1 on a failure. Env: ORIGIN (a served www/), CHROME,
// CHROME_FLAGS.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { launch, sleep } from './chrome.mjs';
import { site } from './serve.mjs';

const ELEMENTS = fileURLToPath(new URL('../../grid/examples/data/celestrak-active.json', import.meta.url));
const OBJECTS = 300;
const TIME = '2026-11-06T17:30:00Z';
const LAST_MODIFIED = 'Fri, 06 Nov 2026 16:00:00 GMT';

let elements;
try {
  elements = Buffer.from(JSON.stringify(JSON.parse(readFileSync(ELEMENTS, 'utf8')).slice(0, OBJECTS)));
} catch (error) {
  console.error(`orbit-site: no local elements for the mock (${error.message}): run script/fetch-orbit-data once.`);
  process.exit(1);
}

const pages = await site();
const chrome = await launch(['--host-resolver-rules=MAP celestrak.org ~NOTFOUND']);
const { send } = chrome;
const asked = []; // the URLs CelesTrak was asked for
let answer = 200;
const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'ok    ' : 'FAILED'} ${label}`);
  if (!ok) failures.push(label);
};

chrome.on(async ({ method, params }) => {
  if (method !== 'Fetch.requestPaused') return;
  asked.push(params.request.url);
  const headers = [
    { name: 'Access-Control-Allow-Origin', value: '*' },
    { name: 'Content-Type', value: answer === 200 ? 'application/json' : 'text/plain' },
    { name: 'Last-Modified', value: LAST_MODIFIED },
  ];
  const body = answer === 200 ? elements : Buffer.from('Forbidden');
  await send('Fetch.fulfillRequest', {
    requestId: params.requestId,
    responseCode: answer,
    responseHeaders: headers,
    body: body.toString('base64'),
  });
});

const visible = () =>
  chrome.evaluate(`(() => {
    const panel = document.getElementById('celestrak');
    const stats = JSON.parse(globalThis.grid?.stats?.() ?? 'null');
    return {
      panel: Boolean(panel),
      load: Boolean(panel?.querySelector('[data-action=load]')),
      problem: panel?.querySelector('.problem')?.textContent ?? null,
      rows: stats?.visible_cells ?? 0,
    };
  })()`);

async function waitFor(predicate, ms = 30000) {
  for (let waited = 0; waited <= ms; waited += 100) {
    const state = await visible();
    if (state && predicate(state)) return state;
    await sleep(100);
  }
  return visible();
}

async function open() {
  await send('Page.navigate', { url: pages.page('orbit', `data=celestrak&time=${TIME}`) });
  await sleep(500);
}

try {
  await send('Runtime.enable');
  await send('Page.enable');
  await send('Fetch.enable', { patterns: [{ urlPattern: '*://celestrak.org/*' }] });

  // 1. A first visit: the panel, nothing asked, then one click.
  await open();
  let state = await waitFor((s) => s.panel && s.load, 10000);
  check(state.panel && state.load, 'first visit: the panel offers the download');
  await sleep(2000);
  check(asked.length === 0, `nothing asked before the click (asked ${asked.length})`);
  await chrome.evaluate(`document.querySelector('#celestrak [data-action=load]').click()`);
  state = await waitFor((s) => s.rows > 0);
  check(state.rows > 0 && !state.panel, 'after the click the example shows rows');
  check(asked.length === 1, `one request (asked ${asked.length})`);

  // 2. A reload: the browser's copy, nothing asked.
  await open();
  state = await waitFor((s) => s.rows > 0);
  check(state.rows > 0 && !state.panel, 'reload: rows from the copy, no panel');
  check(asked.length === 1, `reload asked nothing (asked ${asked.length})`);

  // 3. No copy, and CelesTrak refuses.
  await chrome.evaluate(`caches.delete('orbit-celestrak')`);
  answer = 403;
  await open();
  state = await waitFor((s) => s.panel && s.load, 10000);
  check(state.panel && state.load, 'without a copy the panel offers the download');
  await chrome.evaluate(`document.querySelector('#celestrak [data-action=load]').click()`);
  state = await waitFor((s) => s.problem !== null, 10000);
  check(/HTTP 403/.test(state.problem ?? '') && !state.load, `403: stopped, says why ("${state.problem}")`);
  check(asked.length === 2, `the click asked once (asked ${asked.length})`);
  await sleep(2000);
  check(asked.length === 2, 'nothing asked after the refusal');
  await open();
  state = await waitFor((s) => s.panel, 10000);
  await sleep(1500);
  state = await visible();
  check(state.panel && !state.load && /HTTP 403/.test(state.problem ?? ''), 'reload after 403: no download offered');
  check(asked.length === 2, `reload after 403 asked nothing (asked ${asked.length})`);
  check(asked.every((url) => url.startsWith('https://celestrak.org/NORAD/elements/gp.php')), 'only gp.php asked');
} finally {
  await chrome.close();
  pages.close();
}
process.exitCode = failures.length > 0 ? 1 : 0;
