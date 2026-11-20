import type { APIRoute } from 'astro';
import { withBase } from '../lib/links';

/*
 * `/robots.txt`: every page may be crawled; the file points to the sitemap
 * and to `/llms.txt`, the map for language models. Cloudflare puts its
 * content signals in front of it.
 */

export const GET: APIRoute = ({ site }) => {
  const url = (path: string) => (site ? new URL(withBase(path), site).href : withBase(path));
  const text = [
    'User-agent: *',
    'Allow: /',
    '',
    '# What gpui-kit-grid is, how to install it, and every page, for language models:',
    `#   ${url('/llms.txt')}`,
    '',
    `Sitemap: ${url('/sitemap-index.xml')}`,
    '',
  ].join('\n');
  return new Response(text, { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
};
