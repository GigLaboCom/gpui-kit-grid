// Headless Chrome (`--headless=new`: no window) over the DevTools protocol
// on a pipe, not a port, attached to its first page. The measuring and
// input scripts share it.
//
// Env: CHROME (the browser binary), CHROME_FLAGS (extra flags, space
// separated).

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

export const CHROME =
  process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const FLAGS = (process.env.CHROME_FLAGS ?? '').split(' ').filter(Boolean);
export const COMMON = [
  '--headless=new',
  '--no-first-run',
  '--no-default-browser-check',
  '--window-size=1280,800',
  ...FLAGS,
];

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Starts Chrome on about:blank, with `flags` after the common ones (a later
// flag wins), and attaches to the page. `send` talks to the page, `browser`
// to the browser; `on` adds a listener for the page's events; `close` ends
// Chrome and removes its profile.
export async function launch(flags = []) {
  const profile = mkdtempSync(join(tmpdir(), 'grid-web-chrome-'));
  const chrome = spawn(
    CHROME,
    [...COMMON, ...flags, '--remote-debugging-pipe', `--user-data-dir=${profile}`, 'about:blank'],
    { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'] },
  );
  const [toChrome, fromChrome] = [chrome.stdio[3], chrome.stdio[4]];

  let nextId = 1;
  const pending = new Map();
  const listeners = [];
  function call(method, params = {}, sessionId) {
    const id = nextId++;
    toChrome.write(`${JSON.stringify({ id, method, params, sessionId })}\0`);
    return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
  }
  let buffer = '';
  fromChrome.on('data', (chunk) => {
    buffer += chunk;
    for (let end; (end = buffer.indexOf('\0')) >= 0; ) {
      const message = JSON.parse(buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      if (message.id) {
        const request = pending.get(message.id);
        pending.delete(message.id);
        if (message.error) request.reject(new Error(message.error.message));
        else request.resolve(message.result);
      } else {
        for (const listener of listeners) listener(message);
      }
    }
  });

  const { targetInfos } = await call('Target.getTargets');
  const page = targetInfos.find((target) => target.type === 'page');
  const { sessionId } = await call('Target.attachToTarget', {
    targetId: page.targetId,
    flatten: true,
  });
  const send = (method, params = {}) => call(method, params, sessionId);
  // The sessions of the page's out-of-process frames (another site's
  // iframes), once `watchFrames` attaches to them.
  const frames = new Set();
  listeners.push(({ method, params }) => {
    if (method === 'Target.attachedToTarget' && params.targetInfo.type === 'iframe') {
      frames.add(params.sessionId);
      call('Runtime.enable', {}, params.sessionId).catch(() => {});
    } else if (method === 'Target.detachedFromTarget') {
      frames.delete(params.sessionId);
    }
  });
  const evaluateIn = async (session, expression) => {
    const { result, exceptionDetails } = await call(
      'Runtime.evaluate',
      { expression, returnByValue: true, awaitPromise: true },
      session,
    );
    return exceptionDetails ? undefined : result.value;
  };
  return {
    send,
    browser: (method, params = {}) => call(method, params),
    // Listens to the browser's own events (`Fetch.enable` sent to `browser`
    // pauses the requests of every page and frame there).
    onBrowser(listener) {
      listeners.push((message) => {
        if (!message.sessionId) listener(message);
      });
    },
    // Listens to the page's events, and its watched frames'.
    on(listener) {
      listeners.push((message) => {
        if (message.sessionId === sessionId || frames.has(message.sessionId)) listener(message);
      });
    },
    // The value of a page expression, `undefined` if it throws.
    evaluate: (expression) => evaluateIn(sessionId, expression),
    // Attaches to each out-of-process frame of the page as it starts; call
    // it before navigating.
    watchFrames: () =>
      send('Target.setAutoAttach', {
        autoAttach: true,
        waitForDebuggerOnStart: false,
        flatten: true,
      }),
    // The value of an expression in the first watched frame whose address
    // contains `part`, `undefined` if there is none or it throws.
    async evaluateInFrame(part, expression) {
      for (const session of frames) {
        const href = await evaluateIn(session, 'location.href').catch(() => undefined);
        if (href?.includes(part)) return evaluateIn(session, expression).catch(() => undefined);
      }
      return undefined;
    },
    async screenshot(path) {
      const shot = await send('Page.captureScreenshot', { format: 'png' });
      writeFileSync(path, Buffer.from(shot.data, 'base64'));
    },
    async close() {
      await call('Browser.close').catch(() => {});
      await new Promise((resolve) =>
        chrome.exitCode === null ? chrome.once('exit', resolve) : resolve(),
      );
      rmSync(profile, { recursive: true, force: true });
    },
  };
}
