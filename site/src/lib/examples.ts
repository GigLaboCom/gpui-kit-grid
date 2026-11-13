/**
 * The repository's examples, for the Examples page and the landing: what
 * each shows, how to run it, where its source is, and, for those that run
 * in the browser, the demo page under `/demo/` and its poster under
 * `/posters/` (drawn by `make -C crates/grid-web posters`).
 */

export interface Demo {
	/** The demo's page under `/demo/`: `crates/grid-web/www/<app>/`. */
	app: string;
	/** The demo's own query, without `?` (`city=helsinki`). */
	query?: string;
	/** The poster's name under `/posters/`, without `.webp`. */
	poster: string;
	/**
	 * Opens only on its full page: a keyboard-first example, whose keys a
	 * documentation page would share.
	 */
	fullPageOnly?: boolean;
	/** Said under the run button: what else the demo loads, and when. */
	note?: string;
}

export interface Example {
	/** The example's name, as `cargo run --example` takes it. */
	name: string;
	/** One line under the name. */
	title: string;
	description: string;
	/** The command that runs it from a checkout of the repository. */
	command: string;
	/** Its source, relative to the repository's root. */
	source: string;
	demo?: Demo;
}

const run = (name: string, features = '') =>
	`cargo run --release -p gpui-kit-grid${features} --example ${name}`;

export const EXAMPLES: Example[] = [
	{
		name: 'quotes',
		title: 'Live quotes, grouped by sector',
		description:
			'150 simulated quotes grouped by sector with live averages in the captions, on the desk’s simulator and its Depth theme. Built for the browser only; the gateway in crates/grid-gateway can feed it over WebSocket.',
		command: 'make -C crates/grid-web build serve',
		source: 'crates/grid-web/apps/quotes',
		demo: { app: 'quotes', poster: 'quotes' },
	},
	{
		name: 'desk',
		title: 'A keyboard-first trading terminal',
		description:
			'A command line, a watchlist grouped by sector, a candle chart, an order book shaded by depth, a trades tape, positions as a tree and a news grid, in three themes switched with F2–F4. Every price is simulated.',
		command: run('desk'),
		source: 'crates/grid/examples/desk',
		demo: { app: 'desk', poster: 'desk', fullPageOnly: true },
	},
	{
		name: 'broker',
		title: 'A light retail broker app',
		description:
			'A watchlist with tall rows and no grid lines, an area chart, an order ticket whose simulated orders fill against the simulated quotes, and positions, orders and movers, in light and dark themes.',
		command: run('broker'),
		source: 'crates/grid/examples/broker',
		demo: { app: 'broker', poster: 'broker' },
	},
	{
		name: 'ops',
		title: 'A data center during an incident storm',
		description:
			'Over a thousand alert updates a second collapse into one live row per alert, sorted by severity and rate, beside a fleet of thousands of hosts as a live tree with roll-ups. Everything is simulated; F8 starts a storm.',
		command: run('ops'),
		source: 'crates/grid/examples/ops',
		demo: { app: 'ops', poster: 'ops' },
	},
	{
		name: 'orbit',
		title: 'Everything in orbit right now',
		description:
			'About 17,000 active satellites from CelesTrak’s public elements, propagated every second and sorted by elevation, so the table reorders on every update; flat, by constellation, or as a tree of orbital shells.',
		command: run('orbit'),
		source: 'crates/grid/examples/orbit',
		demo: {
			app: 'orbit',
			poster: 'orbit',
			note: 'Then your browser fetches the orbital elements from CelesTrak (about 7 MB), only after a second click.',
		},
	},
	{
		name: 'transit',
		title: 'Munich by the timetable',
		description:
			'Every train, tram and city bus that MVV’s published timetable says is under way, placed between its stations and updated every second, with a map and a station’s board of departures. Scheduled, not real-time.',
		command: run('transit'),
		source: 'crates/grid/examples/transit',
		demo: { app: 'transit', poster: 'transit' },
	},
	{
		name: 'transit',
		title: 'Helsinki, live',
		description:
			'The same example on HSL’s high-frequency positioning: every tram, metro, train and ferry in Helsinki, live from HSL to your browser, a row per vehicle with its delay and next stop.',
		command: `TRANSIT_CITY=helsinki ${run('transit')}`,
		source: 'crates/grid/examples/transit',
		demo: {
			app: 'transit',
			query: 'city=helsinki',
			poster: 'helsinki',
			note: 'Your browser connects to HSL; a recording plays if HSL cannot be reached.',
		},
	},
	{
		name: 'wiki',
		title: 'The world editing Wikipedia',
		description:
			'One row per Wikimedia wiki, sorted by edits in the last minute, from Wikimedia’s public EventStreams; a seismograph of the last minute beside it. The stream opens only when you press Start; Replay plays a recording.',
		command: run('wiki'),
		source: 'crates/grid/examples/wiki',
		demo: { app: 'wiki', poster: 'wiki' },
	},
	{
		name: 'ships',
		title: 'Every vessel around Finland',
		description:
			'About 700 vessels live from Digitraffic’s open AIS data, a row each updated in place and sorted by speed, with a map drawn by the vessels themselves.',
		command: run('ships'),
		source: 'crates/grid/examples/ships',
		demo: {
			app: 'ships',
			poster: 'ships',
			note: 'Your browser connects to Digitraffic; a recording plays if it cannot be reached.',
		},
	},
	{
		name: 'quickstart',
		title: 'The minimal application',
		description:
			'1000 trades that a producer thread keeps updating, sorted by price, with selection by key: the code the Start page walks through.',
		command: run('quickstart'),
		source: 'crates/grid/examples/quickstart.rs',
	},
	{
		name: 'cookbook',
		title: 'The Cookbook’s recipes',
		description:
			'A quotes window whose toolbar runs the recipes of the Cookbook page: a reload with progress, a held order, a saved layout, an export of your own, and quotes from the WebSocket gateway.',
		command: run('cookbook'),
		source: 'crates/grid/examples/cookbook.rs',
	},
	{
		name: 'blotter',
		title: 'A trade blotter with every core feature',
		description:
			'Reorder policy and Refresh order, reload under the live stream with progress, quick filter, grouping with aggregates in the captions, a totals row, pinned columns, tooltips, copy, and the column chooser.',
		command: run('blotter'),
		source: 'crates/grid/examples/blotter',
	},
	{
		name: 'portfolio',
		title: 'A live tree',
		description:
			'Funds, desks, books and positions in one column with chevrons, the books’ and desks’ sums written by the producer, totals over the leaves, and a quick filter that keeps the ancestors of matches.',
		command: run('portfolio'),
		source: 'crates/grid/examples/portfolio',
	},
	{
		name: 'dock_layout',
		title: 'Grids in a dock, layout saved',
		description:
			'The column layout and named layouts saved to a file and restored at startup, with a group panel in each trades tab.',
		command: run('dock_layout'),
		source: 'crates/grid/examples/dock_layout.rs',
	},
	{
		name: 'orders',
		title: 'Editing under live updates',
		description:
			'A trader’s orders edited in place while the market streams in: every built-in editor, a “server” that confirms, rejects or conflicts with each commit, widgets in cells, CSV export and drag and drop.',
		command: run('orders'),
		source: 'crates/grid/examples/orders',
	},
	{
		name: 'ladder',
		title: 'A futures price ladder',
		description:
			'Each row a fixed price, so sizes tick in place: working orders, bid, ask and volume at each price, with a tape, a candle chart and P&L. Simulated exchange.',
		command: run('ladder'),
		source: 'crates/grid/examples/ladder',
	},
	{
		name: 'wire',
		title: 'A news and analytics workstation',
		description:
			'A news monitor grid with unread stories in bold and category chips, the open story with live quotes, and a market overview grouped by sector. Stories come from neutral templates over the simulated quotes.',
		command: run('wire'),
		source: 'crates/grid/examples/wire',
	},
	{
		name: 'spike_bench',
		title: 'The headless benchmark',
		description:
			'Every scenario on the Performance page, and the comparison with gpui-kit’s DataTable, measured with headless Metal on macOS.',
		command: run('spike_bench', ' --features test-support'),
		source: 'crates/grid/examples/spike_bench.rs',
	},
];

/** The repository's address of `path`. */
export function sourceUrl(path: string): string {
	const kind = /\.\w+$/.test(path) ? 'blob' : 'tree';
	return `https://github.com/GigLaboCom/gpui-kit-grid/${kind}/main/${path}`;
}
