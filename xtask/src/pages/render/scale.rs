//! Scale: how long one block takes to scan for every store on one CPU core,
//! how that grows with the store count, and how the engine recovers from
//! injected faults.

use super::{heading, key_figure, run_pill};
use crate::pages::format::{
    count, date, float, int, len, maybe_int, ms, ordinal, small_number, whole, NONE,
};
use crate::pages::inputs::{Fault, Phase, Stress, StressPoint, Tick};
use crate::pages::model::Report;
use maud::{html, Markup};

const BYTES_PER_MB: f64 = 1_048_576.0;

/// One measured store count of a run.
struct Row {
    stores: u64,
    /// Average time to scan one block, measured rounds only.
    ms: f64,
    peak_mb: Option<f64>,
    slowest_reply_ms: Option<f64>,
    /// Stores behind at the end.
    behind: u64,
    sustainable: bool,
}

fn average(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / len(values.len()))
}

fn measured(ticks: &[Tick]) -> Vec<f64> {
    ticks
        .iter()
        .filter(|t| t.phase == Phase::Measured)
        .map(|t| float(t.ms))
        .collect()
}

fn rows(run: &Stress) -> Vec<Row> {
    let mut rows: Vec<Row> = run
        .points
        .iter()
        .filter_map(|p| {
            let stores = p.tenants().filter(|n| *n > 0)?;
            let last = p.ticks().last()?;
            Some(Row {
                stores,
                ms: average(&measured(p.ticks())).unwrap_or(0.0),
                peak_mb: p
                    .peak_bytes()
                    .map(|b| (float(b) / BYTES_PER_MB * 10.0).round() / 10.0),
                slowest_reply_ms: p.fixture.http_max_us.map(|us| float(us) / 1000.0),
                behind: last.lagging,
                sustainable: p.sustainable(),
            })
        })
        .collect();
    rows.sort_by_key(|r| r.stores);
    rows
}

/// The first step of these that fits the range in five or fewer.
fn step(range: f64, steps: &[f64]) -> f64 {
    steps
        .iter()
        .copied()
        .find(|s| range / s <= 5.0)
        .unwrap_or_else(|| steps.last().copied().unwrap_or(1.0) * 2.0)
}

/// Time per block against store count, drawn to scale.
fn chart(rows: &[Row]) -> Markup {
    const W: f64 = 640.0;
    const H: f64 = 220.0;
    const LEFT: f64 = 52.0;
    const RIGHT: f64 = 16.0;
    const TOP: f64 = 18.0;
    const BOTTOM: f64 = 34.0;
    let top = float(rows.iter().map(|r| r.stores).max().unwrap_or(1));
    let x_step = step(top, &[8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0]);
    let x_max = (top * 1.08 / x_step).ceil() * x_step;
    let y_raw = (rows.iter().map(|r| r.ms).fold(0.0, f64::max) * 1.25).max(1.0);
    let y_step = step(
        y_raw,
        &[
            10.0, 20.0, 40.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0,
        ],
    );
    let y_max = (y_raw / y_step).ceil() * y_step;
    let x = |n: f64| LEFT + (W - LEFT - RIGHT) * n / x_max;
    let y = |v: f64| TOP + (H - TOP - BOTTOM) * (1.0 - v / y_max);
    let ticks = |max: f64, step: f64| {
        let n = whole(max / step);
        (0..=n).map(move |i| float(i) * step)
    };
    let bar = ((W - LEFT - RIGHT) / len(rows.len()) / 2.0).min(28.0);
    let label = rows
        .iter()
        .map(|r| int(r.stores))
        .collect::<Vec<_>>()
        .join(", ");
    let f = |v: f64| format!("{v:.1}");
    html! {
        svg viewBox={ "0 0 " (W) " " (H) } role="img" aria-label={ "Scan time per block for " (label) " stores" } {
            @for v in ticks(y_max, y_step) {
                line.grid x1=(f(LEFT)) x2=(f(W - RIGHT)) y1=(f(y(v))) y2=(f(y(v))) {}
                text x=(f(LEFT - 8.0)) y=(f(y(v) + 4.0)) text-anchor="end" { (ms(v)) " ms" }
            }
            line.axis x1=(f(LEFT)) x2=(f(W - RIGHT)) y1=(f(y(0.0))) y2=(f(y(0.0))) {}
            @for n in ticks(x_max, x_step) {
                text x=(f(x(n))) y=(f(H - 12.0)) text-anchor="middle" { (ms(n)) }
            }
            text x=(f(W - RIGHT)) y=(f(H - 1.0)) text-anchor="end" { "stores" }
            @for r in rows {
                @let n = float(r.stores);
                rect.barfill x=(f(x(n) - bar / 2.0)) y=(f(y(r.ms))) width=(f(bar)) height=(f(y(0.0) - y(r.ms))) rx="3" {}
                text.lbl x=(f(x(n))) y=(f(y(r.ms) - 6.0)) text-anchor="middle" { (ms(r.ms)) " ms" }
            }
        }
    }
}

/// Seconds from milliseconds, as few digits as it needs: `1.5`.
fn seconds(ms: u64) -> String {
    (float(ms) / 1000.0).to_string()
}

/// A run's measured points: key figures, the chart and the table.
fn run_figures(run: &Stress, title: Option<&str>) -> Option<Markup> {
    let rows = rows(run);
    let (first, last) = (rows.first()?, rows.last()?);
    let per_store = (rows.len() > 1 && last.stores > first.stores)
        .then(|| (last.ms - first.ms) / float(last.stores - first.stores));
    let budget = run.scenario.budget_ms.filter(|b| *b > 0);
    let share = |v: f64| {
        budget.map_or_else(
            || NONE.to_string(),
            |b| format!("{:.1}%", 100.0 * v / float(b)),
        )
    };
    let measured_ticks = run.points.first().and_then(|p| p.fixture.measured_ticks);
    let all_kept_up = rows.iter().all(|r| r.sustainable);
    Some(html! {
        @if let Some(title) = title { div.vhead { div { h2.sub { (title) } } } }
        dl.kv {
            (key_figure(&format!("{} stores, one block", int(last.stores)), &html! { (ms(last.ms)) " ms" }))
            (key_figure(&format!("Share of the {}budget", budget.map_or_else(String::new, |b| format!("{} s ", seconds(b)))), &html! { (share(last.ms)) }))
            (key_figure("Each extra store", &html! { @if let Some(p) = per_store { "+" (format!("{p:.1}")) " ms" } @else { (NONE) } }))
            (key_figure("Peak memory", &html! { @if let Some(mb) = last.peak_mb { (mb) " MB" } @else { (NONE) } }))
        }
        div.two {
            div.card.chart {
                h3 { "Time to scan one block for every store" }
                p.muted.small {
                    "Average of " (measured_ticks.map_or_else(|| "the".to_string(), int)) " measured blocks after warm-up."
                    @if let Some(p) = per_store {
                        " The cost grows by about " (format!("{p:.1}")) " ms per store on top of a fixed "
                        (ms(first.ms - p * float(first.stores))) " ms."
                    }
                }
                (chart(&rows))
            }
            div.card {
                div.tbl { table {
                    thead { tr { th { "Stores" } th.r { "Per block" } th.r { "Of budget" } th.r { "Slowest reply" } th.r { "Behind" } } }
                    tbody {
                        @for r in &rows {
                            tr {
                                td { b { (int(r.stores)) } }
                                td.r { (ms(r.ms)) " ms" }
                                td.r { (share(r.ms)) }
                                td.r { @if let Some(s) = r.slowest_reply_ms { (format!("{s:.1}")) " ms" } @else { (NONE) } }
                                td.r { (int(r.behind)) }
                            }
                        }
                    }
                } }
                p.muted.small.note {
                    "Pass conditions: no store more than " (maybe_int(run.scenario.max_lag_blocks)) " blocks behind, status replies under " (maybe_int(run.scenario.max_http_ms)) " ms. "
                    @if all_kept_up {
                        @if rows.len() == 2 { "Both passed." } @else { "All " (count(rows.len())) " passed." }
                    } @else { "Not every point kept up; see the full report." }
                }
            }
        }
    })
}

/// A fault run in words, and its own figures.
struct FaultCard {
    title: &'static str,
    what: String,
    figures: Vec<(&'static str, String)>,
}

fn describe(fault: Fault, p: &StressPoint, run: &Stress) -> FaultCard {
    let fx = &p.fixture;
    let n = maybe_int;
    match fault {
        Fault::Rpc => FaultCard {
            title: "Node errors",
            what: format!(
                "Every node request slowed by {} ms, and every {} request failed until block {}.",
                n(fx.rpc_delay_ms),
                fx.rpc_fail_every.map_or_else(|| NONE.to_string(), ordinal),
                n(fx.rpc_fail_until_height)
            ),
            figures: vec![(
                "Requests failed",
                format!("{} of {}", n(fx.rpc_failures), n(fx.rpc_calls)),
            )],
        },
        Fault::SqliteLock => {
            let lock = seconds(run.scenario.fault_lock_ms.or(fx.lock_ms).unwrap_or(0));
            FaultCard {
                title: "Database locked",
                what: format!(
                    "Another writer held the database lock for {lock} s during each of the first {} blocks.",
                    n(fx.measured_ticks)
                ),
                figures: vec![("Lock held", format!("{lock} s × {} blocks", n(fx.measured_ticks)))],
            }
        }
        Fault::Custody => FaultCard {
            title: "Key custody slow",
            what: format!(
                "Key custody limited to {} at a time, each taking an extra {} ms.",
                match fx.custody_slots {
                    Some(1) => "1 scan".to_string(),
                    other => format!("{} scans", n(other)),
                },
                n(fx.custody_delay_ms)
            ),
            figures: vec![
                ("Custody scans", n(fx.custody_scans)),
                (
                    "Longest wait",
                    format!(
                        "{} ms",
                        n(fx.custody_max_wait_us.map(|us| (us + 500) / 1000))
                    ),
                ),
            ],
        },
    }
}

fn faults(run: &Stress) -> Option<Markup> {
    let runs: Vec<(Fault, &StressPoint)> = run
        .faults
        .iter()
        .filter(|p| !p.ticks().is_empty())
        .filter_map(|p| Some((p.fault()?, p)))
        .collect();
    let (_, first) = runs.first()?;
    let behind = |p: &StressPoint| p.ticks().iter().map(|t| t.lagging).max().unwrap_or(0);
    let none_behind = runs.iter().all(|(_, p)| behind(p) == 0);
    Some(html! {
        div.vhead { div {
            h2.sub { "Recovering from faults" }
            p {
                "Each run breaks one thing on purpose for " (maybe_int(first.fixture.measured_ticks)) " blocks, then gives the engine "
                (maybe_int(first.fixture.drain_ticks)) " blocks to recover. Each bar is how long one block took; orange bars are the broken ones. "
                @if none_behind { "In all " (small_number(runs.len())) ", no store fell behind." }
                @else { "Some stores fell behind; the drain blocks show whether they caught up." }
            }
        } }
        div.faults {
            @for (fault, p) in &runs {
                @let card = describe(*fault, p, run);
                @let ticks = p.ticks();
                @let tallest = ticks.iter().map(|t| t.ms).max().unwrap_or(1).max(1);
                @let during = measured(ticks).into_iter().fold(0.0, f64::max);
                @let after: Vec<f64> = ticks.iter().filter(|t| t.phase != Phase::Measured).map(|t| float(t.ms)).collect();
                div.card.fault {
                    h3 { (card.title) }
                    p { (card.what) }
                    div.strip role="img" aria-label="Time per block" {
                        @for t in ticks {
                            i class=[(t.phase == Phase::Measured).then_some("f")] style={ "height:" (format!("{:.1}", (100.0 * float(t.ms) / float(tallest)).max(4.0))) "%" } title={ (int(t.ms)) " ms" } {}
                        }
                    }
                    div.cap { span { "fault: " (ms(during)) " ms" } span { "after: " (average(&after).map_or_else(|| NONE.to_string(), ms)) " ms" } }
                    dl {
                        dt { "Stores" } dd { (maybe_int(p.tenants())) }
                        @for (k, v) in &card.figures { dt { (k) } dd { (v) } }
                        dt { "Stores behind" } dd { (int(behind(p))) }
                    }
                }
            }
        }
    })
}

pub(super) fn page(report: &Report) -> Markup {
    let Some(stress) = &report.stress else {
        return html! {};
    };
    let budget = stress.scenario.budget_ms.filter(|b| *b > 0);
    let intro = html! {
        "Could Monokulo keep up on a small computer? This test pins it to a single CPU core, gives it lots of stores at once, and keeps other work competing with it: reading the database, answering status requests, saving settings. A new block arrives every "
        @if let Some(b) = budget { (seconds(b)) " seconds" } @else { "few seconds" }
        ", and every store’s payments have to be checked before the next one lands."
    };
    let hw = &stress.hardware;
    let reports = report.coverage.as_ref().map(|c| &c.reports);
    html! {
        (heading("Scale", &intro, run_pill(report, report.sources.coverage.as_ref())))
        @if let Some(figures) = run_figures(stress, None) { (figures) }
        @else { p.card.unavail { "The stress run recorded no measured points." } }
        (faults(stress).unwrap_or_default())
        @if let Some(scale) = &report.scale {
            (run_figures(scale, Some("Weekly scale run")).unwrap_or_default())
            (faults(scale).unwrap_or_default())
            p.muted.small {
                "Weekly scale run"
                @if let Some(run) = &report.sources.scale {
                    " of " a href=(report.run_url(run.run_id)) { (date(&run.date).unwrap_or_else(|| "its last run".into())) }
                }
                ": the same test with far more stores, so the time per block shows how far one core can go."
            }
        } @else {
            div.next {
                h3 { "These are quick checks, not the limit" }
                p { "They take seconds, so they can run on every change. A bigger weekly run tries far more stores on one core, and runs the faults with more stores too. Its results show up here once it has run on main." }
            }
        }
        p.muted.small {
            "Runner: " (hw.cpu_model.as_deref().unwrap_or("unknown")) ", pinned to one of " (maybe_int(hw.effective_cores)) " cores, SQLite "
            (hw.sqlite_version.as_deref().unwrap_or(NONE)) ". Absolute times vary between runners; the pass condition is that every store keeps up."
            @if let Some(href) = reports.and_then(|r| r.stress.as_ref()) { " " a href=(href) { "Full stress report" } "." }
        }
    }
}
