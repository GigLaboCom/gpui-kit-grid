// The licences of what the pages serve, into `www/licenses/` (a build
// artefact, not committed), for a site to publish with the pages:
//
// - `<app>.html`: the third-party notices of each example's module, the
//   crates compiled into `pkg/<app>/` and the texts of their licences, from
//   cargo-about (`about.toml`) over `Cargo.lock`, for the browser target;
// - `index.html`: this project's licence, the modules' notices, the fonts
//   and their SIL Open Font License texts (`fonts/<family>.txt`), the icons'
//   licence, and the data sources with their attributions;
// - `LICENSE-APACHE.txt` and `LICENSE-LUCIDE.txt`.
//
// It fails, writing nothing, when a crate's licence is not one `about.toml`
// accepts or cannot be read, and it checks cargo-about's result again: no
// GPL, LGPL or AGPL, nothing unknown.
//
// `--site` also prepares www/ for the public site: `data/` becomes a folder
// with only the files the site may serve (not CelesTrak's elements, which
// the orbit page then fetches from CelesTrak on a click; not the raw GTFS
// archive), each with its licence note next to it (`<file>.license`), and
// `pkg/site.json` switches the orbit page to that mode. Without it,
// `pkg/site.json` is removed (`build-wasm.sh` links `data/` again).
//
// cargo-about: CARGO_ABOUT, else `cargo-about` on the PATH, else the main
// checkout's `target/tools/bin/` (cargo install --root target/tools
// cargo-about --features cli). Without it the crates' notices are left out
// and the index says so; `--site` then fails. The notices are made again
// only when `Cargo.lock`, `about.toml` or the examples change.
//
// `node scripts/licenses.mjs [--site]`; `build-wasm.sh` runs it after a
// build (`SITE=1` for `--site`), `make licenses` alone.

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';

const CRATE = fileURLToPath(new URL('..', import.meta.url));
const REPO = join(CRATE, '..', '..');
const WWW = join(CRATE, 'www');
const OUT = join(WWW, 'licenses');
const FONTS = join(REPO, 'crates', 'grid', 'examples', 'fonts');
const DATA = join(REPO, 'crates', 'grid', 'examples', 'data');
const TARGET = 'wasm32-unknown-unknown';
const SITE = process.argv.includes('--site');

// Licences no module may carry, whatever `about.toml` says.
const FORBIDDEN = /GPL|SSPL|BUSL|Commons-Clause|LicenseRef|NOASSERTION|unknown/i;

// The data files the public site serves (`data/` with `--site`), each with
// its licence note. CelesTrak's elements are not among them: CelesTrak
// grants no redistribution, so the orbit page fetches them from CelesTrak.
const SITE_DATA = {
  'mvv-transit.tsv.gz': (feed) =>
    `data/mvv-transit.tsv.gz: the transit page's timetable.

Timetable data (c) Muenchner Verkehrs- und Tarifverbund GmbH (MVV),
licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/).
Source: "Soll-Fahrplandaten MVV-Gesamtnetz" (GTFS), ${feed.url}
published on the City of Munich's open data portal
(https://opendata.muenchen.de/).
Feed version ${feed.version}, retrieved ${feed.retrieved}.

Changed: gpui-kit-grid (script/fetch-transit-data) kept the city network
(U-Bahn, S-Bahn, tram and MVG's city buses) and compiled it into this file's
own tab-separated format. Scheduled, not real-time.
`,
  'mvv-transit-regional.tsv.gz': (feed) =>
    `data/mvv-transit-regional.tsv.gz: the transit page's timetable with the
regional lines (?regional).

Timetable data (c) Muenchner Verkehrs- und Tarifverbund GmbH (MVV),
licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/).
Source: "Soll-Fahrplandaten MVV-Gesamtnetz" (GTFS), ${feed.url}
published on the City of Munich's open data portal
(https://opendata.muenchen.de/).
Feed version ${feed.version}, retrieved ${feed.retrieved}.

Changed: gpui-kit-grid (script/fetch-transit-data) kept the city network and
the regional trains and buses and compiled them into this file's own
tab-separated format. Scheduled, not real-time.
`,
  'hsl-network.tsv.gz': (_, hsl) =>
    `data/hsl-network.tsv.gz: the transit page's network of Helsinki
(?city=helsinki): its lines, its stops and one stop pattern a line and
direction, for the map and the names of the stops.

(c) HSL ${hsl?.year ?? ''}, Helsinki Region Transport, licensed under CC BY 4.0
(https://creativecommons.org/licenses/by/4.0/).
Source: HSL's GTFS feed, ${hsl?.url ?? 'https://infopalvelut.storage.hsldev.com/gtfs/hsl.zip'}
Feed version ${hsl?.version ?? 'unknown'}, retrieved ${hsl?.retrieved ?? 'unknown'}.

Changed: gpui-kit-grid (script/fetch-transit-data --helsinki) kept the lines,
the stops and one stop pattern a line and direction, without trips, and
compiled them into this file's own tab-separated format.
`,
  'hsl-recording.txt.gz': (_, hsl) =>
    `data/hsl-recording.txt.gz: a minute of Helsinki's vehicle positions
(trams, metro, commuter trains, ferries), recorded by gpui-kit-grid's
transit example (TRANSIT_RECORD), which the page replays with
?source=replay or when HSL cannot be reached.

(c) HSL ${hsl?.year ?? ''}, Helsinki Region Transport, licensed under CC BY 4.0
(https://creativecommons.org/licenses/by/4.0/).
Source: HSL's high-frequency positioning (HFP), MQTT at wss://mqtt.hsl.fi/
(https://digitransit.fi/en/developers/apis/5-realtime-api/vehicle-positions/high-frequency-positioning/).

Changed: the messages are recorded as received, with the time each arrived;
the buses' left out.
`,
  'wiki-recording.jsonl': () =>
    `data/wiki-recording.jsonl: a few minutes of Wikimedia's recentchange
stream, recorded by gpui-kit-grid's wiki example (WIKI_RECORD).

The page titles in it are the wikis' content, by Wikimedia contributors,
licensed under CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/).
Source: Wikimedia EventStreams,
https://stream.wikimedia.org/v2/stream/recentchange

Changed: only the wiki, the edit's type, the bot flag, the size change, the
time, the title and its address were kept; user names and addresses were
never read. This recording is shared under the same licence,
CC BY-SA 4.0.
`,
  'ais-recording.txt': () =>
    `data/ais-recording.txt: a few minutes of AIS messages, recorded by
gpui-kit-grid's ships example (SHIPS_RECORD).

Source: Fintraffic / digitraffic.fi, license CC 4.0 BY
(https://creativecommons.org/licenses/by/4.0/).
Digitraffic marine traffic: https://www.digitraffic.fi/en/marine-traffic/

Changed: the messages are recorded as received, with the time each arrived.
`,
};

const escape = (text) =>
  String(text).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: CRATE,
    encoding: 'utf8',
    maxBuffer: 1 << 30,
    ...options,
  });
  if (result.error) throw result.error;
  return result;
}

// cargo-about, or null.
function cargoAbout() {
  const candidates = [process.env.CARGO_ABOUT, 'cargo-about'];
  const common = run('git', ['rev-parse', '--path-format=absolute', '--git-common-dir']);
  if (common.status === 0) {
    candidates.push(join(dirname(common.stdout.trim()), 'target', 'tools', 'bin', 'cargo-about'));
  }
  candidates.push(join(REPO, 'target', 'tools', 'bin', 'cargo-about'));
  for (const candidate of candidates.filter(Boolean)) {
    const probe = spawnSync(candidate, ['--version'], { encoding: 'utf8' });
    if (probe.status === 0) return candidate;
  }
  return null;
}

// The examples, in the switcher's order (`pkg/apps.json`), else the
// crates in `apps/`.
function apps() {
  const listed = join(WWW, 'pkg', 'apps.json');
  if (existsSync(listed)) return JSON.parse(readFileSync(listed, 'utf8'));
  return readdirSync(join(CRATE, 'apps')).sort();
}

// Each example's crates: those its package reaches through normal
// dependencies on the browser target, with the features cargo enables
// there (`cargo tree`: the workspace's metadata unifies features across
// targets and lists crates the module never compiles).
function cratesPerApp(names) {
  const result = run('cargo', ['metadata', '--format-version', '1', '--locked', '--offline']);
  if (result.status !== 0) throw new Error(`cargo metadata failed:\n${result.stderr}`);
  const metadata = JSON.parse(result.stdout);
  const packages = new Map(metadata.packages.map((p) => [p.id, p]));
  const byVersion = new Map(metadata.packages.map((p) => [`${p.name}@${p.version}`, p.id]));
  const perApp = {};
  for (const app of names) {
    const tree = run('cargo', [
      'tree',
      '--locked',
      '--offline',
      '--edges',
      'normal',
      '--target',
      TARGET,
      '--package',
      `gpui-kit-grid-web-${app}`,
      '--prefix',
      'none',
      '--format',
      '{p}',
    ]);
    if (tree.status !== 0) throw new Error(`cargo tree for ${app} failed:\n${tree.stderr}`);
    perApp[app] = new Set(
      tree.stdout
        .split('\n')
        .map((line) => line.match(/^(\S+) v(\S+)/))
        .filter(Boolean)
        .map(([, name, version]) => {
          const id = byVersion.get(`${name}@${version}`);
          if (!id) throw new Error(`cargo tree names ${name} ${version}, which cargo metadata lacks`);
          return id;
        }),
    );
  }
  const lucide = metadata.packages.find((p) => p.name === 'gpui-kit-assets');
  return { perApp, packages, lucide: lucide && join(dirname(lucide.manifest_path), 'LICENSE-LUCIDE') };
}

// cargo-about's JSON for the whole workspace, made again only when its
// inputs change; `null` without cargo-about.
function aboutJson(about) {
  const inputs = createHash('sha256');
  for (const file of ['Cargo.lock', 'Cargo.toml', 'about.toml']) {
    inputs.update(readFileSync(join(CRATE, file)));
  }
  for (const app of readdirSync(join(CRATE, 'apps')).sort()) {
    inputs.update(app).update(readFileSync(join(CRATE, 'apps', app, 'Cargo.toml')));
  }
  const key = inputs.digest('hex');
  const cache = join(CRATE, 'target', 'licenses');
  const cached = join(cache, `about-${key}.json`);
  if (existsSync(cached)) return JSON.parse(readFileSync(cached, 'utf8'));
  if (!about) return null;
  const result = run(about, [
    'generate',
    '--format',
    'json',
    '--workspace',
    '--locked',
    '--offline',
    '--fail',
    '-c',
    join(CRATE, 'about.toml'),
    '-m',
    join(CRATE, 'Cargo.toml'),
  ]);
  if (result.status !== 0) {
    throw new Error(`cargo-about refused the crates' licences:\n${result.stderr}`);
  }
  const json = JSON.parse(result.stdout);
  rmSync(cache, { recursive: true, force: true });
  mkdirSync(cache, { recursive: true });
  writeFileSync(cached, result.stdout);
  return json;
}

// Every crate of `ids` with the licences cargo-about chose for it, the
// licence texts that cover them (identical texts once), and the check.
function notices(about, ids, packages) {
  const accepted = new Set(
    [...readFileSync(join(CRATE, 'about.toml'), 'utf8').matchAll(/^\s*"([^"]+)",?$/gm)].map(
      (match) => match[1],
    ),
  );
  const chosen = new Map(); // crate id -> Set of licence ids
  const texts = new Map(); // normalised text -> { id, name, text, crates }
  for (const licence of about.licenses) {
    if (FORBIDDEN.test(licence.id) || !accepted.has(licence.id)) {
      throw new Error(`forbidden licence ${licence.id} (${licence.name})`);
    }
    const users = licence.used_by.map((use) => use.crate.id).filter((id) => ids.has(id));
    if (users.length === 0) continue;
    for (const id of users) {
      if (!chosen.has(id)) chosen.set(id, new Set());
      chosen.get(id).add(licence.id);
    }
    const norm = licence.text.replace(/\s+/g, ' ').trim();
    if (!texts.has(norm)) {
      texts.set(norm, { id: licence.id, name: licence.name, text: licence.text, crates: new Set() });
    }
    for (const id of users) texts.get(norm).crates.add(id);
  }
  const missing = [...ids].filter((id) => !chosen.has(id));
  if (missing.length > 0) {
    const names = missing.map((id) => `${packages.get(id).name} ${packages.get(id).version}`);
    throw new Error(`no licence found for ${names.join(', ')}`);
  }
  const counts = {};
  for (const set of chosen.values()) {
    const label = [...set].sort().join(' AND ');
    counts[label] = (counts[label] ?? 0) + 1;
  }
  return { chosen, texts: [...texts.values()], counts };
}

const crateLabel = (p) => `${p.name} ${p.version}`;
// Where a crate's source is: crates.io for a published crate, else its
// repository.
function crateLink(p) {
  if (p.source?.startsWith('registry+')) {
    return `https://crates.io/crates/${encodeURIComponent(p.name)}/${encodeURIComponent(p.version)}`;
  }
  return p.repository ?? p.homepage ?? null;
}

const STYLE = `
  :root { color-scheme: light dark; --bg: #ffffff; --fg: #1d2026; --muted: #5e6470; --rule: #d9dce1; --link: #1d5b79; --code: #f4f5f7; }
  @media (prefers-color-scheme: dark) { :root { --bg: #16191e; --fg: #d1d4dc; --muted: #8d94a3; --rule: #2a2f38; --link: #8cc4e6; --code: #1d2128; } }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.55 system-ui, sans-serif; }
  main { max-width: 860px; margin: 0 auto; padding: 32px 16px 64px; }
  h1 { font-size: 24px; margin: 0 0 8px; }
  h2 { font-size: 18px; margin: 36px 0 8px; padding-top: 12px; border-top: 1px solid var(--rule); }
  h3 { font-size: 15px; margin: 24px 0 4px; }
  p, li { color: var(--fg); }
  .muted { color: var(--muted); }
  a { color: var(--link); }
  table { border-collapse: collapse; width: 100%; font-size: 14px; }
  th, td { text-align: left; vertical-align: top; padding: 6px 8px; border-bottom: 1px solid var(--rule); }
  pre { background: var(--code); padding: 12px; overflow-x: auto; white-space: pre-wrap; font-size: 12px; border-radius: 6px; }
  .crates { font-size: 13px; color: var(--muted); }
`;

function page(title, body) {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>${escape(title)}</title>
    <link rel="icon" href="data:," />
    <style>${STYLE}</style>
  </head>
  <body>
    <main>
${body}
    </main>
  </body>
</html>
`;
}

function appPage(app, result, ids, packages) {
  const crates = [...ids].map((id) => packages.get(id)).sort((a, b) => a.name.localeCompare(b.name));
  const rows = crates
    .map((p) => {
      const link = crateLink(p);
      const name = link ? `<a href="${escape(link)}">${escape(crateLabel(p))}</a>` : escape(crateLabel(p));
      return `<tr><td>${name}</td><td>${escape(p.license ?? '')}</td><td>${escape([...result.chosen.get(p.id)].sort().join(', '))}</td></tr>`;
    })
    .join('\n');
  const mpl = crates.filter((p) => result.chosen.get(p.id).has('MPL-2.0'));
  const summary = Object.entries(result.counts)
    .sort((a, b) => b[1] - a[1])
    .map(([licence, count]) => `${escape(licence)} ${count}`)
    .join(', ');
  const texts = result.texts
    .sort((a, b) => a.id.localeCompare(b.id) || b.crates.size - a.crates.size)
    .map((entry) => {
      const users = [...entry.crates]
        .map((id) => escape(crateLabel(packages.get(id))))
        .sort()
        .join(', ');
      return `<h3>${escape(entry.name)} (${escape(entry.id)})</h3>
<p class="crates">Used by ${users}</p>
<pre>${escape(entry.text.trim())}</pre>`;
    })
    .join('\n');
  return page(
    `Third-party notices: ${app}`,
    `<p><a href="./">Licenses</a></p>
<h1>Third-party notices: the ${escape(app)} example</h1>
<p>The ${escape(app)} page runs one WebAssembly module, <code>pkg/${escape(app)}/${escape(app)}_bg.wasm</code>, and its JavaScript glue, <code>pkg/${escape(app)}/${escape(app)}.js</code>. They are compiled from gpui-kit-grid (Apache-2.0) and the ${crates.length} crates below, for <code>${TARGET}</code>, as <code>Cargo.lock</code> pins them. Each crate is used under the licence named in the third column, chosen from its own licence expression; the full texts follow.</p>
<p class="muted">${summary}.</p>
${
  mpl.length > 0
    ? `<p>${mpl.map((p) => escape(crateLabel(p))).join(' and ')} ${mpl.length > 1 ? 'are' : 'is'} under the Mozilla Public License 2.0 and compiled unchanged. Their source code is available from crates.io: ${mpl
        .map((p) => `<a href="${escape(crateLink(p))}">${escape(crateLabel(p))}</a>`)
        .join(', ')}.</p>`
    : ''
}
<table>
<thead><tr><th>Crate</th><th>Licence expression</th><th>Used under</th></tr></thead>
<tbody>
${rows}
</tbody>
</table>
<h2>Licence texts</h2>
${texts}`,
  );
}

// The fonts present (`fonts/<family>/OFL.txt`), each with its copyright
// line and Reserved Font Names.
function fonts() {
  if (!existsSync(FONTS)) return [];
  return readdirSync(FONTS)
    .filter((family) => existsSync(join(FONTS, family, 'OFL.txt')))
    .sort()
    .map((family) => {
      const text = readFileSync(join(FONTS, family, 'OFL.txt'), 'utf8');
      const copyright = text.split('\n').find((line) => /copyright/i.test(line))?.trim() ?? '';
      const reserved = [...copyright.matchAll(/Reserved Font Name[s]? ((?:"[^"]+"(?:,? (?:and )?)?)+)/g)]
        .map((match) => match[1])
        .join(', ');
      const faces = readdirSync(join(FONTS, family)).filter((file) => file.endsWith('.ttf'));
      return { family, text, copyright, reserved, faces };
    });
}

const FAMILY_NAMES = {
  'barlow-semi-condensed': 'Barlow Semi Condensed',
  'ibm-plex-mono': 'IBM Plex Mono',
  'ibm-plex-sans': 'IBM Plex Sans',
  inter: 'Inter',
  iosevka: 'Iosevka',
  'jetbrains-mono': 'JetBrains Mono',
  manrope: 'Manrope',
};

// A compiled timetable's source line: the feed's address, version and
// retrieval date (Munich's by default, `hsl-network` for Helsinki's).
function transitFeed(name = 'mvv-transit') {
  for (const file of [`${name}.tsv.gz`, `${name}.tsv`]) {
    const path = join(DATA, file);
    if (!existsSync(path)) continue;
    const bytes = readFileSync(path);
    const text = (file.endsWith('.gz') ? gunzipSync(bytes) : bytes).toString('utf8');
    const source = text.split('\n').find((line) => line.startsWith('source\t'));
    if (!source) continue;
    const [, publisher, licence, url, version, retrieved] = source.split('\t');
    return { publisher, licence, url, version, retrieved, year: retrieved?.slice(0, 4) };
  }
  return null;
}

function indexPage(names, have, fontList, feed) {
  const modules = names
    .map((app) =>
      have
        ? `<li><a href="./${escape(app)}.html">${escape(app)}</a></li>`
        : `<li>${escape(app)}: not generated in this build (cargo-about was not found)</li>`,
    )
    .join('\n');
  const fontRows = fontList
    .map(
      (font) =>
        `<tr><td>${escape(FAMILY_NAMES[font.family] ?? font.family)}</td><td>${escape(font.copyright)}</td><td>${escape(font.reserved || 'none')}</td><td><a href="./fonts/${escape(font.family)}.txt">SIL OFL 1.1</a></td></tr>`,
    )
    .join('\n');
  const feedLine = feed
    ? `feed version ${escape(feed.version)}, retrieved ${escape(feed.retrieved)}`
    : 'feed version and retrieval date as the page shows them';
  const hsl = transitFeed('hsl-network');
  const hslLine = hsl
    ? `, feed version ${escape(hsl.version)}, retrieved ${escape(hsl.retrieved)}`
    : '';
  return page(
    'Licenses · gpui-kit-grid in the browser',
    `<p><a href="../">Examples</a></p>
<h1>Licenses</h1>
<p>What the example pages serve, under which licence, and the attributions those licences ask for. This page lists facts, not legal advice.</p>

<h2>This project</h2>
<p>gpui-kit-grid, its examples and these pages are licensed under the <a href="./LICENSE-APACHE.txt">Apache License 2.0</a>. They are built on gpui-kit by Longbridge and on GPUI by Zed Industries, both Apache-2.0; their notices are part of each module's notices below.</p>

<h2>Example modules</h2>
<p>Each example is one WebAssembly module with its JavaScript glue. The third-party notices of each list the crates compiled into it and the texts of their licences:</p>
<ul>
${modules}
</ul>

<h2>Fonts</h2>
<p>The pages fetch their themes' fonts from <code>fonts/</code>, unmodified, except Iosevka: they fetch a subset of its faces from <code>pkg/fonts/iosevka/</code> (Latin, Greek, Cyrillic and common symbols only, made with HarfBuzz's hb-subset), a Modified Version under the same licence; Iosevka reserves no font name. Every family is licensed under the SIL Open Font License 1.1; the licence text is served next to the faces (<code>fonts/&lt;family&gt;/OFL.txt</code>, <code>pkg/fonts/iosevka/OFL.txt</code>) and here.</p>
<table>
<thead><tr><th>Family</th><th>Copyright</th><th>Reserved Font Name</th><th>Licence</th></tr></thead>
<tbody>
${fontRows}
</tbody>
</table>

<h2>Icons</h2>
<p>The icons come from gpui-kit's asset crate, which takes them from <a href="https://lucide.dev">Lucide</a> (ISC License); the Lucide icons derived from Feather are under the MIT License. Both notices: <a href="./LICENSE-LUCIDE.txt">LICENSE-LUCIDE</a>.</p>

<h2>Data</h2>
<table>
<thead><tr><th>Page</th><th>Source and licence</th><th>What this site serves</th></tr></thead>
<tbody>
<tr><td>transit (Munich)</td><td>Timetable data © Münchner Verkehrs- und Tarifverbund GmbH (MVV), <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>: “Soll-Fahrplandaten MVV-Gesamtnetz” (GTFS) from the <a href="https://opendata.muenchen.de/">Munich open data portal</a>, ${feedLine}. Changed: the city network, and with the regional lines, kept and compiled into this project's format. Scheduled, not real-time.</td><td><code>data/mvv-transit.tsv.gz</code> and <code>data/mvv-transit-regional.tsv.gz</code>, each with its <code>.license</code></td></tr>
<tr><td>transit (Helsinki)</td><td>© HSL (Helsinki Region Transport), <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>: vehicle positions from <a href="https://digitransit.fi/en/developers/apis/5-realtime-api/vehicle-positions/high-frequency-positioning/">HSL's high-frequency positioning</a>, live from HSL to your browser; the network from HSL's GTFS feed${hslLine}. Changed: the network's lines, stops and one stop pattern a line and direction kept and compiled into this project's format.</td><td><code>data/hsl-network.tsv.gz</code>, and <code>data/hsl-recording.txt.gz</code>, a recording, each with its <code>.license</code></td></tr>
<tr><td>wiki</td><td><a href="https://wikitech.wikimedia.org/wiki/Event_Platform/EventStreams_HTTP_Service">Wikimedia EventStreams</a>; titles and pages are the wikis' content by Wikimedia contributors, <a href="https://creativecommons.org/licenses/by-sa/4.0/">CC BY-SA 4.0</a>. The live stream goes from your browser to Wikimedia.</td><td><code>data/wiki-recording.jsonl</code>, a recording shared under CC BY-SA 4.0, with <code>data/wiki-recording.jsonl.license</code></td></tr>
<tr><td>ships</td><td>Source: <a href="https://www.digitraffic.fi/en/marine-traffic/">Fintraffic / digitraffic.fi</a>, license <a href="https://creativecommons.org/licenses/by/4.0/">CC 4.0 BY</a>. The live data goes from Digitraffic to your browser.</td><td><code>data/ais-recording.txt</code>, a recording, with <code>data/ais-recording.txt.license</code></td></tr>
<tr><td>orbit</td><td>Orbital elements from <a href="https://celestrak.org">CelesTrak</a>, derived from the US Space Force's public catalog. Positions are computed with SGP4 and are approximate.</td><td>Nothing: on a click, your browser downloads the elements from celestrak.org and keeps them for two hours.</td></tr>
<tr><td>quotes, desk, broker</td><td>Simulated. Company names and ticker symbols identify real companies and funds, but every price, trade, order, chart and headline is made up by the page's simulator: not market data, not investment advice, and not affiliated with or endorsed by any company named.</td><td>Nothing</td></tr>
<tr><td>ops</td><td>Simulated. Every region, data center, hall, rack, host, service and alert is made up by the page's simulator.</td><td>Nothing</td></tr>
</tbody>
</table>`,
  );
}

// `data/` for the public site: links to the files it may serve, each with
// its licence note.
function stageSiteData(feed) {
  const data = join(WWW, 'data');
  rmSync(data, { recursive: true, force: true });
  mkdirSync(data);
  for (const [file, note] of Object.entries(SITE_DATA)) {
    if (!existsSync(join(DATA, file))) {
      console.error(`licenses: no ${file} in crates/grid/examples/data/: its page will say so.`);
      continue;
    }
    symlinkSync(join('..', '..', '..', 'grid', 'examples', 'data', file), join(data, file));
    writeFileSync(
      join(data, `${file}.license`),
      note(
        feed ?? { url: '(unknown)', version: '(unknown)', retrieved: '(unknown)' },
        transitFeed('hsl-network'),
      ),
    );
  }
}

function main() {
  const names = apps();
  const about = cargoAbout();
  const { perApp, packages, lucide } = cratesPerApp(names);
  const json = aboutJson(about);
  if (!json && SITE) {
    throw new Error('cargo-about not found: the site needs the notices (see scripts/licenses.mjs).');
  }
  const pages = {};
  for (const app of json ? names : []) {
    const result = notices(json, perApp[app], packages);
    pages[app] = appPage(app, result, perApp[app], packages);
    const counts = Object.entries(result.counts)
      .sort((a, b) => b[1] - a[1])
      .map(([licence, count]) => `${licence} ${count}`)
      .join(', ');
    console.log(`licenses: ${app}: ${perApp[app].size} crates; ${counts}; forbidden: none`);
  }
  if (!json) console.error('licenses: cargo-about not found: the modules\' notices are left out.');

  const fontList = fonts();
  const feed = transitFeed();
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(join(OUT, 'fonts'), { recursive: true });
  for (const [app, html] of Object.entries(pages)) writeFileSync(join(OUT, `${app}.html`), html);
  writeFileSync(join(OUT, 'index.html'), indexPage(names, Boolean(json), fontList, feed));
  copyFileSync(join(REPO, 'LICENSE-APACHE'), join(OUT, 'LICENSE-APACHE.txt'));
  if (lucide && existsSync(lucide)) copyFileSync(lucide, join(OUT, 'LICENSE-LUCIDE.txt'));
  for (const font of fontList) writeFileSync(join(OUT, 'fonts', `${font.family}.txt`), font.text);

  const marker = join(WWW, 'pkg', 'site.json');
  if (SITE) {
    stageSiteData(feed);
    mkdirSync(dirname(marker), { recursive: true });
    writeFileSync(marker, `${JSON.stringify({ site: true })}\n`);
    console.log('licenses: www/ prepared for the site: data/ holds only what it may serve.');
  } else {
    rmSync(marker, { force: true });
  }
  console.log(`licenses: www/licenses/ (${Object.keys(pages).length} modules, ${fontList.length} font families)`);
}

try {
  main();
} catch (error) {
  console.error(`licenses: ${error.message}`);
  process.exit(1);
}
