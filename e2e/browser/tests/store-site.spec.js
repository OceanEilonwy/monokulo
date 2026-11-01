// @ts-check
// A store's plugins and its deletion (docs/design/user-testing/
// store-site-integrations.html: A3, C1). A connected WooCommerce plugin lists
// under Integrations; disconnecting it waits while an order it made can be
// paid, then removes its webhook and changes the store's key. Deleting the
// store asks first, the store's name typed. Without JavaScript the dialogs
// are pages. Captures each for the gallery.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const GROUP = 'store-settings';
let fixture;
// One fixture, its store's plugin connected and disconnected in turn: in
// order, the open order last.
test.describe.configure({ mode: 'serial' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}
const settings = () => `${fixture.base_url}/stores/${fixture.connection_id}/settings`;
async function connectPlugin() {
  const response = await fetch(`${fixture.base_url}/__coverage/integration`, { method: 'POST' });
  expect(response.ok).toBe(true);
  return (await response.json()).integration_id;
}

test('a connected plugin is listed under Integrations; disconnecting it removes its webhook', async ({ page, context }) => {
  await login(context);
  await connectPlugin();
  // The plugin is on Integrations and its webhook on Webhooks; the General
  // tab has no website field.
  await page.goto(`${settings()}?tab=general`);
  await expect(page.locator('#card-store input[name="store_site"]')).toHaveCount(0);
  await page.goto(`${settings()}?tab=webhooks`);
  await expect(page.locator('#card-webhooks')).toContainText('https://shop.localhost/?wc-api=monokulo');
  await page.goto(`${settings()}?tab=integrations`);
  const connections = page.locator('#card-integrations');
  await expect(connections.locator('.int-meta')).toContainText('shop.localhost · plugin 0.4.0 · connected ');
  await expect(connections.locator('.int-meta')).toContainText('last order ');
  await captureCoverageStage(page, 'store-site-connected', test.info(), { group: GROUP });

  await page.locator('#card-integrations').getByRole('link', { name: 'Disconnect…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Disconnect WooCommerce from “shop.localhost”?' });
  await expect(dialog.locator('.checks li.ok')).toContainText('No order from shop.localhost can still be paid');
  await expect(dialog.locator('.checks')).toContainText("The plugin's webhook is removed");
  await expect(dialog.locator('.checks')).toContainText('https://shop.localhost/?wc-api=monokulo');
  await expect(dialog.getByLabel('Type “shop.localhost” to confirm')).toBeFocused();
  await captureCoverageStage(page, 'store-site-disconnect-ready', test.info(), { group: GROUP });
  await dialog.getByLabel('Type “shop.localhost” to confirm').fill('shop.localhost');
  await dialog.getByRole('button', { name: 'Disconnect WooCommerce' }).click();

  await expect(page.getByText('Plugin disconnected')).toBeVisible();
  const after = page.locator('#card-integrations');
  await expect(after.locator('h4')).toHaveText('Before');
  await expect(after).toContainText('No plugin is connected.');
  await page.goto(`${settings()}?tab=webhooks`);
  await expect(page.locator('#card-webhooks')).not.toContainText('https://shop.localhost/?wc-api=monokulo');
  await page.goto(`${settings()}?tab=general`);
  await expect(page.locator('#card-store input[name="store_site"]')).toHaveCount(0);
});

test('without JavaScript the dialogs are pages', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  const integration = await connectPlugin();
  await page.goto(`${settings()}/plugins/${integration}/disconnect`);
  await expect(page.getByRole('heading', { level: 1, name: 'Disconnect WooCommerce from “shop.localhost”?' })).toBeVisible();
  await captureCoverageStage(page, 'store-site-disconnect-page', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.getByLabel('Type “shop.localhost” to confirm').fill('shop.localhost');
  await page.getByRole('button', { name: 'Disconnect WooCommerce' }).click();
  await expect(page).toHaveURL(/\/settings\?saved=integrations/);
  // Deleting the store has its own page.
  await page.goto(`${settings()}/delete`);
  await expect(page.getByRole('heading', { level: 1, name: 'Delete “shop.localhost”?' })).toBeVisible();
  // The fixture has an order that can still be paid: deleting waits.
  await expect(page.locator('.checks li.no')).toContainText('No order can still be paid');
  await expect(page.getByRole('button', { name: 'Delete store' })).toBeDisabled();
  await captureCoverageStage(page, 'store-delete-page', test.info(), { group: GROUP, shapes: ['desktop'] });
  await context.close();
});

test('disconnecting waits while an order from the shop can be paid', async ({ page, context }) => {
  await login(context);
  await connectPlugin();
  await page.goto(`${settings()}?tab=integrations`);
  // An order from the shop can still be paid: the dialog waits.
  const order = await fetch(`${fixture.base_url}/__coverage/integration/order`, { method: 'POST' });
  expect(order.ok).toBe(true);
  await page.reload();
  await page.locator('#card-integrations').getByRole('link', { name: 'Disconnect…' }).click();
  const blocked = page.getByRole('dialog', { name: 'Disconnect WooCommerce from “shop.localhost”?' });
  await expect(blocked.locator('.checks li.no')).toContainText('No order from shop.localhost can still be paid');
  await expect(blocked.locator('.checks .fix')).toContainText('1 can be paid until about');
  await expect(blocked.getByRole('button', { name: 'Disconnect WooCommerce' })).toBeDisabled();
  await captureCoverageStage(page, 'store-site-disconnect-blocked', test.info(), { group: GROUP });
  await blocked.getByRole('link', { name: 'Close' }).click();
  await expect(blocked).toBeHidden();
});

