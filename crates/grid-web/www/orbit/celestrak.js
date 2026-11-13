// The orbit page's elements from CelesTrak itself, for the public site,
// which may not serve a copy: CelesTrak grants no redistribution. The
// visitor's browser downloads them from celestrak.org, and only after a
// click, under CelesTrak's usage policy:
//
// - one request per click, never repeated by the page;
// - the answer kept in the browser (Cache API) for two hours, CelesTrak's
//   update interval: within it the page uses the copy and asks nothing;
// - on anything but HTTP 200 with elements (a 403 for a repeated download,
//   any other status, a body that is not elements, no answer at all) the
//   page stops: no new request for two hours, also after a reload. An older
//   copy, if the browser has one, is offered instead.
//
// `orbitData()` shows the panel (`#celestrak`) at once, so the visitor can
// choose while the module downloads, and resolves with the elements
// (`{ data, modified }`) the module then starts with; until then it does
// not resolve, and the module does not start. The panel goes into the
// page's body, or into `root` with its style in `styles` (`<live-grid>`
// inline: its shadow root, which contains the panel's fixed position).

export const CELESTRAK = 'https://celestrak.org/NORAD/elements/gp.php?GROUP=active&FORMAT=json';
// The Cache API's entries: the elements, and the last refusal.
const CACHE = 'orbit-celestrak';
const REFUSED = `${CELESTRAK}&refused`;
// CelesTrak's update interval: no request within it after a download or a
// refusal.
const HOLD_MS = 2 * 60 * 60 * 1000;
const FETCHED = 'x-fetched-at';

const time = (ms) =>
  new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

// The Cache API, or null outside a secure context.
async function openCache() {
  try {
    return globalThis.caches ? await caches.open(CACHE) : null;
  } catch {
    return null;
  }
}

// A stored entry's bytes and times, or null.
async function stored(cache, key) {
  const response = await cache?.match(key);
  if (!response) return null;
  const fetchedAt = Number(response.headers.get(FETCHED));
  const modified = Date.parse(response.headers.get('Last-Modified') ?? '');
  return {
    fetchedAt,
    modified: Number.isNaN(modified) ? fetchedAt : modified,
    data: new Uint8Array(await response.arrayBuffer()),
  };
}

// Elements: a JSON array of objects with a catalog number.
function isElements(bytes) {
  try {
    const parsed = JSON.parse(new TextDecoder().decode(bytes));
    return Array.isArray(parsed) && parsed.length > 0 && 'NORAD_CAT_ID' in parsed[0];
  } catch {
    return false;
  }
}

// The one request: the elements, or the reason it stopped (`refusal`).
async function download(cache) {
  let response;
  try {
    response = await fetch(CELESTRAK, { credentials: 'omit', referrerPolicy: 'no-referrer' });
  } catch {
    return { refusal: 'CelesTrak could not be reached' };
  }
  if (response.status !== 200) return { refusal: `CelesTrak answered HTTP ${response.status}` };
  const data = new Uint8Array(await response.arrayBuffer());
  if (!isElements(data)) return { refusal: 'CelesTrak answered without orbital elements' };
  const fetchedAt = Date.now();
  const lastModified = response.headers.get('Last-Modified');
  const headers = { 'Content-Type': 'application/json', [FETCHED]: String(fetchedAt) };
  if (lastModified) headers['Last-Modified'] = lastModified;
  await cache?.put(CELESTRAK, new Response(data, { headers })).catch(() => {});
  const modified = Date.parse(lastModified ?? '');
  return { data, modified: Number.isNaN(modified) ? fetchedAt : modified };
}

const PANEL_STYLE = `
  #celestrak {
    box-sizing: border-box;
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 2;
    width: min(520px, calc(100% - 32px));
    padding: 20px;
    border: 1px solid var(--nav-border, #231919);
    border-radius: 8px;
    background: var(--nav, #070505);
    color: var(--foreground, #e8deda);
    font: 14px/1.5 system-ui, sans-serif;
    line-height: 1.5;
  }
  #celestrak h1 { font-size: 16px; margin: 0 0 8px; }
  #celestrak p { color: var(--muted, #97857f); margin: 0 0 12px; }
  #celestrak a { color: var(--accent, #f0757b); }
  #celestrak .actions { display: flex; flex-wrap: wrap; gap: 8px; }
  #celestrak button {
    font: inherit;
    padding: 6px 14px;
    border-radius: 6px;
    border: 1px solid var(--nav-border, #231919);
    background: var(--background, #0a0808);
    color: var(--foreground, #e8deda);
    cursor: default;
  }
  #celestrak button.primary { border-color: var(--accent, #f0757b); color: var(--accent, #f0757b); }
  #celestrak button:disabled { opacity: 0.5; }
  #celestrak .problem { color: var(--foreground, #e8deda); }
`;

// The panel in `container` (its style in `styles`): what the page asks of
// CelesTrak, the choices, and why it stopped. `render` redraws it;
// `close` removes it.
function panel(container = document.body, styles = document.head) {
  const style = document.createElement('style');
  style.textContent = PANEL_STYLE;
  styles.append(style);
  const root = document.createElement('section');
  root.id = 'celestrak';
  root.setAttribute('aria-labelledby', 'celestrak-title');
  container.append(root);
  // The page's loader stays under the panel, hidden, until a choice.
  const status = container === document.body ? document.getElementById('status') : null;
  if (status) status.style.visibility = 'hidden';
  return {
    render({ problem, load, older, busy }) {
      const title = document.createElement('h1');
      title.id = 'celestrak-title';
      title.textContent = 'Orbital data from CelesTrak';
      const about = document.createElement('p');
      about.innerHTML =
        'This example places about 17,000 active satellites from the orbital elements of ' +
        '<a href="https://celestrak.org" target="_blank" rel="noopener">CelesTrak</a>, derived ' +
        "from the US Space Force's public catalog. Your browser downloads them straight from " +
        'celestrak.org (about 7 MB), which sees your IP address, and keeps them for two hours, ' +
        'CelesTrak\'s update interval; the page asks for them again only after that.';
      const parts = [title, about];
      if (problem) {
        const text = document.createElement('p');
        text.className = 'problem';
        text.setAttribute('role', 'alert');
        text.textContent = problem;
        parts.push(text);
      }
      const actions = document.createElement('div');
      actions.className = 'actions';
      if (load) {
        const button = document.createElement('button');
        button.className = 'primary';
        button.dataset.action = 'load';
        button.textContent = busy ? 'Downloading…' : 'Load from CelesTrak (about 7 MB)';
        button.disabled = Boolean(busy);
        button.addEventListener('click', load);
        actions.append(button);
      }
      if (older) {
        const button = document.createElement('button');
        button.dataset.action = 'older';
        button.textContent = `Use the copy from ${time(older.fetchedAt)}`;
        button.disabled = Boolean(busy);
        button.addEventListener('click', older.use);
        actions.append(button);
      }
      parts.push(actions);
      root.replaceChildren(...parts);
    },
    close() {
      root.remove();
      style.remove();
      if (status) status.style.visibility = '';
    },
  };
}

// The elements, from the browser's copy or, after a click, from CelesTrak,
// with the panel in `root` (the body by default) and its style in `styles`.
export async function orbitData({ root, styles } = {}) {
  const cache = await openCache();
  const now = Date.now();
  const copy = await stored(cache, CELESTRAK);
  if (copy && now - copy.fetchedAt < HOLD_MS) return copy;
  const refused = await stored(cache, REFUSED);
  // Without the Cache API (plain http) a refusal holds for this visit only.
  let held = refused && now - refused.fetchedAt < HOLD_MS ? refused : null;

  const view = panel(root, styles);
  return new Promise((resolve) => {
    const finish = (result) => {
      view.close();
      resolve(result);
    };
    const older = copy && { fetchedAt: copy.fetchedAt, use: () => finish(copy) };
    const show = (busy = false) => {
      const reason = held && new TextDecoder().decode(held.data);
      view.render({
        problem: held
          ? `${reason} at ${time(held.fetchedAt)}, so this page asks nothing more of it until ` +
            `${time(held.fetchedAt + HOLD_MS)}. CelesTrak refuses a second download of the same ` +
            'data within two hours, also from another device behind the same address.'
          : null,
        load: held ? null : load,
        older,
        busy,
      });
    };
    async function load() {
      show(true);
      const result = await download(cache);
      if (result.data) {
        finish(result);
        return;
      }
      // An offline browser asked no one: it may try again.
      if (result.refusal === 'CelesTrak could not be reached' && navigator.onLine === false) {
        held = null;
        view.render({
          problem: 'Your browser is offline. Try again once it is back online.',
          load,
          older,
        });
        return;
      }
      const fetchedAt = Date.now();
      held = { fetchedAt, data: new TextEncoder().encode(result.refusal) };
      await cache
        ?.put(REFUSED, new Response(result.refusal, { headers: { [FETCHED]: String(fetchedAt) } }))
        .catch(() => {});
      show();
    }
    show();
  });
}
