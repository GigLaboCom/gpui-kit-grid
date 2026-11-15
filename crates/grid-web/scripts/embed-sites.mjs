// The two-line `<live-grid>` snippet on each snippet platform and blog
// host, in headless Chrome: what to run on every gpui-kit or GPUI upgrade,
// next to `embed-check.mjs`. Each case of `embed-platforms.mjs` is the
// platform's chain of documents, from its top-level page through its
// editor and preview frames (their `sandbox` and `allow`, `srcdoc` or a
// document of its own, the CSP and COOP/COEP headers of the documents that
// carry them) down to the snippet, exactly as a reader pastes it with its
// `src` on the "CDN" (or, for a blog that frames the demo itself, down to
// the example's `?embed` page).
//
// The platforms' hosts are served here under `localhost`: `cdpn.io` as
// `cdpn-io.localhost`, `null.jsbin.com` as `null.jsbin-com.localhost`.
// Chrome keeps a site per name below `localhost` and treats them all as
// secure, so hosts that share a site on the platform share one here, and
// the others are cross-site, in frames of their own processes. The CDN is
// this checkout's www/ on 127.0.0.1, another site again, or SOURCE.
//
// On a fresh page per case: the element fires `ready` (or, as the case
// expects, its frame is refused or it fires `error`; an example's page
// says `live-grid:ready` to its parent), its canvas inside it (inline) or
// its frame (frame mode), with rows, drawing; which backend
// drew (WebGPU or WebGL 2); a click on a row and Ctrl+C copies, or shows
// "Copy needs clipboard permission on this site", as the case expects; no
// panic or exception on any console; and how long from the top-level
// page's start to `ready`.
//
// Prints one line per case, `<id>: ok (webgpu, 0.6 s, copy=notice)` or
// `<id>: FAILED (why)`, and exits 1 on any failure, with a screenshot of
// each failed page in $OUT (`sites-<id>.png`). Env: SITES (case ids, comma
// separated; default all), SOURCE (the element's address instead of this
// checkout's `embed/live-grid.js`, such as the published
// https://gkg.giglabo.com/embed/v1/live-grid.js), DEMO (the examples'
// pages for it: next to `embed/`, or the site's `demo/` for a
// `/embed/v1/` source), ORIGIN (a served www/
// instead of this checkout's), APPS (default quotes; more as in
// `embed-check.mjs`, never a live source; the copy is checked on quotes),
// OUT (default: the current directory), REPORT (a file for the results as
// JSON), CHROME, CHROME_FLAGS.

import { mkdirSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { launch, sleep } from './chrome.mjs';
import { PLATFORMS } from './embed-platforms.mjs';
import {
  COPY_NOTICE,
  EXAMPLES,
  FATAL,
  NO_CELESTRAK,
  answerCelestrak,
  appsFromEnv,
  celestrakElements,
} from './embed-apps.mjs';
import { site } from './serve.mjs';

const OUT = process.env.OUT ?? process.cwd();
const APPS = appsFromEnv();
const START_MS = 40000;
// How long a case that expects the element's frame refused waits for it.
const BLOCKED_MS = 10000;
// A row of the quotes grid, from the element's top left.
const ROW = [300, 140];
const CTRL = 2;

const wanted = (process.env.SITES ?? '').split(',').filter(Boolean);
for (const id of wanted) {
  if (!PLATFORMS.some((platform) => platform.id === id)) {
    console.error(`SITES=${id}: not one of ${PLATFORMS.map(({ id }) => id).join(', ')}`);
    process.exit(1);
  }
}
const platforms = wanted.length
  ? PLATFORMS.filter((platform) => wanted.includes(platform.id))
  : PLATFORMS;
mkdirSync(OUT, { recursive: true });
const elements = APPS.includes('orbit') ? celestrakElements() : null;

// A platform's host as served here: its registrable part (the last two
// labels) joined into one label below `localhost`, which keeps the site.
const localHost = (host) => {
  const labels = host.split('.');
  const site = labels.splice(-2).join('-');
  return [...labels, site, 'localhost'].join('.');
};

const escape = (text) =>
  text.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

// The snippet a reader pastes: the site's two lines, `src` on the source,
// with the example's `query` (and the case's `mode`) on the element.
function snippet(source, app, mode) {
  const [name, query] = EXAMPLES[app];
  const attributes = [
    `app="${name}"`,
    app === 'quotes' ? '' : `query="${escape(query)}"`,
    mode === 'frame' ? 'mode="frame"' : '',
    'autoload',
  ];
  return [
    `<script type="module" src="${escape(source)}"></script>`,
    `<live-grid ${attributes.filter(Boolean).join(' ')}></live-grid>`,
  ].join('\n');
}

// Records the element's events and the page's copies in every document,
// before its own scripts: `window.__embedSites`.
const PROBE = `(() => {
  if (window.__embedSites) return;
  const probe = { events: [], copies: [] };
  Object.defineProperty(window, '__embedSites', { value: probe });
  for (const type of ['ready', 'error']) {
    addEventListener(type, (event) => {
      if (event.target?.localName !== 'live-grid') return;
      probe.events.push({ type, detail: event.detail, at: performance.timeOrigin + performance.now() });
    }, true);
  }
  addEventListener('message', (event) => {
    if (event.data?.type !== 'live-grid:ready' || document.querySelector('live-grid')) return;
    probe.events.push({ type: 'ready', detail: { mode: 'page' }, at: performance.timeOrigin + performance.now() });
  });
  const clipboard = navigator.clipboard;
  if (clipboard?.writeText) {
    const write = clipboard.writeText.bind(clipboard);
    clipboard.writeText = (text) => {
      const written = write(text);
      written.then(
        () => probe.copies.push(['ok', text]),
        (error) => probe.copies.push(['refused', String(error)]),
      );
      return written;
    };
  }
})()`;

const AUTO_ATTACH = { autoAttach: true, waitForDebuggerOnStart: true, flatten: true };

// The documents of each case, served on every name below `localhost`.
function chainServer(source, demo) {
  const cdn = new URL(source);
  const port = () => server.address().port;
  // `https:` in a policy stands for any https site: the platforms' local
  // hosts join it, and the CDN when it is local plain http.
  const policy = (value) => {
    const local = [`http://*.localhost:${port()}`];
    if (cdn.protocol === 'http:') local.push(cdn.origin);
    return value.replace(/(^|\s)https:(?=\s|;|$)/g, `$1https: ${local.join(' ')}`);
  };
  // A frame's address: the example's `?embed` page from the CDN, or the
  // platform's document here.
  const address = (platform, ix, app) => {
    if (platform.frames[ix].example) {
      const [name, query] = EXAMPLES[app];
      return new URL(`${name}/?embed&${query}`, demo).href;
    }
    return `http://${localHost(platform.frames[ix].host)}:${port()}/${platform.id}/${ix}/?app=${app}`;
  };

  function documentHtml(platform, ix, app) {
    const title = `${platform.name} (${platform.frames[ix].host})`;
    const head = `<meta charset="utf-8" />\n<title>${escape(title)}</title>\n<link rel="icon" href="data:," />`;
    if (ix === platform.frames.length - 1) {
      return `<!doctype html>\n<html lang="en">\n<head>\n${head}\n</head>\n<body>\n${snippet(source, app, platform.mode)}\n</body>\n</html>\n`;
    }
    const frame = platform.frames[ix + 1];
    const attributes = { ...frame.attributes };
    if (frame.sandbox !== undefined) attributes.sandbox = frame.sandbox;
    if (frame.allow !== undefined) attributes.allow = frame.allow;
    if (frame.credentialless) attributes.credentialless = '';
    if (frame.example) attributes['data-example'] = '';
    if (frame.srcdoc) attributes.srcdoc = documentHtml(platform, ix + 1, app);
    else attributes.src = address(platform, ix + 1, app);
    const iframe = Object.entries(attributes)
      .map(([name, value]) => (value === '' ? name : `${name}="${escape(value)}"`))
      .join(' ');
    // The frame over the whole viewport: the snippet's page has the top
    // page's coordinates.
    const style = 'html, body { margin: 0; height: 100%; } iframe { position: fixed; inset: 0; width: 100%; height: 100%; border: 0; }';
    return `<!doctype html>\n<html lang="en">\n<head>\n${head}\n<style>${style}</style>\n</head>\n<body>\n<iframe ${iframe}></iframe>\n</body>\n</html>\n`;
  }

  const server = createServer((request, response) => {
    const url = new URL(request.url, 'http://localhost');
    const [, id, level] = url.pathname.split('/');
    const platform = PLATFORMS.find((platform) => platform.id === id);
    const ix = Number(level);
    const app = url.searchParams.get('app') ?? 'quotes';
    const frame = platform?.frames[ix];
    if (!frame || frame.srcdoc || frame.example || !(app in EXAMPLES)) {
      response.writeHead(404).end();
      return;
    }
    const headers = { ...frame.headers };
    for (const name of Object.keys(headers)) {
      if (/^content-security-policy$/i.test(name)) headers[name] = policy(headers[name]);
    }
    response
      .writeHead(200, {
        'Content-Type': 'text/html; charset=utf-8',
        'Cache-Control': 'no-store',
        ...headers,
      })
      .end(documentHtml(platform, ix, app));
  });
  return new Promise((resolve) =>
    server.listen(0, '::', () => resolve({ server, top: (platform, app) => address(platform, 0, app) })),
  );
}

// One case in a fresh Chrome. Resolves with { id, ok, line, ... }.
async function runCase(platform, app, top) {
  const id = app === 'quotes' ? platform.id : `${platform.id}/${app}`;
  const mode = platform.frames.at(-1).example ? 'page' : (platform.mode ?? 'inline');
  const expect = { start: 'ready', ...platform.expect };
  const chrome = await launch([...(platform.flags ?? []), NO_CELESTRAK]);
  const { send, sendTo, evaluate } = chrome;
  const log = [];
  const contexts = new Map(); // `${session} ${id}` → { session, id }
  const asked = [];

  chrome.on(({ method, params, sessionId }) => {
    if (method === 'Target.attachedToTarget') {
      const session = params.sessionId;
      const steps =
        params.targetInfo.type === 'iframe'
          ? [
              ['Page.enable'],
              ['Page.addScriptToEvaluateOnNewDocument', { source: PROBE }],
              ['Runtime.enable'],
              ['Log.enable'],
              ['Emulation.setFocusEmulationEnabled', { enabled: true }],
              ['Target.setAutoAttach', AUTO_ATTACH],
            ]
          : [];
      (async () => {
        for (const [step, stepParams] of steps) await sendTo(session, step, stepParams).catch(() => {});
        await sendTo(session, 'Runtime.runIfWaitingForDebugger').catch(() => {});
      })();
    } else if (method === 'Runtime.executionContextCreated') {
      if (params.context.auxData?.isDefault) {
        contexts.set(`${sessionId} ${params.context.id}`, { session: sessionId, id: params.context.id });
      }
    } else if (method === 'Runtime.executionContextDestroyed') {
      contexts.delete(`${sessionId} ${params.executionContextId}`);
    } else if (method === 'Runtime.executionContextsCleared') {
      for (const [key, context] of contexts) if (context.session === sessionId) contexts.delete(key);
    } else if (method === 'Runtime.consoleAPICalled') {
      log.push(params.args.map((arg) => arg.value ?? arg.description ?? '').join(' '));
    } else if (method === 'Runtime.exceptionThrown') {
      log.push(params.exceptionDetails.exception?.description ?? params.exceptionDetails.text);
    } else if (method === 'Log.entryAdded' && params.entry.level === 'error') {
      log.push(params.entry.text);
    }
  });

  // The value of `expression` in `context`, `undefined` if it throws.
  const evaluateIn = async (context, expression) => {
    const answer = await sendTo(context.session, 'Runtime.evaluate', {
      expression,
      contextId: context.id,
      returnByValue: true,
      awaitPromise: true,
    }).catch(() => undefined);
    return answer && !answer.exceptionDetails ? answer.result.value : undefined;
  };
  // The first document where `expression` is truthy.
  const findContext = async (expression) => {
    for (const context of [...contexts.values()]) {
      if (await evaluateIn(context, expression)) return context;
    }
    return undefined;
  };
  const until = async (poll, ms = 3000) => {
    for (const started = Date.now(); ; await sleep(50)) {
      const value = await poll();
      if (value) return value;
      if (Date.now() - started > ms) return undefined;
    }
  };
  const settle = () =>
    evaluate('new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))');
  const firstError = () => {
    const line = log.find((line) => /error|refused|blocked|failed/i.test(line));
    return line ? `; console: ${line.split('\n')[0].slice(0, 160)}` : '';
  };

  const result = { id, platform: platform.id, app, mode, basis: platform.basis };
  const fail = (why) => Object.assign(result, { ok: false, why });
  const ELEMENT = `document.querySelector('live-grid')`;
  const EXAMPLE_PAGE = `typeof globalThis.grid?.stats === 'function' && !${ELEMENT}`;
  // The document framing the example's page (`example`), in place of the
  // snippet's: its frame plays the element.
  const HOLDER = `!!document.querySelector('iframe[data-example]')`;
  const BOX = mode === 'page' ? `document.querySelector('iframe[data-example]')` : ELEMENT;
  try {
    await send('Runtime.enable');
    await send('Page.enable');
    await send('Log.enable');
    await send('Page.addScriptToEvaluateOnNewDocument', { source: PROBE });
    await send('Target.setAutoAttach', AUTO_ATTACH);
    // A headless page is not focused unless asked: as a reader's tab.
    await send('Emulation.setFocusEmulationEnabled', { enabled: true });
    if (app === 'orbit') await answerCelestrak(chrome, elements, asked);
    await send('Page.navigate', { url: top });

    // The snippet's document, and the element's first event.
    const button = '#celestrak [data-action=load]';
    let leaf;
    let frame;
    let clicked = false;
    let event;
    const waitMs = expect.start === 'blocked' ? BLOCKED_MS : START_MS;
    for (const started = Date.now(); Date.now() - started < waitMs; await sleep(100)) {
      leaf ??= await findContext(mode === 'page' ? HOLDER : `!!${ELEMENT}`);
      if (!leaf) continue;
      event = (await evaluateIn(leaf, 'globalThis.__embedSites?.events[0]')) ?? undefined;
      if (event) break;
      // Orbit asks CelesTrak after a click in its panel.
      if (app === 'orbit' && !clicked) {
        const press = (root) => `(() => { const button = ${root}.querySelector('${button}'); button?.click(); return !!button; })()`;
        if (mode === 'inline') {
          clicked = await evaluateIn(leaf, press(`${ELEMENT}.shadowRoot`));
        } else {
          frame ??= await findContext(`!!document.querySelector('${button}')`);
          if (frame) clicked = await evaluateIn(frame, press('document'));
        }
      }
    }
    const topOrigin = await evaluate('performance.timeOrigin');
    const seconds = event ? (event.at - topOrigin) / 1000 : undefined;
    result.seconds = seconds;

    if (!leaf) return fail(`the snippet's page never loaded${firstError()}`);
    if (expect.start === 'blocked') {
      if (event?.type === 'ready') return fail(`ready in ${seconds.toFixed(1)} s, expected the frame refused`);
      const page = await findContext(EXAMPLE_PAGE);
      if (page) return fail('the example runs in the element’s frame');
      const hasFrame = await evaluateIn(leaf, `!!${ELEMENT}.shadowRoot.querySelector('.stage > iframe')`);
      if (!hasFrame) return fail('the element made no frame');
      result.outcome = 'blocked';
      return Object.assign(result, {
        ok: true,
        line: `ok (blocked as expected: no ready in ${BLOCKED_MS / 1000} s, no example in the element's frame)`,
      });
    }
    if (expect.start === 'error') {
      if (event?.type !== 'error') return fail(event ? `${event.type}, expected an error event` : 'no event');
      const message = event.detail?.message ?? '';
      if (!message.includes(expect.error)) return fail(`error event: ${message}`);
      result.outcome = 'error';
      return Object.assign(result, {
        ok: true,
        line: `ok (error as expected: "${message}", ${seconds.toFixed(1)} s)`,
      });
    }
    if (!event) return fail(`no ready event in ${START_MS / 1000} s${firstError()}`);
    if (event.type === 'error') return fail(`error event: ${event.detail?.message}`);
    if (event.detail?.mode !== mode) return fail(`ready in mode ${event.detail?.mode}`);
    result.outcome = 'ready';

    // Where it draws: the canvas (or the frame) inside the element.
    const drawing = mode === 'inline' ? 'canvas' : 'iframe';
    const placement = mode === 'page' ? '' : await evaluateIn(leaf, `(() => {
      const grid = ${ELEMENT};
      const child = grid.shadowRoot.querySelector('.stage > ${drawing}');
      if (!child) return 'no ${drawing} in the element';
      if (document.querySelector('body > canvas')) return 'a canvas left in the body';
      const [inner, outer] = [child.getBoundingClientRect(), grid.getBoundingClientRect()];
      const inside =
        inner.left >= outer.left - 0.5 && inner.right <= outer.right + 0.5 &&
        inner.top >= outer.top - 0.5 && inner.bottom <= outer.bottom + 0.5;
      if (!inside || inner.width < 100 || inner.height < 100) return 'the ${drawing} is not inside the element';
      return '';
    })()`);
    if (placement !== '') return fail(placement ?? 'the element went away');

    // The document the example runs in, its hooks and its canvas.
    const [example, hooks, canvas] =
      mode === 'inline'
        ? [leaf, `${ELEMENT}.grid`, `${ELEMENT}.shadowRoot.querySelector('.stage > canvas')`]
        : [await until(() => findContext(EXAMPLE_PAGE), START_MS), 'globalThis.grid', `document.querySelector('body > canvas')`];
    if (!example) return fail('no example in the element’s frame');
    const stats = async () => JSON.parse((await evaluateIn(example, `${hooks}?.stats()`)) ?? 'null');
    if (!(await until(async () => (await stats())?.visible_cells > 0, START_MS))) return fail('no rows');
    const before = (await stats()).frames;
    if (!(await until(async () => (await stats())?.frames > before))) return fail('no new frame');
    result.backend = await evaluateIn(
      example,
      `(() => { const canvas = ${canvas}; return canvas.getContext('webgpu') ? 'webgpu' : canvas.getContext('webgl2') ? 'webgl2' : 'unknown'; })()`,
    );
    if (expect.backend && result.backend !== expect.backend) {
      return fail(`drawn with ${result.backend}, expected ${expect.backend}`);
    }

    // Copy: a click on a row, Ctrl+C, and what the reader gets.
    if (app === 'quotes' && expect.copy) {
      const box = await evaluateIn(leaf, `(() => { const box = ${BOX}.getBoundingClientRect(); return [box.x, box.y]; })()`);
      const [x, y] = [box[0] + ROW[0], box[1] + ROW[1]];
      for (const [type, extra] of [
        ['mouseMoved', {}],
        ['mousePressed', { button: 'left', buttons: 1, clickCount: 1 }],
        ['mouseReleased', { button: 'left', buttons: 0, clickCount: 1 }],
      ]) {
        await send('Input.dispatchMouseEvent', { type, x, y, ...extra });
      }
      await settle();
      const selected = await until(async () => {
        const state = JSON.parse((await evaluateIn(example, `${hooks}?.grid_state()`)) ?? '{}');
        return state.selected?.length ? state.selected : null;
      });
      if (!selected) return fail('a click on a row selected nothing');
      for (const type of ['rawKeyDown', 'keyUp']) {
        await send('Input.dispatchKeyEvent', {
          type,
          key: 'c',
          code: 'KeyC',
          windowsVirtualKeyCode: 67,
          nativeVirtualKeyCode: 67,
          modifiers: CTRL,
        });
      }
      const copies = await until(() => evaluateIn(example, 'globalThis.__embedSites?.copies.length && __embedSites.copies'));
      const noticeAt =
        mode === 'inline'
          ? `${ELEMENT}.shadowRoot.querySelector('[role=status]')?.textContent`
          : `document.querySelector('body > [role=status]')?.textContent`;
      if (!copies) {
        result.copy = 'nothing';
      } else if (copies[0][0] === 'ok') {
        result.copy = 'ok';
      } else {
        const notice = await until(() => evaluateIn(example, noticeAt));
        result.copy = notice === COPY_NOTICE ? 'notice' : `refused without the notice (${copies[0][1]})`;
      }
      if (result.copy !== expect.copy) return fail(`copy=${result.copy}, expected ${expect.copy}`);
    }

    const fatal = log.find((line) => FATAL.test(line));
    if (fatal) return fail(`console: ${fatal.split('\n')[0].slice(0, 160)}`);
    const parts = [result.backend, `${seconds.toFixed(1)} s`];
    if (result.copy) parts.push(`copy=${result.copy}`);
    return Object.assign(result, { ok: true, line: `ok (${parts.join(', ')})` });
  } catch (error) {
    return fail(String(error?.stack ?? error).split('\n')[0]);
  } finally {
    if (!result.ok) await chrome.screenshot(join(OUT, `sites-${id.replace('/', '-')}.png`)).catch(() => {});
    await chrome.close();
  }
}

const pages = await site();
const source = process.env.SOURCE ?? new URL('embed/live-grid.js', pages.base).href;
// The examples' pages: next to `embed/` in a www/, `demo/` on the site.
const demo =
  process.env.DEMO ??
  new URL(/\/embed\/v\d+\/live-grid\.js$/.test(source) ? '../../demo/' : '../', source).href;
const chain = await chainServer(source, demo);
console.log(
  `embed-sites: the element from ${source}, the platforms on *.localhost:${chain.server.address().port}`,
);
const startedAt = Date.now();
const results = [];
try {
  for (const platform of platforms) {
    for (const app of APPS) {
      const result = await runCase(platform, app, chain.top(platform, app));
      results.push(result);
      console.log(`${result.id}: ${result.ok ? result.line : `FAILED (${result.why})`}`);
    }
  }
} finally {
  pages.close();
  chain.server.close();
}

if (process.env.REPORT) {
  writeFileSync(process.env.REPORT, `${JSON.stringify({ source, results }, null, 2)}\n`);
}
const failed = results.filter((result) => !result.ok).length;
const seconds = Math.round((Date.now() - startedAt) / 1000);
console.log(`embed-sites: ${results.length - failed} ok, ${failed} failed, ${seconds} s`);
process.exitCode = failed ? 1 : 0;
