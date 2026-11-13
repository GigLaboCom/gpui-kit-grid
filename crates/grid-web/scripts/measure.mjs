// Runs an example's page in headless Chrome (`--headless=new`: no window)
// and prints its `GRID_BENCH` result as JSON: load and first-frame times,
// frame times at each update rate, and the graphics backend GPUI chose.
// Writes `screenshot.png` (DevTools capture after the run),
// `screenshot-cli.png` (Chrome's own `--screenshot`) and `console.log` to
// $OUT.
//
// Env: OUT (default: the current directory), RATES (default 300,5000),
// SECONDS per rate (default 5), QUERY (the page and more of its parameters:
// `app=desk` for the desk's page, the quotes' by default; `rows=50000&sort=chg`),
// ORIGIN (a served www/ instead of this checkout's), CHROME (the browser
// binary), CHROME_FLAGS (extra flags, space separated). DevTools runs over a
// pipe, not a port.

import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CHROME, COMMON, launch } from './chrome.mjs';
import { site } from './serve.mjs';

const OUT = process.env.OUT ?? process.cwd();
const RATES = process.env.RATES ?? '300,5000';
const SECONDS = Number(process.env.SECONDS ?? 5);
// `app=` picks the page; the rest is passed on as written.
const PAIRS = (process.env.QUERY ?? '').split('&').filter(Boolean);
const APP = PAIRS.find((pair) => pair.startsWith('app='))?.slice('app='.length) || 'quotes';
const QUERY = PAIRS.filter((pair) => !pair.startsWith('app=')).map((pair) => `&${pair}`).join('');

mkdirSync(OUT, { recursive: true });
const pages = await site();
const url = pages.page(APP, `bench&seed=7&rates=${RATES}&seconds=${SECONDS}${QUERY}`);

const chrome = await launch();
const send = chrome.send;

const lines = [];
let finish;
const finished = new Promise((resolve) => (finish = resolve));
chrome.on(({ method, params }) => {
  if (method === 'Runtime.consoleAPICalled') {
    const text = params.args.map((arg) => arg.value ?? arg.description ?? '').join(' ');
    lines.push(`[${params.type}] ${text}`);
    if (text.startsWith('GRID_BENCH ')) finish(JSON.parse(text.slice('GRID_BENCH '.length)));
    if (text.startsWith('GRID_ERROR ')) finish({ error: text });
  } else if (method === 'Runtime.exceptionThrown') {
    const details = params.exceptionDetails;
    lines.push(`[exception] ${details.exception?.description ?? details.text}`);
  } else if (method === 'Log.entryAdded') {
    lines.push(`[${params.entry.source} ${params.entry.level}] ${params.entry.text}`);
  }
});
await send('Runtime.enable');
await send('Log.enable');
await send('Page.enable');
const info = await chrome.browser('SystemInfo.getInfo').catch(() => null);
await send('Page.navigate', { url });

const limit = 60 + RATES.split(',').length * (SECONDS + 2);
const timer = setTimeout(() => finish({ error: `no result in ${limit} s` }), limit * 1000);
const result = await finished;
clearTimeout(timer);

await chrome.screenshot(join(OUT, 'screenshot.png'));
await chrome.close();

result.backend =
  lines.find((line) => line.includes('graphics initialized'))?.replace(/^\[\w+\] /, '') ?? null;
result.gpu = info && {
  devices: info.gpu.devices.map((device) => `${device.vendorString} ${device.deviceString}`.trim()),
  featureStatus: Object.fromEntries(
    Object.entries(info.gpu.featureStatus ?? {}).filter(([name]) =>
      /webgpu|webgl2|gpu_compositing|rasterization/.test(name),
    ),
  ),
};
writeFileSync(join(OUT, 'console.log'), `${lines.join('\n')}\n`);
console.log(JSON.stringify(result, null, 2));

// Chrome's own headless capture of the page, without the bench, after a
// virtual-time budget long enough for the first rows.
const cliProfile = mkdtempSync(join(tmpdir(), 'grid-web-chrome-'));
await new Promise((resolve) =>
  spawn(
    CHROME,
    [
      ...COMMON,
      `--user-data-dir=${cliProfile}`,
      `--screenshot=${join(OUT, 'screenshot-cli.png')}`,
      '--virtual-time-budget=8000',
      pages.page(APP, `seed=7${QUERY}`),
    ],
    { stdio: 'ignore' },
  ).on('exit', resolve),
);
pages.close();
rmSync(cliProfile, { recursive: true, force: true });
