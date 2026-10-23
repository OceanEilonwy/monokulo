//! The payer's checkout page for an order: a page for people, served by
//! `http::checkout`, which an order links to. Keyed by the order alone (its
//! id is unguessable, and whoever has it may pay).

use crate::api;
use crate::db::OrderId;

api::resource!(
    "/checkout/{order_id}",
    rel = "checkout",
    params(order_id: OrderId),
    kind = page,
    doc = "The page the payer pays on: the amount, the address and its QR code, and live progress.\n\nSend the payer there, or show it in a frame on the shop's page (`monokulo-client.js` does). It works without JavaScript."
);
