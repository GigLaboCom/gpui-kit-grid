# Shared by the hooks (sourced, not run): the private pattern lists and the
# gitleaks binary, for this repository and, when it is a submodule of
# gpui-kit-grid (custom-docs), for the superproject as well.

# The superproject's top level when this repository is its submodule.
super="$(git rev-parse --show-superproject-working-tree 2>/dev/null || true)"
common="$(git rev-parse --path-format=absolute --git-common-dir)"
root="$(git rev-parse --show-toplevel)"

# The private patterns, one extended regular expression per line, from
# <git common dir>/info/privacy-patterns of this repository and of the
# superproject. The files live inside .git and are never committed.
private_patterns() {
  local dirs=("$common") list
  if [[ -n "$super" ]]; then
    dirs+=("$(git -C "$super" rev-parse --path-format=absolute --git-common-dir)")
  fi
  for list in "${dirs[@]/%//info/privacy-patterns}"; do
    if [[ -f "$list" ]]; then grep -vE '^[[:space:]]*(#|$)' "$list" || true; fi
  done | awk '!seen[$0]++'
}

# Prints the gitleaks binary: on PATH, or target/tools/bin/gitleaks of this
# checkout, of the main checkout (from a worktree) or of the superproject.
# Only one that runs here counts: a checkout shared with a container may
# hold a binary built for the other system.
find_gitleaks() {
  local bin
  for bin in "$(command -v gitleaks 2>/dev/null || true)" \
    "$root/target/tools/bin/gitleaks" \
    "$(dirname "$common")/target/tools/bin/gitleaks" \
    "${super:+$super/target/tools/bin/gitleaks}"; do
    if [[ -n "$bin" && -x "$bin" ]] && "$bin" version >/dev/null 2>&1; then
      echo "$bin"
      return 0
    fi
  done
  return 1
}

# Prints the gitleaks configuration to use: this repository's .gitleaks.toml,
# else the superproject's; nothing for the default rules.
gitleaks_config() {
  local dir
  for dir in "$root" "$super"; do
    if [[ -n "$dir" && -f "$dir/.gitleaks.toml" ]]; then
      echo "$dir/.gitleaks.toml"
      return 0
    fi
  done
}

gitleaks_missing() {
  echo "gitleaks is not installed. Install it on PATH or as"
  echo "target/tools/bin/gitleaks (https://github.com/gitleaks/gitleaks/releases,"
  echo "check the release's checksums), or skip this hook deliberately with --no-verify."
}
