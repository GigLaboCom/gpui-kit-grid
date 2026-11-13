# syntax=docker/dockerfile:1

# The documentation site as a container image: the site/dist that CI builds
# and checks (the Astro pages, the demos under /demo/ and /embed/, rustdoc
# under /api/), served by nginx the way Cloudflare serves it. Nothing is
# compiled here: the Rust, wasm and Astro builds run once, before, and
# docker/site/stage.sh prepares the context (the header maps generated from
# the site's _headers, a .gz next to every file worth compressing), so every
# platform's image gets the same bytes.
#
#   docker/site/stage.sh target/site-image
#   docker build -t gpui-kit-grid-site target/site-image
#   docker/site/smoke.sh gpui-kit-grid-site
#   docker run -p 127.0.0.1:8080:8080 --read-only --tmpfs /tmp gpui-kit-grid-site
#
# Publish the port on 127.0.0.1 and put a proxy in front: a bare `-p 8080:8080`
# opens it on every interface of the host, past a host firewall such as ufw.

FROM nginx:1.29-alpine

LABEL org.opencontainers.image.title="gpui-kit-grid site" \
      org.opencontainers.image.description="The gpui-kit-grid documentation site and live demos, served by nginx" \
      org.opencontainers.image.licenses="Apache-2.0"

COPY nginx.conf /etc/nginx/nginx.conf
COPY site.conf /etc/nginx/conf.d/default.conf
COPY headers-map.conf headers-add.conf /etc/nginx/
COPY dist /usr/share/nginx/html

# Unprivileged on 8080: no root in the container, nothing outside /tmp is
# written, so the filesystem can be mounted read-only.
USER nginx
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -qO- http://127.0.0.1:8080/healthz >/dev/null 2>&1 || exit 1
