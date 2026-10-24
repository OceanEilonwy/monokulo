// The "Machine and links" strip, from the figures of a `machine` event.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Tile } from '../src/machine.ts';
import { SLOT_SECS, resourceTiles } from '../src/machine.ts';
import type { Machine, ResourceReport, ResourceSample } from '../src/wire.ts';

const NOW = 1_800_000_000;

const report = (host: string, samples: ResourceSample[], more: Partial<ResourceReport> = {}): ResourceReport => ({
  host_id: host, cpu_count: 4, machine_memory_bytes: 8_000_000_000, cgroup_memory_bytes: null, samples, ...more,
});
/** A sample `back` slots before the newest, at `cpu` % and `mb` MB. */
const sample = (back: number, cpu: number, mb: number): ResourceSample => ({
  unix: NOW - (NOW % SLOT_SECS) - back * SLOT_SECS, cpu_percent: cpu, memory_bytes: mb * 1_000_000,
});
const machine = (resources: Machine['resources'], more: Partial<Machine> = {}): Machine => ({
  network: 'mainnet', at_unix: NOW, now_unix: NOW, resources, node: null, scaling: null, ...more,
});
const stack = (tile: Tile) => {
  assert.equal(tile.chart.kind, 'stack', `not a stacked chart: ${JSON.stringify(tile.chart)}`);
  return tile.chart as Extract<Tile['chart'], { kind: 'stack' }>;
};

test('different machines are shown apart, and a silent engine says so', () => {
  const apart = resourceTiles(machine({
    engine: report('boot-1', [sample(0, 23, 412)]),
    monokulo: report('boot-2', [sample(0, 2, 96)]),
    one_process: false,
  }));
  assert.deepEqual(apart.map((t) => t.label), ['Engine CPU', 'Engine memory', 'Monokulo CPU', 'Monokulo memory']);
  assert.equal(apart[0].value, '23 %');
  assert.equal(apart[0].key, 'engine-cpu');
  assert.equal(stack(apart[0]).layers.length, 1, 'nothing is stacked');
  const silent = resourceTiles(machine({ engine: null, monokulo: report('boot-2', []), one_process: false }));
  assert.equal(silent[0].label, 'Monokulo CPU');
  assert.equal(silent[0].note, 'no samples yet');
});
