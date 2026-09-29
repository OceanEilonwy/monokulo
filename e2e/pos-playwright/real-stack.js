// Starts the real `scanner` and `monokulo` binaries - their real `main` and
// boot wiring - against empty databases in a temporary directory, plus a
// local fake monerod, so tests exercise exactly what an operator runs
// (admin_settings_v2.md task 6.0). Everything is offline: no stagenet node,
// no real funds.
//
// `buildBinaries` runs once, as the suite's global setup. Each spec file then
// gets a stack of its own (`startStack`, through `useRealStack` in
// tests/real-helpers.js), so every file starts from a fresh instance and
// files can run side by side on several workers.

const { spawn, execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const BIN = (name) => path.join(REPO_ROOT, 'target', 'debug', name);

function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

async function waitFor(url, what, child) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`${what} exited early (code ${child.exitCode})`);
    try {
      const response = await fetch(url, { redirect: 'manual' });
      if (response.status < 500) return;
    } catch {
      // Not listening yet.
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${what} didn't answer at ${url} within 30s`);
}

function readyLine(child, marker) {
  return new Promise((resolve, reject) => {
    let buffered = '';
    const timer = setTimeout(() => reject(new Error(`no ${marker} line within 10s`)), 10_000);
    child.stdout.on('data', (chunk) => {
      buffered += chunk.toString('utf8');
      const at = buffered.indexOf(marker);
      if (at === -1) return;
      clearTimeout(timer);
      resolve(buffered.slice(at + marker.length).split('\n')[0].trim());
    });
    child.once('exit', (code) => reject(new Error(`exited (code ${code}) before ${marker}`)));
  });
}

// The process environment without the services' own settings, so a
// developer's SCANNER_* or MONOKULO_* variables can't override what the
// specs save.
function cleanEnv() {
  return Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('SCANNER_') && !key.startsWith('MONOKULO_')));
}

function buildBinaries() {
  console.log('[real-binaries] building scanner, monokulo, fake-monerod and key-custody-server...');
  execFileSync(
    'cargo',
    ['build', '-p', 'scanner', '--bin', 'scanner', '-p', 'monokulo', '--bin', 'monokulo', '-p', 'scanner-test-support', '--bin', 'fake-monerod', '-p', 'key-custody-server', '--bin', 'key-custody-server'],
    { cwd: REPO_ROOT, stdio: 'inherit' },
  );
}

/** Starts the three processes; resolves to `{ fixture, stop }`. */
async function startStack() {

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'mokulo-e2e-'));
  const log = (name) => fs.openSync(path.join(dir, `${name}.log`), 'a');
  const children = [];

  try {
  const fake = spawn(BIN('fake-monerod'), ['--height', '1000'], { stdio: ['ignore', 'pipe', log('fake-monerod')] });
  children.push(fake);
  const fakeAddress = await readyLine(fake, 'FAKE_MONEROD_READY ');

  const enginePort = await freePort();
  const engineToken = crypto.randomBytes(32).toString('hex');
  // E2E_SCANNER_BIN runs another engine build, e.g. an older one to check a
  // spec fails against the bug it guards.
  const engine = spawn(process.env.E2E_SCANNER_BIN || BIN('scanner'), [], {
    env: {
      ...cleanEnv(),
      SCANNER_DB_PATH: path.join(dir, 'engine.db'),
      SCANNER_SERVER_BIND: `127.0.0.1:${enginePort}`,
      SCANNER_ADMIN_TOKEN: engineToken,
      // Every spec shares this engine and monokulo's one admin token, so the
      // default 120 a minute is spent across specs (status reloads, the Logs
      // page) and a later spec's settings save gets a 429 on a slow runner.
      SCANNER_SERVER_RATE_LIMIT_PER_TOKEN_PER_MIN: '100000',
    },
    stdio: ['ignore', log('engine'), log('engine')],
  });
  children.push(engine);
  const engineUrl = `http://127.0.0.1:${enginePort}`;
  await waitFor(`${engineUrl}/status`, 'the engine', engine);

  const monokuloPort = await freePort();
  const monokulo = spawn(BIN('monokulo'), [], {
    cwd: dir,
    env: {
      ...cleanEnv(),
      MONOKULO_ENCRYPTION_KEY: crypto.randomBytes(32).toString('hex'),
      MONOKULO_DB_PATH: path.join(dir, 'monokulo.db'),
      MONOKULO_BIND: `127.0.0.1:${monokuloPort}`,
      MONOKULO_ENGINE_URL: engineUrl,
      MONOKULO_SCANNER_ADMIN_TOKEN: engineToken,
    },
    stdio: ['ignore', log('monokulo'), log('monokulo')],
  });
  children.push(monokulo);
  const monokuloUrl = `http://127.0.0.1:${monokuloPort}`;
  await waitFor(`${monokuloUrl}/`, 'monokulo', monokulo);

  var fixture = { monokulo_url: monokuloUrl, engine_url: engineUrl, fake_monerod: fakeAddress, logs: dir };
  } catch (e) {
    for (const child of children) child.kill('SIGKILL');
    if (!process.env.KEEP_E2E_LOGS) fs.rmSync(dir, { recursive: true, force: true });
    throw e;
  }
  if (process.env.KEEP_E2E_LOGS) console.log(`[real-binaries] monokulo ${fixture.monokulo_url}, engine ${fixture.engine_url}, fake monerod ${fixture.fake_monerod}, logs in ${dir}`);

  async function stop() {
    await Promise.all(children.reverse().map((child) => new Promise((resolve) => {
      if (child.exitCode !== null || child.signalCode !== null) return resolve();
      const timer = setTimeout(() => { child.kill('SIGKILL'); resolve(); }, 5000);
      child.once('exit', () => { clearTimeout(timer); resolve(); });
      child.kill('SIGTERM');
    })));
    if (!process.env.KEEP_E2E_LOGS) fs.rmSync(dir, { recursive: true, force: true });
  }
  return { fixture, stop };
}

module.exports = { buildBinaries, startStack };
