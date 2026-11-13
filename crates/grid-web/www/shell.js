// What every example's page shares (`<app>/index.html`): `start` loads the
// example's own module (`pkg/<app>/<app>.js`, built from `apps/<app>/`) and
// runs it in the page's canvas with the page's URL parameters, which the
// example reads (`?theme=` its start theme, `?rate=`, `?seed=` and its own;
// `?backend=webgl` skips WebGPU). With `?bench` the page measures it: load
// and first-frame times, then GPUI's frames at each rate of `?rates=`
// (the page's default otherwise), `?seconds=` each. The result goes to the
// console as one `GRID_BENCH {json}` line and into `#bench`.
//
// The browser has no installed fonts and no module embeds any: the page
// fetches the start theme's faces from `fonts/` before the example starts,
// and the other themes' faces after its first rows, or a few seconds
// without them (`fontsReady`). With the start theme's comes IBM Plex Sans,
// the face of GPUI's default family
// (`.SystemUIFont`): text that no theme styles, such as the view of a drag,
// which GPUI draws outside the window's root, lays out in it; without it,
// the module panics there and stops.
//
// GPUI's web platform draws each frame in a `requestAnimationFrame`
// callback, so wrapping that function times every frame's work on the main
// thread (layout, prepaint, paint and the GPU submission). The page's own
// sampling loop uses the unwrapped function.
//
// The module's hooks (`stats`, `grid_state`, `set_rate` and the example's
// own) are on `globalThis.grid` for scripts, and `globalThis.fontsReady`
// resolves once every theme's faces are in. Every path is relative to this
// file, so the pages work under any prefix.
//
// `?embed` makes the page a guest in another site's iframe (`embed/`): no
// switcher, no keyboard taken at the start, the wheel the host's at the
// example's ends. A theme remix (`#theme=v1.…`, `?theme={…}` or `start`'s
// `theme`) is laid over the example's theme once it is on screen.

const started = performance.now();
const params = new URLSearchParams(location.search);
const bench = params.has('bench');
const embedded = params.has('embed');
const seconds = Number(params.get('seconds') ?? 5);

const nativeFrame = window.requestAnimationFrame.bind(window);
const frames = []; // [start ms, duration ms] of each GPUI frame
window.requestAnimationFrame = (callback) =>
  nativeFrame((time) => {
    const start = performance.now();
    callback(time);
    frames.push([start, performance.now() - start]);
  });

// GPUI's web platform copies through `navigator.clipboard.writeText` without
// checking that the browser offers it, and a browser offers it only in a
// secure context (https, localhost, 127.0.0.1): served over plain http on
// another address, a copy threw inside the module and stopped it. There the
// page puts a stand-in in its place before the module starts. It copies text
// the older way, by `document.execCommand('copy')` within the key press or
// the click that asked for it, filling the copy event's data, so the focus
// stays where it is. Reading the clipboard (a paste) stays unavailable and
// fails as it does for a denied permission.
if (!navigator.clipboard) {
  const refused = (what) => {
    const refusal = Promise.reject(
      new DOMException(`${what} needs a secure context (https or localhost)`, 'NotAllowedError'),
    );
    refusal.catch(() => {}); // GPUI drops the promise of a copy unread.
    return refusal;
  };
  const clipboard = {
    writeText(text) {
      let copied = false;
      const fill = (event) => {
        event.clipboardData.setData('text/plain', String(text));
        event.preventDefault();
        copied = true;
      };
      document.addEventListener('copy', fill, true);
      try {
        document.execCommand('copy');
      } catch {
        // No copy command in this browser: refused below.
      } finally {
        document.removeEventListener('copy', fill, true);
      }
      return copied ? Promise.resolve() : refused('Copying');
    },
    write: () => refused('Copying'),
    readText: () => refused('Pasting'),
    read: () => refused('Pasting'),
  };
  try {
    Object.defineProperty(navigator, 'clipboard', { value: clipboard, configurable: true });
  } catch (error) {
    console.warn(`GRID_CLIPBOARD no stand-in for the clipboard: ${error}`);
  }
}

const longTasks = []; // [start ms, duration ms]
try {
  new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) longTasks.push([entry.startTime, entry.duration]);
  }).observe({ type: 'longtask', buffered: true });
} catch {
  // No long-task timing in this browser.
}

const status = document.getElementById('status');

// What the page shows until the example's first rows: a spinner, what it
// waits for and, while the module downloads, how much of it has arrived.
// The spinner turns in CSS (a transform), so it keeps turning while the
// main thread compiles the module or builds the first frame. Without
// JavaScript the page's own text ("Loading…") stays.
const loader = (() => {
  if (!status) return { step() {}, progress() {} };
  const part = (className) => {
    const element = document.createElement('span');
    element.className = className;
    return element;
  };
  const [spinner, label, percent, bar, fill] = ['spinner', 'label', 'percent', 'bar', 'fill'].map(part);
  // A screen reader hears each step, not each percent.
  for (const element of [spinner, percent, bar]) element.setAttribute('aria-hidden', 'true');
  bar.append(fill);
  bar.hidden = true;
  status.setAttribute('role', 'status');
  status.replaceChildren(spinner, label, percent, bar);
  return {
    step(text) {
      label.textContent = text;
      percent.textContent = '';
      bar.hidden = true;
    },
    progress(text, fraction) {
      if (label.textContent !== text) label.textContent = text;
      percent.textContent = `${Math.floor(fraction * 100)}%`;
      bar.hidden = false;
      fill.style.transform = `scaleX(${fraction})`;
    },
  };
})();

const sleep =(ms) => new Promise((resolve) => setTimeout(resolve, ms));
// Resolves with the time the callback ran, not the frame's timestamp:
// headless Chrome keeps the timestamps evenly spaced while the main thread
// is busy.
const nextFrame = () => new Promise((resolve) => nativeFrame(() => resolve(performance.now())));
const round = (value) => Math.round(value * 100) / 100;
// A path under `www/`, wherever the pages are served.
const site = (path) => new URL(path, import.meta.url).href;

function percentile(sorted, p) {
  if (sorted.length === 0) return null;
  return sorted[Math.min(sorted.length - 1, Math.floor((sorted.length * p) / 100))];
}

function summary(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const sum = sorted.reduce((total, value) => total + value, 0);
  return {
    n: sorted.length,
    mean: round(sorted.length ? sum / sorted.length : 0),
    p50: round(percentile(sorted, 50) ?? 0),
    p95: round(percentile(sorted, 95) ?? 0),
    p99: round(percentile(sorted, 99) ?? 0),
    max: round(sorted[sorted.length - 1] ?? 0),
  };
}

// Fetches `files` (paths under `fonts/`) and hands each face to the module;
// returns the bytes fetched.
async function addFonts(wasm, files) {
  const faces = await Promise.all(
    files.map(async (file) => {
      const response = await fetch(site(`fonts/${file}`));
      if (!response.ok) {
        throw new Error(
          `fonts/${file}: HTTP ${response.status}. Run script/fetch-example-fonts, then make build.`,
        );
      }
      return new Uint8Array(await response.arrayBuffer());
    }),
  );
  for (const face of faces) wasm.add_font(face);
  return faces.reduce((total, face) => total + face.length, 0);
}

// The example's module, its download shown by the loader: the response is
// read here and handed on as a stream, so the browser still compiles the
// module while it arrives. The progress needs the decompressed size, which
// the build writes into `pkg/<app>/module.json` (a compressed response's
// length is not it); without it the spinner turns alone.
async function fetchModule(app) {
  const [response, total] = await Promise.all([
    fetch(site(`pkg/${app}/${app}_bg.wasm`)),
    fetch(site(`pkg/${app}/module.json`))
      .then((response) => (response.ok ? response.json() : null))
      .then((module) => module?.bytes ?? 0)
      .catch(() => 0),
  ]);
  if (!response.ok || !response.body || !total) return response;
  const text = 'Downloading the example';
  const reader = response.body.getReader();
  let received = 0;
  loader.progress(text, 0);
  const body = new ReadableStream({
    async pull(controller) {
      const { done, value } = await reader.read();
      if (done) {
        controller.close();
        return;
      }
      received += value.byteLength;
      loader.progress(text, Math.min(1, received / total));
      controller.enqueue(value);
    },
    cancel: (reason) => reader.cancel(reason),
  });
  return new Response(body, { headers: { 'Content-Type': 'application/wasm' } });
}

// The examples (`pkg/apps.json`, which the build writes), each a link to
// its page, `app` marked as the one shown; then the licences
// (`licenses/`, which the build writes) and the page's `notice`.
async function showApps(app, notice) {
  const nav = document.getElementById('apps');
  const response = await fetch(site('pkg/apps.json'));
  if (!nav || !response.ok) return;
  for (const name of await response.json()) {
    const link = document.createElement('a');
    link.href = site(`${name}/`);
    link.textContent = name[0].toUpperCase() + name.slice(1);
    if (name === app) link.setAttribute('aria-current', 'page');
    nav.append(link);
  }
  const licenses = document.createElement('a');
  licenses.href = site('licenses/');
  licenses.className = 'licenses';
  licenses.textContent = 'Licenses';
  nav.append(licenses);
  if (notice) {
    const label = document.createElement('span');
    label.className = 'notice';
    label.textContent = notice;
    nav.append(label);
  }
}

// Runs the example `page.app`. What a page adds, all optional:
//
// - `notice`: a line at the right of the switcher (the markets' "Simulated
//   prices, not market data");
// - `beforeRun(wasm, params)`: hands the module what it needs before it
//   starts (orbit: its data); returns the bytes fetched;
// - `loading`: what the loader says meanwhile (`Loading data`);
// - `rates`: the bench's default rates (`300,5000`);
// - `setRate(wasm, rate)`: sets a bench phase's rate (`wasm.set_rate`);
// - `restore(wasm, startRate)`: after the bench (`setRate` of the start);
// - `beforePhase(wasm)` and `phase(wasm, before, after)`: the example's own
//   counters of a phase, from `stats` before and after it;
// - `theme`: a theme remix shown from the start (`embed/theme.js`).
export function start(page) {
  if (!embedded) showApps(page.app, page.notice).catch(() => {});
  main(page).catch((error) => {
    status.className = 'error';
    status.textContent = `The grid failed to start: ${error}`;
    console.error('GRID_ERROR ' + error);
  });
}

// Lays `theme`, or the remix in the page's address, over the example's
// theme, and again whenever `#theme=` changes.
async function showTheme(wasm, theme) {
  const { applyTheme, themeFromLocation } = await import('./embed/theme.js');
  const show = async (remix) => remix && applyTheme(wasm, remix, (file) => site(`fonts/${file}`));
  addEventListener('hashchange', () =>
    themeFromLocation(location)
      .then((remix) => show(remix ?? '{}'))
      .catch((error) => console.error('GRID_ERROR ' + error)),
  );
  await show(theme ?? (await themeFromLocation(location)));
}

async function main(page) {
  const { app } = page;
  const setRate = page.setRate ?? ((wasm, rate) => wasm.set_rate(rate));
  const rates = (params.get('rates') ?? page.rates ?? '300,5000').split(',').map(Number);
  loader.step('Loading the example');
  const wasm = await import(site(`pkg/${app}/${app}.js`));
  await wasm.default({ module_or_path: fetchModule(app) });
  globalThis.grid = wasm;
  const instantiated = performance.now();
  loader.step('Loading fonts');
  const [firstFonts, laterFonts] = JSON.parse(wasm.font_files(location.search));
  const fontBytes = await addFonts(wasm, firstFonts);
  const fontsAdded = performance.now();
  if (page.beforeRun) loader.step(page.loading ?? 'Loading data');
  const dataBytes = page.beforeRun ? await page.beforeRun(wasm, params) : 0;
  const dataAdded = performance.now();
  loader.step('Starting the example');
  const embed = embedded ? await import('./embed/page.js') : null;
  embed?.beforeRun({ notice: page.notice, licenses: site('licenses/') });
  wasm.run(location.search);
  const runReturned = performance.now();

  // The first frame GPUI draws: the example is on screen, and the loader
  // goes. Its rows may come later, or not at all while its grids are out
  // of view (orbit on a narrow screen), so only the bench waits for them.
  let firstFrame = null;
  while (firstFrame === null) {
    await nextFrame();
    if (frames.length > 0) firstFrame = frames[0][0] + frames[0][1];
    else if (performance.now() - started > 30000) throw new Error('no frame after 30 s');
  }
  status.remove();
  embed?.afterRun(wasm, (file) => site(`fonts/${file}`));
  showTheme(wasm, page.theme).catch((error) => console.error('GRID_ERROR ' + error));
  const trace = []; // [ms, snapshots, rebuilds, frames, visible cells] on each change
  // When the first rows (or the example's error) showed; `null` if none
  // did within 30 s of the page's start.
  const rowsShown = (async () => {
    while (performance.now() - started < 30000) {
      const stats = JSON.parse(wasm.stats());
      if (stats) {
        const sample = [stats.snapshots, stats.rebuilds, stats.frames, stats.visible_cells];
        const last = trace[trace.length - 1];
        if (!last || sample.some((value, ix) => value !== last[ix + 1])) {
          trace.push([round(performance.now()), ...sample]);
        }
        if (stats.visible_cells > 0 || stats.error) return performance.now();
      }
      await nextFrame();
    }
    return null;
  })();
  // The other themes' faces, once the rows are on screen, or after a few
  // seconds without them.
  let laterBytes = 0;
  globalThis.fontsReady = Promise.race([rowsShown, sleep(3000)]).then(async () => {
    const laterStarted = performance.now();
    laterBytes = await addFonts(wasm, laterFonts);
    if (laterFonts.length > 0) wasm.fonts_loaded();
    return performance.now() - laterStarted;
  });
  globalThis.fontsReady.catch((error) => console.error('GRID_ERROR ' + error));
  if (!bench) return;
  const firstRows = await rowsShown;
  if (firstRows === null) throw new Error('no rows after 30 s');
  const startupStats = JSON.parse(wasm.stats());
  if (startupStats.error) throw new Error(startupStats.error);
  const laterFontsMs = await globalThis.fontsReady;

  const navigation = performance.getEntriesByType('navigation')[0];
  const module = performance
    .getEntriesByType('resource')
    .find((entry) => entry.name.endsWith('.wasm'));
  const result = {
    userAgent: navigator.userAgent,
    webgpu: 'gpu' in navigator,
    devicePixelRatio: devicePixelRatio,
    viewport: [innerWidth, innerHeight],
    app,
    load: {
      navigationToScript: round(started),
      wasmFetchMs: module ? round(module.responseEnd - module.startTime) : null,
      wasmBytes: module ? module.encodedBodySize || module.transferSize : null,
      instantiatedMs: round(instantiated),
      fontBytes,
      fontsMs: round(fontsAdded - instantiated),
      ...(page.beforeRun && { dataBytes, dataMs: round(dataAdded - fontsAdded) }),
      runMs: round(runReturned - dataAdded),
      firstFrameMs: firstFrame && round(firstFrame),
      firstRowsMs: round(firstRows),
      domContentLoadedMs: navigation ? round(navigation.domContentLoadedEventEnd) : null,
      laterFontBytes: laterBytes,
      laterFontsMs: round(laterFontsMs),
    },
    startup: JSON.parse(wasm.stats()),
    startupTrace: trace,
    gpuiFramesToRows: frames.filter(([start]) => start <= firstRows).map(([start, duration]) => [round(start), round(duration)]),
    phases: [],
  };

  const initialRate = result.startup.rate;
  await sleep(2000);
  for (const phaseRate of rates) {
    setRate(wasm, phaseRate);
    await sleep(1000); // settle at the new rate
    page.beforePhase?.(wasm);
    const before = JSON.parse(wasm.stats());
    const from = performance.now();
    // The page's own frame callbacks: a gap over 20 ms is a dropped frame.
    const display = [];
    let sampling = true;
    (async () => {
      while (sampling) display.push(await nextFrame());
    })();
    await sleep(seconds * 1000);
    sampling = false;
    const to = performance.now();
    const after = JSON.parse(wasm.stats());
    const measured = frames.filter(([start]) => start >= from && start < to);
    const starts = measured.map(([start]) => start);
    const intervals = starts.slice(1).map((start, ix) => start - starts[ix]);
    const displayIntervals = display.slice(1).map((time, ix) => time - display[ix]);
    const tasks = longTasks.filter(([start]) => start >= from && start < to);
    result.phases.push({
      rate: phaseRate,
      seconds: round((to - from) / 1000),
      measuredRate: Math.round((after.updates - before.updates) / ((to - from) / 1000)),
      gpuiFrames: measured.length,
      gpuiFps: round(measured.length / ((to - from) / 1000)),
      frameWorkMs: summary(measured.map(([, duration]) => duration)),
      frameIntervalMs: summary(intervals),
      displayIntervalMs: summary(displayIntervals),
      displayFramesOver20ms: displayIntervals.filter((interval) => interval > 20).length,
      longTasks: tasks.length,
      longTaskMaxMs: round(Math.max(0, ...tasks.map(([, duration]) => duration))),
      gridFrames: after.frames - before.frames,
      rebuilds: after.rebuilds - before.rebuilds,
      lastRebuildMs: after.last_rebuild_ms,
      maxRebuildMsSinceStart: after.max_rebuild_ms,
      lastPrepaintMs: after.last_prepaint_ms,
      visibleCells: after.visible_cells,
      ...page.phase?.(wasm, before, after),
    });
  }
  (page.restore ?? setRate)(wasm, initialRate);
  const text = JSON.stringify(result);
  document.getElementById('bench').textContent = text;
  console.log('GRID_BENCH ' + text);
}
