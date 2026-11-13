// The snippet Play hands out, and how each snippet site is opened with it
// prefilled: a form posted to a new tab (CodePen, JSFiddle, StackBlitz) or
// a link with the code in it (MDN Playground). No account on either side;
// the visitor's browser sends the form, and the site shows an unsaved
// project.

/**
 * The element's address in snippets for other sites: the site's own host,
 * never a redirect, which would answer without CORS.
 */
export const EMBED_SRC = 'https://gkg.giglabo.com/embed/v1/live-grid.js';

/** A form a button submits in a new tab: where, and its fields. */
export interface Prefill {
  action: string;
  fields: Record<string, string>;
}

const TITLE = 'gpui-kit-grid live quotes';
const DESCRIPTION = 'A live gpui-kit-grid example (GPUI in WebAssembly), themed in Play.';
const CSS = 'body { margin: 0; background: #16191e; }\nlive-grid { height: 100vh; }\n';

/** Escapes text for an HTML attribute in double quotes. */
const attribute = (text: string) =>
  text.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;');

/**
 * The two lines that embed the example: the element's script (`src`, an
 * absolute address) and the tag, with the packed theme when there is one.
 */
export function snippet(src: string, theme: string | null, app = 'quotes'): string {
  const themeAttribute = theme ? ` theme="${attribute(theme)}"` : '';
  return (
    `<script type="module" src="${attribute(src)}"></script>\n` +
    `<live-grid app="${app}"${themeAttribute} autoload></live-grid>\n`
  );
}

/** CodePen's prefill API: one `data` field, JSON. */
export function codePen(html: string): Prefill {
  const data = { title: TITLE, description: DESCRIPTION, html, css: CSS, js: '', editors: '110' };
  return { action: 'https://codepen.io/pen/define', fields: { data: JSON.stringify(data) } };
}

/** JSFiddle's "display a fiddle from POST", without a framework. */
export function jsFiddle(html: string): Prefill {
  return {
    action: 'https://jsfiddle.net/api/post/library/pure/',
    fields: { title: TITLE, description: DESCRIPTION, html, css: CSS, js: '' },
  };
}

/** StackBlitz's POST API with its static `html` template. */
export function stackBlitz(html: string): Prefill {
  const page =
    '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8" />\n' +
    `<title>${TITLE}</title>\n<link rel="stylesheet" href="style.css" />\n</head>\n<body>\n` +
    html +
    '</body>\n</html>\n';
  return {
    action: 'https://stackblitz.com/run',
    fields: {
      'project[title]': TITLE,
      'project[description]': DESCRIPTION,
      'project[template]': 'html',
      'project[files][index.html]': page,
      'project[files][style.css]': CSS,
    },
  };
}

/**
 * MDN Playground: the code in the address, `?state=` the base64 of the
 * deflated JSON `{html, css, js}`, as MDN's playground writes it.
 */
export async function mdnPlayground(html: string): Promise<string> {
  const json = new TextEncoder().encode(JSON.stringify({ html, css: CSS, js: '' }));
  const stream = new Blob([json]).stream().pipeThrough(new CompressionStream('deflate-raw'));
  const bytes = new Uint8Array(await new Response(stream).arrayBuffer());
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  const url = new URL('https://developer.mozilla.org/en-US/play');
  url.searchParams.set('state', btoa(binary));
  return url.href;
}
