const { test, expect, pauseClockAt } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

let fixture;
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

test('real challenge solves in JavaScript and continues to the checkout', async ({ page }) => {
  const response = await page.request.get(`${fixture.base_url}/__coverage/challenge`);
  expect(await response.text()).toContain('Checking your connection');
  await page.goto(`${fixture.base_url}/__coverage/challenge`);
  await expect(page.locator('#checkout-root')).toBeVisible();
  expect(page.url()).toContain('monokulo_proof=');
});

test('real challenge offers a ten second no-JavaScript continuation', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(`${fixture.base_url}/__coverage/challenge`);
    await expect(page.getByRole('heading', { name: 'Checking your connection' })).toBeVisible();
    await expect(page.locator('meta[http-equiv="refresh"]')).toHaveAttribute('content', /^10;url=/);
    await expect(page.getByRole('link', { name: 'continue' })).toBeVisible();
    await captureCoverageStage(page, 'challenge-no-js', test.info());
    await expect(page.locator('#checkout-root')).toBeVisible({ timeout: 20000 });
  } finally { await context.close(); }
});

test('real challenge continues inside a cross-site checkout frame with and without JavaScript', async ({ browser }) => {
  const shop = fixture.base_url.replace('127.0.0.1', 'localhost');
  for (const javaScriptEnabled of [true, false]) {
    const context = await browser.newContext({ javaScriptEnabled });
    try {
      if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
      const page = await context.newPage();
      await page.goto(`${shop}/__coverage/ready`);
      // Framed the way the embed library frames the checkout (420 x 900).
      await page.setContent(`<iframe id="payment" title="Payment" style="width:420px;height:900px;border:0" src="${fixture.base_url}/__coverage/challenge"></iframe>`);
      const frame = page.frameLocator('#payment');
      if (!javaScriptEnabled) {
        await expect(frame.getByRole('heading', { name: 'Checking your connection' })).toBeVisible();
        await expect(frame.getByRole('link', { name: 'continue' })).toBeVisible();
        await captureCoverageStage(page.locator('#payment'), 'challenge-cross-site', test.info());
        // A frame left at the browser's default 300 x 150 still shows the
        // state and the way on.
        await page.locator('#payment').evaluate(element => { element.style.width = '300px'; element.style.height = '150px'; });
        await expect(frame.getByRole('link', { name: 'continue' })).toBeInViewport();
        await captureCoverageStage(page.locator('#payment'), 'challenge-cross-site-small', test.info());
      }
      await expect(frame.locator('#checkout-root')).toBeVisible({ timeout: 20000 });
    } finally { await context.close(); }
  }
});

test('real challenge on a plain-HTTP onion (no Web Crypto) waits ten seconds and continues', async ({ page }) => {
  // A Tor .onion served over plain HTTP is not a secure context, so
  // crypto.subtle is missing and the proof cannot be computed in the page.
  await page.addInitScript(() => { Object.defineProperty(window.crypto, 'subtle', { get: () => undefined }); });
  // The page's time stands still from the start: only the test moves it.
  const start = new Date('2026-01-01T00:00:00Z');
  await pauseClockAt(page, start);
  // The page's own record of its wait: the timer it sets, and when it
  // fires (which is when it moves on). Added after the clock's own script,
  // so it wraps the fake setTimeout.
  await page.addInitScript(() => {
    window.waits = [];
    const setTimeout_ = window.setTimeout;
    window.setTimeout = function (callback, delay, ...args) {
      const wait = { delay, fired: false };
      window.waits.push(wait);
      return setTimeout_.call(this, (...callbackArgs) => { wait.fired = true; return callback(...callbackArgs); }, delay, ...args);
    };
  });
  const waits = () => page.evaluate(() => window.waits);
  await page.goto(`${fixture.base_url}/__coverage/challenge`);
  // Said as the wait is set: 10.5s (challenge.js), so the server's own ten
  // seconds have passed when the page continues. Not a millisecond before.
  await expect(page.locator('#challenge-progress')).toHaveText('This page continues in 10 seconds.');
  expect(await waits()).toEqual([{ delay: 10500, fired: false }]);
  await page.clock.runFor(10499);
  expect(await waits(), 'still waiting at 10.499s').toEqual([{ delay: 10500, fired: false }]);
  expect(page.url()).toContain('/__coverage/challenge');
  await page.clock.runFor(1);
  await expect(page.locator('#checkout-root')).toBeVisible();
  expect(page.url()).toContain('monokulo_wait=');
});
