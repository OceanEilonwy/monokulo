// Starts the real `monokulo-engine` and `monokulo` binaries - their real `main` and
// boot wiring - against empty databases in a temporary directory, plus a
// local fake monerod, so tests exercise exactly what an operator runs
// (admin_settings_v2.md task 6.0). Everything is offline: no stagenet node,
// no real funds.
//
// `buildBinaries` runs once, as the suite's global setup. Each spec file then
// gets a stack of its own (`startStack`, through `useRealStack` in
// tests/backend-helpers.js), so every file starts from a fresh instance and
// files can run side by side on several workers.

const { spawn, execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
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
// developer's ENGINE_* or MONOKULO_* variables can't override what the
// specs save.
function cleanEnv() {
  return Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('ENGINE_') && !key.startsWith('MONOKULO_')));
}

// Every binary the browser suites run, in one cargo command: the real
// stack's three and the coverage fixture server. Cargo unifies features
// across the packages a command names, so building them with different
// commands compiles engine and monokulo twice; with this one, the fixture's
// own rebuild check (coverage-fixture.js) finds nothing to do. With the snp
// key custody backend, as every test build is (zmq is on by default).
const BUILD_ARGS = [
  'build', '--locked', '--features', 'monokulo/snp',
  '-p', 'engine', '--bin', 'monokulo-engine',
  '-p', 'monokulo', '--bin', 'monokulo', '--example', 'coverage_fixture',
  '-p', 'engine-test-support', '--bin', 'fake-monerod',
];

function buildBinaries() {
  console.log('[real-binaries] building monokulo-engine, monokulo, the coverage fixture and fake-monerod...');
  execFileSync('cargo', BUILD_ARGS, { cwd: REPO_ROOT, stdio: 'inherit' });
}

/**
 * A reverse proxy serving `target` (`http://127.0.0.1:port`) under `prefix`
 * (`/monokulo`) on `port`, as an operator's nginx would: it strips the
 * prefix before passing a request on, streams both ways (server-sent events
 * included) and keeps the Host the browser sent. A request outside the
 * prefix never reaches Monokulo: it's answered 404 and kept in `misses`,
 * so a URL Monokulo gave without its base path shows up.
 */
function startPrefixProxy(port, prefix, target) {
  const misses = [];
  const upstream = new URL(target);
  const server = http.createServer((request, response) => {
    const url = request.url || '/';
    if (url !== prefix && !url.startsWith(prefix + '/') && !url.startsWith(prefix + '?')) {
      misses.push(`${request.method} ${url}`);
      response.writeHead(404, { 'content-type': 'text/plain' });
      response.end('Not under ' + prefix);
      return;
    }
    const rest = url.slice(prefix.length);
    const forwarded = http.request({
      host: upstream.hostname,
      port: upstream.port,
      method: request.method,
      path: rest.startsWith('/') ? rest : '/' + rest,
      headers: request.headers,
    }, (answer) => {
      response.writeHead(answer.statusCode || 502, answer.headers);
      answer.pipe(response);
    });
    forwarded.on('error', () => { response.destroy(); });
    request.pipe(forwarded);
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => resolve({
      misses,
      close: () => new Promise((done) => { server.closeAllConnections(); server.close(() => done()); }),
    }));
  });
}

/**
 * Starts the three processes; resolves to `{ fixture, stop }`.
 * `options.prefix` (`/monokulo`) serves monokulo behind a
 * reverse proxy under that path (startPrefixProxy), with `public_uri` the
 * proxy's address: the fixture's `monokulo_url` is then the proxy's, path
 * and all, `direct_url` monokulo's own, and `proxy_misses` every request
 * the proxy turned away.
 */
async function startStack(options = {}) {

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'mokulo-e2e-'));
  const log = (name) => fs.openSync(path.join(dir, `${name}.log`), 'a');
  const children = [];

  try {
  const fake = spawn(BIN('fake-monerod'), ['--height', '1000'], { stdio: ['ignore', 'pipe', log('fake-monerod')] });
  children.push(fake);
  const fakeAddress = await readyLine(fake, 'FAKE_MONEROD_READY ');

  const enginePort = await freePort();
  const engineToken = crypto.randomBytes(32).toString('hex');
  // Each process's options file, in this run's directory: never the
  // user's own in ~/.config, which a spec saving on the admin page would
  // otherwise write.
  const engineOptions = path.join(dir, 'engine.toml');
  fs.writeFileSync(engineOptions, [
    '[server]',
    `bind = "127.0.0.1:${enginePort}"`,
    // A standalone engine limits monokulo's own calls (those without a
    // store's key) to one budget for the instance, 600 a minute by default.
    // Every spec shares this engine, so status reloads, the Logs and engine
    // pages and settings saves across specs add up: the limit stays on, as
    // in production, with room for the whole suite. Each store keeps its
    // own default budget.
    'instance_rate_limit = true',
    'instance_rate_limit_per_min = 100000',
    '[database]',
    `path = ${JSON.stringify(path.join(dir, 'engine.db'))}`,
    '',
  ].join('\n'));
  // E2E_ENGINE_BIN runs another engine build, e.g. an older one to check a
  // spec fails against the bug it guards.
  const engine = spawn(process.env.E2E_ENGINE_BIN || BIN('monokulo-engine'), ['--options', engineOptions], {
    env: { ...cleanEnv(), ENGINE_TOKEN: engineToken },
    stdio: ['ignore', log('engine'), log('engine')],
  });
  children.push(engine);
  const engineUrl = `http://127.0.0.1:${enginePort}`;
  await waitFor(`${engineUrl}/status`, 'the engine', engine);

  const monokuloPort = await freePort();
  const proxyPort = options.prefix ? await freePort() : null;
  const publicUri = options.prefix ? `http://127.0.0.1:${proxyPort}${options.prefix}` : null;
  const monokuloOptions = path.join(dir, 'monokulo.toml');
  fs.writeFileSync(monokuloOptions, [
    ...(publicUri ? [`public_uri = "${publicUri}"`] : []),
    '[server]',
    `bind = "127.0.0.1:${monokuloPort}"`,
    // The engine is a process of its own here (engine-crash-recovery kills it), so
    // monokulo reaches it remotely, not inside itself.
    '[engine]',
    'mode = "remote"',
    `url = "${engineUrl}"`,
    '[database]',
    `path = ${JSON.stringify(path.join(dir, 'monokulo.db'))}`,
    '',
  ].join('\n'));
  const monokulo = spawn(BIN('monokulo'), ['--options', monokuloOptions], {
    cwd: dir,
    env: {
      ...cleanEnv(),
      MONOKULO_ENCRYPTION_KEY: crypto.randomBytes(32).toString('hex'),
      MONOKULO_ENGINE_TOKEN: engineToken,
    },
    stdio: ['ignore', log('monokulo'), log('monokulo')],
  });
  children.push(monokulo);
  const monokuloUrl = `http://127.0.0.1:${monokuloPort}`;
  await waitFor(`${monokuloUrl}/`, 'monokulo', monokulo);

  var proxy = options.prefix ? await startPrefixProxy(proxyPort, options.prefix, monokuloUrl) : null;
  var fixture = {
    monokulo_url: publicUri || monokuloUrl, direct_url: monokuloUrl, engine_url: engineUrl, fake_monerod: fakeAddress, logs: dir,
    proxy_misses: proxy ? proxy.misses : [],
  };
  } catch (e) {
    for (const child of children) child.kill('SIGKILL');
    if (!process.env.KEEP_E2E_LOGS) fs.rmSync(dir, { recursive: true, force: true });
    throw e;
  }
  if (process.env.KEEP_E2E_LOGS) console.log(`[real-binaries] monokulo ${fixture.monokulo_url}, engine ${fixture.engine_url}, fake monerod ${fixture.fake_monerod}, logs in ${dir}`);

  async function stop() {
    if (proxy) await proxy.close();
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

/**
 * Starts one more fake monerod that says it's on `nettype` (for a node on
 * the wrong network, or a second node on the right one); resolves to
 * `{ address, stop }`.
 */
async function startFakeNode(nettype) {
  const child = spawn(BIN('fake-monerod'), ['--height', '1000', '--nettype', nettype], { stdio: ['ignore', 'pipe', 'ignore'] });
  const address = await readyLine(child, 'FAKE_MONEROD_READY ');
  const stop = () => new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve();
    child.once('exit', () => resolve());
    child.kill('SIGTERM');
  });
  return { address, stop };
}

module.exports = { BUILD_ARGS, buildBinaries, startStack, startFakeNode };
