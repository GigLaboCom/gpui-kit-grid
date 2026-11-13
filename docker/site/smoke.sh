#!/usr/bin/env bash
#
# Starts the site image read-only and checks that it serves the site the way
# Cloudflare does: pages, the trailing-slash redirect, the 404 page, the
# embedding headers and cache rules of _headers, wasm and compression, and an
# unprivileged nginx. Paths to try come from the dist the image was built from.
#
#   docker/site/smoke.sh <image> [<dist dir>]     # default: site/dist
#
# curl runs in a container of its own on the site container's network, so
# the check works wherever the Docker daemon is (a CI runner, a remote or
# Desktop daemon, a container driving the host's socket).

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
image="${1:?usage: docker/site/smoke.sh <image> [<dist dir>]}"
dist="${2:-$root/site/dist}"
container="site-smoke-$$"
curl_image="curlimages/curl:8.17.0"
failed=0

pass() { echo "  ok    $*"; }
bad() {
  echo "  FAIL  $*" >&2
  failed=1
}

docker pull -q "$curl_image" >/dev/null
docker run -d --name "$container" --read-only --tmpfs /tmp "$image" >/dev/null
trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
base="http://127.0.0.1:8080"
curl() { docker run --rm --network "container:$container" "$curl_image" "$@"; }
for _ in $(seq 30); do
  curl -fsS "$base/healthz" >/dev/null 2>&1 && break
  sleep 1
done

# status <path> [curl options]: the status code.
status() {
  local path="$1"
  shift
  curl -s -o /dev/null -w '%{http_code}' "$@" "$base$path"
}
# header <path> <name> [curl options]: the header's value, empty without it.
header() {
  local path="$1" field="$2"
  shift 2
  curl -s -o /dev/null -D - "$@" "$base$path" | tr -d '\r' |
    awk -v n="$(printf '%s' "$field" | tr 'A-Z' 'a-z')" -F': ' 'tolower($1) == n { sub(/^[^:]*: /, ""); print; exit }'
}
expect() {
  local what="$1" got="$2" want="$3"
  if [[ "$got" == $want ]]; then pass "$what: $got"; else bad "$what: '$got', expected '$want'"; fi
}

echo "smoke.sh: $image at $base"

expect "/healthz" "$(curl -fsS "$base/healthz" || true)" "ok"
expect "/ status" "$(status /)" 200
expect "/ type" "$(header / Content-Type)" "text/html*"

# A directory page without its slash: a relative redirect to it.
page="$(cd "$dist" && find . -mindepth 2 -maxdepth 2 -name index.html | sort | head -1 | sed 's|^\./||; s|/index\.html$||')"
expect "/$page status" "$(status "/$page")" 301
expect "/$page location" "$(header "/$page" Location)" "/$page/"
expect "/$page/ status" "$(status "/$page/")" 200

# A miss: the site's 404 page with status 404.
miss="/no-such-page-$$"
expect "$miss status" "$(status "$miss")" 404
if curl -s "$base$miss" | cmp -s - "$dist/404.html"; then pass "$miss body: 404.html"; else bad "$miss body is not 404.html"; fi
expect "/_headers status" "$(status /_headers)" 404

# Hashed Astro assets: immutable.
asset="$(cd "$dist" && find _astro -type f | sort | head -1)"
expect "/$asset cache" "$(header "/$asset" Cache-Control)" "*immutable*"

# The embed element and its modules: loadable from any origin.
element="$(cd "$dist" && find embed -name live-grid.js | sort | head -1)"
if [[ -n "$element" ]]; then
  expect "/$element CORS" "$(header "/$element" Access-Control-Allow-Origin)" "[*]"
  expect "/$element CORP" "$(header "/$element" Cross-Origin-Resource-Policy)" "cross-origin"
  expect "/$element cache" "$(header "/$element" Cache-Control)" "*max-age=300*"
  expect "/$element gzip" "$(header "/$element" Content-Encoding -H 'Accept-Encoding: gzip')" "gzip"
  module="$(cd "$dist" && find "$(dirname "$element")/modules" -type f 2>/dev/null | sort | head -1)"
  if [[ -n "$module" ]]; then
    expect "/$module cache" "$(header "/$module" Cache-Control)" "*immutable*"
    expect "/$module CORS" "$(header "/$module" Access-Control-Allow-Origin)" "[*]"
  fi
else
  bad "no embed/*/live-grid.js in $dist"
fi

# A demo's wasm module, when the site carries the demos.
wasm="$(cd "$dist" && find . -name '*.wasm' | sort | head -1 | sed 's|^\./||')"
if [[ -n "$wasm" ]]; then
  expect "/$wasm type" "$(header "/$wasm" Content-Type)" "application/wasm"
  expect "/$wasm gzip" "$(header "/$wasm" Content-Encoding -H 'Accept-Encoding: gzip')" "gzip"
  expect "/$wasm CORS" "$(header "/$wasm" Access-Control-Allow-Origin)" "[*]"
else
  echo "  skip  no wasm in $dist (a site without the demos)"
fi

# No root, and a filesystem nobody writes to.
expect "nginx user" "$(docker exec "$container" id -un)" "nginx"
expect "read-only root" "$(docker inspect -f '{{.HostConfig.ReadonlyRootfs}}' "$container")" "true"

if [[ $failed -ne 0 ]]; then
  docker logs "$container" 2>&1 | tail -20 >&2
  echo "smoke.sh: failed" >&2
  exit 1
fi
echo "smoke.sh: passed"
