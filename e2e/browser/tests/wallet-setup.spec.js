// @ts-check
// Store setup and wallets (docs/wallets.md) against the real binaries.
//
// Setup (/setup): the store step, then the wallet step, whose own small
// steps sit beside it. A new wallet is made in the browser (the
// wallet-setup WebAssembly module): its words and QR show straight away,
// one button makes a different phrase, the Feather tab shows its words in
// place and is recorded as feather, and the check asks two words from
// four, or lets you go on after 20 seconds. Without JavaScript the store
// step and a wallet already added still make a store, and creating a
// wallet is shown unavailable with the reason. Then stores on wallets: a
// wallet's page lists its stores, and a store changes its wallet. Captures
// each page for the gallery.
const { test, expect } = require('../coverage-test');
const { captureCoverageStage } = require('../coverage-screenshot');
const {
  useRealStack, fixture, signInAsAdmin, fakeNodeAddress, saveNodes, addWallet, createStore, finishStoreSetup, WALLET_NAME, expectSaved,
} = require('./backend-helpers');

useRealStack(test);

const GROUP = 'setup';

/** The words on the open tab of the backup screen. */
async function wordsOnTab(page, tab) {
  return page.locator(`[data-app-panel="${tab}"] [data-words] li, [data-app-panel="${tab}"] [data-legacy-words] li`).allTextContents();
}

/**
 * The store step's two fields read as one form: side by side, their labels
 * and boxes line up whatever help or error either has; on a phone they
 * stack, name first.
 */
async function expectFieldsInLine(page) {
  const box = async (selector) => page.locator(selector).boundingBox();
  const [name, site] = [await box('input[name="store_name"]'), await box('input[name="store_site"]')];
  expect(Math.abs(name.y - site.y)).toBeLessThan(1);
  expect(Math.abs(name.height - site.height)).toBeLessThan(1);
  const [nameLabel, siteLabel] = [await box('label[for="store-name"]'), await box('label[for="store-site"]')];
  expect(Math.abs(nameLabel.y - siteLabel.y)).toBeLessThan(1);
  const size = page.viewportSize();
  await page.setViewportSize({ width: 390, height: 844 });
  const [phoneName, phoneSite] = [await box('input[name="store_name"]'), await box('input[name="store_site"]')];
  expect(phoneSite.y).toBeGreaterThan(phoneName.y + phoneName.height);
  expect(Math.abs(phoneName.x - phoneSite.x)).toBeLessThan(1);
  if (size) await page.setViewportSize(size);
}

/** The wallet step's choice screen, from the store step for `site`. */
async function toWalletStep(page, name, site) {
  const { monokulo_url: base } = fixture();
  await page.goto(base + '/setup');
  await page.locator('label.kind-card', { hasText: 'My own website' }).click();
  await page.locator('input[name="store_name"]').fill(name);
  await page.locator('input[name="store_site"]').fill(site);
  await page.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(page.getByRole('heading', { name: `Where should ${name}'s money go?` })).toBeVisible();
}

/** Names a new stagenet wallet on the choice screen and starts making it. */
async function startNewWallet(page, walletName) {
  await page.locator('input[name="name"]').fill(walletName);
  await page.getByText('More options').click();
  await page.locator('select[name="network"]').selectOption('stagenet', { force: true });
  const create = page.getByRole('button', { name: 'Create a new wallet' });
  await expect(create).toBeEnabled();
  await create.click();
  await expect(page.getByRole('heading', { name: `Back up ${walletName}` })).toBeVisible();
}

test('a website store is set up: a new wallet backed up in Feather, checked with two words, then Done', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);

  // Store: whole-card kinds, a name apart from the site.
  await page.goto(base + '/setup');
  await expect(page.locator('.setup-steps [aria-current="step"]')).toHaveText('1Store');
  await expect(page.locator('label.kind-card')).toHaveCount(3);
  await captureCoverageStage(page, 'setup-store', test.info(), { group: GROUP });
  await toWalletStep(page, 'Geomart', 'https://geomart.example/shop?page=2');

  // Wallet: its small steps beside it, the name first, checked as it changes.
  await expect(page.locator('.setup-steps .sub-timeline [aria-current="step"]')).toHaveText('Kind');
  await expect(page.locator('input[name="name"]')).toHaveValue('Geomart takings');
  await expect(page.locator('#wallet-name-check')).toContainText('Free to use');
  await captureCoverageStage(page, 'setup-wallet-choice', test.info(), { group: GROUP });

  await startNewWallet(page, 'Geomart takings');
  const timeline = page.locator('[data-screen="backup"] .setup-steps .sub-timeline');
  await expect(timeline.locator('li.done')).toHaveText('Kind');
  await expect(timeline.locator('[aria-current="step"]')).toHaveText('Back up');
  // The words and the QR, straight away, with nothing to reveal.
  const cake = page.locator('[data-app-panel="cake"]');
  await expect(cake.locator('[data-words] li')).toHaveCount(16);
  await expect(cake.locator('[data-qr] svg')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Enter into Cake Wallet' })).toBeVisible();
  await expect(page.locator('form[data-register] input[name="phrase"]')).toHaveCount(0);
  await captureCoverageStage(page, 'setup-backup-cake', test.info(), { group: GROUP });

  // One button makes a different phrase, everywhere.
  const before = await wordsOnTab(page, 'cake');
  await cake.getByRole('button', { name: 'Make a different phrase' }).click();
  await expect.poll(() => wordsOnTab(page, 'cake')).not.toEqual(before);

  // The 25 words get the main column.
  await page.getByRole('tab', { name: 'Monero GUI / CLI' }).click();
  await expect(page.locator('[data-app-panel="gui"] [data-legacy-words] li')).toHaveCount(25);
  await captureCoverageStage(page, 'setup-backup-25-words', test.info(), { group: GROUP });

  // Feather: its 16 words in place, no switch to paper.
  await page.getByRole('tab', { name: 'Feather' }).click();
  const feather = page.locator('[data-app-panel="feather"]');
  await expect(feather.locator('[data-words] li')).toHaveCount(16);
  await expect(feather.locator('[data-qr]')).toHaveCount(0);
  await expect(page.locator('[data-app-panel="paper"]')).toBeHidden();
  await captureCoverageStage(page, 'setup-backup-feather', test.info(), { group: GROUP });
  const words = await wordsOnTab(page, 'feather');
  const next = page.getByRole('button', { name: 'Next: check two words' });
  await expect(next).toBeDisabled();
  await feather.getByText('Feather opened the wallet with these words.').click();
  await next.click();

  // Check: two words, four choices each; a wrong one is marked and can be tried again.
  await expect(page.getByRole('heading', { name: 'Check two words' })).toBeVisible();
  const go = page.locator('[data-check-next]');
  await expect(go).toBeDisabled();
  await expect(go).toContainText('Continue anyway in');
  await captureCoverageStage(page, 'setup-check', test.info(), { group: GROUP });
  const questions = page.locator('[data-question]');
  const answer = async (i) => {
    const label = await questions.nth(i).locator('[data-question-label]').textContent();
    return words[Number(/Word (\d+)/.exec(label || '')[1]) - 1];
  };
  const first = await answer(0);
  const wrong = questions.nth(0).locator('[data-pick]').filter({ hasNotText: new RegExp(`^${first}$`) }).first();
  await wrong.click();
  await expect(wrong).toHaveClass(/is-wrong/);
  await expect(questions.nth(0).locator('[data-question-note]')).toContainText('Not that one');
  await questions.nth(1).getByRole('button', { name: await answer(1), exact: true }).click();
  await expect(questions.nth(1).locator('[data-question-note]')).toHaveText('✓ Right');
  await captureCoverageStage(page, 'setup-check-wrong', test.info(), { group: GROUP });
  await questions.nth(0).getByRole('button', { name: first, exact: true }).click();
  await expect(go).toBeEnabled();
  await expect(go).toHaveText('Next: add the wallet');
  await captureCoverageStage(page, 'setup-check-right', test.info(), { group: GROUP });
  await go.click();

  // Done: never "taking payments" yet; what's left, with the guide first.
  await expect(page.getByRole('heading', { name: 'Geomart is set up' })).toBeVisible();
  await expect(page.locator('.setup-steps [aria-current="step"]')).toHaveText('3Done');
  await expect(page.getByText('Not taking payments yet')).toBeVisible();
  await expect(page.getByRole('link', { name: /Open the guide/ })).toHaveAttribute('href', /oceaneilonwy\.github\.io\/monokulo\/docs\//);
  await expect(page.getByText('Verify geomart.example')).toBeVisible();
  await captureCoverageStage(page, 'setup-done-website', test.info(), { group: GROUP });

  // The store is called by its name; its site is the host; the backup is Feather's.
  const store = await finishStoreSetup(page);
  await expect(page.getByRole('heading', { name: 'Geomart' })).toBeVisible();
  await expect(page.locator('.store-site')).toHaveText('geomart.example');
  await expect(page.getByRole('link', { name: /Help/ })).toHaveAttribute('href', /\/monokulo\/docs\//);
  await page.goto(base + '/account?tab=wallets');
  await page.getByRole('link', { name: 'Geomart takings' }).click();
  await expect(page.getByText('Made in Monokulo, saved in feather').first()).toBeVisible();

  // The same site again is refused, linking to the store.
  await page.goto(base + '/setup');
  await page.locator('input[name="store_name"]').fill('Geomart again');
  await page.locator('input[name="store_site"]').fill('geomart.example');
  await page.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(page.locator('#store-site-error')).toContainText('already uses geomart.example');
  await expect(page.locator('#store-site-error a')).toHaveAttribute('href', store);
  await expectFieldsInLine(page);
  await captureCoverageStage(page, 'setup-store-site-taken', test.info(), { group: GROUP });
});

test('the check lets you go on without answering after 20 seconds', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await page.clock.install();
  await page.goto(base + '/account/wallets/add');
  // Outside setup: the wallet's own steps only.
  await expect(page.locator('.setup-steps')).toHaveCount(0);
  await expect(page.locator('.sub-timeline [aria-current="step"]')).toHaveText('Kind');
  await startNewWallet(page, 'Patient Wren');
  await page.getByRole('tab', { name: 'On paper' }).click();
  await expect(page.locator('[data-app-panel="paper"] [data-words] li')).toHaveCount(16);
  await captureCoverageStage(page, 'wallets-backup-paper', test.info(), { group: GROUP });
  await page.getByText("I've written all 16 words down").click();
  await page.getByRole('button', { name: 'Next: check two words' }).click();
  const go = page.locator('[data-check-next]');
  await expect(go).toContainText('Continue anyway in 20 s');
  await page.clock.runFor(6000);
  await expect(go).toContainText('Continue anyway in 14 s');
  await expect(go).toBeDisabled();
  await page.clock.runFor(14000);
  await expect(go).toHaveText('Continue without checking');
  await expect(go).toBeEnabled();
  await captureCoverageStage(page, 'setup-check-timed-out', test.info(), { group: GROUP });
  await go.click();
  await expect(page.getByText('Patient Wren is added')).toBeVisible();
  await expect(page.getByText('Written down').first()).toBeVisible();
});

test('skipping the backup needs the warning read, the box ticked and "skip" typed', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await page.goto(base + '/account/wallets/add');
  await startNewWallet(page, 'Skipped Wren');
  await page.getByRole('button', { name: 'Skip backup…' }).click();
  await expect(page.getByRole('heading', { name: "You won't see this phrase again" })).toBeVisible();
  const skip = page.getByRole('button', { name: 'Skip backup and add the wallet' });
  await expect(skip).toBeDisabled();
  await captureCoverageStage(page, 'wallets-skip-warning', test.info(), { group: GROUP });
  await page.getByText('I understand that without the phrase').click();
  await expect(skip).toBeDisabled();
  await page.getByLabel('to confirm').fill('skip');
  await expect(skip).toBeEnabled();
  await skip.click();
  await expect(page.getByText('Skipped Wren is added')).toBeVisible();
  await expect(page.getByText('was not backed up')).toBeVisible();
});

test.describe('with JavaScript off', () => {
  test.use({ javaScriptEnabled: false });

  test('an in-person store is set up on a wallet already added; creating one is shown unavailable', async ({ page }) => {
    const { monokulo_url: base } = fixture();
    await signInAsAdmin(page);
    await saveNodes(page, { stagenet: [fakeNodeAddress()] });
    await addWallet(page);

    await page.goto(base + '/setup');
    await page.locator('label.kind-card', { hasText: 'In person only' }).click();
    // Nothing to ask about a site.
    await expect(page.locator('input[name="store_site"]')).toBeHidden();
    await page.locator('input[name="store_name"]').fill('Saturday market stall');
    await captureCoverageStage(page, 'setup-store-in-person-no-javascript', test.info(), { group: GROUP });
    await page.getByRole('button', { name: 'Next', exact: true }).click();

    await expect(page.getByRole('button', { name: 'Create a new wallet' })).toBeDisabled();
    await expect(page.locator('#create-needs-js')).toContainText('needs JavaScript');
    await expect(page.getByRole('button', { name: 'Connect a device' })).toBeDisabled();
    await captureCoverageStage(page, 'setup-wallet-choice-no-javascript', test.info(), { group: GROUP });
    const option = page.locator('select[name="wallet_id"] option', { hasText: WALLET_NAME });
    await page.locator('select[name="wallet_id"]').selectOption(await option.getAttribute('value'));
    await page.getByRole('button', { name: 'Use this wallet' }).click();

    await expect(page.getByRole('heading', { name: 'Saturday market stall is ready' })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Open the till' })).toHaveAttribute('href', /\/pos$/);
    await captureCoverageStage(page, 'setup-done-in-person', test.info(), { group: GROUP });

    // A site already used is refused in place, the two fields still in line.
    await createStore(page, 'nojs-shop.example');
    await page.goto(base + '/setup');
    await page.locator('input[name="store_name"]').fill('Another shop');
    await page.locator('input[name="store_site"]').fill('https://nojs-shop.example/basket');
    await page.getByRole('button', { name: 'Next', exact: true }).click();
    await expect(page.locator('#store-site-error')).toContainText('already uses nojs-shop.example');
    await expectFieldsInLine(page);
    await captureCoverageStage(page, 'setup-store-site-taken-no-javascript', test.info(), { group: GROUP });
  });

  test('bringing your own wallet works and shows where the keys are', async ({ page }) => {
    const { monokulo_url: base } = fixture();
    await signInAsAdmin(page);
    await page.goto(base + '/account/wallets/add');
    await page.locator('input[name="name"]').fill('Keys only');
    await page.getByRole('button', { name: 'Bring your own wallet' }).click();
    await expect(page.getByRole('heading', { name: 'Bring your own wallet' })).toBeVisible();
    await expect(page.locator('.sub-timeline [aria-current="step"]')).toHaveText('Keys');
    await page.getByText('Where do I find these keys?').click();
    await expect(page.getByText('Copy the secret one')).toBeVisible();
    await captureCoverageStage(page, 'wallets-keys-no-javascript', test.info(), { group: 'wallets' });
  });
});

test('a wallet lists its stores and is renamed on its page', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await createStore(page, 'wallet-shop.example.com');
  await finishStoreSetup(page);
  await expect(page.locator('.kv-table')).toContainText(WALLET_NAME);

  await page.goto(base + '/account?tab=wallets');
  await captureCoverageStage(page, 'wallets-list', test.info(), { group: 'wallets' });
  await page.getByRole('link', { name: WALLET_NAME }).click();
  await expect(page.getByRole('link', { name: 'wallet-shop.example.com' }).first()).toBeVisible();
  await expect(page.locator('section[aria-labelledby="stores-title"] .card-meta')).toHaveText('1 takes payments into this wallet');
  await page.locator('#card-details input[name="name"]').fill('Dev stagenet till');
  await page.locator('#save-bar').getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Dev stagenet till' })).toBeVisible();
  // History is folded until opened.
  await page.locator('details.history-fold summary').click();
  await expect(page.getByText(/Renamed from/)).toBeVisible();
  await captureCoverageStage(page, 'wallets-detail', test.info(), { group: 'wallets' });
  // Put the name back for the specs' helpers.
  await page.locator('#card-details input[name="name"]').fill(WALLET_NAME);
  await page.locator('#save-bar').getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('heading', { name: WALLET_NAME, level: 1 })).toBeVisible();
});

test('a store changes its wallet: the dropdown asks first, then the history shows both', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await createStore(page, 'changing-shop.example.com');
  const store = await finishStoreSetup(page);
  // A second stagenet wallet, with keys of its own.
  await page.goto(base + '/account/wallets/import?name=Cafe%20till&network=stagenet');
  await page.locator('input[name="view_key_hex"]').fill('0707070707070707070707070707070707070707070707070707070707070707');
  await page.locator('input[name="spend_pubkey_hex"]').fill('8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90');
  await page.getByRole('button', { name: 'Add wallet' }).click();
  await expect(page.getByText(/is added|already added this wallet/)).toBeVisible();

  await page.goto(base + store + '/settings');
  const section = page.locator('#card-wallet');
  // Shown in place, not behind an Edit button.
  await expect(section.getByRole('button', { name: /Edit/ })).toHaveCount(0);
  await expect(section).toContainText(`Payments go to ${WALLET_NAME}`);
  const wallet = section.getByRole('combobox', { name: 'Wallet' });
  await expect(wallet).toContainText(WALLET_NAME);
  await expect(wallet.locator('.tag-ok')).toHaveText('Current');

  await wallet.click();
  await section.getByRole('option', { name: /Cafe till/ }).click();
  // Picking posts at once: the card asks.
  await expect(section.getByRole('heading', { name: 'Change to Cafe till?' })).toBeVisible();
  await expect(section).toContainText(`No orders are open on ${WALLET_NAME}`);
  await captureCoverageStage(page, 'wallets-change-ask', test.info(), { group: 'wallets' });
  await section.getByRole('button', { name: 'Change to Cafe till' }).click();
  await expect(section.getByText('Changed to Cafe till.')).toBeVisible();
  await expect(section).toContainText('Payments go to Cafe till');
  await expect(section.getByRole('combobox', { name: 'Wallet' }).locator('.tag-ok')).toHaveText('Current');

  const history = section.locator('details.wallet-history');
  await expect(history.locator('summary')).toHaveText('Wallet history (2 wallets)');
  await history.locator('summary').click();
  await expect(history.locator('tbody tr')).toHaveCount(2);
  await expect(history.locator('tbody tr.current')).toContainText('Cafe till');
  await expect(history.locator('tbody tr').nth(1)).toContainText(WALLET_NAME);
  await expect(history.locator('.tag')).toHaveCount(0);
  await captureCoverageStage(page, 'wallets-change-history', test.info(), { group: 'wallets' });

  // The old wallet's page lists the store under Before. Changing back
  // leaves the helpers' wallet in use again.
  await page.goto(base + '/account?tab=wallets');
  await page.getByRole('link', { name: WALLET_NAME }).click();
  await page.locator('details.history-fold summary').click();
  await expect(page.getByText('changed to another wallet')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Before' })).toBeVisible();
  await page.goto(base + store + '/settings');
  await page.locator('#card-wallet').getByRole('combobox', { name: 'Wallet' }).click();
  await page.locator('#card-wallet').getByRole('option', { name: new RegExp(WALLET_NAME) }).click();
  await page.locator('#card-wallet').getByRole('button', { name: `Change to ${WALLET_NAME}` }).click();
  await expect(page.locator('#card-wallet')).toContainText(`Payments go to ${WALLET_NAME}`);

  // Cafe till is used by nothing now: retired, its keys deleted, said at
  // the top of its page; then brought back with its keys.
  await page.goto(base + '/account?tab=wallets');
  await page.getByRole('link', { name: 'Cafe till' }).click();
  await page.getByRole('link', { name: 'Retire wallet…' }).click();
  const retire = page.getByRole('dialog', { name: 'Retire “Cafe till”?' });
  await expect(retire.locator('.checks li.ok')).toHaveCount(2);
  await retire.getByLabel('Type “Cafe till” to confirm').fill('Cafe till');
  await retire.getByRole('button', { name: 'Retire wallet' }).click();
  const meta = page.locator('.wallet-meta');
  await expect(meta.locator('.tag')).toHaveText('retired');
  await expect(meta).toContainText('Keys deleted');
  await expect(page.getByText('Retired: its keys were deleted from key storage')).toBeAttached();
  await captureCoverageStage(page, 'wallets-retired', test.info(), { group: 'wallets' });
  await page.goto(base + '/account?tab=wallets');
  await expect(page.getByText('Retired wallets (1)')).toBeVisible();
  await page.goto(base + store + '/settings');
  await page.locator('#card-wallet').getByRole('combobox', { name: 'Wallet' }).click();
  await expect(page.locator('#card-wallet').getByRole('option', { name: /Cafe till/ })).toHaveCount(0);
  await page.keyboard.press('Escape');
  await page.locator('#card-wallet details.wallet-history summary').click();
  await expect(page.locator('#card-wallet tbody tr', { hasText: 'Cafe till' }).locator('.tag')).toHaveText('Retired');

  await page.goto(base + '/account?tab=wallets');
  await page.locator('.retired-wallets summary').click();
  await page.getByRole('link', { name: 'Cafe till' }).click();
  await page.getByRole('link', { name: 'Restore wallet…' }).click();
  const restore = page.getByRole('dialog', { name: 'Restore “Cafe till”' });
  // The CLI key help is on the restore form too.
  await expect(restore.getByText('Where do I find these keys?')).toBeVisible();
  await restore.locator('input[name="view_key_hex"]').fill('0707070707070707070707070707070707070707070707070707070707070707');
  await restore.locator('input[name="spend_pubkey_hex"]').fill('8621f587cfc4d6f869720476565ecd0972451ff7b8dada3498c9d3c2ca54fc90');
  await restore.getByRole('button', { name: 'Restore wallet' }).click();
  await expect(page.getByText('Cafe till is back.')).toBeVisible();
  await expect(page.locator('.wallet-meta')).toHaveCount(0);
});
