// `<live-grid>` on another site's page, in headless Chrome: what to run on
// every gpui-kit or GPUI upgrade. Inline mode moves the canvas GPUI's web
// platform appends to the body into the element, and holds the focus GPUI
// takes when its window opens (gpui-pre 0.3.8), so an upgrade that places
// the canvas or the focus elsewhere breaks it quietly.
//
// This checkout's www/ is the "CDN" (on 127.0.0.1, or ORIGIN); the host
// pages (`scripts/embed-host/`) are served on another port and opened as
// `localhost`, another site. The checks, each on a freshly loaded page:
//
// - inline start: the element is `ready`, its canvas inside it (none left
//   in the body), with rows, drawing;
// - frame start: the same with `mode="frame"`, the page in an iframe;
// - no focus grab: the host's field keeps the keyboard after either starts;
// - keys after a click: a click selects a row, ArrowDown moves the
//   selection (read through the module's hooks), and Ctrl+C copies it;
// - focus back: a click in the host's field, and typing goes there;
// - wheel at the edges: at the grid's top a wheel up scrolls the page, a
//   wheel down (and another mid-list) the grid only, and at its bottom a
//   wheel down the page again; inline and in a frame;
// - copy notice: a cross-site frame without `allow="clipboard-write"`
//   (an `?embed` page, and the inline element in one) shows the notice on
//   Ctrl+C; the element's own frame, with `allow`, copies;
// - theme: `setTheme` reports the key applied and an unknown key ignored,
//   and the colour shows, inline and in a frame; JSON that does not parse
//   rejects; the `theme` attribute recolours; a `#theme=` link packed by
//   `LiveGrid.packTheme` round-trips, and a new `#theme=` applies;
// - notice line: the licences line (and the market examples' "Simulated
//   prices" notice) under the canvas, its link answering, inline and in the
//   frame's page;
// - two grids: two inline elements on one page both start, each with its
//   own canvas;
// - more examples (APPS): each starts inline and in a frame and shows rows,
//   on what its page hands its module first (`<app>/setup.js`): transit's
//   timetable at a fixed time, Helsinki's and ships' recordings, wiki's
//   recording, orbit's elements from a mocked CelesTrak after a click in its
//   panel (inside the element inline); its line credits the data's source.
//
// Prints one line per check, `name: ok` or `name: FAILED (why)`, and exits
// 1 on any failure, with a screenshot of each failed page in $OUT
// (`embed-<page>.png`). Env: OUT (default: the current directory), APPS
// (default quotes; more of quotes, desk, broker, orbit, transit, helsinki,
// wiki, ships, ops, or `all`: never a live source, and CelesTrak is
// answered here, never asked), ORIGIN (a served www/ instead of this
// checkout's), CHROME, CHROME_FLAGS.

import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdirSync, readFileSync } from 'node:fs';
import { launch, sleep } from './chrome.mjs';
import { serve, site } from './serve.mjs';

const OUT = process.env.OUT ?? process.cwd();
const TIME = '2026-11-06T17:30:00Z';
// Each case's example and `query`: a fixed seed, its recording, a fixed
// time, or (orbit) CelesTrak's elements, which the check answers itself.
const CASES = {
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
const APPS =
  process.env.APPS === 'all'
    ? Object.keys(CASES)
    : (process.env.APPS ?? 'quotes').split(',').filter(Boolean);
const QUERIES = { quotes: CASES.quotes[1] };
// What the line under an inline example names, on open data.
const CREDITS = {
  orbit: 'CelesTrak',
  transit: 'MVV',
  helsinki: 'HSL',
  wiki: 'Wikimedia',
  ships: 'digitraffic.fi',
};
// The mocked CelesTrak's elements: the first few hundred objects of the
// local `data/celestrak-active.json` (script/fetch-orbit-data), never
// served by the examples' site build.
const ELEMENTS = fileURLToPath(new URL('../../grid/examples/data/celestrak-active.json', import.meta.url));
const ORBIT_OBJECTS = 300;
const MARKETS = ['quotes', 'desk', 'broker'];
const SIMULATED = 'Simulated prices, not market data';
const COPY_NOTICE = 'Copy needs clipboard permission on this site';
const START_MS = 40000;
// Wheel events further apart than this start a new gesture (`guest.js`).
const GESTURE_GAP_MS = 200;
// A row of the quotes grid, from the element's top left.
const ROW = [300, 140];
const CTRL = 2;
const FATAL = /panicked|RuntimeError|already borrowed|unreachable|TypeError|Uncaught|GRID_ERROR/i;

for (const app of APPS) {
  if (!(app in CASES)) {
    console.error(`APPS=${app}: not one of ${Object.keys(CASES).join(', ')}`);
    process.exit(1);
  }
}
mkdirSync(OUT, { recursive: true });

let elements = null;
if (APPS.includes('orbit')) {
  try {
    const all = JSON.parse(readFileSync(ELEMENTS, 'utf8'));
    elements = Buffer.from(JSON.stringify(all.slice(0, ORBIT_OBJECTS)));
  } catch (error) {
    console.error(`APPS=orbit: no local elements for the mocked CelesTrak (${error.message}): run script/fetch-orbit-data once.`);
    process.exit(1);
  }
}

const pages = await site();
const cdn = pages.base;
const hostServer = await serve(0, fileURLToPath(new URL('./embed-host/', import.meta.url)));
const port = hostServer.address().port;
// The host is another site than the examples, and `cross` another than the
// host (the servers listen on 127.0.0.1; Chrome resolves localhost there).
const [hostName, crossName] =
  cdn.hostname === 'localhost' ? ['127.0.0.1', 'localhost'] : ['localhost', '127.0.0.1'];
const host = `http://${hostName}:${port}/`;
const cross = `http://${crossName}:${port}/`;

const hostPage = (file, params) =>
  `${host}${file}?${new URLSearchParams({ cdn: cdn.href, ...params })}`;

const results = [];
function report(name, why) {
  results.push([name, why]);
  process.stdout.write(why ? `${name}: FAILED (${why})\n` : `${name}: ok\n`);
}

// One page in a fresh Chrome: `run(page)` makes its checks. A check
// returns nothing when it passes and why when not; a panic or exception
// on the console fails the check it happened in. Chrome never reaches
// celestrak.org; with `celestrak` the page and its frames get the mock's
// elements for it, counted in `page.asked`.
async function onPage(name, url, run, { celestrak = false } = {}) {
  const chrome = await launch(['--host-resolver-rules=MAP celestrak.org ~NOTFOUND']);
  const log = [];
  const asked = [];
  // The browser's, so that requests from the page and its frames pause.
  chrome.onBrowser(({ method, params }) => {
    if (method === 'Fetch.requestPaused') {
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
    }
  });
  chrome.on(({ method, params }) => {
    if (method === 'Runtime.consoleAPICalled') {
      log.push(params.args.map((arg) => arg.value ?? arg.description ?? '').join(' '));
    } else if (method === 'Runtime.exceptionThrown') {
      const details = params.exceptionDetails;
      log.push(details.exception?.description ?? details.text);
    }
  });
  let failed = false;
  let fatalSeen = 0;
  const { send, evaluate } = chrome;
  const page = {
    send,
    evaluate,
    asked,
    frame: chrome.evaluateInFrame,
    // The value of `poll()` once truthy, polled every 50 ms; `undefined`
    // after `ms`.
    async until(poll, ms = 3000) {
      for (const started = Date.now(); ; await sleep(50)) {
        const value = await poll();
        if (value) return value;
        if (Date.now() - started > ms) return undefined;
      }
    },
    waitFor: (expression, ms) => page.until(() => evaluate(expression), ms),
    // Two animation frames of the page: what it does for an input is drawn.
    settle: () =>
      evaluate('new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))'),
    rect: (expression) =>
      evaluate(`(() => {
        const box = (${expression}).getBoundingClientRect();
        return { x: box.x, y: box.y, w: box.width, h: box.height };
      })()`),
    async click(x, y) {
      for (const [type, extra] of [
        ['mouseMoved', {}],
        ['mousePressed', { button: 'left', buttons: 1, clickCount: 1 }],
        ['mouseReleased', { button: 'left', buttons: 0, clickCount: 1 }],
      ]) {
        await send('Input.dispatchMouseEvent', { type, x, y, ...extra });
      }
      await page.settle();
    },
    async key(key, code, keyCode, modifiers = 0) {
      for (const type of ['rawKeyDown', 'keyUp']) {
        await send('Input.dispatchKeyEvent', {
          type,
          key,
          code,
          windowsVirtualKeyCode: keyCode,
          nativeVirtualKeyCode: keyCode,
          modifiers,
        });
      }
    },
    copyKey: () => page.key('c', 'KeyC', 67, CTRL),
    // Each character of `text` as a key press that types it.
    async type(text) {
      for (const char of text) {
        const keyCode = char.toUpperCase().charCodeAt(0);
        const key = {
          key: char,
          code: `Key${char.toUpperCase()}`,
          windowsVirtualKeyCode: keyCode,
          nativeVirtualKeyCode: keyCode,
        };
        await send('Input.dispatchKeyEvent', { type: 'keyDown', text: char, ...key });
        await send('Input.dispatchKeyEvent', { type: 'keyUp', ...key });
      }
    },
    wheel: (x, y, deltaY) =>
      send('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX: 0, deltaY }),
    async check(label, test) {
      let why;
      try {
        why = (await test()) || undefined;
      } catch (error) {
        why = String(error?.message ?? error);
      }
      const fatal = log.slice(fatalSeen).find((line) => FATAL.test(line));
      fatalSeen = log.length;
      if (!why && fatal) why = `console: ${fatal.split('\n')[0]}`;
      if (why) failed = true;
      report(label, why);
      return !why;
    },
  };
  try {
    await send('Runtime.enable');
    await send('Page.enable');
    await chrome.watchFrames();
    // A headless page is not focused unless asked: as a reader's tab, so
    // that `document.hasFocus()` tells where the keyboard is.
    await send('Emulation.setFocusEmulationEnabled', { enabled: true });
    if (celestrak) {
      await chrome.browser('Fetch.enable', { patterns: [{ urlPattern: '*://celestrak.org/*' }] });
    }
    await send('Page.navigate', { url });
    await run(page);
  } catch (error) {
    failed = true;
    report(`${name} page`, String(error?.stack ?? error).split('\n')[0]);
  } finally {
    if (failed) await chrome.screenshot(join(OUT, `embed-${name}.png`)).catch(() => {});
    await chrome.close();
  }
}

// The event the element `ix` of a host page sent first, `ready` or
// `error`, as [type, detail].
async function started(page, ix = 0) {
  const event = await page.waitFor(
    `events.find(([ix, type]) => ix === ${ix} && (type === 'ready' || type === 'error'))`,
    START_MS,
  );
  return event?.slice(1);
}

// Why the element `ix` did not start in `mode`, or nothing: its event, and
// its canvas (inline) or frame inside it, of a usable size.
async function startProblem(page, mode, ix = 0) {
  const event = await started(page, ix);
  if (!event) return `no ready event in ${START_MS / 1000} s`;
  if (event[0] === 'error') return `error event: ${event[1]?.message}`;
  if (event[1]?.mode !== mode) return `ready in mode ${event[1]?.mode}`;
  const drawing = mode === 'inline' ? 'canvas' : 'iframe';
  return page.evaluate(`(() => {
    const grid = grids[${ix}];
    const child = grid.shadowRoot.querySelector('.stage > ${drawing}');
    if (!child) return 'no ${drawing} in the element';
    if (document.querySelector('body > canvas')) return 'a canvas left in the body';
    const [inner, outer] = [child.getBoundingClientRect(), grid.getBoundingClientRect()];
    const inside =
      inner.left >= outer.left - 0.5 && inner.right <= outer.right + 0.5 &&
      inner.top >= outer.top - 0.5 && inner.bottom <= outer.bottom + 0.5;
    if (!inside || inner.width < 100 || inner.height < 100) {
      const at = (box) => [box.x, box.y, box.width, box.height].map(Math.round).join(',');
      return 'placed at ' + at(inner) + ', the element at ' + at(outer);
    }
    return '';
  })()`);
}

// Why the host's field does not have the keyboard, or nothing.
async function focusProblem(page) {
  const [active, focused] = await page.evaluate(
    '[document.activeElement.id || document.activeElement.localName, document.hasFocus()]',
  );
  if (!focused) return 'the host page lost the focus';
  return active === 'field' ? '' : `the focus is on the host's ${active}`;
}

// Why the module behind `stats` (an expression in `evaluate`) shows no rows
// or draws no new frame, or nothing.
async function drawingProblem(page, evaluate, stats) {
  const read = async () => JSON.parse((await evaluate(stats)) ?? 'null');
  const rows = await page.until(async () => (await read())?.visible_cells > 0, START_MS);
  if (!rows) return 'no rows';
  const before = (await read()).frames;
  if (!(await page.until(async () => (await read())?.frames > before))) return 'no new frame';
  return '';
}

// Why the line under the canvas is missing or wrong, or nothing. `line` is
// an expression for the `.live-grid-credit` element, `canvas` for the
// canvas it sits under, both evaluated by `evaluate`.
async function lineProblem(page, app, evaluate, line, canvas) {
  const found = await evaluate(`(() => {
    const line = ${line};
    const canvas = ${canvas};
    if (!line || !canvas) return null;
    return {
      text: line.textContent,
      href: line.querySelector('a')?.href,
      target: line.querySelector('a')?.target,
      gap: line.getBoundingClientRect().top - canvas.getBoundingClientRect().bottom,
    };
  })()`);
  if (!found) return 'no line';
  if (MARKETS.includes(app) && !found.text.includes(SIMULATED)) return `says "${found.text}"`;
  if (!found.text.includes('Licenses') || !found.href) return `no licences link in "${found.text}"`;
  if (found.target !== '_blank') return 'the link opens in the same tab';
  if (Math.abs(found.gap) > 1) return `${found.gap} px between the canvas and the line`;
  const status = await page.evaluate(
    `fetch(${JSON.stringify(found.href)}).then((response) => response.status, String)`,
  );
  return status === 200 ? '' : `${found.href}: ${status}`;
}

// The line in the first element of a host page, under its canvas.
const inlineLineProblem = (page, app) =>
  lineProblem(
    page,
    app,
    page.evaluate,
    `grids[0].shadowRoot.querySelector('.live-grid-credit')`,
    `grids[0].shadowRoot.querySelector('.stage > canvas')`,
  );

// The line on the example's page in a frame (`evaluate` evaluates there).
const frameLineProblem = (page, app, evaluate) =>
  lineProblem(
    page,
    app,
    evaluate,
    `document.querySelector('.live-grid-credit')`,
    `document.querySelector('body > canvas')`,
  );

// Why the wheel at the grid's ends does not reach the page, or within it
// moves the page, or nothing. `rows()` is the first row in view, `room()`
// the module's `scroll_room` (bit 0 up, bit 1 down), `box` the element.
async function wheelProblem(page, box, rows, room) {
  const [x, y] = [box.x + box.w / 2, box.y + box.h / 2];
  const scrollY = () => page.evaluate('scrollY');
  // The page's scroll once it stops moving (the browser may animate it).
  const stillScroll = async () => {
    for (let last = await scrollY(); ; ) {
      await page.settle();
      const now = await scrollY();
      if (now === last) return now;
      last = now;
    }
  };
  // `guest.js` tells gestures apart by the time between wheel events only.
  const nextGesture = () => sleep(GESTURE_GAP_MS + 100);
  const top = await stillScroll();
  if ((await rows()) !== 0) return `the grid starts at row ${await rows()}, not its top`;
  if (top < 50) return `the page starts at scroll ${top}, with no room up`;

  await page.wheel(x, y, -100);
  if (!(await page.until(async () => (await scrollY()) < top))) {
    return 'a wheel up at the top did not scroll the page';
  }
  if ((await rows()) !== 0) return 'a wheel up at the top moved the grid';

  for (const where of ['at the top', 'mid-list']) {
    await nextGesture();
    const [pageAt, rowAt] = [await stillScroll(), await rows()];
    await page.wheel(x, y, 100);
    if (!(await page.until(async () => (await rows()) > rowAt))) {
      return `a wheel down ${where} did not scroll the grid`;
    }
    await page.settle();
    if ((await stillScroll()) !== pageAt) return `a wheel down ${where} scrolled the page too`;
  }

  // To the bottom in one gesture (events closer than the gesture gap).
  await nextGesture();
  for (let ix = 0; ix < 300 && (await room()) & 2; ix++) {
    await page.wheel(x, y, 400);
    await sleep(20);
  }
  if ((await room()) & 2) return 'the grid did not reach its bottom';
  await nextGesture();
  const bottom = await stillScroll();
  await page.wheel(x, y, 100);
  if (!(await page.until(async () => (await scrollY()) > bottom))) {
    return 'a wheel down at the bottom did not scroll the page';
  }
  return '';
}

// The selection of the grid behind `state` (a `grid_state()` expression).
const selection = async (evaluate, state) =>
  JSON.parse((await evaluate(state)) ?? '{}').selected ?? [];

// Wraps `navigator.clipboard.writeText` (in a page or frame) to record
// each copy and whether the browser took it, into `copies`.
const RECORD_COPIES = `(() => {
  window.copies = [];
  const write = navigator.clipboard.writeText.bind(navigator.clipboard);
  navigator.clipboard.writeText = (text) => {
    const written = write(text);
    written.then(
      () => copies.push(['ok', text]),
      (error) => copies.push(['refused', String(error)]),
    );
    return written;
  };
  return true;
})()`;

// A remix with one key the module knows and one it does not.
const REMIX = { colors: { 'table.background': '#002200' }, bogus: 1 };
// The table's background in a `current_theme()` answer, lower case.
const tableBackground = (current) =>
  JSON.parse(current ?? 'null')?.theme?.colors?.['table.background']?.toLowerCase();

// Why `setTheme` did not apply the remix (or reported no ignored key), or
// nothing; `current` reads the module's `current_theme()`.
async function themeProblem(page, current) {
  const report = await page.evaluate(
    `grids[0].setTheme(${JSON.stringify(JSON.stringify(REMIX))})
      .then((report) => report, (error) => ({ rejected: error.message }))`,
  );
  if (!report || report.rejected) return `setTheme rejected: ${report?.rejected}`;
  if (!report.applied?.includes('colors.table.background')) {
    return `applied ${JSON.stringify(report.applied)}`;
  }
  if (!report.ignored?.some(({ key }) => key === 'bogus')) {
    return `ignored ${JSON.stringify(report.ignored)}`;
  }
  const shown = await page.until(async () => tableBackground(await current()) === '#002200');
  if (!shown) return `the table shows ${tableBackground(await current())}`;
  const bad = await page.evaluate(
    `grids[0].setTheme('{bad').then(() => 'resolved', () => 'rejected')`,
  );
  return bad === 'rejected' ? '' : 'setTheme of JSON that does not parse resolved';
}

// Why orbit's CelesTrak panel did not show inside the element (inline) or
// on the frame's page, or asked before the click, or nothing; clicks its
// download.
async function celestrakProblem(page, mode, inFrame) {
  const button = `#celestrak [data-action=load]`;
  const [find, click] =
    mode === 'inline'
      ? [
          (expression) => page.evaluate(expression),
          `grids[0].shadowRoot.querySelector('${button}')`,
        ]
      : [inFrame, `document.querySelector('${button}')`];
  if (!(await page.until(() => find(`!!${click}`), START_MS))) {
    const outside = await page.evaluate(`!!document.querySelector('${button}')`);
    return outside ? 'the panel is in the host page, not the element' : 'no panel';
  }
  if (page.asked.length) return `asked CelesTrak ${page.asked.length} times before the click`;
  await find(`${click}.click()`);
  return '';
}

const startedAt = Date.now();
try {
  // Inline: start, focus, the line, keys, focus back, copy, wheel, theme.
  await onPage(
    'inline',
    hostPage('index.html', { app: 'quotes', mode: 'inline', query: QUERIES.quotes }),
    async (page) => {
      const glue = (hook) => page.evaluate(`grids[0].grid?.${hook}()`);
      await page.check('inline start', async () => {
        return (
          (await startProblem(page, 'inline')) ||
          (await drawingProblem(page, page.evaluate, 'grids[0].grid?.stats()'))
        );
      });
      await page.check('no focus grab (inline)', () => focusProblem(page));
      await page.check('notice line (inline)', () => inlineLineProblem(page, 'quotes'));
      await page.check('keys after a click', async () => {
        const box = await page.rect('grids[0]');
        await page.click(box.x + ROW[0], box.y + ROW[1]);
        const state = 'grids[0].grid.grid_state()';
        const first = await page.until(async () => {
          const selected = await selection(page.evaluate, state);
          return selected.length ? selected : null;
        });
        if (!first) return 'a click on a row selected nothing';
        await page.key('ArrowDown', 'ArrowDown', 40);
        const next = await page.until(async () => {
          const selected = await selection(page.evaluate, state);
          return selected.length && selected.join() !== first.join() ? selected : null;
        });
        if (!next) return `ArrowDown left the selection on ${first.join()}`;
        await page.evaluate(RECORD_COPIES);
        await page.copyKey();
        const copies = await page.waitFor('copies.length && copies');
        if (!copies) return 'Ctrl+C copied nothing';
        const [result, text] = copies[0];
        if (result !== 'ok') return `Ctrl+C: ${text}`;
        return text.includes(next[0]) ? '' : `Ctrl+C copied "${text.slice(0, 40)}", not ${next[0]}`;
      });
      await page.check('focus back', async () => {
        await page.evaluate('scrollTo(0, 0)');
        await page.settle();
        const field = await page.rect('document.getElementById("field")');
        await page.click(field.x + 20, field.y + field.h / 2);
        const before = await selection(page.evaluate, 'grids[0].grid.grid_state()');
        await page.type('abc');
        const typed = await page.waitFor(
          `document.activeElement.id === 'field' && document.getElementById('field').value`,
        );
        if (typed !== 'abc') return `the field holds "${typed ?? ''}"`;
        const after = await selection(page.evaluate, 'grids[0].grid.grid_state()');
        return after.join() === before.join() ? '' : 'the grid took the keys too';
      });
      await page.check('wheel at the edges (inline)', async () => {
        await page.evaluate('scrollTo(0, 120)');
        await page.settle();
        return wheelProblem(
          page,
          await page.rect('grids[0]'),
          async () => JSON.parse(await glue('grid_state')).rows[0],
          () => glue('scroll_room'),
        );
      });
      await page.check('theme (inline)', async () => {
        const problem = await themeProblem(page, () => glue('current_theme'));
        if (problem) return problem;
        const theme = JSON.stringify({ colors: { 'table.background': '#220000' } });
        await page.evaluate(`grids[0].setAttribute('theme', ${JSON.stringify(theme)})`);
        const shown = await page.until(
          async () => tableBackground(await glue('current_theme')) === '#220000',
        );
        return shown ? '' : 'the theme attribute did not recolour the table';
      });
    },
  );

  // A frame: start, focus, the line, copy with `allow`, wheel, theme.
  const quotesFrame = new URL('quotes/', cdn).href;
  await onPage(
    'frame',
    hostPage('index.html', { app: 'quotes', mode: 'frame', query: QUERIES.quotes }),
    async (page) => {
      const inFrame = (expression) => page.frame(quotesFrame, expression);
      await page.check('frame start', async () => {
        return (
          (await startProblem(page, 'frame')) ||
          (await drawingProblem(page, inFrame, 'globalThis.grid?.stats()'))
        );
      });
      await page.check('no focus grab (frame)', async () => {
        const problem = await focusProblem(page);
        if (problem) return problem;
        return (await inFrame('document.hasFocus()')) ? 'the frame has the focus' : '';
      });
      await page.check('notice line (frame)', async () => {
        if (await page.evaluate(`!!grids[0].shadowRoot.querySelector('.live-grid-credit')`)) {
          return 'the element draws a line over the page’s own';
        }
        return frameLineProblem(page, 'quotes', inFrame);
      });
      await page.check('copy (frame with allow)', async () => {
        const box = await page.rect('grids[0]');
        await page.click(box.x + ROW[0], box.y + ROW[1]);
        const selected = await page.until(async () => {
          const rows = await selection(inFrame, 'grid.grid_state()');
          return rows.length ? rows : null;
        });
        if (!selected) return 'a click on a row selected nothing';
        await inFrame(RECORD_COPIES);
        await page.copyKey();
        const copies = await page.until(() => inFrame('copies.length && copies'));
        if (!copies) return 'Ctrl+C copied nothing';
        if (copies[0][0] !== 'ok') return `Ctrl+C: ${copies[0][1]}`;
        const notice = await inFrame(`document.querySelector('body > [role=status]')?.textContent`);
        return notice ? `the notice shows: ${notice}` : '';
      });
      await page.check('wheel at the edges (frame)', async () => {
        await page.evaluate('scrollTo(0, 120)');
        await page.settle();
        return wheelProblem(
          page,
          await page.rect('grids[0]'),
          async () => JSON.parse((await inFrame('grid.grid_state()')) ?? '{}').rows?.[0],
          () => inFrame('grid.scroll_room()'),
        );
      });
      await page.check('theme (frame)', () =>
        themeProblem(page, () => inFrame('grid.current_theme()')),
      );
    },
  );

  // Cross-site frames without `allow`: the notice instead of a copy.
  await onPage('copy', hostPage('copy.html', { cross }), async (page) => {
    const frames = [
      {
        label: 'copy notice (frame without allow)',
        id: 'page',
        part: quotesFrame,
        stats: 'globalThis.grid?.stats()',
        notice: `document.querySelector('body > [role=status]')?.textContent`,
      },
      {
        label: 'copy notice (inline in a frame without allow)',
        id: 'snippet',
        part: `${cross}snippet.html`,
        stats: 'grids[0].grid?.stats()',
        notice: `grids[0].shadowRoot.querySelector('[role=status]')?.textContent`,
      },
    ];
    for (const frame of frames) {
      const inFrame = (expression) => page.frame(frame.part, expression);
      await page.check(frame.label, async () => {
        const problem = await drawingProblem(page, inFrame, frame.stats);
        if (problem) return problem;
        if (await inFrame(`document.featurePolicy?.allowsFeature('clipboard-write') !== false`)) {
          return 'the frame may copy: no notice to show';
        }
        const box = await page.rect(`document.getElementById('${frame.id}')`);
        await page.click(box.x + ROW[0], box.y + ROW[1]);
        await page.copyKey();
        const notice = await page.until(() => inFrame(frame.notice));
        return notice === COPY_NOTICE ? '' : `no notice after Ctrl+C (${notice ?? 'none'})`;
      });
    }
  });

  // A `#theme=` link, packed by the element's class, on the example's page.
  await onPage('link', hostPage('index.html', { count: 0 }), async (page) => {
    await page.check('theme link', async () => {
      const remix = {
        colors: { 'table.background': '#101030', 'base.yellow': '#ff00ff' },
        radius: 2,
      };
      const element = `customElements.get('live-grid')`;
      if (!(await page.waitFor(`!!${element}`, START_MS))) {
        return 'live-grid.js did not define the element';
      }
      const pack = (theme) => page.evaluate(`${element}.packTheme(${JSON.stringify(theme)})`);
      const packed = await pack(remix);
      const back = await page.evaluate(`${element}.unpackTheme(${JSON.stringify(packed)})`);
      if (JSON.stringify(back) !== JSON.stringify(remix)) return `unpacked ${JSON.stringify(back)}`;
      const again = await pack({ colors: { 'table.background': '#303010' } });
      const link = `${pages.page('quotes', 'seed=7')}#theme=${packed}`;
      await page.send('Page.navigate', { url: link });
      const theme = await page.waitFor(
        `(() => {
          const theme = JSON.parse(globalThis.grid?.current_theme() ?? 'null')?.theme;
          return theme?.colors['table.background'].toLowerCase() === '#101030' && theme;
        })()`,
        START_MS,
      );
      if (!theme) return 'the link did not apply its theme';
      if (theme.colors['base.yellow'].toLowerCase() !== '#ff00ff' || theme.radius !== 2) {
        return `the link applied ${JSON.stringify(theme).slice(0, 80)}`;
      }
      await page.evaluate(`location.hash = 'theme=${again}'`);
      const changed = await page.until(
        async () => tableBackground(await page.evaluate('grid.current_theme()')) === '#303010',
      );
      return changed ? '' : 'a new #theme= did not apply';
    });
  });

  // Two inline elements on one page.
  await onPage(
    'two',
    hostPage('index.html', { app: 'quotes', mode: 'inline', query: QUERIES.quotes, count: 2 }),
    async (page) => {
      await page.check('two grids', async () => {
        for (const ix of [0, 1]) {
          const problem =
            (await startProblem(page, 'inline', ix)) ||
            (await drawingProblem(page, page.evaluate, `grids[${ix}].grid?.stats()`));
          if (problem) return `grid ${ix + 1}: ${problem}`;
        }
        const canvases = await page.evaluate(
          `new Set(grids.map((grid) => grid.shadowRoot.querySelector('canvas'))).size`,
        );
        return canvases === 2 ? '' : `${canvases} canvases for two elements`;
      });
    },
  );

  // More examples: start, rows and the line, inline and in a frame, on
  // what each page hands its module first (`<app>/setup.js`).
  for (const name of APPS.filter((name) => name !== 'quotes')) {
    const [app, query] = CASES[name];
    for (const mode of ['inline', 'frame']) {
      const url = hostPage('index.html', { app, mode, query });
      const run = async (page) => {
        const appFrame = new URL(`${app}/`, cdn).href;
        const inFrame = (expression) => page.frame(appFrame, expression);
        const [read, stats] =
          mode === 'inline'
            ? [page.evaluate, 'grids[0].grid?.stats()']
            : [inFrame, 'globalThis.grid?.stats()'];
        if (app === 'orbit') {
          await page.check(`${name} ${mode} CelesTrak panel`, () => celestrakProblem(page, mode, inFrame));
        }
        await page.check(`${name} ${mode} rows`, async () => {
          return (await startProblem(page, mode)) || (await drawingProblem(page, read, stats));
        });
        if (app === 'orbit') {
          await page.check(`${name} ${mode} CelesTrak asked once`, () =>
            page.asked.length === 1 ? '' : `asked ${page.asked.length} times`,
          );
        }
        await page.check(`${name} notice line (${mode})`, async () => {
          if (mode === 'frame') return frameLineProblem(page, app, inFrame);
          const problem = await inlineLineProblem(page, app);
          if (problem || !CREDITS[name]) return problem;
          const text = await page.evaluate(
            `grids[0].shadowRoot.querySelector('.live-grid-credit')?.textContent`,
          );
          return text?.includes(CREDITS[name]) ? '' : `no credit to ${CREDITS[name]} in "${text}"`;
        });
      };
      await onPage(`${name}-${mode}`, url, run, { celestrak: app === 'orbit' });
    }
  }
} finally {
  pages.close();
  hostServer.close();
}

const failed = results.filter(([, why]) => why).length;
const seconds = Math.round((Date.now() - startedAt) / 1000);
console.log(`embed-check: ${results.length - failed} ok, ${failed} failed, ${seconds} s`);
process.exitCode = failed ? 1 : 0;
