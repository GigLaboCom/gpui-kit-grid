---
title: Privacy
description: Which cookies and browser storage this site uses and why, how to change your choice, and what the live demos connect to.
---

This site is static documentation with demos that run in your browser. It
has no accounts, no comments and no forms. This page explains which cookies
and similar technologies it uses, why, and how you control them.

Last updated: November 2026.

## Cookies and browser storage

Cookies are small pieces of data a site keeps on your device. This site
also uses your browser's local and session storage for the same purposes,
and treats them under the same rules.

## Analytics

The site counts visits with **Google Analytics 4**, loaded through **Google
Tag Manager** in Google's **Consent Mode v2**. Until you accept analytics, it
runs without cookies: Google receives a visit without an identifier that
ties it to your earlier or later visits.

- **In the European Union** a banner asks first. Nothing beyond the
  cookieless count happens until you choose.
- **Elsewhere** analytics is allowed by default. You can turn it off any
  time with the button below.

To decide whether to show the banner, the page asks this site's own server
(`/api-shared/ip`) whether your address is in the EU. Only the yes or no
comes back to the page; the address is not stored. If the server cannot
tell, the banner is shown.

## Categories

| Category | What it covers |
|---|---|
| **Essential** | Your cookie choice itself. Always on. |
| **Analytics** | Google Analytics cookies, which tell visits apart. Off until you accept in the EU. |
| **Marketing** | Advertising. The site uses none; the category records your choice in case that ever changes. |

## What is stored

| Name | Where | Purpose | Duration | Category |
|---|---|---|---|---|
| `gkg-consent` | local storage | Your cookie choice | Until you clear it | Essential |
| `gkg-eu-check` | session storage | Whether the banner applies in your region, so the check runs once | Until the tab closes | Essential |
| `_ga`, `_ga_…` | cookies | Google Analytics: tells visits apart | Up to 2 years | Analytics, only once allowed |
| `starlight-theme` | local storage | Light or Dark, once you pick it in the header (Auto removes it) | Until you clear it | Set at your request |
| `sl-sidebar-state` | session storage | Which sidebar groups are open, and how far it is scrolled | Until the tab closes | Set at your request |

Some other things stay in your browser and never leave it:

- **A theme in a link** (`#theme=…`, which [Play](../play/) writes as you
  edit) stays in the address: the part after `#` is never sent to the site.
- **The orbit demo's data**, once you ask for it, is kept in your browser's
  Cache Storage for two hours, so the demo does not download it again; so is
  a refusal from CelesTrak.

## Change your choice

<button type="button" data-cookie-settings>Cookie settings</button>

The same button is in the footer of every page. You can also clear cookies
and site data in your browser's settings, and block Google Analytics with
the [Google Analytics opt-out add-on](https://tools.google.com/dlpage/gaoptout).

## The embed script and the demos

Pages under `/embed/` and the demos under `/demo/`, which other sites load
to show a grid, carry no analytics and no banner. `live-grid.js` stores
nothing in your browser and fetches only the example it shows (its module
and fonts) from this site.

The demos run on simulated data unless you start a live one. A live demo
connects your browser directly to the public source it names on the page,
and that source sees your IP address:

- the Finnish marine traffic data of Fintraffic (digitraffic.fi);
- Wikimedia's public stream of recent changes (stream.wikimedia.org);
- CelesTrak (celestrak.org, in the United States), for the orbit demo's
  orbital elements, only when you press its download button.

The Munich demo uses the MVV's published timetable, served from this site,
and connects to no one.

## Hosting

The site runs on a server at Hetzner in Germany, behind Cloudflare. Both
process your IP address and the request to deliver the pages and protect
the service, as any web host does.

## Questions

Open an issue in the [repository](https://github.com/GigLaboCom/gpui-kit-grid/issues).
