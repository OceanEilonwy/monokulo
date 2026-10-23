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
may not use it.

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

## Versions

`/api/v1` changes only by adding: new links, forms, fields and problems.
A client that follows links and ignores what it doesn't know keeps
working. Anything that would break one becomes `/api/v2`, beside it.
