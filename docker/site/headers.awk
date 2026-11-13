# Turns the site's `_headers` (header rules in the format of Cloudflare's
# static assets, site/public/_headers) into nginx configuration, so the
# rules live in one file.
#
#   awk -v add=headers-add.conf -f headers.awk _headers > headers-map.conf
#
# For every header name, one `map $uri` (http level, standard output) from
# each rule's path to its value, and one `add_header` (server level, the file
# `add`); nginx leaves a header out when its value is empty. A path's `*`
# matches anything, `:name` one segment. Cloudflare joins a header two rules
# set with a comma; a map takes the first match, so two rules may not set the
# same header on overlapping paths (`_headers` already keeps to that). Any
# other syntax (absolute URLs, `! Header` removals) fails the build.

function fail(msg) {
  printf "headers.awk: line %d: %s\n", NR, msg > "/dev/stderr"
  failed = 1
  exit 1
}

function regex(path, out) {
  out = path
  gsub(/[.+?()|{}^$\[\]\\]/, "\\\\&", out)
  gsub(/\*/, ".*", out)
  gsub(/:[A-Za-z_][A-Za-z0-9_]*/, "[^/]+", out)
  return "^" out "$"
}

function quote(s) {
  gsub(/\\/, "\\\\", s)
  gsub(/"/, "\\\"", s)
  return "\"" s "\""
}

/^[[:space:]]*(#|$)/ { next }

/^\// {
  path = $0
  sub(/[[:space:]]+$/, "", path)
  if (path ~ /[[:space:]]/) fail("a path with spaces: " path)
  rule = regex(path)
  next
}

/^[[:space:]]+[A-Za-z0-9-]+:/ {
  if (rule == "") fail("a header before any path")
  line = $0
  sub(/^[[:space:]]+/, "", line)
  name = line
  sub(/:.*/, "", name)
  value = line
  sub(/^[^:]*:[[:space:]]*/, "", value)
  sub(/[[:space:]]+$/, "", value)
  key = tolower(name)
  if (!(key in names)) {
    order[++count] = key
    names[key] = name
  }
  rules[key] = rules[key] sprintf("    %s %s;\n", quote("~" rule), quote(value))
  next
}

{ fail("not a path, a header or a comment: " $0) }

END {
  if (failed) exit 1
  if (count == 0) {
    print "headers.awk: no rules" > "/dev/stderr"
    exit 1
  }
  print "# Generated from the site's _headers by docker/site/headers.awk."
  printf "# Generated from the site's _headers by docker/site/headers.awk.\n" > add
  for (i = 1; i <= count; i++) {
    key = order[i]
    var = "site_header_" key
    gsub(/-/, "_", var)
    printf "map $uri $%s {\n    default \"\";\n%s}\n", var, rules[key]
    printf "add_header %s $%s;\n", names[key], var > add
  }
}
