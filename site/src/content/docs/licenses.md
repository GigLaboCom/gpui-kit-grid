---
title: Licenses
description: The licences of gpui-kit-grid, of what its demos and this site serve, and the attributions their data sources ask for.
---

This page lists what the site and its demos are made of, under which
licence, and the attributions those licences ask for.

## gpui-kit-grid

gpui-kit-grid, its examples and this site's own pages and code are licensed
under the [Apache License 2.0](https://github.com/GigLaboCom/gpui-kit-grid/blob/main/LICENSE-APACHE). The grid is
built on [gpui-kit](https://gpui-kit.com) by Longbridge and on GPUI by Zed
Industries, both under the Apache License 2.0.

## Demos

Each demo is a WebAssembly module with its JavaScript glue, compiled from the
grid, gpui-kit, GPUI and a few hundred Rust crates, all under permissive
licences (Apache-2.0, MIT, BSD, ISC, Zlib, Unicode and similar) except two
crates under the Mozilla Public License 2.0 (`imbl` and
`imbl-sized-chunks`), compiled unchanged, whose source is on crates.io. No
crate is under the GPL, LGPL or AGPL; the build fails if one ever is.

The [demos' licence page](../demo/licenses/) has the third-party notices of
each module: every crate, its licence and the licence texts. It is generated
from the repository's `Cargo.lock` by every build of the demos.

## Data

| Demo | Source and licence | Served by this site |
|---|---|---|
| Transit (Munich) | Timetable data © Münchner Verkehrs- und Tarifverbund GmbH (MVV), [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/): “Soll-Fahrplandaten MVV-Gesamtnetz” (GTFS) from the [Munich open data portal](https://opendata.muenchen.de/). The page names the feed's version and the day it was retrieved. Changed: the city network, and the regional lines as an option, kept and compiled into the demo's own format. Scheduled, not real-time. | The compiled timetables, each with its licence note |
| Transit (Helsinki) | © HSL (Helsinki Region Transport), [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/): vehicle positions from [HSL's high-frequency positioning](https://digitransit.fi/en/developers/apis/5-realtime-api/vehicle-positions/high-frequency-positioning/), live from HSL to your browser, and the network from HSL's GTFS feed; the page shows “© HSL” with the year. Changed: the network's lines, stops and one stop pattern a line and direction kept and compiled into the demo's own format. | The compiled network and a short recording of the positions, each with its licence note |
| Wiki | [Wikimedia EventStreams](https://wikitech.wikimedia.org/wiki/Event_Platform/EventStreams_HTTP_Service); titles and pages are the wikis' content by Wikimedia contributors, [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). | A short recording of the stream, shared under CC BY-SA 4.0, with its licence note |
| Ships | Source: [Fintraffic / digitraffic.fi](https://www.digitraffic.fi/en/marine-traffic/), license [CC 4.0 BY](https://creativecommons.org/licenses/by/4.0/). | A short recording of the AIS messages, with its licence note |
| Orbit | Orbital elements from [CelesTrak](https://celestrak.org), derived from the US Space Force's public catalog. Positions are computed with SGP4 and are approximate. | Nothing: your browser downloads the elements from celestrak.org when you ask, and keeps them for two hours |
| Quotes, Desk, Broker | Simulated: see below. | Nothing |

The live demos connect your browser straight to their source: Wikimedia
when you press Start, Digitraffic when the page opens, CelesTrak when you
ask. [Privacy](../privacy/) describes what those connections reveal.

## Simulated markets

The market demos (Quotes, Desk, Broker) name real companies and funds by
their names and ticker symbols, so that the grid looks like what it is made
for. Every price, trade, order, chart and headline in them is produced by a
simulator inside the page: **simulated, not market data**, and not
investment advice. The demos are not affiliated with, sponsored or endorsed
by any company named; the names and symbols belong to their owners.

## Fonts

The demos and this site use fonts under the
[SIL Open Font License 1.1](https://openfontlicense.org): IBM Plex Sans and
IBM Plex Mono (© IBM Corp., Reserved Font Name “Plex”), Inter, Iosevka,
JetBrains Mono, Manrope and Barlow Semi Condensed. They are served
unmodified, with their licence texts next to them; the demos' licence page
has each family's copyright line and licence.

## Icons

The demos' icons come from gpui-kit, which takes them from
[Lucide](https://lucide.dev) (ISC License); the Lucide icons derived from
Feather are under the MIT License. The demos' licence page has both notices.

## This site

The site is built with [Astro](https://astro.build) (© 2021 Fred K. Schott),
[Starlight](https://starlight.astro.build) (© 2023 Astro contributors),
[Expressive Code](https://expressive-code.com) (© 2023 Tibor Schiemann)
and [Pagefind](https://pagefind.app) (© Pagefind) for search, all under the
MIT License below, and serves IBM Plex Sans and IBM Plex Mono through
[Fontsource](https://fontsource.org) under the SIL Open Font License 1.1.

```text
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
