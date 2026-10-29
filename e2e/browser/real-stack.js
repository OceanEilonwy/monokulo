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
const os = require('node:os');
const path = require('node:path');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const BIN = (name) => path.join(REPO_ROOT, 'target', 'debug', name);

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

/**
 * Resolves to the rest of the first whole line `child` writes to `stream`
 * (its stdout or stderr) after `marker`: the address a process bound,
 * asked for port 0 so no other worker can take it between a pick and the
 * bind. `hint` ends the error when the line doesn't come.
 */
function readyLine(child, stream, marker, { timeout = 10_000, hint = '' } = {}) {
  return new Promise((resolve, reject) => {
    let buffered = '';
    const settle = (finish) => {
      clearTimeout(timer);
      stream.off('data', onData);
      child.off('exit', onExit);
      finish();
    };
    const timer = setTimeout(() => settle(() => reject(new Error(`no ${marker.trim()} line within ${timeout / 1000}s${hint}`))), timeout);
    const onData = (chunk) => {
      buffered += chunk.toString('utf8');
      const at = buffered.indexOf(marker);
      if (at === -1) return;
      const end = buffered.indexOf('\n', at);
      if (end === -1) return;
      settle(() => resolve(buffered.slice(at + marker.length, end).trim()));
    };
    const onExit = (code) => settle(() => reject(new Error(`exited (code ${code}) before ${marker.trim()}${hint}`)));
    stream.on('data', onData);
    child.once('exit', onExit);
  });
}

// A piped stream copied into a log file for the life of its process, so the
// process never blocks on a pipe nobody reads.
function tee(stream, file) {
  stream.pipe(fs.createWriteStream(file, { flags: 'a' }));
}

// The process environment without the services' own settings, so a
// developer's ENGINE_* or MONOKULO_* variables can't override what the
// specs save.
function cleanEnv() {
  return Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('ENGINE_') && !key.startsWith('MONOKULO_')));
}

// Every binary the browser suites run but xtask, in one cargo command: the
// real stack's three and the coverage fixture server. Cargo unifies features
// across the packages a command names, so building them with different
// commands compiles engine and monokulo twice. With the snp key custody
// backend, as every test build is (zmq is on by default).
const BUILD_ARGS = [
  'build', '--locked', '--features', 'monokulo/snp',
  '-p', 'engine', '--bin', 'monokulo-engine',
  '-p', 'monokulo', '--bin', 'monokulo', '--example', 'coverage_fixture',
  '-p', 'engine-test-support', '--bin', 'fake-monerod',
];

// xtask, which api-explorer.spec.js runs to build the API explorer, in a
// command of its own: named in BUILD_ARGS, its dependencies' features would
// change engine's and monokulo's, and every other build of them (cargo
// xtask, nextest) would compile them again. Built here, as `cargo xtask`
// builds it, so no spec runs cargo while other workers' specs wait on the
// build lock it holds.
const XTASK_BUILD_ARGS = ['build', '--locked', '-p', 'xtask', '--bin', 'xtask'];

// The binaries BUILD_ARGS and XTASK_BUILD_ARGS make, where cargo leaves them.
const BINARIES = [BIN('monokulo-engine'), BIN('monokulo'), BIN(path.join('examples', 'coverage_fixture')), BIN('fake-monerod'), BIN('xtask')];

// E2E_PREBUILT_BINARIES=1 runs the binaries already in target/debug instead:
// CI's browser job downloads them from the job that built the Rust tests,
// and Cargo, which goes by modification times, would build them again.
function prebuilt() {
  return process.env.E2E_PREBUILT_BINARIES === '1';
}

function buildBinaries() {
  if (prebuilt()) {
    const missing = BINARIES.filter((binary) => !fs.existsSync(binary));
    if (missing.length) throw new Error(`E2E_PREBUILT_BINARIES is set, but these aren't built: ${missing.join(', ')}`);
    console.log('[real-binaries] running the prebuilt monokulo-engine, monokulo, coverage fixture, fake-monerod and xtask in target/debug');
    return;
  }
  console.log('[real-binaries] building monokulo-engine, monokulo, the coverage fixture, fake-monerod and xtask...');
  execFileSync('cargo', BUILD_ARGS, { cwd: REPO_ROOT, stdio: 'inherit' });
  execFileSync('cargo', XTASK_BUILD_ARGS, { cwd: REPO_ROOT, stdio: 'inherit' });
}

/**
 * A reverse proxy serving under `prefix` (`/monokulo`) on a port of its
 * own, as an operator's nginx would: it strips the prefix before passing a
 * request on, streams both ways (server-sent events included) and keeps
 * the Host the browser sent. A request outside the prefix never reaches
 * Monokulo: it's answered 404 and kept in `misses`, so a URL Monokulo gave
 * without its base path shows up. It listens before monokulo starts, whose
 * public_uri names its port; `target(url)` then says where monokulo is
 * (`http://127.0.0.1:port`), and until then a request is answered 503.
 */
function startPrefixProxy(prefix) {
  const misses = [];
  let upstream = null;
  const server = http.createServer((request, response) => {
    const url = request.url || '/';
    if (!upstream) {
      response.writeHead(503, { 'content-type': 'text/plain' });
      response.end('Monokulo is not up yet');
      return;
    }
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
    server.listen(0, '127.0.0.1', () => resolve({
      port: server.address().port,
      misses,
      target: (url) => { upstream = new URL(url); },
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
  const logFile = (name) => path.join(dir, `${name}.log`);
  const log = (name) => fs.openSync(logFile(name), 'a');
  const children = [];
  let proxy = null;

  try {
  const fake = spawn(BIN('fake-monerod'), ['--height', '1000'], { stdio: ['ignore', 'pipe', log('fake-monerod')] });
  children.push(fake);
  tee(fake.stdout, logFile('fake-monerod'));
  const fakeAddress = await readyLine(fake, fake.stdout, 'FAKE_MONEROD_READY ');

  const engineToken = crypto.randomBytes(32).toString('hex');
  // Each process's options file, in this run's directory: never the
  // user's own in ~/.config, which a spec saving on the admin page would
  // otherwise write.
  const engineOptions = path.join(dir, 'engine.toml');
  fs.writeFileSync(engineOptions, [
    '[server]',
    // Any free port: the engine says which (ENGINE_LISTENING).
    'bind = "127.0.0.1:0"',
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
    stdio: ['ignore', 'pipe', log('engine')],
  });
  children.push(engine);
  tee(engine.stdout, logFile('engine'));
  const engineAddress = await readyLine(engine, engine.stdout, 'ENGINE_LISTENING ', {
    timeout: 30_000,
    hint: process.env.E2E_ENGINE_BIN ? `; an E2E_ENGINE_BIN built before the engine printed it doesn't` : '',
  });
  const engineUrl = `http://${engineAddress}`;
  await waitFor(`${engineUrl}/status`, 'the engine', engine);

  proxy = options.prefix ? await startPrefixProxy(options.prefix) : null;
  const publicUri = proxy ? `http://127.0.0.1:${proxy.port}${options.prefix}` : null;
  const monokuloOptions = path.join(dir, 'monokulo.toml');
  fs.writeFileSync(monokuloOptions, [
    ...(publicUri ? [`public_uri = "${publicUri}"`] : []),
    '[server]',
    // Any free port: monokulo says which ("Open Monokulo: ...").
    'bind = "127.0.0.1:0"',
    // The engine is a process of its own here (engine-crash-recovery kills it), so
    // monokulo reaches it remotely, not inside itself.
    '[engine]',
    'mode = "remote"',
    `url = "${engineUrl}"`,
    // The engine's status is trusted for its floor, a second, not ten: a
    // spec that changes a node sees the status follow within one. The
    // default is pinned by a unit test in settings.rs.
    'status_cache_ms = 1000',
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
    stdio: ['ignore', log('monokulo'), 'pipe'],
  });
  children.push(monokulo);
  tee(monokulo.stderr, logFile('monokulo'));
  const monokuloUrl = new URL(await readyLine(monokulo, monokulo.stderr, 'Open Monokulo: ', { timeout: 30_000 })).origin;
  await waitFor(`${monokuloUrl}/`, 'monokulo', monokulo);
  if (proxy) proxy.target(monokuloUrl);

  var fixture = {
    monokulo_url: publicUri || monokuloUrl, direct_url: monokuloUrl, engine_url: engineUrl, fake_monerod: fakeAddress, logs: dir,
    proxy_misses: proxy ? proxy.misses : [],
  };
  } catch (e) {
    if (proxy) await proxy.close();
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
  const address = await readyLine(child, child.stdout, 'FAKE_MONEROD_READY ');
  child.stdout.resume();
  const stop = () => new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve();
    child.once('exit', () => resolve());
    child.kill('SIGTERM');
  });
  return { address, stop };
}

module.exports = { BIN, buildBinaries, prebuilt, readyLine, tee, startStack, startFakeNode };
