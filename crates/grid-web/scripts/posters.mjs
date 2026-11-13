// The site's posters: one WebP still of each example, drawn by the example
// itself in headless Chrome, as the site's pages show it in a frame
// (`?embed`, 800×450 CSS pixels, the width of a page's column, drawn at
// 1.6× into 1280×720); the desk, which the site opens only on its own page,
// as that page at 1280×720. Every poster comes from something fixed and offline:
// the markets and ops from a seed, transit from a fixed time in Munich's
// timetable and from Helsinki's recording, wiki and ships from their
// recordings, orbit from the local `data/celestrak-active.json`, which the
// script hands the page itself (a site build serves no copy; Chrome cannot
// resolve celestrak.org here). No live source is ever asked.
//
// Each poster is written at the highest WebP quality that keeps it under
// LIMIT (150 kB), to $OUT/<name>.webp (default: the site's
// `public/posters/`). Env: OUT, APPS (default every poster: quotes, desk,
// broker, orbit, transit, helsinki, wiki, ships, ops), ORIGIN (a served
// www/ instead of this checkout's), CHROME, CHROME_FLAGS. Exits 1 if a page
// shows no rows and gets no updates (then `<name>-failed.png` in OUT), or a
// poster does not fit.

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch, sleep } from './chrome.mjs';
import { site } from './serve.mjs';

const OUT = process.env.OUT ?? fileURLToPath(new URL('../../../site/public/posters/', import.meta.url));
const LIMIT = 150 * 1000;
const FRAME = [800, 450, 1.6]; // CSS width and height, scale
const FULL_PAGE = [1280, 720, 1];
const ELEMENTS = fileURLToPath(new URL('../../grid/examples/data/celestrak-active.json', import.meta.url));
// The posters: the example, its query, how long it runs after its first
// rows before the still (for highlights, a filled replay, a storm), and the
// size it runs at.
const POSTERS = {
  quotes: { app: 'quotes', query: 'seed=7', settle: 4000 },
  desk: { app: 'desk', query: 'seed=7', settle: 5000, size: FULL_PAGE },
  broker: { app: 'broker', query: 'seed=7', settle: 4000 },
  orbit: { app: 'orbit', query: 'data=local&time=2026-11-06T17:30:00Z', settle: 4000 },
  transit: { app: 'transit', query: 'time=2026-11-10T07:45', settle: 5000 },
  helsinki: { app: 'transit', query: 'city=helsinki&source=replay', settle: 8000 },
  wiki: { app: 'wiki', query: 'replay&speed=4', settle: 10000 },
  ships: { app: 'ships', query: 'source=replay&speed=4', settle: 10000 },
  ops: { app: 'ops', query: 'seed=7&storm', settle: 8000 },
};
const START_MS = 60000;

const names = process.env.APPS ? process.env.APPS.split(',').filter(Boolean) : Object.keys(POSTERS);
for (const name of names) {
  if (!(name in POSTERS)) {
    console.error(`APPS=${name}: not one of ${Object.keys(POSTERS).join(', ')}`);
    process.exit(1);
  }
}
mkdirSync(OUT, { recursive: true });

let elements = null;
if (names.some((name) => POSTERS[name].app === 'orbit')) {
  try {
    elements = readFileSync(ELEMENTS);
  } catch (error) {
    console.error(`posters: orbit needs the local elements (${error.message}): run python3 -I script/fetch-orbit-data once.`);
    process.exit(1);
  }
}

// The still at the highest quality that fits, as [bytes, quality].
async function still(send) {
  for (let quality = 85; quality >= 30; quality -= 5) {
    const { data } = await send('Page.captureScreenshot', { format: 'webp', quality });
    const bytes = Buffer.from(data, 'base64');
    if (bytes.length <= LIMIT || quality === 30) return [bytes, quality];
  }
  throw new Error('unreachable');
}

const pages = await site();
const failures = [];
try {
  for (const name of names) {
    const { app, query, settle, size = FRAME } = POSTERS[name];
    const [WIDTH, HEIGHT, SCALE] = size;
    // The scale as a flag, not DevTools emulation: under emulation Chrome
    // reports the canvas's size at scale 1 (`input.mjs`). The size is
    // emulated: a headless window's viewport is smaller than the window.
    const chrome = await launch([
      '--host-resolver-rules=MAP celestrak.org ~NOTFOUND',
      '--hide-scrollbars',
      `--window-size=${WIDTH},${HEIGHT}`,
      `--force-device-scale-factor=${SCALE}`,
    ]);
    const { send, evaluate } = chrome;
    try {
      await send('Page.enable');
      await send('Runtime.enable');
      await send('Emulation.setDeviceMetricsOverride', {
        width: WIDTH,
        height: HEIGHT,
        deviceScaleFactor: 0,
        mobile: false,
      });
      if (elements) {
        // Orbit's elements, from here: the site build serves none.
        await send('Fetch.enable', { patterns: [{ urlPattern: '*/data/celestrak-active.json' }] });
        chrome.on(({ method, params }) => {
          if (method !== 'Fetch.requestPaused') return;
          send('Fetch.fulfillRequest', {
            requestId: params.requestId,
            responseCode: 200,
            responseHeaders: [
              { name: 'Content-Type', value: 'application/json' },
              { name: 'Last-Modified', value: 'Fri, 06 Nov 2026 16:00:00 GMT' },
            ],
            body: elements.toString('base64'),
          }).catch(() => {});
        });
      }
      await send('Page.navigate', { url: pages.page(app, `embed&${query}`) });
      let rows = false;
      for (const started = Date.now(); Date.now() - started < START_MS; await sleep(200)) {
        const stats = JSON.parse((await evaluate('globalThis.grid?.stats?.() ?? "null"')) ?? 'null');
        if (stats?.error) throw new Error(stats.error);
        // Rows on screen, or its source flowing where the frame's narrow
        // layout puts the table below the fold (transit's map first).
        if (stats?.visible_cells > 0 || stats?.updates > 0) {
          rows = true;
          break;
        }
      }
      if (!rows) {
        await chrome.screenshot(join(OUT, `${name}-failed.png`)).catch(() => {});
        throw new Error(`no rows in ${START_MS / 1000} s (${name}-failed.png)`);
      }
      await evaluate('globalThis.fontsReady');
      await sleep(settle);
      const [bytes, quality] = await still(send);
      writeFileSync(join(OUT, `${name}.webp`), bytes);
      const fits = bytes.length <= LIMIT;
      console.log(`${name}: ${Math.round(bytes.length / 1000)} kB at quality ${quality}${fits ? '' : ' (over the limit)'}`);
      if (!fits) failures.push(name);
    } catch (error) {
      console.error(`${name}: FAILED (${error.message ?? error})`);
      failures.push(name);
    } finally {
      await chrome.close();
    }
  }
} finally {
  pages.close();
}
process.exitCode = failures.length ? 1 : 0;
