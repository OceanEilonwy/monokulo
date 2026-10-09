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

test('refresh icon and settings fallback remain usable without JS', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(fixture.base_url + '/status');
  const refresh = page.getByRole('link', { name: 'Refresh page' });
  await expect(refresh).toBeVisible();
  const bounds = await refresh.boundingBox(); expect(bounds.width).toBeLessThan(50);
  await page.goto(fixture.base_url + store + '/settings');
  await expect(page.locator('#card-base-currency select')).toBeVisible();
  await context.close();
});
