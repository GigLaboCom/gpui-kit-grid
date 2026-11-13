// Code on the pages and in the README comes from regions of compiled files
// (`src/lib/regions.ts`). These checks run without a site build:
//
// - every region a page names exists, and its fence has no body of its own;
// - a `rust` fence without a source says `ignore`;
// - every README snippet that names a region is that region, character for
//   character, and the README's other `rust` fences say `ignore`;
// - every region marker in the examples is well formed;
// - every `<!-- include … -->` names a file that exists.

import { describe, expect, test } from 'bun:test';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { REPOSITORY, fillFence, includedMarkdown } from '../src/lib/markdown-regions.ts';
import { dedent, metaValue, metaWords, readRegion, regionsOf } from '../src/lib/regions.ts';

const SITE = join(import.meta.dir, '..');
const ROOT = join(SITE, '..');
const DOCS = join(SITE, 'src/content/docs');

function files(dir: string, keep: (name: string) => boolean): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      return ['node_modules', 'target', 'fonts', 'data'].includes(entry.name) ? [] : files(path, keep);
    }
    return keep(entry.name) ? [path] : [];
  });
}

interface Fence {
  line: number;
  lang: string;
  words: string[];
  body: string;
  /** The nearest non-blank line before the fence. */
  before: string;
}

/** The fenced code blocks of a Markdown text (``` or ~~~, any indent). */
function fences(text: string): Fence[] {
  const lines = text.split('\n');
  const found: Fence[] = [];
  for (let ix = 0; ix < lines.length; ix++) {
    const open = /^(\s*)(`{3,}|~{3,})\s*([^\s`]*)\s*(.*)$/.exec(lines[ix]);
    if (!open) continue;
    const [, indent, marker, lang, meta] = open;
    const body: string[] = [];
    let end = ix + 1;
    while (end < lines.length && !lines[end].trim().startsWith(marker)) {
      body.push(lines[end].slice(Math.min(indent.length, /^\s*/.exec(lines[end])![0].length)));
      end++;
    }
    let prev = ix - 1;
    while (prev >= 0 && lines[prev].trim() === '') prev--;
    found.push({
      line: ix + 1,
      lang,
      words: metaWords(meta),
      body: body.join('\n'),
      before: prev >= 0 ? lines[prev].trim() : '',
    });
    ix = end;
  }
  return found;
}

const pages = files(DOCS, (name) => /\.mdx?$/.test(name));

describe('pages', () => {
  test('there are pages to check', () => {
    expect(pages.length).toBeGreaterThan(10);
  });

  for (const page of pages) {
    const name = relative(SITE, page);
    const text = readFileSync(page, 'utf8');

    test(`${name}: code comes from regions`, () => {
      for (const fence of fences(text)) {
        const where = `${name}:${fence.line}`;
        const src = metaValue(fence.words, 'src');
        if (src) {
          expect(fence.body.trim(), `${where}: a fence with src has no body`).toBe('');
          expect(() => readRegion(ROOT, src), where).not.toThrow();
        } else if (fence.lang === 'rust') {
          expect(fence.words, `${where}: a rust fence without src says "ignore"`).toContain('ignore');
        }
      }
    });

    test(`${name}: includes exist`, () => {
      for (const match of text.matchAll(/<!--\s*include\s+(\S+)\s*-->/g)) {
        expect(existsSync(join(ROOT, match[1])), `${name}: ${match[1]}`).toBe(true);
      }
    });
  }
});

describe('README', () => {
  const readme = readFileSync(join(ROOT, 'README.md'), 'utf8');
  const all = fences(readme);

  test('snippets that name a region are that region', () => {
    const named = all.filter((fence) => /^<!--\s*region\s/.test(fence.before));
    expect(named.length).toBeGreaterThan(0);
    for (const fence of named) {
      const reference = /^<!--\s*region\s+(\S+)\s*-->$/.exec(fence.before)?.[1];
      expect(reference, `README.md:${fence.line}: <!-- region path#name -->`).toBeDefined();
      expect(fence.body, `README.md:${fence.line}: differs from ${reference}`).toBe(
        readRegion(ROOT, reference!).code,
      );
    }
  });

  test('other rust fences say ignore', () => {
    for (const fence of all) {
      if (fence.lang !== 'rust' || /^<!--\s*region\s/.test(fence.before)) continue;
      expect(fence.words, `README.md:${fence.line}`).toContain('ignore');
    }
  });

  test('links stay out of the local design notes', () => {
    expect(readme).not.toMatch(/\]\(\.?\/?docs\//);
  });
});

describe('regions in the sources', () => {
  const sources = files(join(ROOT, 'crates'), (name) => name.endsWith('.rs')).filter((path) =>
    readFileSync(path, 'utf8').includes('#region'),
  );

  test('the examples have regions', () => {
    expect(sources.some((path) => path.endsWith('quickstart.rs'))).toBe(true);
    expect(sources.some((path) => path.endsWith('cookbook.rs'))).toBe(true);
  });

  for (const path of sources) {
    test(`${relative(ROOT, path)}: markers are well formed`, () => {
      expect(() => regionsOf(readFileSync(path, 'utf8'), path)).not.toThrow();
    });
  }
});

describe('the parts', () => {
  test('regionsOf reads nested regions without their markers', () => {
    const source = [
      'fn main() {',
      '    // #region outer',
      '    let a = 1;',
      '    // #region inner',
      '    let b = 2;',
      '    // #endregion inner',
      '    // #endregion outer',
      '}',
    ].join('\n');
    const regions = regionsOf(source);
    expect(regions.get('outer')?.code).toBe('let a = 1;\nlet b = 2;');
    expect(regions.get('inner')?.code).toBe('let b = 2;');
  });

  test('regionsOf refuses broken markers', () => {
    expect(() => regionsOf('// #region a\nx')).toThrow(/not closed/);
    expect(() => regionsOf('// #endregion a')).toThrow(/without/);
    expect(() => regionsOf('// #region a\nx\n// #endregion a\n// #region a\ny\n// #endregion a')).toThrow(
      /twice/,
    );
    expect(() => regionsOf('// #region a\n\n// #endregion a')).toThrow(/empty/);
  });

  test('dedent keeps relative indentation', () => {
    expect(dedent('\n    a\n      b\n\n    c\n')).toBe('a\n  b\n\nc');
  });

  test('metaWords keeps quoted values whole', () => {
    const words = metaWords('src="a b#c" ignore title="x"');
    expect(words).toEqual(['src="a b#c"', 'ignore', 'title="x"']);
    expect(metaValue(words, 'src')).toBe('a b#c');
    expect(metaValue(['src=a#b'], 'src')).toBe('a#b');
  });

  test('fillFence fills a fence from its region', () => {
    const fence = { lang: 'rust', meta: 'src="crates/grid/examples/quickstart.rs#row" wrap', value: '' };
    const filled = fillFence(fence, ROOT, 'page.md');
    expect(filled.value).toContain('impl GridRow for Trade');
    expect(filled.meta).toBe('wrap');
  });

  test('fillFence refuses rust without a source, a body beside src, and a missing region', () => {
    expect(() => fillFence({ lang: 'rust', value: 'fn main() {}' }, ROOT, 'page.md')).toThrow(/needs src/);
    expect(fillFence({ lang: 'rust', meta: 'ignore', value: 'fn main() {}' }, ROOT, 'page.md').meta).toBeNull();
    expect(fillFence({ lang: 'sh', value: 'ls' }, ROOT, 'page.md').value).toBe('ls');
    const withBody = { lang: 'rust', meta: 'src=crates/grid/examples/quickstart.rs#row', value: 'x' };
    expect(() => fillFence(withBody, ROOT, 'page.md')).toThrow(/has a body/);
    const missing = { lang: 'rust', meta: 'src=crates/grid/examples/quickstart.rs#nope', value: '' };
    expect(() => fillFence(missing, ROOT, 'page.md')).toThrow(/no region "nope"/);
  });

  test('includedMarkdown drops the title and points relative links into the repository', () => {
    const roadmap = includedMarkdown(ROOT, 'ROADMAP.md');
    expect(roadmap.startsWith('# ')).toBe(false);
    expect(roadmap).toContain('## ');
    const targets = [...roadmap.matchAll(/\]\(([^)]*)\)/g)].map((link) => link[1]);
    const local = targets.filter(
      (target) => !/^(?:https?:|#)/.test(target) && !target.startsWith(REPOSITORY),
    );
    expect(local).toEqual([]);
  });
});
