const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const coverage = require('istanbul-lib-coverage');
const reports = require('istanbul-reports');
const report = require('istanbul-lib-report');
const sourceMaps = require('istanbul-lib-source-maps');

const root = path.resolve(__dirname, '../..');
const directory = path.join(root, 'target/coverage/browser');
const rawDir = path.join(directory, 'raw');
const initial = JSON.parse(fs.readFileSync(path.join(directory, 'assets/initial.json')));
const map = coverage.createCoverageMap(initial);
const records = fs.readdirSync(rawDir).filter(name => name.endsWith('.json'));
if (!records.length) throw new Error('browser coverage has no Playwright frame snapshots');
for (const name of records) {
  const record = JSON.parse(fs.readFileSync(path.join(rawDir, name)));
  if (record.status !== 'passed') throw new Error(`browser test failed: ${record.title}`);
  for (const snapshot of record.snapshots) map.merge(snapshot);
}

async function main() {
  const gallery = path.join(root, 'target/coverage/screenshots');
  const entries = JSON.parse(fs.readFileSync(path.join(gallery, 'manifest.json')));
  if (entries.length < 10) throw new Error(`browser screenshot manifest has only ${entries.length} stages`);
  const imageSet = new Set();
  const groups = new Set();
  for (const entry of entries) {
    if (!['checkout', 'pos', 'challenge'].includes(entry.group)) throw new Error(`invalid screenshot group ${entry.group}`);
    if (!/^images\/[a-z0-9-]+\.png$/.test(entry.image)) throw new Error(`invalid screenshot path ${entry.image}`);
    if (imageSet.has(entry.image)) throw new Error(`duplicate screenshot path ${entry.image}`);
    imageSet.add(entry.image);
    if (!fs.statSync(path.join(gallery, entry.image)).size) throw new Error(`empty screenshot ${entry.image}`);
    if (entry.stage !== 'failure') groups.add(entry.group);
  }
  if (!['checkout', 'pos', 'challenge'].every(group => groups.has(group))) {
    throw new Error('browser screenshots lack a required checkout, POS, or challenge stage');
  }
  if (!fs.existsSync(path.join(gallery, 'index.html'))) throw new Error('browser screenshot gallery is missing');
  const finalMap = await sourceMaps.createSourceMapStore().transformCoverage(map);
  const required = [
    'crates/monokulo/static/checkout.js',
    'crates/monokulo/static/challenge.js',
    'crates/monokulo/static/monokulo-client.js',
    'crates/monokulo/pos-ui/src/main.tsx',
  ];
  for (const name of required) {
    const file = finalMap.files().find(file => file.endsWith(name));
    if (!file) throw new Error(`browser coverage is missing required source ${name}`);
    const summary = finalMap.fileCoverageFor(file).toSummary();
    if (!summary.lines.total || !summary.branches.total || !summary.lines.covered || !summary.branches.covered) {
      throw new Error(`browser coverage has no executed lines or branches for ${name}`);
    }
  }
  for (const file of finalMap.files()) {
    if (file.endsWith('pos-app.js') || file.endsWith('jsQR.js') || file.includes('/tests/')) {
      throw new Error(`generated, vendor, or test source entered browser coverage: ${file}`);
    }
  }
  if (finalMap.files().length !== required.length) {
    throw new Error(`unexpected browser source set: ${finalMap.files().join(', ')}`);
  }
  const context = report.createContext({ dir: directory, coverageMap: finalMap, defaultSummarizer: 'pkg' });
  reports.create('html').execute(context);
  reports.create('lcovonly', { file: 'lcov.info' }).execute(context);
  reports.create('json', { file: 'coverage-final.json' }).execute(context);
  const summary = finalMap.getCoverageSummary();
  const version = (command, args) => execFileSync(command, args, { cwd: root, encoding: 'utf8' }).trim();
  const manifest = {
    component: 'browser', revision: version('git', ['rev-parse', 'HEAD']),
    source_dirty: version('git', ['status', '--porcelain']).length > 0,
    tools: {
      rustc: version('rustc', ['--version']), cargo: version('cargo', ['--version']),
      collector: `istanbul-lib-instrument ${require('istanbul-lib-instrument/package.json').version}`,
      playwright: require('@playwright/test/package.json').version,
      node: process.version,
    },
    test: { status: 'passed', command: 'playwright test -c coverage-browser.config.js', exit_code: 0, log: 'browser/test.log' },
    lines: { covered: summary.lines.covered, total: summary.lines.total },
    branches: { covered: summary.branches.covered, total: summary.branches.total },
    report: 'browser/index.html', unavailable: [],
  };
  fs.writeFileSync(path.join(root, 'target/coverage/browser.json'), JSON.stringify(manifest, null, 2));
  console.log(`Browser lines ${summary.lines.covered}/${summary.lines.total}; branches ${summary.branches.covered}/${summary.branches.total}`);
}

main().catch(error => { console.error(error); process.exitCode = 1; });
