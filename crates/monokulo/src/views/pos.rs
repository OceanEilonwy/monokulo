//! Merchant POS shell. Solid 2 owns the whole terminal, including its own
//! payment card (QR, address, refund address); it does not embed the public
//! checkout page.

use maud::{html, Markup};

use super::{layout_bare_with_head, logo_mark, status_indicator, theme_toggle, PageChrome};

pub struct PosViewModel {
    pub connection_id: crate::db::ConnectionId,
    pub public_key: String,
    pub display_name: String,
    pub base_currency: String,
    pub base_currency_decimals: u8,
    /// The store opted in to client logs: the POS records and sends its
    /// session timeline (`pos-ui/src/timeline.ts`).
    pub client_logging: bool,
}

pub fn page(chrome: &PageChrome, data: &PosViewModel) -> Markup {
    let title = format!("POS - {} - Monokulo", data.display_name);
    let head = html! { link rel="stylesheet" href="/static/pos-app.css"; };
    let body = html! {
        div id="pos-root"
            data-connection-id=(data.connection_id)
            data-public-key=(data.public_key)
            data-store-name=(data.display_name)
            data-currency=(data.base_currency)
            data-decimals=(data.base_currency_decimals)
            data-client-logging=(data.client_logging) {}
        // The site's logo, status indicator and theme toggle, rendered here
        // so the POS's top bar carries exactly what the site nav does; the
        // POS app moves them into its top bar.
        div id="pos-site-brand" hidden {
            a class="pos-brand" href="/dashboard" aria-label="Monokulo dashboard" {
                (logo_mark(22, "pos-brand-logo"))
                span class="pos-brand-name" { "Monokulo" }
            }
        }
        div id="pos-site-controls" hidden {
            (theme_toggle(chrome))
            (status_indicator(chrome.health, "pos-status-link"))
        }
        noscript { p class="error" { "POS requires JavaScript. Use Create an order on the store page instead." } }
        script type="module" src="/static/pos-app.js" {}
    };
    layout_bare_with_head(
        chrome,
        &title,
        "width=device-width, initial-scale=1, viewport-fit=cover",
        head,
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_solid_mount_with_store_config_and_no_site_nav() {
        let chrome = PageChrome::from_user(None, "/dashboard/stores/conn-1/pos");
        let data = PosViewModel {
            connection_id: shared::ids::ConnectionId::new("conn-1"),
            public_key: "pk_test".into(),
            display_name: "example.com".into(),
            base_currency: "XMR".into(),
            base_currency_decimals: 12,
            client_logging: false,
        };
        let html = page(&chrome, &data).into_string();
        assert!(html.contains("data-connection-id=\"conn-1\""));
        assert!(html.contains("data-decimals=\"12\""));
        assert!(html.contains("data-client-logging=\"false\""));
        assert!(html.contains("/static/pos-app.js"));
        assert!(html.contains("/static/pos-app.css"));
        assert!(!html.contains("<nav class=\"site-nav\""));
        assert!(
            html.contains(
                r#"<div id="pos-site-brand" hidden><a class="pos-brand" href="/dashboard""#
            ),
            "the POS bar gets the site's mark: {html}"
        );
    }
}
