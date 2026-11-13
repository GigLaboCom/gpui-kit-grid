#!/usr/bin/env bash
# The `perf` example on Linux: its snapshots (PERF_SNAPSHOT, headless wgpu
# on Mesa) and a short windowed run under Xvfb, with a little scripted load
# so the process tree has branches, new rows and exited ones.
#   docker/linux/perf.sh        → out/linux/perf/ (perf-*.png, *.log, perf-window.png)
# The images show the container's processes only.
set -euo pipefail
source "$(dirname "$0")/common.sh"
ensure_image
"${DOCKER_RUN[@]}" bash -c '
  # No pipe into `tail`: it would hide a failed build.
  log=/tmp/build.log
  if ! cargo build --release --locked -p gpui-kit-grid --features test-support --example perf >"$log" 2>&1; then
    tail -n 40 "$log"
    echo "build failed" >&2
    exit 1
  fi
  tail -n 1 "$log"
  mkdir -p /target/perf && cp /target/release/examples/perf /target/perf/perf
  PERF_BIN=/target/perf/perf bash docker/linux/inside-perf.sh'
