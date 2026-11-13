// Serves www/ over HTTP on 127.0.0.1 (a secure context, so WebGPU is
// offered): the index, each example's page (`<app>/`, a folder without its
// slash redirected to it), the modules, and the fonts and the orbit
// example's data through the `www/fonts` and `www/data` links the build
// makes; each file with its `Last-Modified`, which the orbit page shows as
// the data's age. `node scripts/serve.mjs [port]`; the checks import
// `site`. Single-threaded wasm needs no COOP/COEP headers. Every file may
// be loaded from another origin, as the site serves them for
// `<live-grid>` (`embed/live-grid.js`) on other pages. `serve(port, root)`
// serves another folder the same way (the embed check's host pages).

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('../www/', import.meta.url));
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.ttf': 'font/ttf',
  '.json': 'application/json',
};

export function serve(port = 0, root = ROOT) {
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://localhost');
    const path = url.pathname;
    const file = normalize(join(root, path.endsWith('/') ? `${path}index.html` : path));
    if (!file.startsWith(root)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const info = await stat(file);
      if (info.isDirectory()) {
        response.writeHead(301, { Location: `${path}/${url.search}` }).end();
        return;
      }
      response
        .writeHead(200, {
          'Content-Type': TYPES[extname(file)] ?? 'application/octet-stream',
          'Cache-Control': 'no-store',
          'Last-Modified': info.mtime.toUTCString(),
          'Access-Control-Allow-Origin': '*',
          'Cross-Origin-Resource-Policy': 'cross-origin',
        })
        .end(await readFile(file));
    } catch {
      response.writeHead(404).end();
    }
  });
  return new Promise((resolve) =>
    server.listen(port, '127.0.0.1', () => resolve(server)),
  );
}

// The pages the checks run against: ORIGIN (the address of a served www/,
// such as http://127.0.0.1:8091/ or a site's folder), or this checkout's
// www/ served on a free port. `page(app, query)` is the address of an
// example's page with `query` (without `?`); `close` stops the server.
export async function site() {
  let base;
  let server = null;
  if (process.env.ORIGIN) {
    base = new URL(process.env.ORIGIN);
    if (!base.pathname.endsWith('/')) base.pathname += '/';
  } else {
    server = await serve();
    base = new URL(`http://127.0.0.1:${server.address().port}/`);
  }
  return {
    base,
    page(app, query = '', from = base) {
      return `${new URL(`${app}/`, from).href}${query ? `?${query}` : ''}`;
    },
    close() {
      server?.close();
    },
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const server = await serve(Number(process.argv[2] || 8080));
  console.log(`http://127.0.0.1:${server.address().port}/`);
}
