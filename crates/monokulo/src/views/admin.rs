//! Admin-only pages: first-run setup (`GET`/`POST /admin/setup`), the
//! settings page (`GET`/`POST /admin/settings?tab=<id>`), the invites page
//! (`GET /admin/invites` and its create-link/delete actions), and
//! the public `GET`/`POST /request-invite` form.

use maud::{html, Markup, PreEscaped};

use super::settings::{Card, Field, Save};
pub use super::settings::{Toast, ToastKind};
use super::{script, Load, PageChrome};
use ui::{Choice, Select};

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
    let header = ui::PageHeader::new("Set up your admin account");
    let body = html! {
        p {
            "This is a one-time step. The account created here is this instance's single administrator, "
            "and can configure every monokulo and scanner setting from the admin page afterward."
        }
        @if let Some(error) = &data.error {
            (ui::Alert::new(error.as_str(), ui::Tone::Error).quiet(true))
        }
        form method="post" action=(crate::base::url("/admin/setup")) class="mk-stack" {
            (ui::Field::new("Email", "setup-email", "email")
                .kind(ui::InputType::Email)
                .value(data.email.as_str())
                .required(true)
                .autocomplete("email"))
            (ui::Field::new("Password", "setup-password", "password")
                .kind(ui::InputType::Password)
                .required(true)
                .autocomplete("new-password")
                .attr("minlength", "12"))
            (ui::Field::new("Confirm password", "setup-confirm-password", "confirm_password")
                .kind(ui::InputType::Password)
                .required(true)
                .autocomplete("new-password")
                .attr("minlength", "12"))
            (ui::FormActions::new(ui::Button::new("Create admin account")
                .variant(ui::ButtonVariant::Primary)
                .kind(ui::ButtonType::Submit)))
        }
    };
    super::Page::new("Set up admin account - Monokulo", header).render(chrome, body)
}

pub struct RequestInviteViewModel {
    pub error: Option<String>,
    /// `true` after a successful `POST` - the page shows a plain thank-you
    /// message instead of the form again, so a visitor can't accidentally
    /// double-submit by refreshing.
    pub submitted: bool,
}

pub fn request_invite_page(chrome: &PageChrome, data: &RequestInviteViewModel) -> Markup {
    let header = ui::PageHeader::new("Request an invite");
    let body = html! {
        @if data.submitted {
            p { "Thanks - we'll be in touch if there's a spot for you." }
        } @else {
            @if let Some(error) = &data.error {
                (ui::Alert::new(error.as_str(), ui::Tone::Error).quiet(true))
            }
            p { "This instance is invite-only right now. Tell us a bit about what you'd like to use it for and we'll follow up." }
            form method="post" action=(crate::base::url("/request-invite")) class="mk-stack" {
                (ui::Field::new("Email", "request-invite-email", "email")
                    .kind(ui::InputType::Email)
                    .required(true)
                    .autocomplete("email"))
                (ui::Field::new("Message", "request-invite-message", "message")
                    .multiline(true)
                    .required(true))
                (ui::FormActions::new(ui::Button::new("Request an invite")
                    .variant(ui::ButtonVariant::Primary)
                    .kind(ui::ButtonType::Submit)))
            }
        }
    };
    super::Page::new("Request an invite - Monokulo", header).render(chrome, body)
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

/// A row's two actions, each in a column of its own so they line up from
/// row to row: email the invite (an envelope), and delete the request (a
/// red trash can). A row just deleted says so across both.
fn invite_row(row: &AdminInviteRequestRow, page: u32) -> ui::Row {
    let action = |children: Markup| ui::Cell::new(children).flag("data-invite-action");
    let actions: Vec<ui::Slot> = if row.just_deleted {
        vec![ui::Cell::new("deleted").attr("colspan", "2").into()]
    } else {
        let delete = crate::base::url(format!("/admin/invites/{}/delete?page={}", row.id, page));
        vec![
            action(html! {
                @if let Some(href) = &row.mailto_href {
                    (ui::IconLink::new(format!("Email an invite to {}", row.email), href, html! { (PreEscaped(ui::assets::icons::MAIL)) }))
                }
            })
            .into(),
            action(html! {
                form method="post" action=(delete) class="inline-form" fx-action=(delete) fx-method="POST" fx-target="#invites" {
                    (ui::IconButton::new(format!("Delete the request from {}", row.email), html! { (PreEscaped(ui::assets::icons::TRASH)) })
                        .kind(ui::ButtonType::Submit)
                        .attr("data-variant", "danger"))
                }
            })
            .into(),
        ]
    };
    let tr = ui::Row::new(ui::Slot::many(
        [
            ui::Cell::new(row.email.as_str()).into(),
            ui::Cell::new(row.message.as_str()).into(),
            ui::Cell::new(row.created_at_display.clone()).into(),
        ]
        .into_iter()
        .chain(actions),
    ));
    if row.just_deleted {
        tr.flag("data-deleted")
    } else {
        tr
    }
}

/// The invite requests' table, its header and its rows.
fn invites_table(rows: impl IntoIterator<Item = ui::Row>) -> ui::Table {
    let head = ui::Row::new(ui::Slot::many(
        ["Email", "Message", "Requested"]
            .map(|name| ui::Cell::new(name).header(true).into())
            .into_iter()
            .chain([ui::Cell::new("Actions")
                .header(true)
                .attr("colspan", "2")
                .into()]),
    ));
    ui::Table::new(ui::Slot::many(rows.into_iter().map(ui::Slot::from))).head(head)
}

/// Everything on the invites page below its heading: what fixi swaps back
/// after any button here, or a page change.
pub fn invites_section(data: &AdminInvitesViewModel) -> Markup {
    let target = "#invites";
    html! {
        section id="invites" {
            @if let Some(error) = &data.error {
                (ui::Alert::new(error.as_str(), ui::Tone::Error).flag("data-fx-focus").attr("tabindex", "-1"))
            }
            @if let Some(success) = &data.success {
                (ui::Alert::new(success.as_str(), ui::Tone::Success).flag("data-fx-focus").attr("tabindex", "-1"))
            }

            form method="post" action=(crate::base::url("/admin/invites/create-link"))
                fx-action=(crate::base::url("/admin/invites/create-link")) fx-method="POST" fx-target=(target) {
                (ui::FormActions::new(ui::Button::new("Create invite link")
                    .variant(ui::ButtonVariant::Primary)
                    .kind(ui::ButtonType::Submit)))
            }
            @if let Some(link) = &data.created_link {
                p { "Share this link - it works once:" }
                pre data-fx-focus tabindex="-1" { (link) }
            }

            h2 { "Pending requests" }
            @if !data.rows.is_empty() {
                (invites_table(
                    data.just_deleted_row
                        .iter()
                        .chain(&data.rows)
                        .map(|row| invite_row(row, data.page)),
                ))
                (ui::Pagination::new("Invite request pages")
                    .maybe_previous(data.has_previous.then(|| crate::base::url(format!("/admin/invites?page={}", data.previous_page))))
                    .maybe_next(data.has_next.then(|| crate::base::url(format!("/admin/invites?page={}", data.next_page))))
                    .status(format!("Page {} of {}", data.page, data.total_pages))
                    .fixi_target(target))
                form method="post" action=(crate::base::url("/admin/invites/delete-all"))
                    fx-action=(crate::base::url("/admin/invites/delete-all")) fx-method="POST" fx-target=(target) {
                    (ui::FormActions::new(ui::Button::new("Delete all").kind(ui::ButtonType::Submit)))
                }
            } @else if let Some(deleted) = &data.just_deleted_row {
                (invites_table([invite_row(deleted, data.page)]))
            } @else {
                (ui::Hint::new("No pending invite requests."))
            }
        }
    }
}

pub fn admin_invites_page(chrome: &PageChrome, data: &AdminInvitesViewModel) -> Markup {
    let header = ui::PageHeader::new("Invites").crumbs(super::crumbs(&[("/", "Home")]));
    let body = html! {
        (invites_section(data))
    };
    super::Page::new("Invites - Monokulo", header).render(chrome, body)
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
        /// When the saved one ends, when a refused save shows another
        /// value: so Discard can put it back.
        saved_until_label: Option<String>,
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
    /// A secret the page may set (`sources: [Database, Env]`: the
    /// CoinMarketCap API key). Write-only: the page says whether one is
    /// set, never what it is, and offers a box to replace it and a box to
    /// remove it ([`secret_remove_name`]).
    pub stored_secret: bool,
}

/// The name of the box that removes the secret setting `key` on save.
pub fn secret_remove_name(key: &str) -> String {
    format!("{key}:remove")
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
    ui::Tag::new(html! {
        span aria-hidden="true" { (source.label()) }
        span class="visually-hidden" { (source.sentence()) }
    })
    .icon(source_icon(source))
    .tone(if source == SettingSourceView::Default {
        ui::TagTone::Dashed
    } else {
        ui::TagTone::Muted
    })
    .attr("title", source.sentence())
    .render()
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
    /// when the rows have been reordered. `None` for a row that isn't saved
    /// (one a refused save added).
    pub saved_index: Option<usize>,
    /// The saved node's values, when a refused save shows the row with
    /// others: so the page's script counts it as changed, and Discard puts
    /// them back.
    pub saved: Option<crate::admin_nodes::NodeRow>,
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
/// `/admin/settings?tab=<id>`, so switching works as a plain link
/// and a save can send the browser back to the tab it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Nodes,
    Payments,
    Custody,
    Abuse,
    /// How visitors on Tor reach this instance (`crate::tor`).
    Tor,
    Server,
    Logging,
    /// The options files the settings are kept in, each with its Reload.
    Files,
    /// Engine (or monokulo) settings added after this map was written and
    /// not placed yet. Only shown while something is in it.
    Other,
}

impl SettingsTab {
    /// Every tab, in the order the tab bar shows them. General comes first:
    /// the engine's tabs stay empty until its connection there works.
    pub const ALL: [SettingsTab; 10] = [
        SettingsTab::General,
        SettingsTab::Nodes,
        SettingsTab::Payments,
        SettingsTab::Custody,
        SettingsTab::Abuse,
        SettingsTab::Tor,
        SettingsTab::Server,
        SettingsTab::Logging,
        SettingsTab::Files,
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
            SettingsTab::Tor => "tor",
            SettingsTab::Server => "server",
            SettingsTab::Logging => "logging",
            SettingsTab::Files => "files",
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
            SettingsTab::Tor => "Tor",
            SettingsTab::Server => "Server",
            SettingsTab::Logging => "Logging",
            SettingsTab::Files => "Files",
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
        format!("/admin/settings?tab={}", self.id())
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
            SettingsTab::Tor => &[Group::TorService, Group::TorVisitors],
            SettingsTab::Server => &[Group::Server(Monokulo), Group::Server(Engine)],
            SettingsTab::Logging => &[Group::Logging(Monokulo), Group::Logging(Engine)],
            SettingsTab::Files => &[],
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
    /// How Tor runs: inside monokulo, an external tor, or off.
    TorService,
    /// What visitors on the web hear about the onion address.
    TorVisitors,
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
                "public_uri" | "onion_uri" => Group::PublicAddress,
                "tor" if rest == "onion_location" => Group::TorVisitors,
                "tor" => Group::TorService,
                "engine" => Group::EngineConnection,
                "exchange_rate" => Group::ExchangeRates,
                "webhooks" => Group::Webhooks,
                "abuse" if matches!(rest, "challenge_bits" | "under_attack") => {
                    Group::AbuseChallenge
                }
                // Where an external tor connects: part of how Tor runs.
                "abuse" if rest == "onion_listener" => Group::TorService,
                "abuse" if rest == "trusted_proxies" => Group::AbuseVisitors,
                "abuse" | "rate_limit" => Group::AbuseLimits,
                "http_cache" | "database" | "server" | "crypto" => Group::Server(owner),
                // The Query page's switch, beside the server's own settings.
                "admin" => Group::Server(owner),
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
                // How long the engine keeps the order events webhooks are
                // sent from.
                "order_events" => Group::Webhooks,
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
            Group::TorService | Group::TorVisitors => SettingsTab::Tor,
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
            Group::PublicAddress => "Public addresses".into(),
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
            Group::TorService => "Onion service".into(),
            Group::TorVisitors => "Visitors on Tor".into(),
            Group::Server(o) | Group::Logging(o) | Group::Other(o) => owner(o).into(),
        }
    }

    /// What the card is for, under its heading, where its name doesn't say.
    fn hint(self) -> Option<&'static str> {
        match self {
            Group::Orders => Some("Defaults for new stores. Each store's own settings win for its orders."),
            Group::ExchangeRates => Some("Which providers stores may price fiat orders with. Each store still chooses whether to use one, and in what order."),
            Group::AbuseLimits => Some("Requests a minute, per visitor, merchant or shop."),
            Group::PublicAddress => Some("Each visitor gets links on the address they came in on: web visitors the web address, visitors on Tor the onion address."),
            Group::Webhooks => Some("How stores' webhooks are sent, and how long the engine keeps the order events they're sent from."),
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
            Group::TorService => f.write_str("tor-service"),
            Group::TorVisitors => f.write_str("tor-visitors"),
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
    "webhooks.allow_private_urls",
    "webhooks.keep_delivered_days",
    "webhooks.keep_given_up_days",
    "order_events.retention_days",
    "abuse.soft_per_min",
    "abuse.hard_per_min",
    "abuse.stream_cap",
    "abuse.client_logs_per_min",
    "abuse.signed_in_per_min",
    "rate_limit.per_store_key_per_min",
    "tor.mode",
    "tor.state_dir",
    "tor.hidden_service_dir",
    "abuse.onion_listener",
    "server.bind",
    "server.worker_threads",
    "server.cpus",
    "server.nice",
    "server.rate_limit_per_token_per_min",
    "server.instance_rate_limit",
    "server.instance_rate_limit_per_min",
];

/// The name, and the help in place of its description, the page gives an
/// engine setting whose key alone reads badly; any other is named after its
/// key ([`crate::http::admin_settings`]) and helped by its description.
pub fn engine_setting_words(key: &str) -> Option<(&'static str, Option<&'static str>)> {
    match key {
        "server.rate_limit_per_token_per_min" => Some(("Requests a minute for each store", None)),
        "server.instance_rate_limit" => Some((
            "Limit monokulo's own requests",
            Some("Off when the engine runs inside monokulo: only monokulo can call it."),
        )),
        "server.instance_rate_limit_per_min" => {
            Some(("Requests a minute for monokulo's own calls", None))
        }
        _ => None,
    }
}

/// A setting that only means something while a switch in its card is on:
/// its key, and the switch's. While the switch is off its control is
/// disabled, so it isn't sent and a save leaves it as it is;
/// `admin-settings.js` enables it as the switch turns on.
const NEEDS_SWITCH: &[(&str, &str)] = &[(
    "server.instance_rate_limit_per_min",
    "server.instance_rate_limit",
)];

/// The switch `field` needs, among the card's `fields`: its control's id,
/// and whether it is on.
fn needed_switch(
    field: &AdminScalarFieldView,
    fields: &[&AdminScalarFieldView],
) -> Option<(String, bool)> {
    let (_, switch) = NEEDS_SWITCH.iter().find(|(key, _)| *key == field.key)?;
    let switch = fields.iter().find(|f| f.key == *switch)?;
    Some((field_id(switch.form_name()), switch.value == "true"))
}

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
    /// monokulo's options file, then the engine's (when it answered).
    pub options_files: Vec<OptionsFileView>,
    /// What the Tor tab says about the onion service, above its cards;
    /// `None` while Tor is off.
    pub tor: Option<TorPanel>,
}

/// The onion service, as the Tor tab shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum TorPanel {
    /// Tor inside monokulo, and what it is doing.
    Inside(crate::tor::TorStatus),
    /// An external tor serving `onion_uri`.
    External { onion_uri: String },
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

    /// Whether `group`'s "Saved" takes focus: the first card a save this
    /// page answers saved.
    fn focus_saved(&self, group: Group) -> bool {
        self.answers_save
            && matches!(&self.outcome, Some(SaveOutcome::Saved { groups, .. }) if groups.first() == Some(&group))
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
    scalar_input_needing(field, None)
}

/// [`scalar_input`], disabled while the switch it needs is off (a whole
/// number's only: the one kind [`NEEDS_SWITCH`] holds).
fn scalar_input_needing(field: &AdminScalarFieldView, needs: Option<(String, bool)>) -> Markup {
    let name = field.form_name();
    let id = field_id(name);
    let help = help_id(field);
    // A refused value shown again carries the saved one, for the page's
    // script to tell it's still unsaved.
    let saved = field.saved_value.as_deref();
    match &field.kind {
        SettingKindView::Integer { min, max } => html! {
            (ui::Input::new(name)
                .kind(ui::InputType::Number)
                .value(field.value.as_str())
                .maybe_attr("min", min.map(|min| min.to_string()))
                .maybe_attr("max", max.map(|max| max.to_string()))
                .attr("step", "1")
                .id(id)
                .maybe_described_by(help)
                .maybe_attr("data-saved", saved)
                .disabled(needs.as_ref().is_some_and(|(_, on)| !on))
                .maybe_attr("data-needs-switch", needs.map(|(switch, _)| switch)))
        },
        SettingKindView::Bool => super::controls::switch(
            name,
            &id,
            field.value == "true",
            help.as_deref(),
            saved.map(|saved| saved == "true"),
        ),
        SettingKindView::Choice { choices } => html! {
            (Select::new(
                name,
                choices
                    .iter()
                    .map(|choice| Choice::new(choice, choice).selected(&field.value == choice))
                    .collect::<Vec<_>>(),
            )
            .id(id.as_str())
            .maybe_attr("aria-describedby", help)
            .maybe_attr("data-saved", saved))
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
                    (ui::Checkbox::new()
                        .text(choice.as_str())
                        .name(name)
                        .value(choice.as_str())
                        .checked(chosen.contains(&choice.as_str()))
                        .maybe_attr("data-saved", was.as_ref().map(|was| if was.contains(&choice.as_str()) { "on" } else { "off" })))
                }
            }
        }
        SettingKindView::Url => {
            html! {
                (ui::Input::new(name)
                    .kind(ui::InputType::Url)
                    .value(field.value.as_str())
                    .id(id)
                    .maybe_described_by(help)
                    .maybe_attr("data-saved", saved))
            }
        }
        SettingKindView::Json => {
            html! {
                (ui::TextArea::new(name)
                    .rows(4usize)
                    .value(field.value.as_str())
                    .id(id)
                    .maybe_described_by(help)
                    .maybe_attr("data-saved", saved))
            }
        }
        SettingKindView::TimeLimit {
            now,
            until_label,
            saved_until_label,
        } => {
            let until: u64 = field.value.trim().parse().unwrap_or(0);
            let on = until > *now;
            // The saved time too, when a refused save shows another: Discard
            // puts it back, so it needs an option to select.
            let saved_on = saved
                .and_then(|saved| saved.trim().parse::<u64>().ok())
                .filter(|saved| *saved > *now && *saved != until)
                .zip(saved_until_label.as_deref());
            let mut choices = Vec::new();
            if on {
                choices.push(
                    Choice::new(until.to_string(), format!("On until {until_label}"))
                        .selected(true),
                );
            }
            if let Some((saved, label)) = saved_on {
                choices.push(Choice::new(saved.to_string(), format!("On until {label}")));
            }
            // Off, a time already past posts as it is, so it isn't read as a
            // change.
            choices.push(
                Choice::new(
                    (if on || until == 0 { 0 } else { until }).to_string(),
                    "Off",
                )
                .selected(!on),
            );
            for (hours, label) in [
                (1, "On for 1 hour"),
                (4, "On for 4 hours"),
                (24, "On for 24 hours"),
            ] {
                choices.push(Choice::new((now + hours * 3600).to_string(), label));
            }
            html! {
                (Select::new(name, choices)
                    .id(id.as_str())
                    .maybe_attr("aria-describedby", help)
                    .maybe_attr("data-saved", saved))
            }
        }
        _ => {
            html! {
                (ui::Input::new(name)
                    .value(field.value.as_str())
                    .id(id)
                    .maybe_described_by(help)
                    .maybe_attr("data-saved", saved))
            }
        }
    }
}

/// A setting's name with the chip saying where its value comes from, then
/// what it's for, then its control, then anything more about it. A list of
/// choices is a group of checkboxes, named by its label.
fn scalar_field(field: &AdminScalarFieldView) -> Markup {
    scalar_field_needing(field, None)
}

/// [`scalar_field`] for a setting that needs a switch (`needs`: the
/// switch's id, and whether it is on; see [`NEEDS_SWITCH`]).
fn scalar_field_needing(field: &AdminScalarFieldView, needs: Option<(String, bool)>) -> Markup {
    let help = help_id(field);
    let help_text = field.help.as_ref().map(|help| html! { (help) });
    let control_id = field_id(field.form_name());
    let group_id = format!("{control_id}-label");
    let list = matches!(field.kind, SettingKindView::ChoiceList { .. }) && field.locked.is_none();
    let mut setting = if list {
        Field::group(&field.label, &group_id)
    } else {
        Field::new(&field.label, &control_id)
    }
    .chip(source_chip(field.source));
    if let Some(text) = help_text {
        setting = setting.help(help.as_deref(), text);
    }
    setting.render(html! {
        @if list {
            div class="setting-choices" { (scalar_input(field)) }
        // A secret the page may set is asked for, never shown.
        } @else if field.locked.is_none() && field.stored_secret {
            (secret_input(field))
        // Any other secret comes from the environment only, and is never
        // shown: always locked.
        } @else if field.locked.is_some() || field.kind == SettingKindView::Secret {
            (locked_input(field))
        } @else {
            (scalar_input_needing(field, needs))
        }
        @if let Some(reason) = &field.locked {
            (ui::FieldNote::new(reason))
        }
        (field_status(field))
    })
}

/// A secret the page may set: whether one is set (never its value), an
/// empty password box whose new value replaces it on save (left empty, it
/// stays as it is), and, while one is set, a box that removes it.
fn secret_input(field: &AdminScalarFieldView) -> Markup {
    let set = !field.value.is_empty();
    let input = ui::Input::new(field.form_name())
        .kind(ui::InputType::Password)
        .id(field_id(field.form_name()))
        .autocomplete("off")
        .placeholder(if set {
            "Enter a new key to replace it"
        } else {
            "Paste your API key"
        })
        .maybe_described_by(help_id(field));
    html! {
        @if set {
            (ui::FieldNote::new("A key is set. It isn't shown again: enter a new one to replace it, or remove it."))
        } @else {
            (ui::FieldNote::new("No key is set."))
        }
        (input)
        @if set {
            (ui::Checkbox::new()
                .text("Remove the key")
                .name(secret_remove_name(&field.key))
                .id(format!("{}-remove", field_id(field.form_name()))))
        }
    }
}

/// A setting the page can't change (given on the command line or in the
/// environment, or kept in a file it can't write): its value in a disabled
/// box with a padlock inside, and no `name`, so it is never sent with the
/// form. A secret shows as dots, never its value.
fn locked_input(field: &AdminScalarFieldView) -> Markup {
    let locked = ui::LockedValue::new(field.value.as_str())
        .id(field_id(field.form_name()))
        .secret(matches!(field.kind, SettingKindView::Secret));
    html! {
        (match help_id(field) {
            Some(help) => locked.described_by(help),
            None => locked,
        })
    }
}

/// Under a setting's control: an example, whether it waits for a restart,
/// and anything wrong with it.
fn field_status(field: &AdminScalarFieldView) -> Markup {
    html! {
        @if let Some(example) = &field.example {
            (ui::FieldNote::new(html! { "Example: " code { (example) } }))
        }
        @if field.restart_only {
            (ui::FieldNote::new("Applies after a restart."))
        }
        @if field.pending_restart {
            (ui::FieldNote::new("Saved - restart needed for it to take effect.").tone(ui::NoteTone::Pending))
        }
        @if let Some(problem) = &field.problem {
            (ui::FieldNote::new(problem).tone(ui::NoteTone::Problem))
        }
    }
}

fn notices(items: &[Notice]) -> Markup {
    html! {
        @for notice in items {
            @match notice {
                Notice::Success(text) => (ui::Alert::new(text.as_str(), ui::Tone::Success)),
                Notice::Error(text) => (ui::Alert::new(text.as_str(), ui::Tone::Error)),
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
    super::settings::toast_region(data.toast.as_ref(), oob)
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
                    (ui::Panel::new(html! {
                        form class="options-file" method="post" action=(crate::base::url("/admin/settings/reload")) {
                            input type="hidden" name="tab" value=(data.tab.id());
                            input type="hidden" name="owner" value=(match file.owner {
                                SettingOwner::Monokulo => "monokulo",
                                SettingOwner::Engine => "engine",
                            });
                            div class="options-file-where" {
                                span class="options-file-path" { (icon(file_icon_paths())) code { (file.path) } }
                                (ui::Hint::new(html! {
                                    @if !file.writable {
                                        "The options file of " (process) ". It can't be written by " (process)
                                        ", so its settings are locked here: edit the file, then reload it."
                                    } @else if !file.exists {
                                        "The options file of " (process) ". Not created yet: saving a setting here creates it."
                                    } @else {
                                        "The options file of " (process) ". Saving here writes it; after editing it by hand, reload it."
                                    }
                                }))
                            }
                            (ui::Button::new(ui::Slot::many([
                                icon(html! {
                                    path d="M20 11a8 8 0 1 0-2.3 5.7" {}
                                    path d="M20 4v7h-7" {}
                                })
                                .into(),
                                "Reload options file".into(),
                            ]))
                            .kind(ui::ButtonType::Submit))
                        }
                    })
                    .look(ui::PanelLook::Tile))
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
        (ui::Alert::new(
            format!("Could not reach the configured engine: {}", data.engine_error.as_deref().unwrap_or("")),
            ui::Tone::Error,
        ))
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
/// shows while a setting nobody placed is in it, and Files, while there is
/// an options file.
fn tab_shown(data: &AdminSettingsViewModel, tab: SettingsTab) -> bool {
    match tab {
        SettingsTab::Other => !tab_fields(data, tab).is_empty(),
        SettingsTab::Files => !data.options_files.is_empty(),
        _ => true,
    }
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
        nav id="settings-tabs" class="tab-bar" aria-label="Settings sections" data-leave-asks data-fx-oob[oob] {
            @for tab in SettingsTab::ALL.into_iter().filter(|tab| tab_shown(data, *tab)) {
                @let href = crate::base::url(tab.href());
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
    }
}

/// One node row, an item of its network's sortable list, closed to a line
/// until it's opened: a handle to drag it by (with JavaScript), its place (Primary, Fallback N), its address and
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
) -> ui::SortableItem {
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
    let saved = row.saved.as_ref();
    let saved_text = |f: fn(&crate::admin_nodes::NodeRow) -> &str| saved.map(|s| f(s).to_string());
    let saved_box = |f: fn(&crate::admin_nodes::NodeRow) -> bool| {
        saved.map(|s| if f(s) { "on" } else { "off" })
    };
    let summary = html! {
        @if position.is_some() {
            (ui::SortHandle::new(format!("Reorder {place}: press the up or down arrow")).flag("data-node-handle"))
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
    };
    let address_help = id("address-help");
    let ssl_help = id("ssl-help");
    let self_signed_help = id("self_signed-help");
    let zmq_help = id("zmq_pub-help");
    let body = html! {
        (ui::Setting::new("Address", id("address"), html! {
            (ui::Input::new(name("address"))
                .id(id("address"))
                .value(row.row.address.as_str())
                .described_by(described)
                .maybe_attr("data-saved", saved_text(|s| &s.address))
                .invalid(row.row.error.is_some())
                .autocomplete("off")
                .attr("spellcheck", "false")
                .attr("inputmode", "url"))
            @if let (Some(error), Some(error_id)) = (&row.row.error, &error_id) {
                (ui::FieldNote::new(error).tone(ui::NoteTone::Problem).id(error_id))
            }
        })
        .help(html! { "The node's host and port, like " code { (example) } ". "
            code { "http://" } " or " code { "https://" } " in front is fine (" code { "https://" } " also ticks Use TLS); "
            "an IPv6 address goes in brackets, like " code { "[::1]:18081" } "." })
        .help_id(address_help))
        (ui::Setting::new("Use TLS", id("ssl"), ui::Checkbox::new()
            .name(name("ssl"))
            .id(id("ssl"))
            .checked(row.row.ssl)
            .described_by(ssl_help.as_str())
            .flag("data-node-tls")
            .maybe_attr("data-saved", saved_box(|s| s.ssl)))
        .help("Connect with TLS (https). Off by default; most nodes on port 18081 or 18089 don't use it.")
        .help_id(ssl_help))
        // Shown only while Use TLS is ticked, with JavaScript.
        (ui::Setting::new("Accept a self-signed certificate", id("self_signed"), ui::Checkbox::new()
            .name(name("self_signed"))
            .id(id("self_signed"))
            .checked(row.row.self_signed)
            .described_by(self_signed_help.as_str())
            .maybe_attr("data-saved", saved_box(|s| s.self_signed)))
        .help("Many community nodes use a self-signed TLS certificate; tick this to accept one. Only used with TLS.")
        .help_id(self_signed_help)
        .flag("data-node-self-signed"))
        (ui::Setting::new("Announcements (ZMQ)", id("zmq_pub"), ui::Input::new(name("zmq_pub"))
            .id(id("zmq_pub"))
            .value(row.row.zmq_pub.as_str())
            .described_by(zmq_help.as_str())
            .maybe_attr("data-saved", saved_text(|s| &s.zmq_pub))
            .autocomplete("off")
            .attr("spellcheck", "false")
            .attr("inputmode", "url"))
        .help(html! { "Optional, for your own node: the address it was started with as " code { "--zmq-pub" } ", like "
            code { "tcp://127.0.0.1:18083" } ". Payments are then seen the moment the node sees them, instead of at the next check. "
            "Needs an engine built with ZMQ support; leave empty otherwise." })
        .help_id(zmq_help))
        @if let Some(status) = &row.status { (node_status(status)) }
        @if let Some((at, count)) = position {
            @let action = |label: &str, value: String| {
                ui::Button::new(label).kind(ui::ButtonType::Submit).attr("name", "node_action").attr("value", value)
            };
            (ui::FormActions::new(html! {
                @if at > 0 {
                    (action("Move up", format!("up:{n}:{index}")).attr("data-only", "no-script"))
                }
                @if at + 1 < count {
                    (action("Move down", format!("down:{n}:{index}")).attr("data-only", "no-script"))
                }
                (action("Remove", format!("remove:{n}:{index}")).flag("data-node-remove"))
            }))
        }
    };
    ui::SortableItem::new(
        ui::Disclosure::new(summary, body)
            .look(ui::DisclosureLook::Row)
            .open(row.row.error.is_some()),
    )
    .fixed(position.is_none())
    .attr("data-node-row", index.to_string())
    .maybe_attr("data-node-add", position.is_none().then_some(""))
    .maybe_attr("data-node-saved", row.saved_index.map(|at| at.to_string()))
}

/// `stagenet` as `Stagenet`, for a heading.
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
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

/// A card as the save left it: its owner chip and any meta in its head,
/// "Not saved" after a refusal, "Restart needed", "Saved" and when, and
/// (with JavaScript) its unsaved changes and Discard.
fn card<'a>(
    data: &AdminSettingsViewModel,
    group: Group,
    name: &'a str,
    owner: &str,
    meta: Option<String>,
    fields: &[&AdminScalarFieldView],
) -> Card<'a> {
    let restart = fields.iter().any(|f| f.pending_restart);
    Card::new(name, group.title())
        .head(html! {
            span class="owner-chip" { (owner) }
            @if let Some(meta) = meta { span class="card-meta" { (meta) } }
        })
        .badge(restart.then(
            || html! { span class="mk-status-text" data-tone="warning" { "Restart needed" } },
        ))
        .failed(data.failure(group).is_some(), None)
        .saved(data.saved_at(group), data.focus_saved(group))
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
                (ui::Hint::new(html! {
                    "What this site's forms check before they encrypt a merchant's keys. They must match the engine's settings above: "
                    "they are checked against the engine when saved, and the status page shows an alert if they ever differ."
                }))
            }
            @if let Some(heading) = field_subheading(&field.key) { h4 { (heading) } }
            (scalar_field_needing(field, needed_switch(field, fields)))
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
    let name = group.to_string();
    let failure = data.failure(group);
    let mut card = card(
        data,
        group,
        &name,
        owner_label(&fields, data),
        None,
        &fields,
    )
    .failed(failure.is_some(), failure.map(|f| f.message.as_str()))
    .readonly(readonly)
    .hidden(hidden);
    if let Some(backend) = backend {
        card = card.shown_by(ENABLED_BACKENDS, backend);
    }
    card.render(html! {
        @if let Some(hint) = group.hint() { (ui::Hint::new(hint)) }
        (card_fields(group, &fields, data))
    })
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
    let blank = NodeRowView {
        row: crate::admin_nodes::NodeRow {
            self_signed: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let rows = html! {
        @if let Some(error) = &network.error {
            (ui::Alert::new(error.as_str(), ui::Tone::Error))
        } @else if let Some(failure) = failure {
            (ui::Alert::new(failure.message.as_str(), ui::Tone::Error))
        }
        (ui::Hint::new(html! {
            "Fallbacks are tried in order when the one before fails. Open a node to change it"
            span data-only="script" { ", or drag it by its handle to reorder it" } "."
        }))
        (ui::SortableList::new(
            format!("{} nodes", capitalized(n)),
            ui::Slot::many(
                network
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(at, row)| node_row(network, at, row, Some((at, count))))
                    .chain([node_row(network, count, &blank, None)])
                    .map(ui::Slot::from),
            ),
        )
        .attr("data-node-rows", n.as_str())
        // admin-settings.js counts a row's changes once, as the row's.
        .flag("data-counted-by-page"))
        div class="node-add-another" data-only="script" { (ui::Button::new("Add another").attr("data-node-add-another", n.as_str())) }
        // Whether this network's blocks' proof of work is checked
        // (`proof_of_work.<network>`).
        @for field in &fields { (scalar_field(field)) }
    };
    let open =
        count > 0 || network.tenant_count > 0 || network.error.is_some() || failure.is_some();
    let name = group.to_string();
    // An error the engine reports for the network marks the card too,
    // without the "Not saved" a refused save gives it.
    let class = if failure.is_none() && network.error.is_some() {
        "node-network is-failed"
    } else {
        "node-network"
    };
    card(data, group, &name, "Engine", Some(used_by), &fields)
        .class(class)
        .data("data-network", n)
        .data("data-tenant-count", network.tenant_count)
        .data("data-saved-count", network.saved_count)
        // How the network's scan and its node in use are doing is on the
        // engine page.
        .link(html! {
            a class="engine-link" href=(crate::base::url(format!("/status/engine?network={n}"))) { "See it on the engine page" }
        })
        .render(html! {
            @if open {
                (rows)
            } @else {
                (ui::Disclosure::new(format!("Add a node for {n}"), rows).flag("data-node-network-closed"))
            }
        })
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
    let message = html! {
        @if let Some(failure) = failure {
            strong { @if partly { "Changes partly saved." } @else { "Nothing saved." } } " " (failure.message)
            @if let Some(group) = failure.group {
                " " a href=(format!("#{}", group.card_id())) data-show-card=(group) { "Show" }
            }
        } @else {
            "Saving writes the changes on this tab to the options file and applies them."
        }
    };
    super::settings::save_bar(
        failure.is_some(),
        data.answers_save && failure.is_some(),
        message,
        &data.tab.href(),
    )
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
            @if tab == SettingsTab::Files {
                (ui::Hint::new("Each process keeps its settings in an options file. After editing one by hand, reload it here to apply it."))
                (options_file_bars(data))
            } @else {
                (settings_tab(data, tab))
            }
        }
    }
}

/// The onion service's state, above the Tor tab's cards.
fn tor_panel(panel: &TorPanel) -> Markup {
    use crate::tor::TorStatus;
    let address = |address: &str| {
        let uri = format!("http://{address}");
        html! {
            (ui::CopyValue::new(uri.as_str()).copy_label("Copy the onion address"))
        }
    };
    let alert = match panel {
        TorPanel::External { onion_uri } => ui::Alert::new(html! {
            "Visitors on Tor are sent to "
            (ui::CopyValue::new(onion_uri.as_str()).copy_label("Copy the onion address"))
        }, ui::Tone::Notice)
        .title("An external tor serves the onion address.")
        .icon(html! { (PreEscaped(ui::assets::icons::INFO)) }),
        TorPanel::Inside(TorStatus::Running { address: onion, .. }) => ui::Alert::new(html! {
            "Visitors on Tor reach this instance at " (address(onion))
        }, ui::Tone::Success)
        .title("Onion service running.")
        .icon(html! { (PreEscaped(ui::assets::icons::CHECK)) }),
        TorPanel::Inside(TorStatus::Publishing { address: onion }) => ui::Alert::new(html! {
            "Tor is telling the network about " (address(onion)) ". It is usually reachable within a minute; Tor marks it running once every directory has it."
        }, ui::Tone::Notice)
        .title("Publishing the onion address…")
        .icon(html! { (PreEscaped(ui::assets::icons::CLOCK)) }),
        TorPanel::Inside(TorStatus::Degraded { address: onion, problem }) => ui::Alert::new(html! {
            "Tor is still trying to keep " (address(onion)) " reachable: " (problem)
        }, ui::Tone::Warning)
        .title("Onion service may be unreachable.")
        .icon(html! { (PreEscaped(ui::assets::icons::WARNING)) }),
        TorPanel::Inside(TorStatus::Starting { percent, doing }) => ui::Alert::new(html! {
            (doing) " (" (percent) "%). A first start takes up to a minute; later starts are quicker. The web address isn't affected."
        }, ui::Tone::Notice)
        .title("Starting Tor…")
        .icon(html! { (PreEscaped(ui::assets::icons::CLOCK)) }),
        TorPanel::Inside(TorStatus::Failed { error, .. }) => ui::Alert::new(html! {
            (error) " It starts again by itself every 30 seconds; the web address isn't affected."
        }, ui::Tone::Error)
        .title("Tor stopped.")
        .icon(html! { (PreEscaped(ui::assets::icons::WARNING)) }),
        TorPanel::Inside(TorStatus::Off) => ui::Alert::new(
            "Save to start Tor inside monokulo.",
            ui::Tone::Notice,
        )
        .title("Tor isn't running yet."),
    };
    html! { div id="tor-status" { (alert.block(true).quiet(true)) } }
}

/// A tab of settings: its cards in the settings form, and the save bar.
fn settings_tab(data: &AdminSettingsViewModel, tab: SettingsTab) -> Markup {
    html! {
        (ui::Hint::new("Saving writes the options file and applies the change straight away. A setting given on the command line or in the environment is locked here: change it where it is given."))
        @if tab == SettingsTab::Abuse {
            (ui::Hint::new(html! {
                "How this instance tells visitors apart and slows down anyone sending too many requests. A visitor "
                "past the soft limit is asked to pass a short check (automatic with JavaScript, a 10-second wait "
                "without); past the hard limit they're refused until the minute is up. Signed-in merchants and "
                "plugins using their store's secret key are never checked."
            }))
        }
        @if tab == SettingsTab::Tor {
            (ui::Hint::new("Monokulo can serve an onion address beside its web address. Visitors on Tor reach the same stores and checkouts without leaving Tor; everyone else keeps using the web address."))
            @if let Some(panel) = &data.tor {
                (tor_panel(panel))
            }
        }
        @if tab == SettingsTab::Nodes {
            (ui::Hint::new(html! {
                "The Monero nodes the engine reads each network's chain from: a primary, and fallbacks tried when it fails. "
                "A network with no nodes isn't used. A node that doesn't answer is still saved; one on another network is refused."
            }))
        }
        @if tab.engine_only() && !engine_available(data) {
            (engine_unavailable(data))
        } @else {
            (super::settings::form("/admin/settings", Save::Fixi { target: "#settings-panel" }, tab.label(), html! {
                input type="hidden" name="tab" value=(tab.id());
                @if tab == SettingsTab::Nodes {
                    // Enter in a text box presses a form's first submit
                    // button; here that would be a row's Move or Remove.
                    // This one, first and out of sight, is Save.
                    span class="visually-hidden" {
                        (ui::Button::new("Save").kind(ui::ButtonType::Submit).attr("tabindex", "-1").attr("aria-hidden", "true"))
                    }
                }
                (tab_cards(data, tab))
                (save_bar(data))
            }))
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

/// The question a save asks when it leaves a network stores use with no
/// node (`admin-settings.js` fills in which, opens it, and saves on Save
/// anyway). Without JavaScript the save goes through and the page says so.
fn node_clear_dialog() -> Markup {
    ui::Dialog::new(
        "node-clear-dialog",
        "node-clear-title",
        html! {
            (ui::DialogTitle::new("node-clear-title", "Save without a node?"))
            p data-node-clear-text {}
            (ui::DialogActions::new(html! {
                (ui::Button::new("Keep editing").attr("data-closes-dialog", ""))
                (ui::Button::new("Save anyway").variant(ui::ButtonVariant::Danger).attr("data-node-clear-go", ""))
            }))
        },
    )
    .render()
}

pub fn admin_settings_page(chrome: &PageChrome, data: &AdminSettingsViewModel) -> Markup {
    let header = ui::PageHeader::new("Admin settings").crumbs(super::crumbs(&[("/", "Home")]));
    let body = html! {
        (banners(data, false))
        (tab_bar(data, false))
        (settings_panel(data, false))
        (toasts(data, false))
        (node_clear_dialog())
        (script("admin-settings.js", Load::Now))
    };
    super::Page::new(
        format!("{} - Admin settings - Monokulo", data.tab.label()),
        header,
    )
    .render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/admin/settings")
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
            ("public_uri", M, General, "public-address"),
            ("onion_uri", M, General, "public-address"),
            ("tor.mode", M, Tor, "tor-service"),
            ("tor.state_dir", M, Tor, "tor-service"),
            ("tor.hidden_service_dir", M, Tor, "tor-service"),
            ("tor.onion_location", M, Tor, "tor-visitors"),
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
                "exchange_rate.coinmarketcap_api_key",
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
            ("webhooks.allow_private_urls", M, Payments, "webhooks"),
            ("webhooks.delivery_timeout_ms", M, Payments, "webhooks"),
            ("webhooks.max_attempts", M, Payments, "webhooks"),
            ("webhooks.keep_delivered_days", M, Payments, "webhooks"),
            ("webhooks.keep_given_up_days", M, Payments, "webhooks"),
            ("engine.mode", M, General, "engine"),
            ("engine.url", M, General, "engine"),
            ("engine.status_cache_ms", M, General, "engine"),
            ("engine.token", M, General, "engine"),
            ("http_cache.max_mb", M, Server, "server-monokulo"),
            ("database.read_connections", M, Server, "server-monokulo"),
            ("database.path", M, Server, "server-monokulo"),
            ("server.bind", M, Server, "server-monokulo"),
            ("crypto.encryption_key", M, Server, "server-monokulo"),
            ("admin.query_enabled", M, Server, "server-monokulo"),
            ("logging.format", M, Logging, "logging-monokulo"),
            ("abuse.soft_per_min", M, Abuse, "abuse-limits"),
            ("abuse.hard_per_min", M, Abuse, "abuse-limits"),
            ("abuse.signed_in_per_min", M, Abuse, "abuse-limits"),
            ("abuse.client_logs_per_min", M, Abuse, "abuse-limits"),
            ("abuse.challenge_bits", M, Abuse, "abuse-challenge"),
            ("abuse.under_attack", M, Abuse, "abuse-challenge"),
            ("abuse.trusted_proxies", M, Abuse, "abuse-visitors"),
            ("abuse.onion_listener", M, Tor, "tor-service"),
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
            ("order_events.retention_days", E, Payments, "webhooks"),
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
            ("server.instance_rate_limit", E, Server, "server-engine"),
            (
                "server.instance_rate_limit_per_min",
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

        // Other only appears while a setting nobody placed is in it.
        {
            let html = page(&full_view(SettingsTab::General));
            assert!(!html.contains("tab=other"), "{html}");
            let mut data = full_view(SettingsTab::Other);
            data.engine_fields
                .push(field_for("telemetry.sample_rate", SettingOwner::Engine));
            let html = page(&data);
            assert!(html.contains(&tab_link(SettingsTab::Other, true)), "{html}");
            assert!(html.contains(r#"name="telemetry.sample_rate""#), "{html}");
        }
    }

    #[test]
    fn a_tab_is_found_by_its_id_and_anything_else_is_general() {
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::Files)
        {
            assert_eq!(SettingsTab::from_id(Some(tab.id())), tab);
        }
        assert_eq!(SettingsTab::from_id(None), SettingsTab::General);
        assert_eq!(SettingsTab::from_id(Some("nope")), SettingsTab::General);
        assert_eq!(SettingsTab::Nodes.href(), "/admin/settings?tab=nodes");
        assert!(SettingsTab::Nodes.engine_only());
        assert!(
            !SettingsTab::Custody.engine_only()
                && !SettingsTab::Payments.engine_only()
                && !SettingsTab::Server.engine_only()
                && !SettingsTab::Logging.engine_only()
        );
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
            saved: None,
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

    #[test]
    fn each_tab_shows_only_its_own_settings() {
        for tab in SettingsTab::ALL
            .into_iter()
            .filter(|tab| *tab != SettingsTab::Files)
        {
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
                    r#"<button type="submit" class="mk-btn" data-variant="primary" data-save data-mk="6"><!--mk:7-->Save<!--/mk:7-->"#
                )
                .count(),
                1,
                "{tab:?}"
            );
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
                .filter(|t| !matches!(t, SettingsTab::Other | SettingsTab::Files))
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
            .filter(|tab| !matches!(tab, SettingsTab::Other | SettingsTab::Files))
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
                        && html.contains(r#"class="mk-btn" data-variant="primary" data-save data-mk="6"><!--mk:7-->Save<!--/mk:7-->"#),
                    "{tab:?}"
                );
                assert!(
                    !html.contains(">Orders</h3>"),
                    "one message, not a card per group: {html}"
                );
            }
        }
        let payments = page(&unreachable(SettingsTab::Payments));
        assert!(
            payments.contains(r#"name="exchange_rate.cache_seconds""#),
            "{payments}"
        );
        // Webhooks are monokulo's own: their card stays, without the
        // engine's retention.
        assert!(
            payments.contains(r#"name="webhooks.max_attempts""#),
            "{payments}"
        );
        assert!(!payments.contains(r#"name="order_events.retention_days""#));
        let logging = page(&unreachable(SettingsTab::Logging));
        assert!(
            logging.find(">Monokulo</h3>").unwrap() < logging.find("Could not reach").unwrap()
                && !logging.contains(">Engine</h3>"),
            "{logging}"
        );
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
            boolean.contains(r#"role="switch""#) && !boolean.contains("checked"),
            "an on/off setting is a switch, off here: {boolean}"
        );
        let on = scalar_field(&field(SettingKindView::Bool, "true")).into_string();
        assert!(on.contains("checked"), "{on}");
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

    /// A card, by id, as the page renders it: from its start to its end.
    fn card<'a>(html: &'a str, group: &str) -> &'a str {
        let start = html
            .find(&format!(r#"<section id="card-{group}""#))
            .unwrap_or_else(|| panic!("card-{group} in {html}"));
        let rest = &html[start..];
        &rest[..rest.find("</section>").unwrap()]
    }

    #[test]
    fn a_refused_card_is_red_says_why_and_shows_what_was_typed() {
        let mut data = full_view(SettingsTab::Payments);
        let attempts = data
            .engine_fields
            .iter_mut()
            .find(|f| f.key == "order_events.retention_days")
            .unwrap();
        attempts.value = "900".into();
        attempts.saved_value = Some("7".into());
        attempts.problem = Some("must be from 1 to 365".into());
        let failures = vec![Failure {
            group: Some(Group::Webhooks),
            message:
                "The engine refused the change: order_events.retention_days must be from 1 to 365."
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
            webhooks.contains(r#"<span class="mk-status-text" data-tone="error" data-card-failed data-mk="3">Not saved</span>"#),
            "{webhooks}"
        );
        assert!(webhooks.contains(r#"<p class="error" role="alert" data-mk="6">The engine refused the change: order_events.retention_days must be from 1 to 365.</p>"#), "{webhooks}");
        // What was typed, still to fix, with the saved value for Discard.
        assert!(
            webhooks.contains(r#"name="order_events.retention_days" id="setting-order_events.retention_days" value="900""#),
            "{webhooks}"
        );
        assert!(webhooks.contains(r#"data-saved="7""#), "{webhooks}");
        assert!(webhooks.contains("must be from 1 to 365"), "{webhooks}");
        assert!(!webhooks.contains("Saved 8 Oct"), "{webhooks}");

        // The card that was saved says when; the others say nothing.
        assert!(card(&html, "exchange-rates").contains(
            r#"<span class="card-meta card-saved" data-card-saved data-mk="4">Saved 8 Oct, 14:22</span>"#
        ));
        assert!(!card(&html, "orders").contains("Saved 8 Oct"));

        // The bar is red, says why, links to the card, and takes focus.
        assert!(
            html.contains(r#"<div id="save-bar" class="save-bar is-failed""#),
            "{html}"
        );
        assert!(html.contains(r##"<p class="save-bar-message" data-save-bar-message tabindex="-1" data-fx-focus><span data-mk="2"><strong>Changes partly saved.</strong> The engine refused the change: order_events.retention_days must be from 1 to 365. <a href="#card-webhooks" data-show-card="webhooks">Show</a></span><!--mk:3--></p>"##), "{html}");
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
        assert!(html.contains(r#"data-fx-focus><span data-mk="2"><strong>Nothing saved.</strong> Could not reach the configured engine: connection refused</span><!--mk:3--></p>"#), "{html}");
    }

    /// A refused save showing development logging off while it's saved on
    /// keeps an option for the saved time, so Discard can put it back.
    #[test]
    fn a_time_limit_shown_again_keeps_the_saved_time_to_discard_to() {
        let now = 1_790_000_000;
        let field = AdminScalarFieldView {
            key: "logging.dev_mode_until".into(),
            label: "logging dev mode until".into(),
            value: "0".into(),
            saved_value: Some((now + 600).to_string()),
            stored_secret: false,
            kind: SettingKindView::TimeLimit {
                now,
                until_label: "1 Jan, 00:00".into(),
                saved_until_label: Some("21 Sep, 22:23".into()),
            },
            ..Default::default()
        };
        let html = scalar_field(&field).into_string();
        assert!(
            html.contains(r#"<option value="0" selected>Off</option>"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"<option value="{}">On until 21 Sep, 22:23</option>"#,
                now + 600
            )),
            "{html}"
        );

        // Development logging is chosen as off or a number of hours and says
        // when it ends.
        {
            let now = 1_790_000_000;
            let field = |value: &str| AdminScalarFieldView {
                key: "logging.dev_mode_until".to_string(),
                label: "logging dev mode until".to_string(),
                value: value.to_string(),
                kind: SettingKindView::TimeLimit {
                    now,
                    until_label: "21 Sep, 22:23".into(),
                    saved_until_label: None,
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
    }
}
