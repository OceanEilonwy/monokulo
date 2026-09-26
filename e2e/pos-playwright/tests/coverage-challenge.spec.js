const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

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
    await page.getByRole('link', { name: 'continue' }).click();
    await expect(page.locator('#checkout-root')).toBeVisible();
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
      await page.setContent(`<iframe id="payment" title="Payment" src="${fixture.base_url}/__coverage/challenge"></iframe>`);
      const frame = page.frameLocator('#payment');
      if (!javaScriptEnabled) {
        await expect(frame.getByRole('heading', { name: 'Checking your connection' })).toBeVisible();
        await frame.getByRole('link', { name: 'continue' }).click();
      }
      await expect(frame.locator('#checkout-root')).toBeVisible();
    } finally { await context.close(); }
  }
});
