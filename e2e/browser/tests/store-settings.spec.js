// A store's settings page (crates/monokulo/src/views/store_settings.rs): its
// settings are cards in one form with one save bar (the settings components,
// crates/ui); the wallet, the keys, the domains (domains.spec.js) and the
// webhooks are cards of their own beside it. Every save works without
// JavaScript too.
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
const store = '/dashboard/stores/coverage-store';

/** Records the settings components' events, across page loads. */
async function recordEvents(page) {
  await page.addInitScript(() => {
    window.__mk = [];
    for (const name of ['mk-dirty', 'mk-saved', 'mk-failed']) {
      document.addEventListener(name, (event) => window.__mk.push({ name, detail: event.detail }));
    }
  });
}
const events = (page, name) => page.evaluate((name) => window.__mk.filter((e) => e.name === name).map((e) => e.detail), name);

test('a change marks its card and shows the save bar, and Discard puts it back', async ({ page, context }) => {
  await login(context);
  await recordEvents(page);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(fixture.base_url + store + '/settings');
  const bar = page.locator('#save-bar');
  const card = page.locator('#card-confirmation-thresholds');
  const field = card.locator('input[name="confirmations_required"]');
  const saved = await field.inputValue();
  await expect(bar).toBeHidden();
  // No dialog open: every setting is in its card; only the website's
  // confirm dialogs wait, closed.
  await expect(page.locator('dialog[open]')).toHaveCount(0);
  await expect(page.locator('dialog:not(.confirm-dialog)')).toHaveCount(0);
  await captureCoverageStage(page, 'store-settings', test.info());

  await field.fill('7');
  await expect(card).toHaveClass(/is-dirty/);
  await expect(card.locator('[data-unsaved]')).toHaveText('1 unsaved');
  await expect(card.locator('.setting-field.is-changed .changed-mark').first()).toBeVisible();
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('1 unsaved change in Confirmation thresholds');
  expect((await events(page, 'mk-dirty')).at(-1)).toEqual({ count: 1, groups: ['confirmation-thresholds'] });
  await captureCoverageStage(page, 'store-settings-dirty', test.info());

  // A second card's change is named too.
  await page.locator('#card-diagnostics label.switch').click();
  await expect(bar).toContainText('2 unsaved changes in Confirmation thresholds and Diagnostics');

  await card.getByRole('button', { name: 'Discard' }).click();
  await expect(field).toHaveValue(saved);
  await expect(card).not.toHaveClass(/is-dirty/);
  await expect(bar).toContainText('1 unsaved change in Diagnostics');
  await bar.getByRole('link', { name: 'Discard changes' }).click();
  await expect(bar).toBeHidden();
  await expect(page.locator('#card-diagnostics input[role="switch"]')).not.toBeChecked();
  expect((await events(page, 'mk-dirty')).at(-1)).toEqual({ count: 0, groups: [] });
  expect(errors).toEqual([]);
});

test('a save says so on the card and in a toast; a refused one keeps what was typed', async ({ page, context }) => {
  await login(context);
  await recordEvents(page);
  await page.goto(fixture.base_url + store + '/settings');
  const bar = page.locator('#save-bar');
  const card = page.locator('#card-confirmation-thresholds');
  const field = () => page.locator('#card-confirmation-thresholds input[name="confirmations_required"]');
  const saved = await field().inputValue();

  // Refused: nothing saved, the card says why, what was typed stays.
  await field().fill('721');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(card).toHaveClass(/is-failed/);
  await expect(card.locator('.card-body > p.error')).toContainText('from 0 to 720');
  await expect(field()).toHaveValue('721');
  await expect(bar).toBeVisible();
  await expect(bar).toHaveClass(/is-failed/);
  await expect(bar).toContainText('Nothing saved.');
  await expect(page.locator('#settings-toasts .toast-error')).toContainText('from 0 to 720');
  expect(await events(page, 'mk-failed')).toEqual([{ group: 'confirmation-thresholds', message: 'Enter a whole number of confirmations from 0 to 720.' }]);
  await captureCoverageStage(page, 'store-settings-failed', test.info());

  // Discard puts back what's saved, and the refusal goes with it.
  await card.getByRole('button', { name: 'Discard' }).click();
  await expect(field()).toHaveValue(saved);
  await expect(card).not.toHaveClass(/is-failed/);
  await expect(bar).toBeHidden();

  // Saved: the card says so, and a toast.
  await field().fill('4');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Settings saved');
  await expect(card.locator('[data-card-saved]')).toHaveText('Saved');
  await expect(field()).toHaveValue('4');
  await expect(bar).toBeHidden();
  expect(await events(page, 'mk-saved')).toEqual([{ groups: ['confirmation-thresholds'] }]);
  await captureCoverageStage(page, 'store-settings-saved', test.info());

  // Back as it was.
  await field().fill(saved);
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
});

// A save swaps the page it answers with in where the page is scrolled
// (Save::Page), without loading it: the card being edited stays where it
// was on screen, saved or refused.
test('a save keeps the card being edited where it was on screen, without reloading the page', async ({ page, context }) => {
  await login(context);
  await page.setViewportSize({ width: 1280, height: 600 });
  await page.goto(fixture.base_url + store + '/settings');
  await page.evaluate(() => { window.__samePage = true; });
  const bar = page.locator('#save-bar');
  const at = (card) => card.evaluate((el) => Math.round(el.getBoundingClientRect().top));
  const scrollTo = (card) => card.evaluate((el) => window.scrollTo({ top: el.getBoundingClientRect().top + window.scrollY - 200, behavior: 'instant' }));

  const diagnostics = page.locator('#card-diagnostics');
  await scrollTo(diagnostics);
  const before = await at(diagnostics);
  expect(await page.evaluate(() => window.scrollY)).toBeGreaterThan(0);
  for (const _ of ['on', 'off']) {
    await diagnostics.locator('label.switch').click();
    // The last save's toast goes first, so this one's is waited for.
    await page.locator('#settings-toasts .toast').evaluateAll((all) => all.forEach((toast) => toast.remove()));
    await bar.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(page).toHaveURL(/\?saved=diagnostics$/);
    await expect(page.locator('#settings-toasts .toast-success')).toBeVisible();
    expect(await at(diagnostics)).toBe(before);
  }

  // Refused: the page refusing it comes in the same way, at the same address.
  const thresholds = page.locator('#card-confirmation-thresholds');
  await scrollTo(thresholds);
  const refusedAt = await at(thresholds);
  const url = page.url();
  await thresholds.locator('input[name="confirmations_required"]').fill('721');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(thresholds).toHaveClass(/is-failed/);
  expect(page.url()).toBe(url);
  expect(await at(thresholds)).toBe(refusedAt);
  await thresholds.getByRole('button', { name: 'Discard' }).click();

  expect(await page.evaluate(() => window.__samePage)).toBe(true);
});

test('without JavaScript the save bar is always there and saves, refused or not', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(fixture.base_url + store + '/settings');
  const bar = page.locator('#save-bar');
  await expect(bar).toBeVisible();
  const field = page.locator('#card-confirmation-thresholds input[name="confirmations_required"]');
  const saved = await field.inputValue();

  await field.fill('abc');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#card-confirmation-thresholds .card-body > p.error')).toContainText('Enter a whole number');
  await expect(field).toHaveValue('abc');
  await expect(page.locator('#settings-toasts .toast-error')).toBeVisible();

  await field.fill('5');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  await expect(page.locator('#card-confirmation-thresholds [data-card-saved]')).toHaveText('Saved');
  await expect(field).toHaveValue('5');
  await captureCoverageStage(page, 'store-settings-no-js', test.info());

  await field.fill(saved);
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/\?saved=confirmation-thresholds$/);
  await context.close();
});

