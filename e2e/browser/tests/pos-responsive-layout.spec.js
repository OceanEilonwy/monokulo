const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

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
 * screenshot checkpoints to take at this size (the gallery is Chromium's).
 * Each project's browser runs every size: WebKit, the engine of iPhone and
 * iPad Safari, sizes flex and grid differently enough to check directly. */
async function checkFit(page, context, label, w, h, captures = {}) {
  const fixture = await startCoverageFixture();
  try {
    if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
    await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
    await page.setViewportSize({ width: w, height: h });
    await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
    await expect(page.locator('.pos-pay-card')).toBeVisible();
    await page.getByRole('button', { name: 'Background order', exact: true }).click();
    await expect(page.locator('.pos-keypad')).toBeVisible();
    await assertNoOuterScroll(page, `${label} keypad`);
    if (captures.keypad) await captureCoverageStage(page, captures.keypad, test.info(), { asIs: true });
    for (const digit of ['1', '2', '3', '4', '5']) await page.getByRole('button', { name: digit, exact: true }).click();
    await page.locator('#pos-reference').fill('A Fairly Long Customer Name Here');
    await assertNoOuterScroll(page, `${label} filled`);
    await page.getByRole('button', { name: 'Charge' }).click();
    await expect(page.locator('.pos-pay-card .pos-qr svg')).toBeVisible();
    await assertNoOuterScroll(page, `${label} payment`);
    // On its side, what the counter needs is all on screen without scrolling.
    if (w > h) {
      for (const needed of ['.pos-pay-xmr', '.pos-qr', '.pos-address', '.pos-order-heading h1']) {
        await expect(page.locator(needed), `${label}: ${needed} on screen`).toBeInViewport({ ratio: 1 });
      }
      await expect(page.getByRole('button', { name: 'Background order', exact: true }), `${label}: Background order on screen`).toBeInViewport({ ratio: 1 });
      await expect(page.getByRole('button', { name: 'Cancel order' }), `${label}: Cancel order on screen`).toBeInViewport({ ratio: 1 });
    }
    if (captures.payment) await captureCoverageStage(page, captures.payment, test.info(), { asIs: true });
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
