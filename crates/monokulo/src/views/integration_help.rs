//! The integration-help fragment - shared verbatim by the post-connect
//! success page (`views::connect`) and the store detail page
//! (`views::store_detail`), so the two can never show different
//! instructions for the same store. Not a full page - just a `Markup`
//! fragment a caller embeds in its own body, same as the old
//! `_integration_help.html.hbs` partial.

use maud::{html, Markup};

/// `public_url` is this instance's configured public address
/// (`crate::settings::public_url`); when set, the snippets below use it in
/// absolute URLs so they can be pasted as-is into a page on another site.
/// While it isn't set they fall back to paths relative to wherever this
/// dashboard is being viewed.
pub fn fragment(public_key: &str, public_url: Option<&str>, is_woocommerce: bool) -> Markup {
    let base = public_url.unwrap_or("");
    html! {
        div class="box" {
            h2 { "Integrate this store" }
            p { "Your public key (safe to publish - it identifies your store, it can't move funds or read your data):" }
            pre { (public_key) }

            @if is_woocommerce {
                p {
                    "This store is already connected via the WooCommerce plugin - nothing further to configure there. "
                    "Need to connect a second WordPress site to this same store? From that site's plugin settings, click "
                    strong { "Connect" } ", then choose " strong { "use an existing store" } " and pick this one."
                }
            } @else {
                p { strong { "WooCommerce" } " - the easiest path if your store runs WooCommerce:" }
                ol class="steps" {
                    li { "Install the " code { "monokulo" } " plugin from your WordPress admin." }
                    li { "Go to " strong { "WooCommerce → Settings → Payments → Monokulo" } " and click " strong { "Connect" } "." }
                    li { "Choose " strong { "use an existing store" } " and pick this one - no keys to paste in again." }
                }
            }

            p { strong { "Widget embed" } " - paste this where you want a \"Pay with Monero\" button on any static page:" }
            pre {
                "<button id=\"monokulo-buy-button\">Pay with Monero</button>\n"
                "<div id=\"monokulo-checkout\"></div>\n"
                "<script src=\"" (base) "/static/monokulo-client.js\"></script>\n"
                "<script>\n"
                "document.getElementById('monokulo-buy-button').addEventListener('click', function () {\n"
                "  Monokulo.createOrder({\n"
                "    publicKey: '" (public_key) "',\n"
                "    amount: 9.99,                             // replace with the real amount\n"
                "    currency: 'USD',                          // or 'XMR' - always works, no provider needed\n"
                "    merchantOrderId: 'order-1234',            // optional - your own order/cart id, shown on the order's dashboard page\n"
                "  }).then(function (order) {\n"
                "    Monokulo.mount('#monokulo-checkout', order, {\n"
                "      onPaid: function () { /* e.g. window.location = '/thank-you.html'; */ },\n"
                "    });\n"
                "  });\n"
                "});\n"
                "</script>"
            }
            p class="hint" {
                "The script infers this instance's own address from its own " code { "<script src>" } " - paste it "
                "verbatim"
                @if public_url.is_none() { ", from wherever this dashboard is hosted" }
                "."
            }

            p {
                strong { "Direct API integration" } " - create an order against this same Monokulo instance "
                "(not the underlying engine - this instance owns pricing and the checkout page) and redirect the buyer to the "
                "returned checkout link:"
            }
            pre {
                "POST " (base) "/pay/" (public_key) "/orders\n"
                "Content-Type: application/json\n"
                "\n"
                "{\n"
                "  \"currency\": \"USD\",\n"
                "  \"amount\": \"25.00\",\n"
                "  \"merchant_order_id\": \"order-1234\"\n"
                "}"
            }
            p class="hint" { code { "merchant_order_id" } " is optional - your own order/cart id, if you have one." }
            p class="hint" {
                @if public_url.is_none() {
                    "Send this request to wherever this dashboard is hosted (the same origin this page is on). "
                }
                "The response includes a " code { "order_id" } " - send the buyer to "
                code { (base) "/pay/" (public_key) "/orders/<order_id>" } " to complete the payment. This endpoint "
                "needs no secret while your store accepts orders from any website. If you restrict it to your "
                "verified domains, orders must come from a page on one of them, or from a plugin (which uses your "
                "store's secret key)."
            }
        }
    }
}
