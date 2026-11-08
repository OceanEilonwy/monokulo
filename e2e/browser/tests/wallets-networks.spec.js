// @ts-check
// Wallets by network (issue #7): the Account page's Wallets tab lists
// mainnet wallets first and folds the test networks' away, open when
// there's no mainnet wallet; the same network badge shows on a wallet's
// page, the dashboard's stores and the wallet pickers; and a store can
// change to a wallet on any network, warned first about a test network's.
// Works without JavaScript.
// Captures each page for the gallery.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const GROUP = 'wallets';
let fixture;
// One fixture, its wallets seeded once, read by every test.
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

test('pickers show each wallet with its network, and a test network is warned about', async ({ page, context }) => {
  await login(context);
  // A new store picks from every network (setup's wallet step).
  await page.goto(fixture.base_url + '/setup/wallet?store_name=New&store_site=new.example');
  const picker = page.locator('.already').getByRole('combobox');
  await picker.click();
  const options = page.getByRole('option');
  await expect(options).toHaveCount(5);
  await expect(page.locator('.mk-option .tag-network')).toHaveText(['Mainnet', 'Mainnet', 'Stagenet', 'Stagenet', 'Testnet']);
  await captureCoverageStage(page, 'wallets-networks-new-store-picker', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.keyboard.press('Escape');

  // A store changes to a wallet on any network.
  await page.goto(fixture.base_url + `/stores/${fixture.connection_id}/settings?tab=wallet`);
  const section = page.locator('#card-wallet');
  await expect(section).not.toContainText('its own network');
  await section.getByRole('combobox', { name: 'Wallet' }).click();
  await expect(section.getByRole('option')).toHaveCount(5);
  await expect(section.locator('.mk-option .tag-network.is-main')).toHaveCount(2);
  await expect(section.getByRole('option', { name: /Feather test/ })).toHaveCount(1);
  await captureCoverageStage(page, 'wallets-networks-change-picker', test.info(), { group: GROUP, shapes: ['desktop'] });

  // Picking a stagenet wallet asks first, with a warning; nothing changes yet.
  await section.getByRole('option', { name: /Feather test/ }).click();
  await expect(section.getByRole('heading', { name: 'Change to Feather test?' })).toBeVisible();
  const warning = section.getByText('Feather test is a stagenet wallet. Its coins have no value');
  await expect(warning).toBeVisible();
  await captureCoverageStage(page, 'wallets-networks-change-test-network-warning', test.info(), { group: GROUP });

  // A mainnet wallet asks without one.
  await section.getByRole('combobox', { name: 'Wallet' }).click();
  await section.getByRole('option', { name: /Savings/ }).click();
  await expect(section.getByRole('heading', { name: 'Change to Savings?' })).toBeVisible();
  await expect(section.getByText('have no value')).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-networks-change-mainnet', test.info(), { group: GROUP, shapes: ['desktop'] });
});
