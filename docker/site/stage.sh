#!/usr/bin/env bash
#
# Fills the build context of the site image from a built site (default:
# site/dist, after `bun run build` and crates/grid-web/scripts/site-dist.mjs,
# as in .github/workflows/site.yml):
#
# - the Dockerfile and the nginx configuration of docker/site;
# - the header maps of the site's _headers (headers.awk), and the site
#   without that rules file;
# - a .gz next to every file worth compressing, which nginx serves as is
#   (gzip_static); the demos' wasm modules gain the most.
#
# The context holds nothing else, so `docker build` sends only the site, and
# the work is done once, not per platform.
#
#   docker/site/stage.sh <context dir> [<dist dir>]
#   docker build -t gpui-kit-grid-site <context dir>

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
fail() {
  echo "stage.sh: $*" >&2
  exit 1
}

[[ $# -ge 1 && $# -le 2 ]] || fail "usage: docker/site/stage.sh <context dir> [<dist dir>]"
dest="$1"
dist="${2:-$root/site/dist}"
[[ -f "$dist/index.html" && -f "$dist/404.html" && -f "$dist/_headers" ]] \
  || fail "$dist is not a built site (index.html, 404.html, _headers); build it first"

# Only a folder this script made before is replaced.
marker=".site-image-context"
if [[ -e "$dest" ]]; then
  [[ -f "$dest/$marker" ]] || fail "$dest exists and is not a context this script staged"
  rm -rf "$dest"
fi
mkdir -p "$dest"
touch "$dest/$marker"
printf '%s\n' "$marker" >"$dest/.dockerignore"
cp "$here/Dockerfile" "$here/nginx.conf" "$here/site.conf" "$dest/"
cp -R "$dist" "$dest/dist"

awk -v add="$dest/headers-add.conf" -f "$here/headers.awk" "$dest/dist/_headers" >"$dest/headers-map.conf"
rm "$dest/dist/_headers"

find "$dest/dist" -type f -size +1k \
  \( -name '*.html' -o -name '*.css' -o -name '*.js' -o -name '*.mjs' \
  -o -name '*.json' -o -name '*.svg' -o -name '*.xml' -o -name '*.txt' \
  -o -name '*.wasm' -o -name '*.tsv' -o -name '*.jsonl' \) \
  -exec gzip -9 -k -n {} +

echo "stage.sh: $dest ($(du -sh "$dest/dist" | cut -f1) of site, $(grep -c '^map ' "$dest/headers-map.conf") header maps)"
