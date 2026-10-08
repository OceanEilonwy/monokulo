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

test('order creation, Back and changed or unchanged resubmission work', async ({ page, context }) => {
  await login(context);
  await page.addInitScript(() => Object.defineProperty(crypto, "randomUUID", { value: undefined }));
  await page.goto(fixture.base_url + store + '/orders/new');
  await page.locator('#amount').fill('0.01');
  await page.locator('#currency').selectOption('XMR');
  const key = await page.locator('[name=request_key]').inputValue();
  await page.getByRole('button', { name: 'Create order', exact: true }).click();
  await expect(page).toHaveURL(/\/orders\/order_/);
  const first = page.url();
  await page.goBack();
  await expect(page.locator('[name=request_key]')).not.toHaveValue(key);
  await page.locator('#amount').fill('0.01');
  await page.locator('#currency').selectOption('XMR');
  await page.getByRole('button', { name: 'Create order', exact: true }).click();
  await expect(page).toHaveURL(/\/orders\/order_/);
  expect(page.url()).not.toBe(first);
  await page.goBack();
  await page.locator('#amount').fill('0.02');
  await page.locator('#currency').selectOption('XMR');
  await page.getByRole('button', { name: 'Create order', exact: true }).click();
  await expect(page).toHaveURL(/\/orders\/order_/);
});
