// Boots the real backend half of this suite (crates/scanner/src/bin/pos_e2e_server.rs
// - a real, network-bound engine against the real public stagenet node, plus a
// real, network-bound monokulo with one real account/store already connected)
// once, before either test file runs, and tears it down once after both finish -
// see that binary's own doc comment for exactly what it provisions and why this
// is a real `[[bin]]` rather than another `cargo test`.
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
const SERVER_BIN = path.join(REPO_ROOT, 'target', 'debug', 'pos-e2e-server');
const READY_MARKER = 'POS_E2E_READY ';

module.exports = async function globalSetup() {
  console.log('[global-setup] building the real stagenet e2e binaries (cargo build --features e2e)...');
  execFileSync(
    'cargo',
    ['build', '-p', 'scanner', '--features', 'e2e', '--bin', 'pos-e2e-server', '--bin', 'pos-e2e-send-payment'],
    { cwd: REPO_ROOT, stdio: 'inherit' },
  );

  console.log('[global-setup] starting pos-e2e-server (real stagenet-bound engine + monokulo)...');
  const child = spawn(SERVER_BIN, [], { cwd: REPO_ROOT, stdio: ['ignore', 'pipe', 'inherit'] });

  const fixture = await new Promise((resolve, reject) => {
    let buffered = '';
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error('timed out waiting for pos-e2e-server to print its ready line (60s)'));
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
        reject(new Error(`failed to parse pos-e2e-server's ready line: ${e}\nline: ${line}`));
      }
    };
    child.stdout.on('data', onData);
    child.once('exit', (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new Error(`pos-e2e-server exited early (code ${code}) before becoming ready - check the stagenet node is reachable and stdout above for its own error`));
    });
  });

  fs.writeFileSync(FIXTURE_PATH, JSON.stringify(fixture, null, 2));
  console.log(`[global-setup] ready - monokulo at ${fixture.monokulo_base_url}, connection ${fixture.connection_id}`);

  return async function globalTeardown() {
    console.log('[global-teardown] stopping pos-e2e-server...');
    child.kill('SIGTERM');
    try {
      fs.unlinkSync(FIXTURE_PATH);
    } catch {
      // Already gone, or never written - fine either way.
    }
  };
};
