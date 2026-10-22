// The round card: the live round, or a past one picked from the recent
// rounds with a chip back to live; each tier's lane of segments to the
// round's scale, the timing details, and the recent rounds.
import { For, Show, untrack } from 'solid-js';
import { Disclosure } from '@monokulo/ui/solid';
import type { Bar, Lane, RibbonMark, RoundView, Shape } from './present.ts';
import { meets } from './present.ts';

const ROUND_TITLE = 'Scanner round for this network since the engine started; resets on engine restart';

const pct = (ms: number, scale: number) => Math.min(100, (ms / Math.max(1, scale)) * 100);
/** Where `bar` ends, in percent of the lane. */
const barEnd = (bar: Bar, scale: number) => pct(bar.start_ms, scale) + pct(bar.span_ms, scale);

/** A segment: its shapes placed in it, in percent of its drawn length.
 *  Shapes back to back are one run, square where they meet. */
function Segment(props: { bar: Bar; scale: number }) {
  const at = (ms: number) => (props.bar.span_ms ? ((ms - props.bar.start_ms) / props.bar.span_ms) * 100 : 0);
  const place = (from: number, to: number) => ({
    left: `${at(from).toFixed(2)}%`,
    width: `${(props.bar.span_ms ? at(to) - at(from) : 100).toFixed(2)}%`,
  });
  const run = (i: number) => ({
    'meets-prev': meets(props.bar.shapes[i - 1], props.bar.shapes[i]),
    'meets-next': meets(props.bar.shapes[i], props.bar.shapes[i + 1]),
  });
  const piece = (shape: Shape, i: number) => {
    switch (shape.kind) {
      case 'fill':
        return <i class={{ fill: true, p2: shape.leftover, ...run(i) }} style={place(shape.from_ms, shape.to_ms)} />;
      case 'thread':
        return <i class={{ thread: true, band: shape.band }} style={place(shape.from_ms, shape.to_ms)} />;
      case 'work': {
        const band = shape.band_from;
        const style = () => (band === null ? place(shape.from_ms, shape.to_ms) : {
          ...place(shape.from_ms, shape.to_ms),
          '--join': `${at(band).toFixed(2)}%`,
          '--join-w': `${(at(shape.to_ms) - at(band)).toFixed(2)}%`,
        });
        return <i class={{ work: true, joined: shape.joined, band: !shape.joined && band !== null, ...run(i) }} style={style()} />;
      }
    }
  };
  return (
    <div
      class={{ bar: true, last: props.bar.last }}
      style={{ left: `${pct(props.bar.start_ms, props.scale).toFixed(2)}%`, width: `${pct(props.bar.span_ms, props.scale).toFixed(2)}%` }}
      title={props.bar.title}
    >
      {/* Rebuilt with the segment: a shape's place is its kind's. */}
      <For each={props.bar.shapes}>{(shape, i) => piece(shape, untrack(i))}</For>
    </div>
  );
}

function LaneRow(props: { lane: Lane; scale: number }) {
  return (
    <>
      <div class="lane-label"><span class={`tierchip t-${props.lane.tier}`}>{props.lane.name}</span><small>{props.lane.share}</small></div>
      <div class={`track t-${props.lane.tier}`}>
        <div class="track-in">
          <Show when={props.lane.reserved}>
            {(reserved) => (
              <div class="share" style={{ left: `${pct(reserved()[0], props.scale).toFixed(2)}%`, width: `${Math.max(0.5, pct(reserved()[1], props.scale)).toFixed(2)}%` }} />
            )}
          </Show>
          <For each={props.lane.bars} keyed={false}>
            {(bar) => (
              <>
                <Segment bar={bar()} scale={props.scale} />
                {/* After the bar as drawn: a short one is drawn wider than its time. */}
                <Show when={bar().label}>
                  {(label) => {
                    const end = () => barEnd(bar(), props.scale);
                    return <span class={{ 'lane-time': true, before: end() > 88 }} style={{ left: `${Math.min(99.5, end()).toFixed(2)}%` }}>{label()}</span>;
                  }}
                </Show>
              </>
            )}
          </For>
        </div>
      </div>
      <div class="outcome">
        <Show when={props.lane.outcome}>
          {(chip) => <span class={`engine-chip ${chip().tone}`} title={chip().text}>{chip().text}</span>}
        </Show>
      </div>
    </>
  );
}

/** The recent rounds, each with what keeps its place as the ribbon moves on:
 *  a round by its number, a sleep by the round before it. */
function keyedRibbon(ribbon: RibbonMark[]): { key: string; mark: RibbonMark }[] {
  let before = 'start', sleeps = 0;
  return ribbon.map((mark) => {
    if (mark.kind === 'round') {
      [before, sleeps] = [String(mark.number), 0];
      return { key: `round-${mark.number}`, mark };
    }
    return { key: `sleep-after-${before}-${sleeps++}`, mark };
  });
}

/** The round's total, under the lanes where the segment that finished last
 *  ends (its marker is on that segment alone). */
function EndMarker(props: { round: RoundView }) {
  const left = () => {
    const last = props.round.lanes.flatMap((lane) => lane.bars).find((bar) => bar.last);
    return last ? barEnd(last, props.round.scale_ms) : pct(props.round.elapsed_ms, props.round.scale_ms);
  };
  return <span class="ruler-label" style={{ left: `${Math.min(99.5, left()).toFixed(2)}%` }}>{props.round.elapsed}</span>;
}

export function RoundCard(props: {
  round: RoundView | null;
  ribbon: RibbonMark[];
  /** The past round shown, if one is. */
  pinned: number | null;
  onPin: (number: number) => void;
  onResume: () => void;
}) {
  return (
    <section class="engine-card" id="engine-round" aria-labelledby="h-round">
      <Show
        when={props.round}
        fallback={<header><h2 id="h-round" title={ROUND_TITLE}>Round</h2><span class="engine-hint">No round recorded yet.</span></header>}
      >
        {(round) => (
          <>
            <header class="round-head">
              <h2 id="h-round" title={ROUND_TITLE}>{round().title}</h2>
              <Show when={props.pinned !== null}>
                <button type="button" class="engine-chip round-paused" id="round-resume" title="Showing a past round: back to the live one" onClick={() => props.onResume()}>
                  × Paused
                </button>
              </Show>
              <span class="engine-hint round-state">{round().state}</span>
            </header>
            <div class="lanes">
              <For each={round().lanes} keyed={(lane) => lane.tier}>{(lane) => <LaneRow lane={lane()} scale={round().scale_ms} />}</For>
              <div />
              <div class="ruler"><div class="ruler-in"><EndMarker round={round()} /></div></div>
              <div />
            </div>
            <Disclosure summary="Timing details" data-round-breakdown="">
              <For each={round().lanes.flatMap((lane) => lane.bars)} keyed={false}>
                {(bar) => (
                  <>
                    <p><strong>{bar().title}</strong></p>
                    <ul><For each={bar().details} keyed={false}>{(detail) => <li>{detail()}</li>}</For></ul>
                  </>
                )}
              </For>
            </Disclosure>
          </>
        )}
      </Show>
      <div class="ribbon-row">
        <span class="engine-hint">Last rounds</span>
        <div class="ribbon" id="ribbon" aria-label="Recent rounds">
          <For each={keyedRibbon(props.ribbon)} keyed={(entry) => entry.key}>
            {(entry) => {
              // A round is a round for good, a sleep a sleep: the key says.
              const round = () => entry().mark as Extract<RibbonMark, { kind: 'round' }>;
              const sleep = () => entry().mark as Extract<RibbonMark, { kind: 'sleep' }>;
              return untrack(entry).mark.kind === 'round' ? (
                <button
                  type="button"
                  class={{ rbar: true, pinned: props.pinned === round().number }}
                  data-round={round().number}
                  style={{ height: `${round().height}px` }}
                  title={round().title}
                  aria-label={round().title}
                  onClick={() => props.onPin(round().number)}
                >
                  <For each={round().parts} keyed={false}>
                    {(part) => <i class={`t-${part()[0]}`} style={{ height: `${(part()[1] * 100).toFixed(1)}%` }} />}
                  </For>
                </button>
              ) : (
                <i class={{ rgap: true, woken: sleep().woken }} title={sleep().title} />
              );
            }}
          </For>
        </div>
        <div class="legend">
          <span><i class="rgap" aria-hidden="true" />slept</span>
          <span><i class="rgap woken" aria-hidden="true" />woken by a new block</span>
          <span><i class="rpair" aria-hidden="true"><i /><i /></i>back to back: work was left</span>
        </div>
      </div>
    </section>
  );
}
