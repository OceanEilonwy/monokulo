// The engine page's events table when the tab was hidden. A hidden page
// keeps only its newest frame (apps/engine/src/engine-view.tsx), so a mark can come in a
// frame that is never played: the fixture's reorg story ends with
// "Rewound", then round 4's frames, which carry no marks. Shown again, the
// page plays only the last of those, and the table still has to list the
// reorg's last mark (#125, issue #126). This file has the story to itself:
// its fixture engine is its own, so live-view.spec.js's tests see theirs
// untouched.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

let fixture;
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture && fixture.process); });

test('a tab hidden while the reorg plays lists its last mark when shown again', async ({ page, context }) => {
  // The story takes about ten seconds.
  test.setTimeout(60_000);
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  // Notes the history, and counts the frames that arrive after the one
  // carrying "Rewound".
  await page.addInitScript(() => {
    window.framesAfterRewound = -1;
    const Original = window.EventSource;
    window.EventSource = class extends Original {
      constructor(url, options) {
        super(url, options);
        this.addEventListener('history', () => { window.engineHistory = true; });
        this.addEventListener('frame', (e) => {
          const frame = JSON.parse(e.data);
          if (frame.marks.some((mark) => mark.what.kind === 'rewound')) window.framesAfterRewound = 0;
          else if (window.framesAfterRewound >= 0) window.framesAfterRewound++;
        });
      }
    };
  });
  await context.addCookies([{ name: 'session', value: fixture.admin_session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/status/engine?network=mainnet`);
  await expect(page.locator('#tl-mode')).toHaveValue('live');
  const events = page.locator('#engine-events');
  await page.waitForFunction(() => window.engineHistory);

  // Hidden, as far as the page can tell.
  const setHidden = (hidden) => page.evaluate((hidden) => {
    window.tabHidden = hidden;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => window.tabHidden });
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => (window.tabHidden ? 'hidden' : 'visible') });
    document.dispatchEvent(new Event('visibilitychange'));
  }, hidden);
  await setHidden(true);
  await page.request.post(`${fixture.base_url}/__coverage/engine/story`);
  await page.waitForFunction(() => window.framesAfterRewound >= 1, null, { timeout: 30_000 });
  await expect(events).not.toContainText('Rewound');

  await setHidden(false);
  await expect(events).toContainText('Rewound: block 3,412,881 deleted', { timeout: 5_000 });
  await expect(events.locator('tbody tr').first()).toContainText('Rewound: block 3,412,881 deleted');
  expect(errors).toEqual([]);
});
