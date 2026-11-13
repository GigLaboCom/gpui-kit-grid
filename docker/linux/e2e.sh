#!/usr/bin/env bash
# Level 3: behavioural end-to-end run. xdotool drives real `blotter`,
# `portfolio`, `dock_layout` and `orders` windows under Xvfb with the
# mouse and the keyboard; they report events, state and geometry in
# BLOTTER_E2E_LOG, PORTFOLIO_E2E_LOG, DOCK_E2E_LOG and ORDERS_E2E_LOG, and
# script/e2e/e2e.py checks them.
#   docker/linux/e2e.sh [scenario-prefix ...]   → out/linux/e2e/ (summary.txt, logs, screenshots)
#
# Built with `test-support`: the blotter's log measures the chips of the
# group panel and reads their focus through gpui-kit's element observation
# (examples/blotter/e2e.rs). The binaries are copied to /target/e2e, so the
# plain build of smoke.sh only relinks.
set -euo pipefail
source "$(dirname "$0")/common.sh"
ensure_image
"${DOCKER_RUN[@]}" bash -c '
  # No pipe into `tail`: it would hide a failed build, and the run would go
  # on with stale binaries.
  log=/tmp/build.log
  if ! cargo build --release --locked -p gpui-kit-grid --features test-support \
    --example blotter --example portfolio --example dock_layout --example orders >"$log" 2>&1; then
    tail -n 40 "$log"
    echo "build failed" >&2
    exit 1
  fi
  tail -n 1 "$log" &&
  mkdir -p /target/e2e &&
  for example in blotter portfolio dock_layout orders; do
    cp "/target/release/examples/$example" "/target/e2e/$example"
  done &&
  BLOTTER_BIN=/target/e2e/blotter PORTFOLIO_BIN=/target/e2e/portfolio DOCK_BIN=/target/e2e/dock_layout \
    ORDERS_BIN=/target/e2e/orders \
    bash script/e2e/run.sh "$@"' e2e "$@"
