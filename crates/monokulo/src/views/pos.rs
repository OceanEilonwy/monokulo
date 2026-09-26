//! Merchant POS shell. Solid 2 owns the terminal interaction; the shared
//! public checkout stays an independent page inside its payment iframe.

use maud::{html, Markup};

use super::{layout_bare_with_head, PageChrome};

pub struct PosViewModel {
    pub connection_id: String,
    pub public_key: String,
    pub display_name: String,
    pub base_currency: String,
    pub base_currency_decimals: u8,
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
            data-decimals=(data.base_currency_decimals) {}
        noscript { p class="error" { "POS requires JavaScript. Use Create an order on the store page instead." } }
        script type="module" src="/static/pos-app.js" {}
    };
    layout_bare_with_head(chrome, &title, "width=device-width, initial-scale=1, viewport-fit=cover", head, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_solid_mount_with_store_config_and_no_site_nav() {
        let chrome = PageChrome::from_user(None, "/dashboard/stores/conn-1/pos");
        let data = PosViewModel {
            connection_id: "conn-1".into(), public_key: "pk_test".into(), display_name: "example.com".into(),
            base_currency: "XMR".into(), base_currency_decimals: 12,
        };
        let html = page(&chrome, &data).into_string();
        assert!(html.contains("data-connection-id=\"conn-1\""));
        assert!(html.contains("data-decimals=\"12\""));
        assert!(html.contains("/static/pos-app.js"));
        assert!(html.contains("/static/pos-app.css"));
        assert!(!html.contains("<nav class=\"site-nav\""));
    }
}
