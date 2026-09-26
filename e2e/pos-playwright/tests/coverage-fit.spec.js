const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

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

for (const [name, width, height] of sizes) {
  for (const [orientation, w, h] of [['portrait', width, height], ['landscape', height, width]]) {
    test(`real POS fits ${name} ${orientation} (${w}x${h})`, async ({ page, context }) => {
      const fixture = await startCoverageFixture();
      try {
        if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
        await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
        await page.setViewportSize({ width: w, height: h });
        await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
        await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
        await page.getByRole('button', { name: 'Background order', exact: true }).click();
        await expect(page.locator('.pos-keypad')).toBeVisible();
        await assertNoOuterScroll(page, `${name} ${orientation} keypad`);
        for (const digit of ['1', '2', '3', '4', '5']) await page.getByRole('button', { name: digit, exact: true }).click();
        await page.locator('#pos-reference').fill('A Fairly Long Customer Name Here');
        await assertNoOuterScroll(page, `${name} ${orientation} filled`);
        await page.getByRole('button', { name: 'Charge' }).click();
        await expect(page.frameLocator('.pos-checkout-card iframe').locator('.qr-wrap svg')).toBeVisible();
        await assertNoOuterScroll(page, `${name} ${orientation} payment`);
      } finally { await stopCoverageFixture(fixture.process); }
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
