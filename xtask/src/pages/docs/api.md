# HTTP API

Monokulo's HTTP API lets a shop's server, a shop plugin, a connected app
or a page with the [JS library](../js-library/) make orders and follow
them: `/api/v1` on your Monokulo's own address. The
[reference](reference/) lists every resource, and the
[OpenAPI document](v1/openapi.json) has the same for tools.

## Keys

Every request names its store with a key, `Authorization: Bearer <key>`;
no URL has the store in it.

| Key | Who has it | What it may do |
| --- | --- | --- |
| `sk_…` | The shop's server: the store's secret key, which a [plugin](../plugins/) gets when it connects. | Everything about the store's orders and events, and send a plugin's logs. |
| `ak_…` | A [connected app](../apps/): its own key, made when it connects. | The same as the secret key, and read or disconnect its own connection. |
| `pk_…` | A browser page: the store's public key. It proves nothing. | Make an order and read it. A store that takes orders only from its verified websites refuses one from anywhere else. |

`GET /api/v1` takes no key, so a client can check it found a Monokulo. A
route that needs a key answers `401` without one, and `403` with one that
may not use it. A key this Monokulo doesn't know (mistyped, replaced, or a
disconnected app's) is `401` on every route, the entry included: never the
entry as if no key had been sent.

## Find your way

Start at the entry, `GET /api/v1`, with your key. It links everything your
key may read and carries every form it may use, so creating an order is the
`create-order` form's `target`, not a path you build.

The entry can be kept: it answers `Cache-Control: private, max-age=3600`,
`Vary: Authorization` and an `ETag`.

- Keep it for as long as `max-age` says, per key, and use its links and
  forms from your copy: making an order is then one request.
- When it has expired, ask again with `If-None-Match: <its ETag>`. While it
  is the same you get `304 Not Modified` with no body, and keep your copy
  for another `max-age`.
- Read it again at once when something you followed from it answers `401`,
  `404` or `405`: your key changed, or the API moved on.

Everything else (the store, orders, events) is `Cache-Control: no-store`:
read it each time.

## Follow links, use forms

Resources are HAL-FORMS (`application/prs.hal-forms+json`). Each has
`_links`, where to go next, and `_templates`, the forms its key may use
there: a form's `method`, `target` and `properties` say what to send, with
each field's rules.

```json
{
  "_links": {
    "self": { "href": "/api/v1" },
    "mk:orders": { "href": "/api/v1/orders", "title": "The store's orders, newest first, a page at a time." },
    "curies": [{ "name": "mk", "href": "https://docs.monokulo.com/rels/{rel}", "templated": true }]
  },
  "_templates": {
    "create-order": { "method": "POST", "target": "/api/v1/orders", "contentType": "application/json", "properties": [ … ] }
  }
}
```

- A link or form the key may not use is left out, so a client finds out
  what its key does by reading the entry.
- Links are paths on Monokulo's own host. Resolve them against the URL you
  called: a client that came in over the onion address stays on it.
- Onion links are the exception: see [Tor](#tor).
- Monokulo's own relations are written `mk:<name>`; the `mk` curie points at
  each one's page, [`/rels/{name}/`](reference/).
- Making something answers `201 Created` with its `Location`.
- An order links its payer's page, `mk:checkout`, and, for the store's
  secret key only, its page on the merchant's dashboard,
  [`mk:dashboard-order`](../rels/dashboard-order/): show that one to the
  merchant ("View in Monokulo") rather than building its URL.

### Under a path

A Monokulo behind a reverse proxy can be served under a path,
`https://example.com/monokulo/`, the proxy passing
`https://example.com/monokulo/api/v1` on as `/api/v1`. Its admin then sets
`public_uri` to `https://example.com/monokulo`, and every link, form
`target` and `Location` the API gives starts with `/monokulo`, as does the
`api` the plugin and app handshakes give. Resolved against the host you
called, they reach Monokulo; build nothing from `/api/v1` yourself. Onion
links have no such path: tor reaches Monokulo directly. Monokulo's own
pages, the dashboard, the POS and the checkout, work under the path too:
their links, forms, redirects, scripts, live updates and session cookie
all stay under it.

## Amounts

An amount is a decimal string in its currency, with up to 12 decimals in
any currency (XMR's own precision). Trailing zeros don't count:
`12.50000000` is `12.50`. Monokulo keeps the amount as you sent it and
gives it back unchanged; its pages show it with the currency's usual
decimals.

## Tor

While a Monokulo has an onion address (Tor inside it, or an external tor),
its responses also carry absolute links to the same things on that address,
whichever address you called. Without one they're absent.

- Every order (made, read, listed, or sent on its event stream) has
  [`mk:onion-checkout`](../rels/onion-checkout/): its checkout page on the
  onion address.
- The entry, `GET /api/v1`, has [`mk:onion-api`](../rels/onion-api/): the
  entry on the onion address. Everything you follow from there is relative,
  so you stay on Tor.
- The plugin and app handshakes give the same URL as `onion_api`.

```json
{
  "order_id": "order_9f3c…",
  "_links": {
    "self": { "href": "/api/v1/orders/order_9f3c…" },
    "mk:checkout": { "href": "/checkout/order_9f3c…", "type": "text/html" },
    "mk:onion-checkout": { "href": "http://2gzy…wid.onion/checkout/order_9f3c…", "type": "text/html" }
  }
}
```

Anyone on Tor must be sent to the onion links, never to a link resolved
against the web address: a shop server that calls Monokulo over the web
address but serves a customer on the shop's own `.onion` sends them to
`mk:onion-checkout`. Nothing Monokulo serves over its onion address points
at its web address.

## Errors

An error is `application/problem+json` (RFC 9457), in the API and on every
other route a machine calls (the plugin and app handshakes, the point of
sale, the checkout's own scripts); only pages for people answer with HTML:

```json
{
  "type": "https://docs.monokulo.com/problems/invalid-amount",
  "title": "The order can't be priced",
  "status": 400,
  "detail": "unknown currency: NOPE"
}
```

`type` is the problem's page, which says what it means and what to do;
`detail` says what went wrong this time. Every problem is listed in the
[reference](reference/). A busy client may be asked to
solve a small proof of work (`challenge-required`); the JS library does it
by itself.

## Event streams

`mk:events` (the store's sales, for a secret or app key) and
`mk:order-events` (one order) are server-sent events
(`text/event-stream`): each event an `event:` line with its name and a
`data:` line, mostly JSON, and a comment now and then while nothing
happens. A sale event links its order, so you follow the link rather than
build the order's URL:

```json
{
  "event": "sale.paid",
  "order_id": "order_9f3c…",
  "amount": "24.50",
  "currency": "USD",
  "_links": {
    "mk:order": { "href": "/api/v1/orders/order_9f3c…", "title": "One order: what to pay, where, and how far the payment has got." },
    "curies": [{ "name": "mk", "href": "https://docs.monokulo.com/rels/{rel}", "templated": true }]
  }
}
```

Each stream's page in the [reference](reference/) lists its events and
their data; the OpenAPI document has them as `x-events` on the
`text/event-stream` answer (OpenAPI 3.1 has no schema for a stream's
events), with a recorded stream as its example.

## The OpenAPI document

Every Monokulo serves its [OpenAPI 3.1 document](v1/openapi.json) at
`/api/v1/openapi.json` (the entry's `service-desc`). It has every route,
who may call it, what it takes and answers; the pages the API links
(`text/html`); the plugin and app handshakes with every problem they
answer; and each stream's events. The copy on this site also has examples,
real exchanges recorded against a test engine: the entry and the store as
each key sees them, orders, streams, problems.

To generate a client at a pinned Monokulo commit, write the same document
from its source:

```
git clone https://github.com/OceanEilonwy/monokulo && cd monokulo
cargo xtask api openapi --out openapi.json
```

It needs Rust only: `rustup` installs the toolchain the repository names
(`rust-toolchain.toml`: nightly, with the WebAssembly target) by itself.
It builds monokulo's library and one example, no tests and no browser
code, so neither Node nor `npm ci` is needed. The example records the
exchanges against an in-process test engine, checks each against the
document, and writes it with them as examples.

## Versions

`/api/v1` changes only by adding: new links, forms, fields and problems.
A client that follows links and ignores what it doesn't know keeps
working. Anything that would break one becomes `/api/v2`, beside it.
