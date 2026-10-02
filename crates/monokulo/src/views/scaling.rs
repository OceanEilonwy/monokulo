//! How the engine is performing and scaling, as people read it
//! (docs/engine_scaling.md section 6): the figures' formatting here, and
//! the admin page's performance panels.

use maud::{html, Markup};
use shared::resources::{ResourceReport, ResourceSample};
use shared::scaling::{
    ChunkLimit, HeadersFirstReason, LinkPoint, LinkSnapshot, NetworkScaling, Pace, SlowBlock, Trend,
};

/// `bytes` in decimal units, as sizes on a network are given: "412 MB",
/// "1.8 MB", "13 kB".
pub fn bytes(bytes: u64) -> String {
    let b = bytes as f64;
    let (value, unit) = if b >= 1e9 {
        (b / 1e9, "GB")
    } else if b >= 1e6 {
        (b / 1e6, "MB")
    } else if b >= 1e3 {
        (b / 1e3, "kB")
    } else {
        return format!("{bytes} B");
    };
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

/// A link's rate, from bytes a second: "3.1 Mbit/s", "480 kbit/s".
pub fn rate(bytes_per_sec: u64) -> String {
    let bits = bytes_per_sec as f64 * 8.0;
    if bits >= 1e9 {
        format!("{:.1} Gbit/s", bits / 1e9)
    } else if bits >= 1e6 {
        format!("{:.1} Mbit/s", bits / 1e6)
    } else {
        format!("{:.0} kbit/s", bits / 1e3)
    }
}

/// An elapsed time to the second: "2 m 10 s", "45 s", "1 h 5 m".
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h} h {m} m")
    } else if m > 0 {
        format!("{m} m {s} s")
    } else {
        format!("{s} s")
    }
}

/// A rough length of time: "18 minutes", "40 seconds", "2 hours".
pub fn duration_rough(secs: i64) -> String {
    let secs = secs.max(0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    if secs >= 2 * 3600 {
        plural((secs as f64 / 3600.0).round() as i64, "hour")
    } else if secs >= 90 {
        plural((secs as f64 / 60.0).round() as i64, "minute")
    } else {
        plural(secs, "second")
    }
}

/// `n` with thousands separators: "3,412,001".
pub fn thousands(n: u64) -> String {
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

/// A network's name for the start of a sentence: "Mainnet".
fn network_name(network: &str) -> String {
    let mut name = network.to_string();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}

/// One sentence about a slow block: what is slow, at what rate, how much
/// longer it needs and what would help (docs/engine_scaling.md section 5).
/// The node only when `show_node` (an operator is looking).
pub fn slow_block_message(network: &str, slow: &SlowBlock, show_node: bool) -> String {
    let size = slow
        .wire_bytes
        .map(|wire| format!(" ({})", bytes(wire)))
        .unwrap_or_default();
    let mut message = format!(
        "{}: block {}{size} has taken {} so far",
        network_name(network),
        thousands(slow.height),
        duration(slow.elapsed_secs),
    );
    match (show_node, &slow.node, slow.rate_bytes_per_sec) {
        (true, Some(node), Some(speed)) => {
            message.push_str(&format!(", at {} from {node}", rate(speed)));
        }
        (_, _, Some(speed)) => message.push_str(&format!(", at {}", rate(speed))),
        _ => {}
    }
    message.push('.');
    if let Some(remaining) = slow.remaining_secs.filter(|secs| *secs > 0) {
        message.push_str(&format!(
            " At this rate it needs about {} more.",
            duration_rough(remaining)
        ));
    }
    message.push_str(" A faster node or a larger scan memory budget would help.");
    message
}

/// "3 minutes ago", or "just now" within the last slot.
fn ago(secs: i64) -> String {
    if secs < SLOT_SECS {
        "just now".to_string()
    } else {
        format!("{} ago", duration_rough(secs))
    }
}

/// Each process samples itself into slots this long (`shared::resources`).
pub const SLOT_SECS: i64 = 10;
/// An hour of slots: what the charts show.
pub const SLOTS: usize = 360;
/// Slots a chart's hover detail covers at a time: a minute.
const SLOTS_PER_MINUTE: usize = 6;

/// The start of the chart's first slot, so its last one holds `now`.
fn first_slot(now: i64) -> i64 {
    now.div_euclid(SLOT_SECS) * SLOT_SECS - (SLOTS as i64 - 1) * SLOT_SECS
}

/// One process's samples placed on the hour's slots ending at `now`:
/// `None` for a slot it has no sample for, which the chart leaves as a gap.
pub fn on_slots(
    samples: &[ResourceSample],
    now: i64,
    value: impl Fn(&ResourceSample) -> f64,
) -> Vec<Option<f64>> {
    let first = first_slot(now);
    let mut slots = vec![None; SLOTS];
    for sample in samples {
        let at = (sample.unix - first).div_euclid(SLOT_SECS);
        if let Some(slot) = usize::try_from(at).ok().and_then(|at| slots.get_mut(at)) {
            *slot = Some(value(sample));
        }
    }
    slots
}

/// The admin's Resources panel: both processes' CPU and memory
/// (docs/engine_scaling.md section 6).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcesView {
    /// `None` when the engine didn't report its own (or didn't answer).
    pub engine: Option<ResourceReport>,
    pub monokulo: ResourceReport,
    pub now_unix: i64,
}

/// One process's band in a chart.
struct Layer {
    /// Its colour role's class, `chart-engine` or `chart-monokulo`.
    class: &'static str,
    name: &'static str,
    slots: Vec<Option<f64>>,
}

/// A line across the chart: a container's memory limit.
struct Limit {
    value: f64,
    class: &'static str,
    label: String,
}

/// A chart of `layers` stacked in order (the first at the bottom), scaled
/// so `max` is the top. A slot is drawn only where every layer has a
/// sample: a total with a part missing would understate it.
struct Chart {
    what: &'static str,
    layers: Vec<Layer>,
    max: f64,
    format: fn(f64) -> String,
    limits: Vec<Limit>,
    now: i64,
}

impl Chart {
    /// Each drawable slot's layer values, `None` for a gap.
    fn stacks(&self) -> Vec<Option<Vec<f64>>> {
        (0..SLOTS)
            .map(|slot| {
                self.layers
                    .iter()
                    .map(|layer| layer.slots.get(slot).copied().flatten())
                    .collect()
            })
            .collect()
    }

    fn y(&self, value: f64) -> f64 {
        if self.max <= 0.0 {
            return 100.0;
        }
        (100.0 - value / self.max * 100.0).clamp(0.0, 100.0)
    }

    /// The SVG path of layer `k`: a step outline per run of drawn slots,
    /// from the layers below it to its own top.
    fn path(&self, stacks: &[Option<Vec<f64>>], k: usize) -> String {
        let mut d = String::new();
        let mut slot = 0;
        while slot < stacks.len() {
            if stacks[slot].is_none() {
                slot += 1;
                continue;
            }
            let start = slot;
            while slot < stacks.len() && stacks[slot].is_some() {
                slot += 1;
            }
            let run = start..slot;
            let below =
                |at: usize| -> f64 { stacks[at].as_ref().map_or(0.0, |v| v[..k].iter().sum()) };
            let top = |at: usize| -> f64 { below(at) + stacks[at].as_ref().map_or(0.0, |v| v[k]) };
            let mut points = Vec::new();
            for at in run.clone() {
                let y = self.y(top(at));
                points.push(format!("{at},{y:.1}"));
                points.push(format!("{},{y:.1}", at + 1));
            }
            for at in run.rev() {
                let y = self.y(below(at));
                points.push(format!("{},{y:.1}", at + 1));
                points.push(format!("{at},{y:.1}"));
            }
            // Two slots at one height meet at a point both would add.
            points.dedup();
            d.push_str(&format!("M{}Z", points.join("L")));
        }
        d
    }

    /// "25 % (engine 23 %, monokulo 2 %)": a total and, when stacked, its
    /// parts.
    fn split(&self, values: &[f64]) -> String {
        let total: f64 = values.iter().sum();
        if self.layers.len() < 2 {
            return (self.format)(total);
        }
        let parts: Vec<String> = self
            .layers
            .iter()
            .zip(values)
            .map(|(layer, value)| format!("{} {}", layer.name, (self.format)(*value)))
            .collect();
        format!("{} ({})", (self.format)(total), parts.join(", "))
    }

    /// The newest drawn slot's values, and the highest total with how
    /// long ago it was.
    fn now_and_peak(&self, stacks: &[Option<Vec<f64>>]) -> Option<(Vec<f64>, f64, i64)> {
        let current = stacks.iter().rev().flatten().next()?.clone();
        let (peak_at, peak) = stacks
            .iter()
            .enumerate()
            .filter_map(|(at, values)| Some((at, values.as_ref()?.iter().sum::<f64>())))
            .fold(
                (0, f64::MIN),
                |best, next| if next.1 > best.1 { next } else { best },
            );
        let peak_unix = first_slot(self.now) + peak_at as i64 * SLOT_SECS;
        Some((current, peak, self.now - peak_unix))
    }

    fn render(&self, capacity: &str) -> Markup {
        let stacks = self.stacks();
        let figures = self.now_and_peak(&stacks);
        let minutes: Vec<(usize, String)> = (0..SLOTS / SLOTS_PER_MINUTE)
            .filter_map(|minute| {
                let slots = &stacks[minute * SLOTS_PER_MINUTE..(minute + 1) * SLOTS_PER_MINUTE];
                let drawn: Vec<&Vec<f64>> = slots.iter().flatten().collect();
                if drawn.is_empty() {
                    return None;
                }
                let average: Vec<f64> = (0..self.layers.len())
                    .map(|k| drawn.iter().map(|v| v[k]).sum::<f64>() / drawn.len() as f64)
                    .collect();
                let end =
                    first_slot(self.now) + ((minute + 1) * SLOTS_PER_MINUTE) as i64 * SLOT_SECS;
                let when = ago(self.now - end + SLOT_SECS);
                Some((
                    minute * SLOTS_PER_MINUTE,
                    format!("{when}: {}", self.split(&average)),
                ))
            })
            .collect();
        let label = match &figures {
            Some((current, _, _)) => format!(
                "{} over the last hour, now {} {capacity}",
                self.what,
                self.split(current)
            ),
            None => format!("{}: no samples yet", self.what),
        };
        html! {
            div class="resource" {
                p class="resource-figure" {
                    strong { (self.what) }
                    @match &figures {
                        Some((current, peak, peak_ago)) => {
                            " " (self.split(current)) " " (capacity)
                            span class="muted" { " · peak " ((self.format)(*peak)) " " (ago(*peak_ago)) }
                        }
                        None => { " " span class="muted" { "no samples yet" } }
                    }
                }
                svg class="resource-chart" viewBox=(format!("0 0 {SLOTS} 100")) preserveAspectRatio="none" role="img" aria-label=(label) {
                    @for (k, layer) in self.layers.iter().enumerate() {
                        path class=(format!("chart-layer {}", layer.class)) d=(self.path(&stacks, k)) {}
                    }
                    @for limit in &self.limits {
                        @let y = format!("{:.1}", self.y(limit.value));
                        line class=(format!("chart-limit {}", limit.class)) x1="0" x2=(SLOTS) y1=(y) y2=(y) vector-effect="non-scaling-stroke" {
                            title { (limit.label) }
                        }
                    }
                    @for (x, text) in &minutes {
                        rect class="chart-hover" x=(x) y="0" width=(SLOTS_PER_MINUTE) height="100" { title { (text) } }
                    }
                }
                div class="chart-axis" aria-hidden="true" { span { "an hour ago" } span { "now" } }
            }
        }
    }
}

fn percent(value: f64) -> String {
    format!("{value:.0} %")
}

fn memory(value: f64) -> String {
    bytes(value.max(0.0) as u64)
}

/// The CPU and memory charts for `processes` stacked (first at the
/// bottom), all on `machine`.
fn charts(processes: &[(&'static str, &'static str, &ResourceReport)], now: i64) -> Markup {
    let Some((_, _, machine)) = processes.first() else {
        return html! {};
    };
    let cores = if machine.cpu_count == 1 {
        "of 1 core".to_string()
    } else {
        format!("of {} cores", machine.cpu_count)
    };
    let cpu = Chart {
        what: "CPU",
        layers: processes
            .iter()
            .map(|(name, class, report)| Layer {
                class,
                name,
                slots: on_slots(&report.samples, now, |s| f64::from(s.cpu_percent)),
            })
            .collect(),
        max: 100.0,
        format: percent,
        limits: Vec::new(),
        now,
    };
    let total_memory = machine.machine_memory_bytes.unwrap_or(0) as f64;
    let ram = Chart {
        what: "Memory",
        layers: processes
            .iter()
            .map(|(name, class, report)| Layer {
                class,
                name,
                slots: on_slots(&report.samples, now, |s| s.memory_bytes as f64),
            })
            .collect(),
        max: total_memory,
        format: memory,
        limits: processes
            .iter()
            .filter_map(|(name, class, report)| {
                let limit = report.cgroup_memory_bytes? as f64;
                (limit < total_memory).then(|| Limit {
                    value: limit,
                    class,
                    label: format!("The {name}'s container limit: {}", bytes(limit as u64)),
                })
            })
            .collect(),
        now,
    };
    let of_ram = match machine.machine_memory_bytes {
        Some(total) => format!("of {}", bytes(total)),
        None => String::new(),
    };
    html! {
        (cpu.render(&cores))
        (ram.render(&of_ram))
        @if !ram.limits.is_empty() {
            p class="hint" { "A dashed line marks a container's memory limit: a process near it is stopped before the machine fills." }
        }
    }
}

fn legend(processes: &[(&'static str, &'static str)]) -> Markup {
    html! {
        ul class="chart-legend" {
            @for (name, class) in processes {
                li { span class=(format!("chart-swatch {class}")) {} (name) }
            }
        }
    }
}

/// The Resources panel at the top of the Monero nodes tab: CPU and memory
/// for monokulo and the engine together, each a band of one stacked chart,
/// when they share a machine; apart when they don't.
pub fn resources_panel(view: &ResourcesView, refresh_href: &str) -> Markup {
    let now = view.now_unix;
    const ENGINE: (&str, &str) = ("engine", "chart-engine");
    const MONOKULO: (&str, &str) = ("monokulo", "chart-monokulo");
    html! {
        section class="resources" aria-labelledby="resources-title" {
            h3 id="resources-title" { "Resources" }
            p class="hint" {
                "CPU and memory over the last hour, sampled every 10 seconds. Hover over a chart for a minute's figures. "
                a href=(refresh_href) fx-action=(refresh_href) fx-target="#settings-panel" { "Refresh" }
                " (unsaved changes on this tab are lost)."
            }
            @match &view.engine {
                Some(engine) if engine.host_id == view.monokulo.host_id => {
                    (legend(&[ENGINE, MONOKULO]))
                    (charts(&[(ENGINE.0, ENGINE.1, engine), (MONOKULO.0, MONOKULO.1, &view.monokulo)], now))
                }
                Some(engine) => {
                    p class="hint" { "The engine and monokulo run on different machines, so each is shown against its own." }
                    h4 { "Engine" }
                    (charts(&[(ENGINE.0, ENGINE.1, engine)], now))
                    h4 { "Monokulo" }
                    (charts(&[(MONOKULO.0, MONOKULO.1, &view.monokulo)], now))
                }
                None => {
                    p class="hint" { "The engine didn't report its CPU and memory; monokulo's own are below." }
                    (charts(&[(MONOKULO.0, MONOKULO.1, &view.monokulo)], now))
                }
            }
        }
    }
}

/// A node's link measurements for its row on the Monero nodes tab.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeLinkView {
    pub link: LinkSnapshot,
    pub now_unix: i64,
}

/// A one-hour sparkline of one of a node's figures, a point per minute,
/// with gaps where it wasn't measured.
fn sparkline(view: &NodeLinkView, value: fn(&LinkPoint) -> u64) -> Markup {
    const MINUTES: i64 = 60;
    let this_minute = view.now_unix.div_euclid(60) * 60;
    let mut points: Vec<Option<u64>> = vec![None; MINUTES as usize];
    for point in &view.link.history {
        let back = (this_minute - point.minute_unix).div_euclid(60);
        if (0..MINUTES).contains(&back) {
            points[(MINUTES - 1 - back) as usize] = Some(value(point));
        }
    }
    let max = points.iter().flatten().copied().max().unwrap_or(0).max(1) as f64;
    let mut runs: Vec<String> = Vec::new();
    let mut run: Vec<String> = Vec::new();
    for (x, point) in points.iter().enumerate() {
        match point {
            Some(v) => run.push(format!("{x},{:.1}", 15.0 - *v as f64 / max * 14.0)),
            None if !run.is_empty() => runs.push(std::mem::take(&mut run).join(" ")),
            None => {}
        }
    }
    if !run.is_empty() {
        runs.push(run.join(" "));
    }
    html! {
        svg class="sparkline" viewBox="0 0 59 16" preserveAspectRatio="none" aria-hidden="true" {
            @for points in &runs {
                @if points.contains(' ') {
                    polyline points=(points) {}
                } @else {
                    circle cx=(points.split(',').next().unwrap_or("0")) cy=(points.split(',').nth(1).unwrap_or("8")) r="1" {}
                }
            }
        }
    }
}

fn count(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What the engine measured of a node's link: its transfer rate, round
/// trip and time to a block's first byte, each with its last hour, then
/// when and what went wrong lately (docs/engine_scaling.md section 6).
pub fn link_figures(view: &NodeLinkView) -> Markup {
    let link = &view.link;
    let trouble = match (link.timeouts_last_hour, link.failures_last_hour) {
        (0, 0) => "no timeouts or failures in the last hour".to_string(),
        (timeouts, failures) => format!(
            "{} and {} in the last hour",
            count(timeouts, "timeout", "timeouts"),
            count(failures, "failure", "failures")
        ),
    };
    html! {
        @if link.measured {
            ul class="node-link" {
                li { "Transfer " strong { (rate(link.rate_bytes_per_sec)) } " " (sparkline(view, |p| p.rate_bytes_per_sec)) }
                li { "Round trip " strong { (link.rtt_ms) " ms" } " " (sparkline(view, |p| p.rtt_ms)) }
                li { "First byte " strong { (link.ttfb_per_block_ms) " ms" } " a block " (sparkline(view, |p| p.ttfb_per_block_ms)) }
            }
            p class="node-status" {
                @if let Some(at) = link.last_measured_unix {
                    "Measured " (ago(view.now_unix - at)) "; "
                }
                (trouble) "."
            }
        } @else {
            p class="node-status" {
                "Link not measured yet: the engine assumes " (rate(link.rate_bytes_per_sec))
                " until its first block request. " (capitalize_first(&trouble)) "."
            }
        }
    }
}

fn capitalize_first(text: &str) -> String {
    network_name(text)
}

/// Why headers come first and for how long: "for about 50 minutes more: a
/// block request ran out of time or came back too large, so each block's
/// size is checked before it is fetched".
fn headers_first_why(on: &shared::scaling::HeadersFirst) -> String {
    format!(
        "for about {} more: {}, so each block's size is checked before it is fetched",
        duration_rough(on.remaining_secs),
        match on.reason {
            HeadersFirstReason::FailedRequest => {
                "a block request ran out of time or came back too large"
            }
            HeadersFirstReason::LargeBlock => {
                "a recent block came near the size that is scanned in pages"
            }
        }
    )
}

/// The status page's sentence while a network's blocks are read
/// header-first (docs/engine_scaling.md section 4).
pub fn headers_first_sentence(on: &shared::scaling::HeadersFirst) -> String {
    format!("Reading block headers first {}.", headers_first_why(on))
}

/// The node the scan is reading from, for the "Pace set by" line.
pub struct ActiveNode<'a> {
    pub label: &'a str,
    pub rate_bytes_per_sec: Option<u64>,
}

fn blocks(n: u64) -> String {
    if n == 1 {
        "1 block".to_string()
    } else {
        format!("{} blocks", thousands(n))
    }
}

/// One network's Scanning panel: how far behind, what sets the pace, the
/// requests, block sizes, memory and round, and a slow block if there is
/// one (docs/engine_scaling.md section 6).
pub fn scanning_panel(
    network: &str,
    scaling: &NetworkScaling,
    active: Option<ActiveNode<'_>>,
) -> Markup {
    let scan = &scaling.scan;
    let (tag_class, tag) = if scaling.slow.is_some() {
        ("tag-slow", "slow")
    } else if scaling.blocks_behind == 0 {
        ("tag-ok", "caught up")
    } else {
        ("tag-syncing", "catching up")
    };
    let progress = if scaling.blocks_behind == 0 {
        "At the node's tip.".to_string()
    } else {
        let mut text = format!(
            "{} behind · {:.1} blocks a minute",
            blocks(scaling.blocks_behind),
            scan.blocks_per_minute
        );
        if let Some(secs) = scaling.catch_up_secs {
            text.push_str(&format!(
                " · caught up in about {}",
                duration_rough(secs as i64)
            ));
        }
        text
    };
    let pace = match scaling.pace {
        Pace::CaughtUp => "Nothing: the scan is at the node's tip.".to_string(),
        Pace::Memory => "The scan memory budget, which caps each request. A larger budget (Server tab) would speed it up.".to_string(),
        Pace::Link => match &active {
            Some(ActiveNode { label, rate_bytes_per_sec: Some(speed) }) => {
                format!("Link speed: {label} at {}. A faster node would speed it up.", rate(*speed))
            }
            Some(ActiveNode { label, .. }) => format!("Link speed: {label}. A faster node would speed it up."),
            None => "Link speed. A faster node would speed it up.".to_string(),
        },
        Pace::Cpu => "Scanning for stores (CPU), which takes longer than fetching. A faster machine would speed it up.".to_string(),
        Pace::Round => "The round's time for blocks, shared with payments and checks.".to_string(),
    };
    let request = {
        let mut text = match scan.last_chunk {
            Some(plan) => format!(
                "{} a request, set by {}",
                blocks(plan.blocks),
                match plan.limited_by {
                    ChunkLimit::Memory => "the memory budget",
                    ChunkLimit::Link => "the link's speed",
                    ChunkLimit::Maximum => "the most one request asks for",
                    ChunkLimit::Remaining => "the blocks left",
                    ChunkLimit::Cpu => "the time scanning takes",
                }
            ),
            None => "None yet".to_string(),
        };
        if let (Some(block), Some(secs)) = (&scan.in_progress, scan.in_progress_secs) {
            text.push_str(&format!(
                " · block {} in progress for {}",
                thousands(block.height),
                duration(secs)
            ));
            if let Some(pages) = block.pages {
                text.push_str(&format!(
                    ", in pages: {} of {} transactions scanned, {} a page",
                    thousands(pages.done_txs),
                    thousands(pages.total_txs),
                    thousands(pages.page_txs)
                ));
            }
        }
        text
    };
    let block_size = {
        let trend = match scan.block_size_trend {
            Trend::Rising => "rising ↗",
            Trend::Falling => "falling ↘",
            Trend::Steady => "steady",
        };
        let mut text = format!("Average {} ({trend})", bytes(scan.avg_block_bytes));
        if let Some(largest) = &scan.largest_recent {
            text.push_str(&format!(
                " · largest recent {} (block {}), took {}",
                bytes(largest.wire_bytes),
                thousands(largest.height),
                duration(largest.secs.round() as i64)
            ));
        }
        text
    };
    let memory_line = {
        let mut text = format!("Budget {} MB", thousands(u64::from(scaling.budget_mb)));
        if let Some(peak) = scan.peak_cache_bytes {
            text.push_str(&format!(
                " · block cache peak {} in the last hour",
                bytes(peak)
            ));
        }
        if let Some(max) = scaling.max_budget_mb {
            text.push_str(&format!(
                " · this machine allows up to {} MB",
                thousands(u64::from(max))
            ));
        }
        text
    };
    let round = if scaling.round_deadline_secs > scaling.round_base_secs {
        format!(
            "Deadline {} s, raised from {} s so one large block's work fits",
            scaling.round_deadline_secs, scaling.round_base_secs
        )
    } else {
        format!("Deadline {} s (the base)", scaling.round_deadline_secs)
    };
    let headers_first = match &scan.headers_first {
        Some(on) => format!("On {}", headers_first_why(on)),
        None => "Off: blocks are fetched whole without asking their size first".to_string(),
    };
    let time = format!(
        "Fetching {}, scanning {}",
        duration(scan.fetch_secs_recent.round() as i64),
        duration(scan.scan_secs_recent.round() as i64)
    );
    html! {
        div class="scanning" data-scanning=(network) {
            h4 { "Scanning " span class=(format!("tag {tag_class}")) { (tag) } }
            @if let Some(slow) = &scaling.slow {
                p class="notice slow-block" role="status" { (slow_block_message(network, slow, true)) }
            }
            dl class="scan-figures" {
                dt { "Progress" } dd { (progress) }
                dt { "Pace set by" } dd { (pace) }
                dt { "Requests" } dd { (request) }
                dt { "Block size" } dd { (block_size) }
                dt { "Memory" } dd { (memory_line) }
                dt { "Round" } dd { (round) }
                dt { "Headers first" } dd { (headers_first) }
                dt { "Last 10 minutes" } dd { (time) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn report(host: &str, samples: Vec<ResourceSample>) -> ResourceReport {
        ResourceReport {
            host_id: host.to_string(),
            cpu_count: 4,
            machine_memory_bytes: Some(8_000_000_000),
            cgroup_memory_bytes: None,
            samples,
        }
    }

    /// A sample `back` slots before the newest, at `cpu` % and `mb` MB.
    fn sample(back: i64, cpu: f32, mb: u64) -> ResourceSample {
        ResourceSample {
            unix: NOW - NOW % SLOT_SECS - back * SLOT_SECS,
            cpu_percent: cpu,
            memory_bytes: mb * 1_000_000,
        }
    }

    #[test]
    fn samples_land_on_their_slot_and_missing_ones_are_gaps() {
        let slots = on_slots(
            &[sample(0, 5.0, 1), sample(2, 7.0, 1), sample(400, 9.0, 1)],
            NOW,
            |s| f64::from(s.cpu_percent),
        );
        assert_eq!(slots.len(), SLOTS);
        assert_eq!(slots[SLOTS - 1], Some(5.0));
        assert_eq!(
            slots[SLOTS - 2],
            None,
            "a slot nobody sampled is a gap, not zero"
        );
        assert_eq!(slots[SLOTS - 3], Some(7.0));
        assert_eq!(
            slots.iter().flatten().count(),
            2,
            "older than an hour is dropped"
        );
    }

    /// On one machine the two processes stack: one figure for the total,
    /// its split, and a band each (docs/engine_scaling.md section 6).
    #[test]
    fn one_machine_stacks_engine_under_monokulo_with_the_total_and_split() {
        let view = ResourcesView {
            engine: Some(ResourceReport {
                cgroup_memory_bytes: Some(2_000_000_000),
                ..report(
                    "boot-1",
                    vec![
                        sample(7, 60.0, 500),
                        sample(1, 23.0, 412),
                        sample(0, 23.0, 412),
                    ],
                )
            }),
            monokulo: report(
                "boot-1",
                vec![sample(7, 3.0, 90), sample(1, 2.0, 96), sample(0, 2.0, 96)],
            ),
            now_unix: NOW,
        };
        let html = resources_panel(&view, "/dashboard/admin/settings?tab=nodes").into_string();
        assert!(
            html.contains("CPU</strong> 25 % (engine 23 %, monokulo 2 %) of 4 cores"),
            "{html}"
        );
        assert!(
            html.contains("Memory</strong> 508 MB (engine 412 MB, monokulo 96.0 MB) of 8.0 GB"),
            "{html}"
        );
        assert!(html.contains("peak 63 % 70 seconds ago"), "{html}");
        assert!(
            html.contains(r#"class="chart-layer chart-engine""#),
            "{html}"
        );
        assert!(
            html.contains(r#"class="chart-layer chart-monokulo""#),
            "{html}"
        );
        assert!(
            html.find("chart-layer chart-engine") < html.find("chart-layer chart-monokulo"),
            "the engine is drawn first, at the bottom"
        );
        assert!(
            html.contains(r#"<span class="chart-swatch chart-engine"></span>engine"#),
            "{html}"
        );
        // The engine's container limit is a line on the memory chart.
        assert!(
            html.contains("The engine&#39;s container limit: 2.0 GB")
                || html.contains("The engine's container limit: 2.0 GB"),
            "{html}"
        );
        // A minute's hover detail.
        assert!(
            html.contains("<title>just now: 25 % (engine 23 %, monokulo 2 %)</title>"),
            "{html}"
        );
        // Two runs (seven slots back, then the last two), so each band's path has two parts.
        let engine_path = html
            .split(r#"class="chart-layer chart-engine" d=""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default();
        assert_eq!(engine_path.matches('M').count(), 2, "{engine_path}");
        assert!(!html.contains("different machines"));
    }

    #[test]
    fn different_machines_are_shown_apart_and_a_silent_engine_says_so() {
        let apart = ResourcesView {
            engine: Some(report("boot-1", vec![sample(0, 23.0, 412)])),
            monokulo: report("boot-2", vec![sample(0, 2.0, 96)]),
            now_unix: NOW,
        };
        let html = resources_panel(&apart, "/x").into_string();
        assert!(html.contains("run on different machines"), "{html}");
        assert!(
            html.contains("<h4>Engine</h4>") && html.contains("<h4>Monokulo</h4>"),
            "{html}"
        );
        assert!(html.contains("CPU</strong> 23 % of 4 cores"), "{html}");
        assert!(
            !html.contains("chart-legend"),
            "nothing is stacked, so no legend"
        );

        let silent = ResourcesView {
            engine: None,
            monokulo: report("boot-2", vec![]),
            now_unix: NOW,
        };
        let html = resources_panel(&silent, "/x").into_string();
        assert!(
            html.contains("The engine didn&#39;t report")
                || html.contains("The engine didn't report"),
            "{html}"
        );
        assert!(html.contains("no samples yet"), "{html}");
    }

    fn link(measured: bool) -> NodeLinkView {
        NodeLinkView {
            link: LinkSnapshot {
                measured,
                rtt_ms: 120,
                ttfb_per_block_ms: 45,
                rate_bytes_per_sec: 387_500,
                bytes_per_block: 50_000,
                last_measured_unix: measured.then_some(NOW - 12),
                timeouts_last_hour: 2,
                failures_last_hour: 1,
                history: vec![
                    LinkPoint {
                        minute_unix: NOW / 60 * 60,
                        rate_bytes_per_sec: 387_500,
                        rtt_ms: 120,
                        ttfb_per_block_ms: 45,
                    },
                    LinkPoint {
                        minute_unix: NOW / 60 * 60 - 60,
                        rate_bytes_per_sec: 300_000,
                        rtt_ms: 110,
                        ttfb_per_block_ms: 40,
                    },
                    LinkPoint {
                        minute_unix: NOW / 60 * 60 - 600,
                        rate_bytes_per_sec: 100_000,
                        rtt_ms: 90,
                        ttfb_per_block_ms: 30,
                    },
                ],
            },
            now_unix: NOW,
        }
    }

    #[test]
    fn a_node_shows_its_link_with_an_hour_of_each_figure() {
        let html = link_figures(&link(true)).into_string();
        assert!(
            html.contains("Transfer <strong>3.1 Mbit/s</strong>"),
            "{html}"
        );
        assert!(
            html.contains("Round trip <strong>120 ms</strong>"),
            "{html}"
        );
        assert!(
            html.contains("First byte <strong>45 ms</strong> a block"),
            "{html}"
        );
        assert!(
            html.contains("Measured 12 seconds ago; 2 timeouts and 1 failure in the last hour."),
            "{html}"
        );
        // The two latest minutes are a line; ten minutes back, alone, a dot.
        assert_eq!(html.matches("<polyline").count(), 3, "{html}");
        assert_eq!(html.matches("<circle").count(), 3, "{html}");

        let html = link_figures(&link(false)).into_string();
        assert!(
            html.contains("Link not measured yet: the engine assumes 3.1 Mbit/s"),
            "{html}"
        );
        assert!(!html.contains("<svg"), "{html}");
    }

    fn scaling(pace: Pace) -> NetworkScaling {
        NetworkScaling {
            scan: shared::scaling::ScanReport {
                avg_block_bytes: 1_800_000,
                block_size_trend: Trend::Rising,
                last_chunk: Some(shared::scaling::ChunkPlan {
                    blocks: 1,
                    limited_by: ChunkLimit::Link,
                }),
                blocks_per_minute: 3.2,
                fetch_secs_recent: 400.0,
                scan_secs_recent: 12.0,
                largest_recent: Some(shared::scaling::DoneBlock {
                    height: 3_411_990,
                    wire_bytes: 412_000_000,
                    secs: 1_080.0,
                    finished_unix: NOW - 100,
                }),
                in_progress: Some(shared::scaling::BlockInProgress {
                    height: 3_412_001,
                    started_unix: NOW - 130,
                    wire_bytes: None,
                    pages: None,
                }),
                in_progress_secs: Some(130),
                peak_cache_bytes: Some(301_000_000),
                discarded_cache_bytes: 0,
                round_budget_secs: None,
                headers_first: None,
            },
            blocks_behind: 14,
            catch_up_secs: Some(240),
            pace,
            budget_mb: 256,
            max_budget_mb: Some(1_536),
            round_deadline_secs: 10,
            round_base_secs: 10,
            slow: None,
        }
    }

    #[test]
    fn the_scanning_panel_says_what_sets_the_pace_and_why() {
        let node = || {
            Some(ActiveNode {
                label: "node.example:18089",
                rate_bytes_per_sec: Some(387_500),
            })
        };
        let html = scanning_panel("mainnet", &scaling(Pace::Link), node()).into_string();
        assert!(
            html.contains(r#"<span class="tag tag-syncing">catching up</span>"#),
            "{html}"
        );
        assert!(
            html.contains("14 blocks behind · 3.2 blocks a minute · caught up in about 4 minutes"),
            "{html}"
        );
        assert!(
            html.contains(
                "Link speed: node.example:18089 at 3.1 Mbit/s. A faster node would speed it up."
            ),
            "{html}"
        );
        assert!(
            html.contains("1 block a request, set by the link&#39;s speed")
                || html.contains("1 block a request, set by the link's speed"),
            "{html}"
        );
        assert!(
            html.contains("block 3,412,001 in progress for 2 m 10 s"),
            "{html}"
        );
        assert!(html.contains("Average 1.8 MB (rising ↗) · largest recent 412 MB (block 3,411,990), took 18 m 0 s"), "{html}");
        assert!(html.contains("Budget 256 MB · block cache peak 301 MB in the last hour · this machine allows up to 1,536 MB"), "{html}");
        assert!(html.contains("Deadline 10 s (the base)"), "{html}");
        assert!(
            html.contains("Off: blocks are fetched whole without asking their size first"),
            "{html}"
        );
        assert!(html.contains("Fetching 6 m 40 s, scanning 12 s"), "{html}");

        let html = scanning_panel("mainnet", &scaling(Pace::Memory), node()).into_string();
        assert!(
            html.contains("The scan memory budget, which caps each request."),
            "{html}"
        );

        let mut caught_up = scaling(Pace::CaughtUp);
        caught_up.blocks_behind = 0;
        caught_up.round_deadline_secs = 45;
        let html = scanning_panel("mainnet", &caught_up, None).into_string();
        assert!(
            html.contains(r#"<span class="tag tag-ok">caught up</span>"#),
            "{html}"
        );
        assert!(
            html.contains("At the node&#39;s tip.") || html.contains("At the node's tip."),
            "{html}"
        );
        assert!(html.contains("Deadline 45 s, raised from 10 s"), "{html}");

        // A large block scanned a page at a time says how far it has got,
        // and why headers come first.
        let mut paged = scaling(Pace::Link);
        paged.scan.headers_first = Some(shared::scaling::HeadersFirst {
            remaining_secs: 3_000,
            reason: HeadersFirstReason::FailedRequest,
        });
        if let Some(block) = paged.scan.in_progress.as_mut() {
            block.pages = Some(shared::scaling::PageProgress {
                done_txs: 41_000,
                total_txs: 97_000,
                page_txs: 100,
            });
        }
        let html = scanning_panel("mainnet", &paged, None).into_string();
        assert!(
            html.contains("in pages: 41,000 of 97,000 transactions scanned, 100 a page"),
            "{html}"
        );
        assert!(
            html.contains("On for about 50 minutes more: a block request ran out of time or came back too large"),
            "{html}"
        );

        let mut slow = scaling(Pace::Link);
        slow.slow = Some(SlowBlock {
            height: 3_412_001,
            wire_bytes: Some(412_000_000),
            elapsed_secs: 130,
            node: Some("node.example:18089".into()),
            rate_bytes_per_sec: Some(387_500),
            remaining_secs: Some(1_080),
        });
        let html = scanning_panel("mainnet", &slow, node()).into_string();
        assert!(
            html.contains(r#"<span class="tag tag-slow">slow</span>"#),
            "{html}"
        );
        assert!(html.contains("Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far, at 3.1 Mbit/s from node.example:18089."), "{html}");
    }

    #[test]
    fn figures_read_as_people_write_them() {
        assert_eq!(bytes(412_000_000), "412 MB");
        assert_eq!(bytes(1_800_000), "1.8 MB");
        assert_eq!(bytes(13_000), "13.0 kB");
        assert_eq!(bytes(900), "900 B");
        assert_eq!(bytes(7_600_000_000), "7.6 GB");
        assert_eq!(rate(387_500), "3.1 Mbit/s");
        assert_eq!(rate(60_000), "480 kbit/s");
        assert_eq!(duration(130), "2 m 10 s");
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(3_900), "1 h 5 m");
        assert_eq!(duration_rough(1_080), "18 minutes");
        assert_eq!(duration_rough(40), "40 seconds");
        assert_eq!(duration_rough(60), "60 seconds");
        assert_eq!(duration_rough(7_200), "2 hours");
        assert_eq!(thousands(3_412_001), "3,412,001");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
    }
}
