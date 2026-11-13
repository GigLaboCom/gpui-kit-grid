// High-DPI rendering and input in headless Chrome: the quotes page at
// device scale factor 2, then input through the DevTools protocol (the wheel scrolls the
// rows, a click selects a row, arrow keys move the selection, a header click
// sorts), each step read back from the page's `grid_state` hook and
// captured. Prints the steps as JSON and writes `hidpi.png` and
// `input-<step>.png` to $OUT (default: the current directory).
//
// Scale 2 comes from DevTools emulation (`Emulation.setDeviceMetricsOverride`
// with `deviceScaleFactor: 2`), or from Chrome itself with
// CHROME_FLAGS=--force-device-scale-factor=2. Under emulation Chrome reports
// the canvas's device-pixel size at scale 1, so GPUI lays the page out at
// half the viewport; `hidpi` shows the sizes it saw.
//
// The simulator runs at rate 0, so nothing moves between the steps.
// Env: OUT, ORIGIN (a served www/ instead of this checkout's), CHROME,
// CHROME_FLAGS.

import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { launch, sleep } from './chrome.mjs';
import { site } from './serve.mjs';

const OUT = process.env.OUT ?? process.cwd();
const [WIDTH, HEIGHT] = [1280, 800];
// The page's switcher over the canvas (`www/shell.css`), the quotes'
// status line and the grid's header, in CSS pixels (`src/quotes.rs`: a
// 32 px status line, 24 px rows).
const NAV = 28;
const STATUS = NAV + 32;
const ROW = 24;
// The Last column's header: after Ticker (84 px) and Name (200 px); Sector
// is hidden.
const LAST_HEADER = { x: 84 + 200 + 46, y: STATUS + ROW / 2 };

const EMULATE = !/--force-device-scale-factor/.test(process.env.CHROME_FLAGS ?? '');

mkdirSync(OUT, { recursive: true });
const pages = await site();
const chrome = await launch();
const { send } = chrome;
const state = async () => JSON.parse((await chrome.evaluate('grid.grid_state()')) ?? 'null');

async function click(x, y) {
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
  for (const type of ['mousePressed', 'mouseReleased']) {
    await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
  }
  await sleep(300);
}

async function key(name, code, keyCode) {
  for (const type of ['rawKeyDown', 'keyUp']) {
    await send('Input.dispatchKeyEvent', {
      type,
      key: name,
      code,
      windowsVirtualKeyCode: keyCode,
      nativeVirtualKeyCode: keyCode,
    });
  }
  await sleep(300);
}

const steps = {};
try {
  await send('Page.enable');
  if (EMULATE) {
    await send('Emulation.setDeviceMetricsOverride', {
      width: WIDTH,
      height: HEIGHT,
      deviceScaleFactor: 2,
      mobile: false,
    });
  }
  await send('Page.navigate', { url: pages.page('quotes', 'seed=7&rate=0') });
  for (let waited = 0; ; waited += 100) {
    const stats = JSON.parse((await chrome.evaluate('grid.stats()')) ?? 'null');
    if (stats?.visible_cells > 0) break;
    if (waited > 30000) throw new Error('no rows after 30 s');
    await sleep(100);
  }
  await sleep(500);

  steps.hidpi = await chrome.evaluate(`(() => {
    const canvas = document.querySelector('canvas');
    return {
      scale: ${EMULATE ? "'emulated'" : "'forced'"},
      devicePixelRatio,
      cssSize: [canvas.clientWidth, canvas.clientHeight],
      canvasSize: [canvas.width, canvas.height],
    };
  })()`);
  await chrome.screenshot(join(OUT, 'hidpi.png'));
  steps.start = await state();

  // The wheel over the rows: five rows down.
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 600, y: 300 });
  await send('Input.dispatchMouseEvent', {
    type: 'mouseWheel',
    x: 600,
    y: 300,
    deltaX: 0,
    deltaY: 5 * ROW,
  });
  await sleep(300);
  steps.wheel = await state();
  steps.wheel.scrolled = steps.wheel.rows[0] > steps.start.rows[0];
  await chrome.screenshot(join(OUT, 'input-wheel.png'));

  // A click on the Name of the third line under the header; one line down
  // if that was a group caption.
  for (let line = 2; line < 6; line++) {
    await click(400, STATUS + ROW + line * ROW + ROW / 2);
    steps.click = await state();
    if (steps.click.selected.length > 0) break;
  }
  steps.click.selectedRow = steps.click.selected.length === 1;
  await chrome.screenshot(join(OUT, 'input-click.png'));

  // Arrow down: the next data row in display order (or, after a group's
  // last row, the next group's caption: no row focused).
  await key('ArrowDown', 'ArrowDown', 40);
  steps.arrowDown = await state();
  const { visible } = steps.arrowDown;
  steps.arrowDown.movedToNext =
    visible.indexOf(steps.arrowDown.selected[0]) === visible.indexOf(steps.click.selected[0]) + 1 ||
    steps.arrowDown.focused === null;
  await key('ArrowUp', 'ArrowUp', 38);
  steps.arrowUp = await state();
  steps.arrowUp.movedBack = steps.arrowUp.selected[0] === steps.click.selected[0];
  await chrome.screenshot(join(OUT, 'input-keys.png'));

  // A click on the Last header sorts by it.
  await click(LAST_HEADER.x, LAST_HEADER.y);
  steps.header = await state();
  steps.header.sortedByLast = steps.header.sort[0]?.[0] === 'last';
  await chrome.screenshot(join(OUT, 'input-header.png'));
} finally {
  await chrome.close();
  pages.close();
}
console.log(JSON.stringify(steps, null, 2));
