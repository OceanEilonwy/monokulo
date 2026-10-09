//! How the engine is performing and scaling, as people read it
//! (docs/engine_scaling.md section 6): the figures' formatting here, and
//! what the engine page's "Machine and links" strip and Scanning panel
//! show, worked out once for the page and for its live `machine` event.

use serde::Serialize;
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

/// A network's name as a word, for a title or the start of a sentence:
/// "Mainnet". The status page's network cards and the network badge use it.
pub fn network_name(network: &str) -> String {
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

/// Both processes' CPU and memory (docs/engine_scaling.md section 6), for
/// the engine page's "Machine and links" strip.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcesView {
    /// `None` when the engine didn't report its own (or didn't answer).
    pub engine: Option<ResourceReport>,
    pub monokulo: ResourceReport,
    pub now_unix: i64,
    /// The engine runs inside monokulo: one process, its CPU split by
    /// thread (`ResourceReport::hosted`) and its memory shared, so memory
    /// is shown once, for both.
    pub one_process: bool,
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
    what: String,
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

    /// The chart as a tile: the total now, what it is of and the hour's
    /// peak, and the stacked bands with any limit dashed across them. The
    /// split between the processes, and when the peak was, are in its
    /// title.
    fn tile(&self, key: &str, capacity: &str) -> Tile {
        let stacks = self.stacks();
        let (value, note, mut title) = match self.now_and_peak(&stacks) {
            Some((current, peak, peak_ago)) => {
                let total: f64 = current.iter().sum();
                let peak_text = format!("peak {}", (self.format)(peak));
                (
                    (self.format)(total),
                    if capacity.is_empty() {
                        peak_text
                    } else {
                        format!("{capacity} · {peak_text}")
                    },
                    format!(
                        "{} over the last hour, now {}{}; peak {} {}",
                        self.what,
                        self.split(&current),
                        if capacity.is_empty() {
                            String::new()
                        } else {
                            format!(" {capacity}")
                        },
                        (self.format)(peak),
                        ago(peak_ago)
                    ),
                )
            }
            None => (
                "–".to_owned(),
                "no samples yet".to_owned(),
                format!("{}: no samples yet", self.what),
            ),
        };
        for limit in &self.limits {
            title.push_str(&format!(". {} (dashed)", limit.label));
        }
        Tile {
            key: key.to_owned(),
            label: self.what.clone(),
            value,
            note,
            title,
            chart: TileChart::Stack {
                layers: self
                    .layers
                    .iter()
                    .enumerate()
                    .map(|(k, layer)| ChartLayer {
                        class: layer.class,
                        d: self.path(&stacks, k),
                    })
                    .collect(),
                limits: self
                    .limits
                    .iter()
                    .map(|limit| ChartLimit {
                        class: limit.class,
                        y: format!("{:.1}", self.y(limit.value)),
                        label: limit.label.clone(),
                    })
                    .collect(),
            },
        }
    }
}

/// One small multiple of the engine page's "Machine and links" strip: a
/// figure, a line under it and a small chart. Worked out here; the page
/// draws it (`views::engine`), and its script draws the same fields again
/// from each `machine` event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tile {
    /// Which tile: `cpu`, `memory`, `transfer`, `round-trip`,
    /// `first-byte`, `block-size` (with `engine-` or `monokulo-` before
    /// the first two when the processes are on different machines).
    pub key: String,
    pub label: String,
    pub value: String,
    pub note: String,
    /// The tile in a sentence, for its tooltip and screen readers.
    pub title: String,
    pub chart: TileChart,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TileChart {
    None,
    /// Bands stacked over an hour of 10-second slots, the first at the
    /// bottom (`viewBox="0 0 360 100"`), and limits dashed across.
    Stack {
        layers: Vec<ChartLayer>,
        limits: Vec<ChartLimit>,
    },
    /// An hour at a point a minute (`viewBox="0 0 59 16"`): a line per run
    /// of measured minutes, a dot for a run of one.
    Spark {
        runs: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartLayer {
    /// `chart-engine` or `chart-monokulo`.
    pub class: &'static str,
    /// The band's SVG path.
    pub d: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartLimit {
    pub class: &'static str,
    /// Where it crosses, on the chart's 0 to 100.
    pub y: String,
    pub label: String,
}

fn percent(value: f64) -> String {
    format!("{value:.0} %")
}

fn memory(value: f64) -> String {
    bytes(value.max(0.0) as u64)
}

const ENGINE: (&str, &str) = ("engine", "chart-engine");
const MONOKULO: (&str, &str) = ("monokulo", "chart-monokulo");

/// The CPU and memory tiles for `processes` stacked (first at the bottom),
/// all on one machine, their labels and keys after `who` ("Engine" when
/// the processes are apart). With `shared_memory`, the processes are one
/// process: memory is the last one's, under that name.
fn machine_tiles(
    processes: &[(&'static str, &'static str, &ResourceReport)],
    now: i64,
    shared_memory: Option<&'static str>,
    who: Option<&str>,
) -> Vec<Tile> {
    let Some((_, _, machine)) = processes.first() else {
        return Vec::new();
    };
    let (cpu_label, memory_label, cpu_key, memory_key) = match who {
        Some(who) => (
            format!("{who} CPU"),
            format!("{who} memory"),
            format!("{}-cpu", who.to_ascii_lowercase()),
            format!("{}-memory", who.to_ascii_lowercase()),
        ),
        None => (
            "CPU".to_owned(),
            "Memory".to_owned(),
            "cpu".to_owned(),
            "memory".to_owned(),
        ),
    };
    let cores = if machine.cpu_count == 1 {
        "of 1 core".to_string()
    } else {
        format!("of {} cores", machine.cpu_count)
    };
    let cpu = Chart {
        what: cpu_label,
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
    let memory_of: Vec<(&'static str, &'static str, &ResourceReport)> = match shared_memory {
        Some(name) => processes
            .last()
            .map(|(_, class, report)| (name, *class, *report))
            .into_iter()
            .collect(),
        None => processes.to_vec(),
    };
    let ram = Chart {
        what: memory_label,
        layers: memory_of
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
    vec![cpu.tile(&cpu_key, &cores), ram.tile(&memory_key, &of_ram)]
}

/// The strip's CPU and memory tiles: monokulo and the engine together,
/// each a band of one stacked chart, when they share a machine; a pair of
/// tiles each when they don't.
pub fn resource_tiles(view: &ResourcesView) -> Vec<Tile> {
    let now = view.now_unix;
    let monokulo = (MONOKULO.0, MONOKULO.1, &view.monokulo);
    match &view.engine {
        Some(engine) if view.one_process => machine_tiles(
            &[(ENGINE.0, ENGINE.1, engine), monokulo],
            now,
            Some("monokulo and the engine"),
            None,
        ),
        Some(engine) if engine.host_id == view.monokulo.host_id => {
            machine_tiles(&[(ENGINE.0, ENGINE.1, engine), monokulo], now, None, None)
        }
        Some(engine) => {
            let mut tiles =
                machine_tiles(&[(ENGINE.0, ENGINE.1, engine)], now, None, Some("Engine"));
            tiles.extend(machine_tiles(&[monokulo], now, None, Some("Monokulo")));
            tiles
        }
        // The engine didn't report its own: monokulo's alone.
        None => machine_tiles(&[monokulo], now, None, Some("Monokulo")),
    }
}

/// An hour of one of a node's figures, a point a minute, as runs of
/// measured minutes ("x,y x,y …"); a run of one minute is one point.
fn spark_runs(history: &[LinkPoint], now: i64, value: fn(&LinkPoint) -> u64) -> Vec<String> {
    const MINUTES: i64 = 60;
    let this_minute = now.div_euclid(60) * 60;
    let mut points: Vec<Option<u64>> = vec![None; MINUTES as usize];
    for point in history {
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
    runs
}

fn count(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The node the engine reads a network's chain from right now, for the
/// strip's link tiles.
#[derive(Debug, Clone, Copy)]
pub struct InUseNode<'a> {
    /// The engine's label for it (`host:port`).
    pub label: &'a str,
    /// Not the network's primary: the engine fell back to it.
    pub fallback: bool,
    pub link: Option<&'a LinkSnapshot>,
}

/// The Transfer, Round trip and First byte tiles: what the engine measured
/// of the link to the node it is using now, each with its last hour, the
/// tile naming the node. With no node in use there is nothing to measure.
pub fn link_tiles(node: Option<InUseNode<'_>>, now: i64) -> Vec<Tile> {
    type Figure = (
        &'static str,
        &'static str,
        fn(&LinkPoint) -> u64,
        fn(u64) -> String,
    );
    let figures: [Figure; 3] = [
        ("transfer", "Transfer", |p| p.rate_bytes_per_sec, rate),
        (
            "round-trip",
            "Round trip",
            |p| p.rtt_ms,
            |ms| format!("{ms} ms"),
        ),
        (
            "first-byte",
            "First byte",
            |p| p.ttfb_per_block_ms,
            |ms| format!("{ms} ms"),
        ),
    ];
    let now_of = |link: &LinkSnapshot| LinkPoint {
        minute_unix: now,
        rate_bytes_per_sec: link.rate_bytes_per_sec,
        rtt_ms: link.rtt_ms,
        ttfb_per_block_ms: link.ttfb_per_block_ms,
    };
    figures
        .iter()
        .map(|(key, label, value, format)| {
            let Some(node) = node else {
                return Tile {
                    key: (*key).to_owned(),
                    label: (*label).to_owned(),
                    value: "–".to_owned(),
                    note: "no node in use".to_owned(),
                    title: format!("{label}: the engine isn't using a node on this network right now."),
                    chart: TileChart::None,
                };
            };
            let name = if node.fallback {
                format!("{} · fallback", node.label)
            } else {
                node.label.to_owned()
            };
            match node.link {
                Some(link) if link.measured => {
                    let trouble = match (link.timeouts_last_hour, link.failures_last_hour) {
                        (0, 0) => "no timeouts or failures in the last hour".to_string(),
                        (timeouts, failures) => format!(
                            "{} and {} in the last hour",
                            count(timeouts, "timeout", "timeouts"),
                            count(failures, "failure", "failures")
                        ),
                    };
                    let measured = link
                        .last_measured_unix
                        .map(|at| format!("measured {}; ", ago(now - at)))
                        .unwrap_or_default();
                    let what = if *key == "first-byte" { "First byte of a block" } else { label };
                    Tile {
                        key: (*key).to_owned(),
                        label: (*label).to_owned(),
                        value: format(value(&now_of(link))),
                        note: name.clone(),
                        title: format!(
                            "{what} from {name}: {} now, the last hour drawn; {measured}{trouble}.",
                            format(value(&now_of(link)))
                        ),
                        chart: TileChart::Spark {
                            runs: spark_runs(&link.history, now, *value),
                        },
                    }
                }
                _ => Tile {
                    key: (*key).to_owned(),
                    label: (*label).to_owned(),
                    value: "–".to_owned(),
                    note: name.clone(),
                    title: format!(
                        "{label} from {name}: not measured yet. The engine assumes {} until its first block request.",
                        rate(node.link.map_or(0, |link| link.rate_bytes_per_sec))
                    ),
                    chart: TileChart::None,
                },
            }
        })
        .collect()
}

/// The Block size tile: the average of recent blocks and which way it's
/// going.
pub fn block_size_tile(scaling: Option<&NetworkScaling>) -> Tile {
    let (value, note, title) = match scaling {
        Some(scaling) => {
            let trend = trend(scaling.scan.block_size_trend);
            let average = bytes(scaling.scan.avg_block_bytes);
            (
                average.clone(),
                format!("average, {trend}"),
                format!("Recent blocks average {average}, {trend}."),
            )
        }
        None => (
            "–".to_owned(),
            "not reported".to_owned(),
            "The engine hasn't reported its block sizes.".to_owned(),
        ),
    };
    Tile {
        key: "block-size".to_owned(),
        label: "Block size".to_owned(),
        value,
        note,
        title,
        chart: TileChart::None,
    }
}

fn trend(trend: Trend) -> &'static str {
    match trend {
        Trend::Rising => "rising ↗",
        Trend::Falling => "falling ↘",
        Trend::Steady => "steady",
    }
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

/// One network's Scanning panel on the engine page: a short line for its
/// summary, a slow block if there is one, and its figures: how far
/// behind, what sets the pace, the requests, block sizes, memory and
/// round (docs/engine_scaling.md section 6).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScanningView {
    /// "at the tip · pace: link speed", "14 behind · pace: memory budget".
    pub preview: String,
    pub slow: Option<String>,
    pub rows: Vec<(&'static str, String)>,
}

pub fn scanning(
    network: &str,
    scaling: &NetworkScaling,
    active: Option<ActiveNode<'_>>,
) -> ScanningView {
    let scan = &scaling.scan;
    let preview = {
        let progress = if scaling.blocks_behind == 0 {
            "at the tip".to_owned()
        } else {
            format!("{} behind", thousands(scaling.blocks_behind))
        };
        let pace = if scaling.slow.is_some() {
            Some("a slow block")
        } else {
            match scaling.pace {
                Pace::CaughtUp => None,
                Pace::Memory => Some("pace: memory budget"),
                Pace::Link => Some("pace: link speed"),
                Pace::Cpu => Some("pace: CPU"),
                Pace::Round => Some("pace: round time"),
            }
        };
        match pace {
            Some(pace) => format!("{progress} · {pace}"),
            None => progress,
        }
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
        let mut text = format!(
            "Average {} ({})",
            bytes(scan.avg_block_bytes),
            trend(scan.block_size_trend)
        );
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
        // Blocks fetched ahead and let go of before their scan were fetched
        // for nothing; a larger budget would have kept them.
        if scan.discarded_cache_bytes_recent > 0 {
            text.push_str(&format!(
                " · {} fetched ahead was let go of unscanned in the last ten minutes, to be fetched again (a larger budget keeps more)",
                bytes(scan.discarded_cache_bytes_recent)
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
    ScanningView {
        preview,
        slow: scaling
            .slow
            .as_ref()
            .map(|slow| slow_block_message(network, slow, true)),
        rows: vec![
            ("Progress", progress),
            ("Pace set by", pace),
            ("Requests", request),
            ("Block size", block_size),
            ("Memory", memory_line),
            ("Round", round),
            ("Headers first", headers_first),
            ("Last 10 minutes", time),
        ],
    }
}

/// What the engine page's "Machine and links" strip and Scanning panel
/// show, from monokulo's cached copy of the engine's `/status`: drawn
/// with the page, and sent again to its script as the `machine` event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineView {
    /// CPU, Memory, Transfer, Round trip, First byte, Block size.
    pub tiles: Vec<Tile>,
    /// The engine and monokulo are stacked in a chart: the strip's head
    /// says which colour is which.
    pub stacked: bool,
    pub scanning: Option<ScanningView>,
    /// When the engine wrote the `/status` this is from (its clock).
    pub at_unix: i64,
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
            hosted_cpu_percent: None,
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

    fn stack(tile: &Tile) -> (&[ChartLayer], &[ChartLimit]) {
        match &tile.chart {
            TileChart::Stack { layers, limits } => (layers, limits),
            other => panic!("not a stacked chart: {other:?}"),
        }
    }

    /// On one machine the two processes stack: one figure for the total,
    /// its split in the title, and a band each (docs/engine_scaling.md
    /// section 6).
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
            one_process: false,
        };
        let tiles = resource_tiles(&view);
        let keys: Vec<&str> = tiles.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["cpu", "memory"]);
        let (cpu, ram) = (&tiles[0], &tiles[1]);
        assert_eq!((cpu.label.as_str(), cpu.value.as_str()), ("CPU", "25 %"));
        assert_eq!(cpu.note, "of 4 cores · peak 63 %");
        assert_eq!(
            cpu.title,
            "CPU over the last hour, now 25 % (engine 23 %, monokulo 2 %) of 4 cores; peak 63 % 70 seconds ago"
        );
        assert_eq!(ram.value, "508 MB");
        assert_eq!(ram.note, "of 8.0 GB · peak 590 MB");
        assert!(
            ram.title.starts_with(
                "Memory over the last hour, now 508 MB (engine 412 MB, monokulo 96.0 MB) of 8.0 GB"
            ),
            "{}",
            ram.title
        );
        // The engine's container limit is a dashed line on the memory chart.
        assert!(
            ram.title
                .ends_with("The engine's container limit: 2.0 GB (dashed)"),
            "{}",
            ram.title
        );
        let (layers, limits) = stack(ram);
        assert_eq!(limits.len(), 1);
        assert_eq!(
            (limits[0].class, limits[0].y.as_str()),
            ("chart-engine", "75.0")
        );
        let classes: Vec<&str> = layers.iter().map(|l| l.class).collect();
        assert_eq!(
            classes,
            ["chart-engine", "chart-monokulo"],
            "the engine is drawn first, at the bottom"
        );
        // Two runs (seven slots back, then the last two), so each band's
        // path has two parts.
        let (layers, limits) = stack(cpu);
        assert!(limits.is_empty());
        assert_eq!(layers[0].d.matches('M').count(), 2, "{}", layers[0].d);
    }

    /// The engine inside monokulo: one process, sampled once. CPU is split
    /// by the engine's threads, stacked as before; memory, which the two
    /// share, is one band, counted once.
    #[test]
    fn one_process_splits_cpu_and_shows_memory_once() {
        let process = report(
            "boot-1",
            vec![
                ResourceSample {
                    hosted_cpu_percent: Some(20.0),
                    ..sample(1, 25.0, 300)
                },
                ResourceSample {
                    hosted_cpu_percent: Some(20.0),
                    ..sample(0, 25.0, 300)
                },
            ],
        );
        let view = ResourcesView {
            engine: Some(process.hosted()),
            monokulo: process.without_hosted(),
            now_unix: NOW,
            one_process: true,
        };
        let tiles = resource_tiles(&view);
        assert!(
            tiles[0]
                .title
                .contains("now 25 % (engine 20 %, monokulo 5 %) of 4 cores"),
            "{}",
            tiles[0].title
        );
        assert_eq!(tiles[1].value, "300 MB", "once, not 600 MB");
        assert_eq!(stack(&tiles[1]).0.len(), 1, "one band for memory");
    }

    #[test]
    fn different_machines_are_shown_apart_and_a_silent_engine_says_so() {
        let apart = ResourcesView {
            engine: Some(report("boot-1", vec![sample(0, 23.0, 412)])),
            monokulo: report("boot-2", vec![sample(0, 2.0, 96)]),
            now_unix: NOW,
            one_process: false,
        };
        let tiles = resource_tiles(&apart);
        let labels: Vec<&str> = tiles.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Engine CPU",
                "Engine memory",
                "Monokulo CPU",
                "Monokulo memory"
            ]
        );
        assert_eq!(tiles[0].value, "23 %");
        assert_eq!(tiles[0].key, "engine-cpu");
        assert!(stack(&tiles[0]).0.len() == 1, "nothing is stacked");

        let silent = ResourcesView {
            engine: None,
            monokulo: report("boot-2", vec![]),
            now_unix: NOW,
            one_process: false,
        };
        let tiles = resource_tiles(&silent);
        assert_eq!(tiles[0].label, "Monokulo CPU");
        assert_eq!(tiles[0].note, "no samples yet");
    }

    fn link(measured: bool) -> LinkSnapshot {
        LinkSnapshot {
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
        }
    }

    /// The link tiles follow the node in use, naming it, a fallback said
    /// so; each figure has its hour.
    #[test]
    fn the_link_tiles_follow_the_node_in_use() {
        let measured = link(true);
        let tiles = link_tiles(
            Some(InUseNode {
                label: "node.example.com:18081",
                fallback: false,
                link: Some(&measured),
            }),
            NOW,
        );
        let figures: Vec<(&str, &str, &str)> = tiles
            .iter()
            .map(|t| (t.label.as_str(), t.value.as_str(), t.note.as_str()))
            .collect();
        assert_eq!(
            figures,
            [
                ("Transfer", "3.1 Mbit/s", "node.example.com:18081"),
                ("Round trip", "120 ms", "node.example.com:18081"),
                ("First byte", "45 ms", "node.example.com:18081"),
            ]
        );
        assert_eq!(
            tiles[0].title,
            "Transfer from node.example.com:18081: 3.1 Mbit/s now, the last hour drawn; measured 12 seconds ago; 2 timeouts and 1 failure in the last hour."
        );
        // The two latest minutes are a line; ten minutes back, alone, a dot.
        match &tiles[1].chart {
            TileChart::Spark { runs } => {
                assert_eq!(runs.len(), 2, "{runs:?}");
                assert!(!runs[0].contains(' ') && runs[1].contains(' '), "{runs:?}");
            }
            other => panic!("{other:?}"),
        }

        let fallback = link_tiles(
            Some(InUseNode {
                label: "backup.example.org:18089",
                fallback: true,
                link: Some(&measured),
            }),
            NOW,
        );
        assert_eq!(fallback[0].note, "backup.example.org:18089 · fallback");

        let unmeasured = link(false);
        let tiles = link_tiles(
            Some(InUseNode {
                label: "node.example.com:18081",
                fallback: false,
                link: Some(&unmeasured),
            }),
            NOW,
        );
        assert_eq!(tiles[0].value, "–");
        assert!(
            tiles[0]
                .title
                .contains("not measured yet. The engine assumes 3.1 Mbit/s"),
            "{}",
            tiles[0].title
        );
        assert_eq!(tiles[0].chart, TileChart::None);

        let none = link_tiles(None, NOW);
        assert_eq!(none.len(), 3);
        assert!(none.iter().all(|t| t.note == "no node in use"));
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
                discarded_cache_bytes_recent: 0,
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

    fn row<'a>(view: &'a ScanningView, label: &str) -> &'a str {
        view.rows
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no {label} in {view:?}"))
    }

    #[test]
    fn the_scanning_panel_says_what_sets_the_pace_and_why() {
        let node = || {
            Some(ActiveNode {
                label: "node.example:18089",
                rate_bytes_per_sec: Some(387_500),
            })
        };
        let view = scanning("mainnet", &scaling(Pace::Link), node());
        assert_eq!(view.preview, "14 behind · pace: link speed");
        assert_eq!(
            row(&view, "Progress"),
            "14 blocks behind · 3.2 blocks a minute · caught up in about 4 minutes"
        );
        assert_eq!(
            row(&view, "Pace set by"),
            "Link speed: node.example:18089 at 3.1 Mbit/s. A faster node would speed it up."
        );
        assert_eq!(
            row(&view, "Requests"),
            "1 block a request, set by the link's speed · block 3,412,001 in progress for 2 m 10 s"
        );
        assert_eq!(
            row(&view, "Block size"),
            "Average 1.8 MB (rising ↗) · largest recent 412 MB (block 3,411,990), took 18 m 0 s"
        );
        assert_eq!(row(&view, "Memory"), "Budget 256 MB · block cache peak 301 MB in the last hour · this machine allows up to 1,536 MB");
        let mut discarding = scaling(Pace::Memory);
        discarding.scan.discarded_cache_bytes_recent = 9_400_000;
        let view = scanning("mainnet", &discarding, node());
        assert_eq!(view.preview, "14 behind · pace: memory budget");
        assert!(row(&view, "Memory").ends_with("this machine allows up to 1,536 MB · 9.4 MB fetched ahead was let go of unscanned in the last ten minutes, to be fetched again (a larger budget keeps more)"));
        assert_eq!(row(&view, "Round"), "Deadline 10 s (the base)");
        assert_eq!(
            row(&view, "Headers first"),
            "Off: blocks are fetched whole without asking their size first"
        );
        assert_eq!(
            row(&view, "Last 10 minutes"),
            "Fetching 6 m 40 s, scanning 12 s"
        );
        assert!(row(&view, "Pace set by")
            .starts_with("The scan memory budget, which caps each request."));

        let mut caught_up = scaling(Pace::CaughtUp);
        caught_up.blocks_behind = 0;
        caught_up.round_deadline_secs = 45;
        let view = scanning("mainnet", &caught_up, None);
        assert_eq!(view.preview, "at the tip");
        assert_eq!(row(&view, "Progress"), "At the node's tip.");
        assert!(row(&view, "Round").starts_with("Deadline 45 s, raised from 10 s"));
        let mut tip_by_link = scaling(Pace::Link);
        tip_by_link.blocks_behind = 0;
        assert_eq!(
            scanning("mainnet", &tip_by_link, None).preview,
            "at the tip · pace: link speed"
        );

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
        let view = scanning("mainnet", &paged, None);
        assert!(row(&view, "Requests")
            .contains("in pages: 41,000 of 97,000 transactions scanned, 100 a page"));
        assert!(row(&view, "Headers first").starts_with(
            "On for about 50 minutes more: a block request ran out of time or came back too large"
        ));

        let mut slow = scaling(Pace::Link);
        slow.slow = Some(SlowBlock {
            height: 3_412_001,
            wire_bytes: Some(412_000_000),
            elapsed_secs: 130,
            node: Some("node.example:18089".into()),
            rate_bytes_per_sec: Some(387_500),
            remaining_secs: Some(1_080),
        });
        let view = scanning("mainnet", &slow, node());
        assert_eq!(view.preview, "14 behind · a slow block");
        assert!(view.slow.as_deref().is_some_and(|m| m.starts_with("Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far, at 3.1 Mbit/s from node.example:18089.")), "{view:?}");
    }

    #[test]
    fn the_block_size_tile_gives_the_average_and_its_trend() {
        let tile = block_size_tile(Some(&scaling(Pace::Link)));
        assert_eq!(
            (tile.value.as_str(), tile.note.as_str()),
            ("1.8 MB", "average, rising ↗")
        );
        assert_eq!(block_size_tile(None).note, "not reported");
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
