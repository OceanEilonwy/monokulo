const base = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

async function installCoverageContext(context) {
  if (!process.env.COVERAGE_RAW_DIR) return;
  await context.addInitScript(() => {
    window.addEventListener('pagehide', () => {
      if (!window.__coverage__) return;
      try {
        const key = '__monokulo_coverage_snapshots__';
        const prior = JSON.parse(sessionStorage.getItem(key) || '[]');
        prior.push(window.__coverage__);
        sessionStorage.setItem(key, JSON.stringify(prior.slice(-20)));
      } catch (_) { /* A blocked storage API must not break the app. */ }
    });
  });
}

async function collectCoverageContext(context, testInfo) {
  if (!process.env.COVERAGE_RAW_DIR) return;
  const snapshots = [];
  for (const page of context.pages()) {
    for (const frame of page.frames()) {
      try {
        const captured = await frame.evaluate(() => {
          let previous = [];
          try { previous = JSON.parse(sessionStorage.getItem('__monokulo_coverage_snapshots__') || '[]'); }
          catch (_) { /* A browser may deny storage to a cross-site frame. */ }
          return [...previous, window.__coverage__ || {}];
        });
        snapshots.push(...captured.filter(Boolean));
      } catch (_) { /* A frame may detach during test teardown. */ }
    }
  }
  const directory = process.env.COVERAGE_RAW_DIR;
  fs.mkdirSync(directory, { recursive: true });
  const id = crypto.createHash('sha256').update(`${testInfo.testId}:${testInfo.retry}:${testInfo.workerIndex}`).digest('hex').slice(0, 24);
  const file = path.join(directory, `${id}.json`);
  const previous = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file)) : { snapshots: [] };
  fs.writeFileSync(file, JSON.stringify({
    title: testInfo.title, id: testInfo.testId, retry: testInfo.retry,
    status: testInfo.status || 'passed', snapshots: [...previous.snapshots, ...snapshots],
  }));
}

// Debug builds of monokulo embed Solid's development build, which reports
// reactivity mistakes (a read that will not update, a write from inside a
// computation, ...) to the console as `[CODE] message`. Any of those fails
// the test that produced it.
const SOLID_DIAGNOSTIC = /^\[[A-Z][A-Z_]+\] /;

const test = base.test.extend({
  context: async ({ context }, use, testInfo) => {
    const diagnostics = [];
    context.on('console', message => {
      if (['warning', 'error'].includes(message.type()) && SOLID_DIAGNOSTIC.test(message.text())) diagnostics.push(message.text());
    });
    await installCoverageContext(context);
    await use(context);
    await collectCoverageContext(context, testInfo);
    if (diagnostics.length) throw new Error(`Solid reported ${diagnostics.length} diagnostic(s):\n${diagnostics.join('\n')}`);
  },
});

module.exports = { ...base, test, installCoverageContext, collectCoverageContext };
