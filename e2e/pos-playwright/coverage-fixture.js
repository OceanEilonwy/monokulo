const { spawn, spawnSync } = require('node:child_process');
const path = require('node:path');

const root = path.resolve(__dirname, '../..');
const binary = path.join(root, 'target/debug/examples/coverage_fixture');

async function startCoverageFixture() {
  const build = spawnSync('cargo', ['build', '--offline', '--locked', '-p', 'monokulo', '--example', 'coverage_fixture'],
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
