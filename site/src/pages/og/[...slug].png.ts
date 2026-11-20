import type { APIRoute, GetStaticPaths, InferGetStaticPropsType } from 'astro';
import { publicDir, root } from 'astro:config/server';
import { getCollection } from 'astro:content';
import { cardAssets, DEFAULT_POSTER, renderCard } from '../../lib/og';

/*
 * `/og/<page>.png`: each page's Open Graph card (src/lib/og.ts), made when
 * the site is built; src/routeData.ts points the page's `og:image` at it.
 */

const assets = cardAssets(root, publicDir);

export const getStaticPaths = (async () =>
  (await getCollection('docs')).map((entry) => ({
    params: { slug: entry.id },
    props: {
      title: entry.data.title,
      description: entry.data.description,
      poster: entry.data.poster ?? DEFAULT_POSTER,
    },
  }))) satisfies GetStaticPaths;

type Props = InferGetStaticPropsType<typeof getStaticPaths>;

export const GET: APIRoute<Props> = async ({ props }) =>
  new Response(new Uint8Array(await renderCard(props, assets)), { headers: { 'Content-Type': 'image/png' } });
