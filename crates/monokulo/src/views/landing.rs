//! `GET /` - see `http::home::landing`'s own doc comment.

use maud::{html, Markup};

use super::{layout, PageChrome};

pub fn page(chrome: &PageChrome, signup_public: bool) -> Markup {
    let body = html! {
        div class="wrap" {
            div class="hero" {
                img src="/static/logo.svg" alt="" width="64" height="64" class="hero-logo";
                div {
                    h1 { "Accept Monero. Non-custodial. Nothing to trust but math." }
                    p {
                        "Monokulo is a hosted payment gateway for Monero (XMR). You give it a "
                        strong { "watch-only view key" }
                        " - never your spend key, never your funds. It watches the chain, "
                        "detects payments, and tells your store when an order is paid. Your coins never pass through us, "
                        "because they never can."
                    }
                }
            }
            p class="hint" {
                "\"Monokulo\" - "
                em { "mon" }
                "(ero) + "
                em { "okulo" }
                " (Esperanto for \"eye\"): a Monero eye, "
                "watching one key, nothing else it can see or touch."
            }

            @if signup_public {
                a class="btn" href="/dashboard/signup" { "Sign up - it's free to connect a store" }
            } @else {
                a class="btn" href="/request-invite" { "Request an invite to join" }
            }
            a class="btn btn-secondary" href="/dashboard/login" { "Log in" }

            hr class="rule";

            div class="grid-2" {
                div class="box" {
                    h2 { "Non-custodial, provably" }
                    p {
                        "Only a private "
                        em { "view" }
                        " key and a public spend key are ever stored - the pair that lets us "
                        "watch incoming payments, and nothing else. No code path here can ever move your funds, because no "
                        "spend key exists anywhere in this system."
                    }
                }
                div class="box" {
                    h2 { "Drop-in for WooCommerce" }
                    p {
                        "Install the plugin, click Connect, done. No manual key-pasting, no config files - see the "
                        "guided setup once you've signed up."
                    }
                }
                div class="box" {
                    h2 { "Real-time detection" }
                    p {
                        "Zero-conf for small amounts, configurable confirmation depth for larger ones, and automatic "
                        "reorg / double-spend handling - your dashboard and your storefront both learn the moment a "
                        "payment lands."
                    }
                }
                div class="box" {
                    h2 { "Own the integration" }
                    p {
                        "Prefer to talk to the API directly? A store's public key and a plain HTTP endpoint are all you "
                        "need - no SDK required. Full instructions land on your store's page the moment it's connected."
                    }
                }
            }

            hr class="rule";
            p class="hint" {
                "Monokulo is built on the same open-source, self-hostable payment-watching "
                "engine either way - this is the hosted, zero-ops version of it, with pricing and checkout "
                "handled for you."
            }
        }
    };
    layout(chrome, "Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_page_renders_with_a_signup_cta_in_public_mode() {
        let html = page(&PageChrome::from_user(None, "/"), true).into_string();
        assert!(html.contains(r#"class="btn" href="/dashboard/signup""#), "expected the main sign-up CTA, got: {html}");
        assert!(!html.contains("Request an invite"));
        assert!(html.to_lowercase().contains("monero"));
    }

    #[test]
    fn landing_page_renders_with_a_request_invite_cta_in_invite_only_mode() {
        let html = page(&PageChrome::from_user(None, "/"), false).into_string();
        assert!(html.contains(r#"href="/request-invite""#), "expected the request-invite CTA, got: {html}");
        assert!(!html.contains(r#"class="btn" href="/dashboard/signup""#), "the main sign-up CTA must not appear in invite-only mode");
    }
}
