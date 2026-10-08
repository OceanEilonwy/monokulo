//! Coverage: the totals, every Rust crate with its files lowest first, and
//! the browser and WooCommerce reports.

use super::{heading, key_figure, meter};
use crate::coverage::Counts;
use crate::pages::format::{coverage as covered, plural, ratio, share};
use crate::pages::inputs::{Coverage, CoverageRun, Crate, FileRow};
use crate::pages::model::Report;
use maud::{html, Markup};
use std::cmp::Ordering;

/// From this line coverage up, a bar is drawn as healthy.
const HEALTHY: f64 = 85.0;

fn bar(c: Counts) -> Markup {
    let fill = share(c.covered, c.total);
    html! {
        div.covcell { (meter(fill, fill.is_some_and(|p| p >= HEALTHY))) b { (covered(c)) } }
    }
}

/// Lowest coverage first; files with nothing measured last.
fn by_coverage(a: &FileRow, b: &FileRow) -> Ordering {
    let key = |f: &FileRow| share(f.coverage.lines.covered, f.coverage.lines.total);
    match (key(a), key(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.path.cmp(&b.path),
    }
}

pub(super) fn page(report: &Report) -> Markup {
    let Some(run) = &report.coverage else {
        return html! {};
    };
    let intro = html! {
        "While the tests run, a tool watches which lines of code actually get used. Code that no test ever runs is where bugs can hide. Higher is better, but 100% isn’t the goal: some code, like the stress-test tools, never ships. A " b { "branch" } " is each way an " code { "if" } " can go. Open a crate (a package of Rust code) to see its files, lowest first."
    };
    let mut crates: Vec<&Crate> = run
        .crates
        .iter()
        .filter(|c| c.coverage.lines.total > 0)
        .collect();
    crates.sort_by(|a, b| {
        a.tool
            .cmp(&b.tool)
            .then(b.coverage.lines.total.cmp(&a.coverage.lines.total))
    });
    let (shipping, tools): (Vec<&Crate>, Vec<&Crate>) = crates.into_iter().partition(|c| !c.tool);
    let others = other_reports(run);
    let measured_with: Vec<&str> = [&run.tools.rust, &run.tools.browser, &run.tools.woocommerce]
        .into_iter()
        .filter_map(Option::as_deref)
        .collect();
    html! {
        (heading("Coverage", &intro, None))
        dl.kv {
            @if run.shipping.lines.total > 0 { (total("Rust, shipping crates", run.shipping)) }
            @if let Some(c) = run.totals.rust { (total("Rust, whole workspace", c)) }
            @if let Some(c) = run.totals.browser { (total("Browser", c)) }
            @if let Some(c) = run.totals.woocommerce { (total("WooCommerce", c)) }
        }
        div.card {
            div.tree.cov {
                div.treehead.ccols { span { "Crate" } span { "Lines" } span { "Covered / total" } span { "Branches" } }
                @for c in &shipping { (crate_row(c)) }
                @if !tools.is_empty() {
                    div.grp { "Test tools" }
                    @for c in &tools { (crate_row(c)) }
                }
                @if !others.is_empty() {
                    div.grp { "Other reports" }
                    @for row in &others { (row) }
                }
                @if shipping.is_empty() && tools.is_empty() && others.is_empty() {
                    p.empty { "No coverage figures in this run." }
                }
            }
        }
        @if !measured_with.is_empty() {
            p.muted.small {
                "Measured with " (list(&measured_with)) ". Files that can’t be instrumented are listed under their crate rather than counted as 0%."
            }
        }
    }
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn total(title: &str, c: Coverage) -> Markup {
    key_figure(
        title,
        &html! { (covered(c.lines)) " " small { (covered(c.branches)) " branches" } },
    )
}

fn other_reports(run: &CoverageRun) -> Vec<Markup> {
    [
        ("Browser", "checkout, embed, POS", run.totals.browser, &run.reports.browser),
        ("WooCommerce", "plugin", run.totals.woocommerce, &run.reports.woocommerce),
    ]
    .into_iter()
    .filter_map(|(title, note, totals, report)| {
        let c = totals?;
        Some(html! {
            div.row.ccols {
                span.nm {
                    b { @if let Some(href) = report { a href=(href) { (title) } } @else { (title) } }
                    " " span.muted.small { (note) }
                }
                (bar(c.lines))
                span.r { (ratio(c.lines)) }
                span.r { (covered(c.branches)) }
            }
        })
    })
    .collect()
}

fn crate_row(c: &Crate) -> Markup {
    let mut files: Vec<&FileRow> = c.files.iter().collect();
    files.sort_by(|a, b| by_coverage(a, b));
    let prefix = format!("crates/{}/", c.name);
    html! {
        details {
            summary {
                span.ccols {
                    span.nm { b { (c.name) } " " span.muted.small { (plural(c.files.len(), "file", "files")) } }
                    (bar(c.coverage.lines))
                    span.r { (ratio(c.coverage.lines)) }
                    span.r { (covered(c.coverage.branches)) }
                }
            }
            div.files { div.tbl { table {
                thead { tr { th { "File" } th { "Lines" } th.r { "Covered" } th.r { "Branches" } } }
                tbody {
                    @for f in &files {
                        tr {
                            td {
                                a href=(f.report) title="Annotated source" { (f.path) }
                                @if f.path.contains("/bin/") && f.coverage.lines.covered == 0 { " " span.lowtag { "tool binary" } }
                            }
                            td { (bar(f.coverage.lines)) }
                            td.r { (ratio(f.coverage.lines)) }
                            td.r { (ratio(f.coverage.branches)) }
                        }
                    }
                    @for path in &c.unmeasured {
                        tr { td { (path.strip_prefix(&prefix).unwrap_or(path)) } td.muted colspan="3" { "not instrumented" } }
                    }
                }
            } } }
        }
    }
}
