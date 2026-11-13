#!/usr/bin/env bash
# Level 2, inside the container (docker/linux/smoke.sh runs it): `blotter`
# in a real X11 window under Xvfb with `BLOTTER_SECONDS=10`, on a small data
# set and at P1. Each run must open the window, paint frames under the
# stream and exit 0. Out: out/linux/smoke-<run>.log (status lines),
# smoke-<run>.png (a screenshot mid-run), smoke-summary.txt.
set -uo pipefail
cd "$(dirname "$0")/../.."
source script/e2e/xsession.sh
OUT=out/linux
SUMMARY="$OUT/smoke-summary.txt"
mkdir -p "$OUT"
: >"$SUMMARY"
start_x || exit 1
trap stop_x EXIT

failed=0
# name, rows, rate
for run in "small 200 50" "p1 20000 2000"; do
  read -r name rows rate <<<"$run"
  log="$OUT/smoke-$name.log"
  shot="$OUT/smoke-$name.png"
  ROWS=$rows RATE=$rate BLOTTER_SECONDS=10 timeout 120 "$BLOTTER_BIN" >"$log" 2>&1 &
  pid=$!
  sleep 7
  import -window root "$shot" 2>/dev/null || echo "no screenshot" >>"$log"
  wait "$pid"
  code=$?
  # "  9.5s  window renders 40 (visible true) · 20000 rows, 2000 updates/s · 38 fps · ..."
  last=$(grep -E '^ *[0-9.]+s +window renders' "$log" | tail -1)
  max_fps=$(grep -oE '· [0-9]+ fps' "$log" | grep -oE '[0-9]+' | sort -n | tail -1)
  shown=$(grep -cE "· $rows rows," "$log")
  panics=$(grep -cE 'panicked|ERROR' "$log")
  verdict=pass
  if [[ $code -ne 0 || -z $last || ${max_fps:-0} -eq 0 || $shown -eq 0 || $panics -ne 0 ]]; then
    verdict=FAIL
    failed=1
  fi
  {
    echo "$verdict $name (ROWS=$rows RATE=$rate): exit $code, max ${max_fps:-0} fps, $panics panics"
    echo "  last status: ${last# }"
  } | tee -a "$SUMMARY"
done
exit "$failed"
