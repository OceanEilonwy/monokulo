//! An order's page on the merchant's dashboard: a page for people, served
//! by `http::orders::order_detail` to the merchant signed in to it, which
//! an order links to for a caller that acts for the merchant.

use crate::api;
use crate::db::{ConnectionId, OrderId};

api::resource!(
    "/stores/{id}/orders/{order_id}",
    rel = "dashboard-order",
    params(id: ConnectionId, order_id: OrderId),
    kind = page,
    who = sk | session,
    doc = "The order's page on the merchant's dashboard: everything Monokulo knows about the order and its payment.\n\nFor the merchant, signed in to Monokulo: a shop plugin links it from the shop's own order page (\"View in Monokulo\"), so it never builds the URL itself. It is offered only to the store's secret key (`sk_`) and the merchant's session; a browser's public key and a connected app's key don't get it."
);
