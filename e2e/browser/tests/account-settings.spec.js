// The Account page (crates/monokulo/src/views/account.rs): its cards and
// save bar as on the admin settings page, the email question (a dialog with
// JavaScript, a page of its own without), and the account menu that leads
// there. Every save works without JavaScript too.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
let fixture;
// This file's tests share one fixture and the settings they save, so
// they run in order on one worker.
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}
const EMAIL = 'coverage@example.test';

test('with JavaScript a change shows the save bar, and a new email is confirmed in a dialog', async ({ page, context }) => {
  await login(context);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(fixture.base_url + '/account');
  const bar = page.locator('#save-bar');
  await expect(bar).not.toBeVisible();
  await captureCoverageStage(page, 'account-profile', test.info(), { group: 'site' });

  // A change marks its card and shows the bar; Discard puts it back.
  await page.locator('label.radio-card', { hasText: 'Light' }).click();
  await expect(page.locator('#card-appearance')).toHaveClass(/is-dirty/);
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('1 unsaved change in Appearance');
  await page.locator('#card-appearance').getByRole('button', { name: 'Discard' }).click();
  await expect(bar).not.toBeVisible();
  await expect(page.locator('input[name="theme"][value="system"]')).toBeChecked();

  // Save with a new email opens the question; a different address is refused.
  await page.locator('#email').fill('dialog@example.test');
  await bar.getByRole('button', { name: 'Save' }).click();
  const dialog = page.getByRole('dialog', { name: 'Log in with a new email?' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('From now on you log in with dialog@example.test instead of ' + EMAIL + '.');
  await captureCoverageStage(page, 'account-email-confirm', test.info(), { group: 'site' });
  await dialog.getByLabel('Type the new email again').fill('dialog@example.tset');
  await dialog.getByRole('button', { name: 'Change email' }).click();
  await expect(dialog).toContainText("That isn't the same email.");
  await expect(dialog).toBeVisible();

  // Cancel keeps the change unsaved; Save asks again.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.locator('#card-email')).toHaveClass(/is-dirty/);
  await bar.getByRole('button', { name: 'Save' }).click();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel('Type the new email again')).toHaveValue('');
  await dialog.getByLabel('Type the new email again').fill('dialog@example.test');
  await dialog.getByRole('button', { name: 'Change email' }).click();
  await expect(page).toHaveURL(/\/account\?saved=email$/);
  await expect(page.locator('.toast')).toContainText('You log in with dialog@example.test from now on.');
  await expect(page.locator('.acct-menu')).toContainText('dialog@example.test');

  // And back.
  await page.locator('#email').fill(EMAIL);
  await bar.getByRole('button', { name: 'Save' }).click();
  await dialog.getByLabel('Type the new email again').fill(EMAIL);
  await dialog.getByLabel('Type the new email again').press('Enter');
  await expect(page.locator('#email')).toHaveValue(EMAIL);
  expect(errors).toEqual([]);
});
