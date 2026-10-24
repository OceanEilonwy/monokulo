// @ts-check
// Wallets by network (issue #7): the Account page's Wallets tab lists
// mainnet wallets first and folds the test networks' away, open when
// there's no mainnet wallet; the same network badge shows on a wallet's
// page, the dashboard's stores and the wallet pickers; and a store only
// changes to a wallet on its own network. Works without JavaScript.
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

test('pickers show each wallet with its network, and a store changes only within its own', async ({ page, context }) => {
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

  // A store changes only to a wallet on its own network.
  await page.goto(fixture.base_url + `/stores/${fixture.connection_id}/settings?tab=wallet`);
  const section = page.locator('#card-wallet');
  await expect(section).toContainText('A store can only change to a wallet on its own network.');
  await section.getByRole('combobox', { name: 'Wallet' }).click();
  await expect(section.getByRole('option')).toHaveCount(2);
  await expect(section.locator('.mk-option .tag-network.is-main')).toHaveCount(2);
  await expect(section.getByRole('option', { name: /Feather test/ })).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-networks-change-picker', test.info(), { group: GROUP, shapes: ['desktop'] });
});
