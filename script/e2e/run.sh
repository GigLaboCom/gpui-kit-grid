#!/usr/bin/env bash
# Level 3, inside the container (docker/linux/e2e.sh runs it) or on any
# Linux with Xvfb, xdotool, xclip, ImageMagick and Python 3: the scenarios
# of e2e.py against release `blotter`, `portfolio`, `dock_layout` and
# `orders` builds in X11 windows under Xvfb.
#   bash script/e2e/run.sh [scenario-prefix ...]    → out/linux/e2e/
set -uo pipefail
cd "$(dirname "$0")/../.."
source script/e2e/xsession.sh
export E2E_OUT=${E2E_OUT:-out/linux/e2e} BLOTTER_BIN PORTFOLIO_BIN DOCK_BIN ORDERS_BIN
rm -rf "$E2E_OUT" && mkdir -p "$E2E_OUT"
start_x || exit 1
trap stop_x EXIT
python3 -I script/e2e/e2e.py "$@"
