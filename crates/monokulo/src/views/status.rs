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
    /// The nodes' ZMQ announcements (docs/monero_zmq.md): set only for an
    /// admin, and only when a node of this network has a publisher.
    pub announcements: Option<AnnouncementsView>,
    /// Proof-of-work checking (docs/proof_of_work.md): set only for an
    /// admin, and only while checking is on for this network.
    pub proof: Option<ProofView>,
}

/// A network's proof-of-work checking, ready to show.
pub struct ProofView {
    pub tag_label: String,
    pub tag_class: String,
    pub summary: String,
    /// The anchor, the hashing, the last check: a sentence each.
    pub facts: Vec<String>,
    pub nodes: Vec<ProofNodeView>,
    /// Settlement is held: offer to take a new anchor.
    pub can_take_new_anchor: bool,
}

pub struct ProofNodeView {
    pub node: String,
    pub height: String,
    pub verdict: String,
    pub detail: Option<String>,
    pub excluded: bool,
}

/// A network's ZMQ announcements, ready to show.
pub struct AnnouncementsView {
    pub publishers: Vec<PublisherView>,
    pub pool_passes_woken: u64,
    pub rounds_woken: u64,
}

pub struct PublisherView {
    pub node: String,
    pub endpoint: String,
    pub connected: bool,
    /// "since 5m ago", or when it was lost.
    pub state_display: String,
    pub connections: u64,
    pub pool_announcements: u64,
    pub block_announcements: u64,
    pub last_announcement_display: String,
    /// The last failure and when, if any.
    pub last_error: Option<String>,
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
    /// Link each network to its engine page (`docs/engine_visualizer.md`):
    /// only for an admin, who alone may open it.
    pub engine_page: bool,
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

                        h3 {
                            "Chain scanner"
                            @if data.engine_page {
                                " " a class="hint" href=(format!("/status/engine?network={}", network.network)) { "Watch it live" }
                            }
                        }
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
                        @if let Some(proof) = &network.proof {
                            (proof_section(&network.network, proof))
                        }
                        @if let Some(announcements) = &network.announcements {
                            (announcements_section(announcements))
                        }
                    }
                }
            }
        }
    }
}

/// A network's proof-of-work checking: where it stands, and what each
/// node was found to serve.
fn proof_section(network: &str, proof: &ProofView) -> Markup {
    html! {
        section class="proof" {
            h3 { "Proof of work" }
            p class="hint" {
                "Only operators see this. Orders on this network settle only on blocks whose proof of work the engine checked itself."
            }
            p {
                span class=(format!("tag {}", proof.tag_class)) { (proof.tag_label) }
                " " (proof.summary)
            }
            @for fact in &proof.facts { p class="muted" { (fact) } }
            table {
                thead { tr { th { "Node" } th { "Height" } th { "Found" } th { "Scanned" } } }
                tbody {
                    @for node in &proof.nodes {
                        tr {
                            td { code { (node.node) } }
                            td { (node.height) }
                            td { (node.verdict) }
                            td {
                                @if node.excluded { span class="tag tag-error" { "left out" } }
                                @else { span class="tag tag-ok" { "yes" } }
                            }
                        }
                        @if let Some(detail) = &node.detail {
                            tr { td colspan="4" class="hint" { (detail) } }
                        }
                    }
                }
            }
            @if proof.can_take_new_anchor {
                form method="post" action=(format!("/dashboard/admin/proof/{network}/reanchor")) {
                    p class="hint" {
                        "A new anchor is taken on the nodes' word, 720 blocks below their tips. "
                        "Check that the nodes are your own or ones you trust before taking one."
                    }
                    button type="submit" { "Take a new anchor" }
                }
            }
        }
    }
}

/// A network's ZMQ publishers and what their announcements did.
fn announcements_section(announcements: &AnnouncementsView) -> Markup {
    html! {
        section class="announcements" {
            h3 { "Announcements (ZMQ)" }
            p class="hint" {
                "Only operators see this. A node's announcements start a check at once instead of at the next poll; "
                "the regular checks carry on and find anything they miss."
            }
            table {
                thead { tr { th { "Node" } th { "Publisher" } th { "State" } th { "Transactions" } th { "Blocks" } th { "Last" } th { "Connections" } } }
                tbody {
                    @for publisher in &announcements.publishers {
                        tr {
                            td { code { (publisher.node) } }
                            td { code { (publisher.endpoint) } }
                            td {
                                @if publisher.connected { span class="tag tag-ok" { "connected" } }
                                @else { span class="tag tag-error" { "not connected" } }
                                " " span class="muted" { (publisher.state_display) }
                            }
                            td { (publisher.pool_announcements) }
                            td { (publisher.block_announcements) }
                            td { (publisher.last_announcement_display) }
                            td { (publisher.connections) }
                        }
                        @if let Some(error) = &publisher.last_error {
                            tr { td colspan="7" class="hint" { (error) } }
                        }
                    }
                }
            }
            p {
                "Started early: " (announcements.pool_passes_woken) " mempool checks, "
                (announcements.rounds_woken) " scan rounds."
            }
            p class="hint" {
                "A transaction the node relays privately (Dandelion++ stem, or one sent over Tor or I2P) is never announced; "
                "the regular check finds it once it is public."
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
            engine_page: false,
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
            engine_page: false,
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
            engine_page: false,
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
                announcements: None,
                proof: None,
            }],
            poll_interval_secs: 30,
            generated_at_display: "just now".to_string(),
            slow_blocks: vec![],
            engine_page: false,
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
