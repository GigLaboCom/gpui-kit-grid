// Code on the site's pages and in the README comes from compiled files only:
// a region of a source file, marked
//
//     // #region name
//     …
//     // #endregion name
//
// and named on a page as `path#name` from the repository's root. The
// examples build with `cargo clippy --all-targets`, so a snippet cannot
// drift from the API without breaking that build or `bun test`.

import { readFileSync } from 'node:fs';
import { isAbsolute, relative, resolve } from 'node:path';

const START = /^\s*\/\/\s*#region\s+(\S+)\s*$/;
const END = /^\s*\/\/\s*#endregion\s+(\S+)\s*$/;

/** A region's source and where it is. */
export interface Region {
  name: string;
  /** The region's lines, without the markers, dedented. */
  code: string;
  /** 1-based line of the `#region` marker. */
  line: number;
}

/**
 * Every region of a source file by name. Regions may nest; the markers of
 * a nested region are left out of the outer one's code. Fails on a region
 * that is not closed, closed twice, or named twice.
 */
export function regionsOf(source: string, file = 'source'): Map<string, Region> {
  const lines = source.split('\n');
  const open = new Map<string, { start: number; body: string[] }>();
  const done = new Map<string, Region>();
  lines.forEach((text, ix) => {
    const start = START.exec(text);
    const end = END.exec(text);
    if (start) {
      const name = start[1];
      if (open.has(name) || done.has(name)) {
        throw new Error(`${file}:${ix + 1}: region "${name}" is defined twice`);
      }
      open.set(name, { start: ix + 1, body: [] });
    } else if (end) {
      const name = end[1];
      const region = open.get(name);
      if (!region) throw new Error(`${file}:${ix + 1}: "#endregion ${name}" without its "#region"`);
      open.delete(name);
      const code = dedent(region.body.join('\n'));
      if (code.trim() === '') throw new Error(`${file}:${region.start}: region "${name}" is empty`);
      done.set(name, { name, code, line: region.start });
    } else {
      for (const region of open.values()) region.body.push(text);
    }
  });
  for (const [name, region] of open) {
    throw new Error(`${file}:${region.start}: region "${name}" is not closed`);
  }
  return done;
}

/** Removes the indentation every non-empty line shares, and outer blank lines. */
export function dedent(code: string): string {
  const lines = code.replace(/\s+$/, '').split('\n');
  while (lines.length > 0 && lines[0].trim() === '') lines.shift();
  const indents = lines
    .filter((line) => line.trim() !== '')
    .map((line) => /^[ \t]*/.exec(line)![0].length);
  const cut = indents.length > 0 ? Math.min(...indents) : 0;
  return lines.map((line) => line.slice(cut).trimEnd()).join('\n');
}

/** `crates/grid/examples/quickstart.rs#row` → its path and region name. */
export function parseSource(reference: string): { path: string; name: string } {
  const hash = reference.lastIndexOf('#');
  if (hash <= 0 || hash === reference.length - 1) {
    throw new Error(`"${reference}" is not a region reference (path#name)`);
  }
  return { path: reference.slice(0, hash), name: reference.slice(hash + 1) };
}

const cache = new Map<string, Map<string, Region>>();

/**
 * The region a reference names, read from the repository at `root`. Fails
 * with the reference in the message when the file or the region is missing,
 * or when the path leaves the repository.
 */
export function readRegion(root: string, reference: string): Region {
  const { path, name } = parseSource(reference);
  const absolute = resolve(root, path);
  const inside = relative(root, absolute);
  if (isAbsolute(path) || inside.startsWith('..')) {
    throw new Error(`${reference}: a region path is relative to the repository and stays in it`);
  }
  let regions = cache.get(absolute);
  if (!regions) {
    let source: string;
    try {
      source = readFileSync(absolute, 'utf8');
    } catch {
      throw new Error(`${reference}: cannot read ${path}`);
    }
    regions = regionsOf(source, path);
    cache.set(absolute, regions);
  }
  const region = regions.get(name);
  if (!region) throw new Error(`${reference}: ${path} has no region "${name}"`);
  return region;
}

/** Forgets the files read so far (for a dev server that watches them). */
export function clearRegionCache() {
  cache.clear();
}

/**
 * A code fence's meta string split into words, keeping `key="a b"` whole:
 * `src="x#y" ignore` → `['src="x#y"', 'ignore']`.
 */
export function metaWords(meta: string | null | undefined): string[] {
  return meta ? (meta.match(/\S+?="[^"]*"|\S+/g) ?? []) : [];
}

/** The value of `key=value` or `key="value"` among a fence's meta words. */
export function metaValue(words: string[], key: string): string | undefined {
  for (const word of words) {
    const match = new RegExp(`^${key}=(?:"([^"]*)"|(\\S+))$`).exec(word);
    if (match) return match[1] ?? match[2];
  }
  return undefined;
}
