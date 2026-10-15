//! `GET /status` - `http::status_page::status_page`.

use maud::{html, Markup, PreEscaped};
pub use shared::proof::{NodeVerdict, ProofState};
use ui::{Alert, Button, ButtonType, Cell, CellPart, Disclosure, Fact, Facts, Hint, LinkButton, Panel, Row, Slot, Table, TableLook, Tag, TagTone, Tone};

use super::scaling::{network_name, thousands};
use super::PageChrome;

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
            (Hint::new(format!(
                "Generated {}, with node heights as of that moment. The scan loop polls every {}s.",
                data.generated_at_display, data.poll_interval_secs
            )))

            @if let Some(alert) = &data.snp_alert {
                (Alert::new(alert.as_str(), Tone::Error).block(true).attr("id", "snp-policy-alert"))
            }
            @for message in &data.slow_blocks {
                (Alert::new(message.as_str(), Tone::Notice).flag("data-slow-block"))
            }
            @if let Some(abuse) = &data.abuse {
                (Panel::new(abuse_protection(abuse)).attr("id", "abuse-protection"))
            }
            @if let Some(error) = &data.engine_error {
                (Alert::new(format!("The engine could not be reached: {error}"), Tone::Error).quiet(true).block(true))
            } @else if data.networks.is_empty() {
                (Panel::new(html! { p { "No Monero nodes are configured on this engine." } }))
            } @else {
                @for network in &data.networks {
                    (network_card(network, data.engine_page))
                }
            }
        }
    }
}

/// Abuse-protection activity over the last hour, for operators.
fn abuse_protection(abuse: &AbuseStatusView) -> Markup {
    let row = |what: &str, count: u64| {
        Slot::from(Row::new(Slot::many([
            Cell::new(what).into(),
            Cell::new(count.to_string()).into(),
        ])))
    };
    html! {
        div class="net-head" { h2 { "Abuse protection" } }
        (Hint::new("Only operators see this."))
        p {
            "Under-attack mode: "
            @if abuse.under_attack { (Tag::new("on").tone(TagTone::Error)) " - every visitor who isn't signed in is challenged." }
            @else { (Tag::new("off").tone(TagTone::Ok)) }
        }
        (Table::new(Slot::many([
            row("Issued", abuse.issued),
            row("Solved", abuse.solved),
            row("Refused", abuse.refused),
        ]))
        .head(Row::new(Slot::many([
            Cell::new("Last hour").header(true).into(),
            Cell::new("Challenges").header(true).into(),
        ])))
        .scroll(false))
        (Hint::new("Refused counts wrong or replayed answers and requests turned away past the hard limit."))
    }
}

/// One network: its title and state, its nodes, its scanner and, for an
/// admin, its proof-of-work checking and announcements.
fn network_card(network: &StatusNetworkView, engine_page: bool) -> Markup {
    let name = network_name(&network.network);
    let scanner = &network.scanner;
    let scanner_tag = html! {
        span class="mk-status-text" data-tone=[tone(&scanner.status_tag_class)] { (scanner.status_label) }
    };
    let body = html! {
        div class="net-head" {
            h2 { (name) }
            (scanner_tag)
            @if let Some(proof) = &network.proof {
                @let (label, class) = proof_tag(proof.state);
                span class="mk-status-text" data-tone=[tone(class)] { (label) }
            }
            @if engine_page {
                (LinkButton::new(html! { (PreEscaped(ui::assets::icons::EYE)) "Watch it live" }, format!("/status/engine?network={}", network.network)).attr("data-watch-live", ""))
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
            (Alert::new(last_error.as_str(), Tone::Error).quiet(true).block(true))
        }
        @if let Some(headers_first) = &network.headers_first {
            (Hint::new(headers_first.as_str()))
        }
        @if let Some(proof) = &network.proof {
            (proof_section(&network.network, &name, proof))
        }
        @if let Some(announcements) = &network.announcements {
            (announcements_section(announcements))
        }
    };
    html! {
        (Panel::new(body).attr("id", format!("network-{}", network.network)).flag("data-net-card"))
    }
}

/// The words' tone for a status beside a title, from the tag class it
/// would have as a chip: the words say the state, the colour only backs
/// them up.
fn tone(tag_class: &str) -> Option<&'static str> {
    match tag_class {
        "tag-ok" => Some("success"),
        "tag-error" => Some("error"),
        "tag-warn" => Some("warning"),
        _ => None,
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

/// The nodes, one row each: a card each on a phone (`TableLook::Cards`),
/// its name and use first, then its proof-of-work verdict and height, then
/// what went wrong. Reachability shows on a phone's card only when it's a
/// problem (`data-quiet` otherwise). The Proof of work column only while
/// checking is shown.
fn node_table(network: &StatusNetworkView) -> Markup {
    let proof = network.proof.is_some();
    let head = |name: &str| Slot::from(Cell::new(name).header(true));
    let mut heads = vec![head("Node"), head("Use")];
    if proof {
        heads.push(head("Proof of work"));
    }
    heads.extend([head("Height"), head("Reachable")]);
    let rows = network.nodes.iter().map(|node| {
        let detail = node
            .detail
            .as_ref()
            .map(|detail| html! { span class="sub" { (detail) } });
        let mut cells = vec![
            Slot::from(Cell::new(html! { code { (node.label) } }).part(CellPart::Title)),
            Slot::from(
                Cell::new(match node.node_use {
                    NodeUse::InUse => Slot::from("in use"),
                    NodeUse::Standby => Slot::from(html! { span class="muted" { "standby" } }),
                    NodeUse::LeftOut => Slot::from(Tag::new("left out").tone(TagTone::Error)),
                })
                .part(CellPart::Status),
            ),
        ];
        if proof {
            let verdict = node.verdict.map(verdict_words);
            cells.push(Slot::from(
                Cell::new(html! {
                    @if let Some((words, class)) = verdict { span class=(class) { (words) } }
                    (detail.clone().unwrap_or_default())
                })
                .part(CellPart::Amount),
            ));
        }
        cells.push(Slot::from(Cell::new(node.height_display.as_str()).part(CellPart::When)));
        let reachable = if node.is_reachable {
            Tag::new("reachable").tone(TagTone::Ok)
        } else {
            Tag::new("unreachable").tone(TagTone::Error)
        };
        let mut meta = Cell::new(html! {
            (reachable)
            @if !proof { (detail.unwrap_or_default()) }
        })
        .part(CellPart::Meta);
        if node.is_reachable {
            meta = meta.flag("data-quiet");
        }
        cells.push(Slot::from(meta));
        Slot::from(Row::new(Slot::many(cells)))
    });
    html! {
        (Table::new(Slot::many(rows))
            .head(Row::new(Slot::many(heads)))
            .look(TableLook::Cards)
            .attr("data-table", "nodes"))
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
                        (Hint::new(concat!(
                            "A new anchor is taken on the nodes' word, 720 blocks below their tips. ",
                            "Check that the nodes are your own or ones you trust before taking one."
                        )))
                        (Button::new("Take a new anchor").kind(ButtonType::Submit))
                    }
                }
            } @else if let Some(window) = &proof.window {
                (proof_window(window))
            } @else {
                p { (proof.summary) }
            }
            @if !proof.facts.is_empty() {
                (Disclosure::new("How it's checked", Slot::many([
                    Slot::from(Facts::new(proof.facts.iter().map(|(term, value)| Fact {
                        term: (*term).to_owned(),
                        value: value.as_str().into(),
                    }).collect::<Vec<_>>())),
                    Slot::from(Hint::new(format!(
                        "Only operators see this section. Orders on {name} settle only on blocks whose proof of work the engine checked itself."
                    ))),
                ])).flag("data-proof-facts"))
            }
        }
    }
}

/// The last [`WINDOW_BLOCKS`] blocks at a fixed scale: filled when proven,
/// outlined when only seen, the block orders settle up to ringed and the
/// tip marked at the end. The sentence and its chip say the same in words;
/// the figures sit on their own line, never along the blocks. The chip is
/// the same neutral tag at every count: a few blocks behind is checking
/// at work, not a problem.
fn proof_window(window: &ProofWindow) -> Markup {
    let chip = match window.behind() {
        0 => "caught up".to_owned(),
        n => format!("{n} behind the tip"),
    };
    let pips = window.pips();
    let before_anchor = pips.iter().any(|(_, pip)| *pip == Pip::BeforeAnchor);
    html! {
        p class="pline" {
            "Orders settle on blocks up to " strong { (thousands(window.ceiling)) } ", proven by the engine itself."
            " " (Tag::new(chip).tone(TagTone::Unknown))
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
            (Hint::new(format!("Anchored at block {}, {age}: every block since then is checked.", thousands(*anchor)))
                .flag("data-pip-anchor"))
        }
    }
}

/// A network's ZMQ publishers and what their announcements did.
fn announcements_section(announcements: &AnnouncementsView) -> Markup {
    html! {
        section class="announcements" {
            h3 class="caps" { "Announcements (ZMQ)" }
            (Hint::new(concat!(
                "Only operators see this. A node's announcements start a check at once instead of at the next poll; ",
                "the regular checks carry on and find anything they miss."
            )))
            (publishers_table(&announcements.publishers))
            p {
                "Started early: " (announcements.pool_passes_woken) " mempool checks, "
                (announcements.rounds_woken) " scan rounds."
            }
            (Hint::new(concat!(
                "A transaction the node relays privately (Dandelion++ stem, or one sent over Tor or I2P) is never announced; ",
                "the regular check finds it once it is public."
            )))
        }
    }
}

/// The ZMQ publishers, a row each, and its last failure on a row under it.
fn publishers_table(publishers: &[PublisherView]) -> Markup {
    let head = Row::new(Slot::many(
        ["Node", "Publisher", "State", "Transactions", "Blocks", "Last", "Connections"]
            .map(|name| Slot::from(Cell::new(name).header(true))),
    ));
    let rows = publishers.iter().flat_map(|publisher| {
        let state = if publisher.connected {
            Tag::new("connected").tone(TagTone::Ok)
        } else {
            Tag::new("not connected").tone(TagTone::Error)
        };
        let row = Slot::from(Row::new(Slot::many([
            Slot::from(Cell::new(html! { code { (publisher.node) } })),
            Slot::from(Cell::new(html! { code { (publisher.endpoint) } })),
            Slot::from(Cell::new(html! { (state) " " span class="muted" { (publisher.state_display) } })),
            Slot::from(Cell::new(publisher.pool_announcements.to_string())),
            Slot::from(Cell::new(publisher.block_announcements.to_string())),
            Slot::from(Cell::new(publisher.last_announcement_display.as_str())),
            Slot::from(Cell::new(publisher.connections.to_string())),
        ])));
        let error = publisher.last_error.as_ref().map(|error| {
            Slot::from(Row::new(Cell::new(Hint::new(error.as_str()).inline(true)).attr("colspan", "7")))
        });
        std::iter::once(row).chain(error)
    });
    html! { (Table::new(Slot::many(rows)).head(head)) }
}

pub fn page(chrome: &PageChrome, data: &StatusPageViewModel) -> Markup {
    let mut header = ui::PageHeader::new("Engine status").actions(super::reload_button("/status"));
    if chrome.logged_in {
        header = header.crumbs(super::crumbs(&[("/", "Home")]));
    }
    let body = html! {
        (live_fragment(data))
        // Streams the part above while the page is open, with JavaScript.
        span hidden fx-action="/status/events" fx-trigger="fx:inited" fx-swap="none" fx-sse-reconnect {}
    };
    super::Page::new("Status - Monokulo", header).render(chrome, body)
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
            html.contains(r#"<p class="notice" role="status" data-slow-block="">Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far.</p>"#),
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
            r#"<div class="error">scanner blew up</div>"#,
            r#"<p class="hint">Reading block headers first for about 50 minutes more.</p>"#,
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
                r#"<span class="mk-status-text" data-tone="success">scanning</span>"#,
                r#"<span class="mk-status-text" data-tone="success">proof checked</span>"#,
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
        assert!(html.contains(r#"<span class="mk-status-text">finding an anchor</span>"#));
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
            html.contains(r#"<a class="mk-btn" href="/status/engine?network=mainnet" data-watch-live=""><svg xmlns="http://www.w3.org/2000/svg" class="mk-icon""#)
                && html.contains("</svg>Watch it live</a></div>"),
            "an eye, then the words: {html}"
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
            html.contains(r#"<p class="line scanner-line"><span class="mk-status-text" data-tone="success">scanning</span> last tick 2s ago · 41,203 ticks · 3 stores scanned last tick</p>"#),
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
            html.contains(r#"<div class="table-scroll"><table class="table-cards" data-table="nodes">"#),
            "{html}"
        );
        assert!(
            html.contains(concat!(
                r#"<tr><td class="card-title"><code>a:18081</code></td>"#,
                r#"<td class="card-status">in use</td>"#,
                r#"<td class="card-amount"><span class="verdict is-ok">on the proven chain</span></td>"#,
                r#"<td class="card-when">3,412,880</td>"#,
                r#"<td class="card-meta" data-quiet=""><span class="tag tag-ok">reachable</span></td></tr>"#,
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
            r#"table[data-table="nodes"] td.card-amount { display: contents; font-weight: 400; }"#,
            r#"table[data-table="nodes"] .card-meta[data-quiet] { display: none; }"#,
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
            html.contains(r#"<p class="pline">Orders settle on blocks up to <strong>3,412,881</strong>, proven by the engine itself. <span class="tag tag-unknown">caught up</span></p>"#),
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
            r#"<p class="hint" data-pip-anchor="">Anchored at block 3,412,160, 2d ago: every block since then is checked.</p>"#
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

    /// Fourteen behind: the same neutral chip, never amber.
    #[test]
    fn the_window_fourteen_behind() {
        let html = render(&admin_with(following(14, 3_412_160), vec![]));
        assert!(
            html.contains(r#"<span class="tag tag-unknown">14 behind the tip</span>"#),
            "{html}"
        );
        assert!(!html.contains("tag-slow"), "{html}");
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
        assert!(html
            .contains(r#"<span class="mk-status-text" data-tone="error">settlement held</span>"#));
        assert!(
            html.contains(concat!(
                r#"<div class="proof-held"><p>Every node's chain left the proven one more than 720 blocks back.</p>"#,
                r#"<form method="post" action="/dashboard/admin/proof/mainnet/reanchor">"#,
                r#"<p class="hint">A new anchor is taken on the nodes' word, 720 blocks below their tips. Check that the nodes are your own or ones you trust before taking one.</p>"#,
                r#"<button class="mk-btn" type="submit">Take a new anchor</button></form></div>"#,
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
                r#"<details class="disclosure" data-proof-facts=""><summary>How it's checked</summary><dl class="facts">"#,
                "<dt>Anchor</dt><dd>Block 3,412,160, 2 of 3 nodes agreed, 2d ago</dd>",
                "<dt>Last checked</dt><dd>3s ago</dd></dl>",
            )),
            "{html}"
        );
    }
}
