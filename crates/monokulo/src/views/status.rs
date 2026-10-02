//! `GET /status` - `http::status_page::status_page`.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// One Monero node's row - mirrors `engine_client::NodeStatus` but with
/// presentation already done (`height_display`/`is_reachable`).
pub struct StatusNodeView {
    pub label: String,
    pub is_active: bool,
    pub is_reachable: bool,
    pub height_display: String,
    pub error: Option<String>,
}

/// One network's scanner-loop row - mirrors `engine_client::ScannerStatusView`
/// plus a single overall `status_label` so the page renders one tag instead
/// of re-deriving it from three bools.
pub struct StatusScannerView {
    pub ever_ticked: bool,
    pub status_label: String,
    pub status_tag_class: String,
    pub last_tick_display: String,
    pub tick_count: u64,
    pub tenants_scanned: usize,
    pub last_error: Option<String>,
}

pub struct StatusNetworkView {
    pub network: String,
    pub nodes: Vec<StatusNodeView>,
    pub scanner: StatusScannerView,
    /// While block headers are read first, why and for how long
    /// (docs/engine_scaling.md section 4).
    pub headers_first: Option<String>,
}

/// `engine_error`, when set, means the engine itself couldn't be reached at
/// all - a genuinely different, more serious case than any one node or
/// scanner being unhealthy.
/// Abuse-protection activity, shown to operators (admins) only.
pub struct AbuseStatusView {
    pub under_attack: bool,
    pub issued: u64,
    pub solved: u64,
    pub refused: u64,
}

pub struct StatusPageViewModel {
    /// Set only when an admin is viewing (`http::status_page`).
    pub abuse: Option<AbuseStatusView>,
    pub engine_error: Option<String>,
    pub networks: Vec<StatusNetworkView>,
    pub poll_interval_secs: u64,
    pub generated_at_display: String,
    /// A sentence per network with a slow block (docs/engine_scaling.md
    /// section 5).
    pub slow_blocks: Vec<String>,
}

/// Everything on the status page below its heading: streamed as a whole
/// to replace itself (`http::status_page::status_events`).
pub fn live_fragment(data: &StatusPageViewModel) -> Markup {
    html! {
        div id="status-live" {
            p class="hint" {
                "Generated " (data.generated_at_display) ", with node heights as of that moment. The scan loop polls every "
                (data.poll_interval_secs) "s."
            }

            @for message in &data.slow_blocks {
                p class="notice slow-block" role="status" { (message) }
            }
            @if let Some(abuse) = &data.abuse {
                div class="box" id="abuse-protection" {
                    h2 { "Abuse protection" }
                    p class="hint" { "Only operators see this." }
                    p {
                        "Under-attack mode: "
                        @if abuse.under_attack { span class="tag tag-error" { "on" } " - every visitor who isn't signed in is challenged." }
                        @else { span class="tag tag-ok" { "off" } }
                    }
                    table {
                        thead { tr { th { "Last hour" } th { "Challenges" } } }
                        tbody {
                            tr { td { "Issued" } td { (abuse.issued) } }
                            tr { td { "Solved" } td { (abuse.solved) } }
                            tr { td { "Refused" } td { (abuse.refused) } }
                        }
                    }
                    p class="hint" { "Refused counts wrong or replayed answers and requests turned away past the hard limit." }
                }
            }
            @if let Some(error) = &data.engine_error {
                div class="error" { "The engine could not be reached: " (error) }
            } @else if data.networks.is_empty() {
                div class="box" { p { "No Monero nodes are configured on this engine." } }
            } @else {
                @for network in &data.networks {
                    div class="box" {
                        h2 { (network.network) }

                        h3 { "Nodes" }
                        table {
                            thead { tr { th { "Node" } th { "Active" } th { "Status" } th { "Height" } } }
                            tbody {
                                @for node in &network.nodes {
                                    tr {
                                        td { code { (node.label) } }
                                        td { @if node.is_active { "yes" } @else { span class="muted" { "-" } } }
                                        td {
                                            @if node.is_reachable {
                                                span class="tag tag-ok" { "reachable" }
                                            } @else {
                                                span class="tag tag-error" { "error" }
                                            }
                                        }
                                        td { (node.height_display) }
                                    }
                                    @if !node.is_reachable {
                                        @if let Some(error) = &node.error {
                                            tr { td colspan="4" class="hint" { (error) } }
                                        }
                                    }
                                }
                            }
                        }

                        h3 { "Chain scanner" }
                        p {
                            span class=(format!("tag {}", network.scanner.status_tag_class)) { (network.scanner.status_label) }
                            @if network.scanner.ever_ticked {
                                "\u{a0} last tick " (network.scanner.last_tick_display) " \u{a0}·\u{a0} " (network.scanner.tick_count) " ticks total \u{a0}·\u{a0} "
                                (network.scanner.tenants_scanned) " tenants scanned last tick"
                            }
                        }
                        @if let Some(last_error) = &network.scanner.last_error {
                            div class="error" { (last_error) }
                        }
                        @if let Some(headers_first) = &network.headers_first {
                            p class="hint headers-first" { (headers_first) }
                        }
                    }
                }
            }
        }
    }
}

pub fn page(chrome: &PageChrome, data: &StatusPageViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" {
                @if chrome.logged_in { a href="/dashboard" { "Dashboard" } }
                @else { a href="/" { "Home" } }
            }
            h1 { "Engine status" }
            p { (super::reload_button("/status")) }
            (live_fragment(data))
            // Streams the part above while the page is open, with JavaScript.
            span hidden fx-action="/status/events" fx-trigger="fx:inited" fx-swap="none" fx-sse-reconnect {}
        }
    };
    layout(chrome, "Status - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/status")
    }

    #[test]
    fn shows_a_plain_error_banner_when_the_engine_is_unreachable() {
        let data = StatusPageViewModel {
            abuse: None,
            engine_error: Some("the engine could not be reached".to_string()),
            networks: vec![],
            poll_interval_secs: 30,
            generated_at_display: "just now".to_string(),
            slow_blocks: vec![],
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("the engine could not be reached"));
        assert!(
            !html.contains("<table"),
            "an engine error must not still show a (fabricated) networks table"
        );
    }

    /// A slow block shows as a notice above everything else
    /// (docs/engine_scaling.md section 5).
    #[test]
    fn a_slow_block_is_announced_at_the_top() {
        let data = StatusPageViewModel {
            abuse: None,
            engine_error: None,
            networks: vec![],
            poll_interval_secs: 30,
            generated_at_display: "just now".to_string(),
            slow_blocks: vec![
                "Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far.".to_string(),
            ],
        };
        let html = page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<p class="notice slow-block" role="status">Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far.</p>"#),
            "{html}"
        );
    }

    #[test]
    fn shows_no_configured_networks_message_when_networks_is_empty() {
        let data = StatusPageViewModel {
            abuse: None,
            engine_error: None,
            networks: vec![],
            poll_interval_secs: 30,
            generated_at_display: "just now".to_string(),
            slow_blocks: vec![],
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html
            .to_lowercase()
            .contains("no monero nodes are configured"));
    }

    #[test]
    fn shows_a_node_row_and_its_error_when_unreachable() {
        let data = StatusPageViewModel {
            abuse: None,
            engine_error: None,
            networks: vec![StatusNetworkView {
                network: "mainnet".to_string(),
                nodes: vec![StatusNodeView {
                    label: "node.example.com".to_string(),
                    is_active: true,
                    is_reachable: false,
                    height_display: "-".to_string(),
                    error: Some("connection refused".to_string()),
                }],
                scanner: StatusScannerView {
                    ever_ticked: true,
                    status_label: "tick failing".to_string(),
                    status_tag_class: "tag-error".to_string(),
                    last_tick_display: "5m ago".to_string(),
                    tick_count: 42,
                    tenants_scanned: 3,
                    last_error: Some("scanner blew up".to_string()),
                },
                headers_first: Some(
                    "Reading block headers first for about 50 minutes more.".to_string(),
                ),
            }],
            poll_interval_secs: 30,
            generated_at_display: "just now".to_string(),
            slow_blocks: vec![],
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("node.example.com"));
        assert!(html.contains("tag-error"));
        assert!(html.contains("connection refused"));
        assert!(html.contains("scanner blew up"));
        assert!(
            html.contains(r#"<p class="hint headers-first">Reading block headers first for about 50 minutes more.</p>"#),
            "{html}"
        );
        assert!(
            html.contains("42"),
            "expected the tick count shown, got: {html}"
        );
    }
}
