// A theme remix: a small set of a gpui-kit theme file's keys laid over an
// example's own theme, by the module's `set_theme` (see the shell's
// `remix.rs` for the keys). A remix travels as JSON (`?theme={…}`, the
// `theme` attribute of `<live-grid>`, `setTheme`) or packed into a link:
// `#theme=v1.<base64url of the deflated JSON>`. The fragment never reaches
// a server, so the link is the theme.
//
// Fonts: a remix names one of the families the pages ship (the module lists
// them); its faces are fetched from the demos' `fonts/` before the theme is
// shown. Any other family falls back to the example's own.

/// The version prefix of a packed remix: keys are only ever added to v1.
export const PACKED_PREFIX = 'v1.';

const toBase64Url = (bytes) => {
  let text = '';
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
};

const fromBase64Url = (text) =>
  Uint8Array.from(atob(text.replaceAll('-', '+').replaceAll('_', '/')), (c) => c.charCodeAt(0));

const pipe = async (bytes, transform) =>
  new Uint8Array(await new Response(new Blob([bytes]).stream().pipeThrough(transform)).arrayBuffer());

/// Packs a remix (an object or its JSON) into `v1.…`.
export async function packTheme(theme) {
  const json = typeof theme === 'string' ? theme : JSON.stringify(theme);
  const deflated = await pipe(new TextEncoder().encode(json), new CompressionStream('deflate-raw'));
  return PACKED_PREFIX + toBase64Url(deflated);
}

/// The remix a `v1.…` string packs, as an object.
export async function unpackTheme(packed) {
  if (!packed.startsWith(PACKED_PREFIX)) {
    throw new Error(`a packed theme starts with ${PACKED_PREFIX}`);
  }
  const bytes = fromBase64Url(packed.slice(PACKED_PREFIX.length));
  const json = new TextDecoder().decode(await pipe(bytes, new DecompressionStream('deflate-raw')));
  return JSON.parse(json);
}

/// A remix as JSON text, from an object, JSON text or a `v1.…` string;
/// throws for text that is neither.
export async function themeJson(theme) {
  if (typeof theme !== 'string') return JSON.stringify(theme);
  const text = theme.trim();
  return JSON.stringify(text.startsWith(PACKED_PREFIX) ? await unpackTheme(text) : JSON.parse(text));
}

/// The remix a page's address carries, as JSON text, or `null`: `#theme=`
/// (packed) wins over `?theme=` when that is JSON (`?theme=` with a name
/// picks one of the example's own themes instead).
export async function themeFromLocation(location) {
  const hash = new URLSearchParams(location.hash.slice(1)).get('theme');
  if (hash) return themeJson(hash);
  const query = new URLSearchParams(location.search).get('theme')?.trim();
  return query?.startsWith('{') ? query : null;
}

/// Shows `theme` (an object, JSON text or `v1.…`) in the running module
/// `wasm`, after fetching the faces of a shipped family it names (`fontUrl`
/// turns a path under `fonts/` into its address). Resolves with the
/// module's report (`applied`, `ignored`, `fonts_replaced`); rejects for
/// JSON that does not parse. Ignored keys are warned about on the console.
export async function applyTheme(wasm, theme, fontUrl) {
  const json = await themeJson(theme);
  const files = JSON.parse(wasm.theme_font_files(json));
  const faces = await Promise.all(
    files.map(async (file) => {
      const response = await fetch(fontUrl(file));
      if (!response.ok) throw new Error(`fonts/${file}: HTTP ${response.status}`);
      return new Uint8Array(await response.arrayBuffer());
    }),
  );
  for (const face of faces) wasm.add_theme_font(face);
  const report = JSON.parse(wasm.set_theme(json));
  if (report.error) throw new Error(`theme: ${report.error}`);
  for (const { key, reason } of report.ignored) console.warn(`theme: ${key} ignored: ${reason}`);
  for (const family of report.fonts_replaced) {
    console.warn(`theme: no font family ${family} here; the example's own is shown`);
  }
  return report;
}
