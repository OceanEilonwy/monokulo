// The "Machine and links" strip: small multiples of the machine's CPU and
// memory, the link to the node in use and the blocks' size, each `machine`
// event's figures (machine.ts), the chip saying how fresh they are.
import { For, Match, Switch } from 'solid-js';
import type { MachineView, Tile } from './machine.ts';

function TileView(props: { tile: Tile }) {
  return (
    <div class="tile" data-tile={props.tile.key} title={props.tile.title}>
      <span class="k">{props.tile.label}</span>
      <span class="v">{props.tile.value}</span>
      <span class="s">{props.tile.note}</span>
      <span class="visually-hidden">{props.tile.title}</span>
      <Switch>
        <Match when={props.tile.chart.kind === 'stack' && props.tile.chart}>
          {(chart) => (
            <svg class="resource-chart tile-chart" viewBox="0 0 360 100" preserveAspectRatio="none" aria-hidden="true">
              <For each={chart().layers}>{(layer) => <path class={`chart-layer ${layer.class}`} d={layer.d} />}</For>
              <For each={chart().limits}>
                {(limit) => (
                  <line class={`chart-limit ${limit.class}`} x1="0" x2="360" y1={limit.y} y2={limit.y} vector-effect="non-scaling-stroke">
                    <title>{limit.label}</title>
                  </line>
                )}
              </For>
            </svg>
          )}
        </Match>
        <Match when={props.tile.chart.kind === 'spark' && props.tile.chart}>
          {(chart) => (
            <svg class="sparkline tile-spark" viewBox="0 0 59 16" preserveAspectRatio="none" aria-hidden="true">
              <For each={chart().runs}>
                {(points) => {
                  if (points.includes(' ')) return <polyline points={points} />;
                  const [x, y] = points.split(',');
                  return <circle cx={x} cy={y} r="1" />;
                }}
              </For>
            </svg>
          )}
        </Match>
      </Switch>
    </div>
  );
}

export function MachineStrip(props: { machine: MachineView; age: string | null }) {
  return (
    <section class="engine-card tiles-card" id="engine-machine" aria-labelledby="h-machine">
      <header class="tiles-head">
        <h2 id="h-machine">Machine and links</h2>
        <ul class="chart-legend" id="machine-legend" hidden={!props.machine.stacked}>
          <li><span class="chart-swatch chart-engine" />engine</li>
          <li><span class="chart-swatch chart-monokulo" />monokulo</li>
        </ul>
        <span class="live-chip" id="machine-live" title="Updated every few seconds while the page follows the engine">
          <span class="live-dot" />
          <span id="machine-age">{props.age ?? 'live'}</span>
        </span>
      </header>
      <div class="tiles" id="machine-tiles">
        <For each={props.machine.tiles} keyed={(tile) => tile.key}>{(tile) => <TileView tile={tile()} />}</For>
      </div>
    </section>
  );
}
