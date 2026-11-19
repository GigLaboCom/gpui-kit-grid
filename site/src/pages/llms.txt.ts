import type { APIRoute } from 'astro';
import { getCollection } from 'astro:content';
import { EXAMPLES, sourceUrl } from '../lib/examples';
import { withBase } from '../lib/links';
import { SIDEBAR } from '../lib/sidebar';

/*
 * `/llms.txt` (https://llmstxt.org/): what the crate is and where its pages
 * are, for language models and the tools built on them. It is built with the
 * site from what the site already says, so the two never differ: the pages'
 * titles and descriptions in the sidebar's order and groups, the install
 * lines of Getting started, and the demos of the examples' list.
 */

/** Sidebar pages about the project rather than the crate: last, under `Optional`. */
const OPTIONAL = new Set(['products', 'roadmap', 'sponsor']);

const SUMMARY = `\
# gpui-kit-grid

> A live data grid for Rust applications built with gpui-kit on GPUI, the UI framework of the Zed editor: rows addressed by key and written from any thread, every change highlighted as it arrives, and sorting, filtering, grouping, aggregates and trees computed in the background.

gpui-kit-grid is a crate on crates.io under the Apache-2.0 licence. It depends on gpui-kit alone: an application reaches GPUI through gpui-kit and does not list a GPUI crate of its own. It is not gpui-kit's \`DataTable\`, which is a separate component with another API.`;

const TYPES = `\
The main types: \`LiveGrid\` is the element an application places; \`LiveGridState\` the grid's state and view; \`GridSchema\` the columns it shows, of a \`GridColumns\` set (usually an enum); \`GridRow\` a row, an immutable value with a key; \`GridStore\` the rows and \`GridWriter\` what writes them from any thread; \`LiveGridDelegate\` the presentation and command hooks; \`LiveGridEvent\` what the grid reports. The \`decimal\` and \`derive\` features are off by default.

The library also builds for \`wasm32-unknown-unknown\`: the demos below are examples compiled to WebAssembly and drawn with WebGPU, or WebGL 2 without it.`;

type Entry = string | { slug: string } | { link: string } | { items: readonly (string | { slug: string })[] };

function slugOf(item: Entry): string | undefined {
  if (typeof item === 'string') return item;
  return 'slug' in item ? item.slug : undefined;
}

/** Getting started's install lines: the TOML block through the compatibility table under it. */
function installOf(body: string | undefined): string {
  const section = body?.match(/^## Install\n([\s\S]*?)(?=^## )/m)?.[1] ?? '';
  const lines = section.match(/^```toml\n[\s\S]*?^```\n[\s\S]*?(?:^\|.*\n)+/m)?.[0];
  if (!lines) throw new Error('llms.txt: no TOML block and table under "## Install" in start.md');
  return lines.trimEnd();
}

export const GET: APIRoute = async ({ site }) => {
  if (!site) throw new Error('llms.txt: `site` is not set in astro.config.mjs');
  const pages = new Map((await getCollection('docs')).map((entry) => [entry.id, entry]));
  const url = (path: string) => new URL(withBase(path), site).href;
  const line = (title: string, href: string, note?: string) => `- [${title}](${href})${note ? `: ${note}` : ''}`;
  const page = (slug: string) => {
    const entry = pages.get(slug);
    if (!entry) throw new Error(`llms.txt: the sidebar names ${slug}, which is not a page`);
    return line(entry.data.title, url(`/${slug}/`), entry.data.description);
  };
  const section = (title: string, lines: string[]) => `## ${title}\n\n${lines.join('\n')}`;

  const docs: string[] = [];
  const optional: string[] = [];
  const groups: string[] = [];
  for (const item of SIDEBAR as readonly (Entry & { label?: string })[]) {
    if (typeof item !== 'string' && 'items' in item) {
      groups.push(section(item.label ?? '', item.items.map((inner) => page(slugOf(inner)!))));
      continue;
    }
    const slug = slugOf(item);
    // The sidebar's links, rustdoc and crates.io, are under `API`.
    if (slug) (OPTIONAL.has(slug) ? optional : docs).push(page(slug));
  }

  const api = [
    line('API reference on docs.rs', 'https://docs.rs/gpui-kit-grid', 'rustdoc of the released crate, with the decimal and derive features'),
    line('API reference on this site', url('/api/'), 'the same rustdoc, built with this site'),
    line('crates.io', 'https://crates.io/crates/gpui-kit-grid', 'releases of gpui-kit-grid and gpui-kit-grid-macros'),
    line('Repository', 'https://github.com/GigLaboCom/gpui-kit-grid', 'source, issues and releases'),
    line('Source of the examples', sourceUrl('crates/grid/examples'), 'runnable desktop applications, `cargo run --release -p gpui-kit-grid --example <name>` from a checkout'),
  ];

  const demos = EXAMPLES.flatMap(({ title, description, demo }) =>
    demo ? [line(title, url(`/demo/${demo.app}/${demo.query ? `?${demo.query}` : ''}`), description)] : [],
  );

  const text = [
    SUMMARY,
    `Install it with the gpui-kit release it is built against:\n\n${installOf(pages.get('start')?.body)}`,
    TYPES,
    section('Docs', docs),
    ...groups,
    section('Demos in the browser', demos),
    section('API', api),
    section('Optional', optional),
  ].join('\n\n');

  return new Response(`${text}\n`, { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
};
