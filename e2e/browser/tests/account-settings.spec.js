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

/** Waits for a navigation's view transition to end: with JavaScript off, a
 * click that lands mid-transition hangs (see backend-helpers.js). */
async function transitionDone(page) {
  await expect.poll(() => page.evaluate(() => !document.activeViewTransition)).toBe(true);
}

test('without JavaScript the account menu opens and the profile saves', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(fixture.base_url + '/');
  // The menu is a <details>: it opens with no script.
  await expect(page.locator('.acct-menu')).not.toBeVisible();
  await page.locator('.acct > summary').click();
  await expect(page.locator('.acct-menu')).toBeVisible();
  await expect(page.locator('.acct-menu')).toContainText(EMAIL);
  await page.locator('.acct-menu').getByRole('link', { name: /^Times in/ }).click();
  await expect(page).toHaveURL(/\/account#card-time$/);
  await transitionDone(page);

  // The save bar is always there without JavaScript.
  const save = page.locator('#save-bar').getByRole('button', { name: 'Save' });
  await page.locator('#timezone').selectOption('Australia/Perth');
  await page.locator('label.radio-card', { hasText: 'Dark' }).click();
  await save.click();
  await expect(page).toHaveURL(/\/account\?saved=appearance,time$/);
  await transitionDone(page);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(page.locator('.acct-menu')).toContainText('Times in Australia/Perth');
  await expect(page.locator('#card-time [data-card-saved]')).toHaveText('Saved');

  // A new email asks on a page of its own first.
  await page.locator('#email').fill('renamed@example.test');
  await save.click();
  await expect(page.getByRole('heading', { name: 'Log in with a new email?' })).toBeVisible();
  await transitionDone(page);
  await page.getByLabel('Type the new email again').fill('renamed@example.test');
  await page.getByRole('button', { name: 'Change email' }).click();
  await expect(page).toHaveURL(/\/account\?saved=email$/);
  await transitionDone(page);
  await expect(page.locator('#email')).toHaveValue('renamed@example.test');

  // Put it all back for the tests after this one.
  await page.locator('#email').fill(EMAIL);
  await page.locator('#timezone').selectOption('');
  await page.locator('label.radio-card', { hasText: 'System' }).click();
  await save.click();
  await expect(page.getByRole('heading', { name: 'Log in with a new email?' })).toBeVisible();
  await transitionDone(page);
  await page.getByLabel('Type the new email again').fill(EMAIL);
  await page.getByRole('button', { name: 'Change email' }).click();
  await expect(page).toHaveURL(/\/account\?saved=email,appearance,time$/);
  await expect(page.locator('#email')).toHaveValue(EMAIL);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', /./);
  await context.close();
});

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

test('the account menu closes on a click outside it, and its items join the phone menu', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/');
  await page.locator('.acct > summary').click();
  const menu = page.locator('.acct-menu');
  await expect(menu).toBeVisible();
  await expect(menu.getByRole('link', { name: /^Wallets/ })).toHaveAttribute('href', '/account?tab=wallets');
  await captureCoverageStage(page, 'account-menu', test.info(), { group: 'site' });
  await page.locator('h1').click();
  await expect(menu).not.toBeVisible();
  await page.locator('.acct > summary').click();
  await page.keyboard.press('Escape');
  await expect(menu).not.toBeVisible();

  // On a phone the items are rows of the hamburger list, each a 44px target.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator('.nav-toggle-label').click();
  for (const name of ['Account', /^Wallets/, 'Log out']) {
    const item = page.locator('.site-nav').getByRole(name === 'Log out' ? 'button' : 'link', { name });
    await expect(item).toBeVisible();
    expect((await item.boundingBox()).height).toBeGreaterThanOrEqual(44);
  }
  // The status dot is a 44px target too.
  const hit = await page.locator('#status-indicator').evaluate(el => {
    const before = getComputedStyle(el, '::before');
    const box = el.getBoundingClientRect();
    return { width: box.width - 2 * parseFloat(before.left), height: box.height - 2 * parseFloat(before.top) };
  });
  expect(hit.width).toBeGreaterThanOrEqual(44);
  expect(hit.height).toBeGreaterThanOrEqual(44);
});

test('the nav bar is one height signed in or out', async ({ browser }) => {
  const signedOut = await browser.newPage();
  await signedOut.goto(fixture.base_url + '/dashboard/login');
  const outHeight = (await signedOut.locator('.site-nav').boundingBox()).height;
  const context = await browser.newContext();
  await login(context);
  const signedIn = await context.newPage();
  await signedIn.goto(fixture.base_url + '/');
  const inHeight = (await signedIn.locator('.site-nav').boundingBox()).height;
  expect(inHeight).toBe(outHeight);
  await signedOut.close();
  await context.close();
});
