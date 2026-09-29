//! The "Checking your connection" interstitial (abuse protection step 9d)
//! and the page shown past the hard limit. Both are plain server-rendered
//! pages that work with JavaScript off and inside a frame: no nav, no
//! cookies.
//!
//! With JavaScript, `static/challenge.js` solves the proof-of-work with Web
//! Crypto and continues by itself. Without it, a `<meta http-equiv=
//! "refresh">` continues after 10 seconds to a URL carrying a wait token
//! (`crate::abuse::challenge`), and a plain link does the same by hand.

use maud::{html, Markup};

use super::{layout_bare, layout_bare_with_head, PageChrome};

pub struct ChallengePageView {
    /// The signed proof-of-work challenge.
    pub challenge: String,
    pub difficulty: u32,
    /// Where to go once solved: the original URL, without challenge
    /// parameters. The script appends `monokulo_proof=...`.
    pub continue_url: String,
    /// The original URL plus `monokulo_wait=<token>`, redeemable after the
    /// wait.
    pub wait_url: String,
    /// Why the last attempt didn't work, if it didn't.
    pub error: Option<String>,
}

/// A stage card, as the checkout shows its own state, so a checkout frame
/// goes from this check to the payment without changing look. The card
/// (what's happening, and without JavaScript the continue link) comes first
/// and fits a frame as small as a browser's default 300 x 150.
const STYLE: &str = r#"
body { padding-bottom: 0; }
.challenge-wrap { max-width: 34rem; margin: clamp(.5rem, 5vh, 3rem) auto; padding: 0 .75rem; }
.challenge-stage { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: .65em; align-items: start; padding: .65em .8em; border: 1px solid var(--state-border); border-radius: var(--radius-md); background: var(--state-bg); color: var(--state-ink); }
.challenge-stage h1 { margin: 0; padding: 0; border: 0; font-size: 1.05rem; font-weight: 800; }
.challenge-stage p { margin: .15em 0 0; font-size: .85em; line-height: 1.4; }
.challenge-stage a { color: inherit; font-weight: 800; }
.challenge-spin { width: 1.1em; height: 1.1em; margin-top: .2em; border-radius: 50%; border: 2px solid color-mix(in srgb, currentColor 25%, transparent); border-top-color: currentColor; animation: challenge-spin 1.4s linear infinite; }
@keyframes challenge-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { .challenge-spin { animation: none; } }
.challenge-why { margin: .8em 0 0; color: var(--muted); font-size: .85em; }
"#;

pub fn challenge_page(chrome: &PageChrome, view: &ChallengePageView) -> Markup {
    let head = html! {
        style { (maud::PreEscaped(STYLE)) }
        noscript { meta http-equiv="refresh" content=(format!("10;url={}", view.wait_url)); }
    };
    let body = html! {
        main class="challenge-wrap" id="challenge" aria-busy="true"
            data-challenge=(view.challenge)
            data-difficulty=(view.difficulty)
            data-continue=(view.continue_url)
            data-wait=(view.wait_url) {
            div class="challenge-stage state-confirming" {
                span class="challenge-spin" aria-hidden="true" {}
                div {
                    h1 { "Checking your connection" }
                    noscript {
                        p class="challenge-progress" role="status" {
                            "This page continues in 10 seconds. If it doesn't, " a href=(view.wait_url) { "continue" } "."
                        }
                    }
                    p class="challenge-progress" id="challenge-progress" role="status" aria-live="polite" hidden { "Working…" }
                }
            }
            @if let Some(error) = &view.error {
                p class="error" role="alert" { (error) }
            }
            p class="challenge-why" {
                "This page has had a lot of requests from your connection, so we're making sure it's a real visitor "
                "before continuing. Nothing is stored on your device, and you won't be asked again for a while."
            }
        }
        script src="/static/challenge.js" {}
    };
    layout_bare_with_head(chrome, "Checking your connection", "width=device-width, initial-scale=1", head, body)
}

/// Past the hard limit: nothing to solve, just wait.
pub fn too_many_requests_page(chrome: &PageChrome, retry_after_secs: u64) -> Markup {
    let body = html! {
        main class="wrap" {
            h1 { "Too many requests" }
            p {
                "Your connection has made too many requests in the last minute. Please wait "
                (retry_after_secs) " seconds, then reload this page."
            }
        }
    };
    layout_bare(chrome, "Too many requests", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interstitial_works_without_javascript_and_announces_itself() {
        let view = ChallengePageView {
            challenge: "abc.def".to_string(),
            difficulty: 16,
            continue_url: "/pay/pk/orders/o1".to_string(),
            wait_url: "/pay/pk/orders/o1?monokulo_wait=tok".to_string(),
            error: None,
        };
        let html = challenge_page(&PageChrome::from_user(None, "/"), &view).into_string();
        assert!(html.contains(r#"<meta http-equiv="refresh" content="10;url=/pay/pk/orders/o1?monokulo_wait=tok">"#), "{html}");
        assert!(html.contains("This page continues in 10 seconds."));
        // The state and the way on come before the explanation, so a small
        // frame shows them.
        assert!(html.find(r#"class="challenge-stage"#).unwrap() < html.find(r#"class="challenge-why""#).unwrap());
        assert!(html.find(">continue</a>").unwrap() < html.find(r#"class="challenge-why""#).unwrap());
        assert!(html.contains(r#"role="status""#));
        assert!(html.contains(r#"data-challenge="abc.def""#));
        assert!(html.contains(r#"src="/static/challenge.js""#));
    }
}
