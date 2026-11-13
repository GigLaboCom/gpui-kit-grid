#!/usr/bin/env bash
# Builds the web examples, each into its own module in www/pkg/<app>/: one
# cargo run for wasm32-unknown-unknown over every app asked for (so what
# they share compiles once and the apps in parallel), then for each app, in
# parallel, wasm-bindgen's JavaScript glue; with --release wasm-opt first.
# Writes www/pkg/apps.json, the examples the pages' switcher and the index
# list, and each module's size into www/pkg/<app>/module.json, and links the examples' fonts into www/fonts/ and the orbit
# and transit examples' data into www/data/, where the pages fetch them.
#
# `build-wasm.sh [--release]` builds every app; APP=desk (or APP=desk,orbit)
# builds some, and leaves the others' modules as they are. Then
# scripts/licenses.mjs writes the licences of what the pages serve into
# www/licenses/; with SITE=1 it also prepares www/ for the public site: only
# the data the site may serve, and orbit fetching CelesTrak's elements from
# CelesTrak on a click.
#
# Needs the fonts of script/fetch-example-fonts (in
# crates/grid/examples/fonts/), the wasm32-unknown-unknown target, and the
# wasm-bindgen CLI of the version Cargo.toml pins
# (cargo install wasm-bindgen-cli --version 0.2.129).
set -euo pipefail

# The examples, in the switcher's order: each the crate apps/<app> (package
# gpui-kit-grid-web-<app>) and the page www/<app>/index.html. A new example
# is one more name here.
APPS=(quotes desk broker orbit transit wiki ships ops)

CRATE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE=debug
CARGO_FLAGS=()
if [[ "${1:-}" == "--release" ]]; then
    MODE=release
    CARGO_FLAGS=(--release)
fi
TARGET_DIR="${CARGO_TARGET_DIR:-$CRATE/target}"
OUT="$CRATE/www/pkg"
FONTS="$CRATE/../grid/examples/fonts"

BUILD=("${APPS[@]}")
if [[ -n "${APP:-}" ]]; then
    IFS=', ' read -r -a BUILD <<<"$APP"
    for app in "${BUILD[@]}"; do
        if [[ ! " ${APPS[*]} " == *" $app "* ]]; then
            echo "APP=$app: not one of ${APPS[*]}." >&2
            exit 1
        fi
    done
fi

cd "$CRATE"
if ! ls "$FONTS"/*/*.ttf >/dev/null 2>&1; then
    echo "No fonts in crates/grid/examples/fonts/: run script/fetch-example-fonts first." >&2
    exit 1
fi
ln -sfn ../../grid/examples/fonts "$CRATE/www/fonts"

# Iosevka (the ops page, the desk's Heat theme) is 8.4 MB a face: the pages
# fetch a subset into www/pkg/fonts/iosevka/ instead (about 250 KB a face),
# made with HarfBuzz's hb-subset (Homebrew `harfbuzz`, Debian
# `libharfbuzz-bin`) when the face is newer: Latin, Greek, Cyrillic,
# punctuation, arrows, maths, box drawing, shapes and the symbols the
# examples draw, every OpenType feature the text uses kept, every name
# record too. Iosevka's OFL reserves no font name, so the subset keeps it;
# its licence goes next to it. Without hb-subset the whole faces are copied.
SUBSET="$OUT/fonts/iosevka"
SUBSET_UNICODES=20-7E,A0-24F,370-3FF,400-4FF,1E00-1EFF,2000-206F,20A0-20C0,2100-214F,2190-21FF,2200-22FF,2300-23FF,2500-25FF,2600-26FF,2700-27BF
mkdir -p "$SUBSET"
cp "$FONTS/iosevka/OFL.txt" "$SUBSET/OFL.txt"
for face in Iosevka-Regular Iosevka-Bold; do
    source_face="$FONTS/iosevka/$face.ttf"
    if [[ -f "$SUBSET/$face.ttf" && "$SUBSET/$face.ttf" -nt "$source_face" ]]; then
        continue
    fi
    if command -v hb-subset >/dev/null; then
        hb-subset --unicodes="$SUBSET_UNICODES" --layout-features+=tnum --name-IDs='*' \
            --output-file="$SUBSET/$face.ttf" "$source_face"
    else
        echo "No hb-subset (HarfBuzz): www/pkg/fonts/iosevka/$face.ttf is the whole face." >&2
        cp "$source_face" "$SUBSET/$face.ttf"
    fi
done
# A site build's data/ is a folder (scripts/licenses.mjs --site).
if [[ -d "$CRATE/www/data" && ! -L "$CRATE/www/data" ]]; then
    rm -rf "$CRATE/www/data"
fi
# The orbit and transit pages show how to get their data when the link
# leads nowhere.
ln -sfn ../../grid/examples/data "$CRATE/www/data"
if [[ ! -f "$CRATE/../grid/examples/data/celestrak-active.json" ]]; then
    echo "No orbital data in crates/grid/examples/data/: run script/fetch-orbit-data for the orbit page." >&2
fi
if [[ ! -f "$CRATE/../grid/examples/data/mvv-transit.tsv.gz" ]]; then
    echo "No timetable in crates/grid/examples/data/: run script/fetch-transit-data for the transit page." >&2
fi

PACKAGES=()
for app in "${BUILD[@]}"; do
    PACKAGES+=(-p "gpui-kit-grid-web-$app")
done

# A module names the source files of its panic locations: as neutral
# prefixes, never the builder's folders (a published module would carry
# the home folder's name). Cargo's folder, this repository and gpui-kit
# beside it, each as written and as resolved; the last prefix that matches
# wins, so the home folder, the widest, comes first.
remap() {
    local from=$1 to=$2
    [[ -d "$from" ]] || return 0
    REMAP+=("--remap-path-prefix=$(cd "$from" && pwd)=$to" "--remap-path-prefix=$(cd "$from" && pwd -P)=$to")
}
REMAP=()
remap "$HOME" "~"
remap "${CARGO_HOME:-$HOME/.cargo}" cargo
remap "$CRATE/../.." gpui-kit-grid
remap "$CRATE/../../../gpui-kit" gpui-kit
# Cargo takes RUSTFLAGS, when set, instead of the target's own flags, not
# beside them, and CI's toolchain action sets it (`-D warnings`): the
# remapping then joins it, or the modules would name the runner's home.
if [[ -n "${RUSTFLAGS:-}" ]]; then
    export RUSTFLAGS="$RUSTFLAGS ${REMAP[*]}"
else
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="${REMAP[*]}"
fi
# gpui-pre 0.3.8 always enables gpui_web's `multithreaded` feature, and its
# `wasm_thread` dependency needs a nightly feature gate even in this
# single-threaded build (fixed upstream after 0.3.8 by zed#65289).
# RUSTC_BOOTSTRAP lets the repository's pinned stable toolchain build it;
# nothing here uses a nightly feature otherwise.
RUSTC_BOOTSTRAP=1 cargo build --target wasm32-unknown-unknown \
    ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} "${PACKAGES[@]}"

# One app's module and glue into www/pkg/<app>/. Left as it is when cargo
# linked nothing new for the app since (`.built` names what it was made
# from), so a change to one example touches no other example's module.
package() {
    local app=$1
    local name="gpui_kit_grid_web_$app"
    local wasm="$TARGET_DIR/wasm32-unknown-unknown/$MODE/$name.wasm"
    local optimize=no
    if [[ "$MODE" == release ]] && command -v wasm-opt >/dev/null; then
        optimize=wasm-opt
    fi
    local built="$OUT/$app/.built"
    if [[ -f "$built" && "$built" -nt "$wasm" && "$(cat "$built")" == "$wasm $optimize" ]]; then
        return
    fi
    local input="$wasm"
    if [[ "$optimize" == wasm-opt ]]; then
        # Before wasm-bindgen: binaryen 116 cannot parse the `table.fill`
        # the glue adds. The flags are the features Rust's wasm32 target
        # enables.
        input="$TARGET_DIR/$name.opt.wasm"
        wasm-opt -O2 \
            --enable-bulk-memory --enable-mutable-globals --enable-sign-ext \
            --enable-nontrapping-float-to-int --enable-reference-types --enable-multivalue \
            "$wasm" -o "$input"
    fi
    rm -rf "${OUT:?}/$app"
    wasm-bindgen "$input" --out-dir "$OUT/$app" --out-name "$app" --target web --no-typescript
    echo "$wasm $optimize" >"$built"
}

mkdir -p "$OUT"
# Files directly in www/pkg/ are the one module of an earlier layout.
find "$OUT" -maxdepth 1 -type f -delete
pids=()
for app in "${BUILD[@]}"; do
    package "$app" &
    pids+=($!)
done
for pid in "${pids[@]}"; do
    wait "$pid"
done

printf '%s\n' "${APPS[@]}" | sed 's/.*/"&"/' | paste -sd, - | sed 's/.*/[&]/' >"$OUT/apps.json"
for app in "${BUILD[@]}"; do
    module="$OUT/$app/${app}_bg.wasm"
    raw=$(($(wc -c <"$module")))
    gz=$(($(gzip -9 -c "$module" | wc -c)))
    # The module's size as the page receives it (decompressed), for the
    # page's download progress: a compressed response's length is not it.
    printf '{"bytes":%d}\n' "$raw" >"$OUT/$app/module.json"
    echo "www/pkg/$app/${app}_bg.wasm: $raw bytes, $gz gzip"
done

node "$CRATE/scripts/licenses.mjs" ${SITE:+--site}
