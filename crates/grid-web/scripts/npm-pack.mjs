// Assembles the npm packages of `<live-grid>` (`npm/README.md`: demo
// modules, not a JavaScript library) from www/ and lists each with
// `npm pack --dry-run`. It publishes nothing and logs in nowhere.
//
// The packages go to `<out>/<scope>/<name>@<version>/`, the layout of a CDN
// that mirrors npm (jsDelivr's `/npm/`, unpkg), so that `<out>` served as
// files behaves as the CDN would:
//
// - `<scope>/live-grid`: `embed/live-grid.js` and the two modules it
//   imports, and the faces every example draws in (`fonts/<family>/` with
//   its `OFL.txt`, Iosevka as the subsets in `pkg/fonts/iosevka/`);
// - `<scope>/live-grid-<app>`, one per example: `pkg/<app>/<app>.js` and
//   `<app>_bg.wasm`, its `<app>/setup.js` and the modules that imports, the
//   data files the setup names (each with its `.license`), `pkg/site.json`
//   where the setup reads it, and the module's third-party notices
//   (`licenses/<app>.html`, `LICENSE-APACHE.txt`, `LICENSE-LUCIDE.txt`).
//
// Every package has `package.json` (from `npm/package.template.json`),
// `README.md` (`npm/README.md`) and `LICENSE` (the repository's
// Apache-2.0). All are released with one version: the element finds its
// fonts in its own package and an example in the package
// `live-grid-<app>` next to its own, both at the version it was packed
// with, wherever it is loaded from and whether a page names an exact
// version or a range. The example's page (`mode="frame"`) and the licences
// page stay on the site: CDNs serve HTML from npm as text.
//
// It needs a site build (`make site`): the notices and the data the site
// may serve. `--draft` packs what there is instead, takes the data from
// `crates/grid/examples/data/` (never CelesTrak's elements, an archive or a
// raw recording) and reports what is missing: for the listing, never for a
// release. Exits 1 on any problem.
//
// node scripts/npm-pack.mjs [--scope=@giglabo] [--version=<crate's>]
//   [--out=<dir>] [--apps=quotes,desk] [--draft]

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const CRATE = fileURLToPath(new URL('..', import.meta.url));
const REPO = join(CRATE, '..', '..');
const WWW = join(CRATE, 'www');
const NPM = join(CRATE, 'npm');
const FONTS = join(REPO, 'crates', 'grid', 'examples', 'fonts');
const EXAMPLE_DATA = join(REPO, 'crates', 'grid', 'examples', 'data');
// Where the element's frame pages and licences page are.
const SITE = 'https://gkg.giglabo.com/demo/';
const ELEMENT = 'live-grid';

const option = (name) =>
  process.argv.find((arg) => arg.startsWith(`--${name}=`))?.slice(name.length + 3);
const DRAFT = process.argv.includes('--draft');
const SCOPE = option('scope') ?? '@giglabo';
const VERSION =
  option('version') ??
  readFileSync(join(REPO, 'crates', 'grid', 'Cargo.toml'), 'utf8').match(/^version = "(.+)"/m)[1];
const OUT = option('out') ?? join(REPO, 'target', 'npm');

// Data no package may carry, as `scripts/site-dist.mjs` keeps it off the site.
const FORBIDDEN_DATA = [/celestrak/i, /\.zip$/i, /recording-all/i, /\.tsv$/i];
// jsDelivr's limits: 150 MB a package; 20 MB a file is its limit for
// GitHub files, which npm files above it have not met so far.
const MAX_PACKAGE = 150 * 1000 * 1000;
const LARGE_FILE = 20 * 1000 * 1000;

const problems = new Set();
const warnings = new Set();
// A missing input: a problem, or with `--draft` a warning.
const missing = (text) => (DRAFT ? warnings : problems).add(text);
const mb = (bytes) => `${(bytes / 1000 / 1000).toFixed(2)} MB`;
const posix = (path) => path.split(sep).join('/');

/** Replaces the one `from` in `text`; fails if it is not there once. */
function replaceOnce(text, from, to) {
  const parts = text.split(from);
  if (parts.length !== 2) {
    throw new Error(`live-grid.js: expected ${from} once, found it ${parts.length - 1} times`);
  }
  return parts.join(to);
}

/** Copies `from` to `dir/path`, making the folders. */
function put(dir, path, from) {
  mkdirSync(dirname(join(dir, path)), { recursive: true });
  copyFileSync(from, join(dir, path));
}

/** Every file under `dir`, links followed, as [path, bytes]. */
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

/** The element with its bases moved to the packages' layout. */
function element() {
  let text = readFileSync(join(WWW, 'embed', 'live-grid.js'), 'utf8');
  text = replaceOnce(
    text,
    "const DEMO = new URL('../', import.meta.url);",
    `// The npm packages (\`scripts/npm-pack.mjs\`): this one and each
// example's next to it, at the version this element was packed with.
const VERSION = '${VERSION}';
const PACKAGES = new URL('../../', import.meta.url);
const DEMO = new URL(\`${ELEMENT}@\${VERSION}/\`, PACKAGES);
const appBase = (app) => new URL(\`${ELEMENT}-\${app}@\${VERSION}/\`, PACKAGES);
// The examples' pages and the licences page, which CDNs serve as text.
const SITE = '${SITE}';`,
  );
  text = replaceOnce(text, "new URL(`${app}/setup.js`, DEMO)", 'new URL(`${app}/setup.js`, appBase(app))');
  text = replaceOnce(text, 'new URL(`pkg/${app}/${app}_bg.wasm`, DEMO)', 'new URL(`pkg/${app}/${app}_bg.wasm`, appBase(app))');
  text = replaceOnce(
    text,
    'new URL(`pkg/${app}/${app}.js?instance=${instances++}`, DEMO)',
    'new URL(`pkg/${app}/${app}.js?instance=${instances++}`, appBase(app))',
  );
  text = replaceOnce(text, "new URL('licenses/', DEMO)", "new URL('licenses/', SITE)");
  text = replaceOnce(text, 'new URL(`${this.app}/${this.#query()}${hash}`, DEMO)', 'new URL(`${this.app}/${this.#query()}${hash}`, SITE)');
  return text;
}

/** `package.json` from the template. */
function manifest(name, description, fields = {}) {
  const template = JSON.parse(readFileSync(join(NPM, 'package.template.json'), 'utf8'));
  return { ...template, name, version: VERSION, description, ...fields };
}

/** Writes `package.json`, `README.md` and `LICENSE`; `files` from what is there. */
function finish(dir, json) {
  copyFileSync(join(NPM, 'README.md'), join(dir, 'README.md'));
  copyFileSync(join(REPO, 'LICENSE-APACHE'), join(dir, 'LICENSE'));
  const own = new Set(['package.json', 'README.md', 'LICENSE']);
  json.files = readdirSync(dir)
    .filter((name) => !own.has(name))
    .map((name) => (statSync(join(dir, name)).isDirectory() ? `${name}/` : name))
    .sort();
  writeFileSync(join(dir, 'package.json'), `${JSON.stringify(json, null, 2)}\n`);
}

/** The element's package. */
function elementPackage(dir) {
  mkdirSync(join(dir, 'embed'), { recursive: true });
  writeFileSync(join(dir, 'embed', 'live-grid.js'), element());
  for (const name of ['guest.js', 'theme.js']) {
    const text = readFileSync(join(WWW, 'embed', name), 'utf8');
    if (/^\s*import\s/m.test(text)) throw new Error(`${name}: an embed module imports nothing`);
    writeFileSync(join(dir, 'embed', name), text);
  }
  if (!existsSync(FONTS)) {
    problems.add('crates/grid/examples/fonts/ is missing: run script/fetch-example-fonts.');
    return;
  }
  for (const family of readdirSync(FONTS).filter((name) => !name.startsWith('.'))) {
    const faces = readdirSync(join(FONTS, family));
    if (!faces.includes('OFL.txt')) problems.add(`fonts/${family}/ has no OFL.txt`);
    for (const face of faces) {
      // Iosevka's whole faces are 8.4 MB each: the examples fetch the subsets.
      if (family === 'iosevka' && face.endsWith('.ttf')) continue;
      if (face.endsWith('.ttf') || face === 'OFL.txt') {
        put(dir, `fonts/${family}/${face}`, join(FONTS, family, face));
      }
    }
  }
  const subsets = join(WWW, 'pkg', 'fonts', 'iosevka');
  for (const face of ['Iosevka-Regular.ttf', 'Iosevka-Bold.ttf', 'OFL.txt']) {
    if (existsSync(join(subsets, face))) put(dir, `pkg/fonts/iosevka/${face}`, join(subsets, face));
    else problems.add(`www/pkg/fonts/iosevka/${face} is missing: run make build`);
  }
  finish(
    dir,
    manifest(
      `${SCOPE}/${ELEMENT}`,
      'The <live-grid> element: a live gpui-kit-grid example in any page. Demo modules, not a JavaScript library.',
      { jsdelivr: 'embed/live-grid.js', unpkg: 'embed/live-grid.js' },
    ),
  );
}

/** The data folder to take files from, and whether it is a site build's. */
function dataSource() {
  const data = join(WWW, 'data');
  if (existsSync(data) && !lstatSync(data).isSymbolicLink()) return { dir: data, site: true };
  missing('www/data/ is not a site build’s folder (run make site): the data and its .license notes are not the site’s');
  return { dir: EXAMPLE_DATA, site: false };
}

/** One example's package. */
function appPackage(dir, app, data) {
  for (const file of [`${app}.js`, `${app}_bg.wasm`]) {
    const from = join(WWW, 'pkg', app, file);
    if (existsSync(from)) put(dir, `pkg/${app}/${file}`, from);
    else problems.add(`www/pkg/${app}/${file} is missing: run make site`);
  }
  const setup = join(WWW, app, 'setup.js');
  if (existsSync(setup)) {
    const text = readFileSync(setup, 'utf8');
    put(dir, `${app}/setup.js`, setup);
    for (const [, module] of text.matchAll(/from '\.\/([\w-]+\.js)'/g)) {
      put(dir, `${app}/${module}`, join(WWW, app, module));
    }
    // The orbit page fetches CelesTrak's elements in the browser, as on the site.
    if (text.includes('pkg/site.json')) {
      mkdirSync(join(dir, 'pkg'), { recursive: true });
      writeFileSync(join(dir, 'pkg', 'site.json'), `${JSON.stringify({ site: true })}\n`);
    }
    const names = existsSync(data.dir) ? readdirSync(data.dir) : [];
    for (const name of names) {
      if (name.endsWith('.license') || FORBIDDEN_DATA.some((pattern) => pattern.test(name))) continue;
      const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      if (!new RegExp(`(^|[^\\w.-])${escaped}(?![\\w.-])`).test(text)) continue;
      put(dir, `data/${name}`, join(data.dir, name));
      if (names.includes(`${name}.license`)) {
        put(dir, `data/${name}.license`, join(data.dir, `${name}.license`));
      } else {
        missing(`data/${name} has no ${name}.license (${app})`);
      }
    }
  }
  const licenses = join(WWW, 'licenses');
  for (const file of [`${app}.html`, 'LICENSE-APACHE.txt', 'LICENSE-LUCIDE.txt']) {
    if (existsSync(join(licenses, file))) put(dir, `licenses/${file}`, join(licenses, file));
    else missing(`www/licenses/${file} is missing (make site runs cargo-about)`);
  }
  finish(
    dir,
    manifest(
      `${SCOPE}/${ELEMENT}-${app}`,
      `The gpui-kit-grid ${app} example in WebAssembly, for the <live-grid> element (${SCOPE}/${ELEMENT}). Demo modules, not a JavaScript library.`,
    ),
  );
}

/** `npm pack --dry-run` in `dir`: its report. */
function dryRun(dir) {
  // Offline: a dry run needs no registry.
  const result = spawnSync('npm', ['pack', '--dry-run', '--json', '--ignore-scripts', '--offline'], {
    cwd: dir,
    env: { ...process.env, npm_config_update_notifier: 'false' },
    encoding: 'utf8',
    maxBuffer: 1 << 26,
  });
  if (result.status !== 0) throw new Error(`npm pack --dry-run in ${dir} failed:\n${result.stderr}`);
  return JSON.parse(result.stdout)[0];
}

function main() {
  const apps = JSON.parse(readFileSync(join(WWW, 'pkg', 'apps.json'), 'utf8'));
  const chosen = option('apps')?.split(',') ?? apps;
  for (const app of chosen) if (!apps.includes(app)) throw new Error(`--apps: no example ${app}`);

  const root = join(OUT, SCOPE);
  rmSync(root, { recursive: true, force: true });
  const elementDir = join(root, `${ELEMENT}@${VERSION}`);
  elementPackage(elementDir);
  const data = dataSource();
  const dirs = [elementDir];
  for (const app of chosen) {
    const dir = join(root, `${ELEMENT}-${app}@${VERSION}`);
    appPackage(dir, app, data);
    dirs.push(dir);
  }

  // No local path in what the packages carry (a module's panic locations).
  const home = Buffer.from(homedir());
  for (const dir of dirs) {
    for (const [path] of files(dir)) {
      if (readFileSync(path).includes(home)) {
        problems.add(`${posix(relative(OUT, path))} names the home folder: build with scripts/build-wasm.sh`);
      }
    }
  }

  const reports = dirs.map((dir) => ({ dir, report: dryRun(dir) }));
  for (const { report } of reports) {
    console.log(`\n${report.name}@${report.version}: ${report.entryCount} files, tarball ${mb(report.size)}, unpacked ${mb(report.unpackedSize)}`);
    for (const file of report.files) {
      console.log(`  ${mb(file.size).padStart(10)}  ${file.path}`);
      if (file.size > LARGE_FILE) warnings.add(`${report.name}: ${file.path} is ${mb(file.size)}, over 20 MB`);
    }
    if (report.unpackedSize > MAX_PACKAGE) problems.add(`${report.name}: unpacked ${mb(report.unpackedSize)}, over jsDelivr's 150 MB`);
  }
  const total = (key) => reports.reduce((sum, { report }) => sum + report[key], 0);
  console.log(`\nnpm-pack: ${reports.length} packages in ${posix(relative(REPO, root)) || root}, tarballs ${mb(total('size'))}, unpacked ${mb(total('unpackedSize'))}`);

  // The snippet, with the element's integrity (it covers the element only).
  const script = readFileSync(join(elementDir, 'embed', 'live-grid.js'));
  const integrity = `sha384-${createHash('sha384').update(script).digest('base64')}`;
  console.log('npm-pack: the snippet once published (jsDelivr):');
  console.log(`  <script type="module" src="https://cdn.jsdelivr.net/npm/${SCOPE}/${ELEMENT}@${VERSION}/embed/live-grid.js" integrity="${integrity}" crossorigin="anonymous"></script>`);

  for (const text of warnings) console.warn(`npm-pack: warning: ${text}`);
  if (DRAFT) console.warn('npm-pack: --draft: for the listing only, not for a release.');
  if (problems.size) {
    for (const text of problems) console.error(`npm-pack: ${text}`);
    process.exit(1);
  }
}

try {
  main();
} catch (error) {
  console.error(`npm-pack: ${error.message}`);
  process.exit(1);
}
