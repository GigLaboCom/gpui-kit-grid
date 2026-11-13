# The web examples in the browser (single-threaded GPUI web platform), each
# its own module and page: the quotes grid (`quotes/`), the desk (`desk/`),
# the broker (`broker/`), orbit (`orbit/`) and transit (`transit/`), listed
# on the index page. `make build` builds them all (APP=desk for one,
# APP=desk,orbit for some), then `make serve` and open the address it
# prints (PORT=8091 for another port). `make measure` runs a page in
# headless Chrome and prints frame times (QUERY=app=desk for the desk;
# QUERY=app=orbit RATES=1,60 for orbit at time ×1 and ×60;
# QUERY=app=transit&time=2026-11-10T07:45 RATES=1,60 for transit at a
# Tuesday's morning rush). `make gateway` serves the quotes over WebSocket
# for `quotes/?feed=ws://127.0.0.1:9001`; `make measure-feed` measures the
# page fed by it, `make input` checks the quotes' high-DPI rendering and
# input, `make examples` the desk's, the broker's and orbit's views and
# keys, `make survive` that every page survives the gestures that once
# stopped it (drags, and copying over plain http), `make embed-check` that
# `<live-grid>` still works on another site's page (run it on every
# gpui-kit or GPUI upgrade). The checks serve www/ themselves, or run
# against a served one with ORIGIN=http://127.0.0.1:8091/. `make site-dist`
# stages the site build under the site's dist/demo/, `make posters` draws
# the site's poster of each example.

.PHONY: build build-dev site site-dist posters licenses serve gateway measure measure-feed input examples survive embed-check orbit-site clean

build: ## Release build into www/pkg/<app>/ (APP=desk for one)
	./scripts/build-wasm.sh --release

build-dev: ## Debug build into www/pkg/<app>/ (APP=desk for one)
	./scripts/build-wasm.sh

site: ## Release build for the public site: notices required, only servable data, orbit from CelesTrak
	SITE=1 ./scripts/build-wasm.sh --release

site-dist: site ## The site build staged as files into the site's dist/demo/ (DIST=<dir>; API=<target/doc> for rustdoc in dist/api/), checked against the host's limits
	node scripts/site-dist.mjs $(DIST) $(if $(API),--api=$(API))

posters: ## A WebP poster of each example from its recording, fixed time or seed, into site/public/posters/ (OUT=<dir>, APPS=quotes)
	node scripts/posters.mjs

licenses: ## The licences and third-party notices of what the pages serve, into www/licenses/
	node scripts/licenses.mjs

serve: ## Serve www/ on http://127.0.0.1:8080 (PORT=8091 for another)
	node scripts/serve.mjs $(PORT)

gateway: ## The quotes gateway on ws://127.0.0.1:9001 (ARGS="--rate 5000" for more)
	cargo run --release --manifest-path ../grid-gateway/Cargo.toml -- $(ARGS)

measure: ## Load and frame times in headless Chrome (OUT=<dir>, QUERY=app=desk)
	node scripts/measure.mjs

measure-feed: ## Latency, conflation, resyncs and reconnects with the gateway (build it first)
	node scripts/measure-feed.mjs

input: ## High-DPI capture and input in headless Chrome (OUT=<dir> for the captures)
	node scripts/input.mjs

examples: ## The desk's, the broker's and orbit's views and keys in headless Chrome (OUT=<dir>, APPS=orbit)
	node scripts/examples.mjs

survive: ## Drags and copies in headless Chrome, each page alive after (OUT=<dir>, APPS=desk)
	node scripts/survive.mjs

embed-check: ## <live-grid> inline and in a frame on another site's page, in headless Chrome (OUT=<dir>, APPS=quotes,wiki)
	node scripts/embed-check.mjs

orbit-site: ## The orbit page's CelesTrak mode in headless Chrome, CelesTrak mocked
	node scripts/orbit-site.mjs

clean:
	rm -rf www/pkg www/licenses target
