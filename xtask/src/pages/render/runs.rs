//! The nightly runs: property tests and fuzzing, with the scenarios each
//! one's generated histories reached.

use super::{heading, key_figure, mark, meter, run_pill};
use crate::exploration::Status;
use crate::pages::format::{count, duration, int, maybe_int, per_case, sentence, share, NONE};
use crate::pages::inputs::{FuzzTarget, Observations, Test, TestStatus};
use crate::pages::model::{property_group, Report};
use maud::{html, Markup};
use std::collections::BTreeMap;

/// The steps a generated history is made of, by the name the engine records
/// them under, in words.
const COMMANDS: [(&str, &str); 12] = [
    ("Arrive", "Payment arrives in the mempool"),
    ("Mine", "Payment mined into a block"),
    ("Extend", "Chain extended by a block"),
    ("Reorg", "Chain reorganised"),
    ("Drop", "Payment dropped from the mempool"),
    ("Spent", "Output spent elsewhere"),
    ("Proof", "Payment proof supplied"),
    ("Advance", "Clock advanced"),
    ("Round", "Engine scan round"),
    ("Deliver", "Webhook delivery attempted"),
    ("Restart", "Engine restarted"),
    ("Fault", "Fault injected"),
];

/// A box of observations: what they are, and each one in words.
struct Observed {
    title: &'static str,
    why: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

const FAULTS: Observed = Observed {
    title: "Faults injected",
    why: "Things broken on purpose in the middle of a history. After each one the engine has to report the problem, keep every balance right, and carry on.",
    rows: &[
        ("sql-denial-reached", "Database write refused"),
        ("rpc-timeout-cancelled", "Node request timed out"),
        ("custody-error-reached", "Key custody failed"),
        ("all-node-outage-preserves-money-and-cursors", "Every node down at once"),
        ("http-503-reached", "Shop answered 503"),
        ("worker-restarted-mid-history", "Scan worker restarted mid-history"),
        ("connection-reopened-mid-history", "Database reopened mid-history"),
        ("custody-handle-replaced", "Custody handle replaced"),
    ],
};

const RULES: Observed = Observed {
    title: "Money rules checked",
    why: "Results the model insists on. One mismatch fails the whole run.",
    rows: &[
        ("proven-settlement-released", "Proven payment settles"),
        (
            "unproven-payment-keeps-settlement-pending",
            "Unproven payment stays pending",
        ),
        (
            "missing-proof-holds-settlement",
            "Missing proof holds settlement",
        ),
        (
            "mismatching-proof-holds-settlement",
            "Mismatching proof holds settlement",
        ),
        (
            "disputed-spent-retains-funds",
            "Disputed spend keeps funds counted",
        ),
        (
            "unanimous-spent-void-checked",
            "Spend confirmed by every node voids the output",
        ),
        (
            "void-restored-to-canonical-block",
            "Voided output restored after reorg",
        ),
        (
            "http-retry-stable-bytes-and-drained",
            "Webhook retries resend identical bytes",
        ),
        ("expiry-derived", "Order expiry derived"),
        (
            "connection-reopened-final-ledger",
            "Ledger identical after reopening",
        ),
    ],
};

const SETUPS: Observed = Observed {
    title: "Setups",
    why: "How each history started: how many wallets and transactions, which way of scanning, and whether it used real recorded mainnet transactions.",
    rows: &[
        ("setup:wallets-2", "2 wallets"),
        ("setup:wallets-3", "3 wallets"),
        ("setup:wallets-4", "4 wallets"),
        ("setup:transactions-2", "2 transactions"),
        ("setup:transactions-3", "3 transactions"),
        ("setup:transactions-4", "4 transactions"),
        ("backend:direct", "Direct scanning"),
        ("backend:worker", "Worker scanning"),
        ("fixture:recorded-and-synthetic", "Recorded mainnet + synthetic transactions"),
        ("fixture:scanner-valid-synthetic", "Synthetic transactions only"),
    ],
};

/// The observation every property run reaches, used to explain the bars.
const EXAMPLE: &str = "sql-denial-reached";

fn seen(obs: &Observations, key: &str) -> u64 {
    obs.get(key).copied().unwrap_or(0)
}

fn obs_head(first: &str) -> Markup {
    html! {
        div.obshead { span { (first) } span { "Relative" } span { "Times" } span title="Times per history" { "#/H" } }
    }
}

fn obs_row(label: &str, n: u64, max: u64, cases: u64) -> Markup {
    html! {
        div.obsrow {
            span { (label) }
            (meter(share(n, max), false))
            span.r.n { (int(n)) }
            span.r { (per_case(n, cases).unwrap_or_default()) }
        }
    }
}

/// A box of observations, leaving out what never happened; nothing when
/// none did.
fn observed(group: &Observed, obs: &Observations, cases: u64) -> Option<Markup> {
    let rows: Vec<(&str, u64)> = group
        .rows
        .iter()
        .map(|(key, label)| (*label, seen(obs, key)))
        .filter(|(_, n)| *n > 0)
        .collect();
    let max = rows.iter().map(|(_, n)| *n).max()?;
    Some(html! {
        div {
            h3 { (group.title) }
            p.why { (group.why) }
            (obs_head("Situation"))
            @for (label, n) in &rows { (obs_row(label, *n, max, cases)) }
        }
    })
}

/// The history steps the generator picked, applied and skipped.
fn commands(obs: &Observations) -> Option<Markup> {
    struct Step {
        label: &'static str,
        picked: u64,
        applied: u64,
        skipped: u64,
    }
    let steps: Vec<Step> = COMMANDS
        .iter()
        .map(|(name, label)| Step {
            label,
            picked: seen(obs, &format!("selected-command:{name}")),
            applied: seen(obs, &format!("applied-transition:{name}")),
            skipped: seen(obs, &format!("skipped-command:{name}")),
        })
        .filter(|s| s.picked > 0 || s.applied > 0)
        .collect();
    let max = steps.iter().map(|s| s.picked.max(s.applied)).max()?;
    let extended = seen(obs, "applied-transition:ExtendInsteadOfMine");
    Some(html! {
        div {
            h3 { "Chain events generated" }
            p.why { "Each history is a random list of these steps. Sometimes a step can’t happen (there’s no waiting payment to drop, say), so it’s skipped. At the end the model works out what every wallet should have, and the engine has to match." }
            div.legend { span { "Applied" } span.sk { "Picked but skipped" } }
            div.obshead { span { "Step" } span { "Picked" } span { "Applied" } span { "Skipped" } }
            @for s in &steps {
                div.obsrow {
                    span { (s.label) }
                    div.meter role="img" aria-label={ (s.applied) " applied, " (s.skipped) " skipped" } {
                        i style={ "width:" (super::width(share(s.applied, max))) "%" } {}
                        i.skip style={ "width:" (super::width(share(s.skipped, max))) "%" } {}
                    }
                    span.r.n { (int(s.applied)) }
                    span.r { @if s.skipped > 0 { (int(s.skipped)) } @else { (NONE) } }
                }
            }
            @if extended > 0 {
                p.why.after { (int(extended)) " skipped “mine” steps had nothing to mine and extended the chain with an empty block instead." }
            }
        }
    })
}

pub(super) fn properties(report: &Report) -> Markup {
    let Some(p) = &report.properties else {
        return html! {};
    };
    let intro = html! {
        "A normal test checks one example written by hand: pay 1 XMR, expect the order to be paid. A property test checks a rule instead: however the payments, reorgs and crashes happen, every wallet ends up with exactly what it was sent. The computer invents hundreds of random histories, runs the real engine through each one, and compares the result with a separate, much simpler model that works out the right answer on its own. If they ever disagree, the history is shrunk to the smallest version that still fails and saved, so every future run checks it again."
    };
    let mut by_group: BTreeMap<String, Vec<&Test>> = BTreeMap::new();
    for t in &p.tests {
        by_group.entry(property_group(&t.name)).or_default().push(t);
    }
    let secs = |tests: &[&Test]| tests.iter().map(|t| t.secs).sum::<f64>();
    // Slowest groups first, and the slowest properties first within each.
    let mut groups: Vec<(String, Vec<&Test>)> = by_group.into_iter().collect();
    for (_, tests) in &mut groups {
        tests.sort_by(|a, b| b.secs.total_cmp(&a.secs));
    }
    groups.sort_by(|a, b| secs(&b.1).total_cmp(&secs(&a.1)));
    let failed = p.tests.iter().any(|t| t.status == TestStatus::Failed);
    let (obs, cases) = (&p.observations, p.cases);
    let example = seen(obs, EXAMPLE);
    html! {
        (heading("Property tests", &intro, run_pill(report, report.sources.properties.as_ref())))
        dl.kv {
            (key_figure("Properties", &html! { (count(p.tests.len())) " " small { @if failed { "some failed" } @else { "passed" } } }))
            (key_figure("Cases per property", &html! { (p.settings.proptest_cases.as_deref().unwrap_or(NONE)) }))
            (key_figure("Histories modelled", &html! { (int(cases)) }))
            (key_figure("CPU time", &html! { (duration(p.tests.iter().map(|t| t.secs).sum())) }))
        }
        @if example > 0 && cases > 0 {
            @let per = per_case(example, cases).unwrap_or_default();
            div.card.pad.read {
                div {
                    h3 { "Reading the bars below" }
                    p { "While a history plays out, the engine keeps a tally of every situation it lands in, like a refused database write or the chain reorganising. " b { "Times" } " is the tally across all " (int(cases)) " histories from last night. One history can hit the same situation many times, so a tally can be bigger than " (int(cases)) ". " b { "#/H" } " is the average per history." }
                    p { "The bars only compare rows in the same box. What matters is that no row is zero. A zero would mean the random histories had stopped reaching that situation, and the rule would still pass without really being tested." }
                }
                div.example {
                    (obs_head("Situation"))
                    (obs_row("Database write refused", example, example, cases))
                    div.ann {
                        b { "Times" } span { "the harness refused a database write " (int(example)) " times in total" }
                        b { "#/H" } span { "times per history: about " (per.trim_end_matches('×')) " refusals in each history" }
                        b { "Relative" } span { "the most frequent fault, so its bar is full" }
                        b { "Passed" } span { "after every refusal the engine reported the problem and every balance stayed right" }
                    }
                }
            }
        }
        div.two {
            div.card.pad.obs { (commands(obs).unwrap_or_default()) (observed(&SETUPS, obs, cases).unwrap_or_default()) }
            div.card.pad.obs { (observed(&FAULTS, obs, cases).unwrap_or_default()) (observed(&RULES, obs, cases).unwrap_or_default()) }
        }
        div.card {
            div.tree {
                div.treehead.cols { span { "Group" } span { "Properties" } span { "Time" } }
                @for (group, tests) in &groups {
                    details {
                        summary { span.cols { span.nm.mono { (group) } span.cnt { (count(tests.len())) } span.tim { (duration(secs(tests))) } } }
                        ul.tlist {
                            @for t in tests {
                                li {
                                    (mark(t.status))
                                    span { (sentence(t.name.rsplit("::").next().unwrap_or_default())) }
                                    span.tim { (duration(t.secs)) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What each fuzz target checks, in words.
fn describe(target: &str) -> &'static str {
    match target {
        "portfolio" => "Plays out payments to several wallets at once and checks every wallet’s money against the model",
        "history" => "Plays out whole chain histories through the real scanner and database and checks the money against the model",
        "status" => "Checks each order’s status (paid, underpaid, expired and so on) follows correctly from the payments seen",
        "notifications" => "Checks the engine is woken when there’s work and never waits longer than its time limits",
        "mempool" => "Checks the cache of unconfirmed payments: who owns each entry, what gets thrown out, and that it stays within its memory budget",
        "queue" => "Checks work handed to the engine’s workers is dispatched correctly, even when queues fill up or close",
        "scheduler" => "Checks scan scheduling: retries, waits and node time-outs all stay within their bounds",
        "resources" => "Checks the scan budgets worked out from network speed stay inside their limits",
        "inputs" => "Feeds junk text to the CPU-list setting and checks the result is sorted, has no repeats, and reads back the same",
        _ => "",
    }
}

/// A target's executions last night, if it ran.
fn executions_of(targets: &[FuzzTarget], name: &str) -> Option<u64> {
    targets.iter().find(|t| t.target == name)?.executions
}

/// `about 148 million`, `724`: a count in words for a sentence.
fn about(n: u64) -> String {
    const MILLION: u64 = 1_000_000;
    if n >= MILLION {
        format!("about {} million", (n + MILLION / 2) / MILLION)
    } else {
        int(n)
    }
}

/// New code edges: `+35`, or `−3` when a run found fewer than it started with.
fn growth(g: i64) -> String {
    let sign = if g < 0 { '−' } else { '+' };
    format!("{sign}{}", int(g.unsigned_abs()))
}

/// `724`, `150 M`.
fn executions(n: u64) -> String {
    const MILLION: u64 = 1_000_000;
    if n >= MILLION {
        format!("{} M", (n + MILLION / 2) / MILLION)
    } else {
        int(n)
    }
}

pub(super) fn fuzzing(report: &Report) -> Markup {
    let targets = &report.fuzz;
    let intro = html! {
        "A fuzzer throws strange inputs at the code, keeps the ones that reach code it hasn’t seen before, and mutates those to go further. It isn’t only looking for crashes. Every target is wrapped in checks: the two big ones, portfolio and history, play a whole run of payments through the real engine and compare every balance with the money model, and the smaller ones each check one specific rule. If any check fails, the fuzzer counts it as a crash and saves the input that caused it."
    };
    let failed = targets
        .iter()
        .filter(|t| t.status != Status::Passed)
        .count();
    let sum = |f: fn(&FuzzTarget) -> Option<u64>| targets.iter().filter_map(f).sum::<u64>();
    let reached: Vec<&FuzzTarget> = targets.iter().filter(|t| t.cases > 0).collect();
    html! {
        (heading("Fuzzing", &intro, run_pill(report, report.sources.fuzz.as_ref())))
        dl.kv {
            (key_figure("Targets", &html! { (count(targets.len())) " " small { @if failed == 0 { "passed" } @else { (count(failed)) " failed" } } }))
            (key_figure("Executions", &html! { (executions(sum(|t| t.executions))) }))
            (key_figure("Corpus inputs", &html! { (int(sum(|t| t.corpus))) }))
            (key_figure("New last night", &html! { (int(sum(|t| t.new_inputs))) }))
        }
        div.card { div.tbl { table {
            thead { tr { th { "Target" } th.r { "Time" } th.r { "Executions" } th.r { "Corpus" } th.r { "New" } th.r { "Code edges" } } }
            tbody {
                @for t in targets {
                    tr {
                        td {
                            b { (t.target) }
                            span.muted.desc { (describe(&t.target)) }
                            @if t.status != Status::Passed { " " span.lowtag { (t.status.name()) } }
                        }
                        td.r { (duration(t.seconds)) }
                        td.r { (maybe_int(t.executions)) }
                        td.r { (maybe_int(t.corpus)) }
                        td.r { (maybe_int(t.new_inputs)) }
                        td.r {
                            (maybe_int(t.edges))
                            @if let Some(g) = t.edge_growth { " " span.muted { (growth(g)) } }
                        }
                    }
                }
            }
        } } }
        div.callout {
            b { "Why so few portfolio runs?" }
            span {
                @if let (Some(fast), Some(slow)) = (executions_of(targets, "resources"), executions_of(targets, "portfolio")) {
                    "Resources ran " (about(fast)) " times last night, portfolio " (about(slow)) ". "
                }
                "Each portfolio input builds a whole multi-wallet chain and runs the real scanner over it, a bit like playing a full game rather than pressing one button. "
                b { "Code edges" } " counts the different paths through the code the fuzzer has found; the + is how many were new last night."
            }
        }
        @if !reached.is_empty() {
            div.vhead { div {
                h2.sub { "What the inputs reached" }
                p { "The targets that play out histories count the situations they land in, like the property tests. Open one to see last night’s." }
            } }
            div.card {
                div.tree {
                    @for t in &reached {
                        details {
                            summary { span.cols { span.nm { (t.target) } span.cnt { (int(t.cases)) } span.tim { "histories" } } }
                            div.files.obs {
                                (commands(&t.observations).unwrap_or_default())
                                @for group in [&FAULTS, &RULES, &SETUPS] {
                                    (observed(group, &t.observations, t.cases).unwrap_or_default())
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
