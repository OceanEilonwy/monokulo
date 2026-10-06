const { test, expect } = require('@playwright/test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
let fixture;
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
async function login(context, admin = false) {
  await context.addCookies([{ name: 'session', value: admin ? fixture.admin_session : fixture.session, url: fixture.base_url }]);
}
const store = '/dashboard/stores/coverage-store';

test('common setup saves a store default and can be skipped without JS', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(fixture.base_url + store + '/setup');
  await page.locator('[name=base_currency]').selectOption('AUD');
  await page.locator('[name=provider]').selectOption('coingecko');
  await page.locator('[name=confirmations]').fill('3');
  await page.getByRole('button', { name: 'Save and continue' }).click();
  await expect(page).toHaveURL(fixture.base_url + store);
  await page.goto(fixture.base_url + store + '/orders/new');
  await expect(page.locator('#currency')).toHaveValue('AUD');
  await page.goto(fixture.base_url + store + '/setup');
  await page.getByRole('button', { name: 'Skip for now' }).click();
  await expect(page).toHaveURL(fixture.base_url + store);
  await context.close();
});
