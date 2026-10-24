const { spawn, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { BUILD_ARGS } = require('./real-stack');

const root = path.resolve(__dirname, '../..');
const binary = path.join(root, 'target/debug/examples/coverage_fixture');
const coverageAssets = path.join(root, 'target/coverage/browser/assets');

async function startCoverageFixture() {
  const build = spawnSync('cargo', [...BUILD_ARGS, '--offline'],
    { cwd: root, encoding: 'utf8' });
  if (build.status !== 0) throw new Error(`coverage fixture build failed:\n${build.stderr || build.error}`);
  const child = spawn(binary, [], { cwd: root, detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'] });
  let diagnostics = '';
  child.stderr.on('data', chunk => { diagnostics += chunk.toString(); });
  const info = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`coverage fixture did not start: ${diagnostics}`)), 15000);
    let buffer = '';
    child.stdout.on('data', chunk => {
      buffer += chunk.toString();
      const line = buffer.split('\n').find(line => line.startsWith('COVERAGE_FIXTURE='));
      if (line) {
        clearTimeout(timeout);
        try { resolve(JSON.parse(line.slice('COVERAGE_FIXTURE='.length))); }
        catch (error) { reject(error); }
      }
    });
    child.once('exit', code => { clearTimeout(timeout); reject(new Error(`coverage fixture exited ${code}: ${diagnostics}`)); });
  });
  const health = await fetch(`${info.base_url}/__coverage/ready`);
  if (!health.ok || await health.text() !== 'ready') {
    await stopCoverageFixture(child);
    throw new Error('coverage fixture health check failed');
  }
  return { ...info, process: child };
}

async function stopCoverageFixture(child) {
  if (!child || child.exitCode !== null) return;
  try {
    if (process.platform === 'win32') child.kill('SIGTERM');
    else process.kill(-child.pid, 'SIGTERM');
  } catch (error) {
    if (error.code !== 'ESRCH') throw error;
  }
  await new Promise(resolve => {
    const timeout = setTimeout(resolve, 5000);
    child.once('exit', () => { clearTimeout(timeout); resolve(); });
  });
}

module.exports = { startCoverageFixture, stopCoverageFixture };

async function serveInstrumentedAssets(context) {
  // A page whose requests Playwright intercepts (any route on them, even
  // one that only passes them on) is revealed outside the cross-document
  // view transition the page before it started (site.css's
  // `@view-transition`), and Chromium aborts that transition with an error
  // the page reports: "ViewTransition opt-in disabled". No visitor's
  // browser intercepts, so these pages opt out instead: an adopted sheet
  // comes after the page's own in the cascade.
  await context.addInitScript(() => {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync('@view-transition { navigation: none; }');
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  });
  await context.route('**/static/*', async route => {
    // A script's URL carries its digest (crate::assets); the file is named without it.
    const name = path.basename(new URL(route.request().url()).pathname).replace(/\.[0-9a-f]{16}(?=\.(js|css)$)/, '');
    // The instrumented scripts, and the chunks the coverage builds made
    // with them, named by that build's hashes. The islands aren't counted:
    // they come from the server, with the chunks its page asks for up
    // front (a chunk of the same name is the same file either way).
    if (name === 'ui-islands.js' || !/^[\w.-]+\.(js|css)$/.test(name) || !fs.existsSync(path.join(coverageAssets, name))) {
      return route.continue();
    }
    return route.fulfill({ path: path.join(coverageAssets, name),
      contentType: name.endsWith('.css') ? 'text/css' : 'text/javascript' });
  });
}

module.exports.serveInstrumentedAssets = serveInstrumentedAssets;
