//! The report's pages, rendered with maud: one page per section, each with
//! the same header, headline figures and section tabs. They read without
//! JavaScript; `assets/quality.js` only adds the tests' and screens' filters
//! and the screenshot viewer.

mod coverage;
mod runs;
mod scale;
mod screens;
mod tests;

use super::format::{self, coverage as covered, date, int, share, NONE};
use super::model::Report;
use crate::coverage::Counts;
use crate::logo;
use crate::support::at;
use maud::{html, Markup, PreEscaped, DOCTYPE};
use std::{fs, io, path::Path};

/// A section of the report, and the page it is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tab {
    Overview,
    Tests,
    Coverage,
    Properties,
    Fuzzing,
    Scale,
    Live,
    Screens,
}

impl Tab {
    const ALL: [Tab; 8] = [
        Tab::Overview,
        Tab::Tests,
        Tab::Coverage,
        Tab::Properties,
        Tab::Fuzzing,
        Tab::Scale,
        Tab::Live,
        Tab::Screens,
    ];

    pub(super) fn file(self) -> &'static str {
        match self {
            Tab::Overview => "index.html",
            Tab::Tests => "tests.html",
            Tab::Coverage => "coverage.html",
            Tab::Properties => "properties.html",
            Tab::Fuzzing => "fuzzing.html",
            Tab::Scale => "scale.html",
            Tab::Live => "live.html",
            Tab::Screens => "screens.html",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "What’s tested",
            Tab::Tests => "All tests",
            Tab::Coverage => "Coverage",
            Tab::Properties => "Property tests",
            Tab::Fuzzing => "Fuzzing",
            Tab::Scale => "Scale",
            Tab::Live => "Live network",
            Tab::Screens => "Screens",
        }
    }

    /// Whether the report has anything for this section. The overview is
    /// the front page, so it is there whenever any other section is.
    fn has_data(self, report: &Report) -> bool {
        match self {
            Tab::Overview => true,
            Tab::Tests => !report.cases.is_empty(),
            Tab::Coverage => report.coverage.as_ref().is_some_and(|c| {
                !c.crates.is_empty()
                    || c.totals.rust.is_some()
                    || c.totals.browser.is_some()
                    || c.totals.woocommerce.is_some()
            }),
            Tab::Properties => report.properties.is_some(),
            Tab::Fuzzing => !report.fuzz.is_empty(),
            Tab::Scale => report.stress.is_some() || report.scale.is_some(),
            Tab::Live => report.live.is_some(),
            Tab::Screens => report
                .gallery
                .as_ref()
                .is_some_and(|g| !g.screens.is_empty()),
        }
    }

    /// The count its tab shows, if any.
    fn count(self, report: &Report) -> Option<usize> {
        match self {
            Tab::Tests => Some(report.cases.len()),
            Tab::Properties => report.properties.as_ref().map(|p| p.tests.len()),
            Tab::Fuzzing => Some(report.fuzz.len()),
            Tab::Live => report.live.as_ref().map(|l| l.tests.len()),
            Tab::Screens => report.gallery.as_ref().map(|g| g.total),
            Tab::Overview | Tab::Coverage | Tab::Scale => None,
        }
    }
}

/// What every page shares: the sections there are, and the header's run
/// line and figures.
struct Shell {
    tabs: Vec<Tab>,
    counts: Vec<Option<usize>>,
    runline: Markup,
    lead: String,
    figures: Markup,
}

/// Writes a page for every section the report has data for, and the front
/// page even when it has none. Returns the pages written.
pub(super) fn write(report: &Report, out: &Path) -> io::Result<Vec<Tab>> {
    let mut tabs: Vec<Tab> = Tab::ALL
        .into_iter()
        .filter(|t| t.has_data(report))
        .collect();
    if tabs == [Tab::Overview] {
        tabs.clear();
    }
    let shell = Shell {
        counts: tabs.iter().map(|t| t.count(report)).collect(),
        tabs,
        runline: runline(report),
        lead: lead(report),
        figures: figures(report),
    };
    if shell.tabs.is_empty() {
        let body = html! { p.card.unavail { "No test results have been published yet." } };
        save(
            out,
            Tab::Overview,
            &page(report, &shell, Tab::Overview, &body),
        )?;
        return Ok(vec![Tab::Overview]);
    }
    for &tab in &shell.tabs {
        let body = match tab {
            Tab::Overview => tests::overview(report),
            Tab::Tests => tests::all(report),
            Tab::Coverage => coverage::page(report),
            Tab::Properties => runs::properties(report),
            Tab::Fuzzing => runs::fuzzing(report),
            Tab::Scale => scale::page(report),
            Tab::Live => runs::live(report),
            Tab::Screens => screens::page(report),
        };
        save(out, tab, &page(report, &shell, tab, &body))?;
    }
    Ok(shell.tabs)
}

fn save(out: &Path, tab: Tab, page: &Markup) -> io::Result<()> {
    let path = out.join(tab.file());
    fs::write(&path, &page.0).map_err(|e| at(&path, e))
}

const TITLE: &str = "How Monokulo is tested";

fn page(report: &Report, shell: &Shell, tab: Tab, body: &Markup) -> Markup {
    let repo = &report.repo;
    // Only the pages with filters or a viewer load the script.
    let script = matches!(tab, Tab::Tests | Tab::Screens);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title {
                    @if tab == Tab::Overview { (TITLE) } @else { (tab.label()) " · " (TITLE) }
                }
                meta name="description" content="Monokulo’s tests, coverage, property tests, fuzzing, load checks, live-network checks and screenshots, rebuilt from every CI run on main.";
                link rel="icon" href="../assets/favicon.svg" type="image/svg+xml";
                link rel="stylesheet" href="assets/theme.css";
                link rel="stylesheet" href="assets/quality.css";
                @if script { script src="assets/quality.js" defer {} }
            }
            body {
                header.chrome {
                    div.bar {
                        a.brand href="../" {
                            svg.logo-mark width="30" height="30" viewBox="0 0 64 64" aria-hidden="true" focusable="false" {
                                (PreEscaped(logo::small()))
                            }
                            span { "Monokulo" }
                        }
                        nav aria-label="Site" {
                            a href="../#how" { "How it works" }
                            a href="../#install" { "Install" }
                            a href="./" aria-current="page" { "Quality" }
                            a href=(repo) { "Source code" }
                        }
                    }
                }
                main {
                    div.intro {
                        h1 { (TITLE) }
                        p.lead {
                            "Monokulo handles other people’s money, so it gets tested hard. Every change runs "
                            (shell.lead)
                            " automatic tests. Every night, two kinds of robot tester invent thousands of strange situations and check that no payment goes missing. This page rebuilds itself from those runs. Open any section to see the details."
                        }
                        div.runline { (shell.runline) }
                    }
                    @if !shell.tabs.is_empty() {
                        div.figs { (shell.figures) }
                        nav.tabs aria-label="Report sections" {
                            @for (t, n) in shell.tabs.iter().zip(&shell.counts) {
                                a href=(t.file()) aria-current=[(*t == tab).then_some("page")] {
                                    (t.label())
                                    @if let Some(n) = n { span.c { (format::count(*n)) } }
                                }
                            }
                        }
                    }
                    section.view { (body) }
                    footer.foot {
                        "Rebuilt by "
                        a href={ (repo) "/blob/main/.github/workflows/pages.yml" } { "the Pages workflow" }
                        " from the latest runs on main, using "
                        a href={ (repo) "/tree/main/xtask/src/pages" } { code { "cargo xtask pages build" } }
                        "."
                    }
                }
            }
        }
    }
}

/// "more than 2,200": the per-change test count, rounded down.
fn lead(report: &Report) -> String {
    match report.per_change().count() {
        0 => "thousands of".into(),
        n if n < 100 => format::count(n),
        n => format!("more than {}", format::count(n / 100 * 100)),
    }
}

/// Whether everything passed, and the runs the figures come from.
fn runline(report: &Report) -> Markup {
    let sources = &report.sources;
    let mut parts: Vec<Markup> = Vec::new();
    parts.push(if report.all_passed() {
        html! { span.pill.ok { "All checks passed" } }
    } else {
        html! { span.pill { "Some checks failed" } }
    });
    if let Some(run) = &sources.coverage {
        let sha = run
            .sha
            .get(..7)
            .or_else(|| report.coverage.as_ref()?.revision.as_deref()?.get(..7))
            .unwrap_or("");
        parts.push(html! {
            span.muted {
                "Tests and coverage from "
                a href=(report.run_url(run.run_id)) { code { (sha) } }
                @if let Some(d) = date(&run.date) { " on " (d) }
            }
        });
    }
    for (name, run) in [
        ("Nightly properties", &sources.properties),
        ("Fuzzing", &sources.fuzz),
        ("Live network", &sources.live),
    ] {
        if let Some(d) = run.as_ref().and_then(|r| date(&r.date)) {
            parts.push(html! { span.muted { (name) " " (d) } });
        }
    }
    html! {
        @for (i, part) in parts.iter().enumerate() {
            @if i > 0 { span.muted { "·" } }
            (part)
        }
    }
}

fn figures(report: &Report) -> Markup {
    let per_change: Vec<_> = report.per_change().collect();
    let passed = per_change
        .iter()
        .filter(|c| c.status == super::inputs::TestStatus::Passed)
        .count();
    let skipped = per_change
        .iter()
        .filter(|c| c.status == super::inputs::TestStatus::Skipped)
        .count();
    let coverage = report.coverage.as_ref();
    let lines = |pick: fn(&super::inputs::CoverageRun) -> Option<Counts>| coverage.and_then(pick);
    let shipping = lines(|c| Some(c.shipping.lines).filter(|l| l.total > 0));
    let browser = lines(|c| c.totals.browser.map(|t| t.lines));
    let woocommerce = lines(|c| c.totals.woocommerce.map(|t| t.lines));
    let nightly = report.properties.as_ref().map(|p| p.tests.len());
    let mut beside = Vec::new();
    if !report.fuzz.is_empty() {
        beside.push(format!(
            "plus {}",
            format::plural(report.fuzz.len(), "fuzz target", "fuzz targets")
        ));
    }
    if let Some(live) = &report.live {
        beside.push(format!("{} live daily", format::count(live.tests.len())));
    }
    html! {
        (figure("Tests", &format::count(per_change.len()),
            &format!("{} passed · {} skipped", format::count(passed), format::count(skipped)), None))
        (coverage_figure("Engine and server", shipping, "Rust"))
        (coverage_figure("Checkout and POS", browser, "browser"))
        (coverage_figure("WooCommerce", woocommerce, "PHP"))
        (figure("Properties nightly", &nightly.map_or_else(|| NONE.to_string(), format::count),
            &if beside.is_empty() { "no fuzz run yet".to_string() } else { beside.join(" · ") }, None))
    }
}

fn coverage_figure(title: &str, lines: Option<Counts>, language: &str) -> Markup {
    match lines {
        Some(l) => figure(
            title,
            &covered(l),
            &format!("of {} {language} lines", int(l.total)),
            share(l.covered, l.total),
        ),
        None => figure(title, NONE, "not measured in this run", None),
    }
}

fn figure(key: &str, value: &str, sub: &str, meter_share: Option<f64>) -> Markup {
    html! {
        div.fig {
            span.k { (key) }
            span.v { (value) }
            span.s { (sub) }
            @if meter_share.is_some() { (meter(meter_share, true)) }
        }
    }
}

/// A bar filled to a share, or empty and labelled when nothing was measured.
fn meter(fill: Option<f64>, ok: bool) -> Markup {
    let label = fill.map_or_else(|| "not measured".to_string(), |p| format!("{p:.1} percent"));
    html! {
        div.meter.ok[ok] role="img" aria-label=(label) {
            i style={ "width:" (width(fill)) "%" } {}
        }
    }
}

/// A bar's width in percent: at least a sliver when anything was measured.
fn width(fill: Option<f64>) -> String {
    fill.map_or_else(|| "0".into(), |p| format!("{:.2}", p.clamp(1.0, 100.0)))
}

/// A section's heading and what it shows, with an optional link to the run.
fn heading(title: &str, intro: &Markup, run: Option<(String, String)>) -> Markup {
    html! {
        div.vhead {
            div { h2 { (title) } p { (intro) } }
            @if let Some((href, label)) = run { a.pill href=(href) { (label) } }
        }
    }
}

/// The pill linking a section to the run its figures come from.
fn run_pill(report: &Report, run: Option<&super::fetch::SourceRun>) -> Option<(String, String)> {
    let run = run?;
    let label = date(&run.date).map_or_else(|| "The run".to_string(), |d| format!("Run of {d}"));
    Some((report.run_url(run.run_id), label))
}

/// A key figure inside a section: the label carries the context.
fn key_figure(label: &str, value: &Markup) -> Markup {
    html! { div { dt { (label) } dd { (value) } } }
}

/// A test's result mark.
fn mark(status: super::inputs::TestStatus) -> Markup {
    use super::inputs::TestStatus::{Failed, Passed, Skipped};
    let (class, label, glyph) = match status {
        Passed => ("passed", "passed", "✓"),
        Skipped => ("skipped", "skipped", "–"),
        Failed => ("failed", "failed", "✕"),
    };
    html! { span.st.(class) aria-label=(label) { (glyph) } }
}
