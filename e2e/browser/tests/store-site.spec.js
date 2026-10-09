// @ts-check
// A store's website and its plugins (docs/design/user-testing/
// store-site-integrations.html: A3, B1, C1). A connected WooCommerce plugin
// locks the website and lists in Connections; disconnecting it waits while
// an order it made can be paid, then removes its webhook and changes the
// store's key, and the website unlocks; changing the website then asks
// first, the store's name typed. Without JavaScript the dialogs are pages.
// Captures each for the gallery.
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
const settings = () => `${fixture.base_url}/dashboard/stores/${fixture.connection_id}/settings`;
async function connectPlugin() {
  const response = await fetch(`${fixture.base_url}/__coverage/integration`, { method: 'POST' });
  expect(response.ok).toBe(true);
  return (await response.json()).integration_id;
}

test('a connected plugin locks the website; disconnecting it removes its webhook and unlocks the website, which then changes behind its own dialog', async ({ page, context }) => {
  await login(context);
  await connectPlugin();
  await page.goto(settings());
  const store = page.locator('#card-store');
  await expect(store.locator('.locked-input')).toHaveText('shop.localhost');
  await expect(store.locator('.lock-note')).toHaveText('Set by WooCommerce · see Connections');
  await expect(store.locator('input[name="store_site"]')).toHaveCount(0);
  const connections = page.locator('#card-connections');
  await expect(connections.locator('.int-meta')).toContainText('shop.localhost · plugin 0.4.0 · connected ');
  await expect(connections.locator('.int-meta')).toContainText('last order ');
  await captureCoverageStage(page, 'store-site-connected', test.info(), { group: GROUP });

  await expect(page.locator('#card-webhooks')).toContainText('https://shop.localhost/?wc-api=monokulo');
  await page.locator('#card-connections').getByRole('link', { name: 'Disconnect…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Disconnect WooCommerce from “shop.localhost”?' });
  await expect(dialog.locator('.checks li.ok')).toContainText('No order from shop.localhost can still be paid');
  await expect(dialog.locator('.checks')).toContainText("The plugin's webhook is removed");
  await expect(dialog.locator('.checks')).toContainText('https://shop.localhost/?wc-api=monokulo');
  await expect(dialog.getByLabel('Type “shop.localhost” to confirm')).toBeFocused();
  await captureCoverageStage(page, 'store-site-disconnect-ready', test.info(), { group: GROUP });
  await dialog.getByLabel('Type “shop.localhost” to confirm').fill('shop.localhost');
  await dialog.getByRole('button', { name: 'Disconnect WooCommerce' }).click();

  await expect(page.getByText('Plugin disconnected')).toBeVisible();
  await expect(page.locator('#card-webhooks')).not.toContainText('https://shop.localhost/?wc-api=monokulo');
  const after = page.locator('#card-connections');
  await expect(after.locator('h4')).toHaveText('Before');
  await expect(after).toContainText('No plugin is connected.');
  const site = page.locator('#card-store input[name="store_site"]');
  await expect(site).toHaveValue('shop.localhost');

  // Changing the website through the save: it asks first, with the new
  // address; the store's name typed.
  await site.fill('new-shop.localhost');
  await page.locator('#save-bar').getByRole('button', { name: 'Save', exact: true }).click();
  const change = page.getByRole('dialog', { name: 'Change the website of “shop.localhost”?' });
  await expect(change.locator('input[name="site"]')).toHaveValue('new-shop.localhost');
  await expect(change.locator('.checks')).toContainText('Checkouts embedded on shop.localhost stop loading');
  await captureCoverageStage(page, 'store-site-change-ready', test.info(), { group: GROUP });
  // Cancel leaves the website as it was (the change itself: http tests).
  await change.getByRole('link', { name: 'Cancel' }).click();
  await expect(change).toBeHidden();
  await page.reload();
  await expect(page.locator('#card-store input[name="store_site"]')).toHaveValue('shop.localhost');
});

test('without JavaScript the dialogs are pages', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  const integration = await connectPlugin();
  // The website, while the plugin is connected: blocked.
  await page.goto(`${settings()}/website?site=elsewhere.localhost`);
  await expect(page.getByRole('heading', { level: 1, name: 'Change the website of “shop.localhost”?' })).toBeVisible();
  await expect(page.locator('.checks li.no')).toContainText('No plugin is connected to it');
  await expect(page.getByRole('button', { name: 'Change website' })).toBeDisabled();
  await captureCoverageStage(page, 'store-site-change-blocked-page', test.info(), { group: GROUP });
  await page.getByRole('link', { name: 'disconnect it first' }).click();
  await expect(page).toHaveURL(`${settings()}/connections/${integration}/disconnect`);
  await expect(page.getByRole('heading', { level: 1, name: 'Disconnect WooCommerce from “shop.localhost”?' })).toBeVisible();
  await captureCoverageStage(page, 'store-site-disconnect-page', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.getByLabel('Type “shop.localhost” to confirm').fill('shop.localhost');
  await page.getByRole('button', { name: 'Disconnect WooCommerce' }).click();
  await expect(page).toHaveURL(/\/settings\?saved=connections/);
  // Emptying the website and saving goes to the remove page.
  await page.locator('#card-store input[name="store_site"]').fill('');
  await page.locator('#save-bar').getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(`${settings()}/website?remove=1`);
  await expect(page.getByRole('heading', { level: 1, name: 'Remove the website of “shop.localhost”?' })).toBeVisible();
  await expect(page.locator('input[name="site"]')).toHaveCount(0);
  await context.close();
});

test('on a phone the Connections card stacks its button', async ({ page, context }) => {
  await login(context);
  await connectPlugin();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(settings() + '#card-store');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  const row = page.locator('#card-connections .int-rows li').first();
  const meta = await row.locator('.int-meta').boundingBox();
  const button = await row.getByRole('link', { name: 'Disconnect…' }).boundingBox();
  expect(button.y).toBeGreaterThan(meta.y + meta.height - 1);
  await page.locator('#card-connections').scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-site-phone', test.info(), { group: GROUP, asIs: true });
});

test('disconnecting waits while an order from the shop can be paid', async ({ page, context }) => {
  await login(context);
  await connectPlugin();
  await page.goto(settings());
  // An order from the shop can still be paid: the dialog waits.
  const order = await fetch(`${fixture.base_url}/__coverage/integration/order`, { method: 'POST' });
  expect(order.ok).toBe(true);
  await page.reload();
  await page.locator('#card-connections').getByRole('link', { name: 'Disconnect…' }).click();
  const blocked = page.getByRole('dialog', { name: 'Disconnect WooCommerce from “shop.localhost”?' });
  await expect(blocked.locator('.checks li.no')).toContainText('No order from shop.localhost can still be paid');
  await expect(blocked.locator('.checks .fix')).toContainText('1 can be paid until about');
  await expect(blocked.getByRole('button', { name: 'Disconnect WooCommerce' })).toBeDisabled();
  await captureCoverageStage(page, 'store-site-disconnect-blocked', test.info(), { group: GROUP });
  await blocked.getByRole('link', { name: 'Close' }).click();
  await expect(blocked).toBeHidden();
});

