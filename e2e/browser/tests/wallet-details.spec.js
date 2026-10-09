// @ts-check
// A wallet's page (crates/monokulo/src/views/wallets.rs, detail_page): its
// name is a Details card with the settings components (views::settings,
// static/settings-form.js), saved with the page's save bar: marked when
// changed, put back with Discard, renamed with a toast, a refused name kept
// to fix. Without JavaScript the save bar is always there.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const GROUP = 'wallets';
let fixture;
// One fixture, its wallets seeded once; the tests run in order.
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => {
  fixture = await startCoverageFixture();
  const seeded = await fetch(`${fixture.base_url}/__coverage/wallets`, { method: 'POST' });
  expect(seeded.ok).toBe(true);
});
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}

/** Opens the wallet called `name` from the Account page's Wallets tab. */
async function openWallet(page, name) {
  await page.goto(fixture.base_url + '/account?tab=wallets');
  await page.getByRole('link', { name, exact: true }).click();
  await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible();
}

test('the Details card renames the wallet: marked, discarded, refused, saved', async ({ page, context }) => {
  await login(context);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await openWallet(page, 'Savings');
  const card = page.locator('#card-details');
  const name = card.getByLabel('Name');
  const bar = page.locator('#save-bar');
  await expect(name).toHaveValue('Savings');
  await expect(bar).toBeHidden();
  await captureCoverageStage(page, 'wallet-details', test.info(), { group: GROUP });

  await name.fill('Savings jar');
  await expect(card).toHaveClass(/is-dirty/);
  await expect(card.locator('.changed-mark')).toBeVisible();
  await expect(bar).toContainText('1 unsaved change in Details');
  await captureCoverageStage(page, 'wallet-details-dirty', test.info(), { group: GROUP });
  await card.getByRole('button', { name: 'Discard' }).click();
  await expect(name).toHaveValue('Savings');
  await expect(bar).toBeHidden();

  // Another wallet's name: refused, what was typed stays to fix.
  await name.fill('Cake – shop takings');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(card).toHaveClass(/is-failed/);
  await expect(card.locator('.card-body > p.error')).toContainText('You already have a wallet called');
  await expect(name).toHaveValue('Cake – shop takings');
  await expect(bar).toHaveClass(/is-failed/);
  await expect(page.locator('#settings-toasts .toast-error')).toContainText('Not renamed');
  await captureCoverageStage(page, 'wallet-details-failed', test.info(), { group: GROUP });

  await name.fill('Savings jar');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Savings jar', level: 1 })).toBeVisible();
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Renamed');
  await expect(card.locator('[data-card-saved]')).toHaveText('Saved');
  await expect(bar).toBeHidden();

  // Back as it was.
  await name.fill('Savings');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Savings', level: 1 })).toBeVisible();
  expect(errors).toEqual([]);
});

test('without JavaScript the wallet is renamed with the save bar, which is always there', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await openWallet(page, 'Savings');
  const bar = page.locator('#save-bar');
  await expect(bar).toBeVisible();
  await page.locator('#card-details').getByLabel('Name').fill('Savings box');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Savings box', level: 1 })).toBeVisible();
  await expect(page.locator('#card-details [data-card-saved]')).toHaveText('Saved');
  await page.locator('#card-details').getByLabel('Name').fill('Savings');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Savings', level: 1 })).toBeVisible();
  await context.close();
});
