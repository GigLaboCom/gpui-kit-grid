#!/usr/bin/env bash
# Level 2: `blotter` in a real X11 window under Xvfb, Mesa on the CPU and
# GPUI's Linux backend, with BLOTTER_SECONDS=10 on a small data set and at P1.
#   docker/linux/smoke.sh       → out/linux/smoke-*.log, smoke-*.png, smoke-summary.txt
set -euo pipefail
source "$(dirname "$0")/common.sh"
ensure_image
"${DOCKER_RUN[@]}" bash -c '
  # No pipe into `tail`: it would hide a failed build.
  log=/tmp/build.log
  if ! cargo build --release --locked -p gpui-kit-grid --example blotter >"$log" 2>&1; then
    tail -n 40 "$log"
    echo "build failed" >&2
    exit 1
  fi
  tail -n 1 "$log"
  bash docker/linux/inside-smoke.sh'
