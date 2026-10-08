//! The front page (what's tested, by area) and every test, grouped by area
//! and then by the module or spec file it lives in.

use super::{heading, mark};
use crate::pages::format::{count, duration, plural};
use crate::pages::model::{Area, Case, Report, Suite};
use maud::{html, Markup};
use std::collections::BTreeMap;

/// How many of an area's tests its card names.
const SAMPLES: usize = 3;
/// Names longer than this read badly on a card, so shorter ones come first.
const SAMPLE_MAX_CHARS: usize = 90;

fn in_area(report: &Report, area: Area) -> Vec<&Case> {
    report.cases.iter().filter(|c| c.area == area).collect()
}

fn suites(cases: &[&Case]) -> Vec<Suite> {
    Suite::ALL
        .into_iter()
        .filter(|s| cases.iter().any(|c| c.suite == *s))
        .collect()
}

pub(super) fn overview(report: &Report) -> Markup {
    let intro = html! {
        "Each test checks one thing Monokulo should do, and its name says what, so the list reads like a rulebook. They’re sorted by the part of Monokulo they protect. Pick an area to see every test in it."
    };
    html! {
        (heading("What’s tested", &intro, None))
        @if report.cases.is_empty() {
            p.card.unavail { "This run published no test results. The other sections have what it did publish." }
        } @else {
            div.areas {
                @for area in Area::ALL {
                    @let cases = in_area(report, area);
                    @if !cases.is_empty() {
                        (area_card(area, &cases))
                    }
                }
            }
        }
    }
}

fn area_card(area: Area, cases: &[&Case]) -> Markup {
    let mut sample: Vec<&&Case> = cases
        .iter()
        .filter(|c| c.suite != Suite::Property)
        .collect();
    sample.sort_by(|a, b| {
        let short = |c: &Case| c.leaf.chars().count() < SAMPLE_MAX_CHARS;
        short(b).cmp(&short(a)).then(b.secs.total_cmp(&a.secs))
    });
    html! {
        a.area href={ "tests.html#area-" (area.id()) } {
            span.t { h3 { (area.name()) } span.n { (count(cases.len())) } }
            p { (area.description()) }
            ul {
                @for case in sample.iter().take(SAMPLES) {
                    li title=(case.label) { (case.label) }
                }
            }
            span.suites {
                @for suite in suites(cases) { span.tag { (suite.tag()) } }
            }
        }
    }
}

fn total_secs<'a>(cases: impl IntoIterator<Item = &'a &'a Case>) -> f64 {
    cases.into_iter().map(|c| c.secs).sum()
}

pub(super) fn all(report: &Report) -> Markup {
    let intro = html! {
        "Every test, sorted by area and then by the file it lives in. A tick means it passed on the last run; the time is how long it took."
    };
    let proptest_cases = report
        .properties
        .as_ref()
        .and_then(|p| p.settings.proptest_cases.as_deref());
    let all: Vec<&Case> = report.cases.iter().collect();
    html! {
        (heading("All tests", &intro, None))
        // The filters work in the browser; without JavaScript every test is listed.
        div.fpanel data-filters hidden {
            div.frow {
                span { "Search" }
                label.search { input #q type="search" placeholder="e.g. reorg, refund, double spend" aria-label="Search tests"; }
            }
            div.frow {
                span { "Suite" }
                div { div.seg role="group" aria-label="Suite" {
                    button type="button" data-suite="" aria-pressed="true" { "All" }
                    @for suite in Suite::ALL { button type="button" data-suite=(suite.name()) aria-pressed="false" { (suite.name()) } }
                } }
            }
            div.frow {
                span { "Area" }
                div.filters {
                    button.chip type="button" data-area="" aria-pressed="true" { "All areas" }
                    @for area in Area::ALL {
                        @if report.cases.iter().any(|c| c.area == area) {
                            button.chip type="button" data-area=(area.id()) aria-pressed="false" { (area.name()) }
                        }
                    }
                }
            }
        }
        div.toolbar {
            span.muted.small #tcount { (plural(all.len(), "test", "tests")) " · " (duration(total_secs(&all))) " in total" }
            div.grp data-filters hidden {
                button.btn type="button" #expand-all { "Expand all" }
                button.btn type="button" #collapse-all { "Collapse all" }
            }
        }
        div.card #tbox {
            div.tree {
                div.treehead.cols { span { "Area and group" } span { "Tests" } span { "Time" } }
                @for area in Area::ALL {
                    @let cases = in_area(report, area);
                    @if !cases.is_empty() { (area_tree(area, &cases, proptest_cases)) }
                }
            }
            p.empty #tnone hidden { "No tests match. Try a shorter word, or clear the suite and area filters." }
        }
    }
}

fn area_tree(area: Area, cases: &[&Case], proptest_cases: Option<&str>) -> Markup {
    let mut groups: BTreeMap<&str, Vec<&Case>> = BTreeMap::new();
    for case in cases {
        groups.entry(case.group.as_str()).or_default().push(case);
    }
    html! {
        details.area-tree id={ "area-" (area.id()) } data-area=(area.id()) {
            summary {
                span.cols {
                    span.nm { (area.name()) " " span.muted.small data-groups { (plural(groups.len(), "group", "groups")) } }
                    span.cnt { (count(cases.len())) }
                    span.tim { (duration(total_secs(cases))) }
                }
            }
            div.mods {
                @for (group, cases) in &groups {
                    details data-group {
                        summary {
                            span.cols {
                                span.nm { (group) }
                                span.cnt { (count(cases.len())) }
                                span.tim { (duration(total_secs(cases))) }
                            }
                        }
                        ul.tlist {
                            @for case in cases { (test_row(case, proptest_cases)) }
                        }
                    }
                }
            }
        }
    }
}

fn test_row(case: &Case, proptest_cases: Option<&str>) -> Markup {
    html! {
        li data-suite=(case.suite.name()) data-secs=(case.secs) {
            (mark(case.status))
            span {
                span.label { (case.label) }
                @if case.suite == Suite::Property {
                    @if let Some(n) = proptest_cases { " " span.tag { (n) " cases" } }
                }
                span.raw { (case.raw) }
            }
            span.tim { (duration(case.secs)) }
        }
    }
}
