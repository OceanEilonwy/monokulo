const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

let fixture;

test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

test('real checkout renders full and compact views from the controlled engine', async ({ page }) => {
  const checkout = `${fixture.base_url}/pay/${fixture.public_key}/orders/${fixture.order_id}`;
  await page.goto(checkout);
  await expect(page.locator('#checkout-root')).toBeVisible();
  await expect(page.locator('#address')).toHaveValue(/^8/);
  await expect(page.locator('#refund-form')).toBeVisible();
  await page.goto(`${checkout}?view=compact`);
  await expect(page.locator('#checkout-root')).toBeVisible();
  await expect(page.locator('.checkout-compact')).toBeVisible();
  await expect(page.locator('.qr-wrap svg')).toBeVisible();
  if (process.env.COVERAGE_INSTRUMENT === '1') {
    const file = await page.evaluate(() => window.__coverage__?.['crates/monokulo/static/checkout.js']);
    expect(Object.keys(file?.branchMap || {}).length).toBeGreaterThan(0);
  }
});

test('real POS opens its compact checkout iframe', async ({ page, context }) => {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
  await expect(page.locator('#pos-root')).toBeVisible();
  await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
  await expect(page.frameLocator('.pos-checkout-card iframe').locator('.qr-wrap svg')).toBeVisible();
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await page.getByRole('button', { name: '1', exact: true }).click();
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
  await expect(page.frameLocator('.pos-checkout-card iframe').locator('.qr-wrap svg')).toBeVisible();
  if (process.env.COVERAGE_INSTRUMENT === '1') {
    const file = await page.evaluate(() => window.__coverage__?.['crates/monokulo/pos-ui/src/main.tsx']);
    expect(Object.keys(file?.branchMap || {}).length).toBeGreaterThan(0);
  }
});
