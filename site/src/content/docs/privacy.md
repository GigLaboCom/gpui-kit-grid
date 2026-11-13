---
title: Privacy
description: What this site stores, what it measures and for whom, and what the live demos connect to.
---

This site is static documentation with demos that run in your browser. It
has no accounts, no comments and no forms.

## Analytics

- **In the European Economic Area, the United Kingdom and Switzerland**
  the site loads no analytics and sets no cookies. Nothing about your visit
  is measured.
- **Elsewhere** the site counts visits with Google Analytics 4, loaded by
  Cloudflare Zaraz: pages viewed, the referring page, the browser and device
  type, and an approximate location from your IP address. Google Signals and
  ad personalisation are off, and the data is kept for two months. Google
  Analytics sets its own cookies (`_ga` and `_ga_…`); you can block them in
  your browser or with the
  [Google Analytics opt-out add-on](https://tools.google.com/dlpage/gaoptout).

Which group you are in is decided by Cloudflare from your IP address on each
visit. If it cannot tell, the site loads no analytics.

Pages under `/embed/` and the demos under `/demo/`, which other sites load
to show a grid, never carry analytics.

## What your browser keeps

- **The colour scheme you pick** in the header: Light or Dark is kept in
  your browser's local storage (`starlight-theme`), so the next page uses
  it, and only once you pick it. Picking Auto removes it again. Until you
  pick, nothing is stored.
- **The sidebar's state** (which groups are open, how far it is scrolled)
  stays in the tab's session storage (`sl-sidebar-state`) while you move
  between pages, and goes when you close the tab.
- **A theme in a link** (`#theme=…`, which [Play](../play/) writes as you
  edit) stays in the address: the part after `#` is never sent to the site.
  Play keeps nothing else.
- **The orbit demo's data**, once you ask for it, is kept in your browser's
  Cache Storage with the time it was downloaded, for two hours, so the demo
  does not download it again; so is a refusal from CelesTrak, for the same
  time. Your browser's site data settings clear it.

None of these leave your browser.

## The embed script

`live-grid.js` under `/embed/`, which other sites load to show a grid,
stores nothing in your browser and loads no analytics. It fetches only the
example it shows (its module and fonts) from this site.

## Hosting

The site is served by Cloudflare, which processes your IP address and the
request to deliver the pages and protect the service, as any web host does.

## Live demos

The demos run on simulated data unless you start a live one. A live demo
connects your browser directly to the public source it names on the page,
and that source sees your IP address:

- the Finnish marine traffic data of Fintraffic (digitraffic.fi);
- Wikimedia's public stream of recent changes (stream.wikimedia.org).
- CelesTrak (celestrak.org, in the United States), for the orbit demo's
  orbital elements, only when you press its download button.

The Munich demo uses the MVV's published timetable, served from this site,
and connects to no one.

## Questions

Open an issue in the [repository](https://github.com/GigLaboCom/gpui-kit-grid/issues).
