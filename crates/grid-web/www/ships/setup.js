// What the ships example needs before it runs, for its page and for
// `<live-grid app="ships">` inline (`embed/live-grid.js`). It connects to
// Digitraffic itself, with no server between: the latest locations and
// metadata from its REST API (fetched as soon as `setup` is called, while
// the module downloads), then MQTT over the browser's WebSocket from the
// module. `source=replay` in `params` plays the recording instead
// (`data/ais-recording.txt`, made natively with SHIPS_RECORD), as does a
// run that cannot reach Digitraffic, with a notice, and a run whose MQTT
// feed stays out of reach (the module asks; the recording is fetched only
// then); `speed=` is how fast a replay runs.

// As `crates/grid/examples/ships/live.rs`: who asks, and how far back.
const API = 'https://meri.digitraffic.fi/api/ais/v1';
const USER = 'gpui-kit-grid/ships';
const LOCATIONS_MS = 30 * 60 * 1000;
const METADATA_MS = 2 * 24 * 60 * 60 * 1000;
const RECORDING = new URL('../data/ais-recording.txt', import.meta.url);

async function fetchBytes(url) {
  const response = await fetch(url, { headers: { 'Digitraffic-User': USER } });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

// The recording: `{ bytes }`, or `{ reason }` it could not be fetched.
async function fetchRecording() {
  const response = await fetch(RECORDING).catch((error) => error);
  if (!(response instanceof Response) || !response.ok) {
    return { reason: response instanceof Response ? `HTTP ${response.status}` : String(response) };
  }
  return { bytes: new Uint8Array(await response.arrayBuffer()) };
}

// The recording, fetched once MQTT stays out of reach and the module asks
// for it.
function awaitFallback(wasm) {
  const timer = setInterval(async () => {
    if (!wasm.ships_fallback_wanted()) return;
    clearInterval(timer);
    const { bytes, reason } = await fetchRecording();
    wasm.ships_fallback(bytes, reason && `data/ais-recording.txt: ${reason}`);
  }, 1000);
}

/// The example's start for `params`: `{ loading, beforeRun }`, where
/// `beforeRun(wasm)` hands the module the snapshot to go live with, or the
/// recording to replay, and returns the bytes handed over.
export function setup(params) {
  const replay = params.get('source') === 'replay';
  const now = Date.now();
  const snapshot = replay
    ? null
    : Promise.all([
        fetchBytes(`${API}/locations?from=${now - LOCATIONS_MS}`),
        fetchBytes(`${API}/vessels?from=${now - METADATA_MS}`),
      ]);
  snapshot?.catch(() => {}); // Read in `beforeRun`.

  async function beforeRun(wasm) {
    let notice;
    if (snapshot) {
      try {
        const [locations, vessels] = await snapshot;
        wasm.ships_live(locations, vessels);
        awaitFallback(wasm);
        return locations.length + vessels.length;
      } catch (error) {
        notice = `Digitraffic unreachable (${error.message ?? error}): a recording replays`;
      }
    }
    const speed = params.has('speed') ? Number(params.get('speed')) : undefined;
    const { bytes, reason } = await fetchRecording();
    if (!bytes) {
      const error = `data/ais-recording.txt: ${reason}. Record one natively with SHIPS_RECORD (see the ships example), then make build.`;
      wasm.ships_replay(undefined, notice ? `${notice}; ${error}` : error, notice, speed);
      return 0;
    }
    wasm.ships_replay(bytes, undefined, notice, speed);
    return bytes.length;
  }

  return { loading: replay ? 'Loading the recording' : 'Connecting to Digitraffic', beforeRun };
}
