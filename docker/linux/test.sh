#!/usr/bin/env bash
# Level 1: the Linux build, clippy and the test suite, headless UI tests
# included, as the CI commands (AGENTS.md, "Common Commands").
#   docker/linux/test.sh        → out/linux/test.log, out/linux/test-summary.txt
set -euo pipefail
source "$(dirname "$0")/common.sh"
ensure_image
log="$OUT/test.log"
summary="$OUT/test-summary.txt"
status=0
"${DOCKER_RUN[@]}" bash -c '
  set -euo pipefail
  step() { echo "== $*"; "$@"; }
  step cargo build --workspace --locked
  step cargo clippy --workspace --all-targets --features gpui-kit-grid/test-support --locked -- --deny warnings
  step cargo test --workspace --features gpui-kit-grid/test-support --locked
  step cargo test --workspace --features gpui-kit-grid/test-support --locked --doc
' 2>&1 | tee "$log" || status=$?

# "test result: ok. 12 passed; 0 failed; 3 ignored; ..." per test binary,
# summed per step. Plain `cargo test` runs the doc tests too; the `--doc`
# step runs them again on their own, as CI does.
{
  echo "level 1 ($(date -u +%Y-%m-%dT%H:%MZ)): exit $status"
  awk '/^== / { step = substr($0, 4); steps[++n] = step }
       /^test result:/ { p[step] += $4; f[step] += $6; i[step] += $8; t[step] = 1 }
       END { for (k = 1; k <= n; k++) { s = steps[k]
               if (t[s]) printf "%s: %d passed, %d failed, %d ignored\n", s, p[s], f[s], i[s]
               else printf "%s\n", s } }' "$log"
} | tee "$summary"
exit "$status"
