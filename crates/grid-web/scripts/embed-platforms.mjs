// The snippet platforms and blog hosts `scripts/embed-sites.mjs`
// reproduces: one case per way a reader meets the two-line `<live-grid>`
// snippet (or an example's `?embed` page) there, as data. A case is the
// chain of documents from the top-level page down to the one that holds
// the snippet, each with the platform's iframe attributes and headers, and
// what the element should do at the bottom of it.
//
// A case:
// - `id`, `name`;
// - `basis`: how its attributes are known, `read` (from the platform's
//   current page, bundle, source or response headers), `documented` (from
//   its documentation, or source the live site could not be checked
//   against) or `assumed` (neither: a plausible chain to replace once
//   read); `source`, where, with the date it was read;
// - `frames`: the documents from the top. Each has the platform's `host`
//   (its real name: the runner serves it under `localhost`, keeping which
//   hosts share a site), the `headers` it is served with (a `srcdoc`
//   document has none: it inherits its parent's policies), and, below the
//   top, the iframe that holds it: `sandbox`, `allow`, `credentialless`,
//   `srcdoc` (the document written into the attribute instead of loaded),
//   and `attributes` (any other, as the platform writes them). The last
//   document holds the snippet, or with `example` is the example's own
//   `?embed` page from the CDN (a blog that frames the demo itself);
// - `mode`: the element's `mode` attribute (`inline` unless set);
// - `flags`: Chrome flags the case needs (a browser without WebGPU);
// - `expect`: `start` (`ready`, the default; `blocked`, no `ready` and no
//   example in the element's frame; `error`, the element's `error` event
//   with `error` in its message), `copy` (`ok`, Ctrl+C copies, or
//   `notice`, "Copy needs clipboard permission on this site"), and
//   `backend` when the case fixes it (`webgl2`).
//
// A content security policy's `https:` stands for any https site; served
// here over plain http, the runner adds the platforms' local hosts (and
// the CDN, when it is local) next to each `https:`.
//
// Hosts a site may print as a link card only (dev.to's `{% embed %}` of a
// plain page, a GitHub README) run no script and frame nothing: no case.

const BLOG = 'blog.example'; // the reader's page, another site than any platform
const FORUM = 'forum.example'; // a Discourse forum

// JSFiddle's result frame (editor, and inside its `/embedded/` page).
// `allow-clipboard-*` are not sandbox tokens: Chrome ignores them.
const JSFIDDLE_SANDBOX =
  'allow-downloads allow-forms allow-modals allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation allow-clipboard-read allow-clipboard-write';
const JSFIDDLE_ALLOW =
  'camera *; display-capture *; geolocation *; microphone *; web-share *; clipboard-write *; clipboard-read *; summarizer *;';
const JSFIDDLE_EMBED_SANDBOX =
  'allow-forms allow-scripts allow-same-origin allow-top-navigation-by-user-activation allow-downloads allow-pointer-lock allow-presentation allow-clipboard-read allow-clipboard-write allow-modals allow-popups';
const JSFIDDLE_HEADERS = { 'Cross-Origin-Opener-Policy': 'same-origin', 'X-Frame-Options': 'ALLOWALL' };
const JSFIDDLE_EDITOR = [
  { host: 'jsfiddle.net', headers: JSFIDDLE_HEADERS },
  {
    host: 'fiddle.jshell.net',
    sandbox: JSFIDDLE_SANDBOX,
    allow: JSFIDDLE_ALLOW,
    attributes: { name: 'result', loading: 'lazy' },
    headers: { 'Cross-Origin-Opener-Policy': 'same-origin' },
  },
];
// JSFiddle's embed code: no `allow` on the frame the blog writes.
const jsfiddleEmbed = (blog) => [
  { host: blog },
  {
    host: 'jsfiddle.net',
    attributes: { loading: 'lazy', allowtransparency: 'true', allowfullscreen: 'true' },
    headers: JSFIDDLE_HEADERS,
  },
  {
    host: 'fiddle.jshell.net',
    sandbox: JSFIDDLE_EMBED_SANDBOX,
    allow: JSFIDDLE_ALLOW,
    headers: JSFIDDLE_HEADERS,
  },
];

// CodePen's srcdoc preview, as Full Page writes it (cdpn.io, read
// 2026-11-15); the embed pages' preview is assumed to be the same.
const CODEPEN_SANDBOX =
  'allow-downloads allow-forms allow-modals allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-presentation allow-same-origin allow-scripts allow-top-navigation-by-user-activation';
const CODEPEN_ALLOW =
  'accelerometer *; bluetooth *; camera *; clipboard-read *; clipboard-write *; display-capture *; encrypted-media *; geolocation *; gyroscope *; language-detector *; language-model *; local-network-access *; microphone *; midi *; rewriter *; serial *; summarizer *; translator *; web-share *; writer *; xr-spatial-tracking *';
const codepenPreview = (host) => ({ host, srcdoc: true, sandbox: CODEPEN_SANDBOX, allow: CODEPEN_ALLOW });
// What CodePen's embed script (embed/index.js) writes in Chrome.
const CODEPEN_SCRIPT_SANDBOX =
  'allow-downloads allow-forms allow-modals allow-pointer-lock allow-popups-to-escape-sandbox allow-popups allow-presentation allow-same-origin allow-scripts allow-top-navigation-by-user-activation';
const CODEPEN_SCRIPT_ALLOW =
  'accelerometer; bluetooth; camera; clipboard-read; clipboard-write; display-capture; encrypted-media; geolocation; gyroscope; language-detector; language-model; local-network-access; microphone; midi; rewriter; serial; summarizer; tools; translator; web-share; writer; xr-spatial-tracking';
// A pen's embed page framed by a blog without `allow` (dev.to, Hashnode,
// a Discourse onebox), or with what `frame` adds.
const codepenEmbed = (blog, frame = {}, headers = {}) => [
  { host: blog, headers },
  { host: 'codepen.io', attributes: { loading: 'lazy', scrolling: 'no' }, ...frame },
  codepenPreview('codepen.io'),
];

// StackBlitz: the preview's `allow` is the editor document's
// `featurePolicy.allowedFeatures()`; these are what a top-level editor
// has (a subset that holds the clipboard).
const STACKBLITZ_PREVIEW_ALLOW =
  'accelerometer; autoplay; camera; clipboard-read; clipboard-write; cross-origin-isolated; display-capture; encrypted-media; fullscreen; geolocation; gyroscope; magnetometer; microphone; midi; payment; picture-in-picture; serial; usb; xr-spatial-tracking';
const STACKBLITZ_SDK_ALLOW =
  'accelerometer *; ambient-light-sensor *; autoplay *; battery *; bluetooth *; camera *; clipboard-read *; clipboard-write *; display-capture *; encrypted-media *; fullscreen *; gamepad *; geolocation *; gyroscope *; hid *; idle-detection *; local-network *; local-network-access *; loopback-network *; magnetometer *; microphone *; midi *; payment *; picture-in-picture *; publickey-credentials-get *; screen-wake-lock *; serial *; usb *; web-share *; xr-spatial-tracking *';
const STACKBLITZ_NODE = [
  {
    host: 'stackblitz.com',
    headers: { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'credentialless' },
  },
  {
    host: 'abc123.local-credentialless.webcontainer-api.io',
    allow: 'clipboard-read; clipboard-write; cross-origin-isolated',
    // A cross-origin document in a COEP page needs CORP as well.
    headers: {
      'Cross-Origin-Embedder-Policy': 'credentialless',
      'Cross-Origin-Resource-Policy': 'cross-origin',
    },
  },
];

const MDN_CSP =
  "default-src 'self' https:; connect-src 'self' https: data:; script-src 'self' https: 'unsafe-eval' 'unsafe-inline' 'wasm-unsafe-eval'; script-src-elem 'self' https: 'unsafe-eval' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'report-sample' 'self' https: 'unsafe-inline' 'unsafe-eval'; img-src 'self' blob: https: data:; media-src 'self' blob: https:; base-uri 'self'; worker-src 'self'; manifest-src 'self';";
const MDN_RUNNER = 'abc123.mdnplay.dev';
const MDN_EDITOR = [
  { host: 'developer.mozilla.org' },
  {
    host: MDN_RUNNER,
    sandbox: 'allow-scripts allow-same-origin allow-forms',
    headers: { 'Content-Security-Policy': MDN_CSP },
  },
];

const CODESANDBOX_SANDBOX =
  'allow-forms allow-modals allow-popups allow-presentation allow-same-origin allow-scripts allow-downloads allow-pointer-lock';
const CODESANDBOX_ALLOW =
  'accelerometer; ambient-light-sensor; camera; encrypted-media; geolocation; gyroscope; hid; microphone; midi; payment; usb; vr; xr-spatial-tracking';

const LIVECODES_SANDBOX =
  'allow-same-origin allow-downloads allow-forms allow-modals allow-orientation-lock allow-pointer-lock allow-popups allow-presentation allow-scripts';
const LIVECODES_ALLOW =
  'accelerometer; bluetooth; camera; clipboard-read; clipboard-write; display-capture; encrypted-media; geolocation; gyroscope; language-detector; language-model; local-network-access; microphone; midi; proofreader; rewriter; serial; summarizer; translator; web-share; writer; window-placement; xr-spatial-tracking';
const LIVECODES_PREVIEW = {
  host: 'livecodes-sandbox.pages.dev',
  sandbox: LIVECODES_SANDBOX,
  allow: LIVECODES_ALLOW,
};

const DEVTO = 'dev.to';
const DEVTO_HEADERS = { 'Content-Security-Policy': 'frame-ancestors https://dev.to' };
const DISCOURSE_HEADERS = { 'Cross-Origin-Opener-Policy': 'same-origin' };

const ISOLATED = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};
const CREDENTIALLESS = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'credentialless',
};

// Chrome whose WebGPU has no adapter (as a blocklisted GPU), and without
// WebGL as well.
const NO_WEBGPU = ['--disable-features=WebGPUService'];
const NO_GPU = [...NO_WEBGPU, '--disable-webgl'];

export const PLATFORMS = [
  // A page of its own.
  {
    id: 'page',
    name: "Another site's page, top level (a Ghost HTML card, WordPress Custom HTML)",
    basis: 'documented',
    source: 'forum.ghost.org/t/adding-javascript-to-a-page-with-html-card/26776; WordPress roles (unfiltered_html)',
    frames: [{ host: BLOG }],
    expect: { copy: 'ok' },
  },

  // JSFiddle.
  {
    id: 'jsfiddle-editor',
    name: "JSFiddle, the editor's result frame",
    basis: 'read',
    source: 'jsfiddle.net/vite-dist/assets/editor-B22Ko7wH.js and response headers (2026-11-15)',
    frames: JSFIDDLE_EDITOR,
    expect: { copy: 'ok' },
  },
  {
    id: 'jsfiddle-editor-frame',
    name: "JSFiddle, the editor's result frame, the element in frame mode",
    basis: 'read',
    source: 'as jsfiddle-editor',
    frames: JSFIDDLE_EDITOR,
    mode: 'frame',
    expect: { copy: 'ok' },
  },
  {
    id: 'jsfiddle-embed-iframe',
    name: 'JSFiddle, the embed code (/embedded/) on a page',
    basis: 'read',
    source: 'jsfiddle.net/vite-dist/assets/EmbedModal-Bx8IuHjA.js, embed-CYoM0-9c.js (2026-11-15)',
    frames: jsfiddleEmbed(BLOG),
    expect: { copy: 'notice' },
  },

  // CodePen.
  {
    id: 'codepen-classic-editor',
    name: "CodePen Classic, the editor: cdpn.io's frame, the srcdoc preview in it",
    basis: 'documented',
    source: 'editor frame without clipboard (codepen.io, 2026-11-07; Cloudflare challenge on 2026-11-15); srcdoc from cdpn.io/team/codepen/fullpage/PNaGbb (2026-11-15)',
    frames: [{ host: 'codepen.io' }, { host: 'cdpn.io' }, codepenPreview('cdpn.io')],
    expect: { copy: 'notice' },
  },
  {
    id: 'codepen-classic-editor-frame',
    name: 'CodePen Classic, the editor, the element in frame mode',
    basis: 'documented',
    source: 'as codepen-classic-editor',
    frames: [{ host: 'codepen.io' }, { host: 'cdpn.io' }, codepenPreview('cdpn.io')],
    mode: 'frame',
    expect: { copy: 'notice' },
  },
  {
    id: 'codepen-fullpage',
    name: 'CodePen Full Page: cdpn.io at the top, the srcdoc preview in it',
    basis: 'read',
    source: 'cdpn.io/team/codepen/fullpage/PNaGbb (2026-11-15)',
    frames: [{ host: 'cdpn.io' }, codepenPreview('cdpn.io')],
    expect: { copy: 'ok' },
  },
  {
    id: 'codepen-embed-script-chrome',
    name: 'CodePen, the embed script in Chrome, on a blog of its own (Ghost, WordPress)',
    basis: 'read',
    source: 'public.codepenassets.com/embed/index.js (2026-11-15); the embed page behind Cloudflare, its preview assumed as Full Page',
    frames: codepenEmbed(BLOG, { sandbox: CODEPEN_SCRIPT_SANDBOX, allow: CODEPEN_SCRIPT_ALLOW }),
    expect: { copy: 'ok' },
  },
  {
    id: 'codepen2-editor',
    name: "CodePen 2.0, the editor's preview on codepen.dev",
    basis: 'documented',
    source: 'blog.codepen.io/docs/what-changed/ (2026-11-15): *.codepen.dev with a session id; attributes not verifiable',
    frames: [{ host: 'codepen.io' }, { host: 'p1a2b3.codepen.dev' }],
    expect: { copy: 'notice' },
  },

  // StackBlitz.
  {
    id: 'stackblitz-html-editor',
    name: 'StackBlitz, html template (EngineBlock): the editor and its preview on stackblitz.io',
    basis: 'read',
    source: 'c.staticblitz.com/assets/pack/client-0e73011cbcc2a.js, stackblitz.com/run (2026-11-15)',
    frames: [{ host: 'stackblitz.com' }, { host: 'web-abc123.stackblitz.io', allow: STACKBLITZ_PREVIEW_ALLOW, attributes: { title: 'Preview page' } }],
    expect: { copy: 'ok' },
  },
  {
    id: 'stackblitz-html-preview-top',
    name: 'StackBlitz, html preview opened by itself',
    basis: 'read',
    source: 'js.stackblitz.io (2026-11-15)',
    frames: [{ host: 'web-abc123.stackblitz.io' }],
    expect: { copy: 'ok' },
  },
  {
    id: 'stackblitz-sdk-embed',
    name: 'StackBlitz, the SDK embed (embedProject) on a page',
    basis: 'read',
    source: 'github.com/stackblitz/sdk src/helpers.ts (2026-11-15)',
    frames: [
      { host: BLOG },
      { host: 'stackblitz.com', allow: STACKBLITZ_SDK_ALLOW },
      { host: 'web-abc123.stackblitz.io', allow: 'clipboard-read; clipboard-write; fullscreen' },
    ],
    expect: { copy: 'ok' },
  },
  {
    id: 'stackblitz-node-editor',
    name: 'StackBlitz, node template (WebContainers): COOP and COEP credentialless',
    basis: 'read',
    source: 'stackblitz.com/edit/node headers (2026-11-15); the preview headers assumed',
    frames: STACKBLITZ_NODE,
    expect: { copy: 'ok' },
  },
  {
    id: 'stackblitz-node-editor-frame',
    name: 'StackBlitz, node template, the element in frame mode (a credentialless frame)',
    basis: 'read',
    source: 'as stackblitz-node-editor',
    frames: STACKBLITZ_NODE,
    mode: 'frame',
    expect: { copy: 'ok' },
  },

  // MDN Playground.
  {
    id: 'mdn-editor',
    name: 'MDN Playground, the runner on mdnplay.dev in the editor (after Run)',
    basis: 'read',
    source: 'github.com/mdn/fred components/play-runner/element.js, vendor/yari/libs/play/index.js (2026-11-15)',
    frames: MDN_EDITOR,
    expect: { copy: 'notice' },
  },
  {
    id: 'mdn-editor-frame',
    name: 'MDN Playground, the element in frame mode (frame-src from its CSP)',
    basis: 'read',
    source: 'as mdn-editor',
    frames: MDN_EDITOR,
    mode: 'frame',
    expect: { copy: 'notice' },
  },
  {
    id: 'mdn-runner-permalink-top',
    name: 'MDN Playground, the runner permalink at the top level',
    basis: 'read',
    source: 'github.com/mdn/fred vendor/yari/libs/play/index.js; GET of a runner permalink (2026-11-15)',
    frames: [{ host: MDN_RUNNER, headers: { 'Content-Security-Policy': MDN_CSP } }],
    expect: { copy: 'ok' },
  },
  {
    id: 'mdn-runner-foreign-iframe',
    name: "MDN Playground, the runner permalink in another site's iframe",
    basis: 'read',
    source: 'as mdn-runner-permalink-top',
    frames: [{ host: BLOG }, { host: MDN_RUNNER, headers: { 'Content-Security-Policy': MDN_CSP } }],
    expect: { copy: 'notice' },
  },

  // CodeSandbox.
  {
    id: 'codesandbox-embed',
    name: 'CodeSandbox, the embed on a page, the preview on csb.app',
    basis: 'read',
    source: 'codesandbox.io/static/js/embed.1ff8d5a1b.js (2026-11-15); the outer frame not verified',
    frames: [
      { host: BLOG },
      { host: 'codesandbox.io' },
      { host: 'abc123.csb.app', sandbox: CODESANDBOX_SANDBOX, allow: CODESANDBOX_ALLOW },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'codesandbox-preview-top',
    name: 'CodeSandbox, the csb.app preview opened by itself',
    basis: 'read',
    source: 'new.csb.app (2026-11-15)',
    frames: [{ host: 'abc123.csb.app' }],
    expect: { copy: 'ok' },
  },
  {
    id: 'sandpack-static',
    name: "Sandpack's static template on a page of its own",
    basis: 'read',
    source: 'github.com/codesandbox/sandpack sandpack-client/src/clients/static/index.ts (2.19.8, 2026-11-15)',
    frames: [
      { host: BLOG },
      {
        host: 'preview.sandpack-static-server.codesandbox.io',
        sandbox: CODESANDBOX_SANDBOX,
        allow:
          'accelerometer; camera; encrypted-media; geolocation; gyroscope; hid; microphone; midi; clipboard-read; clipboard-write; xr-spatial-tracking;',
      },
    ],
    expect: { copy: 'ok' },
  },

  // LiveCodes.
  {
    id: 'livecodes-editor',
    name: 'LiveCodes, the editor (?html= link) and its result frame',
    basis: 'read',
    source: 'github.com/live-codes/livecodes src/livecodes/core.ts, src/sdk/internal.ts (2026-11-15)',
    frames: [{ host: 'livecodes.io' }, LIVECODES_PREVIEW],
    expect: { copy: 'ok' },
  },
  {
    id: 'livecodes-sdk-embed',
    name: 'LiveCodes, the SDK embed (createPlayground) on a page',
    basis: 'read',
    source: 'github.com/live-codes/livecodes src/sdk/index.ts (2026-11-15)',
    frames: [
      { host: BLOG },
      { host: 'livecodes.io', sandbox: LIVECODES_SANDBOX, allow: LIVECODES_ALLOW },
      LIVECODES_PREVIEW,
    ],
    expect: { copy: 'ok' },
  },

  // JS Bin, Observable.
  {
    id: 'jsbin-editor',
    name: 'JS Bin: the runner on null.jsbin.com (same site, another origin), the output frame in it',
    basis: 'documented',
    source: 'github.com/jsbin/jsbin public/js/render/live.js, public/js/runner/sandbox.js (last commit 2025-06-23); the live site behind Cloudflare',
    frames: [
      { host: 'jsbin.com' },
      {
        host: 'null.jsbin.com',
        sandbox: 'allow-modals allow-forms allow-pointer-lock allow-popups allow-same-origin allow-scripts',
      },
      {
        host: 'null.jsbin.com',
        srcdoc: true,
        sandbox: 'allow-modals allow-forms allow-pointer-lock allow-popups allow-same-origin allow-scripts',
        allow: 'geolocation; midi; camera; microphone; speaker;',
      },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'observable-notebook',
    name: 'Observable, a notebook cell in a sandboxed observableusercontent.com frame',
    basis: 'assumed',
    source: 'chain not verifiable (Vercel challenge, 2026-11-15); talk.observablehq.com: the clipboard stopped working in Chrome 93',
    frames: [
      { host: 'observablehq.com' },
      {
        host: 'abc123.static.observableusercontent.com',
        sandbox:
          'allow-downloads allow-forms allow-modals allow-popups allow-presentation allow-same-origin allow-scripts allow-storage-access-by-user-activation allow-top-navigation-by-user-activation',
      },
    ],
    expect: { copy: 'notice' },
  },

  // Blog hosts around the platforms.
  {
    id: 'devto-codepen',
    name: "dev.to {% codepen %}: the pen's embed page, the srcdoc preview",
    basis: 'read',
    source: 'github.com/forem/forem app/views/liquids/_codepen.html.erb (2026-11-15)',
    frames: codepenEmbed(DEVTO, {}, DEVTO_HEADERS),
    expect: { copy: 'notice' },
  },
  {
    id: 'devto-jsfiddle',
    name: 'dev.to {% jsfiddle %}: jsfiddle.net/embedded, the result on jshell.net',
    basis: 'read',
    source: 'jsfiddle.net/vite-dist/assets/embed-CYoM0-9c.js; forem liquid tag (2026-11-15)',
    frames: [{ ...jsfiddleEmbed(DEVTO)[0], headers: DEVTO_HEADERS }, ...jsfiddleEmbed(DEVTO).slice(1)],
    expect: { copy: 'notice' },
  },
  {
    id: 'devto-stackblitz',
    name: 'dev.to {% stackblitz %}: stackblitz.com/edit?embed=1, the preview on stackblitz.io',
    basis: 'read',
    source: 'c.staticblitz.com/assets/pack/client-0e73011cbcc2a.js; forem liquid tag (2026-11-15)',
    frames: [
      { host: DEVTO, headers: DEVTO_HEADERS },
      { host: 'stackblitz.com', attributes: { allowfullscreen: '' } },
      { host: 'web-abc123.stackblitz.io', allow: 'fullscreen' },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'devto-livecodes',
    name: 'dev.to {% livecodes %}: livecodes.io, the result frame',
    basis: 'read',
    source: 'github.com/forem/forem app/views/liquids/_livecodes.html.erb (2026-11-15)',
    frames: [
      { host: DEVTO, headers: DEVTO_HEADERS },
      { host: 'livecodes.io', sandbox: LIVECODES_SANDBOX },
      LIVECODES_PREVIEW,
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'devto-codesandbox',
    name: 'dev.to {% codesandbox %}: codesandbox.io/embed, the preview on csb.app',
    basis: 'read',
    source: 'github.com/forem/forem app/views/liquids/_codesandbox.html.erb (2026-11-15); the preview not re-checked',
    frames: [
      { host: DEVTO, headers: DEVTO_HEADERS },
      {
        host: 'codesandbox.io',
        sandbox: 'allow-modals allow-forms allow-popups allow-scripts allow-same-origin',
        allow: 'geolocation; microphone; camera; midi; vr; accelerometer; gyroscope; payment; ambient-light-sensor; encrypted-media; usb',
      },
      { host: 'abc123.csb.app' },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'medium-codepen',
    name: "Medium, a pen link: Medium's media frame, Embed.ly, the pen's embed page, the preview",
    basis: 'documented',
    source: 'cdn.embedly.com/widgets/media.html (2026-11-15); Medium behind Cloudflare',
    frames: [
      { host: 'medium.com' },
      { host: 'cdn.embedly.com' },
      { host: 'codepen.io', allow: 'accelerometer; gyroscope; fullscreen; encrypted-media; picture-in-picture;' },
      codepenPreview('codepen.io'),
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'hashnode-codepen',
    name: "Hashnode %[pen]: the pen's embed page, the srcdoc preview",
    basis: 'read',
    source: 'github.com/hashnode/webembeds codepen.provider.ts (2026-11-15); whether production adds a frame is not verified',
    frames: codepenEmbed('hashnode.dev', { attributes: { loading: 'lazy', scrolling: 'no', allowfullscreen: '' } }),
    expect: { copy: 'notice' },
  },
  {
    id: 'discourse-codepen-onebox',
    name: "Discourse, a pen link as a onebox: the pen's embed page, the preview",
    basis: 'read',
    source: 'github.com/discourse/discourse lib/onebox/engine/allowlisted_generic_onebox.rb (2026-11-15)',
    frames: codepenEmbed(FORUM, { attributes: { scrolling: 'no', frameborder: '0' } }, DISCOURSE_HEADERS),
    expect: { copy: 'notice' },
  },
  {
    id: 'discourse-codepen-raw-allow',
    name: 'Discourse, a raw iframe with allow="clipboard-write" to the pen\'s embed page',
    basis: 'read',
    source: 'github.com/discourse/discourse frontend/discourse/pretty-text/allow-lister.ts (2026-11-15); the embed page preview assumed as Full Page',
    frames: codepenEmbed(FORUM, { allow: 'clipboard-write; fullscreen' }, DISCOURSE_HEADERS),
    expect: { copy: 'ok' },
  },
  {
    id: 'discourse-own-iframe',
    name: "A Discourse forum of one's own, the example's ?embed page in allowed_iframes",
    basis: 'read',
    source: 'github.com/discourse/discourse config/site_settings.yml (2026-11-15)',
    frames: [{ host: FORUM, headers: DISCOURSE_HEADERS }, { example: true, allow: 'clipboard-write; fullscreen' }],
    expect: { copy: 'ok' },
  },
  {
    id: 'wordpress-oembed',
    name: "Self-hosted WordPress, a page with the snippet through oEmbed: sandbox allow-scripts, an opaque origin",
    basis: 'read',
    source: 'github.com/WordPress/wordpress-develop src/wp-includes/embed.php (2026-11-15)',
    frames: [
      { host: BLOG },
      { host: 'provider.example', sandbox: 'allow-scripts', attributes: { security: 'restricted', title: 'Embedded post' } },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'wordpress-oembed-example',
    name: "Self-hosted WordPress, the example's ?embed page through our oEmbed (once the site has one)",
    basis: 'read',
    source: 'as wordpress-oembed',
    frames: [
      { host: BLOG },
      { example: true, sandbox: 'allow-scripts', attributes: { security: 'restricted' } },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'mastodon-oembed-example',
    name: "Mastodon, a link card from our oEmbed (once the site has one): the example's ?embed page",
    basis: 'read',
    source: 'github.com/mastodon/mastodon lib/sanitize_ext/sanitize_config.rb (2026-11-15)',
    frames: [
      { host: 'mastodon.example', headers: { 'Content-Security-Policy': "frame-src 'self' https:" } },
      { example: true, attributes: { allowfullscreen: '' } },
    ],
    expect: { copy: 'notice' },
  },
  {
    id: 'notion-embed',
    name: "Notion, an /embed block: the example's ?embed page",
    basis: 'documented',
    source: 'www.notion.so CSP (2026-11-15); the frame attributes need an account, assumed absent',
    frames: [
      { host: 'notion.so', headers: { 'Content-Security-Policy': "frame-src 'self' https: http:" } },
      { example: true },
    ],
    expect: { copy: 'notice' },
  },

  // Cross-origin isolated hosts.
  {
    id: 'coep-require-corp',
    name: 'A strict cross-origin isolated host: COOP same-origin, COEP require-corp',
    basis: 'read',
    source: 'the headers as a cross-origin isolated page sends them',
    frames: [{ host: 'isolated.example', headers: ISOLATED }],
    expect: { copy: 'ok' },
  },
  {
    id: 'coep-require-corp-frame',
    name: 'A COEP require-corp host, the element in frame mode (a credentialless frame)',
    basis: 'read',
    source: 'as coep-require-corp',
    frames: [{ host: 'isolated.example', headers: ISOLATED }],
    mode: 'frame',
    expect: { copy: 'ok' },
  },
  {
    id: 'coep-credentialless',
    name: 'A host with COOP same-origin and COEP credentialless',
    basis: 'read',
    source: 'the headers as a cross-origin isolated page sends them',
    frames: [{ host: 'isolated.example', headers: CREDENTIALLESS }],
    expect: { copy: 'ok' },
  },
  {
    id: 'coep-credentialless-frame',
    name: 'A COEP credentialless host, the element in frame mode (a credentialless frame)',
    basis: 'read',
    source: 'as coep-credentialless',
    frames: [{ host: 'isolated.example', headers: CREDENTIALLESS }],
    mode: 'frame',
    expect: { copy: 'ok' },
  },

  // Browsers without WebGPU.
  {
    id: 'webgl2',
    name: "Another site's page in a browser whose WebGPU has no adapter: the WebGL 2 fallback",
    basis: 'read',
    source: 'Chrome with --disable-features=WebGPUService',
    frames: [{ host: BLOG }],
    flags: NO_WEBGPU,
    expect: { copy: 'ok', backend: 'webgl2' },
  },
  {
    id: 'no-gpu',
    name: 'A browser with neither a WebGPU adapter nor WebGL 2: the error event',
    basis: 'read',
    source: 'Chrome with --disable-features=WebGPUService --disable-webgl',
    frames: [{ host: BLOG }],
    flags: NO_GPU,
    expect: { start: 'error', error: 'neither WebGPU nor WebGL 2' },
  },
];
