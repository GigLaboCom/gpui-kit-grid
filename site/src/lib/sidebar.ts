import type { StarlightUserConfig } from '@astrojs/starlight/types';

/**
 * The site's navigation, for Starlight's `sidebar` (astro.config.mjs) and
 * for `/llms.txt` (src/pages/llms.txt.ts), which lists the pages in the same
 * order and under the same groups.
 */
export const SIDEBAR = [
  { slug: 'start', label: 'Start' },
  // Kept near the top so the other products are seen (src/data/products.ts).
  { slug: 'products', label: 'More from GigLabo', badge: { text: 'Products', variant: 'tip' } },
  {
    label: 'Concepts',
    items: [
      { slug: 'concepts', label: 'Overview' },
      'concepts/replaceable-behaviors',
    ],
  },
  {
    label: 'Guides',
    items: [
      'guides/grouping',
      'guides/tree',
      'guides/live-changes',
      'guides/cells',
      'guides/editing',
      'guides/export',
      'guides/drag-and-drop',
      'guides/columns',
      'guides/keyboard',
      'guides/settings',
    ],
  },
  'cookbook',
  'examples',
  // Pages about one demo each, by what a reader may be looking for.
  { label: 'Live demos', items: ['webgpu-demo', 'trade-blotter'] },
  'performance',
  // The other Rust data grids, for a reader choosing one.
  'rust-data-grid',
  { slug: 'web', label: 'Web' },
  'play',
  'embedding',
  // rustdoc, which the site's CI build writes into `api/`.
  { label: 'API', link: '/api/' },
  // The released crates, and their API as docs.rs builds it.
  { label: 'crates.io', link: 'https://crates.io/crates/gpui-kit-grid' },
  'roadmap',
  'sponsor',
] satisfies StarlightUserConfig['sidebar'];
