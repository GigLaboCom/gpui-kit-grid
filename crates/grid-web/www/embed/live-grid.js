// `<live-grid>`: a live gpui-kit-grid example in any page, from one script.
//
//   <script type="module" src="https://gkg.giglabo.com/embed/v1/live-grid.js"></script>
//   <live-grid app="quotes" autoload></live-grid>
//
// Attributes:
// - `app`: the example (`quotes`, the default, `desk`, `broker`, `orbit`,
//   `transit`, `wiki`, `ships`, `ops`); inline, the element first fetches
//   what the example's page fetches before it runs (its data, a recording,
//   or orbit's elements from CelesTrak after a click in the element);
// - `mode`: `inline` (the default) runs the example's module in this page,
//   its canvas inside the element; `frame` shows the example's page in an
//   iframe (`allow="clipboard-write; fullscreen"`), which keeps its
//   keyboard shortcuts away from this page's;
// - `theme`: a theme remix, as JSON or packed (`v1.…`, as `/play/` and
//   `#theme=` links carry it); changing it recolours the running example;
// - `query`: the example's own parameters (`seed=7&rate=20`);
// - `autoload`: starts once the element is on screen; otherwise (and with
//   Save-Data or a touch screen) a button starts it.
//
// Script: `start()`, `setTheme(theme)` (resolves with what was applied),
// `grid` (the module's hooks, inline only); events `ready` and `error`
// (`detail.message`); `LiveGrid.packTheme` and `unpackTheme` on the class
// (`customElements.get('live-grid')`). Size the element with CSS (360 px
// high by default); a 20 px line at its bottom links the licences, says for
// the market examples and ops that their data is simulated, and for the
// others where their data comes from.
//
// Inline, every element instantiates the example's module afresh (a wasm
// module is compiled once per page and app): GPUI's web platform keeps one
// window per module and appends its canvas to the body, from where the
// element moves it into itself. The element keeps nothing in the browser's
// storage and loads nothing but the example: no analytics.

import { LINE_HEIGHT, addGuest, creditLine, holdFocus } from './guest.js';
import { applyTheme, packTheme, themeJson, unpackTheme } from './theme.js';

// The examples' pages, modules and fonts. The site's build points this at
// its `/demo/`; served from the examples' `www/`, they are next to `embed/`.
const DEMO = new URL('../', import.meta.url);

// What the line under an inline example says before its licences, from the
// example's `query`: the market examples name real companies with made-up
// prices, and ops is a made-up data center, as their pages say in their
// switcher; the examples on open data credit its source, as their pages do
// under the canvas. In a frame the example's page says it itself.
const SIMULATED = 'Simulated prices, not market data';
const NOTICES = {
  quotes: () => SIMULATED,
  desk: () => SIMULATED,
  broker: () => SIMULATED,
  ops: () => 'Simulated data',
  orbit: () => "Orbital elements: CelesTrak, from the US Space Force's public catalog",
  transit: (params) =>
    params.get('city') === 'helsinki'
      ? '© HSL, CC BY 4.0'
      : 'Timetable © MVV, CC BY 4.0; scheduled, not real-time',
  wiki: () => 'Wikimedia EventStreams; content CC BY-SA 4.0, Wikimedia contributors',
  ships: () => 'Source: Fintraffic / digitraffic.fi, license CC 4.0 BY',
};

// The examples whose page hands the module something before it runs (their
// data, a recording, or CelesTrak's elements after a click): each page's
// `setup.js`, which the element runs as the page does. Its `setup(params,
// { root, styles })` resolves with `{ beforeRun(wasm) }`; what it shows
// (orbit's CelesTrak panel) goes into `root`, its style into `styles`.
const SETUPS = ['orbit', 'transit', 'wiki', 'ships'];

const STYLE = `
  :host { display: block; position: relative; height: 360px; contain: content; }
  :host([hidden]) { display: none; }
  .stage { position: absolute; inset: 0; overflow: hidden; }
  :host(:not([mode='frame'])) .stage { bottom: ${LINE_HEIGHT}px; }
  .live-grid-credit { position: absolute; left: 0; right: 0; bottom: 0; }
  .stage > canvas, .stage > iframe { position: absolute; inset: 0; width: 100%; height: 100%; border: 0; }
  .poster {
    position: absolute; inset: 0; display: grid; place-content: center; gap: 6px;
    padding: 16px; text-align: center; font: 13px/1.4 system-ui, sans-serif;
    color: #d1d4dc; background: #16191e;
  }
  .poster button {
    font: inherit; font-weight: 600; padding: 8px 14px; border-radius: 6px;
    border: 1px solid #3a404c; background: #252932; color: inherit; cursor: default;
  }
  .poster button:hover { background: #2e333e; }
  .poster button:focus-visible { outline: 2px solid #5b8def; outline-offset: 2px; }
  .poster small { color: #7d8494; }
  .poster[role='alert'] { color: #ef9a9a; }
`;

const compiled = new Map(); // app → Promise<WebAssembly.Module>
const faces = new Map(); // font path → Promise<Uint8Array>
let instances = 0;
let starting = Promise.resolve(); // inline elements start one at a time
let mounting = null; // the element whose canvas GPUI is about to append

// GPUI appends its canvas to the body when its window opens: into the
// element that is starting.
new MutationObserver((records) => {
  for (const record of records) {
    for (const node of record.addedNodes) {
      if (node instanceof HTMLCanvasElement && mounting) mounting.adopt(node);
    }
  }
}).observe(document.body ?? document.documentElement, { childList: true });

const fontUrl = (file) => new URL(`fonts/${file}`, DEMO).href;

function fetchFace(file) {
  if (!faces.has(file)) {
    faces.set(
      file,
      fetch(fontUrl(file)).then(async (response) => {
        if (!response.ok) throw new Error(`fonts/${file}: HTTP ${response.status}`);
        return new Uint8Array(await response.arrayBuffer());
      }),
    );
  }
  return faces.get(file);
}

const nextFrame = () => new Promise((resolve) => requestAnimationFrame(resolve));

const canDraw = () =>
  'gpu' in navigator || Boolean(document.createElement('canvas').getContext('webgl2'));

class LiveGrid extends HTMLElement {
  static observedAttributes = ['theme'];

  /// Packs a remix (an object or its JSON) into `v1.…`, as `#theme=`
  /// links and the `theme` attribute carry it.
  static packTheme = packTheme;

  /// The remix a `v1.…` string packs, as an object.
  static unpackTheme = unpackTheme;

  #stage;
  #state = 'idle'; // idle, starting, ready, failed
  #glue = null; // the module's hooks, inline
  #frame = null;
  #canvas = null;
  #waiting = []; // calls made before the example is ready
  #replies = new Map(); // frame: theme request id → { resolve, reject }
  #nextId = 1;

  constructor() {
    super();
    const shadow = this.attachShadow({ mode: 'open' });
    const style = document.createElement('style');
    style.textContent = STYLE;
    this.#stage = document.createElement('div');
    this.#stage.className = 'stage';
    shadow.append(style, this.#stage);
  }

  connectedCallback() {
    if (this.#state !== 'idle' || this.#stage.childElementCount) return;
    if (this.getAttribute('mode') !== 'frame' && !this.shadowRoot.querySelector('.live-grid-credit')) {
      const licenses = new URL('licenses/', DEMO).href;
      const notice = NOTICES[this.app]?.(new URLSearchParams(this.getAttribute('query') ?? ''));
      this.shadowRoot.append(creditLine({ notice, licenses }));
    }
    const saveData = navigator.connection?.saveData === true;
    const touch = matchMedia('(pointer: coarse)').matches;
    if (this.hasAttribute('autoload') && !saveData && !touch) {
      const observer = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          observer.disconnect();
          this.start();
        }
      });
      observer.observe(this);
    } else {
      this.#poster();
    }
  }

  attributeChangedCallback(name, before, after) {
    if (name === 'theme' && after !== before && this.#state !== 'idle' && after) {
      this.setTheme(after).catch((error) => this.#fail(error, false));
    }
  }

  /// The example: `quotes` unless `app` names another.
  get app() {
    const app = this.getAttribute('app') || 'quotes';
    return /^[a-z]+$/.test(app) ? app : 'quotes';
  }

  /// The module's hooks (`stats`, `set_rate`, `set_theme`, …) once an
  /// inline example is ready; `null` otherwise.
  get grid() {
    return this.#state === 'ready' ? this.#glue : null;
  }

  /// Starts the example, if it has not started.
  start() {
    if (this.#state !== 'idle') return;
    this.#state = 'starting';
    this.#stage.replaceChildren();
    if (this.getAttribute('mode') === 'frame') {
      this.#startFrame().catch((error) => this.#fail(error));
    } else {
      starting = starting.then(() => this.#startInline().catch((error) => this.#fail(error)));
    }
  }

  /// Lays the theme remix `theme` (an object, JSON or `v1.…`) over the
  /// example's theme; resolves with the module's report, once the example
  /// is ready.
  async setTheme(theme) {
    const json = await themeJson(theme);
    if (this.#state !== 'ready') {
      await new Promise((resolve, reject) => this.#waiting.push({ resolve, reject }));
    }
    if (this.#glue) return applyTheme(this.#glue, json, fontUrl);
    const id = this.#nextId++;
    const reply = new Promise((resolve, reject) => this.#replies.set(id, { resolve, reject }));
    this.#frame.contentWindow.postMessage({ type: 'live-grid:set-theme', id, theme: json }, '*');
    return reply;
  }

  /// GPUI's canvas, appended to the body while this element starts.
  adopt(canvas) {
    this.#canvas = canvas;
    this.#stage.append(canvas);
  }

  #query() {
    const own = new URLSearchParams(this.getAttribute('query') ?? '');
    own.delete('embed');
    own.delete('theme');
    const query = own.toString();
    return `?embed${query ? `&${query}` : ''}`;
  }

  async #startInline() {
    if (!canDraw()) throw new Error('this browser can draw neither WebGPU nor WebGL 2');
    const app = this.app;
    const query = this.#query();
    // The example's setup, asked at once as its page asks it: a download
    // (or orbit's panel) goes on while the module compiles.
    const setup = SETUPS.includes(app)
      ? import(new URL(`${app}/setup.js`, DEMO).href).then((module) =>
          module.setup(new URLSearchParams(query), { root: this.#stage, styles: this.shadowRoot }),
        )
      : null;
    setup?.catch(() => {}); // Awaited below.
    if (!compiled.has(app)) {
      compiled.set(
        app,
        WebAssembly.compileStreaming(fetch(new URL(`pkg/${app}/${app}_bg.wasm`, DEMO))),
      );
    }
    const module = await compiled.get(app);
    const glue = await import(new URL(`pkg/${app}/${app}.js?instance=${instances++}`, DEMO).href);
    await glue.default({ module_or_path: module });
    const [first, later] = JSON.parse(glue.font_files(query));
    for (const face of await Promise.all(first.map(fetchFace))) glue.add_font(face);
    if (setup) await (await setup).beforeRun(glue);
    holdFocus();
    mounting = this;
    try {
      glue.run(query);
      for (const started = performance.now(); !this.#canvas; await nextFrame()) {
        if (performance.now() - started > 10000) throw new Error('no canvas after 10 s');
      }
    } finally {
      mounting = null;
    }
    addGuest({ root: this.#stage, events: this.shadowRoot, canvas: () => this.#canvas, wasm: glue });
    // GPUI sets the pointer's shape on the body: leaving the element gives
    // the page its own back.
    this.addEventListener('pointerleave', () => document.body.style.removeProperty('cursor'));
    // The window opens once the canvas has its size.
    for (const started = performance.now(); glue.stats() === 'null'; await nextFrame()) {
      if (performance.now() - started > 30000) throw new Error('the example did not open in 30 s');
    }
    this.#glue = glue;
    const theme = this.getAttribute('theme');
    if (theme) await applyTheme(glue, theme, fontUrl);
    this.#ready();
    // The other themes' faces, for an example's own theme menu.
    Promise.all(later.map(fetchFace)).then((loaded) => {
      for (const face of loaded) glue.add_font(face);
      if (loaded.length) glue.fonts_loaded();
    });
  }

  async #startFrame() {
    const frame = document.createElement('iframe');
    frame.title = `gpui-kit-grid ${this.app} example`;
    frame.allow = 'clipboard-write; fullscreen';
    const theme = this.getAttribute('theme');
    const hash = theme ? `#theme=${await packTheme(await themeJson(theme))}` : '';
    frame.src = new URL(`${this.app}/${this.#query()}${hash}`, DEMO).href;
    addEventListener('message', (event) => {
      if (event.source !== frame.contentWindow) return;
      const message = event.data;
      if (message?.type === 'live-grid:ready' && this.#state === 'starting') {
        this.#ready();
      } else if (message?.type === 'live-grid:theme') {
        const reply = this.#replies.get(message.id);
        this.#replies.delete(message.id);
        if (message.error) reply?.reject(new Error(message.error));
        else reply?.resolve(message.report);
      }
    });
    this.#frame = frame;
    this.#stage.append(frame);
  }

  #ready() {
    this.#state = 'ready';
    for (const { resolve } of this.#waiting.splice(0)) resolve();
    this.dispatchEvent(new CustomEvent('ready', { detail: { mode: this.#glue ? 'inline' : 'frame' } }));
  }

  #fail(error, fatal = true) {
    const message = String(error?.message ?? error);
    console.error(`live-grid: ${message}`);
    if (fatal) {
      this.#state = 'failed';
      for (const { reject } of this.#waiting.splice(0)) reject(error);
      const notice = document.createElement('div');
      notice.className = 'poster';
      notice.setAttribute('role', 'alert');
      notice.textContent = `The grid failed to start: ${message}`;
      this.#stage.append(notice);
    }
    this.dispatchEvent(new CustomEvent('error', { detail: { message } }));
  }

  #poster() {
    const poster = document.createElement('div');
    poster.className = 'poster';
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = `Run the live ${this.app} example`;
    button.addEventListener('click', () => this.start());
    const size = document.createElement('small');
    size.textContent = 'gpui-kit-grid in WebAssembly, about a 7 MB download';
    poster.append(button, size);
    this.#stage.append(poster);
  }
}

if (!customElements.get('live-grid')) customElements.define('live-grid', LiveGrid);
