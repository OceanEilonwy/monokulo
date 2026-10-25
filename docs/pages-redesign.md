# Pages site redesign: proposal

`pages-redesign-mockup.html` (open it in a browser) mocks up a reworked
docs.monokulo.com. Its content is real: the guides' articles and the API
section come from a built site (`cargo xtask pages docs` and `pages api`),
and the OpenAPI document is reduced to what the explorer shows. None of it
is wired into `xtask/src/pages/` yet.

## What changes

**One shell for the docs and the API.** The site bar stays as it is, with
a search box added. Below it there's a left rail that maps the whole section.
That replaces the docs' row of tabs, which overflows at 8 pages ("Connecte…"
is cut off at 1400px), and the API reference's breadcrumbs.

**Docs.** The rail groups the guides as Start here (Key steps, Walkthrough),
Take payments (JS library, When is it paid?, Webhooks), Connect (Shop
plugins, Connected apps) and Reference (HTTP API, Problems, OpenAPI
document, Testing report). The article keeps its reading width. A right
column lists the page's headings as "On this page" and highlights the one
you're reading. Each page ends with Previous / Next and has an "Edit this
page on GitHub" link.

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

**Try it.** The rail has a box for your Monokulo's address and a key. It
lives in sessionStorage only and is sent only to that address, and the box
asks for a stagenet store's key. "Try it" on an operation turns its request
panel into a form: path and query parameters, plus the body, prefilled from
the example. It sends the request from the browser. The answer shows as
Live, with its status, time, headers, the body and a link to the problem's
page. Event streams are read as they arrive, with a Stop button. When the
request can't be sent, the panel says why and shows the recorded example,
marked as recorded.

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

1. Swap the tab row in `docs.rs` and the breadcrumb `page()` in
   `api_reference.rs` for one maud layout: the rail, the content, the right
   column. Give each page in `docs::PAGES` its group, and add
   `api_reference.rs`'s resources to the API rail.
2. Render the explorer from `target/api/` in Rust. The prose, fields and
   recorded examples are server-rendered, so the page reads fully with no
   JS. One small script adds the language and example switches, the
   scroll highlighting, search and the console.
3. Keep the `/rels/{rel}/` and `/problems/{slug}/` pages (the API's links
   point at them), drawn in the same shell. A rel page can be a single
   resource's slice of the explorer.
4. Colours come only from theme roles. The method and status chips need
   new roles in `tokens.css`; the mockup uses the existing tints for now.
