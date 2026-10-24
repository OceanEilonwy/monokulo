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
  await expect(open).not.toHaveAttribute('data-variant');
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
