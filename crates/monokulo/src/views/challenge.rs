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

const STYLE: &str = r#"
.challenge-wrap { max-width: 34rem; margin: 3rem auto; padding: 0 1rem; }
.challenge-progress { font-weight: 600; }
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
            h1 { "Checking your connection" }
            p {
                "This page has had a lot of requests from your connection, so we're making sure it's a real visitor "
                "before continuing. Nothing is stored on your device, and you won't be asked again for a while."
            }
            @if let Some(error) = &view.error {
                p class="error" role="alert" { (error) }
            }
            noscript {
                p class="challenge-progress" role="status" { "Checking your connection, this page continues in 10 seconds." }
                p { "If it doesn't, wait 10 seconds and " a href=(view.wait_url) { "continue" } "." }
            }
            p class="challenge-progress" id="challenge-progress" role="status" aria-live="polite" hidden {
                "Checking your connection…"
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
        assert!(html.contains("this page continues in 10 seconds"));
        assert!(html.contains(r#"role="status""#));
        assert!(html.contains(r#"data-challenge="abc.def""#));
        assert!(html.contains(r#"src="/static/challenge.js""#));
    }
}
