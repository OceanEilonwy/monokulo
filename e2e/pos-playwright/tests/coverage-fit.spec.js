const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { webkit } = require('playwright');

const sizes = [
  ['iPhone SE', 375, 667], ['iPhone 14', 390, 844], ['iPad', 768, 1024],
  ['iPad Pro', 1024, 1366], ['Pixel 7', 412, 915], ['Galaxy S8', 360, 740],
  ['Galaxy Tab', 800, 1280],
];

async function assertNoOuterScroll(page, stage) {
  const geometry = await page.evaluate(() => ({ width: document.documentElement.scrollWidth,
    height: document.documentElement.scrollHeight, innerWidth, innerHeight }));
  expect(geometry.width, `${stage}: horizontal overflow ${JSON.stringify(geometry)}`).toBeLessThanOrEqual(geometry.innerWidth + 1);
  expect(geometry.height, `${stage}: vertical overflow ${JSON.stringify(geometry)}`).toBeLessThanOrEqual(geometry.innerHeight + 1);
}

/** Loads the POS at `w`x`h`, then checks the keypad, a filled keypad and the
 * payment screen each fit with no outer scrolling. `captures` names the
 * screenshot checkpoints to take (Chromium only). */
async function checkFit(page, context, label, w, h, captures = {}) {
  const fixture = await startCoverageFixture();
  try {
    if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
    await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
    await page.setViewportSize({ width: w, height: h });
    await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
    await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
    await page.getByRole('button', { name: 'Background order', exact: true }).click();
    await expect(page.locator('.pos-keypad')).toBeVisible();
    await assertNoOuterScroll(page, `${label} keypad`);
    if (captures.keypad) await captureCoverageStage(page, captures.keypad, test.info());
    for (const digit of ['1', '2', '3', '4', '5']) await page.getByRole('button', { name: digit, exact: true }).click();
    await page.locator('#pos-reference').fill('A Fairly Long Customer Name Here');
    await assertNoOuterScroll(page, `${label} filled`);
    await page.getByRole('button', { name: 'Charge' }).click();
    await expect(page.frameLocator('.pos-checkout-card iframe').locator('.qr-wrap svg')).toBeVisible();
    await assertNoOuterScroll(page, `${label} payment`);
    if (captures.payment) await captureCoverageStage(page, captures.payment, test.info());
  } finally { await stopCoverageFixture(fixture.process); }
}

function orientations(width, height) {
  return [['portrait', width, height], ['landscape', height, width]];
}

for (const [name, width, height] of sizes) {
  for (const [orientation, w, h] of orientations(width, height)) {
    test(`real POS fits ${name} ${orientation} (${w}x${h})`, async ({ page, context }) => {
      const captures = name === 'iPhone SE' && orientation === 'landscape'
        ? { keypad: 'pos-narrow-keypad', payment: 'pos-narrow-payment' } : {};
      await checkFit(page, context, `${name} ${orientation}`, w, h, captures);
    });
  }
}

// The same sizes on WebKit, the engine iPhone and iPad Safari use, whose
// flex/grid sizing differs enough from Chromium's to check directly. WebKit
// needs its own system libraries (`npx playwright install-deps webkit`, as
// root); where it can't launch these are skipped, saying why, rather than
// failing. Its coverage isn't collected (the collector reads Chromium).
let webkitUnavailable;
async function webkitSkipReason() {
  if (webkitUnavailable !== undefined) return webkitUnavailable;
  try {
    const browser = await webkit.launch();
    await browser.close();
    webkitUnavailable = null;
  } catch (e) {
    webkitUnavailable = `WebKit can't launch here (${e.message.split('\n')[0]}); run "npx playwright install-deps webkit" as root to enable these`;
  }
  return webkitUnavailable;
}

for (const [name, width, height] of sizes) {
  for (const [orientation, w, h] of orientations(width, height)) {
    test(`real POS fits ${name} ${orientation} (${w}x${h}) on WebKit`, async () => {
      const reason = await webkitSkipReason();
      test.skip(reason !== null, reason || '');
      const browser = await webkit.launch();
      try {
        const context = await browser.newContext();
        await checkFit(await context.newPage(), context, `WebKit ${name} ${orientation}`, w, h);
      } finally { await browser.close(); }
    });
  }
}

test('real POS retains its fit after five viewport changes', async ({ page, context }) => {
  const fixture = await startCoverageFixture();
  try {
    if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
    await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
    await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
    await page.getByRole('button', { name: 'Background order', exact: true }).click();
    await expect(page.locator('.pos-keypad')).toBeVisible();
    for (const [width, height] of [[375, 667], [667, 375], [1024, 768], [360, 740], [740, 360]]) {
      await page.setViewportSize({ width, height });
      await assertNoOuterScroll(page, `resized ${width}x${height}`);
    }
  } finally { await stopCoverageFixture(fixture.process); }
});
