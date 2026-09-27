// Starts the real `scanner` and `monokulo` binaries - their real `main` and
// boot wiring - against empty databases in a temporary directory, plus a
// local fake monerod, so tests exercise exactly what an operator runs
// (admin_settings_v2.md task 6.0). Everything is offline: no stagenet node,
// no real funds.
//
// Writes `.real-binaries-fixture.json` for the specs and returns a teardown
// that stops all three processes and removes the temporary directory.

const { spawn, execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const BIN = (name) => path.join(REPO_ROOT, 'target', 'debug', name);
const FIXTURE_PATH = path.join(__dirname, '.real-binaries-fixture.json');

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

module.exports = async function globalSetup() {
  console.log('[real-binaries] building scanner, monokulo and fake-monerod...');
  execFileSync(
    'cargo',
    ['build', '-p', 'scanner', '--bin', 'scanner', '-p', 'monokulo', '--bin', 'monokulo', '-p', 'scanner-test-support', '--bin', 'fake-monerod'],
    { cwd: REPO_ROOT, stdio: 'inherit' },
  );

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'mokulo-e2e-'));
  const log = (name) => fs.openSync(path.join(dir, `${name}.log`), 'a');
  const children = [];

  const fake = spawn(BIN('fake-monerod'), ['--height', '1000'], { stdio: ['ignore', 'pipe', log('fake-monerod')] });
  children.push(fake);
  const fakeAddress = await readyLine(fake, 'FAKE_MONEROD_READY ');

  const enginePort = await freePort();
  const engineToken = crypto.randomBytes(32).toString('hex');
  // E2E_SCANNER_BIN runs another engine build, e.g. an older one to check a
  // spec fails against the bug it guards.
  const engine = spawn(process.env.E2E_SCANNER_BIN || BIN('scanner'), [], {
    env: {
      ...process.env,
      SCANNER_DB_PATH: path.join(dir, 'engine.db'),
      SCANNER_SERVER_BIND: `127.0.0.1:${enginePort}`,
      SCANNER_ADMIN_TOKEN: engineToken,
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
      ...process.env,
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

  const fixture = { monokulo_url: monokuloUrl, engine_url: engineUrl, fake_monerod: fakeAddress, logs: dir };
  fs.writeFileSync(FIXTURE_PATH, JSON.stringify(fixture, null, 2));
  console.log(`[real-binaries] ready: monokulo ${monokuloUrl}, engine ${engineUrl}, fake monerod ${fakeAddress}, logs in ${dir}`);

  return async function teardown() {
    for (const child of children.reverse()) child.kill('SIGTERM');
    await new Promise((r) => setTimeout(r, 500));
    try { fs.unlinkSync(FIXTURE_PATH); } catch { /* already gone */ }
    if (!process.env.KEEP_E2E_LOGS) fs.rmSync(dir, { recursive: true, force: true });
  };
};
