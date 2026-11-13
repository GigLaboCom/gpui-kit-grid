#!/usr/bin/env bash
# Inside the container (docker/linux/perf.sh runs it): `perf` drawn
# offscreen with PERF_SNAPSHOT, then in a real X11 window under Xvfb with
# PERF_SECONDS, while a scripted load runs: a busy tree (sh → sh → yes)
# for the heat and the tree, and a churn of short-lived sh → sleep trees
# that appear and exit every second or so. Out: out/linux/perf/.
set -uo pipefail
cd "$(dirname "$0")/../.."
source script/e2e/xsession.sh
OUT=out/linux/perf
PERF_BIN=${PERF_BIN:-/target/release/examples/perf}
mkdir -p "$OUT"
rm -f "$OUT"/*.png "$OUT"/*.log

load_pids=()
start_load() {
  sh -c 'sh -c "yes >/dev/null" & sleep 600' &
  load_pids+=($!)
  sh -c 'while :; do sh -c "sleep 2 & sleep 3" & sleep 1.3; done' &
  load_pids+=($!)
}
stop_load() {
  for pid in "${load_pids[@]}"; do
    pkill -P "$pid" 2>/dev/null
    kill "$pid" 2>/dev/null
  done
  pkill -x yes 2>/dev/null
  true
}
trap 'stop_load; stop_x' EXIT
start_load

failed=0
echo "== snapshots"
PERF_SNAPSHOT=$OUT timeout 120 "$PERF_BIN" >"$OUT/snapshot.log" 2>&1
code=$?
cat "$OUT/snapshot.log"
[[ $code -eq 0 ]] || { echo "snapshot exit $code"; failed=1; }

echo "== window"
start_x || exit 1
PERF_SECONDS=12 timeout 60 "$PERF_BIN" >"$OUT/window.log" 2>&1 &
pid=$!
sleep 9
import -window root "$OUT/perf-window.png" 2>/dev/null || echo "no screenshot" >>"$OUT/window.log"
wait "$pid"
code=$?
tail -n 4 "$OUT/window.log"
panics=$(grep -cE 'panicked|ERROR' "$OUT/window.log")
if [[ $code -ne 0 || $panics -ne 0 ]]; then
  echo "window: exit $code, $panics panics"
  failed=1
fi
exit "$failed"
