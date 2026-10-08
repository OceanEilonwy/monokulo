//! Admin-only pages: first-run setup (`GET`/`POST /admin/setup`), the
//! settings page (`GET`/`POST /dashboard/admin/settings?tab=<id>`), the invites page
//! (`GET /dashboard/admin/invites` and its create-link/delete actions), and
//! the public `GET`/`POST /request-invite` form.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// `error` means the same thing every other page's own re-render-on-
/// rejection `error` field does; `email` is echoed back into the form on a
/// rejected submission (a duplicate email, mismatched passwords) so the
/// operator doesn't have to retype it - the two password fields are never
/// echoed back, same no-echo-a-password convention every other credential
/// field in this codebase follows.
pub struct SetupViewModel {
    pub error: Option<String>,
    pub email: String,
}

pub fn setup_page(chrome: &PageChrome, data: &SetupViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/" { "Home" } }
            h1 { "Set up your admin account" }
            p {
                "This is a one-time step. The account created here is this instance's single administrator, "
                "and can configure every monokulo and scanner setting from the admin page afterward."
            }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }
            form method="post" action="/admin/setup" {
                label { "Email " input type="email" name="email" value=(data.email) required; }
                label { "Password " input type="password" name="password" required minlength="12"; }
                label { "Confirm password " input type="password" name="confirm_password" required minlength="12"; }
                button type="submit" class="btn-primary" { "Create admin account" }
            }
        }
    };
    layout(chrome, "Set up admin account - Monokulo", body)
}

pub struct RequestInviteViewModel {
    pub error: Option<String>,
    /// `true` after a successful `POST` - the page shows a plain thank-you
    /// message instead of the form again, so a visitor can't accidentally
    /// double-submit by refreshing.
    pub submitted: bool,
}

pub fn request_invite_page(chrome: &PageChrome, data: &RequestInviteViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/" { "Home" } }
            h1 { "Request an invite" }
            @if data.submitted {
                p { "Thanks - we'll be in touch if there's a spot for you." }
            } @else {
                @if let Some(error) = &data.error {
                    p class="error" { (error) }
                }
                p { "This instance is invite-only right now. Tell us a bit about what you'd like to use it for and we'll follow up." }
                form method="post" action="/request-invite" {
                    label { "Email " input type="email" name="email" required; }
                    label { "Message " textarea name="message" required {} }
                    button type="submit" class="btn-primary" { "Request an invite" }
                }
            }
        }
    };
    layout(chrome, "Request an invite - Monokulo", body)
}

/// One row on the admin invites page's pending-requests table - the
/// `mailto:` link is built server-side (real HTML `<a href>`, no JS) and is
/// `None` only for the pathological case of a request whose linked invite
/// couldn't be decrypted.
pub struct AdminInviteRequestRow {
    pub id: String,
    pub email: String,
    pub message: String,
    /// When it arrived, in the admin's zone (`views::time`).
    pub created_at_display: Markup,
    pub mailto_href: Option<String>,
    /// Set only for the one row this page just deleted - rendered struck-
    /// through, as a one-time confirmation, outside the page's own real
    /// pagination count.
    pub just_deleted: bool,
}

pub struct AdminInvitesViewModel {
    pub error: Option<String>,
    pub success: Option<String>,
    pub just_deleted_row: Option<AdminInviteRequestRow>,
    pub rows: Vec<AdminInviteRequestRow>,
    pub page: u32,
    pub total_pages: u32,
    pub has_previous: bool,
    pub has_next: bool,
    pub previous_page: u32,
    pub next_page: u32,
    /// The freshly generated standalone link from "create invite link" -
    /// shown exactly once, on this one response, never stored reversibly.
    pub created_link: Option<String>,
}

fn invite_row(row: &AdminInviteRequestRow, page: u32) -> Markup {
    html! {
        tr class=[row.just_deleted.then_some("row-deleted")] {
            td { (row.email) }
            td { (row.message) }
            td { (row.created_at_display) }
            td {
                @if row.just_deleted {
                    "deleted"
                } @else {
                    @if let Some(href) = &row.mailto_href {
                        a href=(href) { "email invite" }
                    }
                    form method="post" action=(format!("/dashboard/admin/invites/{}/delete?page={}", row.id, page)) class="inline-form"
                        fx-action=(format!("/dashboard/admin/invites/{}/delete?page={}", row.id, page)) fx-method="POST" fx-target="#invites" {
                        button type="submit" { "delete" }
                    }
                }
            }
        }
    }
}

/// Everything on the invites page below its heading: what fixi swaps back
/// after any button here, or a page change.
pub fn invites_section(data: &AdminInvitesViewModel) -> Markup {
    let target = "#invites";
    html! {
        section id="invites" {
            @if let Some(error) = &data.error {
                p class="error" role="alert" data-fx-focus tabindex="-1" { (error) }
            }
            @if let Some(success) = &data.success {
                p class="success" role="status" data-fx-focus tabindex="-1" { (success) }
            }

            form method="post" action="/dashboard/admin/invites/create-link"
                fx-action="/dashboard/admin/invites/create-link" fx-method="POST" fx-target=(target) {
                button type="submit" class="btn-primary" { "Create invite link" }
            }
            @if let Some(link) = &data.created_link {
                p { "Share this link - it works once:" }
                pre data-fx-focus tabindex="-1" { (link) }
            }

            h2 { "Pending requests" }
            @if !data.rows.is_empty() {
                table {
                    thead { tr { th { "Email" } th { "Message" } th { "Requested" } th { "Actions" } } }
                    tbody {
                        @if let Some(deleted) = &data.just_deleted_row {
                            (invite_row(deleted, data.page))
                        }
                        @for row in &data.rows {
                            (invite_row(row, data.page))
                        }
                    }
                }
                nav class="pagination" {
                    @if data.has_previous {
                        @let href = format!("/dashboard/admin/invites?page={}", data.previous_page);
                        a href=(href) fx-action=(href) fx-target=(target) fx-push-url { "Previous" }
                    }
                    span { "Page " (data.page) " of " (data.total_pages) }
                    @if data.has_next {
                        @let href = format!("/dashboard/admin/invites?page={}", data.next_page);
                        a href=(href) fx-action=(href) fx-target=(target) fx-push-url { "Next" }
                    }
                }
                form method="post" action="/dashboard/admin/invites/delete-all"
                    fx-action="/dashboard/admin/invites/delete-all" fx-method="POST" fx-target=(target) {
                    button type="submit" { "Delete all" }
                }
            } @else if let Some(deleted) = &data.just_deleted_row {
                table {
                    thead { tr { th { "Email" } th { "Message" } th { "Requested" } th { "Actions" } } }
                    tbody { (invite_row(deleted, data.page)) }
                }
            } @else {
                p class="hint" { "No pending invite requests." }
            }
        }
    }
}

pub fn admin_invites_page(chrome: &PageChrome, data: &AdminInvitesViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Invites" }
            (invites_section(data))
        }
    };
    layout(chrome, "Invites - Monokulo", body)
}

/// What kind of value a setting takes, which decides its input control
/// (task 4.6). Mirrors `live_settings::SettingKind`, owned, so the engine's
/// settings (fetched as JSON) and monokulo's own share one renderer.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKindView {
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Bool,
    Choice {
        choices: Vec<String>,
    },
    ChoiceList {
        choices: Vec<String>,
    },
    Url,
    Address,
    Path,
    #[default]
    Text,
    Secret,
    Json,
    /// A Unix time until which something stays on (development logging),
    /// chosen as "off" or "on for N hours" from `now`. Never sent by the
    /// engine; set by the handler for keys it knows (structured_logging.md
    /// 1.3).
    #[serde(skip)]
    TimeLimit {
        now: u64,
        /// When it ends, in the admin's own zone like every other time on
        /// the page.
        until_label: String,
    },
}

impl From<live_settings::SettingKind> for SettingKindView {
    fn from(kind: live_settings::SettingKind) -> Self {
        use live_settings::SettingKind as K;
        let owned = |choices: Vec<&'static str>| choices.into_iter().map(str::to_string).collect();
        match kind {
            K::Integer { min, max } => SettingKindView::Integer { min, max },
            K::Bool => SettingKindView::Bool,
            K::Choice { choices } => SettingKindView::Choice {
                choices: owned(choices),
            },
            K::ChoiceList { choices } => SettingKindView::ChoiceList {
                choices: owned(choices),
            },
            K::Url => SettingKindView::Url,
            K::Address => SettingKindView::Address,
            K::Path => SettingKindView::Path,
            K::Text => SettingKindView::Text,
            K::Secret => SettingKindView::Secret,
            K::Json => SettingKindView::Json,
        }
    }
}

/// One editable field on the admin settings page - either one of monokulo's
/// own settings or one of the engine's, fetched live over HTTP.
#[derive(Debug, Clone, Default)]
pub struct AdminScalarFieldView {
    /// The stable settings-table key.
    pub key: String,
    /// The form field's `name`, when it isn't `key`: an engine setting
    /// whose key monokulo also has (`logging.level`) is sent as
    /// `engine:<key>`, so the two can share a tab (and a form).
    pub name: String,
    pub label: String,
    /// The field's current *effective* value - what wins under
    /// `environment > command line > options file or database > default`.
    /// Masked for secrets.
    pub value: String,
    /// Where that value comes from: the chip beside the name.
    pub source: SettingSourceView,
    /// What the setting is for (task 4.1).
    pub help: Option<String>,
    pub kind: SettingKindView,
    pub example: Option<String>,
    /// Only applies after a restart.
    pub restart_only: bool,
    /// Saved, but still waiting for that restart.
    pub pending_restart: bool,
    /// Why the value in effect isn't the one set, if it isn't.
    pub problem: Option<String>,
    /// Why the page can't change it, if it can't (given on the command
    /// line or in the environment, or kept in an options file that can't
    /// be written): shown, with a padlock and this reason, but not
    /// editable or sent with the form.
    pub locked: Option<String>,
    /// The saved value, when `value` is one a save just refused (shown
    /// again to fix): with JavaScript the setting is then still unsaved,
    /// and Discard goes back to this.
    pub saved_value: Option<String>,
}

/// Where a setting's value comes from, as the chip beside its name shows
/// it, lowest precedence first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingSourceView {
    /// Nothing sets it.
    #[default]
    Default,
    /// The options file, which saving on this page writes.
    OptionsFile,
    /// A runtime switch, kept in the database.
    Runtime,
    /// A command-line option.
    CommandLine,
    /// An environment variable (secrets only).
    Environment,
}

impl SettingSourceView {
    /// From the name the registry and the engine's API give a source.
    pub fn from_name(name: &str) -> Self {
        match name {
            "toml" => SettingSourceView::OptionsFile,
            "database" => SettingSourceView::Runtime,
            "cli" => SettingSourceView::CommandLine,
            "env" => SettingSourceView::Environment,
            _ => SettingSourceView::Default,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingSourceView::Default => "Default",
            SettingSourceView::OptionsFile => "Options file",
            SettingSourceView::Runtime => "Runtime",
            SettingSourceView::CommandLine => "Command line",
            SettingSourceView::Environment => "Environment",
        }
    }

    /// The whole sentence, for the chip's tooltip and screen readers.
    fn sentence(self) -> &'static str {
        match self {
            SettingSourceView::Default => "The default: nothing sets it.",
            SettingSourceView::OptionsFile => "From the options file, which saving here writes.",
            SettingSourceView::Runtime => "A runtime switch, kept in the database.",
            SettingSourceView::CommandLine => {
                "From a command-line option, which wins over the options file."
            }
            SettingSourceView::Environment => "From an environment variable.",
        }
    }
}

/// A 24-unit stroked icon, drawn in the text colour.
fn icon(paths: Markup) -> Markup {
    html! {
        svg class="src-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false" { (paths) }
    }
}

fn source_icon(source: SettingSourceView) -> Markup {
    icon(match source {
        SettingSourceView::Default => html! {
            circle cx="12" cy="12" r="8" stroke-dasharray="2.5 3" {}
            circle class="filled" cx="12" cy="12" r="1.6" {}
        },
        SettingSourceView::OptionsFile => file_icon_paths(),
        SettingSourceView::Runtime => html! {
            ellipse cx="12" cy="5" rx="7" ry="3" {}
            path d="M5 5v14c0 1.7 3.1 3 7 3s7-1.3 7-3V5" {}
            path d="M5 12c0 1.7 3.1 3 7 3s7-1.3 7-3" {}
        },
        SettingSourceView::CommandLine => html! {
            rect x="3" y="4" width="18" height="16" rx="2" {}
            path d="m7 9 3 3-3 3" {}
            path d="M13 15h4" {}
        },
        SettingSourceView::Environment => html! {
            path d="M8 4H7a2 2 0 0 0-2 2v4l-2 2 2 2v4a2 2 0 0 0 2 2h1" {}
            path d="M16 4h1a2 2 0 0 1 2 2v4l2 2-2 2v4a2 2 0 0 1-2 2h-1" {}
            path d="M14.2 9.3c-.4-.8-1.2-1.3-2.2-1.3-1.3 0-2.3.7-2.3 1.8 0 2.4 4.6 1.4 4.6 3.9 0 1.1-1 1.8-2.3 1.8-1 0-1.9-.5-2.3-1.3" {}
            path d="M12 6.5V8" {}
            path d="M12 16v1.5" {}
        },
    })
}

fn file_icon_paths() -> Markup {
    html! {
        path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" {}
        path d="M14 3v5h5" {}
        path d="M9 13h6" {}
        path d="M9 17h4" {}
    }
}

/// The chip beside a setting's name saying where its value comes from; the
/// whole sentence is its tooltip and what a screen reader reads.
fn source_chip(source: SettingSourceView) -> Markup {
    html! {
        span class={ "source-chip" @if source == SettingSourceView::Default { " default" } }
            title=(source.sentence()) {
            (source_icon(source))
            span aria-hidden="true" { (source.label()) }
            span class="visually-hidden" { (source.sentence()) }
        }
    }
}

/// One process's options file, as the bar above the tabs shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionsFileView {
    pub owner: SettingOwner,
    pub path: String,
    pub exists: bool,
    pub writable: bool,
}

/// A node's status from the engine's `/status`, for its row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeStatusView {
    /// Its height, when it answered.
    pub height: Option<u64>,
    /// Why it didn't answer.
    pub error: Option<String>,
    /// The network it says it's on, when that isn't the one it's saved for.
    pub wrong_network: Option<String>,
    /// The node the engine is using now.
    pub in_use: bool,
    /// Skipped for a while after failing.
    pub resting: bool,
    /// How many blocks it is behind the highest of its network's nodes.
    pub behind: Option<u64>,
    /// What the engine measured of its link.
    pub link: Option<super::scaling::NodeLinkView>,
}

/// One node row on the Monero nodes tab.
#[derive(Debug, Clone, Default)]
pub struct NodeRowView {
    pub row: crate::admin_nodes::NodeRow,
    /// The engine's label for the node (`host:port` as saved), which its
    /// `/status` is found by. Empty for a row that isn't a node yet.
    pub label: String,
    /// `None` for a node with no status yet (just saved, or `/status`
    /// didn't answer).
    pub status: Option<NodeStatusView>,
    /// Its place in the network's saved list, which tells the page's script
    /// when the rows have been reordered. `None` for rows a save refused,
    /// shown as they were sent.
    pub saved_index: Option<usize>,
}

/// One network's block on the Monero nodes tab: its nodes as rows, primary
/// first.
#[derive(Debug, Clone, Default)]
pub struct AdminNetworkFieldView {
    pub network: String,
    /// Empty when this network has no node configured yet.
    pub rows: Vec<NodeRowView>,
    /// An address to show as the example in the address help, from the
    /// engine's example for this network.
    pub example_address: Option<String>,
    /// Stores on this network, for the confirmation before clearing it.
    pub tenant_count: u64,
    /// Why the engine refused this network's nodes (a node on another
    /// network), shown at the top of its block.
    pub error: Option<String>,
    /// How its block scan is going, from the engine's `/status`.
    pub scaling: Option<shared::scaling::NetworkScaling>,
    /// How many nodes it has saved, which `rows` can differ from after a
    /// refused save: the confirmation before clearing a network asks only
    /// when it had some.
    pub saved_count: usize,
}

/// A banner at the top of the page: what stays true after a save, or the
/// welcome after setup (task 4.5). What a save itself did is its toast.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Good news that stays on the page (the welcome after setup).
    Success(String),
    /// Saved, but something is now broken (stores without a node, an
    /// engine that doesn't answer).
    Error(String),
}

/// Which process a setting belongs to: monokulo's own registry, or the
/// engine's (fetched and saved over its admin API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingOwner {
    Monokulo,
    Engine,
}

/// The admin settings page's tabs, grouped by job rather than by which
/// process owns a setting (nicer_admin_screen.md T1). Each is its own URL,
/// `/dashboard/admin/settings?tab=<id>`, so switching works as a plain link
/// and a save can send the browser back to the tab it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Nodes,
    Payments,
    Custody,
    Abuse,
    Server,
    Logging,
    /// Engine (or monokulo) settings added after this map was written and
    /// not placed yet. Only shown while something is in it.
    Other,
}

impl SettingsTab {
    /// Every tab, in the order the tab bar shows them. General comes first:
    /// the engine's tabs stay empty until its connection there works.
    pub const ALL: [SettingsTab; 8] = [
        SettingsTab::General,
        SettingsTab::Nodes,
        SettingsTab::Payments,
        SettingsTab::Custody,
        SettingsTab::Abuse,
        SettingsTab::Server,
        SettingsTab::Logging,
        SettingsTab::Other,
    ];

    /// The tab's `?tab=` value.
    pub fn id(self) -> &'static str {
        match self {
            SettingsTab::General => "general",
            SettingsTab::Nodes => "nodes",
            SettingsTab::Payments => "payments",
            SettingsTab::Custody => "custody",
            SettingsTab::Abuse => "abuse",
            SettingsTab::Server => "server",
            SettingsTab::Logging => "logging",
            SettingsTab::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Nodes => "Monero nodes",
            SettingsTab::Payments => "Payments",
            SettingsTab::Custody => "Key custody",
            SettingsTab::Abuse => "Abuse protection",
            SettingsTab::Server => "Server",
            SettingsTab::Logging => "Logging",
            SettingsTab::Other => "Other",
        }
    }

    /// The tab a `?tab=` value names; General for a missing or unknown one,
    /// so an old bookmark or a typo still lands somewhere useful.
    pub fn from_id(id: Option<&str>) -> SettingsTab {
        SettingsTab::ALL
            .into_iter()
            .find(|tab| Some(tab.id()) == id)
            .unwrap_or(SettingsTab::General)
    }

    /// The tab's page.
    pub fn href(self) -> String {
        format!("/dashboard/admin/settings?tab={}", self.id())
    }

    /// Whether every setting on this tab is the engine's, so the tab has
    /// nothing to show or save while the engine can't be reached.
    pub fn engine_only(self) -> bool {
        self == SettingsTab::Nodes
    }

    /// The tab's cards that are always there, in order. The Monero nodes
    /// tab's networks come first, and each key custody backend's card after
    /// Backends, from what the engine reports ([`tab_groups`]).
    fn fixed_groups(self) -> &'static [Group] {
        use SettingOwner::{Engine, Monokulo};
        match self {
            SettingsTab::General => &[Group::Signup, Group::PublicAddress, Group::EngineConnection],
            SettingsTab::Nodes => &[Group::AllNodes],
            SettingsTab::Payments => &[
                Group::Orders,
                Group::Chain,
                Group::Webhooks,
                Group::ExchangeRates,
            ],
            SettingsTab::Custody => &[Group::CustodyBackends, Group::CustodyCli],
            SettingsTab::Abuse => &[
                Group::AbuseLimits,
                Group::AbuseChallenge,
                Group::AbuseVisitors,
            ],
            SettingsTab::Server => &[Group::Server(Monokulo), Group::Server(Engine)],
            SettingsTab::Logging => &[Group::Logging(Monokulo), Group::Logging(Engine)],
            SettingsTab::Other => &[Group::Other(Monokulo), Group::Other(Engine)],
        }
    }
}

/// A key custody backend this page knows the settings of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyBackend {
    Plain,
    Snp,
}

impl CustodyBackend {
    /// The backend a name the engine gives one (`snp`) is, if the page
    /// knows it.
    pub fn parse(name: &str) -> Option<CustodyBackend> {
        match name {
            "plain" => Some(CustodyBackend::Plain),
            "snp" => Some(CustodyBackend::Snp),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CustodyBackend::Plain => "plain",
            CustodyBackend::Snp => "snp",
        }
    }
}

/// A card on the admin settings page: settings shown, saved and refused
/// together. Where a setting goes is [`Group::of`]; its tab is
/// [`Group::tab`]; its id in the page (`network-stagenet`, after `card-`
/// for the card's element id) is its `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Signup,
    PublicAddress,
    EngineConnection,
    /// A network's nodes and its proof-of-work switch.
    Network(monero::Network),
    /// Settings for every node at once.
    AllNodes,
    Orders,
    Chain,
    Webhooks,
    ExchangeRates,
    CustodyBackends,
    /// A key custody backend's own settings, the engine's and this site's.
    Custody(CustodyBackend),
    CustodyCli,
    AbuseLimits,
    AbuseChallenge,
    AbuseVisitors,
    /// One process's server settings.
    Server(SettingOwner),
    /// One process's logging.
    Logging(SettingOwner),
    /// One process's settings this map doesn't place yet.
    Other(SettingOwner),
}

impl Group {
    /// The card a setting shows in. The one map both the page and the save
    /// use, so a setting can't be shown in one card and then refused as
    /// another's.
    pub fn of(key: &str, owner: SettingOwner) -> Group {
        let (prefix, rest) = key.split_once('.').unwrap_or((key, ""));
        // `key_custody.<backend>_...`: a setting only that backend uses.
        let backend = || {
            CustodyBackend::parse(rest.split('_').next().unwrap_or(rest))
                .map_or(Group::Other(owner), Group::Custody)
        };
        match owner {
            SettingOwner::Monokulo => match prefix {
                "signup" => Group::Signup,
                "public_url" => Group::PublicAddress,
                "engine" => Group::EngineConnection,
                "exchange_rate" => Group::ExchangeRates,
                "abuse" if matches!(rest, "challenge_bits" | "under_attack") => {
                    Group::AbuseChallenge
                }
                "abuse" if matches!(rest, "trusted_proxies" | "onion_listener") => {
                    Group::AbuseVisitors
                }
                "abuse" | "rate_limit" => Group::AbuseLimits,
                "http_cache" | "database" | "server" | "crypto" => Group::Server(owner),
                "logging" => Group::Logging(owner),
                "key_custody" if rest.starts_with("cli_") => Group::CustodyCli,
                "key_custody" => backend(),
                _ => Group::Other(owner),
            },
            SettingOwner::Engine => match prefix {
                // Each network's nodes and its proof-of-work switch, in the
                // network's own card.
                "monero_node" | "proof_of_work" => {
                    shared::network::parse_network(rest).map_or(Group::AllNodes, Group::Network)
                }
                // How much memory a scan may use is about the machine, not
                // about payments.
                "payment" if rest == "scan_chunk_memory_budget_mb" => Group::Server(owner),
                "payment" if matches!(rest, "mempool_poll_interval_ms" | "reorg_check_depth") => {
                    Group::Chain
                }
                "payment" => Group::Orders,
                "webhooks" => Group::Webhooks,
                "key_custody" if matches!(rest, "enabled_backends" | "default_backend") => {
                    Group::CustodyBackends
                }
                "key_custody" => backend(),
                "server" | "database" => Group::Server(owner),
                "logging" => Group::Logging(owner),
                _ => Group::Other(owner),
            },
        }
    }

    /// The tab the card is on.
    pub fn tab(self) -> SettingsTab {
        match self {
            Group::Signup | Group::PublicAddress | Group::EngineConnection => SettingsTab::General,
            Group::Network(_) | Group::AllNodes => SettingsTab::Nodes,
            Group::Orders | Group::Chain | Group::Webhooks | Group::ExchangeRates => {
                SettingsTab::Payments
            }
            Group::CustodyBackends | Group::Custody(_) | Group::CustodyCli => SettingsTab::Custody,
            Group::AbuseLimits | Group::AbuseChallenge | Group::AbuseVisitors => SettingsTab::Abuse,
            Group::Server(_) => SettingsTab::Server,
            Group::Logging(_) => SettingsTab::Logging,
            Group::Other(_) => SettingsTab::Other,
        }
    }

    /// The card's heading.
    pub fn title(self) -> String {
        let owner = |owner: SettingOwner| match owner {
            SettingOwner::Monokulo => "Monokulo",
            SettingOwner::Engine => "Engine",
        };
        match self {
            Group::Signup => "Sign-up".into(),
            Group::PublicAddress => "Public address".into(),
            Group::EngineConnection => "Engine connection".into(),
            Group::Network(network) => capitalized(shared::network::network_str(network)),
            Group::AllNodes => "Every node".into(),
            Group::Orders => "Orders".into(),
            Group::Chain => "Watching the chain".into(),
            Group::Webhooks => "Webhooks".into(),
            Group::ExchangeRates => "Exchange rates".into(),
            Group::CustodyBackends => "Backends".into(),
            Group::Custody(CustodyBackend::Snp) => "SEV-SNP".into(),
            Group::Custody(backend) => format!("Key custody: {}", backend.name()),
            Group::CustodyCli => "key-custody-cli downloads".into(),
            Group::AbuseLimits => "Request limits".into(),
            Group::AbuseChallenge => "Challenge".into(),
            Group::AbuseVisitors => "Telling visitors apart".into(),
            Group::Server(o) | Group::Logging(o) | Group::Other(o) => owner(o).into(),
        }
    }

    /// What the card is for, under its heading, where its name doesn't say.
    fn hint(self) -> Option<&'static str> {
        match self {
            Group::Orders => Some("Defaults for new stores. Each store's own settings win for its orders."),
            Group::ExchangeRates => Some("Which providers stores may price fiat orders with. Each store still chooses whether to use one, and in what order."),
            Group::AbuseLimits => Some("Requests a minute, per visitor, merchant or shop."),
            _ => None,
        }
    }

    /// Whether the card holds only the engine's settings: while the engine
    /// can't be reached, it has nothing to show.
    fn engine_only(self) -> bool {
        matches!(
            self,
            Group::Network(_)
                | Group::AllNodes
                | Group::Orders
                | Group::Chain
                | Group::Webhooks
                | Group::CustodyBackends
                | Group::Server(SettingOwner::Engine)
                | Group::Logging(SettingOwner::Engine)
                | Group::Other(SettingOwner::Engine)
        )
    }

    /// The card's element id, which the toast's and the save bar's Show
    /// links go to (`#card-webhooks`), and a save without JavaScript comes
    /// back to.
    pub fn card_id(self) -> String {
        format!("card-{self}")
    }
}

impl std::fmt::Display for Group {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let owner = |owner: &SettingOwner| match owner {
            SettingOwner::Monokulo => "monokulo",
            SettingOwner::Engine => "engine",
        };
        match self {
            Group::Signup => f.write_str("signup"),
            Group::PublicAddress => f.write_str("public-address"),
            Group::EngineConnection => f.write_str("engine"),
            Group::Network(network) => {
                write!(f, "network-{}", shared::network::network_str(*network))
            }
            Group::AllNodes => f.write_str("nodes-all"),
            Group::Orders => f.write_str("orders"),
            Group::Chain => f.write_str("chain"),
            Group::Webhooks => f.write_str("webhooks"),
            Group::ExchangeRates => f.write_str("exchange-rates"),
            Group::CustodyBackends => f.write_str("custody-backends"),
            Group::Custody(backend) => write!(f, "custody-{}", backend.name()),
            Group::CustodyCli => f.write_str("custody-cli"),
            Group::AbuseLimits => f.write_str("abuse-limits"),
            Group::AbuseChallenge => f.write_str("abuse-challenge"),
            Group::AbuseVisitors => f.write_str("abuse-visitors"),
            Group::Server(o) => write!(f, "server-{}", owner(o)),
            Group::Logging(o) => write!(f, "logging-{}", owner(o)),
            Group::Other(o) => write!(f, "other-{}", owner(o)),
        }
    }
}

/// Settings shown in a set order inside their card; any other keeps the
/// order it comes in (monokulo's registry order, the engine's alphabetical).
const FIELD_ORDER: &[&str] = &[
    "key_custody.enabled_backends",
    "key_custody.default_backend",
    "payment.confirmations_required",
    "payment.order_expiry_minutes",
    "payment.expired_order_grace_period_minutes",
    "payment.mempool_poll_interval_ms",
    "payment.reorg_check_depth",
    "webhooks.max_attempts",
    "webhooks.delivery_timeout_ms",
    "abuse.soft_per_min",
    "abuse.hard_per_min",
    "abuse.stream_cap",
    "abuse.client_logs_per_min",
    "abuse.signed_in_per_min",
    "rate_limit.per_store_key_per_min",
    "server.bind",
    "server.worker_threads",
    "server.cpus",
    "server.nice",
];

fn field_rank(key: &str) -> usize {
    FIELD_ORDER
        .iter()
        .position(|k| *k == key)
        .unwrap_or(FIELD_ORDER.len())
}

/// A subheading inside a card, before the setting that starts its part.
fn field_subheading(key: &str) -> Option<&'static str> {
    match key {
        "exchange_rate.coingecko_enabled" => Some("Coingecko"),
        "exchange_rate.coinmarketcap_enabled" => Some("CoinMarketCap"),
        "exchange_rate.haveno_enabled" => Some("RetoSwap (Haveno)"),
        "exchange_rate.cache_seconds" => Some("Every provider"),
        "abuse.soft_per_min" => Some("Visitors"),
        "abuse.signed_in_per_min" => Some("Merchants and shops"),
        _ => None,
    }
}

/// A small message after a save, in the corner of the window (it fades
/// on its own unless it says something wasn't saved).
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub lines: Vec<String>,
    /// The card to go to: the one a refusal is about.
    pub show: Option<Group>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Saved and applied.
    Success,
    /// Saved, but something waits for a restart.
    Warning,
    /// Something wasn't saved.
    Error,
    /// Nothing happened (nothing had changed).
    Neutral,
}

/// One thing a save refused, and why: on the card holding the setting it
/// names, or the save's own when it names none (the engine couldn't be
/// reached).
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub group: Option<Group>,
    pub message: String,
}

/// What the save a page answers did. A save is all of a tab or none of
/// it; only when monokulo's part is refused after the engine's was saved is
/// it partly saved.
#[derive(Debug, Clone, PartialEq)]
pub enum SaveOutcome {
    /// Nothing on the tab had changed, so nothing was saved.
    Unchanged,
    /// Every change was saved, at `at` (the admin's clock).
    Saved { groups: Vec<Group>, at: String },
    /// Nothing was saved, for these reasons (at least one).
    Refused { failures: Vec<Failure> },
    /// The engine's cards were saved and monokulo's refused.
    PartlySaved {
        saved: Vec<Group>,
        at: String,
        failures: Vec<Failure>,
    },
}

impl SaveOutcome {
    fn saved_at(&self, group: Group) -> Option<&str> {
        match self {
            SaveOutcome::Saved { groups, at }
            | SaveOutcome::PartlySaved {
                saved: groups, at, ..
            } if groups.contains(&group) => Some(at),
            _ => None,
        }
    }

    fn failures(&self) -> &[Failure] {
        match self {
            SaveOutcome::Refused { failures } | SaveOutcome::PartlySaved { failures, .. } => {
                failures
            }
            SaveOutcome::Unchanged | SaveOutcome::Saved { .. } => &[],
        }
    }
}

#[derive(Default)]
pub struct AdminSettingsViewModel {
    /// The tab on show.
    pub tab: SettingsTab,
    /// Banners above the tabs: what is still true after a save (a network
    /// stores use without a node), and the welcome after setup.
    pub notices: Vec<Notice>,
    /// What the save or reload this page answers did, in a toast.
    pub toast: Option<Toast>,
    /// What the save this page answers did, for its cards and save bar.
    pub outcome: Option<SaveOutcome>,
    /// The panel answers a save: the save bar's message takes focus.
    pub answers_save: bool,
    pub monokulo_fields: Vec<AdminScalarFieldView>,
    /// `true` only after a real, successful fetch of the scanner's own
    /// settings.
    pub engine_reachable: bool,
    pub engine_error: Option<String>,
    pub engine_fields: Vec<AdminScalarFieldView>,
    pub engine_networks: Vec<AdminNetworkFieldView>,
    /// Networks stores use that no node answers for, as far as monokulo
    /// knows (the engine's `/status`): the Monero nodes tab is marked.
    pub unreachable_networks: Vec<String>,
    /// Both processes' CPU and memory, on the Monero nodes tab only.
    pub resources: Option<super::scaling::ResourcesView>,
    /// monokulo's options file, then the engine's (when it answered).
    pub options_files: Vec<OptionsFileView>,
}

impl AdminSettingsViewModel {
    /// What the save refused on `group`'s card.
    fn failure(&self, group: Group) -> Option<&Failure> {
        self.outcome
            .as_ref()?
            .failures()
            .iter()
            .find(|f| f.group == Some(group))
    }

    /// When the save saved `group`'s card.
    fn saved_at(&self, group: Group) -> Option<&str> {
        self.outcome.as_ref()?.saved_at(group)
    }
}

/// The id of a setting's control, which its label and help point at.
fn field_id(key: &str) -> String {
    format!("setting-{key}")
}

/// The id of a setting's help text, when it has some.
fn help_id(field: &AdminScalarFieldView) -> Option<String> {
    field
        .help
        .as_ref()
        .map(|_| format!("setting-help-{}", field.form_name()))
}

impl AdminScalarFieldView {
    /// The name the setting's control is sent under.
    pub fn form_name(&self) -> &str {
        if self.name.is_empty() {
            &self.key
        } else {
            &self.name
        }
    }
}

fn scalar_input(field: &AdminScalarFieldView) -> Markup {
    let name = field.form_name();
    let id = field_id(name);
    let help = help_id(field);
    // A refused value shown again carries the saved one, for the page's
    // script to tell it's still unsaved.
    let saved = field.saved_value.as_deref();
    match &field.kind {
        SettingKindView::Integer { min, max } => html! {
            input type="number" name=(name) value=(field.value) min=[min] max=[max] step="1" id=(id) aria-describedby=[help] data-saved=[saved];
        },
        SettingKindView::Bool => html! {
            select name=(name) id=(id) aria-describedby=[help] data-saved=[saved] {
                option value="true" selected[field.value == "true"] { "true" }
                option value="false" selected[field.value == "false"] { "false" }
            }
        },
        SettingKindView::Choice { choices } => html! {
            select name=(name) id=(id) aria-describedby=[help] data-saved=[saved] {
                @for choice in choices {
                    option value=(choice) selected[&field.value == choice] { (choice) }
                }
            }
        },
        // Every choice ticked is sent under the one name; an empty one
        // first, so ticking none still says so.
        SettingKindView::ChoiceList { choices } => {
            let chosen: Vec<&str> = field.value.split(',').map(str::trim).collect();
            let was: Option<Vec<&str>> =
                saved.map(|saved| saved.split(',').map(str::trim).collect());
            html! {
                input type="hidden" name=(name) value="";
                @for choice in choices {
                    label class="inline" {
                        input type="checkbox" name=(name) value=(choice) checked[chosen.contains(&choice.as_str())]
                            data-saved=[was.as_ref().map(|was| if was.contains(&choice.as_str()) { "on" } else { "off" })];
                        " " (choice)
                    }
                }
            }
        }
        SettingKindView::Url => {
            html! { input type="url" name=(name) value=(field.value) id=(id) aria-describedby=[help] data-saved=[saved]; }
        }
        SettingKindView::Json => {
            html! { textarea name=(name) rows="4" id=(id) aria-describedby=[help] data-saved=[saved] { (field.value) } }
        }
        SettingKindView::TimeLimit { now, until_label } => {
            let until: u64 = field.value.trim().parse().unwrap_or(0);
            let on = until > *now;
            html! {
                select name=(name) id=(id) aria-describedby=[help] data-saved=[saved] {
                    @if on {
                        option value=(until) selected { "On until " (until_label) }
                    }
                    // Off, a time already past posts as it is, so it isn't
                    // read as a change.
                    option value=(if on || until == 0 { 0 } else { until }) selected[!on] { "Off" }
                    @for (hours, label) in [(1, "On for 1 hour"), (4, "On for 4 hours"), (24, "On for 24 hours")] {
                        option value=(now + hours * 3600) { (label) }
                    }
                }
            }
        }
        _ => {
            html! { input type="text" name=(name) value=(field.value) id=(id) aria-describedby=[help] data-saved=[saved]; }
        }
    }
}

/// A setting's name with the chip saying where its value comes from, then
/// what it's for, then its control, then anything more about it. A list of
/// choices is a group of checkboxes, named by a legend rather than a label.
fn scalar_field(field: &AdminScalarFieldView) -> Markup {
    let help = html! {
        @if let (Some(help), Some(id)) = (&field.help, help_id(field)) {
            span class="field-help" id=(id) { (help) }
        }
    };
    let locked = html! {
        @if let Some(reason) = &field.locked {
            span class="field-help locked-reason" { (reason) }
        }
    };
    html! {
        @if matches!(field.kind, SettingKindView::ChoiceList { .. }) && field.locked.is_none() {
            fieldset class="setting-field" aria-describedby=[help_id(field)] {
                legend class="setting-label-row" {
                    span class="setting-label" { (field.label) }
                    (source_chip(field.source))
                    span class="changed-mark" { "changed" }
                }
                (help)
                div class="setting-choices" { (scalar_input(field)) }
                (field_status(field))
            }
        } @else {
            div class="setting-field" {
                div class="setting-label-row" {
                    label class="setting-label" for=(field_id(field.form_name())) { (field.label) }
                    (source_chip(field.source))
                    span class="changed-mark" { "changed" }
                }
                (help)
                // A secret comes from the environment only, and is never
                // shown: always locked.
                @if field.locked.is_some() || field.kind == SettingKindView::Secret {
                    (locked_input(field))
                } @else {
                    (scalar_input(field))
                }
                (locked)
                (field_status(field))
            }
        }
    }
}

/// A setting the page can't change (given on the command line or in the
/// environment, or kept in a file it can't write): its value in a disabled
/// box with a padlock inside, and no `name`, so it is never sent with the
/// form. A secret shows as dots, never its value.
fn locked_input(field: &AdminScalarFieldView) -> Markup {
    let secret = matches!(field.kind, SettingKindView::Secret);
    html! {
        span class="locked-input" {
            @if secret {
                input type="password" value="locked" id=(field_id(field.form_name()))
                    aria-describedby=[help_id(field)] disabled;
            } @else {
                input type="text" value=(field.value) id=(field_id(field.form_name()))
                    aria-describedby=[help_id(field)] disabled;
            }
            svg class="lock-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false" {
                path d="M5 7V5a3 3 0 0 1 6 0v2" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" {}
                rect x="3" y="7" width="10" height="7" rx="1.5" fill="currentColor" {}
            }
        }
    }
}

/// Under a setting's control: an example, whether it waits for a restart,
/// and anything wrong with it.
fn field_status(field: &AdminScalarFieldView) -> Markup {
    html! {
        @if let Some(example) = &field.example {
            span class="field-help" { "Example: " code { (example) } }
        }
        @if field.restart_only {
            span class="field-help setting-source" { "Applies after a restart." }
        }
        @if field.pending_restart {
            span class="setting-pending" { "Saved - restart needed for it to take effect." }
        }
        @if let Some(problem) = &field.problem {
            span class="setting-problem" { (problem) }
        }
    }
}

fn notices(items: &[Notice]) -> Markup {
    html! {
        @for notice in items {
            @match notice {
                Notice::Success(text) => p class="success" role="status" { (text) },
                Notice::Error(text) => p class="error" role="alert" { (text) },
            }
        }
    }
}

const ENABLED_BACKENDS: &str = "key_custody.enabled_backends";

/// The key custody backends the engine offers, and whether each is turned
/// on. Empty from an engine that doesn't say.
fn custody_backends(fields: &[AdminScalarFieldView]) -> Vec<(String, bool)> {
    let Some(field) = fields.iter().find(|f| f.key == ENABLED_BACKENDS) else {
        return Vec::new();
    };
    let SettingKindView::ChoiceList { choices } = &field.kind else {
        return Vec::new();
    };
    let enabled: Vec<&str> = field.value.split(',').map(str::trim).collect();
    choices
        .iter()
        .map(|choice| (choice.clone(), enabled.contains(&choice.as_str())))
        .collect()
}

/// The page-wide banners: what is still true after a save (a network
/// stores use left without a node, an engine that doesn't answer at its
/// new URL), above the tab bar on every tab. `oob` marks it for fixi's
/// glue to put in place of the page's own copy when it comes back with a
/// swapped panel.
pub fn banners(data: &AdminSettingsViewModel, oob: bool) -> Markup {
    html! {
        div id="settings-banners" class="save-banners" data-fx-oob[oob] {
            (notices(&data.notices))
        }
    }
}

/// The toast a save or a reload leaves, in the corner of the window. A new
/// one comes with every save, so saving twice shows twice. Without
/// JavaScript it fades by itself (CSS); with it, one saying something
/// wasn't saved stays until it's closed.
pub fn toasts(data: &AdminSettingsViewModel, oob: bool) -> Markup {
    html! {
        div id="settings-toasts" class="toasts" data-fx-oob[oob] {
            @if let Some(toast) = &data.toast {
                @let (class, icon, role) = match toast.kind {
                    ToastKind::Success => ("toast toast-success", "\u{2713}", "status"),
                    ToastKind::Warning => ("toast toast-warning", "!", "status"),
                    ToastKind::Error => ("toast toast-error", "!", "alert"),
                    ToastKind::Neutral => ("toast toast-neutral", "\u{2022}", "status"),
                };
                div class=(class) role=(role) data-toast {
                    span class="toast-icon" aria-hidden="true" { (icon) }
                    div class="toast-text" {
                        strong { (toast.title) }
                        @for line in &toast.lines { span class="toast-line" { (line) } }
                        @if let Some(group) = toast.show {
                            a class="toast-show" href=(format!("#{}", group.card_id())) data-show-card=(group) { "Show" }
                        }
                    }
                    button type="button" class="toast-close js-only" aria-label="Dismiss" data-toast-close { "\u{00D7}" }
                }
            }
        }
    }
}

/// Each process's options file: where it is, whether the page can write
/// it, and a button that reads it again, for an edit made by hand. Not part
/// of what fixi swaps: a save doesn't move the file.
fn options_file_bars(data: &AdminSettingsViewModel) -> Markup {
    html! {
        @if !data.options_files.is_empty() {
            div class="options-files" {
                @for file in &data.options_files {
                    @let process = match file.owner {
                        SettingOwner::Monokulo => "monokulo",
                        SettingOwner::Engine => "the engine",
                    };
                    form class="options-file" method="post" action="/dashboard/admin/settings/reload" {
                        input type="hidden" name="tab" value=(data.tab.id());
                        input type="hidden" name="owner" value=(match file.owner {
                            SettingOwner::Monokulo => "monokulo",
                            SettingOwner::Engine => "engine",
                        });
                        div class="options-file-where" {
                            span class="options-file-path" { (icon(file_icon_paths())) code { (file.path) } }
                            span class="options-file-state" {
                                @if !file.writable {
                                    "The options file of " (process) ". It can't be written by " (process)
                                    ", so its settings are locked here: edit the file, then reload it."
                                } @else if !file.exists {
                                    "The options file of " (process) ". Not created yet: saving a setting here creates it."
                                } @else {
                                    "The options file of " (process) ". Saving here writes it; after editing it by hand, reload it."
                                }
                            }
                        }
                        button type="submit" {
                            (icon(html! {
                                path d="M20 11a8 8 0 1 0-2.3 5.7" {}
                                path d="M20 4v7h-7" {}
                            }))
                            "Reload options file"
                        }
                    }
                }
            }
        }
    }
}

/// The engine answered, so its settings are on hand.
fn engine_available(data: &AdminSettingsViewModel) -> bool {
    data.engine_reachable
}

/// Where the engine's settings would be, while it can't be reached
/// (nicer_admin_screen.md T6).
fn engine_unavailable(data: &AdminSettingsViewModel) -> Markup {
    html! {
        p class="error engine-unavailable" role="alert" {
            "Could not reach the configured engine: "
            @if let Some(engine_error) = &data.engine_error { (engine_error) }
        }
    }
}
/// The cards a tab shows, in order: its fixed ones, with the Monero nodes
/// tab's networks first and each key custody backend's after Backends,
/// from what the engine reports (and any backend only this site's own
/// settings name, while the engine can't say).
fn tab_groups(data: &AdminSettingsViewModel, tab: SettingsTab) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    if tab == SettingsTab::Nodes {
        groups.extend(
            data.engine_networks
                .iter()
                .filter_map(|network| shared::network::parse_network(&network.network).ok())
                .map(Group::Network),
        );
    }
    for fixed in tab.fixed_groups() {
        groups.push(*fixed);
        if *fixed == Group::CustodyBackends {
            let mut backends: Vec<Group> = custody_backends(&data.engine_fields)
                .into_iter()
                .filter_map(|(backend, _)| CustodyBackend::parse(&backend).map(Group::Custody))
                .collect();
            for field in &data.monokulo_fields {
                let group = Group::of(&field.key, SettingOwner::Monokulo);
                if matches!(group, Group::Custody(_)) && !backends.contains(&group) {
                    backends.push(group);
                }
            }
            groups.extend(backends);
        }
    }
    groups
}

/// A card's settings, both owners', in the order they are shown.
fn group_fields(data: &AdminSettingsViewModel, group: Group) -> Vec<&AdminScalarFieldView> {
    let mut fields: Vec<&AdminScalarFieldView> = data
        .engine_fields
        .iter()
        .filter(|f| Group::of(&f.key, SettingOwner::Engine) == group)
        .chain(
            data.monokulo_fields
                .iter()
                .filter(|f| Group::of(&f.key, SettingOwner::Monokulo) == group),
        )
        .collect();
    fields.sort_by_key(|f| field_rank(&f.key));
    fields
}

/// A tab's settings, every card's.
fn tab_fields(data: &AdminSettingsViewModel, tab: SettingsTab) -> Vec<&AdminScalarFieldView> {
    data.engine_fields
        .iter()
        .filter(|f| Group::of(&f.key, SettingOwner::Engine).tab() == tab)
        .chain(
            data.monokulo_fields
                .iter()
                .filter(|f| Group::of(&f.key, SettingOwner::Monokulo).tab() == tab),
        )
        .collect()
}

/// Whether a tab has anything to show: always, except Other, which only
/// shows while a setting nobody placed is in it.
fn tab_shown(data: &AdminSettingsViewModel, tab: SettingsTab) -> bool {
    tab != SettingsTab::Other || !tab_fields(data, tab).is_empty()
}

/// Whether a tab's label carries the marker (T5): a network stores use has
/// no node that answers (Monero nodes), or a saved setting on it waits for
/// a restart.
fn needs_attention(data: &AdminSettingsViewModel, tab: SettingsTab) -> bool {
    let unserved = tab == SettingsTab::Nodes
        && (!data.unreachable_networks.is_empty()
            || data
                .engine_networks
                .iter()
                .any(|n| n.tenant_count > 0 && n.rows.is_empty()));
    unserved || tab_fields(data, tab).iter().any(|f| f.pending_restart)
}

/// The tab bar: plain links, each its own page, so it works without
/// JavaScript; with fixi they swap just the panel and push the URL. Marked
/// tabs say so in words as well as with the dot.
pub fn tab_bar(data: &AdminSettingsViewModel, oob: bool) -> Markup {
    html! {
        nav id="settings-tabs" class="tab-bar" aria-label="Settings sections" data-fx-oob[oob] {
            @for tab in SettingsTab::ALL.into_iter().filter(|tab| tab_shown(data, *tab)) {
                @let href = tab.href();
                a href=(href) fx-action=(href) fx-target="#settings-panel" fx-push-url aria-current=[(tab == data.tab).then_some("page")] {
                    (tab.label())
                    @if needs_attention(data, tab) {
                        span class="tab-marker" aria-hidden="true" { "\u{25CF}" }
                        span class="visually-hidden" { " (needs attention)" }
                    }
                }
            }
        }
    }
}

/// `1234567` as `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A node's status from the engine's `/status`, in words, and whether it
/// is a problem (it doesn't answer, or is on another network).
fn node_status_words(status: &NodeStatusView) -> (String, bool) {
    let problem = status.wrong_network.is_some() || status.height.is_none();
    let mut parts: Vec<String> = Vec::new();
    if let Some(network) = &status.wrong_network {
        parts.push(format!("Wrong network: this node is on {network}"));
    } else if let Some(height) = status.height {
        match status.behind.filter(|behind| *behind > 0) {
            Some(behind) => parts.push(format!(
                "Reachable, height {} ({} behind)",
                thousands(height),
                thousands(behind)
            )),
            None => parts.push(format!("Reachable, height {}", thousands(height))),
        }
    } else if let Some(error) = &status.error {
        parts.push(format!("Not reachable: {error}"));
    }
    if status.in_use {
        parts.push("In use".to_string());
    }
    if status.resting {
        parts.push("Resting after failures".to_string());
    }
    (parts.join(". "), problem)
}

fn node_status(status: &NodeStatusView) -> Markup {
    let (words, problem) = node_status_words(status);
    html! {
        @if !words.is_empty() {
            p class=(if problem { "node-status is-problem" } else { "node-status" }) { (words) "." }
        }
        @if let Some(link) = &status.link { (super::scaling::link_figures(link)) }
    }
}

/// One node row, closed to a line until it's opened: a handle to drag it
/// by (with JavaScript), its place (Primary, Fallback N), its address and
/// its status. Open, its address, TLS and self-signed boxes, its status
/// and its buttons. `position` is its place in the list (`None` for the
/// blank "Add a node" row, which has no status or buttons); `index`
/// numbers its fields. The buttons are submit buttons of the tab's form:
/// without JavaScript pressing one applies it to the submitted rows and
/// saves; with it, Remove takes the row out (saved with the bar) and the
/// handle replaces Move up and Move down.
fn node_row(
    network: &AdminNetworkFieldView,
    index: usize,
    row: &NodeRowView,
    position: Option<(usize, usize)>,
) -> Markup {
    let n = &network.network;
    let name = |field: &str| format!("node_{n}_{index}_{field}");
    let id = |field: &str| format!("node-{n}-{index}-{field}");
    let place = match position {
        None => "Add a node".to_string(),
        Some((0, _)) => "Primary".to_string(),
        Some((at, _)) => format!("Fallback {at}"),
    };
    let example = network
        .example_address
        .as_deref()
        .unwrap_or("node.example.com:18081");
    let error_id = row.row.error.as_ref().map(|_| id("error"));
    let described = match &error_id {
        Some(error) => format!("{} {error}", id("address-help")),
        None => id("address-help"),
    };
    let summary_status = row.status.as_ref().map(node_status_words);
    html! {
        details class="node-row" data-node-row=(index) data-node-add[position.is_none()]
            data-node-saved=[row.saved_index] open[row.row.error.is_some()] {
            summary class="node-row-summary" {
                @if position.is_some() {
                    span class="node-handle js-only" role="button" tabindex="0" title="Drag to reorder"
                        aria-label=(format!("Reorder {place}: press the up or down arrow")) data-node-handle { "\u{283F}" }
                } @else {
                    span class="node-add-mark" aria-hidden="true" { "+" }
                }
                span class="node-row-name" data-node-place { (place) }
                @if position.is_some() {
                    code class="node-row-address" { (row.row.address) }
                }
                span class="node-row-mark" data-node-mark {}
                @if let Some((words, problem)) = &summary_status {
                    span class=(if *problem { "node-row-status is-problem" } else { "node-row-status" }) { (words) }
                }
            }
            div class="node-row-body" {
                div class="setting-field" {
                    div class="setting-label-row" { label class="setting-label" for=(id("address")) { "Address" } }
                    span class="field-help" id=(id("address-help")) {
                        "The node's host and port, like " code { (example) } ". "
                        code { "http://" } " or " code { "https://" } " in front is fine (" code { "https://" } " also ticks Use TLS); "
                        "an IPv6 address goes in brackets, like " code { "[::1]:18081" } "."
                    }
                    input type="text" name=(name("address")) id=(id("address")) value=(row.row.address) aria-describedby=(described)
                        aria-invalid=[row.row.error.as_ref().map(|_| "true")] autocomplete="off" spellcheck="false" inputmode="url";
                    @if let (Some(error), Some(error_id)) = (&row.row.error, &error_id) {
                        span class="setting-problem" id=(error_id) { (error) }
                    }
                }
                div class="setting-field node-tls" {
                    div class="setting-label-row" { label class="setting-label" for=(id("ssl")) { "Use TLS" } }
                    span class="field-help" id=(id("ssl-help")) { "Connect with TLS (https). Off by default; most nodes on port 18081 or 18089 don't use it." }
                    input type="checkbox" name=(name("ssl")) id=(id("ssl")) value="on" checked[row.row.ssl] aria-describedby=(id("ssl-help")) data-node-tls;
                }
                div class="setting-field node-self-signed" data-node-self-signed {
                    div class="setting-label-row" { label class="setting-label" for=(id("self_signed")) { "Accept a self-signed certificate" } }
                    span class="field-help" id=(id("self_signed-help")) {
                        "Many community nodes use a self-signed TLS certificate; tick this to accept one. Only used with TLS."
                    }
                    input type="checkbox" name=(name("self_signed")) id=(id("self_signed")) value="on" checked[row.row.self_signed] aria-describedby=(id("self_signed-help"));
                }
                div class="setting-field node-zmq" {
                    div class="setting-label-row" { label class="setting-label" for=(id("zmq_pub")) { "Announcements (ZMQ)" } }
                    span class="field-help" id=(id("zmq_pub-help")) {
                        "Optional, for your own node: the address it was started with as " code { "--zmq-pub" } ", like "
                        code { "tcp://127.0.0.1:18083" } ". Payments are then seen the moment the node sees them, instead of at the next check. "
                        "Needs an engine built with ZMQ support; leave empty otherwise."
                    }
                    input type="text" name=(name("zmq_pub")) id=(id("zmq_pub")) value=(row.row.zmq_pub) aria-describedby=(id("zmq_pub-help"))
                        autocomplete="off" spellcheck="false" inputmode="url";
                }
                @if let Some(status) = &row.status { (node_status(status)) }
                @if let Some((at, count)) = position {
                    div class="node-row-actions" {
                        @if at > 0 {
                            button type="submit" class="no-js-only" name="node_action" value=(format!("up:{n}:{index}")) { "Move up" }
                        }
                        @if at + 1 < count {
                            button type="submit" class="no-js-only" name="node_action" value=(format!("down:{n}:{index}")) { "Move down" }
                        }
                        button type="submit" name="node_action" value=(format!("remove:{n}:{index}")) data-node-remove { "Remove" }
                    }
                }
            }
        }
    }
}

/// `stagenet` as `Stagenet`, for a heading.
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The node a network's scan reads from, with its measured rate.
fn active_node(network: &AdminNetworkFieldView) -> Option<super::scaling::ActiveNode<'_>> {
    network.rows.iter().find_map(|row| {
        let status = row.status.as_ref().filter(|status| status.in_use)?;
        Some(super::scaling::ActiveNode {
            label: &row.label,
            rate_bytes_per_sec: status
                .link
                .as_ref()
                .filter(|link| link.link.measured)
                .map(|link| link.link.rate_bytes_per_sec),
        })
    })
}

/// Who owns a card's settings, for the chip beside its heading.
fn owner_label(fields: &[&AdminScalarFieldView], data: &AdminSettingsViewModel) -> &'static str {
    let engine = fields
        .iter()
        .any(|f| data.engine_fields.iter().any(|e| std::ptr::eq(e, *f)));
    let monokulo = fields
        .iter()
        .any(|f| data.monokulo_fields.iter().any(|m| std::ptr::eq(m, *f)));
    match (engine, monokulo) {
        (true, true) => "Engine and monokulo",
        (false, true) => "Monokulo",
        _ => "Engine",
    }
}

/// A card's heading row: its name, whose settings these are, anything
/// more (a network's stores), how the last save went for it, and (with
/// JavaScript) its unsaved changes and a Discard button for them.
fn card_header(
    data: &AdminSettingsViewModel,
    group: Group,
    owner: &str,
    meta: Option<String>,
    fields: &[&AdminScalarFieldView],
    readonly: bool,
) -> Markup {
    let failed = data.failure(group).is_some();
    let saved_at = data.saved_at(group);
    let restart = fields.iter().any(|f| f.pending_restart);
    html! {
        header class="card-head" {
            h3 id=(format!("{}-title", group.card_id())) { (group.title()) }
            span class="owner-chip" { (owner) }
            @if let Some(meta) = meta { span class="card-meta" { (meta) } }
            span class="card-state" data-card-state {
                @if failed {
                    span class="badge badge-error" { "Not saved" }
                } @else if restart {
                    span class="badge badge-warning" { "Restart needed" }
                }
            }
            span class="card-spacer" {}
            @if readonly {
                span class="card-meta" { "Can't be changed here" }
            } @else {
                @if let (Some(at), false) = (saved_at, failed) {
                    span class="card-meta card-saved" data-card-saved { "Saved " (at) }
                }
                button type="button" class="card-discard js-only" data-card-discard hidden { "Discard" }
            }
        }
    }
}

/// A card's settings, with any subheadings, and this site's own key entry
/// settings for a key custody backend under their own heading.
fn card_fields(
    group: Group,
    fields: &[&AdminScalarFieldView],
    data: &AdminSettingsViewModel,
) -> Markup {
    let site_heading = matches!(group, Group::Custody(_));
    let first_site = fields
        .iter()
        .position(|f| data.monokulo_fields.iter().any(|m| std::ptr::eq(m, *f)));
    html! {
        @for (i, field) in fields.iter().enumerate() {
            @if site_heading && first_site == Some(i) {
                h4 { "This site's key entry" }
                p class="hint" {
                    "What this site's forms check before they encrypt a merchant's keys. They must match the engine's settings above: "
                    "they are checked against the engine when saved, and the status page shows an alert if they ever differ."
                }
            }
            @if let Some(heading) = field_subheading(&field.key) { h4 { (heading) } }
            (scalar_field(field))
        }
    }
}

/// One card of scalar settings. A key custody backend's card shows only
/// while the backend is turned on (at once with JavaScript, after saving
/// without).
fn settings_card(data: &AdminSettingsViewModel, group: Group) -> Markup {
    let fields = group_fields(data, group);
    if fields.is_empty() {
        return html! {};
    }
    let readonly = fields
        .iter()
        .all(|f| f.locked.is_some() || f.kind == SettingKindView::Secret);
    let backend = match group {
        Group::Custody(backend) => Some(backend.name()),
        _ => None,
    };
    let hidden = backend.is_some_and(|backend| {
        custody_backends(&data.engine_fields)
            .iter()
            .any(|(listed, on)| listed == backend && !on)
    });
    let failure = data.failure(group);
    html! {
        section id=(group.card_id()) class={ "settings-card" @if failure.is_some() { " is-failed" } }
            data-card=(group) data-custody-backend=[backend] hidden[hidden]
            aria-labelledby=(format!("{}-title", group.card_id())) {
            (card_header(data, group, owner_label(&fields, data), None, &fields, readonly))
            div class="card-body" {
                @if let Some(failure) = failure {
                    p class="error" role="alert" { (failure.message) }
                }
                @if let Some(hint) = group.hint() { p class="hint" { (hint) } }
                (card_fields(group, &fields, data))
            }
        }
    }
}

/// One network's card: its nodes in order, each closed to a line, then a
/// blank "Add a node" row (adding needs no JavaScript: fill it in and
/// save), and its proof-of-work switch. A network with no nodes that no
/// store uses starts closed, as "Add a node for <network>".
fn network_card(
    data: &AdminSettingsViewModel,
    network: &AdminNetworkFieldView,
    group: Group,
) -> Markup {
    let n = &network.network;
    let fields = group_fields(data, group);
    let count = network.rows.len();
    let failure = data.failure(group);
    let used_by = if network.tenant_count == 1 {
        "Used by 1 store".to_string()
    } else if network.tenant_count == 0 {
        "Not used by any store".to_string()
    } else {
        format!("Used by {} stores", network.tenant_count)
    };
    let rows = html! {
        @if let Some(error) = &network.error {
            p class="error" role="alert" { (error) }
        } @else if let Some(failure) = failure {
            p class="error" role="alert" { (failure.message) }
        }
        p class="hint" {
            "Fallbacks are tried in order when the one before fails. Open a node to change it"
            span class="js-only" { ", or drag it by its handle to reorder it" } "."
        }
        div class="node-rows" data-node-rows=(n) {
            @for (at, row) in network.rows.iter().enumerate() {
                (node_row(network, at, row, Some((at, count))))
            }
            (node_row(network, count, &NodeRowView { row: crate::admin_nodes::NodeRow { self_signed: true, ..Default::default() }, ..Default::default() }, None))
        }
        button type="button" class="js-only node-add-another" data-node-add-another=(n) { "Add another" }
        // Whether this network's blocks' proof of work is checked
        // (`proof_of_work.<network>`).
        @for field in &fields { (scalar_field(field)) }
    };
    let open =
        count > 0 || network.tenant_count > 0 || network.error.is_some() || failure.is_some();
    html! {
        section id=(group.card_id()) class={ "settings-card node-network" @if failure.is_some() || network.error.is_some() { " is-failed" } }
            data-card=(group) data-network=(n) data-tenant-count=(network.tenant_count) data-saved-count=(network.saved_count)
            aria-labelledby=(format!("{}-title", group.card_id())) {
            (card_header(data, group, "Engine", Some(used_by), &fields, false))
            div class="card-body" {
                @if open {
                    @if let Some(scaling) = &network.scaling {
                        (super::scaling::scanning_panel(n, scaling, active_node(network)))
                    }
                    (rows)
                } @else {
                    details class="node-network-closed" {
                        summary { "Add a node for " (n) }
                        (rows)
                    }
                }
            }
        }
    }
}

/// One tab's cards (`tab_groups`). Where the engine's cards would be while
/// it can't be reached, its message stands in, once.
fn tab_cards(data: &AdminSettingsViewModel, tab: SettingsTab) -> Markup {
    let groups = tab_groups(data, tab);
    let engine_down = !engine_available(data);
    let first_engine = groups.iter().position(|group| group.engine_only());
    let network_of = |group: Group| match group {
        Group::Network(n) => data
            .engine_networks
            .iter()
            .find(|network| network.network == shared::network::network_str(n)),
        _ => None,
    };
    html! {
        @for (i, group) in groups.iter().copied().enumerate() {
            @if engine_down && group.engine_only() {
                @if first_engine == Some(i) { (engine_unavailable(data)) }
            } @else if let Some(network) = network_of(group) {
                (network_card(data, network, group))
            } @else {
                (settings_card(data, group))
            }
        }
    }
}

/// The bar along the bottom of the window, the full width like the site's
/// header, that saves the tab: the page's one Save. With JavaScript it shows once something on the tab changes and
/// names the cards changed; without, it's always there. After a save that
/// refused a card it's red, says why, and links to the card.
fn save_bar(data: &AdminSettingsViewModel) -> Markup {
    let outcome = data.outcome.as_ref();
    let failure = outcome.and_then(|outcome| outcome.failures().first());
    let partly = matches!(outcome, Some(SaveOutcome::PartlySaved { .. }));
    html! {
        div id="save-bar" class={ "save-bar" @if failure.is_some() { " is-failed" } } role="region" aria-label="Save changes" data-save-bar {
          div class="wrap save-bar-inner" {
            p class="save-bar-message" data-save-bar-message tabindex="-1" data-fx-focus[data.answers_save] {
                @if let Some(failure) = failure {
                    strong { @if partly { "Changes partly saved." } @else { "Nothing saved." } } " " (failure.message)
                    @if let Some(group) = failure.group {
                        " " a href=(format!("#{}", group.card_id())) data-show-card=(group) { "Show" }
                    }
                } @else {
                    "Saving writes the changes on this tab to the options file and applies them."
                }
            }
            div class="save-bar-actions" {
                a class="btn" href=(data.tab.href()) data-discard-all { "Discard changes" }
                button type="submit" class="btn-primary" data-save { "Save" }
            }
          }
        }
    }
}

/// The open tab: its heading, its one form, its cards and the save bar.
/// What fixi swaps, for a tab link or a save. `focus` marks the heading for
/// the glue to focus after a tab switch, so keyboard and screen reader
/// users land on the new tab's content.
pub fn settings_panel(data: &AdminSettingsViewModel, focus: bool) -> Markup {
    let tab = data.tab;
    html! {
        section id="settings-panel" aria-labelledby="settings-panel-title" data-tab=(tab.id()) data-tab-label=(tab.label()) {
            h2 id="settings-panel-title" tabindex="-1" data-fx-focus[focus] { (tab.label()) }
            p class="hint" { "Saving writes the options file and applies the change straight away. A setting given on the command line or in the environment is locked here: change it where it is given." }
            @if tab == SettingsTab::Abuse {
                p class="hint" {
                    "How this instance tells visitors apart and slows down anyone sending too many requests. A visitor "
                    "past the soft limit is asked to pass a short check (automatic with JavaScript, a 10-second wait "
                    "without); past the hard limit they're refused until the minute is up. Signed-in merchants and "
                    "plugins using their store's secret key are never checked."
                }
            }
            @if tab == SettingsTab::Nodes {
                p class="hint" {
                    "The Monero nodes the engine reads each network's chain from: a primary, and fallbacks tried when it fails. "
                    "A network with no nodes isn't used. A node that doesn't answer is still saved; one on another network is refused."
                }
            }
            @if tab.engine_only() && !engine_available(data) {
                (engine_unavailable(data))
            } @else {
                @if let (SettingsTab::Nodes, Some(resources)) = (tab, &data.resources) {
                    (super::scaling::resources_panel(resources, &tab.href()))
                }
                form method="post" action="/dashboard/admin/settings" id="settings-form"
                    fx-action="/dashboard/admin/settings" fx-method="POST" fx-target="#settings-panel" {
                    input type="hidden" name="tab" value=(tab.id());
                    @if tab == SettingsTab::Nodes {
                        // Enter in a text box presses a form's first submit
                        // button; here that would be a row's Move or Remove.
                        // This one, first and out of sight, is Save.
                        button type="submit" class="visually-hidden" tabindex="-1" aria-hidden="true" { "Save" }
                    }
                    (tab_cards(data, tab))
                    (save_bar(data))
                }
            }
        }
    }
}

/// What fixi gets back for a tab link or a save: the panel, with the
/// banners and the tab bar out of band (a save can change both: a marker
/// comes or goes, a banner appears), and a save's toast. A tab link brings
/// no toast, so the last save's stays until it fades or is closed.
pub fn settings_fragment(data: &AdminSettingsViewModel, focus_heading: bool) -> Markup {
    html! {
        (settings_panel(data, focus_heading))
        (banners(data, true))
        @if data.toast.is_some() { (toasts(data, true)) }
        (tab_bar(data, true))
    }
}

pub fn admin_settings_page(chrome: &PageChrome, data: &AdminSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Admin settings" }
            (banners(data, false))
            (options_file_bars(data))
            (tab_bar(data, false))
            (settings_panel(data, false))
            (toasts(data, false))
            script { (maud::PreEscaped(include_str!("../../static/admin-settings.js"))) }
        }
    };
    layout(
        chrome,
        &format!("{} - Admin settings - Monokulo", data.tab.label()),
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/admin/settings")
    }

    /// Every setting both registries have today, with the tab and the card
    /// the page shows it in (nicer_admin_screen.md section 2). A setting
    /// added later fails the registry cross-checks below until it is given
    /// a place here.
    const PLACEMENTS: &[(&str, SettingOwner, SettingsTab, &str)] = {
        use SettingOwner::{Engine as E, Monokulo as M};
        use SettingsTab::*;
        &[
            ("signup.mode", M, General, "signup"),
            ("public_url", M, General, "public-address"),
            (
                "exchange_rate.coingecko_enabled",
                M,
                Payments,
                "exchange-rates",
            ),
            (
                "exchange_rate.coingecko_base_url",
                M,
                Payments,
                "exchange-rates",
            ),
            (
                "exchange_rate.coinmarketcap_enabled",
                M,
                Payments,
                "exchange-rates",
            ),
            (
                "exchange_rate.coinmarketcap_base_url",
                M,
                Payments,
                "exchange-rates",
            ),
            (
                "exchange_rate.haveno_enabled",
                M,
                Payments,
                "exchange-rates",
            ),
            (
                "exchange_rate.haveno_base_url",
                M,
                Payments,
                "exchange-rates",
            ),
            ("exchange_rate.cache_seconds", M, Payments, "exchange-rates"),
            ("engine.mode", M, General, "engine"),
            ("engine.url", M, General, "engine"),
            ("engine.token", M, General, "engine"),
            ("http_cache.max_mb", M, Server, "server-monokulo"),
            ("database.read_connections", M, Server, "server-monokulo"),
            ("database.path", M, Server, "server-monokulo"),
            ("server.bind", M, Server, "server-monokulo"),
            ("crypto.encryption_key", M, Server, "server-monokulo"),
            ("logging.format", M, Logging, "logging-monokulo"),
            ("abuse.soft_per_min", M, Abuse, "abuse-limits"),
            ("abuse.hard_per_min", M, Abuse, "abuse-limits"),
            ("abuse.signed_in_per_min", M, Abuse, "abuse-limits"),
            ("abuse.client_logs_per_min", M, Abuse, "abuse-limits"),
            ("abuse.challenge_bits", M, Abuse, "abuse-challenge"),
            ("abuse.under_attack", M, Abuse, "abuse-challenge"),
            ("abuse.trusted_proxies", M, Abuse, "abuse-visitors"),
            ("abuse.onion_listener", M, Abuse, "abuse-visitors"),
            ("abuse.stream_cap", M, Abuse, "abuse-limits"),
            ("rate_limit.per_store_key_per_min", M, Abuse, "abuse-limits"),
            ("logging.level", M, Logging, "logging-monokulo"),
            ("logging.dev_mode_until", M, Logging, "logging-monokulo"),
            ("logging.retention_days", M, Logging, "logging-monokulo"),
            ("logging.max_mb", M, Logging, "logging-monokulo"),
            ("logging.otlp_endpoint", M, Logging, "logging-monokulo"),
            ("logging.otlp_headers", M, Logging, "logging-monokulo"),
            ("monero_node.mainnet", E, Nodes, "network-mainnet"),
            ("monero_node.stagenet", E, Nodes, "network-stagenet"),
            ("monero_node.testnet", E, Nodes, "network-testnet"),
            ("monero_node.strict_tls", E, Nodes, "nodes-all"),
            ("proof_of_work.mainnet", E, Nodes, "network-mainnet"),
            ("proof_of_work.stagenet", E, Nodes, "network-stagenet"),
            ("proof_of_work.testnet", E, Nodes, "network-testnet"),
            (
                "key_custody.enabled_backends",
                E,
                Custody,
                "custody-backends",
            ),
            (
                "key_custody.default_backend",
                E,
                Custody,
                "custody-backends",
            ),
            ("key_custody.snp_product", E, Custody, "custody-snp"),
            ("key_custody.snp_trusted_id_key", E, Custody, "custody-snp"),
            ("key_custody.snp_min_guest_svn", E, Custody, "custody-snp"),
            ("key_custody.snp_min_tcb", E, Custody, "custody-snp"),
            ("key_custody.snp_handoff_url", E, Custody, "custody-snp"),
            ("key_custody.cli_download_url", M, Custody, "custody-cli"),
            ("key_custody.cli_source_url", M, Custody, "custody-cli"),
            ("key_custody.snp_entry_id_key", M, Custody, "custody-snp"),
            (
                "key_custody.snp_entry_min_guest_svn",
                M,
                Custody,
                "custody-snp",
            ),
            ("key_custody.snp_entry_min_tcb", M, Custody, "custody-snp"),
            ("key_custody.snp_entry_required", M, Custody, "custody-snp"),
            (
                "key_custody.snp_bundles_per_user",
                M,
                Custody,
                "custody-snp",
            ),
            (
                "key_custody.snp_bundles_per_user_per_min",
                M,
                Custody,
                "custody-snp",
            ),
            ("payment.confirmations_required", E, Payments, "orders"),
            ("payment.order_expiry_minutes", E, Payments, "orders"),
            (
                "payment.expired_order_grace_period_minutes",
                E,
                Payments,
                "orders",
            ),
            ("payment.reorg_check_depth", E, Payments, "chain"),
            ("payment.mempool_poll_interval_ms", E, Payments, "chain"),
            (
                "payment.scan_chunk_memory_budget_mb",
                E,
                Server,
                "server-engine",
            ),
            ("webhooks.allow_private_urls", E, Payments, "webhooks"),
            ("webhooks.delivery_timeout_ms", E, Payments, "webhooks"),
            ("webhooks.max_attempts", E, Payments, "webhooks"),
            ("server.bind", E, Server, "server-engine"),
            ("server.worker_threads", E, Server, "server-engine"),
            ("server.cpus", E, Server, "server-engine"),
            ("server.nice", E, Server, "server-engine"),
            ("server.max_body_bytes", E, Server, "server-engine"),
            (
                "server.rate_limit_per_token_per_min",
                E,
                Server,
                "server-engine",
            ),
            ("database.read_connections", E, Server, "server-engine"),
            ("database.path", E, Server, "server-engine"),
            ("server.token", E, Server, "server-engine"),
            ("logging.format", E, Logging, "logging-engine"),
            ("logging.level", E, Logging, "logging-engine"),
            ("logging.dev_mode_until", E, Logging, "logging-engine"),
            ("logging.retention_days", E, Logging, "logging-engine"),
            ("logging.max_mb", E, Logging, "logging-engine"),
            ("logging.otlp_endpoint", E, Logging, "logging-engine"),
            ("logging.otlp_headers", E, Logging, "logging-engine"),
        ]
    };

    #[test]
    fn every_setting_known_today_has_a_named_tab() {
        for (key, owner, tab, group) in PLACEMENTS {
            let placed = Group::of(key, *owner);
            assert_eq!(
                (placed.tab(), placed.to_string().as_str()),
                (*tab, *group),
                "{key} ({owner:?})"
            );
            assert_ne!(*tab, SettingsTab::Other, "{key}");
            // The card is one the tab actually shows.
            assert!(
                tab_groups(&full_view(*tab), *tab).contains(&placed),
                "{key} is placed in a card {tab:?} doesn't have"
            );
        }
    }

    /// The list above is every setting of both registries, not a sample.
    #[test]
    fn the_tab_list_covers_both_registries() {
        let listed = |owner: SettingOwner| -> Vec<&str> {
            PLACEMENTS
                .iter()
                .filter(|(_, o, _, _)| *o == owner)
                .map(|(key, ..)| *key)
                .collect()
        };
        let mut monokulo: Vec<&str> = crate::settings::ALL.iter().map(|s| s.key()).collect();
        monokulo.sort_unstable();
        let mut monokulo_listed = listed(SettingOwner::Monokulo);
        monokulo_listed.sort_unstable();
        assert_eq!(
            monokulo_listed, monokulo,
            "every monokulo setting has a tab"
        );

        let mut engine: Vec<&str> = engine::engine_settings::ALL
            .iter()
            .map(|s| s.key())
            .collect();
        engine.sort_unstable();
        let mut engine_listed = listed(SettingOwner::Engine);
        engine_listed.sort_unstable();
        assert_eq!(engine_listed, engine, "every engine setting has a tab");
    }

    #[test]
    fn a_setting_the_map_does_not_know_goes_to_other() {
        for (key, owner) in [
            ("telemetry.sample_rate", SettingOwner::Engine),
            ("brand_new", SettingOwner::Engine),
            ("brand.new", SettingOwner::Monokulo),
            // A key custody backend this page doesn't know.
            ("key_custody.tpm_device", SettingOwner::Engine),
        ] {
            let group = Group::of(key, owner);
            assert_eq!(group, Group::Other(owner), "{key}");
            assert_eq!(group.tab(), SettingsTab::Other);
        }
        assert_eq!(
            Group::Other(SettingOwner::Engine).to_string(),
            "other-engine"
        );
    }

    #[test]
    fn a_tab_is_found_by_its_id_and_anything_else_is_general() {
        for tab in SettingsTab::ALL {
            assert_eq!(SettingsTab::from_id(Some(tab.id())), tab);
        }
        assert_eq!(SettingsTab::from_id(None), SettingsTab::General);
        assert_eq!(SettingsTab::from_id(Some("nope")), SettingsTab::General);
        assert_eq!(
            SettingsTab::Nodes.href(),
            "/dashboard/admin/settings?tab=nodes"
        );
        assert!(SettingsTab::Nodes.engine_only());
        assert!(
            !SettingsTab::Custody.engine_only()
                && !SettingsTab::Payments.engine_only()
                && !SettingsTab::Server.engine_only()
                && !SettingsTab::Logging.engine_only()
        );
    }

    #[test]
    fn setup_page_shows_the_echoed_email_and_an_error() {
        let data = SetupViewModel {
            error: Some("Passwords do not match.".to_string()),
            email: "owner@example.com".to_string(),
        };
        let html = setup_page(&chrome(), &data).into_string();
        assert!(html.contains("Set up your admin account"));
        assert!(html.contains("Passwords do not match."));
        assert!(html.contains(r#"value="owner@example.com""#));
    }

    #[test]
    fn request_invite_page_shows_the_form_before_submission_and_a_thank_you_after() {
        let form = RequestInviteViewModel {
            error: None,
            submitted: false,
        };
        let html = request_invite_page(&chrome(), &form).into_string();
        assert!(html.contains(r#"<form method="post" action="/request-invite">"#));

        let thanks = RequestInviteViewModel {
            error: None,
            submitted: true,
        };
        let html = request_invite_page(&chrome(), &thanks).into_string();
        assert!(html.to_lowercase().contains("thanks"));
        assert!(
            !html.contains("<form method=\"post\" action=\"/request-invite\""),
            "a submitted confirmation must not still show the request form"
        );
    }

    fn row(id: &str, email: &str) -> AdminInviteRequestRow {
        AdminInviteRequestRow {
            id: id.to_string(),
            email: email.to_string(),
            message: "let me in please".to_string(),
            created_at_display: maud::PreEscaped("just now".to_string()),
            mailto_href: Some(format!("mailto:{email}")),
            just_deleted: false,
        }
    }

    #[test]
    fn admin_invites_page_shows_no_pending_message_when_empty() {
        let data = AdminInvitesViewModel {
            error: None,
            success: None,
            just_deleted_row: None,
            rows: vec![],
            page: 1,
            total_pages: 1,
            has_previous: false,
            has_next: false,
            previous_page: 1,
            next_page: 1,
            created_link: None,
        };
        let html = admin_invites_page(&chrome(), &data).into_string();
        assert!(html.contains("No pending invite requests."));
        assert!(!html.contains("<table"));
    }

    #[test]
    fn admin_invites_page_lists_rows_with_a_mailto_link_and_a_delete_form_carrying_the_current_page(
    ) {
        let data = AdminInvitesViewModel {
            error: None,
            success: None,
            just_deleted_row: None,
            rows: vec![row("row-1", "hopeful@example.com")],
            page: 2,
            total_pages: 2,
            has_previous: true,
            has_next: false,
            previous_page: 1,
            next_page: 2,
            created_link: None,
        };
        let html = admin_invites_page(&chrome(), &data).into_string();
        assert!(html.contains("hopeful@example.com"));
        assert!(html.contains("mailto:hopeful@example.com"));
        assert!(html.contains("/dashboard/admin/invites/row-1/delete?page=2"));
        assert!(html.contains("Page 2 of 2"));
    }

    #[test]
    fn admin_invites_page_shows_the_struck_through_just_deleted_row_even_with_no_other_pending_rows(
    ) {
        let mut deleted = row("row-1", "gone@example.com");
        deleted.just_deleted = true;
        let data = AdminInvitesViewModel {
            error: None,
            success: None,
            just_deleted_row: Some(deleted),
            rows: vec![],
            page: 1,
            total_pages: 1,
            has_previous: false,
            has_next: false,
            previous_page: 1,
            next_page: 1,
            created_link: None,
        };
        let html = admin_invites_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"class="row-deleted""#));
        assert!(html.contains("gone@example.com"));
        assert!(!html.contains("No pending invite requests."));
    }

    /// A field as the handler builds it: an engine setting monokulo also
    /// has is sent as `engine:<key>`.
    fn field_for(key: &str, owner: SettingOwner) -> AdminScalarFieldView {
        let shared = owner == SettingOwner::Engine
            && PLACEMENTS
                .iter()
                .any(|(k, o, ..)| *k == key && *o == SettingOwner::Monokulo);
        AdminScalarFieldView {
            key: key.to_string(),
            name: if shared {
                format!("engine:{key}")
            } else {
                String::new()
            },
            label: key.replace(['.', '_'], " "),
            value: "1".to_string(),
            help: Some(format!("What {key} is for.")),
            kind: SettingKindView::Text,
            ..Default::default()
        }
    }

    /// The whole page's data, every setting known today, on `tab`.
    fn full_view(tab: SettingsTab) -> AdminSettingsViewModel {
        let fields = |owner: SettingOwner| -> Vec<AdminScalarFieldView> {
            PLACEMENTS
                .iter()
                .filter(|(key, o, ..)| *o == owner && !key.starts_with("monero_node."))
                .map(|(key, ..)| field_for(key, owner))
                .collect()
        };
        AdminSettingsViewModel {
            tab,
            monokulo_fields: fields(SettingOwner::Monokulo),
            engine_reachable: true,
            engine_fields: fields(SettingOwner::Engine),
            engine_networks: ["mainnet", "stagenet", "testnet"]
                .into_iter()
                .map(|network| AdminNetworkFieldView {
                    network: network.to_string(),
                    rows: vec![node_row_view("node.example.com:18081")],
                    example_address: Some("node.example.com:18089".to_string()),
                    tenant_count: 0,
                    error: None,
                    scaling: None,
                    saved_count: 1,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn node_row_view(address: &str) -> NodeRowView {
        NodeRowView {
            row: crate::admin_nodes::NodeRow {
                address: address.to_string(),
                ssl: false,
                zmq_pub: String::new(),
                self_signed: true,
                error: None,
            },
            label: address.to_string(),
            status: None,
            saved_index: Some(0),
        }
    }

    fn page(data: &AdminSettingsViewModel) -> String {
        admin_settings_page(&chrome(), data).into_string()
    }

    /// The tab bar's link to `tab`, with `aria-current` or not.
    fn tab_link(tab: SettingsTab, current: bool) -> String {
        let href = tab.href();
        let current = if current {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(
            r##"<a href="{href}" fx-action="{href}" fx-target="#settings-panel" fx-push-url{current}>{}"##,
            tab.label()
        )
    }

    /// Each network's proof-of-work checkbox (`proof_of_work.<network>`)
    /// sits in that network's own block, not with the settings for every
    /// node.
    #[test]
    fn each_networks_proof_of_work_switch_is_in_its_own_block() {
        let html = page(&full_view(SettingsTab::Nodes));
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {html}"))
        };
        let networks = ["mainnet", "stagenet", "testnet"];
        for (i, network) in networks.iter().enumerate() {
            let field = at(&format!(r#"name="proof_of_work.{network}""#));
            assert!(
                field > at(&format!(r#"data-network="{network}""#)),
                "{network}"
            );
            if let Some(next) = networks.get(i + 1) {
                assert!(
                    field < at(&format!(r#"data-network="{next}""#)),
                    "{network}"
                );
            }
            assert_eq!(
                html.matches(&format!(r#"name="proof_of_work.{network}""#))
                    .count(),
                1,
                "shown once: {network}"
            );
        }
    }

    #[test]
    fn each_tab_shows_only_its_own_settings() {
        for tab in SettingsTab::ALL {
            let html = page(&full_view(tab));
            for (key, owner, placed, _) in PLACEMENTS
                .iter()
                .filter(|(key, ..)| !key.starts_with("monero_node."))
            {
                let name = field_for(key, *owner).form_name().to_string();
                let shown = html.contains(&format!(r#"name="{name}""#));
                assert_eq!(shown, *placed == tab, "{name} on {tab:?}");
            }
            assert_eq!(
                html.contains(r#"name="node_stagenet_0_address""#),
                tab == SettingsTab::Nodes,
                "{tab:?}"
            );
            // One form, one Save, and the tab it's for.
            assert_eq!(html.matches("<form ").count(), 1, "{tab:?}");
            assert!(
                html.contains(&format!(
                    r#"<input type="hidden" name="tab" value="{}">"#,
                    tab.id()
                )),
                "{tab:?}"
            );
            assert_eq!(
                html.matches(
                    r#"<button type="submit" class="btn-primary" data-save>Save</button>"#
                )
                .count(),
                1,
                "{tab:?}"
            );
        }
    }

    #[test]
    fn tabs_with_two_owners_head_each_group() {
        let payments = page(&full_view(SettingsTab::Payments));
        let order = [
            "name=\"payment.confirmations_required\"",
            ">Webhooks</h3>",
            "name=\"webhooks.max_attempts\"",
            ">Exchange rates</h3>",
            "name=\"exchange_rate.cache_seconds\"",
        ];
        let at: Vec<usize> = order
            .iter()
            .map(|needle| {
                payments
                    .find(needle)
                    .unwrap_or_else(|| panic!("{needle}: {payments}"))
            })
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");

        let logging = page(&full_view(SettingsTab::Logging));
        let monokulo = logging.find(">Monokulo</h3>").expect(&logging);
        let engine = logging.find(">Engine</h3>").expect(&logging);
        assert!(monokulo < logging.find(r#"name="logging.level""#).unwrap());
        assert!(
            engine < logging.find(r#"name="engine:logging.level""#).unwrap() && monokulo < engine
        );

        let server = page(&full_view(SettingsTab::Server));
        assert!(
            server.find(r#"name="server.worker_threads""#).unwrap()
                < server
                    .find(r#"name="payment.scan_chunk_memory_budget_mb""#)
                    .unwrap()
        );
        assert!(server.contains(r#"name="http_cache.max_mb""#));

        let abuse = page(&full_view(SettingsTab::Abuse));
        assert!(
            abuse.contains("How this instance tells visitors apart"),
            "the explanation stays with its settings"
        );
        assert!(!page(&full_view(SettingsTab::General))
            .contains("How this instance tells visitors apart"));
    }

    #[test]
    fn general_is_the_default_and_the_open_tab_is_marked_current() {
        let html = page(&AdminSettingsViewModel {
            ..full_view(SettingsTab::General)
        });
        assert_eq!(AdminSettingsViewModel::default().tab, SettingsTab::General);
        assert!(
            html.contains(&tab_link(SettingsTab::General, true)),
            "{html}"
        );
        assert!(
            html.contains(&tab_link(SettingsTab::Nodes, false)),
            "{html}"
        );
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::Other)
        {
            let html = page(&full_view(tab));
            let bar = &html[html.find(r#"<nav id="settings-tabs""#).unwrap()..];
            let bar = &bar[..bar.find("</nav>").unwrap()];
            assert_eq!(bar.matches(r#"aria-current="page""#).count(), 1, "{tab:?}");
            assert!(html.contains(&tab_link(tab, true)), "{tab:?}: {html}");
            assert!(
                html.contains(&format!(
                    "<title>{} - Admin settings - Monokulo</title>",
                    tab.label()
                )),
                "{tab:?}"
            );
        }
    }

    /// Other only appears while a setting nobody placed is in it.
    #[test]
    fn the_other_tab_shows_only_with_something_in_it() {
        let html = page(&full_view(SettingsTab::General));
        assert!(!html.contains("tab=other"), "{html}");
        let mut data = full_view(SettingsTab::Other);
        data.engine_fields
            .push(field_for("telemetry.sample_rate", SettingOwner::Engine));
        let html = page(&data);
        assert!(html.contains(&tab_link(SettingsTab::Other, true)), "{html}");
        assert!(html.contains(r#"name="telemetry.sample_rate""#), "{html}");
    }

    #[test]
    fn banners_are_above_the_tab_bar_on_every_tab() {
        for tab in SettingsTab::ALL {
            let data = AdminSettingsViewModel {
                notices: vec![
                    Notice::Error("Something is broken.".into()),
                    Notice::Success("Welcome.".into()),
                ],
                ..full_view(tab)
            };
            let html = page(&data);
            let banner = html.find("Something is broken.").expect(&html);
            let welcome = html.find("Welcome.").expect(&html);
            let bar = html.find(r#"<nav id="settings-tabs""#).expect(&html);
            let panel = html.find(r#"<section id="settings-panel""#).expect(&html);
            assert!(banner < bar && welcome < bar && bar < panel, "{tab:?}");
        }
    }

    #[test]
    fn a_tab_is_marked_while_something_on_it_needs_attention() {
        let marked = |html: &str, tab: SettingsTab| {
            let start = html.find(&format!(r#"href="{}""#, tab.href())).unwrap();
            let end = start + html[start..].find("</a>").unwrap();
            html[start..end].contains(r#"<span class="tab-marker" aria-hidden="true">●</span><span class="visually-hidden"> (needs attention)</span>"#)
        };
        let html = page(&full_view(SettingsTab::General));
        assert!(
            SettingsTab::ALL
                .iter()
                .filter(|t| **t != SettingsTab::Other)
                .all(|t| !marked(&html, *t)),
            "nothing to see: {html}"
        );

        // A network stores use that no node answers for.
        let html = page(&AdminSettingsViewModel {
            unreachable_networks: vec!["stagenet".into()],
            ..full_view(SettingsTab::General)
        });
        assert!(marked(&html, SettingsTab::Nodes), "{html}");
        assert!(!marked(&html, SettingsTab::Server));

        // A network stores use with no node at all.
        let mut data = full_view(SettingsTab::General);
        data.engine_networks[1].rows.clear();
        data.engine_networks[1].tenant_count = 2;
        assert!(marked(&page(&data), SettingsTab::Nodes));

        // A saved setting waiting for a restart, whichever process owns it.
        let mut data = full_view(SettingsTab::General);
        data.engine_fields
            .iter_mut()
            .find(|f| f.key == "server.worker_threads")
            .unwrap()
            .pending_restart = true;
        let html = page(&data);
        assert!(
            marked(&html, SettingsTab::Server) && !marked(&html, SettingsTab::Nodes),
            "{html}"
        );
        let mut data = full_view(SettingsTab::General);
        data.monokulo_fields
            .iter_mut()
            .find(|f| f.key == "logging.level")
            .unwrap()
            .pending_restart = true;
        assert!(marked(&page(&data), SettingsTab::Logging));
    }

    /// T6: an engine that doesn't answer. Its own tabs say
    /// so instead of a form; the mixed ones still show and save monokulo's
    /// settings, with the message where the engine's would be.
    #[test]
    fn every_tab_copes_with_an_engine_it_cannot_reach() {
        let unreachable = |tab| AdminSettingsViewModel {
            engine_reachable: false,
            engine_error: Some("connection refused".into()),
            engine_fields: vec![],
            ..full_view(tab)
        };
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::Other)
        {
            let data = unreachable(tab);
            let message = "Could not reach the configured engine: connection refused";
            let html = page(&data);
            let engine_part = tab_groups(&data, tab).iter().any(|g| g.engine_only());
            assert_eq!(
                html.matches(message).count(),
                usize::from(engine_part),
                "{tab:?}: {html}"
            );
            if tab.engine_only() {
                assert!(!html.contains("<form "), "nothing to save on {tab:?}");
            } else {
                assert!(
                    html.contains("<form ")
                        && html.contains(r#"class="btn-primary" data-save>Save</button>"#),
                    "{tab:?}"
                );
                assert!(
                    !html.contains(">Webhooks</h3>"),
                    "one message, not a card per group: {html}"
                );
            }
        }
        let payments = page(&unreachable(SettingsTab::Payments));
        assert!(
            payments.contains(r#"name="exchange_rate.cache_seconds""#),
            "{payments}"
        );
        let logging = page(&unreachable(SettingsTab::Logging));
        assert!(
            logging.find(">Monokulo</h3>").unwrap() < logging.find("Could not reach").unwrap()
                && !logging.contains(">Engine</h3>"),
            "{logging}"
        );
    }

    #[test]
    fn a_setting_shows_its_name_then_help_then_control() {
        let html = page(&AdminSettingsViewModel {
            monokulo_fields: vec![AdminScalarFieldView {
                key: "engine.url".to_string(),
                label: "engine url".to_string(),
                value: "http://scanner.internal".to_string(),
                source: SettingSourceView::OptionsFile,
                help: Some("Where the engine listens.".to_string()),
                kind: SettingKindView::Url,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(
            html.contains(concat!(
                r#"<div class="setting-label-row"><label class="setting-label" for="setting-engine.url">engine url</label>"#,
                r#"<span class="source-chip" title="From the options file, which saving here writes.">"#,
            )),
            "the name, with where its value comes from beside it: {html}"
        );
        assert!(
            html.contains(concat!(
                r#"<span class="visually-hidden">From the options file, which saving here writes.</span></span><span class="changed-mark">changed</span></div>"#,
                r#"<span class="field-help" id="setting-help-engine.url">Where the engine listens.</span>"#,
                r#"<input type="url" name="engine.url" value="http://scanner.internal""#,
            )),
            "then what it is for, then its control: {html}"
        );
        assert!(
            html.contains(r#"aria-describedby="setting-help-engine.url""#),
            "{html}"
        );
    }

    /// A network with no nodes that no store uses starts closed; one with
    /// nodes, or stores, is an open block with its store count.
    #[test]
    fn a_network_is_closed_until_it_has_nodes_or_stores() {
        let mut data = full_view(SettingsTab::Nodes);
        data.engine_networks[0].tenant_count = 2;
        data.engine_networks[2].rows.clear();
        data.engine_networks[2].saved_count = 0;
        let html = page(&data);
        assert!(
            html.contains(r#"<section id="card-network-mainnet" class="settings-card node-network" data-card="network-mainnet" data-network="mainnet" data-tenant-count="2" data-saved-count="1" aria-labelledby="card-network-mainnet-title"><header class="card-head"><h3 id="card-network-mainnet-title">Mainnet</h3><span class="owner-chip">Engine</span><span class="card-meta">Used by 2 stores</span>"#),
            "{html}"
        );
        let testnet = &html[html.find(r#"data-network="testnet""#).unwrap()..];
        assert!(
            testnet.contains(
                r#"<details class="node-network-closed"><summary>Add a node for testnet</summary>"#
            ),
            "{testnet}"
        );
        assert!(!html.contains("<textarea"), "no JSON box");
        assert!(
            !html.contains("<summary>Example</summary>"),
            "no JSON example"
        );

        // With stores but no node, it's open, so the admin sees the gap.
        data.engine_networks[2].tenant_count = 1;
        let html = page(&data);
        let testnet = &html[html.find(r#"data-network="testnet""#).unwrap()..];
        assert!(
            testnet.contains(r#"<span class="card-meta">Used by 1 store</span>"#),
            "{testnet}"
        );
        assert!(!testnet.contains("node-network-closed"), "{testnet}");
    }

    #[test]
    fn node_rows_are_named_in_order_with_their_buttons_and_a_blank_row_to_add_one() {
        let mut data = full_view(SettingsTab::Nodes);
        data.engine_networks[1].rows = vec![
            node_row_view("a.example:1"),
            node_row_view("b.example:2"),
            node_row_view("c.example:3"),
        ];
        data.engine_networks[1].rows[1].row.ssl = true;
        data.engine_networks[1].rows[1].row.self_signed = false;
        let html = page(&data);
        let block = &html[html.find(r#"data-network="stagenet""#).unwrap()
            ..html.find(r#"data-network="testnet""#).unwrap()];
        let legends: Vec<&str> = block
            .match_indices("data-node-place>")
            .map(|(at, m)| {
                &block[at + m.len()..at + m.len() + block[at + m.len()..].find('<').unwrap()]
            })
            .collect();
        assert_eq!(
            legends,
            ["Primary", "Fallback 1", "Fallback 2", "Add a node"]
        );
        assert!(block.contains("Fallbacks are tried in order when the one before fails."));
        // The fields, named by network and row.
        assert!(block.contains(r#"<input type="text" name="node_stagenet_1_address" id="node-stagenet-1-address" value="b.example:2""#), "{block}");
        assert!(block.contains(r#"<input type="checkbox" name="node_stagenet_1_ssl" id="node-stagenet-1-ssl" value="on" checked"#), "{block}");
        assert!(block.contains(r#"<input type="checkbox" name="node_stagenet_1_self_signed" id="node-stagenet-1-self_signed" value="on" aria-describedby"#), "{block}");
        assert!(
            block.contains(
                r#"name="node_stagenet_3_address" id="node-stagenet-3-address" value="""#
            ),
            "the blank row: {block}"
        );
        assert!(block.contains(r#"<input type="checkbox" name="node_stagenet_3_self_signed" id="node-stagenet-3-self_signed" value="on" checked"#), "self-signed is ticked by default: {block}");
        // No Move up on the first row, no Move down on the last, none on the blank one.
        let buttons: Vec<&str> = block
            .match_indices(r#"name="node_action" value=""#)
            .map(|(at, m)| {
                &block[at + m.len()..at + m.len() + block[at + m.len()..].find('"').unwrap()]
            })
            .collect();
        assert_eq!(
            buttons,
            [
                "down:stagenet:0",
                "remove:stagenet:0",
                "up:stagenet:1",
                "down:stagenet:1",
                "remove:stagenet:1",
                "up:stagenet:2",
                "remove:stagenet:2"
            ]
        );
        // Each field: its name, then what it's for, then the control.
        assert!(block.contains(r#"<div class="setting-label-row"><label class="setting-label" for="node-stagenet-0-address">Address</label></div><span class="field-help" id="node-stagenet-0-address-help">The node's host and port, like <code>node.example.com:18089</code>."#), "{block}");
        // Enter in an address box saves, rather than pressing a row's button.
        let form = &html[html.find("<form ").unwrap()..];
        assert!(form.find(r#"<button type="submit" class="visually-hidden" tabindex="-1" aria-hidden="true">Save</button>"#).unwrap() < form.find(r#"name="node_action""#).unwrap());
    }

    /// The Monero nodes tab shows how the engine performs beside the
    /// settings each figure describes (docs/engine_scaling.md section 6):
    /// Resources at the top, a Scanning panel per network, and each node's
    /// lag and link.
    #[test]
    fn the_nodes_tab_shows_resources_scanning_and_each_nodes_link() {
        let mut data = full_view(SettingsTab::Nodes);
        let report = shared::resources::ResourceReport {
            host_id: "boot".into(),
            cpu_count: 2,
            machine_memory_bytes: Some(4_000_000_000),
            cgroup_memory_bytes: None,
            samples: vec![],
        };
        data.resources = Some(crate::views::scaling::ResourcesView {
            engine: Some(report.clone()),
            monokulo: report,
            now_unix: 1_800_000_000,
            one_process: false,
        });
        let network = &mut data.engine_networks[0];
        network.scaling = Some(shared::scaling::NetworkScaling {
            scan: shared::scaling::ScanReport {
                avg_block_bytes: 0,
                block_size_trend: shared::scaling::Trend::Steady,
                last_chunk: None,
                blocks_per_minute: 0.0,
                fetch_secs_recent: 0.0,
                scan_secs_recent: 0.0,
                largest_recent: None,
                in_progress: None,
                in_progress_secs: None,
                peak_cache_bytes: None,
                discarded_cache_bytes_recent: 0,
                round_budget_secs: None,
                headers_first: None,
            },
            blocks_behind: 3,
            catch_up_secs: None,
            pace: shared::scaling::Pace::Link,
            budget_mb: 256,
            max_budget_mb: None,
            round_deadline_secs: 10,
            round_base_secs: 10,
            slow: None,
        });
        network.rows[0].status = Some(NodeStatusView {
            height: Some(1_000),
            behind: Some(2),
            in_use: true,
            link: Some(crate::views::scaling::NodeLinkView {
                link: shared::scaling::LinkSnapshot {
                    measured: true,
                    rtt_ms: 80,
                    ttfb_per_block_ms: 20,
                    rate_bytes_per_sec: 1_250_000,
                    bytes_per_block: 50_000,
                    last_measured_unix: Some(1_800_000_000),
                    timeouts_last_hour: 0,
                    failures_last_hour: 0,
                    history: vec![],
                },
                now_unix: 1_800_000_000,
            }),
            ..Default::default()
        });
        let html = page(&data);
        let resources = html.find(r#"<h3 id="resources-title">Resources</h3>"#);
        let form = html.find(r#"id="settings-form""#);
        assert!(
            resources.is_some() && resources < form,
            "Resources comes first: {html}"
        );
        assert!(
            html.contains("Reachable, height 1,000 (2 behind). In use."),
            "{html}"
        );
        assert!(
            html.contains("Transfer <strong>10.0 Mbit/s</strong>"),
            "{html}"
        );
        assert!(html.contains(r#"data-scanning="mainnet""#), "{html}");
        assert!(
            html.contains("Link speed: node.example.com:18081 at 10.0 Mbit/s."),
            "the pace names the node in use: {html}"
        );

        // Other tabs don't carry it.
        data.tab = SettingsTab::Server;
        assert!(!page(&data).contains("resources-title"));
    }

    #[test]
    fn a_nodes_status_is_said_in_words() {
        let mut data = full_view(SettingsTab::Nodes);
        let statuses = [
            NodeStatusView {
                height: Some(1_234_567),
                in_use: true,
                behind: Some(0),
                ..Default::default()
            },
            NodeStatusView {
                error: Some("connection refused".into()),
                resting: true,
                ..Default::default()
            },
            NodeStatusView {
                height: Some(10),
                wrong_network: Some("mainnet".into()),
                ..Default::default()
            },
        ];
        data.engine_networks[1].rows = statuses
            .iter()
            .enumerate()
            .map(|(i, status)| NodeRowView {
                status: Some(status.clone()),
                ..node_row_view(&format!("n{i}.example:1"))
            })
            .collect();
        data.engine_networks[1]
            .rows
            .push(node_row_view("new.example:1"));
        let html = page(&data);
        assert!(
            html.contains(r#"<p class="node-status">Reachable, height 1,234,567. In use.</p>"#),
            "{html}"
        );
        assert!(html.contains(r#"<p class="node-status is-problem">Not reachable: connection refused. Resting after failures.</p>"#), "{html}");
        assert!(
            html.contains(
                r#"<p class="node-status is-problem">Wrong network: this node is on mainnet.</p>"#
            ),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"class="node-status"#).count(),
            3,
            "nothing for a node with no status yet"
        );
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(1000), "1,000");
    }

    #[test]
    fn what_is_wrong_shows_where_it_is() {
        let mut data = full_view(SettingsTab::Nodes);
        data.engine_networks[1].rows[0].row.address = "node.example.com".into();
        data.engine_networks[1].rows[0].row.error =
            Some("Add the port, like node.example.com:18081.".into());
        data.engine_networks[2].error =
            Some("node.example.com:18081 is on mainnet, not testnet.".into());
        let html = page(&data);
        assert!(
            html.contains(r#"value="node.example.com" aria-describedby="node-stagenet-0-address-help node-stagenet-0-error" aria-invalid="true""#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="setting-problem" id="node-stagenet-0-error">Add the port, like node.example.com:18081.</span>"#), "{html}");
        let testnet = &html[html.find(r#"data-network="testnet""#).unwrap()..];
        assert!(testnet.contains(r#"<p class="error" role="alert">node.example.com:18081 is on mainnet, not testnet.</p><p class="hint">Fallbacks"#), "at the top of its block: {testnet}");
    }

    #[test]
    fn each_key_custody_backend_has_its_own_section_shown_while_it_is_turned_on() {
        let field = |key: &str, value: &str, kind: SettingKindView| AdminScalarFieldView {
            key: key.to_string(),
            label: key.to_string(),
            value: value.to_string(),
            kind,
            ..Default::default()
        };
        let backends = || SettingKindView::ChoiceList {
            choices: vec!["plain".into(), "snp".into()],
        };
        let page = |enabled: &str| {
            let data = AdminSettingsViewModel {
                tab: SettingsTab::Custody,
                engine_reachable: true,
                engine_fields: vec![
                    field(
                        "key_custody.default_backend",
                        "plain",
                        SettingKindView::Choice {
                            choices: vec!["plain".into(), "snp".into()],
                        },
                    ),
                    field("key_custody.enabled_backends", enabled, backends()),
                    field("key_custody.snp_trusted_id_key", "", SettingKindView::Path),
                    field(
                        "key_custody.snp_min_guest_svn",
                        "",
                        SettingKindView::Integer {
                            min: Some(0),
                            max: Some(4_294_967_295),
                        },
                    ),
                ],
                ..Default::default()
            };
            admin_settings_page(&chrome(), &data).into_string()
        };

        let html = page("plain");
        // The backends are boxes to tick, after an empty value so ticking
        // none still says so; they come before the choice among them.
        assert!(
            html.contains(r#"<input type="hidden" name="key_custody.enabled_backends" value="">"#),
            "{html}"
        );
        assert!(html.contains(r#"<input type="checkbox" name="key_custody.enabled_backends" value="plain" checked>"#), "{html}");
        assert!(
            html.contains(
                r#"<input type="checkbox" name="key_custody.enabled_backends" value="snp">"#
            ),
            "{html}"
        );
        assert!(
            html.find(r#"name="key_custody.enabled_backends""#).unwrap()
                < html.find(r#"name="key_custody.default_backend""#).unwrap()
        );
        // The snp backend's device and minimum security version sit in its
        // own section, hidden while snp is off; plain has nothing to set.
        let snp = html
            .find(r#"<section id="card-custody-snp" class="settings-card" data-card="custody-snp" data-custody-backend="snp" hidden"#)
            .expect(&html);
        assert!(
            html.find(r#"name="key_custody.snp_trusted_id_key""#)
                .unwrap()
                > snp,
            "{html}"
        );
        assert!(
            html.find(r#"name="key_custody.snp_min_guest_svn""#)
                .unwrap()
                > snp,
            "{html}"
        );
        assert!(
            html.find(r#"name="key_custody.default_backend""#).unwrap() < snp,
            "{html}"
        );
        assert!(
            !html.contains(r#"data-custody-backend="plain""#),
            "nothing to set up for plain: {html}"
        );
        assert!(
            html.contains(r#"name === "key_custody.enabled_backends""#),
            "shown as soon as it's ticked, with JavaScript"
        );

        let html = page("plain,snp");
        assert!(html.contains(r#"<section id="card-custody-snp" class="settings-card" data-card="custody-snp" data-custody-backend="snp" aria-labelledby="card-custody-snp-title"><header class="card-head"><h3 id="card-custody-snp-title">SEV-SNP</h3>"#), "{html}");
        assert!(html.contains(r#"value="snp" checked"#), "{html}");
    }

    #[test]
    fn inputs_follow_the_kind_of_value_and_secrets_are_never_echoed() {
        let field = |kind: SettingKindView, value: &str| AdminScalarFieldView {
            key: "k".to_string(),
            label: "k".to_string(),
            value: value.to_string(),
            kind,
            ..Default::default()
        };
        let choice = scalar_field(&field(
            SettingKindView::Choice {
                choices: vec!["public".into(), "invite_only".into()],
            },
            "public",
        ))
        .into_string();
        assert!(
            choice.contains(r#"<option value="public" selected>"#),
            "{choice}"
        );
        let boolean = scalar_field(&field(SettingKindView::Bool, "false")).into_string();
        assert!(
            boolean.contains(r#"<option value="false" selected>"#),
            "{boolean}"
        );
        let secret = scalar_field(&field(
            SettingKindView::Secret,
            "\u{2022}\u{2022}\u{2022}\u{2022}",
        ))
        .into_string();
        assert!(
            secret.contains(r#"type="password" value="locked""#) && secret.contains("disabled"),
            "a secret comes from the environment: always locked, {secret}"
        );
        assert!(!secret.contains("name="), "never sent: {secret}");
        assert!(!secret.contains('\u{2022}'));
    }

    #[test]
    fn development_logging_is_chosen_as_off_or_a_number_of_hours_and_says_when_it_ends() {
        let now = 1_790_000_000;
        let field = |value: &str| AdminScalarFieldView {
            key: "logging.dev_mode_until".to_string(),
            label: "logging dev mode until".to_string(),
            value: value.to_string(),
            kind: SettingKindView::TimeLimit {
                now,
                until_label: "21 Sep, 22:23".into(),
            },
            ..Default::default()
        };
        let off = scalar_field(&field("0")).into_string();
        assert!(
            off.contains(r#"<option value="0" selected>Off</option>"#),
            "{off}"
        );
        assert!(
            off.contains(&format!(
                r#"<option value="{}">On for 1 hour</option>"#,
                now + 3600
            )),
            "{off}"
        );
        assert!(
            off.contains(&format!(
                r#"<option value="{}">On for 24 hours</option>"#,
                now + 86_400
            )),
            "{off}"
        );
        assert!(!off.contains("On until"), "{off}");

        let on = scalar_field(&field(&(now + 600).to_string())).into_string();
        assert!(
            on.contains(&format!(
                r#"<option value="{}" selected>On until 21 Sep, 22:23</option>"#,
                now + 600
            )),
            "{on}"
        );
        assert!(on.contains(r#"<option value="0">Off</option>"#), "{on}");

        let ended = scalar_field(&field(&(now - 1).to_string())).into_string();
        // Off, and sent as it is, so saving the tab as shown changes nothing.
        assert!(
            ended.contains(&format!(
                r#"<option value="{}" selected>Off</option>"#,
                now - 1
            )),
            "a time already past is off: {ended}"
        );
    }

    #[test]
    fn notices_render_with_their_level_and_restart_only_fields_say_so() {
        let data = AdminSettingsViewModel {
            notices: vec![
                Notice::Error(
                    "2 stores use the stagenet network, which no longer has any reachable nodes."
                        .into(),
                ),
                Notice::Success("Your admin account is ready.".into()),
            ],
            tab: SettingsTab::Server,
            engine_reachable: true,
            engine_fields: vec![AdminScalarFieldView {
                key: "server.worker_threads".into(),
                label: "server worker threads".into(),
                value: "4".into(),
                source: SettingSourceView::OptionsFile,
                restart_only: true,
                pending_restart: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let html = admin_settings_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<p class="error" role="alert">2 stores use the stagenet network"#));
        assert!(html.contains(r#"<p class="success" role="status">Your admin account is ready."#));
        assert!(html.contains("Applies after a restart."));
        assert!(html.contains("restart needed"));
    }

    /// A card, by id, as the page renders it: from its section to the next.
    fn card<'a>(html: &'a str, group: &str) -> &'a str {
        let start = html
            .find(&format!(r#"<section id="card-{group}""#))
            .unwrap_or_else(|| panic!("card-{group} in {html}"));
        let rest = &html[start..];
        &rest[..rest.find("</section>").unwrap()]
    }

    #[test]
    fn each_group_of_settings_is_a_card_with_its_heading_and_owner() {
        let payments = page(&full_view(SettingsTab::Payments));
        for (group, title, owner, key) in [
            (
                "orders",
                "Orders",
                "Engine",
                "payment.confirmations_required",
            ),
            (
                "chain",
                "Watching the chain",
                "Engine",
                "payment.reorg_check_depth",
            ),
            ("webhooks", "Webhooks", "Engine", "webhooks.max_attempts"),
            (
                "exchange-rates",
                "Exchange rates",
                "Monokulo",
                "exchange_rate.cache_seconds",
            ),
        ] {
            let card = card(&payments, group);
            assert!(
                card.contains(&format!(r#"<h3 id="card-{group}-title">{title}</h3><span class="owner-chip">{owner}</span>"#)),
                "{card}"
            );
            assert!(
                card.contains(&format!(r#"name="{key}""#)),
                "{group}: {card}"
            );
            // Its own Discard, shown by the script while it has changes.
            assert!(
                card.contains(r#"<button type="button" class="card-discard js-only" data-card-discard hidden>Discard</button>"#),
                "{card}"
            );
        }
        // Subheadings inside a card where it has parts.
        let rates = card(&payments, "exchange-rates");
        let parts: Vec<usize> = [
            "<h4>Coingecko</h4>",
            "<h4>CoinMarketCap</h4>",
            "<h4>RetoSwap (Haveno)</h4>",
            "<h4>Every provider</h4>",
        ]
        .iter()
        .map(|h| rates.find(h).unwrap_or_else(|| panic!("{h}: {rates}")))
        .collect();
        assert!(parts.windows(2).all(|w| w[0] < w[1]), "{rates}");

        // A card nothing on the page can change says so, with no Discard.
        let mut data = full_view(SettingsTab::Custody);
        for field in &mut data.monokulo_fields {
            if field.key.starts_with("key_custody.cli_") {
                field.locked = Some("Given on the command line.".into());
            }
        }
        let html = page(&data);
        let cli = card(&html, "custody-cli");
        assert!(
            cli.contains("Can't be changed here") && !cli.contains("data-card-discard"),
            "{cli}"
        );
        // This site's own SEV-SNP settings share the engine's card.
        let snp = card(&html, "custody-snp");
        assert!(
            snp.contains(r#"<span class="owner-chip">Engine and monokulo</span>"#),
            "{snp}"
        );
        assert!(
            snp.find(r#"name="key_custody.snp_min_tcb""#).unwrap()
                < snp.find("<h4>This site's key entry</h4>").unwrap()
                && snp.find("<h4>This site's key entry</h4>").unwrap()
                    < snp.find(r#"name="key_custody.snp_entry_min_tcb""#).unwrap(),
            "{snp}"
        );
    }

    #[test]
    fn the_save_bar_is_the_tabs_one_save_and_discard_loads_the_tab_again() {
        let html = page(&full_view(SettingsTab::Payments));
        let form = &html[html.find("<form ").unwrap()..html.find("</form>").unwrap()];
        let bar = &form[form.find(r#"<div id="save-bar""#).expect(form)..];
        assert!(
            bar.starts_with(r#"<div id="save-bar" class="save-bar" role="region" aria-label="Save changes" data-save-bar>"#),
            "{bar}"
        );
        assert!(bar.contains(
            "Saving writes the changes on this tab to the options file and applies them."
        ));
        assert!(
            bar.contains(r#"<a class="btn" href="/dashboard/admin/settings?tab=payments" data-discard-all>Discard changes</a><button type="submit" class="btn-primary" data-save>Save</button>"#),
            "{bar}"
        );
        // After every card, last in the form.
        assert!(form.rfind("</section>").unwrap() < form.find(r#"id="save-bar""#).unwrap());
    }

    #[test]
    fn a_refused_card_is_red_says_why_and_shows_what_was_typed() {
        let mut data = full_view(SettingsTab::Payments);
        let attempts = data
            .engine_fields
            .iter_mut()
            .find(|f| f.key == "webhooks.max_attempts")
            .unwrap();
        attempts.value = "100".into();
        attempts.saved_value = Some("8".into());
        attempts.problem = Some("must be from 1 to 64".into());
        let failures = vec![Failure {
            group: Some(Group::Webhooks),
            message: "The engine refused the change: webhooks.max_attempts must be from 1 to 64."
                .into(),
        }];
        data.outcome = Some(SaveOutcome::PartlySaved {
            saved: vec![Group::ExchangeRates],
            at: "8 Oct, 14:22".into(),
            failures: failures.clone(),
        });
        data.answers_save = true;
        let html = page(&data);

        let webhooks = card(&html, "webhooks");
        assert!(
            webhooks.starts_with(r#"<section id="card-webhooks" class="settings-card is-failed""#),
            "{webhooks}"
        );
        assert!(
            webhooks.contains(r#"<span class="badge badge-error">Not saved</span>"#),
            "{webhooks}"
        );
        assert!(webhooks.contains(r#"<p class="error" role="alert">The engine refused the change: webhooks.max_attempts must be from 1 to 64.</p>"#), "{webhooks}");
        // What was typed, still to fix, with the saved value for Discard.
        assert!(
            webhooks.contains(r#"name="webhooks.max_attempts" value="100""#),
            "{webhooks}"
        );
        assert!(webhooks.contains(r#"data-saved="8""#), "{webhooks}");
        assert!(webhooks.contains("must be from 1 to 64"), "{webhooks}");
        assert!(!webhooks.contains("Saved 8 Oct"), "{webhooks}");

        // The card that was saved says when; the others say nothing.
        assert!(card(&html, "exchange-rates").contains(
            r#"<span class="card-meta card-saved" data-card-saved>Saved 8 Oct, 14:22</span>"#
        ));
        assert!(!card(&html, "orders").contains("Saved 8 Oct"));

        // The bar is red, says why, links to the card, and takes focus.
        assert!(
            html.contains(r#"<div id="save-bar" class="save-bar is-failed""#),
            "{html}"
        );
        assert!(html.contains(r##"<p class="save-bar-message" data-save-bar-message tabindex="-1" data-fx-focus><strong>Changes partly saved.</strong> The engine refused the change: webhooks.max_attempts must be from 1 to 64. <a href="#card-webhooks" data-show-card="webhooks">Show</a></p>"##), "{html}");
        // Nothing saved: no card says it was.
        data.outcome = Some(SaveOutcome::Refused { failures });
        let html = page(&data);
        assert!(html.contains("<strong>Nothing saved.</strong>"), "{html}");
        assert!(!html.contains("Saved 8 Oct"), "{html}");
        // A refusal that names no setting: the bar says it, with no card to show.
        data.outcome = Some(SaveOutcome::Refused {
            failures: vec![Failure {
                group: None,
                message: "Could not reach the configured engine: connection refused".into(),
            }],
        });
        let html = page(&data);
        assert!(html.contains(r#"data-fx-focus><strong>Nothing saved.</strong> Could not reach the configured engine: connection refused</p>"#), "{html}");
    }

    #[test]
    fn a_toast_says_how_a_save_went() {
        let toast = |kind, show: Option<Group>| {
            let data = AdminSettingsViewModel {
                toast: Some(Toast {
                    kind,
                    title: "Webhooks saved and applied".into(),
                    lines: vec!["One more thing.".into()],
                    show,
                }),
                ..full_view(SettingsTab::Payments)
            };
            let html = page(&data);
            let at = html
                .find(r#"<div id="settings-toasts" class="toasts">"#)
                .expect(&html);
            html[at..at + html[at..].find("</div></div>").unwrap()].to_string()
        };
        let success = toast(ToastKind::Success, None);
        assert!(success.contains(r#"<div class="toast toast-success" role="status" data-toast><span class="toast-icon" aria-hidden="true">✓</span><div class="toast-text"><strong>Webhooks saved and applied</strong><span class="toast-line">One more thing.</span>"#), "{success}");
        assert!(!success.contains("Show"), "{success}");
        assert!(toast(ToastKind::Warning, None)
            .contains(r#"class="toast toast-warning" role="status""#));
        assert!(toast(ToastKind::Neutral, None)
            .contains(r#"class="toast toast-neutral" role="status""#));
        let error = toast(ToastKind::Error, Some(Group::Webhooks));
        assert!(
            error.contains(r#"class="toast toast-error" role="alert""#),
            "{error}"
        );
        assert!(error.contains(r##"<a class="toast-show" href="#card-webhooks" data-show-card="webhooks">Show</a>"##), "{error}");
        // No toast, an empty place for one: a save with fixi fills it.
        let html = page(&full_view(SettingsTab::Payments));
        assert!(
            html.contains(r#"<div id="settings-toasts" class="toasts"></div>"#),
            "{html}"
        );
        // A save's panel brings its toast; a tab link's brings none, so
        // the last save's stays.
        let mut saved = full_view(SettingsTab::Payments);
        saved.toast = Some(Toast {
            kind: ToastKind::Success,
            title: "Orders saved and applied".into(),
            lines: vec![],
            show: None,
        });
        let fragment = settings_fragment(&saved, false).into_string();
        assert!(
            fragment.contains(r#"<div id="settings-toasts" class="toasts" data-fx-oob>"#),
            "{fragment}"
        );
        let fragment = settings_fragment(&full_view(SettingsTab::Payments), false).into_string();
        assert!(!fragment.contains("settings-toasts"), "{fragment}");
    }

    #[test]
    fn a_node_row_is_closed_to_a_line_with_a_handle_and_its_move_buttons_are_for_no_script() {
        let mut data = full_view(SettingsTab::Nodes);
        data.engine_networks[1].rows =
            vec![node_row_view("a.example:1"), node_row_view("b.example:2")];
        data.engine_networks[1].rows[1].saved_index = Some(1);
        data.engine_networks[1].rows[1].status = Some(NodeStatusView {
            height: Some(5),
            in_use: true,
            ..Default::default()
        });
        data.engine_networks[1].saved_count = 2;
        let html = page(&data);
        let block = card(&html, "network-stagenet");
        assert!(block.contains(r#"data-saved-count="2""#), "{block}");
        assert!(
            block.contains(r#"<details class="node-row" data-node-row="1" data-node-saved="1"><summary class="node-row-summary"><span class="node-handle js-only" role="button" tabindex="0" title="Drag to reorder" aria-label="Reorder Fallback 1: press the up or down arrow" data-node-handle>⠿</span><span class="node-row-name" data-node-place>Fallback 1</span><code class="node-row-address">b.example:2</code><span class="node-row-mark" data-node-mark></span><span class="node-row-status">Reachable, height 5. In use</span></summary>"#),
            "{block}"
        );
        assert!(block.contains(r#"<button type="submit" class="no-js-only" name="node_action" value="up:stagenet:1">Move up</button>"#), "{block}");
        assert!(block.contains(r#"<button type="submit" name="node_action" value="remove:stagenet:1" data-node-remove>Remove</button>"#), "{block}");
        // The blank row to add one: no handle, no buttons.
        assert!(block.contains(r#"<details class="node-row" data-node-row="2" data-node-add><summary class="node-row-summary"><span class="node-add-mark" aria-hidden="true">+</span><span class="node-row-name" data-node-place>Add a node</span>"#), "{block}");
        // A row with something to fix opens by itself.
        data.engine_networks[1].rows[0].row.error = Some("Add the port.".into());
        let html = page(&data);
        assert!(
            html.contains(
                r#"<details class="node-row" data-node-row="0" data-node-saved="0" open>"#
            ),
            "{html}"
        );
    }
}
