// Puts the `<live-grid>` element on the site under a versioned path, as
// other sites load it:
//
// - `embed/v1/live-grid.js`, the entry snippets name: it moves with fixes
//   within version 1 (a short cache, `public/_headers`);
// - `embed/v1/modules/<name>-<hash>.js`, the modules it imports, named by
//   their content: cached for good.
//
// The sources are the examples' own (`crates/grid-web/www/embed/`), where
// the pages served from `www/` use them as they are. In the element the
// build rewrites its imports to the hashed modules, and where the examples
// are: next to `embed/` in `www/`, under `/demo/` on the site (the deploy
// copies `www/` there). It needs no wasm: only these few files.

import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const SOURCE = new URL('../../crates/grid-web/www/embed/', import.meta.url);
const VERSION = 'v1';
const MODULES = ['guest.js', 'theme.js'];
const DEMO_HERE = "new URL('../', import.meta.url)";
const DEMO_ON_SITE = "new URL('../../demo/', import.meta.url)";

/** Replaces the one `from` in `text`; fails the build if it is not there once. */
function replaceOnce(text, from, to) {
  const parts = text.split(from);
  if (parts.length !== 2) {
    throw new Error(`live-grid.js: expected ${from} once, found it ${parts.length - 1} times`);
  }
  return parts.join(to);
}

export default function embed() {
  return {
    name: 'live-grid-embed',
    hooks: {
      'astro:build:done': async ({ dir, logger }) => {
        const out = new URL(`embed/${VERSION}/`, dir);
        await mkdir(new URL('modules/', out), { recursive: true });
        let element = await readFile(new URL('live-grid.js', SOURCE), 'utf8');
        for (const name of MODULES) {
          const text = await readFile(new URL(name, SOURCE), 'utf8');
          if (/^\s*import\s/m.test(text)) {
            throw new Error(`${name}: an embed module imports nothing, or the build must rewrite it`);
          }
          const hash = createHash('sha256').update(text).digest('hex').slice(0, 10);
          const hashed = `${name.replace(/\.js$/, '')}-${hash}.js`;
          await writeFile(new URL(`modules/${hashed}`, out), text);
          element = replaceOnce(element, `'./${name}'`, `'./modules/${hashed}'`);
        }
        element = replaceOnce(element, DEMO_HERE, DEMO_ON_SITE);
        await writeFile(new URL('live-grid.js', out), element);
        logger.info(`<live-grid> in ${fileURLToPath(out)}`);
      },
    },
  };
}
