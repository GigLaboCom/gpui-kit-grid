// Every internal link of the built site lands on a page or a file. Run
// after `bun run build`; without `dist/` the check is skipped. `/api/` and
// `/demo/` are left out until they are there: the deploy adds rustdoc and
// the demos next to the site's own files (`crates/grid-web/scripts/
// site-dist.mjs`), and then their links are checked too, but for rustdoc's
// own pages, which link into dependencies `--no-deps` leaves out.

import { describe, expect, test } from 'bun:test';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join, posix, relative } from 'node:path';

const DIST = join(import.meta.dir, '..', 'dist');
const ASSEMBLED_BY_DEPLOY = ['/api/', '/demo/'].filter((prefix) => !existsSync(join(DIST, prefix)));
const RUSTDOC = 'api';
function pages(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return pages(path);
    return entry.name.endsWith('.html') ? [path] : [];
  });
}

/** Whether `/path/` names something in `dist/`: a file, or a folder's index. */
function resolves(path: string): boolean {
  const file = join(DIST, decodeURIComponent(path));
  if (!existsSync(file)) return false;
  return statSync(file).isFile() || existsSync(join(file, 'index.html'));
}

describe.skipIf(!existsSync(DIST))('links in dist/', () => {
  const all = existsSync(DIST)
    ? pages(DIST).filter((page) => relative(DIST, page).split(/[\\/]/)[0] !== RUSTDOC)
    : [];

  test('there are pages', () => {
    expect(all.length).toBeGreaterThan(10);
  });

  test('every internal link resolves', () => {
    const broken: string[] = [];
    for (const page of all) {
      const html = readFileSync(page, 'utf8');
      const here = '/' + relative(DIST, page).split('\\').join('/');
      for (const match of html.matchAll(/\s(?:href|src)="([^"]*)"/g)) {
        const link = match[1].replaceAll('&amp;', '&');
        if (link === '' || link.startsWith('#') || link.startsWith('//')) continue;
        if (/^[a-z][a-z0-9+.-]*:/i.test(link)) continue;
        const target = link.replace(/[?#].*$/, '');
        if (target === '') continue;
        const path = posix.normalize(
          target.startsWith('/') ? target : posix.join(posix.dirname(here), target),
        );
        if (ASSEMBLED_BY_DEPLOY.some((prefix) => path === prefix || path.startsWith(prefix))) continue;
        if (!resolves(path)) broken.push(`${here}: ${link}`);
      }
    }
    expect(broken).toEqual([]);
  });
});
