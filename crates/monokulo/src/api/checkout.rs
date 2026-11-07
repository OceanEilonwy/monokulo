//! The payer's checkout page for an order: a page for people, served by
//! `http::checkout`, which an order links to. Keyed by the order alone (its
//! id is unguessable, and whoever has it may pay).
//!
//! The page and the routes beside it all live under `/api/v1/checkout/{order_id}`
//! and are documented in `docs/embedding-the-checkout.md`:
//!
//! - `GET  /api/v1/checkout/{order_id}`: the page (this resource).
//! - `GET  /api/v1/checkout/{order_id}/events`: its live updates, as server-sent
//!   events (`http::paths::CheckoutEventsPath`).
//! - `POST /api/v1/checkout/{order_id}/refund-address`: where its refund form
//!   posts (`http::paths::RefundAddressPath`).
//! - `GET  /api/v1/checkout/{order_id}/share`: the payment link a merchant
//!   sends, the page in a frame inside Monokulo's own page
//!   (`http::paths::CheckoutSharePath`).
//!
//! They are pages for people, not the JSON API: no key is needed or read, so
//! `http::paths::is_api` leaves them out and they keep the payer's pages'
//! own policy (embedding, CORS, the per-client budget).

use crate::api;
use crate::db::OrderId;

api::resource!(
    "/api/v1/checkout/{order_id}",
    rel = "checkout",
    params(order_id: OrderId),
    kind = page,
    doc = "The page the payer pays on: the amount, the address and its QR code, and live progress.\n\nSend the payer there, or show it in a frame on the shop's page: an `<iframe src=\"…/api/v1/checkout/{order_id}?theme=light\">` is all it takes (`monokulo-client.js` adds status callbacks on top). It works without JavaScript.\n\nQuery options, all optional: `view=detailed|compact|tiny` (the layout; the store's own choice when left out), `theme=light|dark` (pinned to match the shop; the customer's device when left out), `refund=false` (hide the refund address form), `refresh=false` (a page without JavaScript stops reloading itself).\n\nBeside it: `GET …/events` (live updates as server-sent events), `POST …/refund-address` (the refund form's target) and `GET …/share` (the page inside Monokulo's own, to send to a payer). A frame needs `sandbox=\"allow-same-origin allow-scripts allow-forms allow-popups\"` and `allow=\"camera\"` (to scan a refund QR code), and its `Referer` should say the shop's origin (`referrerpolicy=\"strict-origin\"`): a store with verified domains only lets those sites frame it.\n\nFull details: `docs/embedding-the-checkout.md`."
);
