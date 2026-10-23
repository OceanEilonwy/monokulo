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
- Monokulo's own relations are written `mk:<name>`; the `mk` curie points at
  each one's page, [`/rels/{name}/`](reference/).
- Making something answers `201 Created` with its `Location`.

## Errors

An error is `application/problem+json` (RFC 9457):

```json
{
  "type": "https://docs.monokulo.com/problems/invalid-amount",
  "title": "The order can't be priced",
  "status": 400,
  "detail": "unknown currency: NOPE"
}
```

`type` is the problem's page, which says what it means and what to do;
`detail` says what went wrong this time. A busy client may be asked to
solve a small proof of work (`challenge-required`); the JS library does it
by itself.

## Versions

`/api/v1` changes only by adding: new links, forms, fields and problems.
A client that follows links and ignores what it doesn't know keeps
working. Anything that would break one becomes `/api/v2`, beside it.
