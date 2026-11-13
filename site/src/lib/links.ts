const base = import.meta.env.BASE_URL.replace(/\/$/, '');

/**
 * A root-relative link (`/start/`) under the site's base, which is the
 * domain's root unless a preview sets one; any other link as it is.
 */
export function withBase(link: string): string {
  return link.startsWith('/') && !link.startsWith('//') ? base + link : link;
}
