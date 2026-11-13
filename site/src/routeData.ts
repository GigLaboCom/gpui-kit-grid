import { defineRouteMiddleware } from '@astrojs/starlight/route-data';
import { withBase } from './lib/links';

// Starlight hands each request its own copy of the route data, so these
// changes do not leak into other pages.
export const onRequest = defineRouteMiddleware((context) => {
  const route = context.locals.starlightRoute;
  const { data } = route.entry;

  // Starlight writes a hero's action links as they are: root-relative ones
  // go under the site's base, as sidebar links do.
  for (const action of data.hero?.actions ?? []) action.link = withBase(action.link);

  // A page with `stub: true` is an outline of a page still to be written.
  // It says so in a banner, points to the README, and stays out of the
  // search index and search engines. It has no sections, so no table of
  // contents either.
  if (data.stub) {
    data.banner = {
      content:
        'This page is an outline. Until it is written, the <a href="https://github.com/GigLaboCom/gpui-kit-grid#readme">README</a> covers the topic.',
    };
    data.pagefind = false;
    route.toc = undefined;
    route.head.push({ tag: 'meta', attrs: { name: 'robots', content: 'noindex' } });
  }
});
