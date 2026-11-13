// A Markdown plugin that puts compiled code and repository documents on the
// site's pages at build time, for Astro's Sätteri processor:
//
// - a code fence with `src="path#region"` and an empty body takes the
//   region's code (`./regions.ts`): ```rust src="crates/grid/examples/quickstart.rs#row"
// - a `rust` fence without `src` must say `ignore` (a signature quoted in
//   prose, an outline); otherwise the build fails, so code on a page always
//   comes from a file that compiles;
// - `<!-- include ROADMAP.md -->` in a Markdown page is replaced by that
//   document from the repository, without its first heading, its relative
//   links pointing into the repository. One source: the page follows the
//   file.
//
// `test/regions.test.ts` checks the same rules without a build.

import { readFileSync } from 'node:fs';
import { join, posix } from 'node:path';
import { fileURLToPath } from 'node:url';
import { metaValue, metaWords, readRegion } from './regions.ts';

/** The repository's address on its host. */
export const REPOSITORY = 'https://github.com/GigLaboCom/gpui-kit-grid';

const INCLUDE = /^<!--\s*include\s+(\S+)\s*-->$/;

/** The parts of an mdast `code` node this plugin reads and writes. */
export interface CodeFence {
  lang?: string | null;
  meta?: string | null;
  value: string;
}

/**
 * What a fence becomes: its region's code with `src` taken out of the meta,
 * or the fence itself with `ignore` taken out. Throws, naming the page, for
 * a fence with `src` and a body, a missing region, or a `rust` fence with
 * neither `src` nor `ignore`.
 */
export function fillFence(fence: CodeFence, root: string, page: string): CodeFence {
  const words = metaWords(fence.meta);
  const src = metaValue(words, 'src');
  const rest = words.filter((word) => !word.startsWith('src=') && word !== 'ignore');
  const meta = rest.join(' ') || null;
  if (src) {
    if (fence.value.trim() !== '') {
      throw new Error(`${page}: the fence with src="${src}" has a body; its code comes from the region`);
    }
    return { lang: fence.lang, meta, value: readRegion(root, src).code };
  }
  if (fence.lang === 'rust' && !words.includes('ignore')) {
    const first = JSON.stringify(fence.value.split('\n')[0]);
    throw new Error(`${page}: a rust fence needs src="path#region" (code that compiles) or "ignore": ${first}`);
  }
  return { lang: fence.lang, meta, value: fence.value };
}

/** Whether a Markdown link target points into the repository. */
function isRelativeLink(url: string): boolean {
  return !/^([a-z][a-z0-9+.-]*:|#|\/|<)/i.test(url);
}

/**
 * A repository document as Markdown for a page: without its first `# `
 * heading (the page has its title), and with relative links and images
 * pointing at the file in the repository.
 */
export function includedMarkdown(root: string, path: string): string {
  const text = readFileSync(join(root, path), 'utf8');
  const withoutTitle = text.replace(/^# .*\n+/m, '');
  return withoutTitle.replace(/(\]\()([^)\s]+)/g, (whole, open: string, url: string) => {
    if (!isRelativeLink(url)) return whole;
    const target = posix.normalize(posix.join(posix.dirname(path), url));
    return `${open}${REPOSITORY}/blob/main/${target}`;
  });
}

// The few shapes of Sätteri's visitor API this plugin uses.
interface Node {
  type: string;
  value?: string;
  lang?: string | null;
  meta?: string | null;
}

interface Context {
  readonly fileURL: URL | undefined;
  replaceNode(node: Readonly<Node>, content: { raw: string; mdxExpressions?: boolean }): void;
}

function pageOf(context: Context): string {
  return context.fileURL ? fileURLToPath(context.fileURL) : 'a page';
}

/** The plugin for `satteri({ mdastPlugins: [markdownRegions({ root })] })`. */
export default function markdownRegions(options: { root: string }) {
  const { root } = options;
  return {
    name: 'gpui-kit-grid-regions',
    code(node: Readonly<Node>, context: Context) {
      const filled = fillFence({ lang: node.lang, meta: node.meta, value: node.value ?? '' }, root, pageOf(context));
      if (filled.value === node.value && filled.meta === (node.meta ?? null)) return undefined;
      return { type: 'code' as const, lang: filled.lang ?? null, meta: filled.meta, value: filled.value };
    },
    html(node: Readonly<Node>, context: Context) {
      const match = INCLUDE.exec((node.value ?? '').trim());
      if (match) context.replaceNode(node, { raw: includedMarkdown(root, match[1]), mdxExpressions: false });
    },
  };
}
