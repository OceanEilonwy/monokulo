const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { createInstrumenter } = require('istanbul-lib-instrument');

const root = path.resolve(__dirname, '../..');
const output = path.join(root, 'target/coverage/browser/assets');
// Fresh each run: the chunks' names change with their content.
fs.rmSync(output, { recursive: true, force: true });
fs.mkdirSync(output, { recursive: true });
const initial = {};
for (const name of ['checkout', 'challenge', 'monokulo-client']) {
  const source = `crates/monokulo/static/${name}.js`;
  const instrumenter = createInstrumenter({ esModules: false, compact: false });
  const code = instrumenter.instrumentSync(fs.readFileSync(path.join(root, source), 'utf8'), source);
  fs.writeFileSync(path.join(output, `${name}.js`), code);
  initial[source] = instrumenter.lastFileCoverage();
}
fs.writeFileSync(path.join(output, 'initial.json'), JSON.stringify(initial));

// The Vite-built scripts, each instrumented in its own build: the POS and
// the server-rendered pages' scripts (the islands and the engine page).
const vite = path.join(root, 'node_modules/vite/bin/vite.js');
if (!fs.existsSync(vite)) throw new Error(`missing prerequisite: ${vite}`);
for (const [app, bundle, counted] of [
  ['pos', 'pos-app.js', 'src/main.tsx'],
  ['engine', 'engine-view.js', 'src/events.tsx'],
]) {
  const build = spawnSync(process.execPath, [vite, 'build', '--config', 'coverage.vite.config.mjs'],
    { cwd: path.join(root, 'apps', app), stdio: 'inherit' });
  if (build.status !== 0) process.exit(build.status || 1);
  if (!fs.readFileSync(path.join(output, bundle), 'utf8').includes(`apps/${app}/${counted}`)) {
    throw new Error(`${app} coverage bundle did not contain a ${counted} coverage counter`);
  }
}
console.log(`Prepared instrumented browser assets in ${output}`);
