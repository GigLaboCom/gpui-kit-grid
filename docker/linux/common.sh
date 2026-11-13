# Shared settings of the docker/linux scripts; sourced, not run.
#
# The repository is mounted at /work/gpui-kit-grid (read-write: out/ is
# written there). Cargo fetches the pinned gpui-kit revision itself. When a
# gpui-kit checkout sits next to the repository (`../gpui-kit`), it is mounted
# at /work/gpui-kit, read-only, so a local `[patch]` in
# `.cargo/config.local.toml` resolves as on the host. In a worktree
# `../gpui-kit` may be a symlink: it is resolved here, since a symlink does not
# cross a bind mount. The cargo registry, cargo's git checkouts and target/
# live in named Docker volumes.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJ="$(cd "$HERE/../.." && pwd)"
GPUI_KIT_MOUNT=()
if GPUI_KIT="$(cd "$PROJ/../gpui-kit" 2>/dev/null && pwd -P)"; then
  GPUI_KIT_MOUNT=(-v "$GPUI_KIT:/work/gpui-kit:ro")
fi
IMAGE=gpui-kit-grid-linux
CARGO_VOLUME=gpui-kit-grid-linux-cargo
CARGO_GIT_VOLUME=gpui-kit-grid-linux-cargo-git
# target/ is per checkout: two checkouts sharing one volume fight over the
# cargo lock and overwrite each other's binaries. The main checkout keeps
# the plain name (its cache stays warm); a linked git worktree appends its
# directory name; GPUI_KIT_GRID_TARGET_VOLUME sets the volume explicitly.
# A new worktree volume starts cold (the first build is slow): copying the
# main volume while its owner may be building could carry half-written
# artifacts.
MAIN_TARGET_VOLUME=gpui-kit-grid-linux-target
if [ -n "${GPUI_KIT_GRID_TARGET_VOLUME:-}" ]; then
  TARGET_VOLUME="$GPUI_KIT_GRID_TARGET_VOLUME"
elif [ -f "$PROJ/.git" ]; then
  # A linked worktree has a `.git` file, the main checkout a directory.
  name="$(basename "$PROJ" | tr -c 'A-Za-z0-9_.-\n' '-')"
  TARGET_VOLUME="$MAIN_TARGET_VOLUME-$name"
else
  TARGET_VOLUME="$MAIN_TARGET_VOLUME"
fi
OUT="$PROJ/out/linux"
mkdir -p "$OUT"

# Debug info is most of a debug build's size and link time, and nothing here
# reads it (as in CI).
DOCKER_RUN=(docker run --rm --init
  -v "$PROJ:/work/gpui-kit-grid"
  ${GPUI_KIT_MOUNT[@]+"${GPUI_KIT_MOUNT[@]}"}
  -v "$CARGO_VOLUME:/usr/local/cargo/registry"
  -v "$CARGO_GIT_VOLUME:/usr/local/cargo/git"
  -v "$TARGET_VOLUME:/target"
  -e CARGO_PROFILE_DEV_DEBUG=0
  -e CARGO_TERM_COLOR=never
  -w /work/gpui-kit-grid
  "$IMAGE")

# Builds the image if it is missing or the Dockerfile changed (cached layers
# otherwise).
ensure_image() {
  docker build -q -t "$IMAGE" "$HERE" >/dev/null
}
