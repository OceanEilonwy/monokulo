//! Merchant POS shell. Solid 2 owns the whole terminal, including its own
//! payment card (QR, address, refund address); it does not embed the public
//! checkout page.

use maud::{html, Markup};

use super::{
    account_menu, layout_bare_with_head, logo_mark, script, status_indicator, stylesheet, Load,
    PageChrome,
};

pub struct PosViewModel {
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    pub base_currency: String,
    pub base_currency_decimals: u8,
    /// The store opted in to client logs: the POS records and sends its
    /// session timeline (`apps/pos/src/timeline.ts`).
    pub client_logging: bool,
}

pub fn page(chrome: &PageChrome, data: &PosViewModel) -> Markup {
    let title = format!("POS - {} - Monokulo", data.display_name);
    let head = stylesheet("pos-app.css");
    let body = html! {
        div id="pos-root"
            data-connection-id=(data.connection_id)
            data-store-name=(data.display_name)
            data-currency=(data.base_currency)
            data-decimals=(data.base_currency_decimals)
            data-client-logging=(data.client_logging)
            // The QR decoder the app loads for refund addresses, from
            // where the page says (its URL carries the file's version).
            data-qr-decoder=(crate::assets::url("jsQR.js")) {}
        // The site's logo, status indicator and account menu (the theme
        // switch is in it), rendered here so the POS's top bar carries what
        // the site's bar does; the POS app moves them into its top bar.
        div id="pos-site-brand" hidden {
            a class="pos-brand" href="/" aria-label="Monokulo dashboard" {
                (logo_mark(22, "pos-brand-logo"))
                span class="pos-brand-name" { "Monokulo" }
            }
        }
        div id="pos-site-controls" hidden {
            (status_indicator(chrome.health))
            @if chrome.logged_in { (account_menu(chrome)) }
        }
        noscript { (ui::Alert::new("POS requires JavaScript. Use Create an order on the store page instead.", ui::Tone::Error).quiet(true)) }
        // The status indicator's island polls for the instance's health.
        (script("ui-islands.js", Load::Module))
        (script("pos-app.js", Load::Module))
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
            display_name: "example.com".into(),
            base_currency: "XMR".into(),
            base_currency_decimals: 12,
            client_logging: false,
        };
        let html = page(&chrome, &data).into_string();
        assert!(html.contains("data-connection-id=\"conn-1\""));
        assert!(html.contains("data-decimals=\"12\""));
        assert!(html.contains("data-client-logging=\"false\""));
        assert!(html.contains(&crate::assets::url("pos-app.js")));
        assert!(html.contains(&crate::assets::url("pos-app.css")));
        assert!(html.contains(&format!(
            "data-qr-decoder=\"{}\"",
            crate::assets::url("jsQR.js")
        )));
        assert!(!html.contains("<nav class=\"site-nav\""));
        assert!(
            html.contains(r#"<div id="pos-site-brand" hidden><a class="pos-brand" href="/""#),
            "the POS bar gets the site's mark: {html}"
        );
    }
}
