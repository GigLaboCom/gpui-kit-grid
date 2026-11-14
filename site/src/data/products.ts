// The other GigLabo products, for the Products page and the strip on the home
// page (components/ProductGrid.astro). Only what is publicly available. Names
// and lines are each product's own copy; the icons come from their repositories
// (originals in the private docs repository, assets/products/). To add one:
// an icon in src/assets/products/, an entry here.
import type { ImageMetadata } from 'astro';
import smbact from '../assets/products/smbact.png';
import wipemark from '../assets/products/wipemark.svg';
import mnemovi from '../assets/products/mnemovi.svg';
import lazyShot from '../assets/products/heretic-lazy-shot.png';
import watchword from '../assets/products/watchword.svg';
import moatAtlas from '../assets/products/moat-atlas.svg';

export type Group = 'desktop' | 'ai' | 'web';

/** Section headings on the Products page, in this order. */
export const groups: { id: Group; title: string }[] = [
	{ id: 'desktop', title: 'Desktop apps' },
	{ id: 'ai', title: 'AI tools' },
	{ id: 'web', title: 'On the web' },
];

export interface Product {
	group: Group;
	name: string;
	url: string;
	tagline: string;
	icon: ImageMetadata;
	/** A light plate behind an icon drawn for light backgrounds (dark marks on transparent). */
	plate?: boolean;
}

/** In display order: the home page strip shows them as listed. */
export const products: Product[] = [
	{
		group: 'desktop',
		name: 'SMBAct',
		url: 'https://smbact.com',
		tagline: 'A cross-platform window manager and switcher: size, position and fuzzy search of windows with hotkeys.',
		icon: smbact,
	},
	{
		group: 'desktop',
		name: 'Wipemark',
		url: 'https://github.com/GigLaboCom/wipemark-app/releases/latest',
		tagline: 'Take AI provenance marks out of your own text and pictures, on your own machine.',
		icon: wipemark,
	},
	{
		group: 'desktop',
		name: 'MnemoVi',
		url: 'https://mnemovi.com',
		tagline:
			'Turns your videos into a knowledge base you can search: everything said and everything shown on screen. Nothing leaves your device.',
		icon: mnemovi,
	},
	{
		group: 'desktop',
		name: 'Heretic Lazy Shot',
		url: 'https://giglabo.com/heretic/applications/heretic-lazy-shot',
		tagline: 'Capture, annotate and beautify screenshots, with an MCP server built in so AI agents can drive it.',
		icon: lazyShot,
	},
	{
		group: 'ai',
		name: 'Watchword',
		url: 'https://giglabo.com/heretic/tools/watchword',
		tagline:
			'An open-source MCP server: shared memory for every AI agent, text, prompts and files under memorable keywords.',
		icon: watchword,
		plate: true,
	},
	{
		group: 'web',
		name: 'Moat Atlas',
		url: 'https://moa.giglabo.com',
		tagline: 'The geology of defensibility: an interactive atlas of 35 competitive moats.',
		icon: moatAtlas,
	},
];
