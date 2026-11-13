// The desk, the broker and orbit in headless Chrome at a device scale of 2
// (Chrome's own `--force-device-scale-factor`: GPUI ignores DevTools
// emulation), in a 1600×960 canvas like their native snapshots. Captures
// each desk theme, the broker light, dark and red-up, and orbit flat,
// grouped, as a tree, with a selected object and at time ×60, then drives
// every key of the three (and clicks on the broker ticket's Limit tab and
// on an orbit row) through the DevTools protocol and reads each step back
// from the page's `grid_state` hook. Prints one line a step (`true`, or the
// page's state where a key did not do its job) and writes `desk-*.png`,
// `broker-*.png` and `web-orbit-*.png` to $OUT (default: the current
// directory). `APPS=orbit` (a comma-separated list) runs some of them.
//
// A key is sent as a browser on macOS reports it: Option with a letter
// arrives as the character it types (Option-B as `∫`); the `-plain` steps
// send the letter, as on Windows and Linux. `secondary` is Ctrl in the
// browser on every system. Env: OUT, APPS, ORIGIN (a served www/ instead of
// this checkout's), CHROME, CHROME_FLAGS.

import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { launch, sleep } from './chrome.mjs';
import { site } from './serve.mjs';

const OUT = process.env.OUT ?? process.cwd();
const [WIDTH, HEIGHT] = [1600, 960];
// The page's switcher over the canvas (`www/shell.css`).
const NAV = 28;
const [ALT, CTRL, SHIFT] = [1, 2, 8];
// The broker ticket's Limit tab in the canvas: its column keeps to the
// right edge.
const [LIMIT_TAB_FROM_RIGHT, LIMIT_TAB_Y] = [251, 405];
// An orbit data row in the canvas: the fourth below the two pinned ones,
// in the Name column.
const [ORBIT_ROW_X, ORBIT_ROW_Y] = [100, 286];
// The orbit example's clock starts here, as its native snapshot's does.
const ORBIT_TIME = '2026-11-06T17:30:00Z';
const APPS = (process.env.APPS ?? 'desk,broker,orbit').split(',');

mkdirSync(OUT, { recursive: true });
const pages = await site();
const results = {};

// One page of `app` in its own Chrome; `run` drives it.
async function withPage(app, query, run) {
  const chrome = await launch(['--force-device-scale-factor=2', `--window-size=${WIDTH},${HEIGHT}`]);
  const errors = [];
  chrome.on(({ method, params }) => {
    if (method === 'Runtime.exceptionThrown') {
      errors.push(params.exceptionDetails.exception?.description ?? params.exceptionDetails.text);
    } else if (method === 'Runtime.consoleAPICalled' && params.type === 'error') {
      errors.push(params.args.map((arg) => arg.value ?? arg.description ?? '').join(' '));
    }
  });
  try {
    await chrome.send('Runtime.enable');
    await chrome.send('Page.enable');
    // The browser's own bars take a height that differs by system and can
    // change while the page runs: the window is fitted to the canvas, again
    // if a change of the bars was still under way. Returns whether it was
    // resized.
    const { windowId } = await chrome.send('Browser.getWindowForTarget');
    const fit = async () => {
      for (let attempt = 0; attempt < 3; attempt++) {
        const { bounds } = await chrome.browser('Browser.getWindowBounds', { windowId });
        const [width, height] = await chrome.evaluate('[innerWidth, innerHeight]');
        if (width === WIDTH && height === HEIGHT + NAV) return attempt > 0;
        await chrome.browser('Browser.setWindowBounds', {
          windowId,
          bounds: {
            width: bounds.width + WIDTH - width,
            height: bounds.height + HEIGHT + NAV - height,
          },
        });
        await sleep(300);
      }
    };
    await fit();
    await chrome.send('Page.navigate', { url: pages.page(app, query) });
    for (let waited = 0; ; waited += 100) {
      const stats = JSON.parse((await chrome.evaluate('globalThis.grid?.stats()')) ?? 'null');
      if (stats?.visible_cells > 0) break;
      if (waited > 30000) throw new Error(`${app}: no rows after 30 s`);
      await sleep(100);
    }
    await chrome.evaluate('globalThis.fontsReady');
    const page = {
      ...chrome,
      canvas: await chrome.evaluate(`(() => {
        const canvas = document.querySelector('canvas');
        return {
          css: [canvas.clientWidth, canvas.clientHeight],
          px: [canvas.width, canvas.height],
          devicePixelRatio,
        };
      })()`),
      state: async () => JSON.parse((await chrome.evaluate('grid.grid_state()')) ?? 'null'),
      origin: await chrome.evaluate('performance.timeOrigin'),
      async key(name, { code = name, keyCode = 0, modifiers = 0, text } = {}) {
        for (const type of [text ? 'keyDown' : 'rawKeyDown', 'keyUp']) {
          await chrome.send('Input.dispatchKeyEvent', {
            type,
            key: name,
            code,
            windowsVirtualKeyCode: keyCode,
            nativeVirtualKeyCode: keyCode,
            modifiers,
            ...(type === 'keyDown' && { text }),
          });
        }
        await sleep(250);
      },
      async type(text) {
        for (const char of text) {
          const letter = /[a-z]/i.test(char);
          await page.key(char, {
            code: char === ' ' ? 'Space' : letter ? `Key${char.toUpperCase()}` : `Digit${char}`,
            keyCode: char.toUpperCase().charCodeAt(0),
            modifiers: /[A-Z]/.test(char) ? SHIFT : 0,
            text: char,
          });
        }
      },
      async enter() {
        await page.key('Enter', { keyCode: 13, text: '\r' });
      },
      async fkey(n) {
        await page.key(`F${n}`, { keyCode: 111 + n });
      },
      // A left click at (x, y) in the canvas.
      async click(x, y) {
        await chrome.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y: y + NAV });
        for (const type of ['mousePressed', 'mouseReleased']) {
          await chrome.send('Input.dispatchMouseEvent', {
            type,
            x,
            y: y + NAV,
            button: 'left',
            clickCount: 1,
          });
        }
        await sleep(250);
      },
      // The canvas alone, without the switcher; fitted again if a bar came
      // or went while it settled, and laid out again then.
      async shot(name, settle = 1200) {
        await fit();
        await sleep(settle);
        if (await fit()) await sleep(1000);
        const shot = await chrome.send('Page.captureScreenshot', {
          format: 'png',
          clip: { x: 0, y: NAV, width: WIDTH, height: HEIGHT, scale: 1 },
        });
        writeFileSync(join(OUT, `${name}.png`), Buffer.from(shot.data, 'base64'));
      },
    };
    // A letter key with its text, as a keyboard types it.
    page.letter = (char) =>
      page.key(char, { code: `Key${char.toUpperCase()}`, keyCode: char.toUpperCase().charCodeAt(0), text: char });
    const steps = {};
    // Runs `act`, then checks the page's state with `expect`. A page that
    // stopped (a panic) has no state.
    page.step = async (name, act, expect) => {
      await act();
      const state = await page.state();
      if (!state) {
        steps[name] = 'no state: the page stopped';
        return;
      }
      const ok = Boolean(expect(state.app, state));
      // The page's state after a key that did not do its job.
      const { orders, ...app } = state.app;
      steps[name] = ok || { ...app, orders: orders?.map((order) => order.side), selected: state.selected };
    };
    await run(page, steps);
    steps.reloaded = (await chrome.evaluate('performance.timeOrigin')) !== page.origin;
    results[app] = { canvas: page.canvas, steps, errors };
  } finally {
    await chrome.close();
  }
}

try {
  if (APPS.includes('desk')) await withPage('desk', 'seed=7', async (page) => {
    // The simulator runs a few seconds first, as for the native snapshots.
    await sleep(4000);
    await page.shot('desk-amber', 0);
    await page.step('F3 Depth', () => page.fkey(3), (app) => app.theme === 'Depth');
    await page.shot('desk-depth');
    await page.step('F4 Heat', () => page.fkey(4), (app) => app.theme === 'Heat');
    await page.shot('desk-heat');
    await page.step('F2 Amber', () => page.fkey(2), (app) => app.theme === 'Amber');
    await page.step('F1 help', () => page.fkey(1), (app) => app.help);
    await page.step('F1 again', () => page.fkey(1), (app) => !app.help);
    const timeframe = (await page.state()).app.timeframe;
    await page.step('F5 timeframe', () => page.fkey(5), (app) => app.timeframe !== timeframe);
    await page.step('F6 command line', () => page.fkey(6), (app) => app.focus === 'command');
    await page.step(
      'type msft',
      async () => {
        await page.type('msft');
        await page.enter();
      },
      (app) => app.current === 'MSFT',
    );
    await page.step(
      'type THEME heat',
      async () => {
        await page.type('THEME heat');
        await page.enter();
      },
      (app) => app.theme === 'Heat',
    );
    await page.step(
      'type tf 5s',
      async () => {
        await page.type('tf 5s');
        await page.enter();
      },
      (app) => app.timeframe === '5s',
    );
    await page.shot('desk-command');
    await page.step('F7 watchlist', () => page.fkey(7), (app) => app.focus === 'watchlist');
    const before = (await page.state()).selected[0];
    await page.step(
      'arrow down',
      () => page.key('ArrowDown', { keyCode: 40 }),
      (app, grid) => grid.selected[0] !== before && app.current === grid.selected[0],
    );
    await page.step(
      'arrow up',
      () => page.key('ArrowUp', { keyCode: 38 }),
      (app, grid) => grid.selected[0] === before && app.current === before,
    );
    await page.step(
      'page down',
      () => page.key('PageDown', { keyCode: 34 }),
      (app, grid) => grid.selected[0] !== before,
    );
    await page.step('F8 news', () => page.fkey(8), (app) => app.focus === 'news');
    await page.step('F9 book', () => page.fkey(9), (app) => app.focus === 'book');
    await page.step('F10 trades', () => page.fkey(10), (app) => app.focus === 'tape');
    await page.step('F12 positions', () => page.fkey(12), (app) => app.focus === 'positions');
    await page.step('F2 Amber', () => page.fkey(2), (app) => app.theme === 'Amber');
  });

  if (APPS.includes('broker')) await withPage('broker', 'seed=11', async (page) => {
    await sleep(5000);
    // Buy 10 AAPL at the market, as the native snapshot does first.
    await page.step(
      'Option-B Buy, Option-Enter',
      async () => {
        await page.key('∫', { code: 'KeyB', keyCode: 66, modifiers: ALT, text: '∫' });
        await page.key('Enter', { keyCode: 13, modifiers: ALT, text: '\r' });
      },
      (app) => app.orders.at(-1)?.side === 'Buy',
    );
    await page.shot('broker-light');
    await page.step(
      'Option-S Sell, Option-Enter',
      async () => {
        await page.key('ß', { code: 'KeyS', keyCode: 83, modifiers: ALT, text: 'ß' });
        await page.key('Enter', { keyCode: 13, modifiers: ALT, text: '\r' });
      },
      (app) => app.orders.length === 2 && app.orders.at(-1).side === 'Sell',
    );
    await page.step(
      'Alt-B-plain Buy, Alt-Enter',
      async () => {
        await page.key('b', { code: 'KeyB', keyCode: 66, modifiers: ALT });
        await page.key('Enter', { keyCode: 13, modifiers: ALT, text: '\r' });
      },
      (app) => app.orders.length === 3 && app.orders.at(-1).side === 'Buy',
    );
    await page.step(
      'Ctrl-2 Orders',
      () => page.key('2', { code: 'Digit2', keyCode: 50, modifiers: CTRL }),
      (app) => app.tab === 'Orders',
    );
    await page.step(
      'Option-T dark',
      () => page.key('†', { code: 'KeyT', keyCode: 84, modifiers: ALT, text: '†' }),
      (app) => app.theme === 'Dark',
    );
    await page.shot('broker-dark');
    await page.step(
      'Alt-T-plain light',
      () => page.key('t', { code: 'KeyT', keyCode: 84, modifiers: ALT }),
      (app) => app.theme === 'Light',
    );
    await page.step(
      'Option-C red up',
      () => page.key('ç', { code: 'KeyC', keyCode: 67, modifiers: ALT, text: 'ç' }),
      (app) => app.colors === 'Red up',
    );
    await page.step(
      'Ctrl-3 Movers',
      () => page.key('3', { code: 'Digit3', keyCode: 51, modifiers: CTRL }),
      (app) => app.tab === 'Movers',
    );
    await page.shot('broker-red-up');
    await page.step(
      'Alt-C-plain green up',
      () => page.key('c', { code: 'KeyC', keyCode: 67, modifiers: ALT }),
      (app) => app.colors === 'Green up',
    );
    await page.step(
      'Ctrl-1 Positions',
      () => page.key('1', { code: 'Digit1', keyCode: 49, modifiers: CTRL }),
      (app) => app.tab === 'Positions',
    );
    // A new instrument while the Market order hides the ticket's limit
    // price, then the Limit tab: both panicked in the browser while that
    // field had never been drawn. The first arrow focuses the open
    // instrument's row (selected while the tab was hidden), the second
    // moves on.
    const opened = (await page.state()).app.current;
    await page.step(
      'arrow down positions',
      async () => {
        await page.key('ArrowDown', { keyCode: 40 });
        await page.key('ArrowDown', { keyCode: 40 });
      },
      (app) => app.focus === 'positions' && app.current !== opened,
    );
    await page.step(
      'click Limit',
      () => page.click(WIDTH - LIMIT_TAB_FROM_RIGHT, LIMIT_TAB_Y),
      (app) => app.order_type === 'Limit',
    );
    await page.step(
      'Ctrl-F search',
      () => page.key('f', { code: 'KeyF', keyCode: 70, modifiers: CTRL }),
      (app) => app.focus === 'search',
    );
    await page.step(
      'type ap',
      () => page.type('ap'),
      (app) => app.search === 'ap' && app.shown > 0 && app.shown < 24,
    );
    await page.step(
      'type zzz',
      () => page.type('zzz'),
      (app) => app.search === 'apzzz' && app.shown === 0,
    );
    await page.shot('broker-search');
  });

  if (APPS.includes('orbit')) await withPage('orbit', `time=${ORBIT_TIME}`, async (page) => {
    // A few sweeps, the ISS selected (its detail and pass), as natively;
    // the settling lets GPUI lay out again if the window was fitted.
    await sleep(2500);
    await page.shot('web-orbit-sky');
    await page.step(
      'H above horizon',
      () => page.letter('h'),
      (app) => app.up_only && app.shown === app.up,
    );
    await page.step('G grouping', () => page.letter('g'), (app) => app.arrangement === 'By constellation');
    await page.shot('web-orbit-grouped');
    await page.step(
      'T tree',
      () => page.letter('t'),
      // The tree keeps the shells and constellations of what it shows.
      (app) => app.arrangement === 'Tree' && app.shown > app.up,
    );
    await page.step('H every object', () => page.letter('h'), (app) => !app.up_only);
    await page.shot('web-orbit-tree');
    await page.step(
      'T flat',
      () => page.letter('t'),
      (app) => app.arrangement === 'Flat' && app.shown === app.objects,
    );
    await page.step(
      'click a row',
      async () => {
        await page.click(ORBIT_ROW_X, ORBIT_ROW_Y);
        // The detail follows on the next sweep, at most a second later.
        await sleep(1300);
      },
      (app, grid) => grid.selected.length === 1 && app.detail === grid.selected[0] && app.selected !== 25544,
    );
    await page.step('N next pass', () => page.letter('n'), (app) => app.pass_skip === 1);
    await page.shot('web-orbit-selected');
    await page.step('W time ×10', () => page.letter('w'), (app) => app.warp === 10);
    await page.step('W time ×60', () => page.letter('w'), (app) => app.warp === 60);
    await page.shot('web-orbit-warp', 3000);
    await page.step('W real time', () => page.letter('w'), (app) => app.warp === 1);
    await page.step(
      'Ctrl-F search',
      async () => {
        await page.key('f', { code: 'KeyF', keyCode: 70, modifiers: CTRL });
        await page.type('iss');
      },
      (app) => app.search === 'iss' && app.shown > 0 && app.shown < app.objects,
    );
    await page.step(
      'Enter, then H in the table',
      async () => {
        await page.enter();
        await page.letter('h');
      },
      // The letter went to the table, not the search box.
      (app) => app.search === 'iss' && app.up_only,
    );
  });
} finally {
  pages.close();
}
// One line a step: `true`, or the page's state after it.
for (const [app, { steps, ...rest }] of Object.entries(results)) {
  console.log(app, JSON.stringify(rest));
  for (const [name, step] of Object.entries(steps)) console.log(`  ${name}: ${JSON.stringify(step)}`);
}
