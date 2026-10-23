//! The payer's pages beside the checkout itself (`crate::api::checkout`,
//! which the API links to): one type per route. The router registers each
//! route from its type (`RouterExt::typed_get` and friends), and every link
//! and form target to it is the same type printed, so a path can't be
//! spelled two ways. Plus telling the public routes apart in middleware.

use axum_extra::routing::TypedPath;
use serde::Deserialize;

use crate::db::OrderId;

/// The checkout inside monokulo's own page, to share with a payer.
#[derive(TypedPath, Deserialize, Clone, Debug)]
#[typed_path("/checkout/{order_id}/share")]
pub struct CheckoutSharePath {
    pub order_id: OrderId,
}

/// The checkout's live updates, as server-sent events.
#[derive(TypedPath, Deserialize, Clone, Debug)]
#[typed_path("/checkout/{order_id}/events")]
pub struct CheckoutEventsPath {
    pub order_id: OrderId,
}

/// Where the checkout's refund address form posts.
#[derive(TypedPath, Deserialize, Clone, Debug)]
#[typed_path("/checkout/{order_id}/refund-address")]
pub struct RefundAddressPath {
    pub order_id: OrderId,
}

/// Whether `path` is the API's (`crate::api`).
pub fn is_api(path: &str) -> bool {
    path.strip_prefix(crate::api::v1::root::Path::TEMPLATE)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The order a `/checkout/{order_id}/...` path is for.
pub fn checkout_order(path: &str) -> Option<OrderId> {
    let template = crate::api::checkout::Path::TEMPLATE;
    let prefix = template.strip_suffix("{order_id}").unwrap_or(template);
    let segment = path.strip_prefix(prefix)?.split('/').next()?;
    if segment.is_empty() {
        return None;
    }
    url::form_urlencoded::parse(format!("x={}", segment.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, value)| OrderId::new(value.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_prints_as_its_route_with_the_order_in_it() {
        let order_id = OrderId::new("order_1");
        assert_eq!(
            crate::api::checkout::Path {
                order_id: order_id.clone()
            }
            .to_string(),
            "/checkout/order_1"
        );
        assert_eq!(
            CheckoutSharePath { order_id }.to_string(),
            "/checkout/order_1/share"
        );
    }

    #[test]
    fn a_checkout_path_names_its_order() {
        assert_eq!(
            checkout_order("/checkout/order_1/events"),
            Some(OrderId::new("order_1"))
        );
        assert_eq!(
            checkout_order("/checkout/order_1"),
            Some(OrderId::new("order_1"))
        );
        assert_eq!(checkout_order("/checkout/"), None);
        assert_eq!(checkout_order("/api/v1/orders/order_1"), None);
    }

    #[test]
    fn only_the_api_s_paths_are_the_api_s() {
        assert!(is_api("/api/v1"));
        assert!(is_api("/api/v1/orders"));
        assert!(!is_api("/api/v10"));
        assert!(!is_api("/checkout/order_1"));
    }
}
