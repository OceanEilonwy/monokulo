# Embedding the checkout

The payer's checkout is a page for people, served by Monokulo under the
versioned API root: `/api/v1/checkout/{order_id}`. A shop shows it in an
`<iframe>`, or sends the payer to it. It needs no key and works without
JavaScript. `monokulo-client.js` is optional: it frames the same page and adds
status callbacks (`onPaid`, `onExpired`).

An order links to its page as `mk:checkout` (`checkout_url`), and to the same
page on the onion address as `mk:onion-checkout` while Monokulo has one. Build
the URL from that link, not by hand, so a later move of the page costs a shop
nothing.

## The routes

All four are under `/api/v1/checkout/{order_id}`. The order id is unguessable;
whoever has it may pay, so there is no key. They are pages for people, not the
JSON API: no `Authorization` header is read, errors are pages, and the
per-client budget and challenge are the payer's pages' (see `http::abuse`).

| Route | What it is |
|---|---|
| `GET /api/v1/checkout/{order_id}` | The page: the amount, the address and its QR code, live progress. |
| `GET /api/v1/checkout/{order_id}/events` | The page's live updates, as server-sent events (`status`, then `done` once the order is final). |
| `POST /api/v1/checkout/{order_id}/refund-address` | Where the page's refund address form posts. |
| `GET /api/v1/checkout/{order_id}/share` | The payment link a merchant sends: the page in a frame inside Monokulo's own page. |

The page is also in the OpenAPI document (`/api/v1/openapi.json`, tag `pages`).
A missing order is `404`.

## Query options

All optional, on the page (and carried to `events` and `refund-address` by the
page itself).

| Option | Values | Effect |
|---|---|---|
| `view` | `detailed`, `compact`, `tiny` | The layout. Left out, the store's own choice (the store's Checkout tab). |
| `theme` | `light`, `dark` | Pins the theme to match the shop. Left out, the customer's device decides. |
| `refund` | `false` | Hides the refund address form. |
| `refresh` | `false` | Without JavaScript, stops the page reloading itself every minute. |

## Framing it

```html
<iframe
  title="Monero payment"
  src="https://monokulo.example/api/v1/checkout/order_…?theme=light"
  sandbox="allow-same-origin allow-scripts allow-forms allow-popups"
  allow="camera"
  referrerpolicy="strict-origin"
  style="width:100%;border:0;height:880px"></iframe>
```

- **`sandbox`**: the page's script, its refund form and its links out need
  exactly these.
- **`allow="camera"`**: the refund form scans a QR code of an address.
- **`referrerpolicy="strict-origin"`**: says which site is framing the page,
  even from a page that sends no `Referer`. A store that has verified domains
  (the store's Domains tab) only lets those sites frame it
  (`Content-Security-Policy: frame-ancestors`); anywhere else the frame is
  refused. Monokulo's own pages can always frame it.
- **Height**: the page is cross-origin to the shop, so its content can't be
  measured. Use the layout's size: `tiny` 152px, `compact` 380px, `detailed`
  880px. The page scrolls inside the frame beyond that.
- **Cookies**: the page sets none inside a frame (a cross-site `SameSite=Lax`
  cookie is refused anyway); the browser's time zone goes in an `X-Timezone`
  header on its requests instead.

A shop with a Content-Security-Policy needs `frame-src` for the Monokulo
address, and nothing else: the page is not a script the shop runs.

## Knowing it was paid

The page moves to its paid state by itself. For the shop, treat the browser as
a hint only: an order is paid when Monokulo says so, by the signed
`order.paid` webhook, or by reading the order (`GET /api/v1/orders/{order_id}`)
with the store's key. A shop that frames the page can poll its own server for
that, as Handicrafts does, and reload once it answers.

## Versioning

These routes are part of `/api/v1`: they change only as the rest of `/api/v1`
does. The unversioned `/checkout/{order_id}` routes that came before them no
longer exist.
