#!/usr/bin/env bash
# Builds the image and the release `blotter` for Linux (the binary of levels
# 2 and 3). test.sh, smoke.sh and e2e.sh call it themselves.
#   docker/linux/build.sh
set -euo pipefail
source "$(dirname "$0")/common.sh"
docker build -t "$IMAGE" "$HERE"
"${DOCKER_RUN[@]}" cargo build --release --locked -p gpui-kit-grid --example blotter
