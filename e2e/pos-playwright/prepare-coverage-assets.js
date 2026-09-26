const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { createInstrumenter } = require('istanbul-lib-instrument');

const root = path.resolve(__dirname, '../..');
const output = path.join(root, 'target/coverage/browser/assets');
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

const posDir = path.join(root, 'crates/monokulo/pos-ui');
const vite = path.join(posDir, 'node_modules/vite/bin/vite.js');
if (!fs.existsSync(vite)) throw new Error(`missing prerequisite: ${vite}`);
const build = spawnSync(process.execPath, [vite, 'build', '--config', 'coverage.vite.config.mjs'],
  { cwd: posDir, stdio: 'inherit' });
if (build.status !== 0) process.exit(build.status || 1);
if (!fs.readFileSync(path.join(output, 'pos-app.js'), 'utf8').includes('crates/monokulo/pos-ui/src/main.tsx')) {
  throw new Error('POS coverage bundle did not contain a main.tsx coverage counter');
}
console.log(`Prepared instrumented browser assets in ${output}`);
