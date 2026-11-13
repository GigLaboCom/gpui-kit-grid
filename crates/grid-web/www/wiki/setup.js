// What the wiki example needs before it runs, for its page and for
// `<live-grid app="wiki">` inline (`embed/live-grid.js`). The stream is the
// browser's EventSource, straight to Wikimedia, and opens only on Start in
// the view. A recording made with the native example (`WIKI_RECORD`,
// `data/wiki-recording.jsonl`, not committed) gives the Replay button and
// the fallback when the stream cannot be reached; `replay` in `params`
// plays it at start (`speed=` 1 to 16), as the checks do.

const recording = new URL('../data/wiki-recording.jsonl', import.meta.url);

async function fetchRecording() {
  const response = await fetch(recording).catch(() => null);
  return response?.ok ? new Uint8Array(await response.arrayBuffer()) : null;
}

/// The example's start for `params`: `{ loading, beforeRun }`, where
/// `beforeRun(wasm)` hands the module the recording (with `replay` first,
/// otherwise in the background) and returns the bytes fetched before it
/// runs.
export function setup(params) {
  async function beforeRun(wasm) {
    if (params.has('replay')) {
      const bytes = await fetchRecording();
      if (bytes) wasm.wiki_recording(bytes);
      return bytes?.length ?? 0;
    }
    fetchRecording().then((bytes) => bytes && wasm.wiki_recording(bytes));
    return 0;
  }
  return { loading: 'Loading the recording', beforeRun };
}
