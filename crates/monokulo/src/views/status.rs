//! `GET /status` - `http::status_page::status_page`.

use maud::{html, Markup};
pub use shared::proof::{NodeVerdict, ProofState};

use super::scaling::{network_name, thousands};
use super::{layout, PageChrome};

/// One Monero node's row - mirrors `engine_client::NodeStatus` but with
/// presentation already done (`height_display`/`is_reachable`), and what
/// proof-of-work checking found it serves while an admin is looking.
pub struct StatusNodeView {
    pub label: String,
    pub node_use: NodeUse,
    pub is_reachable: bool,
    pub height_display: String,
    /// What proof-of-work checking found the node serves: set only for an
    /// admin, and only while checking is on for its network.
    pub verdict: Option<NodeVerdict>,
    /// What went wrong, in a sentence: the block a node was caught on, or
    /// why it doesn't answer.
    pub detail: Option<String>,
}

/// What a node is used for: the one asked, a fallback, or one left out of
/// scanning for serving another chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeUse {
    InUse,
    Standby,
    LeftOut,
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
    /// The engine's name for it: "mainnet".
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
    pub state: ProofState,
    /// The engine's sentence on where it stands: shown while it waits for
    /// an anchor, and as the reason settlement is held.
    pub summary: String,
    /// Where settlement stands among the last blocks, while following.
    pub window: Option<ProofWindow>,
    /// "How it's checked": the anchor, the hashing and the last check, as
    /// a name and a value each.
    pub facts: Vec<(&'static str, String)>,
}

/// The heights the recent window is drawn from.
pub struct ProofWindow {
    /// The highest block orders may newly settle on.
    pub ceiling: u64,
    /// The newest proven block.
    pub proven: u64,
    /// The tip of the chain being followed: the highest a node on the
    /// proven chain, or past it, reports.
    pub tip: u64,
    /// The block the proven chain started from, and how long ago it was
    /// taken ("2d ago").
    pub anchor: Option<(u64, String)>,
}

/// How many blocks the recent window shows: a fixed scale, so the gap
/// between what settles and the tip is the same size on screen whatever
/// the anchor's age.
pub const WINDOW_BLOCKS: u64 = 30;

/// Settlement more than this many blocks behind the tip is called out as
/// slow; up to it is a normal round or two of checking.
pub const BEHIND_SLOW_AFTER: u64 = 5;

/// What one block of the window is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pip {
    /// Below the anchor: never checked, nor needed.
    BeforeAnchor,
    Proven,
    /// Seen on a node, its proof of work not checked yet.
    Seen,
}

impl ProofWindow {
    /// The window's last block: the tip, or the proven tip should no node
    /// on the proven chain answer.
    fn top(&self) -> u64 {
        self.tip.max(self.proven)
    }

    /// The window's blocks, oldest first, ending at [`Self::top`].
    pub fn pips(&self) -> Vec<(u64, Pip)> {
        let top = self.top();
        let first = top.saturating_sub(WINDOW_BLOCKS - 1);
        (first..=top)
            .map(|height| {
                let pip = match self.anchor {
                    Some((anchor, _)) if height < anchor => Pip::BeforeAnchor,
                    _ if height <= self.proven => Pip::Proven,
                    _ => Pip::Seen,
                };
                (height, pip)
            })
            .collect()
    }

    /// How many blocks settlement trails the tip by.
    pub fn behind(&self) -> u64 {
        self.top().saturating_sub(self.ceiling)
    }
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
    /// Monokulo's SEV-SNP key entry policy and the engine's disagree, or
    /// monokulo requires a backend the engine doesn't have
    /// (`http::status_page::snp_policy_alert`).
    pub snp_alert: Option<String>,
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

            @if let Some(alert) = &data.snp_alert {
                div class="error" role="alert" id="snp-policy-alert" { (alert) }
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
                    (network_card(network, data.engine_page))
                }
            }
        }
    }
}

/// One network: its title and state, its nodes, its scanner and, for an
/// admin, its proof-of-work checking and announcements.
fn network_card(network: &StatusNetworkView, engine_page: bool) -> Markup {
    let name = network_name(&network.network);
    let scanner = &network.scanner;
    let scanner_tag = html! {
        span class=(format!("tag {}", scanner.status_tag_class)) { (scanner.status_label) }
    };
    html! {
        div class="box net-card" id=(format!("network-{}", network.network)) {
            div class="net-head" {
                h2 { (name) }
                (scanner_tag)
                @if let Some(proof) = &network.proof {
                    @let (label, class) = proof_tag(proof.state);
                    span class=(format!("tag {class}")) { (label) }
                }
                @if engine_page {
                    a class="btn net-head-action" href=(format!("/status/engine?network={}", network.network)) { "Watch it live" }
                }
            }

            h3 class="caps" { "Nodes" }
            (node_table(network))

            h3 class="caps" { "Chain scanner" }
            p class="line scanner-line" {
                (scanner_tag)
                @if scanner.ever_ticked {
                    " last tick " (scanner.last_tick_display)
                    " · " (thousands(scanner.tick_count)) (if scanner.tick_count == 1 { " tick" } else { " ticks" })
                    " · " (scanner.tenants_scanned) (if scanner.tenants_scanned == 1 { " store" } else { " stores" })
                    " scanned last tick"
                }
            }
            @if let Some(last_error) = &scanner.last_error {
                div class="error" { (last_error) }
            }
            @if let Some(headers_first) = &network.headers_first {
                p class="hint headers-first" { (headers_first) }
            }
            @if let Some(proof) = &network.proof {
                (proof_section(&network.network, &name, proof))
            }
            @if let Some(announcements) = &network.announcements {
                (announcements_section(announcements))
            }
        }
    }
}

/// The proof state's tag beside the network's title.
fn proof_tag(state: ProofState) -> (&'static str, &'static str) {
    match state {
        ProofState::Following => ("proof checked", "tag-ok"),
        ProofState::Anchoring => ("finding an anchor", "tag-unknown"),
        ProofState::Held => ("settlement held", "tag-error"),
    }
}

/// A node's proof-of-work verdict in words, and its dot's tone.
fn verdict_words(verdict: NodeVerdict) -> (&'static str, &'static str) {
    match verdict {
        NodeVerdict::Unknown => ("not looked at yet", "verdict"),
        NodeVerdict::OnChain => ("on the proven chain", "verdict is-ok"),
        NodeVerdict::Ahead => ("ahead, being checked", "verdict is-watch"),
        NodeVerdict::Lighter => ("on a chain with less work", "verdict is-watch"),
        NodeVerdict::Diverged => ("left the proven chain too far back", "verdict is-bad"),
        NodeVerdict::Caught => ("served a block that breaks the rules", "verdict is-bad"),
        NodeVerdict::Unreachable => ("not answering", "verdict"),
    }
}

/// The nodes, one row each: a card each on a phone (`.table-cards`), its
/// name and use first, then its proof-of-work verdict and height, then
/// what went wrong. Reachability shows on a phone's card only when it's a
/// problem. The Proof of work column only while checking is shown.
fn node_table(network: &StatusNetworkView) -> Markup {
    let proof = network.proof.is_some();
    html! {
        div class="table-scroll" {
            table class="table-cards node-table" {
                thead {
                    tr {
                        th { "Node" }
                        th { "Use" }
                        @if proof { th { "Proof of work" } }
                        th { "Height" }
                        th { "Reachable" }
                    }
                }
                tbody {
                    @for node in &network.nodes {
                        tr {
                            td class="card-title" { code { (node.label) } }
                            td class="card-status" {
                                @match node.node_use {
                                    NodeUse::InUse => { "in use" }
                                    NodeUse::Standby => { span class="muted" { "standby" } }
                                    NodeUse::LeftOut => { span class="tag tag-error" { "left out" } }
                                }
                            }
                            @if proof {
                                td class="card-amount" {
                                    @if let Some(verdict) = node.verdict {
                                        @let (words, class) = verdict_words(verdict);
                                        span class=(class) { (words) }
                                    }
                                    @if let Some(detail) = &node.detail {
                                        span class="sub" { (detail) }
                                    }
                                }
                            }
                            td class="card-when" { (node.height_display) }
                            td class=(if node.is_reachable { "card-meta is-quiet" } else { "card-meta" }) {
                                @if node.is_reachable {
                                    span class="tag tag-ok" { "reachable" }
                                } @else {
                                    span class="tag tag-error" { "unreachable" }
                                }
                                @if !proof {
                                    @if let Some(detail) = &node.detail {
                                        span class="sub" { (detail) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A network's proof-of-work checking: what settles, in a sentence and
/// the recent window, or why nothing does; and how it's checked, folded.
fn proof_section(network: &str, name: &str, proof: &ProofView) -> Markup {
    html! {
        section class="proof" {
            h3 class="caps" { "Proof of work" }
            @if proof.state == ProofState::Held {
                div class="proof-held" {
                    p { (proof.summary) }
                    form method="post" action=(format!("/dashboard/admin/proof/{network}/reanchor")) {
                        p class="hint" {
                            "A new anchor is taken on the nodes' word, 720 blocks below their tips. "
                            "Check that the nodes are your own or ones you trust before taking one."
                        }
                        button type="submit" { "Take a new anchor" }
                    }
                }
            } @else if let Some(window) = &proof.window {
                (proof_window(window))
            } @else {
                p { (proof.summary) }
            }
            @if !proof.facts.is_empty() {
                details class="proof-facts" {
                    summary { "How it's checked" }
                    dl class="facts" {
                        @for (fact, value) in &proof.facts {
                            dt { (fact) }
                            dd { (value) }
                        }
                    }
                    p class="hint" { "Only operators see this section. Orders on " (name) " settle only on blocks whose proof of work the engine checked itself." }
                }
            }
        }
    }
}

/// The last [`WINDOW_BLOCKS`] blocks at a fixed scale: filled when proven,
/// outlined when only seen, the block orders settle up to ringed and the
/// tip marked at the end. The sentence and its chip say the same in words;
/// the figures sit on their own line, never along the blocks.
fn proof_window(window: &ProofWindow) -> Markup {
    let behind = window.behind();
    let (chip, chip_class) = match behind {
        0 => ("caught up".to_owned(), "tag-ok"),
        n if n <= BEHIND_SLOW_AFTER => (format!("{n} behind the tip"), "tag-unknown"),
        n => (format!("{n} behind the tip"), "tag-slow"),
    };
    let pips = window.pips();
    let before_anchor = pips.iter().any(|(_, pip)| *pip == Pip::BeforeAnchor);
    html! {
        p class="pline" {
            "Orders settle on blocks up to " strong { (thousands(window.ceiling)) } ", proven by the engine itself."
            " " span class=(format!("tag {chip_class}")) { (chip) }
        }
        div class="pips" aria-hidden="true" {
            @for (height, pip) in &pips {
                @let class = match pip {
                    Pip::BeforeAnchor => "pip before",
                    Pip::Proven => "pip",
                    Pip::Seen => "pip seen",
                };
                span class=(class) data-settles[*height == window.ceiling] title=(format!("block {}", thousands(*height))) {}
            }
            span class="pip-tip" {}
        }
        p class="pip-legend" {
            span { span class="pip" {} " proven" }
            span { span class="pip seen" {} " seen, not proven yet" }
            @if before_anchor {
                span { span class="pip before" {} " before the anchor" }
            }
            span { span class="pip" data-settles {} " settles to" }
            span { span class="pip-tip" {} " node tip" }
        }
        p class="pip-figures" {
            "Last " (WINDOW_BLOCKS) " blocks · settles to " (thousands(window.ceiling)) " · tip " (thousands(window.top()))
        }
        @if let Some((anchor, age)) = &window.anchor {
            p class="hint pip-anchor" {
                "Anchored at block " (thousands(*anchor)) ", " (age) ": every block since then is checked."
            }
        }
    }
}

/// A network's ZMQ publishers and what their announcements did.
fn announcements_section(announcements: &AnnouncementsView) -> Markup {
    html! {
        section class="announcements" {
            h3 class="caps" { "Announcements (ZMQ)" }
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
            @if chrome.logged_in {
                nav class="context-nav" aria-label="Breadcrumb" { a href="/" { "Dashboard" } }
            }
            div class="page-heading" { h1 { "Engine status" } (super::reload_button("/status")) }
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

    fn empty() -> StatusPageViewModel {
        StatusPageViewModel {
            abuse: None,
            engine_error: None,
            networks: vec![],
            poll_interval_secs: 30,
            generated_at_display: "just now".to_string(),
            slow_blocks: vec![],
            snp_alert: None,
            engine_page: false,
        }
    }

    fn node(label: &str, node_use: NodeUse, verdict: Option<NodeVerdict>) -> StatusNodeView {
        StatusNodeView {
            label: label.to_string(),
            node_use,
            is_reachable: true,
            height_display: "3,412,880".to_string(),
            verdict,
            detail: None,
        }
    }

    fn scanner() -> StatusScannerView {
        StatusScannerView {
            ever_ticked: true,
            status_label: "scanning".to_string(),
            status_tag_class: "tag-ok".to_string(),
            last_tick_display: "2s ago".to_string(),
            tick_count: 41_203,
            tenants_scanned: 3,
            last_error: None,
        }
    }

    fn network(network: &str, nodes: Vec<StatusNodeView>) -> StatusNetworkView {
        StatusNetworkView {
            network: network.to_string(),
            nodes,
            scanner: scanner(),
            headers_first: None,
            announcements: None,
            proof: None,
        }
    }

    fn facts() -> Vec<(&'static str, String)> {
        vec![
            (
                "Anchor",
                "Block 3,412,160, 2 of 3 nodes agreed, 2d ago".to_string(),
            ),
            ("Last checked", "3s ago".to_string()),
        ]
    }

    /// `behind` blocks behind a tip of 3,412,881, anchored at `anchor`.
    fn following(behind: u64, anchor: u64) -> ProofView {
        let tip = 3_412_881;
        ProofView {
            state: ProofState::Following,
            summary: "Proven up to block 3,412,881.".to_string(),
            window: Some(ProofWindow {
                ceiling: tip - behind,
                proven: tip - behind,
                tip,
                anchor: Some((anchor, "2d ago".to_string())),
            }),
            facts: facts(),
        }
    }

    /// The admin's view of a mainnet with a proof section and the engine
    /// page linked.
    fn admin_with(proof: ProofView, nodes: Vec<StatusNodeView>) -> StatusPageViewModel {
        let mut mainnet = network("mainnet", nodes);
        mainnet.proof = Some(proof);
        StatusPageViewModel {
            networks: vec![mainnet],
            engine_page: true,
            ..empty()
        }
    }

    fn render(data: &StatusPageViewModel) -> String {
        live_fragment(data).into_string()
    }

    /// The pips' classes, in order.
    fn pip_classes(html: &str) -> Vec<&str> {
        let start = html.find(r#"<div class="pips""#).expect("a window");
        let pips = &html[start..start + html[start..].find("</div>").unwrap()];
        pips.split(r#"<span class=""#)
            .skip(1)
            .filter_map(|span| span.split('"').next())
            .filter(|class| class.starts_with("pip") && *class != "pip-tip")
            .collect()
    }

    #[test]
    fn shows_a_plain_error_banner_when_the_engine_is_unreachable() {
        let data = StatusPageViewModel {
            engine_error: Some("the engine could not be reached".to_string()),
            ..empty()
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
            slow_blocks: vec![
                "Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far.".to_string(),
            ],
            ..empty()
        };
        let html = page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<p class="notice slow-block" role="status">Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far.</p>"#),
            "{html}"
        );
    }

    #[test]
    fn a_trust_mismatch_is_a_red_alert_at_the_top() {
        let data = StatusPageViewModel {
            snp_alert: Some("SEV-SNP key storage can't be chosen.".to_string()),
            ..empty()
        };
        let html = page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"<div class="error" role="alert" id="snp-policy-alert">SEV-SNP key storage can't be chosen.</div>"#),
            "{html}"
        );
    }

    #[test]
    fn shows_no_configured_networks_message_when_networks_is_empty() {
        let html = page(&chrome(), &empty()).into_string();
        assert!(html
            .to_lowercase()
            .contains("no monero nodes are configured"));
    }

    #[test]
    fn shows_a_node_row_and_its_error_when_unreachable() {
        let mut down = node("node.example.com", NodeUse::InUse, None);
        down.is_reachable = false;
        down.height_display = "-".to_string();
        down.detail = Some("connection refused".to_string());
        let mut mainnet = network("mainnet", vec![down]);
        mainnet.scanner = StatusScannerView {
            status_label: "tick failing".to_string(),
            status_tag_class: "tag-error".to_string(),
            last_tick_display: "5m ago".to_string(),
            tick_count: 42,
            last_error: Some("scanner blew up".to_string()),
            ..scanner()
        };
        mainnet.headers_first =
            Some("Reading block headers first for about 50 minutes more.".to_string());
        let data = StatusPageViewModel {
            networks: vec![mainnet],
            ..empty()
        };
        let html = page(&chrome(), &data).into_string();
        for expected in [
            "<code>node.example.com</code>",
            r#"<td class="card-meta"><span class="tag tag-error">unreachable</span><span class="sub">connection refused</span></td>"#,
            "scanner blew up",
            r#"<p class="hint headers-first">Reading block headers first for about 50 minutes more.</p>"#,
        ] {
            assert!(html.contains(expected), "{expected} in {html}");
        }
    }

    /// The card's title is the network's name as a word, larger than the
    /// small-caps section heads under it, with the scanner's and the
    /// proof's state as tags beside it.
    #[test]
    fn the_card_is_titled_by_the_network_with_its_states_beside_it() {
        let data = admin_with(following(3, 3_412_160), vec![]);
        let html = render(&data);
        assert!(
            html.contains(concat!(
                r#"<div class="net-head"><h2>Mainnet</h2>"#,
                r#"<span class="tag tag-ok">scanning</span>"#,
                r#"<span class="tag tag-ok">proof checked</span>"#,
            )),
            "{html}"
        );
        for head in ["Nodes", "Chain scanner", "Proof of work"] {
            assert!(
                html.contains(&format!(r#"<h3 class="caps">{head}</h3>"#)),
                "{head} in {html}"
            );
        }
        let mut stagenet = network("stagenet", vec![]);
        stagenet.proof = Some(ProofView {
            state: ProofState::Anchoring,
            summary: "Waiting for 2 of 3 nodes to agree on an anchor.".to_string(),
            window: None,
            facts: vec![],
        });
        let html = render(&StatusPageViewModel {
            networks: vec![stagenet, network("testnet", vec![])],
            ..empty()
        });
        assert!(html.contains("<h2>Stagenet</h2>"), "{html}");
        assert!(html.contains("<h2>Testnet</h2>"), "{html}");
        assert!(html.contains(r#"<span class="tag tag-unknown">finding an anchor</span>"#));
        assert!(
            html.contains("<p>Waiting for 2 of 3 nodes to agree on an anchor.</p>"),
            "while anchoring, the engine's sentence and no window: {html}"
        );
        assert!(!html.contains("pips"));
    }

    /// "Watch it live" is a neutral button in the card's header, for an
    /// admin only, and the card's one link to the engine page.
    #[test]
    fn watch_it_live_is_a_button_in_the_header_for_an_admin_only() {
        let html = render(&admin_with(following(0, 1), vec![]));
        assert!(
            html.contains(r#"<a class="btn net-head-action" href="/status/engine?network=mainnet">Watch it live</a></div>"#),
            "{html}"
        );
        assert_eq!(html.matches("/status/engine").count(), 1);
        let html = render(&StatusPageViewModel {
            networks: vec![network("mainnet", vec![])],
            ..empty()
        });
        assert!(!html.contains("Watch it live"), "{html}");
        assert!(!html.contains("/status/engine"), "{html}");
    }

    /// The scanner on one line: its state, then the last tick, the ticks
    /// and the stores.
    #[test]
    fn the_scanner_is_one_line() {
        let html = render(&StatusPageViewModel {
            networks: vec![network("mainnet", vec![])],
            ..empty()
        });
        assert!(
            html.contains(r#"<p class="line scanner-line"><span class="tag tag-ok">scanning</span> last tick 2s ago · 41,203 ticks · 3 stores scanned last tick</p>"#),
            "{html}"
        );
    }

    /// Each node's verdict in words with a dot of its tone, in the Proof
    /// of work column; a failing one's detail on a line under it.
    #[test]
    fn each_verdict_is_shown_in_words() {
        let verdicts = [
            (NodeVerdict::Unknown, "verdict", "not looked at yet"),
            (NodeVerdict::OnChain, "verdict is-ok", "on the proven chain"),
            (
                NodeVerdict::Ahead,
                "verdict is-watch",
                "ahead, being checked",
            ),
            (
                NodeVerdict::Lighter,
                "verdict is-watch",
                "on a chain with less work",
            ),
            (
                NodeVerdict::Diverged,
                "verdict is-bad",
                "left the proven chain too far back",
            ),
            (
                NodeVerdict::Caught,
                "verdict is-bad",
                "served a block that breaks the rules",
            ),
            (NodeVerdict::Unreachable, "verdict", "not answering"),
        ];
        let nodes = verdicts
            .iter()
            .map(|(verdict, _, _)| node("n:18081", NodeUse::InUse, Some(*verdict)))
            .collect();
        let html = render(&admin_with(following(0, 1), nodes));
        assert!(
            html.contains(
                "<th>Node</th><th>Use</th><th>Proof of work</th><th>Height</th><th>Reachable</th>"
            ),
            "{html}"
        );
        assert!(!html.contains("Found"));
        for (_, class, words) in verdicts {
            assert!(
                html.contains(&format!(
                    r#"<td class="card-amount"><span class="{class}">{words}</span></td>"#
                )),
                "{words} in {html}"
            );
        }

        let mut caught = node(
            "10.0.0.5:18081",
            NodeUse::LeftOut,
            Some(NodeVerdict::Caught),
        );
        caught.detail = Some("block 3,412,860's proof of work doesn't meet its difficulty.".into());
        let html = render(&admin_with(
            following(0, 1),
            vec![
                node(
                    "node.home.lan:18081",
                    NodeUse::InUse,
                    Some(NodeVerdict::OnChain),
                ),
                node(
                    "xmr.example.org:18089",
                    NodeUse::Standby,
                    Some(NodeVerdict::Ahead),
                ),
                caught,
            ],
        ));
        for expected in [
            r#"<td class="card-status">in use</td>"#,
            r#"<td class="card-status"><span class="muted">standby</span></td>"#,
            r#"<td class="card-status"><span class="tag tag-error">left out</span></td>"#,
            r#"<span class="verdict is-bad">served a block that breaks the rules</span><span class="sub">block 3,412,860's proof of work doesn't meet its difficulty.</span>"#,
        ] {
            assert!(html.contains(expected), "{expected} in {html}");
        }
    }

    /// On a phone each node is a card (`site.css`'s `.table-cards`): the
    /// node and its use, then the verdict and height, then the detail;
    /// reachability only when it's a problem.
    #[test]
    fn each_node_is_a_card_on_a_phone() {
        let mut down = node("b:18081", NodeUse::Standby, Some(NodeVerdict::Unreachable));
        down.is_reachable = false;
        let html = render(&admin_with(
            following(0, 1),
            vec![
                node("a:18081", NodeUse::InUse, Some(NodeVerdict::OnChain)),
                down,
            ],
        ));
        assert!(
            html.contains(r#"<table class="table-cards node-table">"#),
            "{html}"
        );
        assert!(
            html.contains(concat!(
                r#"<tr><td class="card-title"><code>a:18081</code></td>"#,
                r#"<td class="card-status">in use</td>"#,
                r#"<td class="card-amount"><span class="verdict is-ok">on the proven chain</span></td>"#,
                r#"<td class="card-when">3,412,880</td>"#,
                r#"<td class="card-meta is-quiet"><span class="tag tag-ok">reachable</span></td></tr>"#,
            )),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<td class="card-meta"><span class="tag tag-error">unreachable</span></td>"#
            ),
            "{html}"
        );
        let css = include_str!("site.css");
        for rule in [
            "table.node-table td.card-amount { display: contents; font-weight: 400; }",
            "table.node-table .card-meta.is-quiet { display: none; }",
        ] {
            assert!(css.contains(rule), "{rule}");
        }
    }

    /// Someone not an admin sees the nodes, their use and whether they
    /// answer, and nothing of proof-of-work checking.
    #[test]
    fn a_non_admin_sees_no_proof_of_work() {
        let html = render(&StatusPageViewModel {
            networks: vec![network(
                "mainnet",
                vec![
                    node("node 1", NodeUse::InUse, None),
                    node("node 2", NodeUse::Standby, None),
                ],
            )],
            ..empty()
        });
        assert!(
            html.contains("<th>Node</th><th>Use</th><th>Height</th><th>Reachable</th>"),
            "{html}"
        );
        for absent in [
            "Proof of work",
            "card-amount",
            "proof checked",
            "pips",
            "How it's checked",
        ] {
            assert!(!html.contains(absent), "{absent} in {html}");
        }
    }

    /// Caught up: every block of the window proven, the last one the one
    /// orders settle up to.
    #[test]
    fn the_window_when_caught_up() {
        let html = render(&admin_with(following(0, 3_412_160), vec![]));
        assert!(
            html.contains(r#"<p class="pline">Orders settle on blocks up to <strong>3,412,881</strong>, proven by the engine itself. <span class="tag tag-ok">caught up</span></p>"#),
            "{html}"
        );
        let pips = pip_classes(&html);
        assert_eq!(pips.len(), 30);
        assert!(pips.iter().all(|pip| *pip == "pip"), "{pips:?}");
        assert!(
            html.contains(r#"<span class="pip" data-settles title="block 3,412,881"></span><span class="pip-tip"></span></div>"#),
            "the settled block is ringed, then the tip: {html}"
        );
        assert!(html.contains(
            r#"<p class="pip-figures">Last 30 blocks · settles to 3,412,881 · tip 3,412,881</p>"#
        ));
        assert!(html.contains(
            r#"<p class="hint pip-anchor">Anchored at block 3,412,160, 2d ago: every block since then is checked.</p>"#
        ));
        assert!(
            !html.contains("before the anchor"),
            "the anchor is older than the window"
        );
    }

    /// Three behind: the last three blocks only seen, a neutral chip.
    #[test]
    fn the_window_three_behind() {
        let html = render(&admin_with(following(3, 3_412_160), vec![]));
        assert!(
            html.contains(r#"<span class="tag tag-unknown">3 behind the tip</span>"#),
            "{html}"
        );
        let pips = pip_classes(&html);
        assert_eq!(pips.len(), 30);
        assert_eq!(pips[..27], ["pip"; 27]);
        assert_eq!(pips[27..], ["pip seen"; 3]);
        assert!(html.contains(r#"<span class="pip" data-settles title="block 3,412,878">"#));
    }

    /// Fourteen behind: past the slow mark, the chip says so in amber.
    #[test]
    fn the_window_fourteen_behind() {
        let html = render(&admin_with(following(14, 3_412_160), vec![]));
        assert!(
            html.contains(r#"<span class="tag tag-slow">14 behind the tip</span>"#),
            "{html}"
        );
        let pips = pip_classes(&html);
        assert_eq!(pips.iter().filter(|pip| **pip == "pip seen").count(), 14);
        assert_eq!(pips.iter().filter(|pip| **pip == "pip").count(), 16);
        // Over the whole window behind: every block outlined, the chip
        // carries the number.
        let html = render(&admin_with(following(45, 3_412_160), vec![]));
        assert!(pip_classes(&html).iter().all(|pip| *pip == "pip seen"));
        assert!(html.contains("45 behind the tip"));
        assert!(
            !html.contains("data-settles title"),
            "settled is off the window: {html}"
        );
    }

    /// An anchor inside the window: the blocks below it are drawn as
    /// before the anchor, and the legend says so. One older than the
    /// window is only the line under it.
    #[test]
    fn the_window_with_the_anchor_inside_or_older() {
        let html = render(&admin_with(following(0, 3_412_871), vec![]));
        let pips = pip_classes(&html);
        assert_eq!(pips[..19], ["pip before"; 19]);
        assert_eq!(pips[19..], ["pip"; 11]);
        assert!(html.contains("before the anchor"));
        let html = render(&admin_with(following(0, 3_000_000), vec![]));
        assert!(!pip_classes(&html).contains(&"pip before"));
        assert!(html.contains("Anchored at block 3,000,000, 2d ago"));
    }

    /// Held: one box with the reason and a neutral button to take a new
    /// anchor, no window; the facts still fold under "How it's checked".
    #[test]
    fn the_held_state_offers_a_new_anchor() {
        let mut proof = following(0, 1);
        proof.state = ProofState::Held;
        proof.summary = "Every node's chain left the proven one more than 720 blocks back.".into();
        let html = render(&admin_with(proof, vec![]));
        assert!(html.contains(r#"<span class="tag tag-error">settlement held</span>"#));
        assert!(
            html.contains(concat!(
                r#"<div class="proof-held"><p>Every node's chain left the proven one more than 720 blocks back.</p>"#,
                r#"<form method="post" action="/dashboard/admin/proof/mainnet/reanchor">"#,
                r#"<p class="hint">A new anchor is taken on the nodes' word, 720 blocks below their tips. Check that the nodes are your own or ones you trust before taking one.</p>"#,
                r#"<button type="submit">Take a new anchor</button></form></div>"#,
            )),
            "{html}"
        );
        assert!(!html.contains("pips"), "{html}");
        assert!(!html.contains("Orders settle on"), "{html}");
        assert!(html.contains("<summary>How it's checked</summary>"));

        let html = render(&admin_with(following(0, 1), vec![]));
        assert!(!html.contains("Take a new anchor"), "only while held");
    }

    /// The facts fold under "How it's checked", a name and a value each.
    #[test]
    fn the_facts_fold_under_how_its_checked() {
        let html = render(&admin_with(following(3, 1), vec![]));
        assert!(
            html.contains(concat!(
                r#"<details class="proof-facts"><summary>How it's checked</summary><dl class="facts">"#,
                "<dt>Anchor</dt><dd>Block 3,412,160, 2 of 3 nodes agreed, 2d ago</dd>",
                "<dt>Last checked</dt><dd>3s ago</dd></dl>",
            )),
            "{html}"
        );
    }
}
