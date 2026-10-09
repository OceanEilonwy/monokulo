// @ts-check
// A wallet's page (issues #21 and #22): one column, the name and its
// network badge, where the wallet lives, its stores, details and history,
// and Retire at the bottom opening the checklist dialog. Blocked while a
// store takes payments into it, ready once nothing does: the name typed,
// the wallet retired, then restored with its keys from the dialog at the
// bottom. Without JavaScript the dialogs are pages. Also the setup flow's
// network dropdown, each network as its badge. Captures each for the
// gallery.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const GROUP = 'wallets';
let fixture;
let savingsKeys;
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => {
  fixture = await startCoverageFixture();
  const seeded = await fetch(`${fixture.base_url}/__coverage/wallets`, { method: 'POST' });
  expect(seeded.ok).toBe(true);
  savingsKeys = (await seeded.json()).savings_keys;
});
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}

const page_of = (id) => `${fixture.base_url}/account/wallets/${id}`;

test("a wallet's page is one column: where it lives, stores, details, history, then Retire", async ({ page, context }) => {
  await login(context);
  await page.goto(page_of('w_cake'));
  const title = page.locator('.wallet-title');
  await expect(title.getByRole('heading', { name: 'Cake – shop takings' })).toBeVisible();
  await expect(title.locator('.tag-network.is-main')).toHaveText('Mainnet');
  await expect(page.locator('body')).not.toContainText(/real money/i);

  const banner = page.locator('.where-banner');
  await expect(banner.locator('strong')).toHaveText('Brought in from Cake Wallet');
  await expect(banner.locator('img.app-logo')).toBeVisible();

  const stores = page.locator('section[aria-labelledby="stores-title"]');
  await expect(stores.locator('.card-meta')).toHaveText('1 takes payments into this wallet');
  const row = stores.locator('.store-rows li').first();
  await expect(row.getByRole('link', { name: 'shop.localhost' })).toBeVisible();
  await expect(row.getByRole('link', { name: 'change the wallet' })).toHaveAttribute('href', '/dashboard/stores/coverage-store/settings#wallet');
  // Flush: no list indent.
  await expect(stores.locator('.store-rows')).toHaveCSS('padding-left', '0px');

  const details = page.locator('#card-details');
  await expect(details.locator('dl.facts dt')).toHaveText(['Address', 'Kind']);
  await expect(details.locator('.short-value')).toBeVisible();
  const history = page.locator('details.history-fold');
  await expect(history).not.toHaveAttribute('open', '');
  await expect(history.locator('summary')).toContainText('History');

  const foot = page.locator('.wallet-foot');
  await expect(foot).toContainText('Retire this wallet');
  await expect(foot.getByRole('link', { name: 'Retire wallet…' })).toHaveClass(/btn-danger/);
  // Nothing beside the column, and nothing overflows it.
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await captureCoverageStage(page, 'wallets-page', test.info(), { group: GROUP });
});

test('where it lives says how each wallet was backed up or which app it is in', async ({ page, context }) => {
  await login(context);
  for (const [id, title, warn] of [
    ['w_savings', 'Backed up on paper', false],
    ['w_pos', 'Backup skipped', true],
    ['w_feather', 'Brought in from Feather', false],
    ['w_lab', 'Brought in · app not recorded', false],
  ]) {
    await page.goto(page_of(id));
    const banner = page.locator('.where-banner');
    await expect(banner.locator('strong')).toHaveText(title);
    if (warn) await expect(banner).toHaveClass(/is-warn/);
    else await expect(banner).not.toHaveClass(/is-warn/);
    await captureCoverageStage(page, `wallets-page-where-${id.slice(2)}`, test.info(), { group: GROUP, shapes: ['desktop'] });
  }
});

test('the retire dialog is blocked while a store takes payments into the wallet and an order can be paid', async ({ page, context }) => {
  await login(context);
  await page.goto(page_of('w_cake'));
  await page.getByRole('link', { name: 'Retire wallet…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Retire “Cake – shop takings”?' });
  await expect(dialog).toBeVisible();
  await expect(page).toHaveURL(page_of('w_cake'));
  const checks = dialog.locator('.checks li');
  await expect(checks.nth(0)).toHaveClass('no');
  await expect(checks.nth(0)).toContainText('No store takes payments into it');
  await expect(checks.nth(0).locator('.fix')).toContainText('shop.localhost does · change its wallet');
  // The fixture store's open order, on this wallet.
  await expect(checks.nth(1)).toHaveClass('no');
  await expect(checks.nth(1).locator('.fix')).toContainText('until about');
  await expect(dialog).toContainText('Retire becomes available when both are ticked.');
  await expect(dialog.getByRole('button', { name: 'Retire wallet' })).toBeDisabled();
  await expect(dialog.locator('input[name="confirm"]')).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-retire-blocked', test.info(), { group: GROUP });
  await dialog.getByRole('link', { name: 'Close' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole('link', { name: 'Retire wallet…' })).toBeFocused();

  // A wallet the engine can't answer for: one row says so.
  await page.goto(page_of('w_lab'));
  await page.getByRole('link', { name: 'Retire wallet…' }).click();
  const unknown = page.getByRole('dialog', { name: 'Retire “Lab”?' });
  await expect(unknown.locator('.checks li')).toHaveCount(1);
  await expect(unknown).toContainText("Monokulo can't check right now");
  await expect(unknown.getByRole('link', { name: 'try again' })).toBeVisible();
  await expect(unknown.getByRole('button', { name: 'Retire wallet' })).toBeDisabled();
  await captureCoverageStage(page, 'wallets-retire-unknown', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.keyboard.press('Escape');
  await expect(unknown).toBeHidden();
});

test('a wallet nothing uses is retired by typing its name, then restored with its keys', async ({ page, context }) => {
  await login(context);
  await page.goto(page_of('w_savings'));
  await page.getByRole('link', { name: 'Retire wallet…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Retire “Savings”?' });
  await expect(dialog.locator('.checks li.ok')).toHaveCount(2);
  const confirm = dialog.getByLabel('Type “Savings” to confirm');
  await expect(confirm).toBeFocused();
  // The label sits one --space-sm step above its field.
  const label = await dialog.locator('.setting-label-row').boundingBox();
  const field = await confirm.boundingBox();
  const step = await page.evaluate(() => {
    const probe = document.createElement('div');
    probe.style.width = 'var(--space-sm)';
    document.body.append(probe);
    const width = probe.getBoundingClientRect().width;
    probe.remove();
    return width;
  });
  expect(Math.round(field.y - (label.y + label.height))).toBe(Math.round(step));
  await captureCoverageStage(page, 'wallets-retire-ready', test.info(), { group: GROUP });

  // The wrong name: the retire page again, saying why.
  await confirm.fill('savings');
  await dialog.getByRole('button', { name: 'Retire wallet' }).click();
  await expect(page.getByRole('alert')).toHaveText('Type “Savings” exactly to retire it.');
  await page.getByLabel('Type “Savings” to confirm').fill('Savings');
  await page.getByRole('button', { name: 'Retire wallet' }).click();

  await expect(page).toHaveURL(page_of('w_savings'));
  const meta = page.locator('.wallet-meta');
  await expect(meta.locator('.tag')).toHaveText('retired');
  await expect(meta).toContainText('Keys deleted');
  await expect(meta).toContainText('History kept.');
  const foot = page.locator('.wallet-foot');
  await expect(foot).toContainText('Restore this wallet');
  const open = foot.getByRole('link', { name: 'Restore wallet…' });
  await expect(open).not.toHaveClass(/btn-danger|btn-primary/);
  await expect(page.getByRole('link', { name: 'Retire wallet…' })).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-retired-page', test.info(), { group: GROUP });

  await open.click();
  const restore = page.getByRole('dialog', { name: 'Restore “Savings”' });
  await expect(restore).toBeVisible();
  await captureCoverageStage(page, 'wallets-restore-dialog', test.info(), { group: GROUP });
  await restore.locator('input[name="view_key_hex"]').fill(savingsKeys.view_key_hex);
  await restore.locator('input[name="spend_pubkey_hex"]').fill(savingsKeys.spend_pubkey_hex);
  await restore.getByRole('button', { name: 'Restore wallet' }).click();
  await expect(page.getByText('Savings is back. Stores can use it again.')).toBeVisible();
  await expect(page.locator('.wallet-meta')).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Retire wallet…' })).toBeVisible();
});

test('without JavaScript the dialogs are pages', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(page_of('w_cake'));
  await expect(page.locator('#retire-dialog')).toBeHidden();
  await page.getByRole('link', { name: 'Retire wallet…' }).click();
  await expect(page).toHaveURL(`${page_of('w_cake')}/retire`);
  await expect(page.getByRole('heading', { level: 1, name: 'Retire “Cake – shop takings”?' })).toBeVisible();
  await expect(page.locator('.checks li.no').first()).toContainText('shop.localhost does');
  await expect(page.getByRole('button', { name: 'Retire wallet' })).toBeDisabled();
  await captureCoverageStage(page, 'wallets-retire-page', test.info(), { group: GROUP });
  await page.getByRole('link', { name: 'Close' }).click();
  await expect(page).toHaveURL(page_of('w_cake'));

  // The retired wallet's restore form, on its own page.
  await page.goto(page_of('w_old'));
  // Without JavaScript the save bar is always shown, fixed at the bottom;
  // the page scrolls far enough to bring the row out from under it.
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await page.getByRole('link', { name: 'Restore wallet…' }).click();
  await expect(page).toHaveURL(`${page_of('w_old')}/restore`);
  await expect(page.getByRole('heading', { level: 1, name: 'Restore “Old till”' })).toBeVisible();
  await expect(page.locator('input[name="view_key_hex"]')).toBeVisible();
  await captureCoverageStage(page, 'wallets-restore-page', test.info(), { group: GROUP, shapes: ['desktop'] });
  await context.close();
});

test("on a phone a wallet's page stays one column", async ({ page, context }) => {
  await login(context);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(page_of('w_cake'));
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  const button = page.getByRole('link', { name: 'Retire wallet…' });
  const text = await page.locator('.wallet-foot strong').boundingBox();
  const box = await button.boundingBox();
  expect(box.y).toBeGreaterThan(text.y + text.height);
  await captureCoverageStage(page, 'wallets-page-phone', test.info(), { group: GROUP, asIs: true });
});

test("the setup flow's network dropdown shows each network as its badge", async ({ page, context }) => {
  await login(context);
  await page.goto(fixture.base_url + '/account/wallets/add');
  await page.locator('details.more-options summary').click();
  const network = page.getByRole('combobox', { name: 'Network' });
  await expect(network.locator('.tag-network.is-main')).toHaveText('Mainnet');
  await network.click();
  await expect(page.locator('.mk-option .tag-network')).toHaveText(['Mainnet', 'Stagenet', 'Testnet']);
  await expect(page.locator('.mk-option .mk-label')).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-setup-network', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.getByRole('option', { name: 'Stagenet' }).click();
  await expect(page.locator('select[name="network"]')).toHaveValue('stagenet');

  // "Which app is it in?" on Bring your own wallet: each app with its logo.
  await page.goto(fixture.base_url + '/account/wallets/import?name=Market+till&network=mainnet');
  const app = page.getByRole('combobox', { name: 'Which app is it in? (optional)' });
  await app.click();
  await expect(page.locator('.mk-option img.mk-logo')).toHaveCount(4);
  await expect(page.getByRole('option', { name: 'Other' })).toBeVisible();
  await captureCoverageStage(page, 'wallets-import-app', test.info(), { group: GROUP, shapes: ['desktop'] });
  await page.getByRole('option', { name: 'Feather' }).click();
  await expect(page.locator('select[name="app"]')).toHaveValue('feather');
});
