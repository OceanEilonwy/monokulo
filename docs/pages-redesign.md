# Pages site redesign: proposal

`pages-redesign-mockup.html` (open it in a browser) mocks up a reworked
docs.monokulo.com. Its content is real: the guides' articles and the API
section come from a built site (`cargo xtask pages docs` and `pages api`),
and the OpenAPI document is reduced to what the explorer shows. None of it
is wired into `xtask/src/pages/` yet.

## What changes

**Header.** The current links mix a page's own sections (How it works,
Install) with the site's parts (Docs, Testing, Stylebook). The proposal
follows what Stripe, Supabase, Vercel and Tailscale do: the logo goes home,
and there are three content links plus a menu.

| Now | Proposed | Why |
| --- | --- | --- |
| How it works, Install | **Product** (the homepage) | Sections of one page don't belong in site navigation |
| Docs | **Docs** | Guides, install and operating, in one sidebar |
| API | **API reference** | The usual name; it says this is the per-route reference |
| Testing, Stylebook | **Resources ▾**: Releases, Test reports, Design system, Report an issue | Useful, but not where most readers go first |
| Source code | **GitHub** (icon and label) on the right, beside search | Where readers look for it |
| | **Get started** (the one orange button), on the product page only | Takes a new reader to the docs' Introduction |

The brand no longer repeats the section ("Monokulo | API" beside an API
link); the current link is marked instead.

**Homepage: about the product.** The hero and the six feature tiles stay.
New on it: an order from start to finish (checkout, paid in about a second,
`order.paid`), four ways to take payments that each link into the docs, a
two-call code sample, an install band, and a footer. The desktop and
OpenWrt install guides move into the docs; the homepage links to them.

**One shell for the docs and the API.** A left rail maps the whole section.
That replaces the docs' row of tabs, which overflows at 8 pages ("Connecte…"
is cut off at 1400px), and the API reference's breadcrumbs. On a phone the
rail is a menu, with the site links at its top. The rail ends with links to
the source on GitHub, Releases and Report an issue.

**Docs sidebar.** Each group has a heading in ink, an icon and a rule above
it, with its pages indented below:

- Get started: Introduction (new), Install on a computer, Install on a
  router, Accept your first payment (was Key steps), Walkthrough: Geomart
- Take payments: JS library, When is it paid?, Webhooks
- Integrations: Shop plugins, Connected apps
- Run Monokulo: Config and data, Backups and updates
- Reference: API reference, Problems, OpenAPI document

The install, config and backups pages are the homepage's own words, split
by task: "Where your config and data live" and the OpenWrt config tile are
one page, and the desktop backups tile plus OpenWrt's Updating and
Removing are another. The article keeps its reading width. A right column
lists the page's headings as "On this page" and highlights the one you're
reading. Each page ends with Previous / Next and has an "Edit this page on
GitHub" link.

**API explorer, laid out like Stripe's or Scalar's.** It's one long page:

- the introduction (`api.md`), with the base URL, the three keys and a
  first request beside it;
- each resource (`mk:orders`, …), with its description and its routes;
- each operation in two columns. The left column has the method and path,
  who may call it (key chips), the description, parameters, body fields
  (from the schema) and the responses by status. The right column stays in
  view: the request as curl, JavaScript or raw HTTP, and the recorded
  response. You can pick the status and the example variant (`sk`, `pk`,
  `-with-onion`, …).
- the problems table.

The rail lists every operation with a method chip and highlights the one on
screen.

**Try it.** A banner across the top of the API reference says where
requests go and with which key, with a dot once the connection is checked.
Its Settings button (a cog) opens the Try it dialog. "Try it" on any
operation opens the same dialog: the connection (address and key, with
Check connection) at the top, then that operation's form (path and query
parameters, plus the body, prefilled from the example) and its response.
The key lives in sessionStorage only and is sent only to that address, and
the dialog asks for a stagenet store's key. The answer shows as Live, with
its status, time, headers, the body and a link to the problem's page. It
stays beside the operation after the dialog closes. Event streams are read
as they arrive, with a Stop button. When the request can't be sent, the
panel says why and shows the recorded example, marked as recorded.

**Search.** Press `/` or Ctrl+K to search guides, their sections,
endpoints and problems from one box.

## What it relies on in monokulo

The console works against a real Monokulo today because `/api/v1` already
answers CORS (`http::embed_cors_layer`). It answers any origin for `sk_` and
`ak_` keys, and a `pk_` request only from the store's verified domains, if
it has any. Credentials are never allowed. Two gaps:

- `DELETE /api/v1/app` can't be tried: `cors_layer_base` allows GET and
  POST only. The console says so and disables Send.
- A `pk_` key may get `429 challenge-required`. The console shows that 429
  as it is; it doesn't solve the proof of work.

## Building it for real

1. Change `site_header.rs` to the new links (Product, Docs, API reference,
   Resources, GitHub). Move the landing page's install sections into docs
   Markdown pages, and keep `#install`, `#desktop` and `#openwrt` on the
   homepage as links to them, so old links still land somewhere.
2. Swap the tab row in `docs.rs` and the breadcrumb `page()` in
   `api_reference.rs` for one maud layout: the rail, the content, the right
   column. Give each page in `docs::PAGES` its group, and add
   `api_reference.rs`'s resources to the API rail.
3. Render the explorer from `target/api/` in Rust. The prose, fields and
   recorded examples are server-rendered, so the page reads fully with no
   JS. One small script adds the language and example switches, the
   scroll highlighting, search and the console.
4. Keep the `/rels/{rel}/` and `/problems/{slug}/` pages (the API's links
   point at them), drawn in the same shell. A rel page can be a single
   resource's slice of the explorer.
5. Colours come only from theme roles. The method and status chips need
   new roles in `tokens.css`; the mockup uses the existing tints for now.
