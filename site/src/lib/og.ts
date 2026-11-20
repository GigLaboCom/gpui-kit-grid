import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import satori from 'satori';
import sharp from 'sharp';

/**
 * A page's Open Graph card: what a link to it shows on a social network,
 * in a chat or in a search result. 1200 × 630 px, the size those sites ask
 * for: the crate's mark and name, the page's title and description in the
 * site's dark theme and IBM Plex Sans, and a strip of one of the examples'
 * posters (`public/posters/`) along the bottom.
 *
 * satori lays the card out and writes its text as paths, so the build needs
 * no fonts installed; sharp, which the site already uses, makes it a PNG.
 */

export const OG_WIDTH = 1200;
export const OG_HEIGHT = 630;

/** The poster a card shows when its page names none (`poster` in the frontmatter). */
export const DEFAULT_POSTER = 'quotes';

/** The strip of the poster under the text. */
const STRIP_HEIGHT = 196;

/**
 * Where a poster's strip starts, at the card's width, when the 7% under its
 * title bar would miss the grid: the ops example's alerts open with a toolbar
 * and a search field, the desk's panels with their titles.
 */
const STRIP_TOP: Record<string, number> = { ops: 244, desk: 30 };

/** The dark theme's tokens (src/styles/theme.css). */
const COLOR = {
  background: 'hsl(210, 14%, 9%)',
  title: 'hsl(210, 18%, 94%)',
  text: 'hsl(210, 8%, 78%)',
  dim: 'hsl(210, 6%, 62%)',
  line: 'hsl(210, 10%, 23%)',
};

export interface CardInput {
  title: string;
  description?: string;
  /** A poster's name under `public/posters/`, without `.webp`. */
  poster: string;
}

/** Files the cards are made of, read once: the fonts, the mark, the posters' strips. */
export interface CardAssets {
  fonts: { name: string; data: Buffer; weight: 400 | 600; style: 'normal' }[];
  mark: string;
  strip(poster: string): Promise<string>;
}

/**
 * The fonts and the mark from the site's own files; `root` and `publicDir`
 * as `astro:config/server` gives them.
 */
export function cardAssets(root: URL, publicDir: URL): CardAssets {
  const font = (weight: 400 | 600) => ({
    name: 'IBM Plex Sans',
    data: readFileSync(
      fileURLToPath(new URL(`node_modules/@fontsource/ibm-plex-sans/files/ibm-plex-sans-latin-${weight}-normal.woff`, root)),
    ),
    weight,
    style: 'normal' as const,
  });
  const mark = readFileSync(fileURLToPath(new URL('src/assets/mark-dark.svg', root)));
  const strips = new Map<string, Promise<string>>();
  return {
    fonts: [font(400), font(600)],
    mark: `data:image/svg+xml;base64,${mark.toString('base64')}`,
    // The poster at the card's width, from its column headers down: most of
    // the examples' posters carry a title bar of about 7% of their height.
    strip(poster) {
      let strip = strips.get(poster);
      if (!strip) {
        const file = fileURLToPath(new URL(`posters/${poster}.webp`, publicDir));
        strip = (async () => {
          const scaled = await sharp(file).resize({ width: OG_WIDTH }).png().toBuffer();
          const { height = 0 } = await sharp(scaled).metadata();
          const png = await sharp(scaled)
            .extract({ left: 0, top: STRIP_TOP[poster] ?? Math.round(height * 0.068), width: OG_WIDTH, height: STRIP_HEIGHT })
            .png()
            .toBuffer();
          return `data:image/png;base64,${png.toString('base64')}`;
        })();
        strips.set(poster, strip);
      }
      return strip;
    },
  };
}

type Node = { type: string; props: Record<string, unknown> };
const el = (type: string, style: Record<string, unknown>, children?: unknown, props: Record<string, unknown> = {}): Node => ({
  type,
  props: { style, children, ...props },
});

/** The card of one page, as a PNG. */
export async function renderCard(input: CardInput, assets: CardAssets): Promise<Buffer> {
  const strip = await assets.strip(input.poster);
  const card = el(
    'div',
    {
      width: OG_WIDTH,
      height: OG_HEIGHT,
      display: 'flex',
      flexDirection: 'column',
      backgroundColor: COLOR.background,
      fontFamily: 'IBM Plex Sans',
    },
    [
      el(
        'div',
        { display: 'flex', flexDirection: 'column', flexGrow: 1, padding: '44px 64px 0 64px' },
        [
          el('div', { display: 'flex', alignItems: 'center', justifyContent: 'space-between' }, [
            el('div', { display: 'flex', alignItems: 'center' }, [
              el('img', { width: 40, height: 40, marginRight: 16 }, undefined, { src: assets.mark, width: 40, height: 40 }),
              el('div', { fontSize: 30, fontWeight: 600, color: COLOR.text }, 'gpui-kit-grid'),
            ]),
            el('div', { fontSize: 24, color: COLOR.dim }, 'gkg.giglabo.com'),
          ]),
          el(
            'div',
            { marginTop: 30, fontSize: 58, fontWeight: 600, lineHeight: 1.12, color: COLOR.title, lineClamp: 2 },
            input.title,
          ),
          input.description
            ? el(
                'div',
                { marginTop: 18, fontSize: 27, lineHeight: 1.36, color: COLOR.text, lineClamp: 3 },
                input.description,
              )
            : el('div', {}, ''),
        ],
      ),
      el('img', { width: OG_WIDTH, height: STRIP_HEIGHT, borderTop: `1px solid ${COLOR.line}` }, undefined, {
        src: strip,
        width: OG_WIDTH,
        height: STRIP_HEIGHT,
      }),
    ],
  );
  const svg = await satori(card as unknown as Parameters<typeof satori>[0], {
    width: OG_WIDTH,
    height: OG_HEIGHT,
    fonts: assets.fonts,
  });
  return sharp(Buffer.from(svg)).png({ compressionLevel: 9, palette: false }).toBuffer();
}
