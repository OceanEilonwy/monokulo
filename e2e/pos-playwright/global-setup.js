// Boots the real backend for this suite (crates/scanner/src/bin/e2e_harness.rs
// - a real, network-bound engine against the real public stagenet node, a
// real, network-bound monokulo with one real account/store already connected,
// and this process's own internal /send-payment endpoint) once, before either
// test file runs, and tears it down once after both finish - see that
// binary's own doc comment for exactly what it provisions and why this is a
// real `[[bin]]` rather than another `cargo test`.
//
// Returning a teardown function from `globalSetup` (rather than a separate
// `globalTeardown` file) is deliberate: Playwright guarantees that returned
// function runs in the *same* Node process as this one, so it can close over
// `child` directly - no pid file, no second lookup, no race.

const { spawn, execFileSync } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const FIXTURE_PATH = path.join(__dirname, '.pos-e2e-fixture.json');
const SERVER_BIN = path.join(REPO_ROOT, 'target', 'debug', 'e2e-harness');
const READY_MARKER = 'POS_E2E_READY ';

module.exports = async function globalSetup() {
  console.log('[global-setup] building the real stagenet e2e harness (cargo build --features e2e)...');
  execFileSync(
    'cargo',
    ['build', '-p', 'scanner', '--features', 'e2e', '--bin', 'e2e-harness'],
    { cwd: REPO_ROOT, stdio: 'inherit' },
  );

  console.log('[global-setup] starting e2e-harness (real stagenet-bound engine + monokulo)...');
  const child = spawn(SERVER_BIN, [], { cwd: REPO_ROOT, stdio: ['ignore', 'pipe', 'inherit'] });

  const fixture = await new Promise((resolve, reject) => {
    let buffered = '';
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error('timed out waiting for e2e-harness to print its ready line (60s)'));
    }, 60_000);

    const onData = (chunk) => {
      if (settled) return;
      buffered += chunk.toString('utf8');
      const idx = buffered.indexOf(READY_MARKER);
      if (idx === -1) return;
      const line = buffered.slice(idx + READY_MARKER.length).split('\n')[0];
      settled = true;
      clearTimeout(timer);
      child.stdout.off('data', onData);
      try {
        resolve(JSON.parse(line));
      } catch (e) {
        reject(new Error(`failed to parse e2e-harness's ready line: ${e}\nline: ${line}`));
      }
    };
    child.stdout.on('data', onData);
    child.once('exit', (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new Error(`e2e-harness exited early (code ${code}) before becoming ready - check the stagenet node is reachable and stdout above for its own error`));
    });
  });

  fs.writeFileSync(FIXTURE_PATH, JSON.stringify(fixture, null, 2));
  console.log(`[global-setup] ready - monokulo at ${fixture.monokulo_base_url}, connection ${fixture.connection_id}`);

  return async function globalTeardown() {
    console.log('[global-teardown] stopping e2e-harness...');
    child.kill('SIGTERM');
    try {
      fs.unlinkSync(FIXTURE_PATH);
    } catch {
      // Already gone, or never written - fine either way.
    }
  };
};
