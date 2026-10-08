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

test('settings dialogs save, cancel, preserve focus and keep errors in the editor', async ({ page, context }) => {
  await login(context);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(fixture.base_url + store + '/settings');
  const section = page.locator('#base-currency');
  const edit = section.getByRole('button', { name: 'Edit base currency', exact: true });
  await expect(section.locator('dialog')).not.toBeVisible();
  await edit.click();
  await section.locator('select[name=base_currency]').selectOption('USD');
  await page.keyboard.press('Escape');
  await expect(edit).toBeFocused();
  await edit.click();
  expect(await section.locator('select').inputValue()).not.toBe('USD');
  await section.locator('select').selectOption('USD');
  await section.getByRole('button', { name: 'Update', exact: true }).click();
  await expect(section.locator('dialog')).not.toBeVisible();
  await expect(section).toContainText('Settings saved.');
  await expect(section.locator('.settings-summary')).toHaveText('USD');
  await expect(edit).toBeFocused();
  const confirmations = page.locator('#confirmation-thresholds');
  await confirmations.getByRole('button', { name: 'Edit confirmation thresholds' }).click();
  await confirmations.locator('[name=confirmations_required]').fill('721');
  await confirmations.locator('[name=confirmations_required]').press('Enter');
  await expect(confirmations.locator('dialog')).toBeVisible();
  await expect(confirmations.locator('.error')).toContainText('0 to 720');
  await page.keyboard.press('Escape');
  expect(errors).toEqual([]);
});
