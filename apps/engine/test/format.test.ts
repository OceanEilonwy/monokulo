// Figures written as monokulo's Rust writes them: Rust's `{:.N}` takes an
// exact half to the even digit, where `toFixed` rounds it up.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { bytes, compact, fixed, megabytes, rate, seconds } from '../src/format.ts';
import { resourceTiles } from '../src/machine.ts';

test('an exact half goes to the even digit, as in Rust; anything else to the nearer', () => {
  // What `format!("{:.N}")` printed for each.
  assert.equal(fixed(12.5, 0), '12');
  assert.equal(fixed(13.5, 0), '14');
  assert.equal(fixed(0.5, 0), '0');
  assert.equal(fixed(1.25, 1), '1.2');
  assert.equal(fixed(0.125, 2), '0.12');
  assert.equal(fixed(1.25, 1), '1.2');
  // Not exactly half way in binary: the nearer, as toFixed.
  assert.equal(fixed(1.35, 1), '1.4');
  assert.equal(fixed(1.15, 1), '1.1');
  assert.equal(fixed(7.84, 1), '7.8');
  assert.equal(fixed(100, 0), '100');
  assert.equal(fixed(-2.5, 0), '-2');
});

test('the figures built on it agree with the Rust that wrote them before', () => {
  assert.equal(seconds(125), '0.12s');
  assert.equal(seconds(1_250), '1.2s');
  assert.equal(compact(1_250), '1.2k');
  assert.equal(bytes(1_250_000), '1.2 MB');
  assert.equal(rate(156_250), '1.2 Mbit/s');
  assert.equal(megabytes(1.25 * 1024 * 1024), '1.2 MB');
  // One busy core of eight is 12.5 %.
  const now = 1_800_000_000;
  const report = { host_id: 'h', cpu_count: 8, machine_memory_bytes: null, cgroup_memory_bytes: null,
    samples: [{ unix: now - (now % 10), cpu_percent: 12.5, memory_bytes: 0 }] };
  const [cpu] = resourceTiles({ network: 'mainnet', at_unix: now, now_unix: now, node: null, scaling: null,
    resources: { engine: null, monokulo: report, one_process: false } });
  assert.equal(cpu.value, '12 %');
});
