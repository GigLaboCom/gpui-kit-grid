// What the transit example needs before it runs, for its page and for
// `<live-grid app="transit">` inline (`embed/live-grid.js`): the choice in
// `params` and that choice's data, every path relative to this file.
//
// - Munich (the default; `regional` with the regional trains and buses):
//   the compiled timetable, `data/mvv-transit.tsv.gz` or
//   `data/mvv-transit-regional.tsv.gz` (`time=` a fixed start: `07:45`
//   today in Munich, `2026-11-10T07:45`, or UTC with a `Z`).
// - Helsinki (`city=helsinki`, `buses` with the buses): HSL's network,
//   `data/hsl-network.tsv.gz`; the module connects to HSL's broker itself.
//   `source=replay` plays the recording (`data/hsl-recording.txt.gz`)
//   instead, as does a run whose broker stays out of reach (the module
//   asks; the recording is fetched only then); `speed=` is how fast a
//   replay runs.

// Unpacks a gzip file, or returns the bytes as they are when the server
// has already unpacked them.
async function gunzip(bytes) {
  if (bytes[0] !== 0x1f || bytes[1] !== 0x8b) return bytes;
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

// A data file, unpacked: `{ bytes, packed }`, or `{ error }`.
async function fetchData(file) {
  const url = new URL(`../data/${file}`, import.meta.url);
  const response = await fetch(url).catch((error) => error);
  if (!(response instanceof Response) || !response.ok) {
    const reason = response instanceof Response ? `HTTP ${response.status}` : String(response);
    return { error: `data/${file}: ${reason}` };
  }
  const packed = new Uint8Array(await response.arrayBuffer());
  return { bytes: await gunzip(packed), packed: packed.length };
}

// The recording, fetched once the broker stays out of reach and the module
// asks for it.
function awaitFallback(wasm) {
  const timer = setInterval(async () => {
    if (!wasm.transit_fallback_wanted()) return;
    clearInterval(timer);
    const { bytes, error } = await fetchData('hsl-recording.txt.gz');
    wasm.transit_fallback(bytes, error);
  }, 1000);
}

/// The example's start for `params`: `{ city, loading, beforeRun }`, where
/// `beforeRun(wasm)` hands the module its data (without it the example
/// says how to get it) and returns the bytes fetched.
export function setup(params) {
  const helsinki = params.get('city') === 'helsinki';
  const regional = params.has('regional');
  const replay = params.get('source') === 'replay';

  async function beforeRun(wasm) {
    if (!helsinki) {
      const time = params.get('time') ?? undefined;
      const file = regional ? 'mvv-transit-regional.tsv.gz' : 'mvv-transit.tsv.gz';
      const { bytes, packed, error } = await fetchData(file);
      const hint = error && `${error}. Run python3 -I script/fetch-transit-data, then make build.`;
      wasm.transit_data(bytes, hint, time, regional);
      return packed ?? 0;
    }
    const speed = params.has('speed') ? Number(params.get('speed')) : undefined;
    const [network, recording] = await Promise.all([
      fetchData('hsl-network.tsv.gz'),
      replay ? fetchData('hsl-recording.txt.gz') : Promise.resolve({}),
    ]);
    if (network.error) console.warn(`${network.error}: stop ids stand for names`);
    wasm.transit_helsinki(network.bytes, recording.bytes, recording.error, params.has('buses'), speed);
    if (!replay) awaitFallback(wasm);
    return (network.packed ?? 0) + (recording.packed ?? 0);
  }

  return {
    city: helsinki ? 'helsinki' : 'munich',
    loading: helsinki ? 'Connecting to HSL' : 'Loading the timetable',
    beforeRun,
  };
}
