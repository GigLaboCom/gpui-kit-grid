// The gestures that once stopped a page, each on a freshly loaded page in
// headless Chrome, with trusted input through the DevTools protocol
// (synthetic DOM events start no GPUI drag):
//
// - on every page (`quotes/`, `desk/`, `broker/`, `orbit/`, `transit/`,
//   `ops/`, and `wiki/` and `ships/` on their recordings), dragging a column
//   header, and a row of the column chooser by its grip (the chooser opened
//   from the header's menu, "Choose columns…");
// - on the desk, a click on a dock tab that moves 3 px (a drag to GPUI),
//   dragging a tab, and dragging the empty part of a tab bar;
// - on the transit page, opening Marienplatz's board of scheduled
//   departures and clicking the map, and Helsinki (`?city=helsinki`, on
//   its recording) with its header drag and chooser;
// - on the quotes page served over plain http on a name other than
//   localhost (`grid.test`, mapped to 127.0.0.1: not a secure context, so
//   the browser offers no `navigator.clipboard`), copying with Ctrl+C and
//   with the cell menu's Copy.
//
// After each step the page must still be alive: its grid draws new frames
// (`grid.stats().frames` rises while the simulator runs) and the console
// has no panic and no exception. GPUI draws a drag's view outside the
// window's root, in its default `.SystemUIFont` unless the view sets a font,
// and a page without that face panicked there; GPUI's web platform copied
// through a clipboard the browser did not offer.
//
// The group panel and row drags show on no web page: the native test
// `crates/grid/tests/web_text.rs` covers their ghosts, and the editors.
// Prints one line per gesture and the steps as JSON, writes
// `survive-<app>-<gesture>.png` (mid-gesture) to $OUT (default: the current
// directory), and exits 1 if a page stopped. Env: OUT, APPS (default
// quotes,desk,broker,orbit,transit,wiki,ships,ops; the copies run with quotes), ORIGIN (a served
// www/ instead of this checkout's; the copies then reach its host as
// `grid.test`), CHROME, CHROME_FLAGS.

import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { launch, sleep } from './chrome.mjs';
import { site } from './serve.mjs';

const OUT = process.env.OUT ?? process.cwd();
const APPS = (process.env.APPS ?? 'quotes,desk,broker,orbit,transit,wiki,ships,ops').split(',');
// The orbit example's clock starts here, as in `examples.mjs`; the transit
// example reads the same UTC time (a Friday evening in Munich).
const ORBIT_TIME = '2026-11-06T17:30:00Z';
// A name for 127.0.0.1 that is not a secure context.
const INSECURE_HOST = 'grid.test';
const CTRL = 2;
// "Choose columns…" in a header's menu, from where the menu was opened (a
// page's `choose` where the menu opens higher up).
const CHOOSE_COLUMNS = [70, 256];
// Each page's header press and the move from there, and the grip of a row
// of its chooser, in CSS pixels of the page at the default window size
// (1280 wide, the switcher's 28 px over the canvas); a page's own `choose`
// where its menu opens higher, and its own `query`. The ships page plays its
// recording: it never reaches Digitraffic here.
const PAGES = {
  quotes: { header: [330, 72], by: [140, 0], grip: [344, 231] },
  desk: { header: [196, 100], by: [130, 0], grip: [210, 260] },
  broker: { header: [141, 185], by: [80, 0], grip: [155, 282] },
  ships: { header: [210, 166], by: [130, 0], grip: [224, 308], query: '&source=replay' },
  orbit: { header: [425, 170], by: [130, 0], grip: [438, 438] },
  transit: { header: [130, 174], by: [130, 0], grip: [144, 306], choose: [70, 235] },
  // On its recording: the live stream opens only on Start.
  wiki: { header: [300, 171], by: [130, 0], grip: [313, 331], choose: [70, 237], query: '&replay' },
  // The alert console's Alert header.
  ops: { header: [230, 171], by: [130, 0], grip: [244, 367] },
};
// A quotes cell, and Copy in its menu.
const CELL = [400, 168];
const MENU_COPY = [460, 184];

const GESTURES = [
  ...Object.entries(PAGES).flatMap(([app, page]) => [
    { app, name: 'header', run: (p) => p.drag(page.header, page.by) },
    {
      app,
      name: 'chooser',
      run: async (p) => {
        const choose = page.choose ?? CHOOSE_COLUMNS;
        await p.click(page.header, 'right');
        await p.click([page.header[0] + choose[0], page.header[1] + choose[1]]);
        if (await p.alive('chooser opened')) await p.drag(page.grip, [0, 56], { back: true });
      },
    },
  ]),
  // The Watchlist tab, pressed and released 3 px away.
  { app: 'desk', name: 'tab-jitter', run: (p) => p.drag([50, 73], [3, 0], { steps: 1 }) },
  // The Chart tab, out over the chart and back.
  { app: 'desk', name: 'tab', run: (p) => p.drag([611, 73], [240, 260], { back: true }) },
  // The empty part of the News tab bar.
  { app: 'desk', name: 'tab-bar', run: (p) => p.drag([300, 484], [200, -200], { back: true }) },
  // Marienplatz's board, then a click on the map (a vehicle or a station).
  {
    app: 'transit',
    name: 'board',
    run: async (p) => {
      if (!(await p.evaluate(`grid.transit_station('Marienplatz')`))) {
        await p.fail('board', 'no station Marienplatz');
        return;
      }
      await sleep(1500);
      if (!(await p.alive('board opened'))) return;
      const stats = JSON.parse(await p.evaluate('grid.stats()'));
      if (!(stats.board > 0)) await p.fail('board', `no departures: ${JSON.stringify(stats.board)}`);
      await p.click([1060, 260]);
      await p.alive('map clicked');
    },
  },
  // Helsinki on its recording: the header and the chooser, as above.
  ...['header', 'chooser'].map((name) => ({
    app: 'transit',
    name: `helsinki-${name}`,
    query: '&city=helsinki&source=replay',
    run: async (p) => {
      const page = PAGES.transit;
      if (name === 'header') return p.drag(page.header, page.by);
      await p.click(page.header, 'right');
      await p.click([page.header[0] + page.choose[0], page.header[1] + page.choose[1]]);
      if (await p.alive('chooser opened')) await p.drag(page.grip, [0, 56], { back: true });
    },
  })),
  {
    app: 'quotes',
    name: 'copy-key',
    insecure: true,
    run: async (p) => {
      await p.click(CELL);
      await p.key('c', CTRL);
      await p.alive('Ctrl+C');
    },
  },
  {
    app: 'quotes',
    name: 'copy-menu',
    insecure: true,
    run: async (p) => {
      await p.click(CELL, 'right');
      await p.click(MENU_COPY);
      await p.alive('Copy');
    },
  },
].filter((gesture) => APPS.includes(gesture.app));

const FATAL = /panicked|RuntimeError|already borrowed|unreachable|TypeError|Uncaught/i;
// GPUI's drag starts on the first move past 2 px with the button down, so
// every move after it draws the drag's view.
const STEPS = 8;

mkdirSync(OUT, { recursive: true });
const pages = await site();
// The same pages on a name that is not a secure context.
const insecureBase = new URL(pages.base);
insecureBase.hostname = INSECURE_HOST;

// One gesture on a fresh page; `steps` records each check.
async function run(gesture) {
  const insecure = gesture.insecure ?? false;
  const chrome = await launch(
    insecure ? [`--host-resolver-rules=MAP ${INSECURE_HOST} ${pages.base.hostname}`] : [],
  );
  const { send } = chrome;
  const log = [];
  chrome.on(({ method, params }) => {
    if (method === 'Runtime.consoleAPICalled') {
      const text = params.args.map((arg) => arg.value ?? arg.description ?? '').join(' ');
      log.push(`[${params.type}] ${text}`);
    } else if (method === 'Runtime.exceptionThrown') {
      const details = params.exceptionDetails;
      log.push(`[exception] ${details.exception?.description ?? details.text}`);
    }
  });
  const steps = { checks: [] };
  const fatal = () => log.find((line) => FATAL.test(line));
  let frames = 0;
  let dead = null;
  const mouse = (type, [x, y], button = 'left', buttons = 0) =>
    send('Input.dispatchMouseEvent', {
      type,
      x,
      y,
      button: type === 'mouseMoved' && !buttons ? 'none' : button,
      buttons,
      clickCount: type === 'mouseMoved' ? 0 : 1,
    });
  const page = {
    // Alive: no panic so far, and a frame after the last check within two
    // seconds.
    async alive(label) {
      if (dead) return false;
      for (let waited = 0; waited <= 2000; waited += 100) {
        const why = fatal();
        if (why) {
          dead = { at: label, why };
          return false;
        }
        const stats = JSON.parse((await chrome.evaluate('grid.stats()')) ?? 'null');
        if (!stats) {
          dead = { at: label, why: 'grid.stats() failed' };
          return false;
        }
        if (stats.frames > frames) {
          frames = stats.frames;
          steps.checks.push(label);
          return true;
        }
        await sleep(100);
      }
      dead = { at: label, why: `no frame after ${frames} in 2 s` };
      return false;
    },
    evaluate: (expression) => chrome.evaluate(expression),
    async fail(at, why) {
      dead ??= { at, why };
    },
    async click(point, button = 'left') {
      await mouse('mouseMoved', point);
      await mouse('mousePressed', point, button, button === 'left' ? 1 : 2);
      await mouse('mouseReleased', point, button);
      await sleep(400);
    },
    async key(name, modifiers = 0) {
      const code = `Key${name.toUpperCase()}`;
      const keyCode = name.toUpperCase().charCodeAt(0);
      for (const type of ['rawKeyDown', 'keyUp']) {
        await send('Input.dispatchKeyEvent', {
          type,
          key: name,
          code,
          windowsVirtualKeyCode: keyCode,
          nativeVirtualKeyCode: keyCode,
          modifiers,
        });
      }
      await sleep(300);
    },
    // Presses at `from`, moves by `by` in `steps` (and back with `back`),
    // checking the page after each move, and releases.
    async drag(from, by, { steps: count = STEPS, back = false } = {}) {
      const at = (step) => [
        Math.round(from[0] + (by[0] * step) / count),
        Math.round(from[1] + (by[1] * step) / count),
      ];
      const moves = [...Array(count).keys()].map((ix) => at(ix + 1));
      if (back) moves.push(...moves.slice(0, -1).reverse(), from);
      await mouse('mouseMoved', from);
      await mouse('mousePressed', from, 'left', 1);
      for (const [ix, point] of moves.entries()) {
        if (!(await page.alive(ix === 0 ? 'pressed' : `move ${ix}`))) break;
        await mouse('mouseMoved', point, 'left', 1);
        await sleep(60);
        if (ix === count - 1 && !fatal()) {
          await chrome.screenshot(join(OUT, `survive-${gesture.app}-${gesture.name}.png`));
        }
      }
      await mouse('mouseReleased', moves[moves.length - 1]);
      await sleep(300);
      await page.alive('released');
    },
  };

  try {
    await send('Runtime.enable');
    await send('Page.enable');
    const base = insecure ? insecureBase : pages.base;
    const query = gesture.query ?? PAGES[gesture.app]?.query ?? '';
    const url = pages.page(gesture.app, `seed=7&time=${ORBIT_TIME}${query}`, base);
    await send('Page.navigate', { url });
    for (let waited = 0; ; waited += 100) {
      const stats = JSON.parse((await chrome.evaluate('globalThis.grid?.stats()')) ?? 'null');
      if (stats?.visible_cells > 0) break;
      if (waited > 30000) throw new Error(`${gesture.app}: no rows after 30 s`);
      await sleep(100);
    }
    await chrome.evaluate('globalThis.fontsReady');
    await sleep(1000);
    if (insecure) {
      // What the page offers, what GPUI asks it to copy, and what the
      // browser's copy events carried.
      steps.context = await chrome.evaluate(`(() => {
        globalThis.copies = [];
        globalThis.copied = [];
        document.addEventListener('copy', (event) => {
          copied.push(event.clipboardData.getData('text/plain'));
        });
        const clipboard = navigator.clipboard;
        if (clipboard) {
          const write = clipboard.writeText.bind(clipboard);
          clipboard.writeText = (text) => {
            copies.push(String(text));
            return write(text);
          };
        }
        return { secure: isSecureContext, clipboard: typeof clipboard };
      })()`);
    }
    if (await page.alive('loaded')) await gesture.run(page);
    if (insecure) {
      steps.asked = await chrome.evaluate('globalThis.copies');
      steps.copied = await chrome.evaluate('globalThis.copied');
      if (!dead && (steps.copied.length === 0 || steps.copied.join() !== steps.asked.join())) {
        dead = { at: 'copy', why: 'the browser copied nothing, or other text' };
      }
    }
  } finally {
    await chrome.close();
  }
  steps.alive = !dead;
  if (dead) steps.stopped = dead;
  steps.console = log.filter((line) => FATAL.test(line) || /^\[(error|warning)\]/.test(line));
  return steps;
}

const results = {};
try {
  for (const gesture of GESTURES) {
    const steps = await run(gesture).catch((error) => ({ alive: false, error: String(error) }));
    results[`${gesture.app} ${gesture.name}`] = steps;
    const reason = steps.stopped?.why ?? steps.error;
    const state = steps.alive
      ? `alive${steps.copied ? ` (copied ${JSON.stringify(steps.copied)})` : ''}`
      : `STOPPED at ${steps.stopped?.at ?? 'start'} - ${reason?.split('\n').slice(0, 2).join(' ')}`;
    process.stdout.write(`${gesture.app} ${gesture.name}: ${state}\n`);
  }
} finally {
  pages.close();
}
console.log(JSON.stringify(results, null, 2));
process.exitCode = Object.values(results).every((steps) => steps.alive) ? 0 : 1;
