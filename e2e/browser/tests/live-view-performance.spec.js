const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
let fixture;
// This file's tests share one fixture and the settings they save, so
// they run in order on one worker.
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
async function login(context, admin = false) {
  await context.addCookies([{ name: 'session', value: admin ? fixture.admin_session : fixture.session, url: fixture.base_url }]);
}
const store = '/dashboard/stores/coverage-store';

test('live view remains responsive and bounded through sustained activity and stream recovery', async ({ page, context }) => {
  await login(context, true);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    const Original = window.EventSource;
    window.EventSource = class extends Original {
      constructor(url, options) {
        super(url, options);
        if (String(url).includes('/status/engine/events')) {
          window.engineSource = this;
          this.addEventListener('history', e => { window.engineHistory = JSON.parse(e.data); });
        }
      }
    };
  });
  await page.goto(fixture.base_url + '/status/engine?network=mainnet');
  await expect(page.locator('#tl-mode')).toHaveValue('live');
  await expect(page.locator('#engine-network')).toHaveValue('mainnet');
  await expect(page.locator('.reload')).toBeHidden();
  await page.waitForFunction(() => window.engineHistory);
  const cdp = await context.newCDPSession(page);
  await cdp.send('HeapProfiler.collectGarbage');
  const before = await cdp.send('Runtime.getHeapUsage');
  // A full 30-minute history, followed by another two full histories of
  // distinct events: exercises count pruning and repeated canvas rendering.
  const timings = [];
  for (let cycle = 0; cycle < 3; cycle++) {
    timings.push(await page.evaluate(cycle => {
      const history = window.engineHistory;
      const now = Date.now();
      const marks = Array.from({ length: 50000 }, (_, index) => ({ seq: 100000 + cycle * 50000 + index,
        at_ms: now - 1700000 + index * 30, round: index, tier: 'blocks', key: index % 5000 === 0,
        text: `Synthetic block scan activity, frame ${cycle}` }));
      const frame = { ...history.frame, at_ms: now - 2000, marks, effects: [] };
      const started = performance.now();
      window.engineSource.dispatchEvent(new MessageEvent('frame', { data: JSON.stringify(frame) }));
      return performance.now() - started;
    }, cycle));
    // Played on the page's next draw: its newest events are this frame's.
    await expect(page.locator('#engine-events tr').first()).toContainText(`frame ${cycle}`);
    await page.locator('#tl-mode').selectOption('paused');
    await expect(page.locator('#tl-mode')).toHaveValue('paused');
    await page.locator('#tl-mode').selectOption('live');
  }
  await cdp.send('HeapProfiler.collectGarbage');
  const after = await cdp.send('Runtime.getHeapUsage');
  expect(after.usedSize - before.usedSize).toBeLessThan(45 * 1024 * 1024);
  expect(Math.max(...timings)).toBeLessThan(1500);
  await expect(page.locator('#engine-events tr')).toHaveCount(60);
  await page.evaluate(() => window.engineSource.dispatchEvent(new Event('error')));
  await expect(page.locator('.reload')).toBeVisible();
  await page.evaluate(() => window.engineSource.dispatchEvent(new MessageEvent('history', { data: JSON.stringify(window.engineHistory) })));
  await expect(page.locator('.reload')).toBeHidden();
  await page.evaluate(() => {
    window.dispatchEvent(new PageTransitionEvent('pagehide'));
    window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
  });
  await expect(page.locator('.reload')).toBeHidden();
  await page.locator('#tl-mode').selectOption('paused');
  await expect(page.locator('#tl-mode')).toHaveValue('paused');
  await page.locator('#tl-mode').selectOption('live');
  console.log('Live view stress metrics:',  JSON.stringify({ ingestMs: timings, retainedHeapBytes: after.usedSize - before.usedSize }));
  expect(errors).toEqual([]);
});
