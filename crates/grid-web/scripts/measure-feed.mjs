// Measures the quotes page fed by the gateway (`crates/grid-gateway`) in
// headless Chrome and prints the result as JSON:
//
// - per gateway rate (RATES, default 300,5000): the page's frame work as
//   `measure.mjs` reports it, the server → ingest latency (p50, p99) and the
//   conflation ratio (updates the simulator made / rows sent to the page);
// - `--drop-every 100` at 5000 updates/s for 10 s: the resyncs;
// - `--stall-ms 200` at 5000 updates/s for 5 s: a slow client's conflation;
// - three kills and restarts of the gateway: how long until the page shows
//   live again.
//
// Env: GATEWAY (the binary; default `$CARGO_TARGET_DIR/release/grid-gateway`,
// built with `cargo build --release -p gpui-kit-grid-gateway`), RATES,
// SECONDS per rate (default 5), OUT (writes `feed.png`, a capture of the
// live page, when set), ORIGIN (a served www/ instead of this checkout's),
// CHROME, CHROME_FLAGS. Binds 127.0.0.1 only.

import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { launch, sleep } from './chrome.mjs';
import { site } from './serve.mjs';

const TARGET =
  process.env.CARGO_TARGET_DIR ?? fileURLToPath(new URL('../../../target', import.meta.url));
const GATEWAY = process.env.GATEWAY ?? join(TARGET, 'release', 'grid-gateway');
const RATES = (process.env.RATES ?? '300,5000').split(',').map(Number);
const SECONDS = Number(process.env.SECONDS ?? 5);
const OUT = process.env.OUT;

// A free port for the gateway, the same across its restarts.
const port = await new Promise((resolve) => {
  const probe = createServer().listen(0, '127.0.0.1', () => {
    const { port } = probe.address();
    probe.close(() => resolve(port));
  });
});
const feedUrl = `ws://127.0.0.1:${port}`;

// Starts the gateway; `ready` resolves once it listens. `stats` holds its
// counter lines, parsed, with the time each arrived.
function startGateway(...flags) {
  const child = spawn(GATEWAY, ['--port', String(port), '--seed', '7', ...flags], {
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const gateway = { child, stats: [], exited: new Promise((r) => child.once('exit', r)) };
  gateway.ready = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code) => reject(new Error(`the gateway exited (${code})`)));
    createInterface({ input: child.stdout }).on('line', (line) => {
      if (line.startsWith('listening ')) resolve(performance.now());
      if (line.startsWith('stats ')) {
        const fields = Object.fromEntries(
          line
            .split(' ')
            .slice(1)
            .map((field) => field.split('='))
            .map(([name, value]) => [name, Number(value)]),
        );
        gateway.stats.push({ at: performance.now(), ...fields });
      }
    });
  });
  return gateway;
}

async function stopGateway(gateway, signal = 'SIGTERM') {
  gateway.child.kill(signal);
  await gateway.exited;
}

// Updates made and rows sent while a page was connected: from the first
// counter line with a session to the last.
function conflation(gateway) {
  const connected = gateway.stats.filter((line) => line.sessions > 0);
  if (connected.length < 2) return null;
  const [first, last] = [connected[0], connected[connected.length - 1]];
  const produced = last.produced - first.produced;
  const rowsSent = last.rows_sent - first.rows_sent;
  return {
    seconds: round((last.at - first.at) / 1000),
    produced,
    rowsSent,
    deltas: last.deltas - first.deltas,
    ratio: round(produced / rowsSent),
  };
}

const round = (value) => Math.round(value * 100) / 100;

async function waitFor(read, test, timeoutMs) {
  const started = performance.now();
  for (;;) {
    const value = await read();
    if (test(value)) return value;
    if (performance.now() - started > timeoutMs) throw new Error('timed out');
    await sleep(10);
  }
}

const pages = await site();
const quotes = (query) => pages.page('quotes', query);
const chrome = await launch();
const feedStats = async (reset = false) =>
  JSON.parse((await chrome.evaluate(`globalThis.grid?.feed_stats(${reset}) ?? 'null'`)) ?? 'null');
const isLive = (stats) => stats?.state === 'live';

let bench = null;
chrome.on(({ method, params }) => {
  if (method !== 'Runtime.consoleAPICalled') return;
  const text = params.args.map((arg) => arg.value ?? '').join(' ');
  if (text.startsWith('GRID_BENCH ')) bench?.(JSON.parse(text.slice('GRID_BENCH '.length)));
  if (text.startsWith('GRID_ERROR ')) bench?.({ error: text });
});
await chrome.send('Runtime.enable');
await chrome.send('Page.enable');

const result = { feed: feedUrl, rates: [] };
const gateways = [];
try {
  for (const rate of RATES) {
    const gateway = startGateway('--rate', String(rate));
    gateways.push(gateway);
    await gateway.ready;
    const done = new Promise((resolve) => (bench = resolve));
    await chrome.send('Page.navigate', {
      url: quotes(`bench&feed=${feedUrl}&rates=${rate}&seconds=${SECONDS}`),
    });
    const page = await Promise.race([done, sleep((40 + SECONDS) * 1000).then(() => null)]);
    if (OUT && rate === RATES[RATES.length - 1]) {
      mkdirSync(OUT, { recursive: true });
      await chrome.screenshot(join(OUT, 'feed.png'));
    }
    await chrome.send('Page.navigate', { url: 'about:blank' });
    await sleep(1500); // the last counter line after the page left
    await stopGateway(gateway);
    const phase = page?.phases?.[0];
    result.rates.push({
      rate,
      load: page?.load,
      frameWorkMs: phase?.frameWorkMs,
      displayIntervalMs: phase?.displayIntervalMs,
      gpuiFps: phase?.gpuiFps,
      rowsPerSecond: phase?.measuredRate,
      latencyMs: phase?.feed?.latency_ms,
      feed: phase?.feed && { ...phase.feed, latency_ms: undefined },
      conflation: conflation(gateway),
      error: page ? page.error : 'no result',
    });
  }

  // Fault injection: a dropped delta every 100.
  {
    const gateway = startGateway('--rate', '5000', '--drop-every', '100');
    gateways.push(gateway);
    await gateway.ready;
    await chrome.send('Page.navigate', { url: quotes(`feed=${feedUrl}`) });
    await waitFor(feedStats, isLive, 30000);
    const before = await feedStats(true);
    const from = gateway.stats.at(-1);
    await sleep(10000);
    const after = await feedStats();
    const to = gateway.stats.at(-1);
    await stopGateway(gateway);
    result.dropEvery100 = {
      seconds: 10,
      dropped: to.dropped - (from?.dropped ?? 0),
      resyncs: after.resyncs - before.resyncs,
      snapshots: after.snapshots - before.snapshots,
      state: after.state,
      latencyMs: after.latency_ms,
    };
  }

  // A slow client: the gateway sleeps 200 ms after each delta.
  {
    const gateway = startGateway('--rate', '5000', '--stall-ms', '200');
    gateways.push(gateway);
    await gateway.ready;
    await chrome.send('Page.navigate', { url: quotes(`feed=${feedUrl}`) });
    await waitFor(feedStats, isLive, 30000);
    await feedStats(true);
    await sleep(5000);
    const after = await feedStats();
    await chrome.send('Page.navigate', { url: 'about:blank' });
    await sleep(1500);
    await stopGateway(gateway);
    result.stall200ms = { conflation: conflation(gateway), latencyMs: after.latency_ms };
  }

  // Kill the gateway, restart it after a second, wait for live.
  {
    let gateway = startGateway('--rate', '300');
    gateways.push(gateway);
    await gateway.ready;
    await chrome.send('Page.navigate', { url: quotes(`feed=${feedUrl}`) });
    await waitFor(feedStats, isLive, 30000);
    result.restarts = [];
    for (let round = 0; round < 3; round++) {
      await sleep(2000);
      const killed = performance.now();
      await stopGateway(gateway, 'SIGKILL');
      await waitFor(feedStats, (stats) => !isLive(stats), 10000);
      const offline = performance.now();
      await sleep(1000);
      gateway = startGateway('--rate', '300');
      gateways.push(gateway);
      const listening = await gateway.ready;
      await waitFor(feedStats, isLive, 30000);
      const live = performance.now();
      result.restarts.push({
        offlineAfterKillMs: Math.round(offline - killed),
        liveAfterRestartMs: Math.round(live - listening),
        liveAfterKillMs: Math.round(live - killed),
      });
    }
    result.reconnects = (await feedStats()).reconnects;
    await stopGateway(gateway);
  }
} finally {
  for (const gateway of gateways) {
    if (gateway.child.exitCode === null && gateway.child.signalCode === null) {
      await stopGateway(gateway, 'SIGKILL');
    }
  }
  await chrome.close();
  pages.close();
}
console.log(JSON.stringify(result, null, 2));
