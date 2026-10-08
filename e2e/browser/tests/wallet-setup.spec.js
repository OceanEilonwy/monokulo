// @ts-check
// Wallets (docs/wallets.md) against the real binaries: a merchant makes a new
// wallet in the browser (the wallet-setup WebAssembly module), saves its
// phrase, proves it by typing three words, and has it registered; without
// JavaScript, creating one is shown unavailable and says why, while bringing
// one's own still works; skipping the backup takes the warning, the tick and
// typing "skip". Then stores on wallets: the custom store form picks one, and
// a wallet's page lists its stores. Captures each page for the gallery.
const { test, expect } = require('../coverage-test');
const { captureCoverageStage } = require('../coverage-screenshot');
const {
  useRealStack, fixture, signInAsAdmin, fakeNodeAddress, saveNodes, addWallet, createStore, finishStoreSetup, WALLET_NAME,
} = require('./backend-helpers');

useRealStack(test);

const GROUP = 'wallets';

/** The open "Create a new wallet" page's words, from the written-down list. */
async function wordsOnThePage(page) {
  return page.locator('[data-words] li').allTextContents();
}

/** Starts making a stagenet wallet called `name` from the setup page. */
async function startNewWallet(page, name) {
  const { monokulo_url: base } = fixture();
  await page.goto(base + '/dashboard/wallets/setup');
  await page.locator('input[name="name"]').fill(name);
  await page.getByText('More options').click();
  await page.locator('select[name="network"]').selectOption('stagenet');
  const create = page.getByRole('button', { name: 'Create a new wallet' });
  await expect(create).toBeEnabled();
  await create.click();
  await expect(page.getByRole('heading', { name: `Back up ${name}` })).toBeVisible();
}

test('a new wallet is made in the browser, saved, checked with three of its words and registered', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();

  await page.goto(base + '/dashboard/wallets/setup');
  // With JavaScript, creating one is offered.
  await expect(page.locator('#create-needs-js')).toBeHidden();
  await expect(page.getByText('Coming soon')).toBeVisible();
  await captureCoverageStage(page, 'wallets-choice', test.info(), { group: GROUP });

  await startNewWallet(page, 'Copper Heron');
  // The phrase never goes into a form field.
  await expect(page.locator('form[data-register] input[name="phrase"]')).toHaveCount(0);

  // Saving it in a wallet app: the QR code shows only when asked.
  const cake = page.locator('[data-app-panel="cake"]');
  // One app at a time, and one of Show and Hide.
  await expect(page.locator('[data-app-panel="stack"]')).toBeHidden();
  await expect(page.locator('[data-method-panel="paper"]')).toBeHidden();
  await expect(page.getByRole('heading', { name: 'Check your backup' })).toBeHidden();
  await expect(cake.getByRole('button', { name: 'Hide QR code' })).toBeHidden();
  await expect(cake.locator('[data-qr] svg')).toHaveCount(0);
  await cake.getByRole('button', { name: 'Show QR code' }).click();
  await expect(cake.locator('[data-qr] svg')).toBeVisible();
  await expect(cake.getByRole('button', { name: 'Show QR code' })).toBeHidden();
  await expect(cake.locator('[data-qr-cover]')).toBeHidden();
  await captureCoverageStage(page, 'wallets-backup-app', test.info(), { group: GROUP });
  await page.getByRole('tab', { name: 'Stack Wallet' }).click();
  await expect(cake.locator('[data-qr] svg')).toHaveCount(0, { timeout: 1000 });
  await page.getByRole('tab', { name: 'Monero GUI / CLI' }).click();
  await page.getByRole('button', { name: 'Show the 25-word version' }).click();
  await expect(page.locator('[data-legacy-words] li')).toHaveCount(25);

  // Writing it down.
  await page.getByText('Write it down', { exact: true }).click();
  const words = await wordsOnThePage(page);
  expect(words).toHaveLength(16);
  await captureCoverageStage(page, 'wallets-backup-paper', test.info(), { group: GROUP });
  const next = page.getByRole('button', { name: 'Next: check my backup' });
  await expect(next).toBeDisabled();
  await page.getByText("I've written all 16 words down").click();
  await next.click();

  // The check: three of the words, by number. A wrong one is refused.
  await expect(page.getByRole('heading', { name: 'Check your backup' })).toBeVisible();
  const checks = page.locator('[data-word-check]');
  await expect(checks).toHaveCount(3);
  const asked = [];
  for (let i = 0; i < 3; i += 1) {
    const label = await checks.nth(i).locator('[data-word-label]').textContent();
    asked.push(Number(/Word (\d+)/.exec(label || '')[1]));
  }
  await checks.nth(0).locator('input').fill('wrongword');
  await checks.nth(1).locator('input').fill(words[asked[1] - 1]);
  await checks.nth(2).locator('input').fill(words[asked[2] - 1]);
  await page.getByRole('button', { name: 'Check and add wallet' }).click();
  await expect(checks.nth(0).getByText("That doesn't match.")).toBeVisible();
  await captureCoverageStage(page, 'wallets-check', test.info(), { group: GROUP });
  await checks.nth(0).locator('input').fill(words[asked[0] - 1]);
  await page.getByRole('button', { name: 'Check and add wallet' }).click();

  await expect(page).toHaveURL(/\/dashboard\/wallets\/[^/]+\/ready/);
  await expect(page.getByText('Copper Heron').first()).toBeVisible();
  await captureCoverageStage(page, 'wallets-ready', test.info(), { group: GROUP });

  await page.goto(base + '/dashboard/wallets');
  const row = page.locator('tr', { hasText: 'Copper Heron' });
  await expect(row).toContainText('Made in Monokulo');
  await expect(row).toContainText('stagenet');
});

test('skipping the backup needs the warning read, the box ticked and "skip" typed', async ({ page }) => {
  await signInAsAdmin(page);
  await startNewWallet(page, 'Skipped Wren');
  await page.getByRole('button', { name: 'Skip backup' }).click();
  await expect(page.getByRole('heading', { name: "You won't see this phrase again" })).toBeVisible();
  const skip = page.getByRole('button', { name: 'Skip backup and add the wallet' });
  await expect(skip).toBeDisabled();
  await captureCoverageStage(page, 'wallets-skip-warning', test.info(), { group: GROUP });
  await page.getByText('I understand that without the phrase').click();
  await expect(skip).toBeDisabled();
  await page.getByLabel('to confirm').fill('skip');
  await expect(skip).toBeEnabled();
  await skip.click();
  await expect(page).toHaveURL(/\/ready/);
  await expect(page.getByText('was not backed up')).toBeVisible();
});

test.describe('with JavaScript off', () => {
  test.use({ javaScriptEnabled: false });

  test('creating a wallet is shown unavailable with the reason, and bringing your own still works', async ({ page }) => {
    const { monokulo_url: base } = fixture();
    await signInAsAdmin(page);
    await page.goto(base + '/dashboard/wallets/setup');
    await expect(page.getByRole('button', { name: 'Create a new wallet' })).toBeDisabled();
    await expect(page.locator('#create-needs-js')).toContainText('needs JavaScript');
    await expect(page.getByRole('button', { name: 'Connect a device' })).toBeDisabled();
    await captureCoverageStage(page, 'wallets-choice-no-javascript', test.info(), { group: GROUP });

    await page.getByRole('button', { name: 'Bring your own wallet' }).click();
    await expect(page.getByRole('heading', { name: 'Bring your own wallet' })).toBeVisible();
  });
});

test('stores pick a wallet, and a wallet lists its stores and is renamed on its page', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
  await addWallet(page);

  // Several wallets: the store form picks none for you.
  await page.goto(base + '/dashboard/connect');
  await expect(page.locator('select[name="wallet_id"]')).toHaveValue('');
  await captureCoverageStage(page, 'wallets-store-form', test.info(), { group: GROUP });
  await createStore(page, 'wallet-shop.example.com');
  await finishStoreSetup(page);
  await expect(page.locator('.kv-table')).toContainText(WALLET_NAME);

  await page.goto(base + '/dashboard/wallets');
  await captureCoverageStage(page, 'wallets-list', test.info(), { group: GROUP });
  await page.getByRole('link', { name: WALLET_NAME }).click();
  await expect(page.getByRole('link', { name: 'wallet-shop.example.com' }).first()).toBeVisible();
  await expect(page.getByText('still use this wallet')).toBeVisible();
  await page.locator('form.rename-form input[name="name"]').fill('Dev stagenet till');
  await page.getByRole('button', { name: 'Rename' }).click();
  await expect(page.getByRole('heading', { name: 'Dev stagenet till' })).toBeVisible();
  await expect(page.getByText(/Renamed from/)).toBeVisible();
  await captureCoverageStage(page, 'wallets-detail', test.info(), { group: GROUP });
  // Put the name back for the specs' helpers.
  await page.locator('form.rename-form input[name="name"]').fill(WALLET_NAME);
  await page.getByRole('button', { name: 'Rename' }).click();
});
