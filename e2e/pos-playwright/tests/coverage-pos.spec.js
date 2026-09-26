const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

let fixture;
test.beforeEach(async ({ context }) => {
  fixture = await startCoverageFixture();
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
});
test.afterEach(async () => { await stopCoverageFixture(fixture?.process); });

function posUrl() { return `${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`; }

test('real POS backgrounds, reloads, reopens, cancels, and searches an order', async ({ page }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toContainText('Fixture order');
  await page.reload();
  await expect(page.locator('.pos-stack-card')).toBeVisible();
  await page.locator('.pos-stack-card').click();
  await expect(page.frameLocator('.pos-checkout-card iframe').locator('#checkout-root')).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Cancelled');
  await expect(page.locator('.pos-checkout-card iframe')).toBeHidden();
  await page.getByRole('button', { name: 'New order' }).click();
  await page.getByRole('button', { name: 'View all →' }).click();
  await page.getByRole('tab', { name: /Finished/ }).click();
  await expect(page.locator('.pos-order-card .pos-badge')).toContainText('Cancelled');
  await page.getByRole('searchbox', { name: 'Search reference or order ID' }).fill('fixture');
  await expect(page.locator('.pos-order-card')).toContainText('Fixture order');
});

test('real POS header remains above its compact checkout frame', async ({ page }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
  for (const { width, height } of [{ width: 1126, height: 700 }, { width: 667, height: 375 }, { width: 360, height: 740 }]) {
    await page.setViewportSize({ width, height });
    const bounds = await page.evaluate(() => {
      const bar = document.querySelector('.pos-top').getBoundingClientRect();
      const panel = document.querySelector('.pos-checkout-card').getBoundingClientRect();
      const frame = document.querySelector('.pos-checkout-card iframe').getBoundingClientRect();
      return { barBottom: bar.bottom, panelTop: panel.top, frameTop: frame.top,
        frameBottom: frame.bottom, viewportHeight: innerHeight };
    });
    expect(bounds.panelTop).toBeGreaterThanOrEqual(bounds.barBottom);
    expect(bounds.frameTop).toBeGreaterThanOrEqual(bounds.barBottom);
    if (height >= 620) expect(bounds.frameBottom).toBeLessThanOrEqual(bounds.viewportHeight);
    else {
      await page.getByRole('button', { name: 'Cancel order' }).scrollIntoViewIfNeeded();
      await expect(page.getByRole('button', { name: 'Cancel order' })).toBeInViewport();
    }
  }
});
