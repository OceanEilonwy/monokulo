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
let testerSession;
// One fixture, its wallets seeded once, read by every test.
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => {
  fixture = await startCoverageFixture();
  const seeded = await fetch(`${fixture.base_url}/__coverage/wallets`, { method: 'POST' });
  expect(seeded.ok).toBe(true);
  testerSession = (await seeded.json()).tester_session;
});
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context, session = fixture.session) {
  await context.addCookies([{ name: 'session', value: session, url: fixture.base_url }]);
}

test('mainnet wallets are the list and the test networks are folded below, shut', async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/account?tab=wallets');
  const head = page.locator('.net-section-head');
  await expect(head.locator('.tag-network.is-main')).toHaveText('Mainnet');
  await expect(head).toContainText('Real money · 2 wallets');
  // By name, the retired one left out.
  const main = page.locator('.wallets-table').first();
  await expect(main.locator('tbody tr td.card-title')).toHaveText(['Cake – shop takings', 'Savings']);

  const fold = page.locator('details.test-wallets');
  await expect(fold).not.toHaveAttribute('open', '');
  await expect(fold.locator('summary')).toContainText('Test networks');
  await expect(fold.locator('summary')).toContainText('3 wallets');
  await expect(fold.locator('summary')).toContainText('no real value');
  await captureCoverageStage(page, 'wallets-networks-mixed', test.info(), { group: GROUP });

  await fold.locator('summary').click();
  const rows = fold.locator('tbody tr');
  await expect(rows.locator('td.card-title')).toHaveText(['Feather test', 'POS trial', 'Lab']);
  await expect(rows.locator('.tag-network')).toHaveText(['Stagenet', 'Stagenet', 'Testnet']);
  await expect(page.locator('details.retired-wallets summary')).toContainText('Retired wallets (1)');
  await expect(page.locator('.btn-primary')).toHaveCount(1);
  await captureCoverageStage(page, 'wallets-networks-mixed-open', test.info(), { group: GROUP });
});

test('with only test wallets the fold starts open and says there is no mainnet wallet yet', async ({ browser }) => {
  // Without JavaScript: the fold is a plain <details>.
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context, testerSession);
  const page = await context.newPage();
  await page.goto(fixture.base_url + '/account?tab=wallets');
  await expect(page.getByText("No mainnet wallets yet. Add one when you're ready to take real payments.")).toBeVisible();
  await expect(page.locator('.net-section-head')).toHaveCount(0);
  const fold = page.locator('details.test-wallets');
  await expect(fold).toHaveAttribute('open', '');
  await expect(fold.locator('tbody tr')).toHaveCount(3);
  await fold.locator('summary').click();
  await expect(fold).not.toHaveAttribute('open', '');
  await fold.locator('summary').click();
  await captureCoverageStage(page, 'wallets-networks-test-only', test.info(), { group: GROUP });
  await context.close();
});

test('on a phone each wallet is a card', async ({ page, context }) => {
  await login(context);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(fixture.base_url + '/account?tab=wallets');
  const card = page.locator('.wallets-table tbody tr').first();
  await expect(page.locator('.wallets-table thead').first()).toBeHidden();
  await expect(card).toHaveCSS('display', 'grid');
  await expect(card.locator('td.card-status')).toHaveText('1 store');
  await expect(page.locator('.wallets-table tbody tr').nth(1).locator('td.card-status')).toHaveText('0 stores');
  await captureCoverageStage(page, 'wallets-networks-phone', test.info(), { group: GROUP, asIs: true });
});

test("a wallet's page shows its network beside its name", async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/account/wallets/w_feather');
  const title = page.locator('.wallet-title');
  await expect(title.getByRole('heading', { name: 'Feather test' })).toBeVisible();
  await expect(title.locator('.tag-network.is-test')).toHaveText('Stagenet');
  await expect(page.getByText('Brought in · test network, no real value')).toBeVisible();
  await captureCoverageStage(page, 'wallets-networks-wallet-page', test.info(), { group: GROUP });
});

test("the dashboard shows each store's wallet with its network", async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/');
  const row = page.locator('tr', { hasText: 'shop.localhost' }).first();
  await expect(row).toContainText('Cake – shop takings');
  await expect(row.locator('.tag-network.is-main')).toHaveText('Mainnet');
  await captureCoverageStage(page, 'wallets-networks-dashboard', test.info(), { group: GROUP });
});

test('pickers show each wallet with its network, and a store changes only within its own', async ({ page, context }) => {
  await login(context);
  // A new store picks from every network.
  await page.goto(fixture.base_url + '/dashboard/connect');
  const picker = page.getByRole('combobox', { name: 'Wallet' });
  await picker.click();
  const options = page.getByRole('option');
  await expect(options).toHaveCount(5);
  await expect(page.locator('.mk-option .tag-network')).toHaveText(['Mainnet', 'Mainnet', 'Stagenet', 'Stagenet', 'Testnet']);
  await captureCoverageStage(page, 'wallets-networks-new-store-picker', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.keyboard.press('Escape');

  // A store changes only to a wallet on its own network.
  await page.goto(fixture.base_url + `/dashboard/stores/${fixture.connection_id}/settings`);
  const section = page.locator('#wallet');
  await expect(section).toContainText('A store can only change to a wallet on its own network.');
  await section.getByRole('combobox', { name: 'Wallet' }).click();
  await expect(section.getByRole('option')).toHaveCount(2);
  await expect(section.locator('.mk-option .tag-network.is-main')).toHaveCount(2);
  await expect(section.getByRole('option', { name: /Feather test/ })).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-networks-change-picker', test.info(), { group: GROUP, shapes: ['desktop'] });
});
