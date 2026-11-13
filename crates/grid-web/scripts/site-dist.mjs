// Stages the examples for the public site: www/ of a site build (`make
// site`, `SITE=1`) into `<dist>/demo/`, where the site serves them under
// `/demo/` and `<live-grid>` (`/embed/v1/live-grid.js`) loads their
// modules, fonts, data and setup scripts. `make site-dist` builds and then
// runs this; `node scripts/site-dist.mjs [dist] [--api=<doc>]` stages an
// existing build (default dist: `site/dist`, Astro's output, which is built
// first, as its build empties the folder). With `--api`, rustdoc's output
// (`target/doc` of `cargo doc --no-deps -p gpui-kit-grid`) goes to
// `<dist>/api/` too, with an index that forwards to the crate's page.
//
// What goes, as files (the links `fonts/` and `data/` dereferenced):
//
// - the pages, `shell.js`/`shell.css`, `embed/`, each example's page and
//   `setup.js`, `pkg/` (modules, glue, `module.json`, `apps.json`,
//   `site.json`, the Iosevka subsets) and `licenses/`;
// - `fonts/`: the faces and each family's `OFL.txt`, but not Iosevka's
//   whole faces (8.4 MB each; the pages fetch the subsets in `pkg/fonts/`);
// - `data/`: only what the site build allowed (`scripts/licenses.mjs`),
//   each file with its `.license` next to it. Never CelesTrak's elements,
//   a GTFS archive, an uncompressed table or a raw HSL recording.
//
// Never `.built` (a local path) or dot files, and no staged file may name
// the builder's home folder (`build-wasm.sh` remaps the paths a module
// carries). Then it checks the whole dist against Cloudflare's static
// assets limits (25 MiB a file, 20,000 files) and prints its size and
// largest files. Exits 1 on any problem.

import {
  cpSync,
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const WWW = fileURLToPath(new URL('../www/', import.meta.url));
const args = process.argv.slice(2);
const API = args.find((arg) => arg.startsWith('--api='))?.slice('--api='.length);
const DIST = resolve(
  args.find((arg) => !arg.startsWith('--')) ??
    fileURLToPath(new URL('../../../site/dist/', import.meta.url)),
);
const DEMO = join(DIST, 'demo');
// The crate whose page `/api/` forwards to.
const CRATE = 'gpui_kit_grid';
// Cloudflare Workers static assets (free plan).
const MAX_FILE = 25 * 1024 * 1024;
const MAX_FILES = 20000;
// Data the site must never serve, whatever a build left in `data/`.
const FORBIDDEN_DATA = [
  /celestrak/i, // CelesTrak grants no redistribution: the browser asks it
  /\.zip$/i, // the GTFS archives
  /recording-all/i, // the whole HSL recording
  /\.tsv$/i, // the uncompressed tables
];
// The examples whose page runs a `setup.js` (`embed/live-grid.js` SETUPS).
const SETUPS = ['orbit', 'transit', 'wiki', 'ships'];

const problems = [];
const problem = (text) => problems.push(text);
const mib = (bytes) => `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
const posix = (path) => path.split(sep).join('/');

// A file of www/ (its path relative to www/) that the site serves.
function served(path) {
  const parts = posix(path).split('/');
  const name = parts[parts.length - 1];
  if (name.startsWith('.')) return false; // `.built`, `.DS_Store`
  if (parts[0] === 'fonts' && parts.length === 3) {
    if (parts[1] === 'iosevka' && name.endsWith('.ttf')) return false;
    return name.endsWith('.ttf') || name === 'OFL.txt';
  }
  if (parts[0] === 'data' && parts.length === 2) {
    return !FORBIDDEN_DATA.some((pattern) => pattern.test(name.replace(/\.license$/, '')));
  }
  return true;
}

// Every file under `dir`, links followed, as [path, bytes].
function files(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    const info = statSync(path);
    if (info.isDirectory()) out.push(...files(path));
    else out.push([path, info.size]);
  }
  return out;
}

// The build: a site build, with its notices.
if (!existsSync(join(WWW, 'pkg', 'site.json'))) {
  problem('www/ is not a site build (no pkg/site.json): run make site first.');
}
if (!existsSync(join(WWW, 'licenses', 'index.html'))) {
  problem('www/licenses/ is missing: the site build writes it (cargo-about).');
}
if (existsSync(join(WWW, 'data')) && lstatSync(join(WWW, 'data')).isSymbolicLink()) {
  problem('www/data is the link of a local build, not the site build’s folder: run make site.');
}
if (problems.length) {
  for (const text of problems) console.error(`site-dist: ${text}`);
  process.exit(1);
}

// What the pages and the element need, per example.
const apps = JSON.parse(readFileSync(join(WWW, 'pkg', 'apps.json'), 'utf8'));
for (const app of apps) {
  const needed = [
    `${app}/index.html`,
    `pkg/${app}/${app}.js`,
    `pkg/${app}/${app}_bg.wasm`,
    `pkg/${app}/module.json`,
    `licenses/${app}.html`,
    ...(SETUPS.includes(app) ? [`${app}/setup.js`] : []),
  ];
  for (const path of needed) if (!existsSync(join(WWW, path))) problem(`www/${path} is missing`);
}
for (const path of ['index.html', 'shell.js', 'shell.css', 'embed/live-grid.js', 'embed/page.js', 'fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf', 'pkg/fonts/iosevka/Iosevka-Regular.ttf', 'pkg/fonts/iosevka/OFL.txt']) {
  if (!existsSync(join(WWW, path))) problem(`www/${path} is missing`);
}

// The copy.
rmSync(DEMO, { recursive: true, force: true });
cpSync(WWW, DEMO, {
  recursive: true,
  dereference: true,
  filter: (source) => {
    const path = relative(WWW, source);
    if (path === '') return true;
    return statSync(source).isDirectory() ? !posix(path).split('/').pop().startsWith('.') : served(path);
  },
});

// The data: each file with its licence note, nothing forbidden.
const dataDir = join(DEMO, 'data');
const data = existsSync(dataDir) ? readdirSync(dataDir) : [];
for (const name of data) {
  if (FORBIDDEN_DATA.some((pattern) => pattern.test(name.replace(/\.license$/, '')))) {
    problem(`demo/data/${name} must not be served`);
  }
  if (!name.endsWith('.license') && !data.includes(`${name}.license`)) {
    problem(`demo/data/${name} has no ${name}.license next to it`);
  }
}
for (const family of readdirSync(join(DEMO, 'fonts'))) {
  if (!existsSync(join(DEMO, 'fonts', family, 'OFL.txt'))) problem(`demo/fonts/${family}/ has no OFL.txt`);
}

// rustdoc under `api/`, its index forwarding to the crate.
if (API) {
  if (!existsSync(join(API, CRATE, 'index.html'))) {
    problem(`--api=${API}: no ${CRATE}/index.html (cargo doc --no-deps -p gpui-kit-grid first)`);
  } else {
    const api = join(DIST, 'api');
    rmSync(api, { recursive: true, force: true });
    cpSync(API, api, {
      recursive: true,
      filter: (source) => !source.split(sep).pop().startsWith('.'), // `.lock`
    });
    writeFileSync(
      join(api, 'index.html'),
      `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>gpui-kit-grid API</title>
<meta http-equiv="refresh" content="0; url=${CRATE}/">
<link rel="canonical" href="${CRATE}/">
</head>
<body><p><a href="${CRATE}/">gpui_kit_grid</a></p></body>
</html>
`,
    );
  }
}

// No local path in what the demos serve (a module's panic locations).
const home = Buffer.from(homedir());
for (const [path] of files(DEMO)) {
  if (readFileSync(path).includes(home)) {
    problem(`demo/${posix(relative(DEMO, path))} names the home folder: build with scripts/build-wasm.sh, which remaps it`);
  }
}

// The whole dist against the host's limits.
const all = files(DIST);
const demo = all.filter(([path]) => path.startsWith(DEMO + sep));
const total = (list) => list.reduce((sum, [, bytes]) => sum + bytes, 0);
for (const [path, bytes] of all) {
  if (bytes > MAX_FILE) problem(`${posix(relative(DIST, path))}: ${mib(bytes)}, over the 25 MiB a file`);
  if (/\.built$|\.DS_Store$/.test(path)) problem(`${posix(relative(DIST, path))} must not be served`);
}
if (all.length > MAX_FILES) problem(`${all.length} files, over ${MAX_FILES}`);
if (!existsSync(join(DIST, 'embed', 'v1', 'live-grid.js'))) {
  console.warn('site-dist: no embed/v1/live-grid.js in the dist yet: build the site (bun run build) first.');
}

console.log(`site-dist: demo/ ${demo.length} files, ${mib(total(demo))}; all of ${DIST}: ${all.length} files, ${mib(total(all))}`);
console.log('site-dist: the largest files (limit 25 MiB):');
for (const [path, bytes] of [...all].sort((a, b) => b[1] - a[1]).slice(0, 10)) {
  console.log(`  ${mib(bytes).padStart(10)}  ${posix(relative(DIST, path))}`);
}
if (problems.length) {
  for (const text of problems) console.error(`site-dist: ${text}`);
  process.exit(1);
}
