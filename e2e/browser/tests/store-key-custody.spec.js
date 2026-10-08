// @ts-check
// A store's key storage through the UI, with the real binaries, on the plain
// backend: the only one that runs without AMD SEV-SNP hardware. Keys going to
// the snp backend (encrypted in the browser or with key-custody-cli) are
// covered by the Rust tests in monokulo and engine-test-support, against a
// stand-in security processor; here, turning snp on where it can't run is
// reported, and plain stores carry on.
const { test, expect } = require('@playwright/test');
const { createStore, useRealStack, fixture, signInAsAdmin, fakeNodeAddress, saveNodes, saveEngineSettings, finishStoreSetup, VIEW_KEY, SPEND_PUBKEY, expectSaved } = require('./backend-helpers');

useRealStack(test);

async function createOrder(page, base, storeId, amount) {
  await page.goto(`${base}/dashboard/stores/${storeId}/orders/new`);
  await page.locator('input[name="amount"]').fill(amount);
  await page.getByRole('button', { name: 'Create order' }).click();
  await expect(page.locator('h1.order-title')).toBeVisible();
}

test('a store keeps its keys in the engine; snp turned on without SEV-SNP hardware is reported, and plain stores carry on', async ({ page }) => {
  const { monokulo_url: base } = fixture();
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);

  // One backend: no choice is offered, and the keys go in as they are.
  // A wallet's keys are given when it's added (Bring your own wallet).
  await page.goto(base + '/dashboard/wallets/import');
  await expect(page.locator('select[name="key_custody_backend"]')).toHaveCount(0);
  await createStore(page, 'kept.example.com');
  const storeId = (await finishStoreSetup(page)).split('/').pop();
  await createOrder(page, base, storeId, '0.5');

  // snp turned on, on a machine that isn't an SEV-SNP guest: it is saved,
  // but it can't start, and the forms don't offer it: plain is still the
  // only choice.
  await saveEngineSettings(page, {
    'key_custody.enabled_backends': 'plain,snp',
    'key_custody.default_backend': 'plain',
  });
  // Said in the toast, and as a banner that stays: the backend can't run.
  await expect(page.locator('#settings-toasts').getByText(/snp backend can.t start/)).toBeVisible();
  await expect(page.locator('#settings-banners').getByText(/snp key custody backend can.t run/)).toBeVisible();
  await page.goto(base + '/dashboard/wallets/import');
  await expect(page.locator('input[name="view_key_hex"]')).toBeVisible();
  await expect(page.locator('select[name="key_custody_backend"]')).toHaveCount(0);

  // The plain store carries on.
  await createOrder(page, base, storeId, '0.25');
  expect(await page.content()).not.toContain(VIEW_KEY);

  await saveEngineSettings(page, { 'key_custody.enabled_backends': 'plain', 'key_custody.default_backend': 'plain' });
  await expectSaved(page);
});
