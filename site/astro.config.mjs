// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { fileURLToPath } from 'node:url';
import embed from './integrations/embed.mjs';
import { satteri } from '@astrojs/markdown-satteri';
import markdownRegions from './src/lib/markdown-regions.ts';

// The repository's root: code on the pages comes from regions of its
// compiled files, and the roadmap page from its `ROADMAP.md`.
const root = fileURLToPath(new URL('..', import.meta.url));

// The site lives at the root of its own host, `gkg.giglabo.com`.
const site = 'https://gkg.giglabo.com';

export default defineConfig({
  site,
  // The root of the domain; `SITE_BASE=/path` serves the site under a path
  // instead, for a preview on another host.
  base: process.env.SITE_BASE || '/',
  trailingSlash: 'always',
  markdown: {
    // Astro's own Markdown processor, with the regions plugin in front of
    // the plugins Starlight adds.
    processor: satteri({ mdastPlugins: [markdownRegions({ root })] }),
  },
  integrations: [
    starlight({
      title: 'gpui-kit-grid',
      description:
        'A live data grid for gpui-kit (GPUI) applications: rows by key from any thread, change highlight, and sorting, filtering, grouping and trees in the background.',
      logo: {
        light: './src/assets/mark-light.svg',
        dark: './src/assets/mark-dark.svg',
      },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'Repository', href: 'https://github.com/GigLaboCom/gpui-kit-grid' }],
      customCss: [
        '@fontsource/ibm-plex-sans/latin-400.css',
        '@fontsource/ibm-plex-sans/latin-400-italic.css',
        '@fontsource/ibm-plex-sans/latin-600.css',
        '@fontsource/ibm-plex-mono/latin-400.css',
        './src/styles/theme.css',
      ],
      components: {
        Footer: './src/components/Footer.astro',
        ThemeSelect: './src/components/ThemeSelect.astro',
      },
      routeMiddleware: './src/routeData.ts',
      sidebar: [
        { slug: 'start', label: 'Start' },
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
        'performance',
        { slug: 'web', label: 'Web' },
        'play',
        'embedding',
        // rustdoc, which the site's CI build writes into `api/`.
        { label: 'API', link: '/api/' },
        'roadmap',
        'sponsor',
      ],
    }),
    // `<live-grid>` under `/embed/v1/`, from the examples' `www/embed/`.
    embed(),
  ],
});
