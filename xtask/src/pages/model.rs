//! What the pages show, worked out once from the inputs: every test with the
//! area of Monokulo it protects and the group it sits in, and the figures
//! the header leads with.

use super::fetch::Sources;
use super::format::{capitalised, sentence};
use super::inputs::{
    BrowserKind, CoverageRun, FuzzTarget, Gallery, LiveRun, Properties, Stress, Test, TestStatus,
};
use regex::Regex;
use std::sync::LazyLock;

/// The suite a test belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Suite {
    Rust,
    Browser,
    /// The nightly property run: listed with the rest, but not counted in
    /// the per-change figures.
    Property,
}

impl Suite {
    pub(super) const ALL: [Suite; 3] = [Suite::Rust, Suite::Browser, Suite::Property];

    pub(super) fn name(self) -> &'static str {
        match self {
            Suite::Rust => "Rust",
            Suite::Browser => "Browser",
            Suite::Property => "Property",
        }
    }

    /// As a tag beside an area: the property run is nightly.
    pub(super) fn tag(self) -> &'static str {
        match self {
            Suite::Property => "Property (nightly)",
            other => other.name(),
        }
    }
}

/// The part of Monokulo a test protects. A test is in the first area that
/// claims it, in this order, else in `Other`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Area {
    Payments,
    Webhooks,
    Checkout,
    Dashboard,
    Custody,
    Storage,
    Rates,
    Abuse,
    Logs,
    Woo,
    Tools,
    Other,
}

impl Area {
    pub(super) const ALL: [Area; 12] = [
        Area::Payments,
        Area::Webhooks,
        Area::Checkout,
        Area::Dashboard,
        Area::Custody,
        Area::Storage,
        Area::Rates,
        Area::Abuse,
        Area::Logs,
        Area::Woo,
        Area::Tools,
        Area::Other,
    ];

    /// Its id in links and the tests page's filters.
    pub(super) fn id(self) -> &'static str {
        match self {
            Area::Payments => "payments",
            Area::Webhooks => "webhooks",
            Area::Checkout => "checkout",
            Area::Dashboard => "dashboard",
            Area::Custody => "custody",
            Area::Storage => "storage",
            Area::Rates => "rates",
            Area::Abuse => "abuse",
            Area::Logs => "logs",
            Area::Woo => "woo",
            Area::Tools => "tools",
            Area::Other => "other",
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Area::Payments => "Payment detection",
            Area::Webhooks => "Webhooks",
            Area::Checkout => "Checkout and POS",
            Area::Dashboard => "Dashboard and settings",
            Area::Custody => "Key custody",
            Area::Storage => "Storage and recovery",
            Area::Rates => "Exchange rates",
            Area::Abuse => "Abuse protection",
            Area::Logs => "Logs and tracing",
            Area::Woo => "Shop plugins",
            Area::Tools => "Test tools",
            Area::Other => "Everything else",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Area::Payments => "Scanning blocks and the mempool, confirmations, reorgs, proof of work and the order statuses customers see.",
            Area::Webhooks => "Signed deliveries to the shop, retries and backoff, and receivers checking them.",
            Area::Checkout => "The hosted payment page with and without JavaScript, the embed, and the point-of-sale screen on phones and tablets.",
            Area::Dashboard => "Admin pages, stores, nodes, every setting and where its value comes from, and options-file handling.",
            Area::Custody => "Encrypting view keys at rest, routing between custody backends, and SEV-SNP attestation.",
            Area::Storage => "SQLite schema, single-writer guarantees, crash recovery and resuming after restarts.",
            Area::Rates => "Fiat prices from each provider, currency handling and their settings.",
            Area::Abuse => "Rate limits, client challenges and the under-attack switch.",
            Area::Logs => "Structured logs, the log viewer, trace context across engine, server and plugin.",
            Area::Woo => "A shop plugin connecting a store, taking payment and receiving its webhooks, played by the mock WooCommerce plugin.",
            Area::Tools => "The stagenet test wallet and fixtures the other suites stand on. Tested too.",
            Area::Other => "Shared types, HTTP plumbing and helpers.",
        }
    }

    /// Whether this area claims a test. `tool` says whether its crate is a
    /// test tool.
    fn claims(self, case: &Case, tool: bool) -> bool {
        static PAYMENTS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^engine::(scanner|work|status|pow|proof|node_events|daemon|scaling|exploration|loops|chain|mempool)").unwrap()
        });
        static WEBHOOKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)webhook").unwrap());
        static CHECKOUT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(checkout|pos|refund|live-view|merchant-client)").unwrap()
        });
        static DASHBOARD: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^monokulo::(http|views|admin|settings|options_file|confirmation|engine_relay|engine_client|currencies|setup|stores|db)").unwrap()
        });
        static DASHBOARD_BROWSER: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(admin|settings|node-settings|site-themes|store-key)").unwrap()
        });
        static CUSTODY_CRATES: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^(key-custody|key-custody-cli|snp-attest)$").unwrap());
        static CUSTODY: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"::(plain|router|outputs|snp)\b").unwrap());
        static STORAGE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^engine::store|engine-crash").unwrap());
        static RATES: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"exchange_rate|haveno|coinmarketcap|fx_provider|currencies").unwrap()
        });
        static ABUSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"::abuse|challenge").unwrap());
        static LOGS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"log-search|Trace|tracing|pos-session-diagnostics").unwrap()
        });
        let (group, krate, suite) = (case.group.as_str(), case.krate.as_str(), case.suite);
        match self {
            Area::Payments => krate == "engine" && PAYMENTS.is_match(group),
            Area::Webhooks => WEBHOOKS.is_match(group),
            Area::Checkout => suite == Suite::Browser && CHECKOUT.is_match(group),
            Area::Dashboard => {
                (krate == "monokulo" && DASHBOARD.is_match(group))
                    || krate == "engine-view"
                    || krate == "live-settings"
                    || (suite == Suite::Browser && DASHBOARD_BROWSER.is_match(group))
            }
            Area::Custody => CUSTODY_CRATES.is_match(krate) || CUSTODY.is_match(group),
            Area::Storage => STORAGE.is_match(group),
            Area::Rates => RATES.is_match(group),
            Area::Abuse => ABUSE.is_match(group),
            Area::Logs => krate == "telemetry" || LOGS.is_match(group),
            Area::Woo => krate == "mock-woocommerce",
            Area::Tools => tool && krate != "mock-woocommerce",
            Area::Other => true,
        }
    }
}

/// One test, as the pages list it.
pub(super) struct Case {
    pub(super) suite: Suite,
    /// The crate, or `browser`.
    pub(super) krate: String,
    /// The module or spec file it sits in.
    pub(super) group: String,
    /// The test's own name, as written.
    pub(super) leaf: String,
    /// The name as a sentence.
    pub(super) label: String,
    /// The name as the test runner reports it.
    pub(super) raw: String,
    pub(super) secs: f64,
    pub(super) status: TestStatus,
    pub(super) area: Area,
}

/// A module path without its `tests` modules: `a::tests::b` as `a::b`.
fn without_tests<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .filter(|p| *p != "tests")
        .collect::<Vec<_>>()
        .join("::")
}

/// `scanner::tests::name` as the path `scanner` and the leaf `name`.
fn split_leaf(name: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = name.split("::").collect();
    let leaf = parts.pop().unwrap_or("");
    (parts, leaf)
}

/// A property's group: its module path without the test modules.
pub(super) fn property_group(name: &str) -> String {
    without_tests(split_leaf(name).0.into_iter())
}

fn rust_case(t: &Test) -> Case {
    let krate = t.class.split("::").next().unwrap_or("").to_string();
    let (parts, leaf) = split_leaf(&t.name);
    let mut group = without_tests(parts.into_iter());
    if group.is_empty() {
        group = t
            .class
            .split_once("::")
            .map_or_else(|| "tests".to_string(), |(_, rest)| rest.to_string());
    }
    Case {
        suite: Suite::Rust,
        group: format!("{krate}::{group}"),
        krate,
        label: sentence(leaf),
        leaf: leaf.to_string(),
        raw: t.name.clone(),
        secs: t.secs,
        status: t.status,
        area: Area::Other,
    }
}

fn browser_case(t: &Test) -> Case {
    let spec = t.class.strip_suffix(".spec.js").unwrap_or(&t.class);
    let group = if t.kind == Some(BrowserKind::RealBinaries) {
        format!("{spec} (real binaries)")
    } else {
        spec.to_string()
    };
    Case {
        suite: Suite::Browser,
        krate: "browser".into(),
        group,
        label: capitalised(&t.name),
        leaf: t.name.clone(),
        raw: t.class.clone(),
        secs: t.secs,
        status: t.status,
        area: Area::Other,
    }
}

fn property_case(t: &Test) -> Case {
    let (_, leaf) = split_leaf(&t.name);
    Case {
        suite: Suite::Property,
        krate: t.class.clone(),
        group: format!("{}::{}", t.class, property_group(&t.name)),
        label: sentence(leaf),
        leaf: leaf.to_string(),
        raw: t.name.clone(),
        secs: t.secs,
        status: t.status,
        area: Area::Other,
    }
}

/// Everything the pages are rendered from.
pub(super) struct Report {
    /// The repository's web address, for links to runs and sources.
    pub(super) repo: String,
    pub(super) sources: Sources,
    pub(super) coverage: Option<CoverageRun>,
    pub(super) stress: Option<Stress>,
    pub(super) scale: Option<Stress>,
    pub(super) gallery: Option<Gallery>,
    pub(super) properties: Option<Properties>,
    pub(super) fuzz: Vec<FuzzTarget>,
    /// The daily live-network run: kept apart from `cases`, since its
    /// tests run against services Monokulo doesn't run.
    pub(super) live: Option<LiveRun>,
    /// Every test of every suite, the nightly properties included.
    pub(super) cases: Vec<Case>,
}

impl Report {
    pub(super) fn new(
        repo: String,
        sources: Sources,
        coverage: Option<CoverageRun>,
        properties: Option<Properties>,
    ) -> Self {
        let mut cases = Vec::new();
        let mut tools: Vec<&str> = Vec::new();
        if let Some(c) = &coverage {
            cases.extend(c.tests.rust.iter().map(rust_case));
            cases.extend(c.tests.browser.iter().map(browser_case));
            tools.extend(c.crates.iter().filter(|c| c.tool).map(|c| c.name.as_str()));
        }
        if let Some(p) = &properties {
            cases.extend(p.tests.iter().map(property_case));
        }
        for case in &mut cases {
            let tool = tools.contains(&case.krate.as_str());
            case.area = Area::ALL
                .into_iter()
                .find(|a| a.claims(case, tool))
                .unwrap_or(Area::Other);
        }
        Report {
            repo,
            sources,
            coverage,
            stress: None,
            scale: None,
            gallery: None,
            properties,
            fuzz: Vec::new(),
            live: None,
            cases,
        }
    }

    /// The tests that run on every change: the nightly properties left out.
    pub(super) fn per_change(&self) -> impl Iterator<Item = &Case> {
        self.cases.iter().filter(|c| c.suite != Suite::Property)
    }

    /// Whether every test, collector and fuzz campaign passed. A live
    /// test that failed while a service it needs was down doesn't count
    /// against the code, nor does a campaign whose budget ran out before
    /// it explored past the seeds.
    pub(super) fn all_passed(&self) -> bool {
        self.cases.iter().all(|c| c.status != TestStatus::Failed)
            && self.coverage.as_ref().is_none_or(|c| c.passed)
            && self.fuzz.iter().all(|f| f.status.counts_as_passed())
            && self
                .live
                .as_ref()
                .is_none_or(|l| l.failures().next().is_none())
    }

    /// The link to a workflow run on the repository.
    pub(super) fn run_url(&self, run_id: u64) -> String {
        format!("{}/actions/runs/{run_id}", self.repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(class: &str, name: &str) -> Test {
        Test {
            class: class.into(),
            name: name.into(),
            secs: 0.5,
            status: TestStatus::Passed,
            kind: None,
            project: String::new(),
            message: None,
        }
    }

    /// Where each test is listed: a Rust test by its crate and module
    /// without the tests module (an integration test binary by its file),
    /// a browser test by its spec and how it ran, each under the first area
    /// that claims it.
    #[test]
    fn each_test_lands_in_the_first_area_that_claims_it() {
        // (class, name, browser kind, crate, group, label)
        type Row<'a> = (
            &'a str,
            &'a str,
            Option<BrowserKind>,
            &'a str,
            &'a str,
            &'a str,
        );
        let rows: &[Row] = &[
            (
                "engine",
                "scanner::tests::a_reorg_moves_it",
                None,
                "engine",
                "engine::scanner",
                "A reorg moves it",
            ),
            (
                "engine::backup_restore",
                "restores_both",
                None,
                "engine",
                "engine::backup_restore",
                "Restores both",
            ),
            (
                "checkout.spec.js",
                "shows the amount",
                Some(BrowserKind::RealBinaries),
                "browser",
                "checkout (real binaries)",
                "Shows the amount",
            ),
            (
                "pos-terminal.spec.js",
                "copies the address",
                Some(BrowserKind::Fixture),
                "browser",
                "pos-terminal",
                "Copies the address",
            ),
        ];
        for (class, name, kind, krate, group, label) in rows {
            let mut t = test(class, name);
            t.kind = *kind;
            let case = if kind.is_some() {
                browser_case(&t)
            } else {
                rust_case(&t)
            };
            assert_eq!(
                (
                    case.krate.as_str(),
                    case.group.as_str(),
                    case.label.as_str()
                ),
                (*krate, *group, *label),
                "{class} {name}"
            );
        }
        let report = Report::new(
            String::new(),
            Sources::default(),
            None,
            Some(Properties {
                settings: crate::pages::inputs::Settings::default(),
                cases: 0,
                observations: crate::pages::inputs::Observations::default(),
                tests: vec![test("engine", "work::tests::properties::money_matches")],
            }),
        );
        assert_eq!(report.cases[0].area, Area::Payments);
        let mut webhook = rust_case(&test("monokulo", "webhooks::tests::signs"));
        webhook.area = Area::ALL
            .into_iter()
            .find(|a| a.claims(&webhook, false))
            .unwrap();
        assert_eq!(webhook.area, Area::Webhooks);
        let mut wallet = rust_case(&test("cli-wallet", "amount::tests::parses"));
        assert!(Area::Tools.claims(&wallet, true));
        wallet.area = Area::ALL
            .into_iter()
            .find(|a| a.claims(&wallet, false))
            .unwrap();
        assert_eq!(wallet.area, Area::Other);
    }
}
