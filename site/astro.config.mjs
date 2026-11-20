// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { fileURLToPath } from 'node:url';
import embed from './integrations/embed.mjs';
import { satteri } from '@astrojs/markdown-satteri';
import markdownRegions from './src/lib/markdown-regions.ts';
import { consentHead } from './src/lib/consent.ts';
import { SIDEBAR } from './src/lib/sidebar.ts';

// The repository's root: code on the pages comes from regions of its
// compiled files, and the roadmap page from its `ROADMAP.md`.
const root = fileURLToPath(new URL('..', import.meta.url));

// The site lives at the root of its own host, `gkg.giglabo.com`.
const site = 'https://gkg.giglabo.com';
// The root of the domain; `SITE_BASE=/path` serves the site under a path
// instead, for a preview on another host.
const base = process.env.SITE_BASE || '/';

export default defineConfig({
  site,
  base,
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
      head: [
        // Google Consent Mode v2 defaults (all denied) and the GTM container,
        // only on gkg.giglabo.com: src/lib/consent.ts; the banner is in Footer.
        { tag: 'script', content: consentHead() },
      ],
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
      sidebar: SIDEBAR,
    }),
    // `<live-grid>` under `/embed/v1/`, from the examples' `www/embed/`.
    embed(),
  ],
});
