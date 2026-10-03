//! The engine page (`docs/engine_visualizer.md`): the engine's scanner, for
//! admins. Rendered here whole, so without JavaScript it is a point-in-time
//! page with a Reload button; `static/engine-view.js` then follows the
//! network live, drawing each frame's [`Presented`] into the same elements.

use maud::{html, Markup, PreEscaped};

use super::{layout_with_head, reload_button, PageChrome};
use crate::engine_view::machine::Mark;
use crate::engine_view::present::{Bar, ChainView, Lane, Panel, Presented, RibbonMark};
use crate::views::scaling::thousands;

/// Blocks drawn without JavaScript (the script fits the strip's width).
const CELLS: u64 = 40;
/// Blocks kept around the lowest group when the span is cut.
const LOW_CELLS: u64 = 7;
/// One cell and its gap, in pixels: what the script lays out with too.
const CELL_PX: u64 = 26;
/// The strip's left padding, in pixels.
const STRIP_PAD_PX: u64 = 14;
/// Events listed without JavaScript.
pub const MARKS_SHOWN: usize = 60;

pub struct EnginePage {
    /// The networks the engine scans, for the tabs.
    pub networks: Vec<String>,
    pub network: String,
    pub view: Option<Presented>,
    /// Newest first.
    pub marks: Vec<Mark>,
    /// Why the engine couldn't be read, if it couldn't.
    pub error: Option<String>,
}

const ENGINE_STYLE: &str = r#"
.wrap.engine-page { max-width: 1880px; display: grid; gap: var(--space-sm); padding-bottom: var(--space-xl); }
.engine-page h1 { border: 0; margin: 0; padding: 0; font-size: 1.3rem; }
.engine-page h2 { border: 0; margin: 0; padding: 0; font-size: 0.85rem; font-weight: 800; }
.engine-top { display: flex; flex-wrap: wrap; gap: var(--space-sm) var(--space-md); align-items: center; margin-top: var(--space-sm); }
.engine-tabs { display: flex; gap: var(--space-xs); }
.engine-tabs a { border: 1px solid var(--btn-border); border-radius: var(--radius-sm); padding: 2px 9px; font-weight: 700; font-size: 0.8rem; color: var(--btn-ink); text-decoration: none; }
.engine-tabs a[aria-current] { box-shadow: inset 0 -3px 0 var(--accent-text); }
.engine-card { background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-md); padding: var(--space-sm) var(--space-md); min-width: 0; }
.engine-card > header { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--space-xs) var(--space-md); margin-bottom: var(--space-xs); }
.engine-hint { font-size: 0.72rem; color: var(--muted); }
.engine-right { margin-left: auto; }
.engine-chip { display: inline-block; font-size: 0.7rem; font-weight: 700; border-radius: 99px; padding: 0 7px; border: 1px solid var(--line-strong); white-space: nowrap; }
.engine-chip.ok { background: var(--tint-success); border-color: var(--success); }
.engine-chip.warn { background: var(--tint-warning); border-color: var(--warning); }
.engine-chip.err { background: var(--tint-error); border-color: var(--error); }
.engine-chip.hi { background: var(--tint-highlight); border-color: var(--accent); }
.engine-timeline { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; gap: var(--space-md); align-items: center; }
.engine-timeline .tl-track { position: relative; display: grid; gap: 2px; }
.engine-timeline canvas { width: 100%; height: 32px; display: block; border-radius: 4px; touch-action: none; cursor: grab; }
.engine-timeline canvas.panning { cursor: grabbing; }
.tl-axis { position: relative; height: 12px; font-size: 0.62rem; color: var(--muted); }
.tl-axis span { position: absolute; transform: translateX(-50%); white-space: nowrap; }
.tl-axis span:first-child { transform: none; }
.tl-axis span:last-child { transform: translateX(-100%); }
.tl-read { font-size: 0.75rem; font-weight: 700; display: flex; align-items: center; gap: 6px; white-space: nowrap; min-width: 13rem; }
.tl-read i { width: 9px; height: 9px; border-radius: 50%; background: var(--success); }
.tl-read.paused i { background: var(--warning); }
.tl-tip { position: absolute; z-index: 30; pointer-events: none; transform: translate(-50%, -100%); top: -4px; background: var(--ink); color: var(--paper-raised); font-size: 0.7rem; font-weight: 700; padding: 3px 7px; border-radius: 4px; white-space: nowrap; max-width: 30rem; overflow: hidden; text-overflow: ellipsis; }
.engine-summary { display: grid; grid-template-columns: repeat(6, minmax(0, 1fr)); padding: 0; }
.engine-summary > div { padding: 5px var(--space-md); border-left: 1px solid var(--line); min-width: 0; display: grid; grid-template-columns: auto 1fr; column-gap: var(--space-sm); align-items: baseline; }
.engine-summary > div:first-child { border-left: 0; }
.engine-summary .k { grid-column: 1 / -1; font-size: 0.62rem; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
.engine-summary .v { font-size: 1.05rem; font-weight: 800; font-variant-numeric: tabular-nums; }
.engine-summary .s { font-size: 0.7rem; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.t-chain { --tier: var(--viz-tier-chain); }
.t-blocks { --tier: var(--viz-tier-blocks); }
.t-mempool { --tier: var(--viz-tier-mempool); }
.t-settlement { --tier: var(--viz-tier-settlement); }
.t-upkeep { --tier: var(--viz-tier-upkeep); }
.t-other { --tier: var(--line-strong); }
.tierchip { display: inline-flex; align-items: center; gap: 5px; font-size: 0.7rem; font-weight: 700; white-space: nowrap; }
.tierchip::before { content: ""; width: 9px; height: 9px; border-radius: 3px; background: var(--tier); flex: none; }
.engine-main { display: grid; grid-template-columns: minmax(0, 1fr) 380px; gap: var(--space-sm); align-items: start; }
.engine-left { display: grid; gap: var(--space-sm); min-width: 0; }
.legend { display: flex; flex-wrap: wrap; gap: 4px 10px; font-size: 0.66rem; color: var(--muted); }
.legend span { display: inline-flex; align-items: center; gap: 4px; }
.legend .cell { width: 11px; height: 13px; border-radius: 3px; }
.chain-row { display: grid; grid-template-columns: minmax(0, 1fr) 176px; gap: 10px; align-items: start; }
.strip-scroll { overflow: hidden; }
.strip { position: relative; padding-inline: 14px 8px; padding-top: 26px; height: 128px; }
.cells { display: flex; gap: 4px; align-items: flex-end; height: 28px; }
.cell { position: relative; width: 22px; height: 26px; border-radius: 4px; border: 1.5px solid var(--viz-cell-edge); background: var(--viz-cell-recorded); overflow: hidden; flex: none; transition: background 0.3s, border-color 0.3s, opacity 0.3s; }
.cell .fill { position: absolute; left: 0; top: 0; bottom: 0; width: 0; background: var(--viz-tier-blocks); opacity: 0.85; transition: width 0.2s linear; }
.cell.new { background: var(--paper-raised); border-style: dashed; }
.cell.cached { background: var(--viz-cell-cached); border-color: var(--viz-tier-blocks); }
.cell.ghost { border-style: dotted; background: transparent; opacity: 0.6; }
.cell.reorg { border-color: var(--error); background: repeating-linear-gradient(135deg, var(--tint-error) 0 4px, var(--paper-raised) 4px 8px); }
.cell.enter { animation: cell-enter 0.6s cubic-bezier(0.3, 1.4, 0.5, 1); }
.cell.flash { box-shadow: 0 0 0 3px var(--tint-highlight); }
.cell.probe::after { content: ""; position: absolute; inset: -1px; border: 2px solid var(--viz-tier-chain); border-radius: 4px; }
.cell .save { position: absolute; right: 1px; top: 1px; width: 7px; height: 7px; border-radius: 2px; background: var(--viz-saved); }
@keyframes cell-enter { from { transform: translateX(60px); opacity: 0; } }
.brk { width: 60px; height: 26px; flex: none; display: grid; place-items: center; font-size: 0.62rem; font-weight: 700; color: var(--muted); border-inline: 2px dotted var(--line-strong); text-align: center; line-height: 1.1; }
.axis { position: absolute; left: 0; right: 0; top: 56px; height: 12px; font-size: 0.62rem; color: var(--muted); }
.axis span { position: absolute; transform: translateX(-50%); white-space: nowrap; }
.marks span { position: absolute; top: 0; font-size: 0.62rem; font-weight: 800; white-space: nowrap; padding: 0 5px; border-radius: 4px; transition: left 0.5s; }
.marks span::after { content: ""; position: absolute; top: 15px; height: 11px; border-left: 2px solid currentColor; }
.marks .m-tip { transform: translateX(-10px); background: var(--paper-raised); border: 1px solid var(--line-strong); }
.marks .m-tip::after { left: 9px; }
.marks .m-hw { transform: translateX(calc(-100% + 10px)); color: var(--accent-text); }
.marks .m-hw::after { right: 9px; }
.marks .m-win { top: 23px; height: 2px; padding: 0; background: var(--viz-tier-chain); }
.marks .m-win::after { display: none; }
.pills { position: absolute; left: 0; right: 0; top: 72px; height: 50px; }
.pill { position: absolute; top: 0; transform: translateX(-12px); display: inline-flex; align-items: center; font-size: 0.68rem; font-weight: 800; padding: 1px 8px 1px 6px; border-radius: 99px; border: 1.5px solid var(--line-strong); background: var(--paper-raised); white-space: nowrap; transition: left 0.45s cubic-bezier(0.4, 0, 0.2, 1); }
.pill::before { content: ""; position: absolute; left: 10px; top: -8px; height: 7px; border-left: 2px solid currentColor; }
.pill.frontier { transform: translateX(calc(-100% + 12px)); border-color: var(--accent); background: var(--tint-highlight); }
.pill.frontier::before { left: auto; right: 10px; }
.pill.catchup { top: 24px; }
.pill.catchup::before { top: -32px; height: 31px; }
.pill.busy { animation: pill-busy 0.6s ease-in-out infinite alternate; }
.pill.waiting { opacity: 0.55; }
@keyframes pill-busy { to { box-shadow: 0 0 0 4px var(--tint-highlight); } }
.nodes { display: grid; gap: 6px; }
.node { border: 1px solid var(--line); border-radius: var(--radius-sm); padding: 5px 8px; font-size: 0.7rem; }
.node .nm { font-weight: 800; display: flex; gap: 6px; align-items: center; font-size: 0.75rem; }
.node .call { font-family: var(--font-mono); font-size: 0.66rem; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.node.spark { animation: node-spark 0.8s; }
@keyframes node-spark { 30% { box-shadow: 0 0 0 4px var(--tint-highlight); } }
.round-head { flex-wrap: nowrap !important; }
.round-head .round-state { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.lanes { display: grid; grid-template-columns: 112px minmax(0, 1fr) 250px; grid-auto-rows: 18px; gap: 2px 10px; align-items: center; }
.lane-label { font-size: 0.7rem; font-weight: 700; display: flex; justify-content: space-between; }
.lane-label small { color: var(--muted); font-weight: 600; }
.track { position: relative; height: 15px; background: var(--surface-sunken); border-radius: 3px; overflow: hidden; }
.bar { position: absolute; top: 2px; bottom: 2px; min-width: 3px; border-radius: 2px; background: var(--tier); transition: left 0.3s, width 0.3s; }
.bar.p2 { background: repeating-linear-gradient(135deg, var(--tier) 0 4px, color-mix(in srgb, var(--tier) 40%, var(--paper-raised)) 4px 7px); }
.share { position: absolute; top: 0; bottom: 0; border: 1.5px dashed var(--line-strong); border-radius: 3px; transition: left 0.3s, width 0.3s; }
.playhead { position: absolute; top: 0; bottom: 0; width: 2px; background: var(--ink); transition: left 0.3s; z-index: 2; }
.outcome { font-size: 0.7rem; height: 18px; display: flex; align-items: center; overflow: hidden; white-space: nowrap; min-width: 0; }
.outcome .engine-chip { line-height: 14px; overflow: hidden; text-overflow: ellipsis; max-width: 100%; }
.ruler { position: relative; height: 18px; }
.ruler-label { position: absolute; top: 3px; transform: translateX(-50%); font: 700 0.62rem/14px var(--font-mono); background: var(--ink); color: var(--paper-raised); padding: 0 5px; border-radius: 3px; white-space: nowrap; transition: left 0.3s; }
.ribbon-row { display: grid; grid-template-columns: 112px minmax(0, 1fr) 250px; gap: 10px; align-items: end; margin-top: 6px; }
.ribbon { display: flex; justify-content: flex-end; align-items: flex-end; height: 30px; overflow: hidden; border-bottom: 1px solid var(--line); }
.rbar { width: 8px; display: flex; flex-direction: column-reverse; flex: none; border-radius: 2px 2px 0 0; overflow: hidden; }
.rbar i { display: block; background: var(--tier); }
.rgap { flex: none; width: 14px; height: 9px; position: relative; display: inline-block; }
.rgap::before { content: ""; position: absolute; left: 2px; right: 2px; bottom: 1px; border-bottom: 2px dotted var(--muted); }
.rgap.woken::before { right: 8px; }
.rgap.woken::after { content: ""; position: absolute; right: 1px; bottom: 0; width: 5px; height: 7px; border: 1.5px solid var(--viz-tier-chain); border-radius: 1.5px; background: var(--paper-raised); }
.engine-side { display: grid; gap: 5px; }
details.mini { background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-md); }
details.mini > summary { list-style: none; display: grid; grid-template-columns: 96px minmax(0, 1fr) 12px; gap: var(--space-sm); align-items: center; padding: 5px 10px; cursor: pointer; min-height: 32px; }
details.mini > summary::-webkit-details-marker { display: none; }
details.mini > summary::after { content: ""; width: 6px; height: 6px; border-right: 2px solid var(--muted); border-bottom: 2px solid var(--muted); transform: rotate(-45deg); transition: transform 0.2s; }
details.mini[open] > summary::after { transform: rotate(45deg); }
details.mini .sum { display: flex; align-items: center; gap: 6px; min-width: 0; font-size: 0.75rem; overflow: hidden; white-space: nowrap; }
details.mini .sum > span { overflow: hidden; text-overflow: ellipsis; }
details.mini .body { padding: 2px 10px 8px; font-size: 0.75rem; border-top: 1px solid var(--line); }
details.mini.alert { border-color: var(--error); }
.kv { display: grid; grid-template-columns: 1fr auto; gap: 2px 10px; padding-top: 6px; }
.kv b { font-variant-numeric: tabular-nums; font-weight: 700; }
.explain { color: var(--muted); font-size: 0.7rem; margin: 6px 0 0; white-space: normal; }
.dots { display: inline-flex; gap: 3px; flex: none; }
.dot { width: 7px; height: 7px; border-radius: 50%; border: 1.5px solid var(--viz-tier-mempool); background: var(--paper-raised); }
.dot.match { background: var(--accent); border-color: var(--accent-text); }
.beat { width: 7px; height: 7px; border-radius: 50%; background: var(--viz-tier-mempool); opacity: 0.25; flex: none; }
.beat.lit, .upd i.lit { opacity: 1; }
.upd { display: inline-flex; gap: 3px; flex: none; }
.upd i { width: 7px; height: 7px; border-radius: 2px; background: var(--viz-tier-upkeep); opacity: 0.25; }
.minibars { display: inline-flex; gap: 2px; align-items: flex-end; height: 14px; flex: none; }
.minibars i { width: 5px; background: var(--line-strong); border-radius: 1px; height: 2px; }
.saved-glyph { display: inline-block; width: 8px; height: 8px; border-radius: 2px; background: var(--viz-saved); flex: none; }
.engine-state { display: inline-block; font-size: 0.66rem; font-weight: 700; border-radius: 99px; padding: 0 6px; border: 1px solid var(--state-border); background: var(--state-bg); color: var(--state-ink); }
.engine-events { max-height: 260px; overflow-y: auto; }
.engine-events table { width: 100%; font-size: 0.75rem; }
.engine-events td { padding: 2px 8px; }
.engine-events tr.key td:last-child { font-weight: 700; }
.engine-events tr.now { box-shadow: inset 3px 0 0 var(--accent); }
.engine-events tbody tr { cursor: pointer; }
.engine-filters { display: flex; flex-wrap: wrap; gap: var(--space-sm); }
.engine-filters label { display: inline-flex; gap: 3px; align-items: center; font-size: 0.7rem; }
.token { position: absolute; z-index: 20; pointer-events: none; width: 12px; height: 12px; border-radius: 3px; border: 2px solid var(--paper-raised); background: var(--tier, var(--ink)); box-shadow: 0 0 0 1px var(--ink); }
.token.pkt { width: auto; height: auto; padding: 0 5px; font: 700 0.62rem/15px var(--font-mono); color: var(--paper-raised); background: var(--viz-tier-blocks); border: 0; box-shadow: none; }
.token.pkt.chain { background: var(--viz-tier-chain); }
.token.pkt.mempool { background: var(--viz-tier-mempool); }
.token.save { width: 10px; height: 10px; border-radius: 2px; background: var(--viz-saved); border: 1.5px solid var(--paper-raised); box-shadow: none; }
.token.envelope { width: 15px; height: 10px; border-radius: 2px; background: var(--paper-raised); border: 1.5px solid var(--ink); box-shadow: none; }
.token.payment { border-radius: 50%; background: var(--accent); }
.ghostcell { position: absolute; z-index: 15; pointer-events: none; }
#engine-stage { position: relative; }
@media (max-width: 1150px) {
  .engine-main { grid-template-columns: minmax(0, 1fr); }
  .engine-side { grid-template-columns: repeat(2, minmax(0, 1fr)); align-items: start; }
  .engine-summary { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .lanes, .ribbon-row { grid-template-columns: 96px minmax(0, 1fr); }
  .lanes .outcome { grid-column: 2; }
  .ribbon-row > :last-child { display: none; }
}
@media (max-width: 640px) {
  .engine-side { grid-template-columns: minmax(0, 1fr); }
  .engine-summary { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .chain-row { grid-template-columns: minmax(0, 1fr); }
  .engine-timeline { grid-template-columns: minmax(0, 1fr); }
}
@media (prefers-reduced-motion: reduce) {
  .engine-page *, .engine-page *::before, .engine-page *::after { animation-duration: 0.01ms !important; animation-iteration-count: 1 !important; transition-duration: 0.15s !important; }
}
"#;

const TIERS: [(&str, &str); 5] = [
    ("chain", "Chain"),
    ("blocks", "Blocks"),
    ("mempool", "Mempool"),
    ("settlement", "Settlement"),
    ("upkeep", "Upkeep"),
];

pub fn page(chrome: &PageChrome, page: &EnginePage) -> Markup {
    let head = html! {
        style { (PreEscaped(ENGINE_STYLE)) }
        script src="/static/engine-view.js" defer {}
    };
    let body = html! {
        div id="engine-stage" {
        main class="wrap engine-page" data-network=(page.network) {
            div class="engine-top" {
                nav class="context-nav" aria-label="Breadcrumb" { a href="/status" { "Status" } }
                h1 { "Engine" }
                @if page.networks.len() > 1 {
                    nav class="engine-tabs" aria-label="Network" {
                        @for network in &page.networks {
                            a href=(format!("/status/engine?network={network}"))
                                aria-current=[(network == &page.network).then_some("page")] { (network) }
                        }
                    }
                }
                span class="engine-right" { (reload_button(&format!("/status/engine?network={}", page.network))) }
            }
            @if let Some(error) = &page.error {
                p class="error" role="alert" { "The engine's activity could not be read: " (error) }
            }
            @if let Some(view) = &page.view {
                (timeline())
                (summary(view))
                div class="engine-main" {
                    div class="engine-left" {
                        (chain(&view.chain))
                        (round(view))
                    }
                    (side(view))
                }
            }
            (events(&page.marks))
        }
        }
    };
    layout_with_head(chrome, "Engine - Monokulo", head, body)
}

/// The timeline: drawn by the script, so hidden until it runs.
fn timeline() -> Markup {
    html! {
        section class="engine-card engine-timeline" id="engine-timeline" aria-label="Timeline" hidden {
            div { button class="btn" id="tl-play" type="button" { "Pause" } " " button class="btn" id="tl-live" type="button" disabled { "Live" } }
            div class="tl-track" id="tl-track" {
                canvas id="tl" tabindex="0" role="slider"
                    aria-label="Timeline. Click to move the playback position, drag to move along the history, scroll to zoom. Left and right jump between key events, with Shift between any events; Page Up and Page Down move along the history; plus and minus zoom; End returns to live."
                    aria-valuemin="0" aria-valuemax="100" aria-valuenow="100" {}
                div class="tl-axis" id="tl-axis" {}
            }
            div class="tl-read" id="tl-read" { i {} span id="tl-text" { "Live" } }
        }
    }
}

fn summary(view: &Presented) -> Markup {
    html! {
        section class="engine-card engine-summary" id="engine-summary" aria-label="Summary" {
            @for figure in &view.summary {
                div { span class="k" { (figure.label) } span class="v" { (figure.value) } span class="s" { (figure.note) } }
            }
        }
    }
}

/// The blocks to draw from `chain` in `cells` cells: `None` is the cut.
pub fn visible_blocks(chain: &ChainView, cells: u64) -> Vec<Option<u64>> {
    let (Some(tip), Some(high_water)) = (chain.tip, chain.high_water) else {
        return Vec::new();
    };
    let right = tip.max(high_water) + 1;
    let lowest = chain.lowest.unwrap_or(high_water).min(high_water);
    let left = lowest
        .saturating_sub(1)
        .min(right.saturating_sub(cells.saturating_sub(1)));
    if right - left < cells {
        return (left..=right).map(Some).collect();
    }
    let low_end = lowest.saturating_sub(1) + LOW_CELLS - 1;
    // The cut is about three cells wide.
    let high_start = right - (cells - LOW_CELLS - 4);
    (lowest.saturating_sub(1)..=low_end)
        .map(Some)
        .chain(std::iter::once(None))
        .chain((high_start..=right).map(Some))
        .collect()
}

fn chain(chain: &ChainView) -> Markup {
    let blocks = visible_blocks(chain, CELLS);
    // Each cell's centre, in the strip's pixels; the cut is three cells
    // wide (`.brk`, 60 px with its gap).
    let mut centres = Vec::with_capacity(blocks.len());
    let mut x = STRIP_PAD_PX + 11;
    for block in &blocks {
        match block {
            Some(height) => {
                centres.push((*height, x));
                x += CELL_PX;
            }
            None => x += 64,
        }
    }
    let centre = |height: u64| {
        centres
            .iter()
            .find(|(h, _)| *h == height)
            .or_else(|| centres.iter().rev().find(|(h, _)| *h < height))
            .map_or(0, |(_, x)| *x)
    };
    let tip = chain.tip.unwrap_or(0);
    let high_water = chain.high_water.unwrap_or(0);
    html! {
        section class="engine-card t-blocks" id="engine-chain" aria-labelledby="h-chain" {
            header {
                h2 id="h-chain" { "Chain" }
                div class="legend" {
                    span { i class="cell" aria-hidden="true" {} "recorded" }
                    span { i class="cell new" aria-hidden="true" {} "at the node" }
                    span { i class="cell cached" aria-hidden="true" {} "in the cache" }
                    span { i class="cell reorg" aria-hidden="true" {} "replaced" }
                    span { i class="saved-glyph" aria-hidden="true" {} "saved to disk" }
                }
                span class="engine-chip engine-right" id="cache-chip" { (chain.cache) }
            }
            div class="chain-row" {
                div class="strip-scroll" id="strip-scroll" {
                    div class="strip" id="strip" {
                        div class="marks" id="chain-marks" {
                            @if chain.tip.is_some() {
                                span class="m-tip" style=(format!("left:{}px", centre(tip))) { "node tip" }
                            }
                            @if high_water < tip {
                                span class="m-hw" style=(format!("left:{}px", centre(high_water))) { "scanned to" }
                            }
                        }
                        div class="cells" id="cells" {
                            @for block in &blocks {
                                @match block {
                                    Some(height) => (cell(chain, *height)),
                                    None => div class="brk" { "…" },
                                }
                            }
                        }
                        div class="axis" id="chain-axis" {
                            @for (height, x) in centres.iter().filter(|(h, _)| h % 5 == 0 && *h <= tip) {
                                span style=(format!("left:{x}px")) { (thousands(*height)) }
                            }
                        }
                        div class="pills" id="pills" {
                            @for group in &chain.groups {
                                div class=(format!("pill {}{}{}", if group.frontier { "frontier" } else { "catchup" }, if group.busy { " busy" } else { "" }, if group.waiting { " waiting" } else { "" }))
                                    data-id=(group.id) title=(group.title)
                                    style=(format!("left:{}px", centre(group.cursor))) { (group.label) }
                            }
                        }
                    }
                }
                div class="nodes" id="nodes" {
                    @for node in &chain.nodes {
                        div class="node" { div class="nm" { (node.label) " " span class=(format!("engine-chip {}", node.tone)) { (node.chip) } } }
                    }
                    div class="node" id="node-call" { div class="call" { (chain.call) } }
                }
            }
        }
    }
}

fn cell(chain: &ChainView, height: u64) -> Markup {
    let tip = chain.tip.unwrap_or(0);
    let high_water = chain.high_water.unwrap_or(0);
    let state = if height > tip {
        " ghost"
    } else if chain.replaced.contains(&height) {
        " reorg"
    } else if chain.cached.contains(&height) {
        " cached"
    } else if height > high_water {
        " new"
    } else {
        ""
    };
    let checkpoint = chain.checkpoints.iter().find(|(h, _)| *h == height);
    let fill = match chain.scanning {
        Some((h, done)) if h == height => done,
        _ => checkpoint.map_or(0.0, |(_, done)| *done),
    };
    html! {
        div class=(format!("cell{state}")) data-h=(height) title=(format!("Block {}", thousands(height))) {
            span class="fill" style=(format!("width:{:.0}%", fill * 100.0)) {}
            @if checkpoint.is_some() { span class="save" {} }
        }
    }
}

fn round(view: &Presented) -> Markup {
    html! {
        section class="engine-card" id="engine-round" aria-labelledby="h-round" {
            @match &view.round {
                Some(round) => {
                    header class="round-head" {
                        h2 id="h-round" { (round.title) }
                        span class="engine-hint round-state" { (round.state) }
                    }
                    div class="lanes" {
                        @for lane in &round.lanes {
                            (lane_row(lane, round.scale_ms, round.elapsed_ms))
                        }
                        div {}
                        div class="ruler" { span class="ruler-label" style=(left_pct(round.elapsed_ms, round.scale_ms)) { (round.elapsed) } }
                        div {}
                    }
                }
                None => {
                    header { h2 id="h-round" { "Round" } span class="engine-hint" { "No round recorded yet." } }
                }
            }
            div class="ribbon-row" {
                span class="engine-hint" { "Last rounds" }
                div class="ribbon" id="ribbon" aria-label="Recent rounds" {
                    @for mark in &view.ribbon {
                        @match mark {
                            RibbonMark::Round { height, parts, title } => {
                                span class="rbar" style=(format!("height:{height}px")) title=(title) {
                                    @for (tier, part) in parts {
                                        i class=(format!("t-{tier}")) style=(format!("height:{:.1}%", part * 100.0)) {}
                                    }
                                }
                            }
                            RibbonMark::Sleep { woken, title } => {
                                i class=(if *woken { "rgap woken" } else { "rgap" }) title=(title) {}
                            }
                        }
                    }
                }
                div class="legend" {
                    span { i class="rgap" aria-hidden="true" {} "slept" }
                    span { i class="rgap woken" aria-hidden="true" {} "woken by a new block" }
                    span { "no gap: work left" }
                }
            }
        }
    }
}

fn lane_row(lane: &Lane, scale_ms: u64, elapsed_ms: u64) -> Markup {
    let tier = lane.tier.to_string();
    html! {
        div class="lane-label" { span class=(format!("tierchip t-{tier}")) { (lane.name) } small { (lane.share) } }
        div class=(format!("track t-{tier}")) {
            @if let Some((start, ms)) = lane.reserved {
                div class="share" style=(span_style(start, ms, scale_ms)) {}
            }
            @for bar in &lane.bars {
                (bar_div(bar, scale_ms))
            }
            div class="playhead" style=(left_pct(elapsed_ms, scale_ms)) {}
        }
        div class="outcome" {
            @if let Some(chip) = &lane.outcome {
                span class=(format!("engine-chip {}", chip.tone)) title=(chip.text) { (chip.text) }
            }
        }
    }
}

fn bar_div(bar: &Bar, scale_ms: u64) -> Markup {
    html! {
        div class=(if bar.leftover { "bar p2" } else { "bar" }) style=(span_style(bar.start_ms, bar.ms, scale_ms)) {}
    }
}

fn pct(ms: u64, scale_ms: u64) -> f64 {
    (ms as f64 / scale_ms.max(1) as f64 * 100.0).min(100.0)
}

fn left_pct(ms: u64, scale_ms: u64) -> String {
    format!("left:{:.2}%", pct(ms, scale_ms).min(99.5))
}

fn span_style(start: u64, ms: u64, scale_ms: u64) -> String {
    format!(
        "left:{:.2}%;width:{:.2}%",
        pct(start, scale_ms),
        pct(ms, scale_ms).max(0.5)
    )
}

fn side(view: &Presented) -> Markup {
    let side = &view.side;
    html! {
        aside class="engine-side" id="engine-side" aria-label="Details" {
            details class=(if side.reorg.alert { "mini t-chain alert" } else { "mini t-chain" }) id="d-reorg" open[side.reorg.alert] {
                summary { span class="tierchip t-chain" { "Reorg" } span class="sum" { span { (side.reorg.summary) } } }
                (panel_body(&side.reorg, "Opens by itself while the node's chain differs from the recorded one. Blocks wait, and no order is settled as paid, until the rewind."))
            }
            details class="mini t-mempool" id="d-mempool" {
                summary {
                    span class="tierchip t-mempool" { "Mempool" }
                    span class="sum" {
                        span class="beat" id="beat" title="the fast path's last pass" {}
                        span class="dots" id="pool-dots" {
                            @for (txid, matched) in &side.pool_txs {
                                span class=(if *matched { "dot match" } else { "dot" }) title=(format!("Transaction {txid}")) {}
                            }
                        }
                        span { (side.mempool.summary) }
                    }
                }
                (panel_body(&side.mempool, "The fast path checks the pool every 250 ms for transactions it hasn't seen; the round's rotation rescans the rest as a safety net."))
            }
            details class="mini t-settlement" id="d-orders" {
                summary {
                    span class="tierchip t-settlement" { "Order status" }
                    span class="sum" id="orders-sum" {
                        span { (side.orders.summary) }
                        @if let Some((_, to)) = side.transitions.first() {
                            span class="engine-hint" { "last" } span class=(format!("engine-state state-{to}")) { (to) }
                        }
                    }
                }
                div class="body" {
                    p class="explain" { "The settlement tier: it turns what the chain and the pool say into each order's status, and queues the shop's webhook. An order is recomputed when one of its payments changed, when it expires, and on each new block while it is confirming." }
                    (rows(&side.orders))
                    @for (from, to) in &side.transitions {
                        div { span class=(format!("engine-state state-{from}")) { (from) } " to " span class=(format!("engine-state state-{to}")) { (to) } }
                    }
                }
            }
            details class="mini t-upkeep" id="d-upkeep" {
                summary { span class="tierchip t-upkeep" { "Upkeep" } span class="sum" { span class="upd" id="upd" { i {} i {} i {} i {} } span { (side.upkeep.summary) } } }
                (panel_body(&side.upkeep, "Pruning old block hashes, the database checkpoint, rechecking voided payments and orders' scanned ranges: a little every round."))
            }
            details class="mini" id="d-database" {
                summary {
                    span class="tierchip t-other" { "Database" }
                    span class="sum" id="db-sum" {
                        span class="minibars" title="Scanner, Webhook and Admin queues" {
                            @for queued in side.queues {
                                i style=(format!("height:{}px", (2 + queued.min(2) * 6))) {}
                            }
                        }
                        span { (side.database.summary) }
                    }
                }
                (panel_body(&side.database, "One thread runs every database job; each queue gets a turn in rotation, so no kind of work waits behind another for more than one job."))
            }
            details class="mini" id="d-webhooks" {
                summary { span class="tierchip t-other" { "Webhooks" } span class="sum" id="hooks-sum" { (sparkline(&side.sent)) span { (side.webhooks.summary) } } }
                (panel_body(&side.webhooks, ""))
            }
            details class="mini" id="d-restart" {
                summary { span class="tierchip t-other" { "Restart safety" } span class="sum" { span class="saved-glyph" aria-hidden="true" {} span { (side.restart.summary) } } }
                (panel_body(&side.restart, "A dark square marks each save on the thing saved. A restart loses only what is in memory, at the cost of some repeated work."))
            }
        }
    }
}

fn panel_body(panel: &Panel, explain: &str) -> Markup {
    html! {
        div class="body" {
            @if !explain.is_empty() { p class="explain" { (explain) } }
            (rows(panel))
        }
    }
}

fn rows(panel: &Panel) -> Markup {
    html! {
        div class="kv" {
            @for (label, value) in &panel.rows { span { (label) } b { (value) } }
        }
    }
}

/// Deliveries per 10 s as a small line, oldest left.
pub fn sparkline(sent: &[u32]) -> Markup {
    let most = sent.iter().copied().max().unwrap_or(0).max(3);
    let points: Vec<String> = sent
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "{},{:.1}",
                1 + i * 3,
                16.0 - f64::from(*n) / f64::from(most) * 14.0
            )
        })
        .collect();
    let line = points.join(" ");
    html! {
        svg class="spark" width="90" height="18" viewBox="0 0 90 18" aria-hidden="true" {
            line x1="1" y1="16.5" x2="89" y2="16.5" stroke="var(--line)" stroke-width="1" {}
            @if !points.is_empty() {
                polyline points=(line) stroke="var(--ink)" stroke-width="1.5" fill="none" stroke-linejoin="round" {}
            }
        }
    }
}

fn events(marks: &[Mark]) -> Markup {
    html! {
        section class="engine-card" aria-labelledby="h-events" {
            header {
                h2 id="h-events" { "Events" }
                span class="engine-hint" { "newest first; with JavaScript, up to the playback position, and a click goes there" }
                div class="engine-filters engine-right" id="engine-filters" hidden {
                    @for (tier, name) in TIERS {
                        label { input type="checkbox" checked data-tier=(tier); span class=(format!("tierchip t-{tier}")) { (name) } }
                    }
                }
            }
            div class="engine-events" {
                table {
                    thead { tr { th { "Round" } th { "Tier" } th { "What happened" } } }
                    tbody id="engine-events" {
                        @for mark in marks {
                            tr class=(if mark.key { "key" } else { "" }) {
                                td class="num" { (thousands(mark.round)) }
                                td { @if let Some(tier) = mark.tier { span class=(format!("tierchip t-{tier}")) { (crate::engine_view::present::tier_name(tier)) } } }
                                td { (mark.text) }
                            }
                        }
                        @if marks.is_empty() {
                            tr { td colspan="3" class="engine-hint" { "Nothing recorded yet." } }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_view::present::{GroupView, NodeView};

    fn chain(tip: u64, high_water: u64, lowest: u64) -> ChainView {
        ChainView {
            tip: Some(tip),
            high_water: Some(high_water),
            lowest: Some(lowest),
            window_from: None,
            cached: vec![],
            checkpoints: vec![],
            replaced: vec![],
            scanning: None,
            groups: vec![GroupView {
                id: 1,
                cursor: lowest,
                frontier: lowest == high_water,
                busy: false,
                waiting: false,
                label: "Frontier, 1 store".into(),
                title: String::new(),
            }],
            cache: String::new(),
            nodes: vec![NodeView {
                label: "node-a".into(),
                chip: "active",
                tone: "ok",
            }],
            call: String::new(),
        }
    }

    /// A short span is drawn whole up to the next block; a long one is cut
    /// after the lowest group's first blocks, keeping the newest.
    #[test]
    fn the_strip_draws_short_spans_whole_and_cuts_long_ones() {
        let whole = visible_blocks(&chain(100, 100, 100), 40);
        assert_eq!(whole.first(), Some(&Some(62)));
        assert_eq!(
            whole.last(),
            Some(&Some(101)),
            "the next block, still to come"
        );
        assert_eq!(whole.len(), 40);
        let cut = visible_blocks(&chain(1_000, 1_000, 900), 40);
        assert_eq!(
            cut.len() - 1 + 3,
            40,
            "the cells and a cut three cells wide fill the strip"
        );
        assert_eq!(cut[0], Some(899));
        assert_eq!(cut[LOW_CELLS as usize], None, "the cut");
        assert_eq!(cut.last(), Some(&Some(1_001)));
        assert!(visible_blocks(
            &ChainView {
                tip: None,
                ..chain(1, 1, 1)
            },
            40
        )
        .is_empty());
    }

    #[test]
    fn the_sparkline_draws_one_point_per_ten_seconds() {
        let svg = sparkline(&[0, 3, 6]).into_string();
        assert!(svg.contains("points=\"1,16.0 4,9.0 7,2.0\""), "{svg}");
        assert!(!sparkline(&[]).into_string().contains("polyline"));
    }
}
