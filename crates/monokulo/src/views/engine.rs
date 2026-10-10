//! The engine page (`docs/engine_visualizer.md`): the engine's scanner, for
//! admins. Rendered here whole, so without JavaScript it is a point-in-time
//! page with a Reload button; `static/engine-view.js` then follows the
//! network live, drawing each frame's [`Presented`] into the same elements,
//! and each `machine` event's [`MachineView`] into the "Machine and links"
//! strip and the Scanning panel.

use maud::{html, Markup, PreEscaped};

use super::controls::Choice;
use super::{layout_with_head, reload_button, script, Load, PageChrome};
use crate::engine_view::present::{
    Bar, ChainView, Lane, MarkView, Panel, Presented, RibbonMark, RoundView, Shape,
};
use crate::views::scaling::{network_name, thousands, MachineView, ScanningView, Tile, TileChart};

/// Blocks drawn without JavaScript: enough for the widest strip. Those
/// that don't fit whole wrap out of sight; the script draws as many as fit.
const CELLS: u64 = 72;
/// Blocks kept around the lowest group when the span is cut.
const LOW_CELLS: u64 = 7;
/// One cell and its gap, in pixels: what the script lays out with too.
const CELL_PX: u64 = 26;
/// The next block's cell, wider, in pixels.
const NEXT_PX: u64 = 32;
/// The cut between the lowest group and the newest blocks, and its gap.
const CUT_PX: u64 = 64;
/// The strip's right padding, in pixels.
const STRIP_PAD_RIGHT_PX: u64 = 8;
/// Events listed without JavaScript.
pub const MARKS_SHOWN: usize = 60;

pub struct EnginePage {
    /// The networks the engine scans, for the tabs.
    pub networks: Vec<String>,
    pub network: String,
    pub view: Option<Presented>,
    /// The "Machine and links" strip and the Scanning panel, from
    /// `/status`; `None` while it can't be read.
    pub machine: Option<MachineView>,
    /// A past round chosen from the recent rounds, shown in the round card
    /// in place of the live one.
    pub pinned: Option<RoundView>,
    /// Newest first.
    pub marks: Vec<MarkView>,
    /// Why the engine couldn't be read, if it couldn't.
    pub error: Option<String>,
}

const ENGINE_STYLE: &str = r#"
.engine-page [hidden] { display: none !important; }
.wrap.engine-page { max-width: 1880px; display: grid; gap: var(--space-sm); padding-bottom: var(--space-xl); }
.engine-page h1 { border: 0; margin: 0; padding: 0; font-size: 1.3rem; }
.engine-page h2 { border: 0; margin: 0; padding: 0; font-size: 0.85rem; font-weight: 800; }
.wrap.engine-page > .context-nav { margin: var(--space-sm) 0 0; }
.engine-titlebar { position: relative; display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-sm) var(--space-md); }
.titlebar-spacer { flex: 1; }
.engine-titlebar .reload { margin: 0; }
.engine-tabs { display: inline-flex; height: 26px; border: 1px solid var(--btn-border); border-radius: var(--radius-sm); overflow: hidden; }
.engine-tabs a, .engine-tabs span { display: flex; align-items: center; padding: 0 11px; font-weight: 700; font-size: 0.8rem; color: var(--btn-ink); background: var(--btn-bg); text-decoration: none; }
.engine-tabs > * + * { border-left: 1px solid var(--btn-border); }
.engine-tabs a:hover { background: var(--btn-hover-bg); }
.engine-tabs a[aria-current] { background: var(--ink); color: var(--paper-raised); }
.engine-tabs span.off { opacity: 0.45; cursor: not-allowed; }
.engine-help > summary { width: 24px; height: 24px; border-radius: 50%; border: 1.5px solid var(--btn-border); display: grid; place-items: center; font-weight: 800; font-size: 0.8rem; color: var(--btn-ink); background: var(--btn-bg); }
.engine-help > summary:hover { border-color: var(--btn-hover-border); }
.engine-help[open] > summary { background: var(--ink); color: var(--paper-raised); border-color: var(--ink); }
.help-body { position: absolute; z-index: 40; top: calc(100% + 6px); left: 0; width: min(920px, calc(100vw - 32px)); max-height: min(78vh, 760px); overflow-y: auto; background: var(--paper-raised); border: 1px solid var(--line-strong); border-radius: var(--radius-md); padding: var(--space-sm) var(--space-md) var(--space-md); display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-xs) var(--space-lg); font-size: 0.75rem; }
.help-body h3 { grid-column: 1 / -1; margin: var(--space-sm) 0 0; padding: 0; border: 0; font-size: 0.8rem; font-weight: 800; }
.help-body dl { margin: 0; display: grid; grid-template-columns: 26px minmax(0, 1fr); gap: 5px 8px; align-items: start; }
.help-body dt { display: flex; justify-content: center; padding-top: 2px; }
.help-body dd { margin: 0; }
.help-body dd b { font-weight: 800; }
.help-body .hatch { display: block; width: 22px; height: 12px; border: 1px solid var(--line-strong); border-radius: 3px; background: repeating-linear-gradient(135deg, var(--muted) 0 1px, transparent 1px 5px); }
.help-body .wide { grid-column: 1 / -1; margin: 0; color: var(--muted); }
.engine-card { background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-md); padding: var(--space-sm) var(--space-md); min-width: 0; }
.engine-card > header { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--space-xs) var(--space-md); margin-bottom: var(--space-xs); }
.engine-hint { font-size: 0.72rem; color: var(--muted); }
.engine-right { margin-left: auto; }
.engine-chip { display: inline-block; font-size: 0.7rem; font-weight: 700; border-radius: 99px; padding: var(--pill-pad-y) var(--pill-pad-x); border: 1px solid var(--line-strong); white-space: nowrap; }
.engine-chip.ok { background: var(--tint-success); border-color: var(--success); }
.engine-chip.warn { background: var(--tint-warning); border-color: var(--warning); }
.engine-chip.err { background: var(--tint-error); border-color: var(--error); }
.engine-chip.hi { background: var(--tint-highlight); border-color: var(--accent); }
.engine-timeline { display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: var(--space-md); align-items: start; }
.engine-timeline .tl-track { position: relative; display: grid; gap: 3px; }
.engine-timeline .tl-read { align-self: center; }
.tl-modes { display: inline-flex; gap: .5rem; align-self: center; height: 28px; border: 1px solid var(--btn-border); border-radius: var(--radius-sm); overflow: hidden; }
.tl-modes label { position: relative; display: flex; align-items: center; padding: 0 12px; font-size: 0.8rem; font-weight: 700; cursor: pointer; color: var(--btn-ink); background: var(--btn-bg); }
.tl-modes label + label { border-left: 1px solid var(--btn-border); }
.tl-modes input { position: absolute; opacity: 0; width: 1px; height: 1px; margin: 0; }
.tl-modes label:has(input:checked) { background: var(--ink); color: var(--paper-raised); }
.tl-modes label:has(input:focus-visible) { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
.tl-modes label:has(input:disabled) { opacity: 0.45; cursor: default; }
.tl-modes label:not(:has(input:checked)):not(:has(input:disabled)):hover { background: var(--btn-hover-bg); }
.engine-timeline canvas { width: 100%; display: block; border-radius: 4px; touch-action: none; }
#tl { height: 32px; cursor: pointer; }
.tl-win { position: absolute; top: 0; height: 32px; box-sizing: border-box; border: 1.5px solid var(--accent-text); border-radius: 4px; background: color-mix(in srgb, var(--accent) 16%, transparent); cursor: grab; touch-action: none; min-width: 14px; }
.tl-head { position: absolute; top: -3px; height: 38px; width: 12px; margin-left: -6px; cursor: ew-resize; touch-action: none; z-index: 3; }
.tl-head::before { content: ""; position: absolute; left: 5px; top: 0; bottom: 0; width: 2px; background: var(--ink); }
.tl-head::after { content: ""; position: absolute; left: 1px; top: 0; border: 5px solid transparent; border-top-color: var(--ink); }
.tl-win.moving { cursor: grabbing; }
.tl-handle { position: absolute; top: 3px; bottom: 3px; width: 10px; border-radius: 3px; background: var(--accent-text); cursor: ew-resize; touch-action: none; }
.tl-handle::after { content: ""; position: absolute; left: 4px; top: 5px; bottom: 5px; border-left: 2px solid var(--paper-raised); }
.tl-handle.l { left: -16px; }
.tl-handle.r { right: -16px; }
.tl-win:focus-visible, .tl-handle:focus-visible, .tl-head:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
.tl-axis { position: relative; height: 12px; font-size: 0.62rem; color: var(--muted); }
.tl-axis span { position: absolute; transform: translateX(-50%); white-space: nowrap; }
.tl-axis span.edge { transform: none; }
.tl-axis span.end { transform: translateX(-100%); }
.tl-read { font-size: 0.75rem; font-weight: 700; display: flex; align-items: center; justify-content: flex-end; white-space: nowrap; min-width: 9rem; height: 32px; font-variant-numeric: tabular-nums; }
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
.tiles-card { padding: 0; }
.tiles-head { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-xs) var(--space-md); padding: var(--space-xs) var(--space-md); border-bottom: 1px solid var(--line); }
.tiles-head .chart-legend { margin: 0; gap: var(--space-sm); font-size: 0.7rem; color: var(--muted); }
.live-chip { display: inline-flex; align-items: center; gap: var(--space-xs); margin-left: auto; padding: var(--pill-pad-y) var(--pill-pad-x); border: 1px solid var(--line); border-radius: 999px; font-size: 0.7rem; color: var(--muted); white-space: nowrap; }
.live-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--success); }
.tiles { display: grid; grid-template-columns: repeat(6, minmax(0, 1fr)); }
.tile { display: grid; align-content: start; padding: var(--space-xs) var(--space-md) var(--space-sm); border-left: 1px solid var(--line); min-width: 0; }
.tile:nth-child(6n+1) { border-left: 0; }
.tile:nth-child(n+7) { border-top: 1px solid var(--line); }
.tile .k { font-size: 0.62rem; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
.tile .v { font-size: 1.05rem; font-weight: 800; font-variant-numeric: tabular-nums; }
.tile .s { font-size: 0.7rem; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.tile-chart, .tile-spark { display: block; width: 100%; height: 2.2rem; margin-top: var(--space-xs); }
.tile-spark { background: var(--surface-sunken); border-radius: var(--radius-sm); }
.legend { display: flex; flex-wrap: wrap; gap: 4px 10px; font-size: 0.66rem; color: var(--muted); }
.legend span { display: inline-flex; align-items: center; gap: 4px; }
.legend .cell { width: 11px; height: 13px; border-radius: 3px; }
.legend .cell.next { width: 13px; }
.legend .cell.next .pool { height: 55%; }
.rpair { display: inline-flex; gap: 0; align-items: flex-end; height: 12px; }
.rpair i { width: 6px; background: var(--viz-tier-blocks); border-radius: 2px 2px 0 0; }
.rpair i:first-child { height: 8px; }
.rpair i:last-child { height: 12px; background: var(--viz-tier-chain); }
.chain-row { display: grid; grid-template-columns: minmax(0, 1fr) 240px; gap: 10px; align-items: start; }
.strip-scroll { overflow: hidden; }
/* The chain strip shows more blocks as it gets wider, never wider blocks:
   every cell keeps its size (22px, the next block 32px, 4px apart). The
   newest block is at the right edge. The cells are in the page newest
   first and laid out right to left, wrapping: a block that doesn't fit
   whole goes to a second line, which is out of sight, so the strip never
   shows part of one. engine-view.js measures the strip and draws as many
   blocks as fit, again when it is resized. */
.strip { --cells-top: 74px; position: relative; padding-inline: 14px 8px; padding-top: var(--cells-top); height: 120px; }
.cells { display: flex; flex-direction: row-reverse; flex-wrap: wrap; align-content: flex-start; gap: 4px; align-items: flex-end; height: 28px; overflow: hidden; }
.cell { position: relative; width: 22px; height: 26px; border-radius: 4px; border: 1.5px solid var(--viz-cell-edge); background: var(--viz-cell-recorded); overflow: hidden; flex: none; transition: background 0.3s, border-color 0.3s, opacity 0.3s; }
.cell .fill { position: absolute; left: 0; top: 0; bottom: 0; width: 0; background: var(--viz-tier-blocks); opacity: 0.85; transition: width 0.2s linear; }
.cell.new { background: var(--paper-raised); border-style: dashed; }
.cell.cached { background: var(--viz-cell-cached); border-color: var(--viz-tier-blocks); }
.cell.ghost { border-style: dotted; background: transparent; opacity: 0.6; }
.cell.next { width: 32px; opacity: 1; border-color: var(--viz-tier-chain); }
.cell.next::before { content: ""; position: absolute; left: 0; right: 0; top: 0; height: 2px; background: var(--viz-penalty); z-index: 1; }
.cell.next.over::before { height: 4px; }
.cell .pool { position: absolute; left: 0; right: 0; bottom: 0; height: 0; background: var(--viz-pool-fill); transition: height 0.6s ease-out; }
.cell .cnt { position: absolute; inset: 0; display: grid; place-items: center; font: 800 0.56rem/1 var(--font-mono); color: var(--ink); z-index: 2; }
.cell.reorg { border-color: var(--error); background: repeating-linear-gradient(135deg, var(--tint-error) 0 4px, var(--paper-raised) 4px 8px); }
.cell.enter { animation: cell-enter 1.1s cubic-bezier(0.3, 1.4, 0.5, 1); }
.cell.flash { box-shadow: 0 0 0 3px var(--tint-highlight); }
.cell.probe::after { content: ""; position: absolute; inset: -1px; border: 2px solid var(--viz-tier-chain); border-radius: 4px; }
.cell .save { position: absolute; right: 1px; top: 1px; width: 7px; height: 7px; border-radius: 2px; background: var(--viz-saved); }
@keyframes cell-enter { from { transform: translateX(60px); opacity: 0; } }
.brk { width: 60px; height: 26px; flex: none; display: grid; place-items: center; font-size: 0.62rem; font-weight: 700; color: var(--muted); border-inline: 2px dotted var(--line-strong); text-align: center; line-height: 1.1; }
.axis { position: absolute; left: 0; right: 0; top: calc(var(--cells-top) + 30px); height: 12px; font-size: 0.62rem; color: var(--muted); }
.axis span { position: absolute; transform: translateX(-50%); white-space: nowrap; }
/* Above the blocks, top down: stores catching up, the frontier, then the
   node's tip and the high-water mark, each with a tick down to its block;
   the reorg window's line sits a --space-sm step above the blocks. */
.marks span { position: absolute; top: 44px; height: 16px; line-height: 14px; font-size: 0.62rem; font-weight: 800; white-space: nowrap; padding: 0 5px; border-radius: 4px; transition: left 0.5s; }
.marks span::after { content: ""; position: absolute; top: 100%; height: calc(var(--cells-top) - 60px); border-left: 2px solid currentColor; }
.marks .m-tip { transform: translateX(-10px); background: var(--paper-raised); border: 1px solid var(--line-strong); }
.marks .m-tip::after { left: 8px; }
.marks .m-hw { transform: translateX(calc(-100% + 10px)); color: var(--accent-text); background: var(--paper-raised); }
/* A mark's label covers a group's tick passing behind it. */
.marks span { z-index: 2; }
.pills { z-index: 1; }
.marks .m-hw::after { right: 9px; }
.marks .m-win { top: calc(var(--cells-top) - var(--space-sm) - 2px); height: 2px; padding: 0; background: var(--viz-tier-chain); }
.marks .m-win::after { display: none; }
.pills { position: absolute; left: 0; right: 0; top: 0; height: 40px; }
.pill { position: absolute; top: 22px; height: 18px; transform: translateX(-12px); display: inline-flex; align-items: center; font-size: 0.68rem; font-weight: 800; padding: 0 var(--pill-pad-x); border-radius: 99px; border: 1.5px solid var(--line-strong); background: var(--paper-raised); white-space: nowrap; transition: left 0.45s cubic-bezier(0.4, 0, 0.2, 1); }
.pill::before { content: ""; position: absolute; left: 9.5px; top: 100%; height: calc(var(--cells-top) - 40px); border-left: 2px solid currentColor; }
.pill.frontier { transform: translateX(calc(-100% + 12px)); border-color: var(--accent); background: var(--tint-highlight); }
.pill.frontier::before { left: auto; right: 9.5px; }
.pill.catchup { top: 0; }
.pill.catchup::before { height: calc(var(--cells-top) - 18px); }
.pill.busy { animation: pill-busy 0.6s ease-in-out infinite alternate; }
.pill.waiting { opacity: 0.55; }
@keyframes pill-busy { to { box-shadow: 0 0 0 4px var(--tint-highlight); } }
.nodes { display: grid; gap: 6px; }
.node { border: 1px solid var(--line); border-radius: var(--radius-sm); padding: 5px 8px; font-size: 0.7rem; }
.node .nm { font-weight: 800; display: flex; gap: 6px; align-items: center; font-size: 0.75rem; min-width: 0; }
.node .nm .label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
.node .nm .engine-chip { flex: none; margin-left: auto; }
.node .call { font-family: var(--font-mono); font-size: 0.66rem; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.node.spark { animation: node-spark 1.2s; }
@keyframes node-spark { 30% { box-shadow: 0 0 0 4px var(--tint-highlight); } }
.round-head { flex-wrap: nowrap !important; }
.round-head .round-state { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.lanes { --row: 24px; --row-gap: var(--space-xs); display: grid; grid-template-columns: 112px minmax(0, 1fr) auto; grid-auto-rows: var(--row); gap: var(--row-gap) 10px; align-items: center; }
.lane-label { font-size: 0.7rem; font-weight: 700; display: flex; justify-content: space-between; }
.lane-label small { color: var(--muted); font-weight: 600; }
.track { position: relative; height: 20px; background: var(--surface-sunken); border-radius: 3px; overflow: hidden; container-type: inline-size; }
.track-in { position: absolute; top: 0; bottom: 0; left: var(--space-sm); right: var(--space-sm); }
/* A segment (D39): its units solid; the tier's work outside its units
   outlined, the solid part's height, 1px, over only the time it took; a
   1px thread across the time other tiers ran before it, or, for a short
   gap, the outline starting where the shape before it ends. */
.bar { position: absolute; top: calc(1.5px + var(--space-xs)); bottom: calc(1.5px + var(--space-xs)); min-width: 3px; transition: left 0.3s, width 0.3s; }
.bar > i { position: absolute; display: block; }
.bar .fill { top: 0; bottom: 0; min-width: 3px; background: var(--tier); border-radius: 2px; }
.bar .fill.p2 { background: repeating-linear-gradient(135deg, var(--tier) 0 4px, color-mix(in srgb, var(--tier) 40%, var(--paper-raised)) 4px 7px); }
.bar .thread { top: calc(50% - 0.5px); height: 1px; background: var(--tier); }
.bar .work { top: 0; bottom: 0; min-width: 3px; box-sizing: border-box; box-shadow: inset 0 0 0 1px var(--tier); border-radius: 2px; }
.bar .fill:has(+ .work.joined) { border-radius: 2px 0 0 2px; }
.bar .work.joined { border-radius: 0 2px 2px 0; }
/* A gap between the two thresholds: split on a wide track, joined on a
   narrow one, where the same share is far fewer pixels. */
@container (max-width: 480px) {
  .bar .thread.band { display: none; }
  .bar .fill:has(+ .thread.band) { border-radius: 2px 0 0 2px; }
  .bar .work.band { left: var(--join) !important; width: var(--join-w) !important; border-radius: 0 2px 2px 0; }
}
.bar.sample { position: relative; display: inline-block; vertical-align: middle; top: auto; bottom: auto; width: 18px; height: 10px; }
.lane-time { position: absolute; top: 0; line-height: 20px; margin-left: 5px; font-size: 0.62rem; font-weight: 700; font-variant-numeric: tabular-nums; white-space: nowrap; color: var(--ink); z-index: 1; }
.lane-time.before { margin-left: -5px; transform: translateX(-100%); }
.round-paused { flex: none; text-decoration: none; color: var(--ink); background: var(--tint-warning); border-color: var(--warning); cursor: pointer; }
.round-paused:hover { border-color: var(--ink); }
.rbar { cursor: pointer; }
.rbar:hover, .rbar.pinned { outline: 2px solid var(--ink); outline-offset: 1px; }
.share { position: absolute; top: 0; bottom: 0; box-sizing: content-box; margin-left: calc(-1.5px - var(--space-xs)); padding-inline: var(--space-xs); border: 1.5px dashed var(--line-strong); border-radius: 3px; transition: left 0.3s, width 0.3s; }
/* The outcome chip sits on its lane's own row, flush with the card's
   right edge, at every width. */
.outcome { font-size: 0.7rem; height: var(--row); display: flex; align-items: center; justify-content: flex-end; overflow: hidden; white-space: nowrap; min-width: 0; max-width: 18rem; }
.outcome .engine-chip { line-height: 16px; overflow: hidden; text-overflow: ellipsis; max-width: 100%; }
.ruler { position: relative; height: var(--row); }
.ruler-in { position: absolute; top: 0; bottom: 0; left: var(--space-sm); right: var(--space-sm); }
.ruler-label { position: absolute; top: var(--space-xs); transform: translateX(-50%); font: 700 0.62rem/14px var(--font-mono); background: var(--ink); color: var(--paper-raised); padding: 0 5px; border-radius: 3px; white-space: nowrap; transition: left 0.3s; }
/* The round's end: a marker on the right edge of the segment that finished
   last, in its lane only, and the round's total under the lanes at the
   same place. */
.bar.last::after { content: ""; position: absolute; right: -5px; top: calc(-1.5px - var(--space-xs)); bottom: calc(-1.5px - var(--space-xs)); width: 2px; background: var(--ink); }
/* 3px clear of the segment, so the marker never covers its colour; its
   time and the round's total move with it. */
.bar.last + .lane-time { margin-left: 9px; }
.ruler-label { margin-left: 4px; }
.round-breakdown { margin-top: var(--space-lg); }
.ribbon-row { display: grid; grid-template-columns: 112px minmax(0, 1fr) auto; gap: 10px; align-items: end; margin-top: 6px; }
.ribbon { display: flex; justify-content: flex-end; align-items: flex-end; height: 30px; overflow: hidden; border-bottom: 1px solid var(--line); }
.rbar { width: 8px; display: flex; flex-direction: column-reverse; flex: none; border-radius: 2px 2px 0 0; overflow: hidden; }
.rbar i { display: block; background: var(--tier); }
.rgap { flex: none; width: 14px; height: 9px; position: relative; display: inline-block; }
.rgap::before { content: ""; position: absolute; left: 2px; right: 2px; bottom: 1px; border-bottom: 2px dotted var(--muted); }
.rgap.woken::before { right: 8px; }
.rgap.woken::after { content: ""; position: absolute; right: 1px; bottom: 0; width: 5px; height: 7px; border: 1.5px solid var(--viz-tier-chain); border-radius: 1.5px; background: var(--paper-raised); }
/* The panels under the round: as many across as fit at 18rem, one a row
   on a phone; an open one takes the whole row. */
.engine-panels { display: grid; grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr)); gap: var(--space-sm); align-items: start; }
.engine-panels details.mini[open] { grid-column: 1 / -1; }
details.mini { background: var(--paper-raised); border: 1px solid var(--line); border-radius: var(--radius-md); min-width: 0; }
details.mini > summary { display: grid; grid-template-columns: 96px minmax(0, 1fr) 12px; gap: var(--space-sm); align-items: center; padding: 5px 10px; min-height: 32px; border-radius: var(--radius-md); cursor: pointer; }
details.mini > summary:hover { background: var(--tint-hover); }
details.mini > summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
details.mini .body .scan-figures { padding-top: 6px; }
details.mini .body .slow-block { margin: 6px 0 0; }
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
.engine-state { display: inline-block; font-size: 0.66rem; font-weight: 700; border-radius: 99px; padding: var(--pill-pad-y) var(--pill-pad-x); border: 1px solid var(--state-border); background: var(--state-bg); color: var(--state-ink); }
.engine-events { max-height: 260px; overflow-y: auto; }
.engine-events table { width: 100%; font-size: 0.75rem; }
.engine-events td { padding: 2px 8px; }
.engine-events tr.key td:last-child { font-weight: 700; }
.engine-events tr.now td { background: var(--tint-highlight); }
.engine-events tbody tr { cursor: pointer; }
.engine-filters { display: flex; flex-wrap: wrap; gap: var(--space-sm); }
.engine-filters label { display: inline-flex; gap: 3px; align-items: center; font-size: 0.7rem; }
.token { position: absolute; z-index: 20; pointer-events: none; width: 12px; height: 12px; border-radius: 3px; border: 2px solid var(--paper-raised); background: var(--tier, var(--ink)); box-shadow: 0 0 0 1px var(--ink); }
.token.pkt { width: auto; height: auto; padding: 0 5px; font: 700 0.62rem/15px var(--font-mono); color: var(--paper-raised); background: var(--viz-tier-blocks); border: 0; box-shadow: none; }
.token.pkt.chain { background: var(--viz-tier-chain); }
.token.pkt.mempool { background: var(--viz-tier-mempool); }
.token.save { width: 10px; height: 10px; border-radius: 2px; background: var(--viz-saved); border: 1.5px solid var(--paper-raised); box-shadow: none; }
.token.payment { border-radius: 50%; background: var(--accent); }
.token.token-stores { background: var(--viz-tier-blocks); border-radius: 99px; width: 18px; }
.ghostcell { position: absolute; z-index: 15; pointer-events: none; }
#engine-stage { position: relative; }
@media (max-width: 1150px) {
  .engine-summary { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .engine-summary > div:nth-child(3n+1) { border-left: 0; }
  .tiles { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .tile:nth-child(n) { border-left: 1px solid var(--line); border-top: 0; }
  .tile:nth-child(3n+1) { border-left: 0; }
  .tile:nth-child(n+4) { border-top: 1px solid var(--line); }
  .lanes { grid-template-columns: 96px minmax(0, 1fr) auto; }
  .ribbon-row { grid-template-columns: 96px minmax(0, 1fr); }
  .ribbon-row > :last-child { display: none; }
}
/* A phone: the summary and the strip two across, the panels one a row.
   The bars keep a mini form beside a narrow label, the chip on their row.
   The chain strip is the cells alone, a --space-md gap above the nodes. */
@media (max-width: 640px) {
  .engine-summary { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .engine-summary > div:nth-child(n) { border-left: 1px solid var(--line); }
  .engine-summary > div:nth-child(2n+1) { border-left: 0; }
  .tiles { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .tile:nth-child(n) { border-left: 1px solid var(--line); border-top: 0; }
  .tile:nth-child(2n+1) { border-left: 0; }
  .tile:nth-child(n+3) { border-top: 1px solid var(--line); }
  .engine-panels { grid-template-columns: minmax(0, 1fr); }
  .chain-row { grid-template-columns: minmax(0, 1fr); gap: var(--space-md); }
  .strip { height: auto; padding-top: var(--space-xs); }
  .strip .marks, .strip .axis, .strip .pills { display: none; }
  .lanes { grid-template-columns: 64px minmax(0, 1fr) auto; gap: 2px var(--space-xs); }
  .lane-label .tierchip { font-size: 0.65rem; }
  .lane-label small { display: none; }
  .engine-timeline { grid-template-columns: minmax(0, 1fr); }
  .help-body { grid-template-columns: minmax(0, 1fr); }
}
@media (prefers-reduced-motion: reduce) {
  .engine-page *, .engine-page *::before, .engine-page *::after { animation-duration: 0.01ms !important; animation-iteration-count: 1 !important; transition-duration: 0.15s !important; }
}
"#;

/// Every Monero network, in the order the switcher shows them.
const NETWORKS: [&str; 3] = ["mainnet", "stagenet", "testnet"];

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
        (script("engine-view.js", Load::Defer))
    };
    let body = html! {
        div id="engine-stage" {
        main class="wrap engine-page" data-network=(page.network) {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/status" { "Status" } }
            div class="engine-titlebar" {
                h1 { "Engine" }
                (help())
                span class="titlebar-spacer" {}
                // With JavaScript, picking a network goes there; without,
                // Go does.
                form class="engine-network" method="get" action="/status/engine" {
                    label for="engine-network" class="visually-hidden" { "Network" }
                    mk-select compact {
                        select id="engine-network" name="network" {
                            @for network in NETWORKS {
                                @let configured = page.networks.iter().any(|n| n == network);
                                @let choice = Choice::new(network, network_name(network)).selected(*network == page.network).disabled(!configured);
                                (if configured { choice } else { choice.note("no node configured") })
                            }
                        }
                    }
                    button type="submit" class="network-go no-js-only" { "Go" }
                }
                (reload_button(&format!("/status/engine?network={}", page.network)))
            }
            @if let Some(error) = &page.error {
                p class="error" role="alert" { "The engine's activity could not be read: " (error) }
            }
            @if let Some(view) = &page.view {
                (timeline())
                p class="sync-motion-note engine-hint" role="status" { "Catching up: showing progress with reduced animation." }
                (summary(view))
            }
            @if let Some(machine) = &page.machine {
                (machine_strip(machine))
            }
            @if let Some(view) = &page.view {
                (chain(&view.chain))
                (round(view, page.pinned.as_ref(), &page.network))
                (panels(view, page.machine.as_ref().and_then(|m| m.scanning.as_ref())))
            }
            (events(&page.marks))
        }
        }
    };
    layout_with_head(chrome, "Engine - Monokulo", head, body)
}

/// The "Machine and links" strip: small multiples of the machine's CPU and
/// memory, the link to the node in use and the blocks' size. Without
/// JavaScript it is as the page was loaded; with it, each `machine` event
/// draws it again and the chip says how fresh it is.
fn machine_strip(machine: &MachineView) -> Markup {
    html! {
        section class="engine-card tiles-card" id="engine-machine" aria-labelledby="h-machine" {
            header class="tiles-head" {
                h2 id="h-machine" { "Machine and links" }
                ul class="chart-legend" id="machine-legend" hidden[!machine.stacked] {
                    li { span class="chart-swatch chart-engine" {} "engine" }
                    li { span class="chart-swatch chart-monokulo" {} "monokulo" }
                }
                span class="live-chip" id="machine-live" title="As the page was loaded: reload for newer figures" {
                    span class="live-dot" hidden {}
                    span id="machine-age" { "as loaded" }
                }
            }
            div class="tiles" id="machine-tiles" {
                @for tile in &machine.tiles { (tile_markup(tile)) }
            }
        }
    }
}

/// One small multiple: what it is, the figure, a line under it and its
/// last hour.
pub fn tile_markup(tile: &Tile) -> Markup {
    html! {
        div class="tile" data-tile=(tile.key) title=(tile.title) {
            span class="k" { (tile.label) }
            span class="v" { (tile.value) }
            span class="s" { (tile.note) }
            span class="visually-hidden" { (tile.title) }
            @match &tile.chart {
                TileChart::Stack { layers, limits } => {
                    svg class="resource-chart tile-chart" viewBox="0 0 360 100" preserveAspectRatio="none" aria-hidden="true" {
                        @for layer in layers { path class=(format!("chart-layer {}", layer.class)) d=(layer.d) {} }
                        @for limit in limits {
                            line class=(format!("chart-limit {}", limit.class)) x1="0" x2="360" y1=(limit.y) y2=(limit.y) vector-effect="non-scaling-stroke" { title { (limit.label) } }
                        }
                    }
                }
                TileChart::Spark { runs } => {
                    svg class="sparkline tile-spark" viewBox="0 0 59 16" preserveAspectRatio="none" aria-hidden="true" {
                        @for points in runs {
                            @if points.contains(' ') {
                                polyline points=(points) {}
                            } @else {
                                @let (x, y) = points.split_once(',').unwrap_or(("0", "8"));
                                circle cx=(x) cy=(y) r="1" {}
                            }
                        }
                    }
                }
                TileChart::None => {}
            }
        }
    }
}

/// The timeline: drawn by the script, so hidden until it runs.
fn timeline() -> Markup {
    html! {
        section class="engine-card engine-timeline" id="engine-timeline" aria-label="Timeline" hidden {
            div class="tl-track" id="tl-track" {
                canvas id="tl" aria-label="The whole history this page holds, each event a line and each key event a circle. A press outside the window takes the window and the playback position there." {}
                div class="tl-win" id="tl-win" tabindex="0" role="group"
                    aria-label="The window: the stretch being looked at. Drag it to move it, a press in it goes to that moment. Left and right jump between key events, with Shift between any events; Space plays; End returns to live." {
                    span class="tl-handle l" id="tl-from" tabindex="0" role="slider" aria-label="Start of the window: left and right move it" {}
                    span class="tl-handle r" id="tl-to" tabindex="0" role="slider" aria-label="End of the window: left and right move it" {}
                }
                div class="tl-head" id="tl-head" role="slider" aria-label="Playback position" {}
                div class="tl-axis" id="tl-axis" {}
            }
            div class="tl-read" id="tl-read" { span id="tl-text" {} }
            div class="tl-modes" id="tl-modes" {
                label for="tl-mode" { "Playback" }
                mk-select compact {
                    select id="tl-mode" name="tl-mode" {
                        option value="live" selected { "Live" }
                        option value="replay" disabled { "Play" }
                        option value="paused" { "Pause" }
                    }
                }
            }
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
    let next = |height: u64| chain.next_block.is_some() && Some(height) == chain.tip.map(|t| t + 1);
    // Each cell's centre, in pixels from the strip's right edge: the
    // newest is at the right, and what doesn't fit is off the left.
    let mut centres = Vec::with_capacity(blocks.len());
    let mut x = STRIP_PAD_RIGHT_PX;
    for block in blocks.iter().rev() {
        match block {
            Some(height) => {
                let width = if next(*height) { NEXT_PX } else { CELL_PX - 4 };
                centres.push((*height, x + width / 2));
                x += width + 4;
            }
            None => x += CUT_PX,
        }
    }
    centres.reverse();
    let centre = |height: u64| {
        centres
            .iter()
            .find(|(h, _)| *h == height)
            .or_else(|| centres.iter().rev().find(|(h, _)| *h < height))
            // Older than every block drawn: at the oldest, the left edge.
            .or_else(|| centres.first())
            .map_or(0, |(_, x)| *x)
    };
    let left = |x: u64| format!("left:calc(100% - {x}px)");
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
                    span { i class="cell ghost next" aria-hidden="true" { span class="pool" {} } "next block: the node's pool" }
                    span { i class="saved-glyph" aria-hidden="true" {} "saved to disk" }
                }
                span class="engine-chip engine-right" id="cache-chip" { (chain.cache) }
            }
            div class="chain-row" {
                div class="strip-scroll" id="strip-scroll" {
                    div class="strip" id="strip" {
                        div class="marks" id="chain-marks" {
                            @if chain.tip.is_some() {
                                span class="m-tip" style=(left(centre(tip))) { "node tip" }
                            }
                            @if high_water < tip {
                                span class="m-hw" style=(left(centre(high_water))) { "scanned to" }
                            }
                        }
                        // Newest first: laid out from the right.
                        div class="cells" id="cells" {
                            @for block in blocks.iter().rev() {
                                @match block {
                                    Some(height) => (cell(chain, *height)),
                                    None => div class="brk" { "…" },
                                }
                            }
                        }
                        div class="axis" id="chain-axis" {
                            @for (height, x) in centres.iter().filter(|(h, _)| h % 5 == 0 && *h <= tip) {
                                span style=(left(*x)) { (thousands(*height)) }
                            }
                        }
                        div class="pills" id="pills" {
                            @for group in &chain.groups {
                                div class=(format!("pill {}{}{}", if group.frontier { "frontier" } else { "catchup" }, if group.busy { " busy" } else { "" }, if group.waiting { " waiting" } else { "" }))
                                    data-id=(group.id) title=(group.title)
                                    style=(left(centre(group.cursor))) { (group.label) }
                            }
                        }
                    }
                }
                div class="nodes" id="nodes" {
                    // The network's nodes, and nothing else: the last call
                    // to the node in use under its name.
                    @for node in &chain.nodes {
                        div class="node" {
                            div class="nm" { span class="label" title=(node.label) { (node.label) } span class=(format!("engine-chip {}", node.tone)) { (node.chip) } }
                            @if let (true, Some(call)) = (node.active, &chain.call) { div class="call" { (call) } }
                        }
                    }
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
    let next = chain.next_block.as_ref().filter(|_| height == tip + 1);
    let class = match next {
        Some(next) if next.over => format!("cell{state} next over"),
        Some(_) => format!("cell{state} next"),
        None => format!("cell{state}"),
    };
    let title = next.map_or_else(
        || format!("Block {}", thousands(height)),
        |next| next.title.clone(),
    );
    html! {
        div class=(class) data-h=(height) title=(title) {
            span class="fill" style=(format!("width:{:.0}%", fill * 100.0)) {}
            @if checkpoint.is_some() { span class="save" {} }
            @if let Some(next) = next {
                span class="pool" style=(format!("height:{:.0}%", next.fill.unwrap_or(0.0) * 100.0)) {}
                span class="cnt" { (next.count) }
            }
        }
    }
}

/// The legend behind the (?) button: what every label, symbol and
/// movement on the page means. A `details`, so it opens without
/// JavaScript too.
fn help() -> Markup {
    let tier = |tier: &str| html! { i class=(format!("tierchip t-{tier}")) aria-hidden="true" {} };
    html! {
        details class="engine-help" id="engine-help" {
            summary aria-label="What the page shows" title="What the page shows" { "?" }
            div class="help-body" {
                p class="wide" { "The engine scans the Monero chain for the stores' payments in rounds. This page follows one network's scanner about 1.5s behind, and the timeline replays the last 30 minutes." }

                h3 { "Timeline" }
                dl {
                    dt { svg width="10" height="16" aria-hidden="true" { line x1="5" y1="2" x2="5" y2="14" stroke="var(--muted)" stroke-width="1.5" {} } }
                    dd { "An event." }
                    dt { svg width="14" height="14" aria-hidden="true" { circle cx="7" cy="7" r="5" fill="var(--viz-tier-blocks)" {} } }
                    dd { b { "A key event" } ", in its tier's colour: a payment found, a reorganisation, a block's scan saved partway, stores catching up. Hover over it for what it was." }
                    dt { svg width="10" height="16" aria-hidden="true" { rect x="4" y="1" width="2" height="14" fill="var(--ink)" {} } }
                    dd { b { "The playback position" } ": the moment the whole page is showing, 1.5s behind the engine while live. Drag it to scrub (that pauses there, as Pause does), press anywhere in the window to go there; Play replays from it, Live returns." }
                }
                dl {
                    dt { span class="tl-win" style="position:static;display:block;width:22px;height:12px" {} }
                    dd { b { "The window" } ": the bar is always the last 30 minutes, the history filling it from the right; paused, it stops at the moment you left live. The orange window is the stretch you are looking at: drag its middle to move it, its handles to widen or narrow it. While it ends at now the page is live; move it into the past and playback pauses at its start." }
                    dt { span class="hatch" aria-hidden="true" {} }
                    dd { b { "Hatched" } ": the engine has no record of that time, because it started (or restarted) since. There is nothing to show or move the window to there; the faint line is where its record starts." }
                    dt { kbd { "←" } }
                    dd { b { "Live, Play, Pause" } " choose how the page plays: following the engine, replaying from the playback position, or held still; off live, it says how far behind live it is. With the window focused, left and right jump between key events (with Shift, any event), space plays and pauses, End goes live." }
                }

                h3 { "Summary" }
                p class="wide" { b { "Node tip" } ": the newest block the node has. " b { "Scanned to" } ": the high-water mark, the newest block the engine has recorded. " b { "Behind" } ": blocks between the node's tip and the store furthest behind. " b { "Last round" } ": how long the last round took, of its 10s budget. " b { "Chain" } ": whether the recorded chain still agrees with the node's." }

                h3 { "Machine and links" }
                p class="wide" { b { "CPU" } " and " b { "Memory" } ": the machine's last hour, the engine's share under monokulo's; a dashed line is a container's memory limit. " b { "Transfer" } ", " b { "Round trip" } " and " b { "First byte" } ": the link to the node the engine is reading from right now, named under the figure (a fallback says so). " b { "Block size" } ": the average of recent blocks. With JavaScript they are updated every few seconds; the chip says how long ago." }

                h3 { "Chain" }
                dl {
                    dt { i class="cell" aria-hidden="true" style="width:11px;height:13px" {} }
                    dd { "A block the engine has recorded." }
                    dt { i class="cell new" aria-hidden="true" style="width:11px;height:13px" {} }
                    dd { "A block the node has that the engine hasn't scanned yet." }
                    dt { i class="cell cached" aria-hidden="true" style="width:11px;height:13px" {} }
                    dd { "Fetched from the node and held in memory to be scanned (the cache chip says how much)." }
                    dt { i class="cell reorg" aria-hidden="true" style="width:11px;height:13px" {} }
                    dd { "Replaced: the node's chain no longer has this block (a reorganisation)." }
                    dt { span class="cell" aria-hidden="true" style="width:11px;height:13px;display:block" { span class="fill" style="width:60%" {} } }
                    dd { "A block being scanned fills left to right. A dark corner square means its scan was saved partway, to carry on next round." }
                }
                dl {
                    dt { i class="cell ghost next" aria-hidden="true" style="width:13px;height:13px" { span class="pool" style="height:55%" {} } }
                    dd { b { "The next block" } ", not mined yet. It fills from the bottom with the transactions waiting in the node's pool (the number) against what a miner can fit in a block at full reward. The red top edge is where the penalty zone starts, and it thickens when the pool holds more than a block's worth. The page asks the node every 5s while it is open." }
                    dt { span class="engine-chip" style="font-size:0.6rem;padding:0 4px" { "tip" } }
                    dd { b { "node tip" } " and " b { "scanned to" } " mark the node's newest block and the engine's high-water mark. The blue line under the last blocks is the reorg window, checked again for a reorganisation every round." }
                    dt { span class="pill frontier" style="position:static;transform:none;padding:0 5px;font-size:0.6rem" { "F" } }
                    dd { b { "Groups of stores" } ", by the block their scan has reached. " b { "Frontier" } " stores are at the high-water mark and scanned as each block arrives. " b { "Catching up" } " stores are behind (a new store, or after downtime) and scanned with time left over until they join the frontier." }
                }

                h3 { "Round" }
                p class="wide" { "Each round gives the five tiers a share of a 10s budget, in order; time left over goes round again. Every millisecond of a round belongs to one tier, so the times written after the segments add up to the round's: the round's opening (deciding whether to ask for the pool, then asking the node for its tip) counts to Chain, keeping fetched blocks for the next round to Blocks. A tier's work that ran back to back is one segment; hover for its total duration and operation count, or open Timing details for the breakdown." }
                dl {
                    dt { (tier("chain")) }
                    dd { b { "Chain" } " asks the node for its tip, then checks the recorded chain still matches the node's: it compares the newest recorded block's hash with the node's. When the engine is caught up, the tip's hash came with the tip and nothing more is asked; otherwise a blue " b { "hash check" } " flies from the node. If they differ, it reconciles the reorganisation: payments are re-examined and blocks rewound." }
                    dt { (tier("blocks")) }
                    dd { b { "Blocks" } " fetches new blocks from the node and scans each one for every group of stores." }
                    dt { (tier("mempool")) }
                    dd { b { "Mempool" } " scans transactions still in the pool, so a payment is seen before it is mined. It runs only while an order waits to be paid." }
                    dt { (tier("settlement")) }
                    dd { b { "Settlement" } " turns what the chain and the pool say into each order's status (paid, confirming, expired) and writes the order event Monokulo sends the store's webhooks from." }
                    dt { (tier("upkeep")) }
                    dd { b { "Upkeep" } " does a little housekeeping each round: pruning old block hashes, rechecking voided payments." }
                }
                dl {
                    dt { small { "40 %" } }
                    dd { "The tier's share of the round's budget." }
                    dt { span class="bar sample t-blocks" { i class="fill" style="left:0;width:100%" {} } }
                    dd { "A unit of work. Striped " span class="bar sample t-blocks" { i class="fill p2" style="left:0;width:100%" {} } " ran on time left over (pass 2). An outline " span class="bar sample t-chain" { i class="work" style="left:0;width:100%" {} } " is work for the tier outside its units, such as the tip request, drawn over only the time it took. When other tiers ran between a unit and that later work, a thin thread crosses the time between " span class="bar sample t-blocks" style="width:40px" { i class="fill" style="left:0;width:40%" {} i class="thread" style="left:40%;width:45%" {} i class="work" style="left:85%;width:15%" {} } "; when that time is short, the outline starts right where the unit ends instead. Hover over a bar for what it was. While stores catch up or a reorganisation is open, a dashed box shows the tier's reserved share." }
                    dt { span class="ruler-label" style="position:static;transform:none" { "s" } }
                    dd { "Each segment's time is written after it. The thin marker is on the right edge of the segment that finished last, and the label under it is the round's time: the sum of the segments'." }
                    dt { span class="engine-chip ok" style="font-size:0.6rem" { "Idle" } }
                    dd { b { "Idle" } ": nothing left to do. " b { "Backlogged" } ": out of time with work left, so the next round starts at once. " b { "Waiting" } ": held up by what it names. " b { "Failed" } ": an error, retried next round." }
                }

                p class="wide" { "Round numbers count scanner rounds for this network since the engine started. They reset to 1 when the engine restarts; refreshing this page or reconnecting the live stream does not reset them." }
                h3 { "Last rounds" }
                dl {
                    dt { span class="rbar" style="height:14px;width:8px" { i class="t-blocks" style="height:60%" {} i class="t-chain" style="height:40%" {} } }
                    dd { "A round. The taller, the longer (on a log scale), coloured by where its time went. Click one to show it in the round card; its " b { "× Paused" } " chip goes back to the live round." }
                    dt { i class="rgap" aria-hidden="true" {} }
                    dd { "The scanner slept until the poll interval was up." }
                }
                dl {
                    dt { i class="rgap woken" aria-hidden="true" {} }
                    dd { "The sleep was cut short: the node announced a new block." }
                    dt { i class="rpair" aria-hidden="true" { i {} i {} } }
                    dd { "Rounds back to back with no sleep between: the first ended with work left, so the next started at once." }
                }

                h3 { "Things that move" }
                dl {
                    dt { span class="token pkt chain" style="position:static;display:inline-block" { "hash" } }
                    dd { "A call to the node, flying from the node to where its answer goes: a hash check (blue), blocks (orange), the pool or transactions (green)." }
                    dt { span class="token token-stores" style="position:static;display:inline-block" {} }
                    dd { "Stores moving on to the next block." }
                }
                dl {
                    dt { span class="token payment" style="position:static;display:inline-block" {} }
                    dd { "A payment found, on its way to the order's status." }
                    dt { span class="saved-glyph" aria-hidden="true" {} }
                    dd { "Something saved to disk. A restart carries on from there." }
                }
                p class="wide" { "The panels under the round sum up the scan's pace, the reorg check, the pool, orders, upkeep, the database worker and what a restart would lose. Click one for its figures." }
            }
        }
    }
}

/// The round card: the live round, or `pinned`, a past one chosen from
/// the recent rounds, with a chip back to live.
fn round(view: &Presented, pinned: Option<&RoundView>, network: &str) -> Markup {
    let live = format!("/status/engine?network={network}");
    html! {
        section class="engine-card" id="engine-round" aria-labelledby="h-round" {
            @match pinned.or(view.round.as_ref()) {
                Some(round) => {
                    header class="round-head" {
                        h2 id="h-round" title="Scanner round for this network since the engine started; resets on engine restart" { (round.title) }
                        @if pinned.is_some() {
                            a class="engine-chip round-paused" id="round-resume" href=(live)
                                title="Showing a past round: back to the live one" { "× Paused" }
                        }
                        span class="engine-hint round-state" { (round.state) }
                    }
                    div class="lanes" {
                        @for lane in &round.lanes {
                            (lane_row(lane, round.scale_ms))
                        }
                        div {}
                        div class="ruler" { div class="ruler-in" { (end_marker(round)) } }
                        div {}
                    }
                    details class="round-breakdown" {
                        summary { "Timing details · " (round.title) " (snapshot)" }
                        @for lane in &round.lanes {
                            @for bar in &lane.bars {
                                p { strong { (bar.title) } }
                                ul { @for detail in &bar.details { li { (detail) } } }
                            }
                        }
                    }
                }
                None => {
                    header { h2 id="h-round" title="Scanner round for this network since the engine started; resets on engine restart" { "Round" } span class="engine-hint" { "No round recorded yet." } }
                }
            }
            div class="ribbon-row" {
                span class="engine-hint" { "Last rounds" }
                div class="ribbon" id="ribbon" aria-label="Recent rounds" {
                    @for mark in &view.ribbon {
                        @match mark {
                            RibbonMark::Round { number, height, parts, title } => {
                                a class=(if pinned.is_some_and(|p| p.number == *number) { "rbar pinned" } else { "rbar" })
                                    href=(format!("{live}&round={number}")) data-round=(number)
                                    style=(format!("height:{height}px")) title=(title) {
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
                    span { i class="rpair" aria-hidden="true" { i {} i {} } "back to back: work was left" }
                }
            }
        }
    }
}

fn lane_row(lane: &Lane, scale_ms: u64) -> Markup {
    let tier = lane.tier.to_string();
    html! {
        div class="lane-label" { span class=(format!("tierchip t-{tier}")) { (lane.name) } small { (lane.share) } }
        div class=(format!("track t-{tier}")) {
            div class="track-in" {
                @if let Some((start, ms)) = lane.reserved {
                    div class="share" style=(span_style(start, ms, scale_ms)) {}
                }
                @for bar in &lane.bars {
                    (bar_div(bar, scale_ms))
                    @if let Some(label) = &bar.label {
                        // After the bar as drawn: a short one is drawn wider than its time.
                        @let end = bar_end(bar, scale_ms);
                        span class=(if end > 88.0 { "lane-time before" } else { "lane-time" })
                            style=(format!("left:{:.2}%", end.min(99.5))) { (label) }
                    }
                }
            }
        }
        div class="outcome" {
            @if let Some(chip) = &lane.outcome {
                span class=(format!("engine-chip {}", chip.tone)) title=(chip.text) { (chip.text) }
            }
        }
    }
}

/// Where `bar` ends, in percent of the lane (one under a millisecond is
/// drawn 3px wide, which its label's margin clears).
fn bar_end(bar: &Bar, scale_ms: u64) -> f64 {
    pct(bar.start_ms, scale_ms) + pct(bar.span_ms, scale_ms)
}

/// The round's total, under the lanes where the segment that finished last
/// ends (its marker is on that segment alone).
fn end_marker(round: &RoundView) -> Markup {
    let left = round
        .lanes
        .iter()
        .flat_map(|lane| &lane.bars)
        .find(|bar| bar.last)
        .map_or_else(
            || pct(round.elapsed_ms, round.scale_ms),
            |bar| bar_end(bar, round.scale_ms),
        );
    html! {
        span class="ruler-label" style=(format!("left:{:.2}%", left.min(99.5))) { (round.elapsed) }
    }
}

/// A segment: its shapes placed in it, in percent of its drawn length.
fn bar_div(bar: &Bar, scale_ms: u64) -> Markup {
    let span = bar.span_ms;
    let at = |ms: u64| {
        if span == 0 {
            0.0
        } else {
            ms.saturating_sub(bar.start_ms) as f64 / span as f64 * 100.0
        }
    };
    let place = |from: u64, to: u64| {
        let left = at(from);
        let width = if span == 0 { 100.0 } else { at(to) - left };
        format!("left:{left:.2}%;width:{width:.2}%")
    };
    html! {
        div class=(if bar.last { "bar last" } else { "bar" }) style=(format!("left:{:.2}%;width:{:.2}%", pct(bar.start_ms, scale_ms), pct(span, scale_ms))) title=(bar.title) {
            @for shape in &bar.shapes {
                @match shape {
                    Shape::Fill { from_ms, to_ms, leftover } => {
                        i class=(if *leftover { "fill p2" } else { "fill" }) style=(place(*from_ms, *to_ms)) {}
                    }
                    Shape::Thread { from_ms, to_ms, band } => {
                        i class=(if *band { "thread band" } else { "thread" }) style=(place(*from_ms, *to_ms)) {}
                    }
                    Shape::Work { from_ms, to_ms, joined, band_from } => {
                        @let class = match (joined, band_from) { (true, _) => "work joined", (false, Some(_)) => "work band", (false, None) => "work" };
                        @let style = match band_from {
                            Some(from) => format!("{};--join:{:.2}%;--join-w:{:.2}%", place(*from_ms, *to_ms), at(*from), at(*to_ms) - at(*from)),
                            None => place(*from_ms, *to_ms),
                        };
                        i class=(class) style=(style) {}
                    }
                }
            }
        }
    }
}

fn pct(ms: u64, scale_ms: u64) -> f64 {
    (ms as f64 / scale_ms.max(1) as f64 * 100.0).min(100.0)
}

fn span_style(start: u64, ms: u64, scale_ms: u64) -> String {
    format!(
        "left:{:.2}%;width:{:.2}%",
        pct(start, scale_ms),
        pct(ms, scale_ms).max(0.5)
    )
}

/// The panels under the round, each a line until opened. Scanning comes
/// first, open.
fn panels(view: &Presented, scanning: Option<&ScanningView>) -> Markup {
    let side = &view.side;
    html! {
        section class="engine-panels" id="engine-panels" aria-label="More about this network" {
            details class="mini t-blocks" id="d-scanning" open {
                summary {
                    span class="tierchip t-blocks" { "Scanning" }
                    span class="sum" id="scan-sum" { span { (scanning.map_or("not reported", |s| s.preview.as_str())) } }
                }
                div class="body" id="scan-body" { (scanning_body(scanning)) }
            }
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
                (panel_body(&side.mempool, "The node's pool is what the next block is mined from; the page asks the node about it every 5s while it is open. The engine itself looks at the pool only while an order could be paid from it: the fast path every 250ms for transactions it hasn't seen, the round's rotation rescanning the rest as a safety net."))
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
                    p class="explain" { "The settlement tier: it turns what the chain and the pool say into each order's status, and writes the order event Monokulo sends the shop's webhooks from. An order is recomputed when one of its payments changed, when it expires, and on each new block while it is confirming." }
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
                        span class="minibars" title="Scanner and Admin queues" {
                            @for queued in side.queues {
                                i style=(format!("height:{}px", (2 + queued.min(2) * 6))) {}
                            }
                        }
                        span { (side.database.summary) }
                    }
                }
                (panel_body(&side.database, "One thread runs every database job; each queue gets a turn in rotation, so no kind of work waits behind another for more than one job."))
            }
            details class="mini" id="d-restart" {
                summary { span class="tierchip t-other" { "Restart safety" } span class="sum" { span class="saved-glyph" aria-hidden="true" {} span { (side.restart.summary) } } }
                (panel_body(&side.restart, "A dark square marks each save on the thing saved. A restart loses only what is in memory, at the cost of some repeated work."))
            }
        }
    }
}

/// The Scanning panel's figures: how far behind, what sets the pace, and
/// the rest (docs/engine_scaling.md section 6).
fn scanning_body(scanning: Option<&ScanningView>) -> Markup {
    html! {
        @match scanning {
            Some(scanning) => {
                @if let Some(slow) = &scanning.slow {
                    p class="notice slow-block" role="status" { (slow) }
                }
                dl class="scan-figures" {
                    @for (label, value) in &scanning.rows { dt { (label) } dd { (value) } }
                }
            }
            None => p class="explain" { "The engine hasn't reported how its scan of this network is going." },
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

fn events(marks: &[MarkView]) -> Markup {
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
                                td { span class=(format!("tierchip t-{}", mark.tier)) { (crate::engine_view::present::tier_name(mark.tier)) } }
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
            next_block: None,
            cache: String::new(),
            nodes: vec![NodeView {
                label: "node-a".into(),
                active: true,
                chip: "active",
                tone: "ok",
            }],
            call: None,
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

    /// A segment's shapes are placed in it in percent of its drawn
    /// length: solid, a thread across the gap, the outline after it; a
    /// band carries where a narrow track joins it; one under a millisecond
    /// is 0 wide here and 3px on the page (its minimum width).
    #[test]
    fn a_segment_places_its_shapes_in_its_length() {
        let bar = |start_ms: u64, span_ms: u64, shapes: Vec<Shape>| Bar {
            start_ms,
            ms: 0,
            span_ms,
            shapes,
            work: false,
            title: String::new(),
            details: Vec::new(),
            label: None,
            last: false,
        };
        let split = bar_div(
            &bar(
                100,
                200,
                vec![
                    Shape::Fill {
                        from_ms: 100,
                        to_ms: 200,
                        leftover: false,
                    },
                    Shape::Thread {
                        from_ms: 200,
                        to_ms: 250,
                        band: true,
                    },
                    Shape::Work {
                        from_ms: 250,
                        to_ms: 300,
                        joined: false,
                        band_from: Some(200),
                    },
                ],
            ),
            1_000,
        )
        .into_string();
        assert_eq!(
            split,
            r#"<div class="bar" style="left:10.00%;width:20.00%" title=""><i class="fill" style="left:0.00%;width:50.00%"></i><i class="thread band" style="left:50.00%;width:25.00%"></i><i class="work band" style="left:75.00%;width:25.00%;--join:50.00%;--join-w:50.00%"></i></div>"#
        );
        let zero = bar_div(
            &bar(
                3,
                0,
                vec![Shape::Fill {
                    from_ms: 3,
                    to_ms: 3,
                    leftover: false,
                }],
            ),
            10,
        )
        .into_string();
        assert!(
            zero.contains(r#"style="left:30.00%;width:0.00%""#)
                && zero.contains(r#"<i class="fill" style="left:0.00%;width:100.00%"></i>"#),
            "{zero}"
        );
    }
}
