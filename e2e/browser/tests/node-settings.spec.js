// @ts-check
// The originally reported bug, end to end with the real binaries
// (admin_settings_v2.md task 6.3): on a fresh instance, a Monero node saved
// on the admin page applies to the running engine straight away, a store
// can then be connected, and the warnings for restart-only settings and
// for clearing a network stores use work.
const { test, expect } = require('@playwright/test');
const { useRealStack, fixture, reloadUntil, saveEngineSettings, saveNodes, finishStoreSetup, VIEW_KEY, SPEND_PUBKEY } = require('./backend-helpers');

useRealStack(test);

test('a node saved on a fresh instance applies straight away, and the warnings work', async ({ page }) => {
  const { monokulo_url: base, fake_monerod: node } = fixture();

  // 1. Fresh instance: the first-run wizard creates the admin account.
  await page.goto(base + '/');
  await expect(page).toHaveURL(/\/admin\/setup/);
  await page.locator('input[name="email"]').fill('admin@example.com');
  await page.locator('input[name="password"]').fill('correct horse battery staple');
  await page.locator('input[name="confirm_password"]').fill('correct horse battery staple');
  await page.getByRole('button', { name: 'Create admin account' }).click();

  // 2. Save a stagenet node on the admin page.
  await saveNodes(page, { stagenet: [node] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();

  // 3. The status page shows stagenet, reachable, with no restart.
  await reloadUntil(page, base + '/status', (html) => html.includes('<h2>stagenet</h2>') && html.includes('reachable'));

  // 4. A stagenet store can be connected.
  await page.goto(base + '/dashboard/connect');
  await page.locator('input[name="site_url"]').fill('https://shop.example.com');
  await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
  await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
  await page.locator('select[name="network"]').selectOption('stagenet');
  await page.getByRole('button', { name: 'Connect' }).click();
  const storeLink = await finishStoreSetup(page);
  const storeId = storeLink.split('/').pop();

  // 5. A saved poll interval shows on the status page.
  await saveEngineSettings(page, { 'payment.mempool_poll_interval_ms': '2000' });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
  await reloadUntil(page, base + '/status', (html) => html.includes('polls every 2s'));

  // 6. A restart-only setting says so.
  await saveEngineSettings(page, { 'server.worker_threads': '3' });
  await expect(page.getByText(/take effect after the engine restarts/)).toBeVisible();

  // 7. Clearing stagenet asks first, then says 1 store is affected, and
  //    the merchant sees an alert everywhere but the POS terminal.
  let dialogText = '';
  page.once('dialog', async (dialog) => {
    dialogText = dialog.message();
    await dialog.accept();
  });
  await saveNodes(page, { stagenet: [] });
  expect(dialogText).toContain('1 store uses the stagenet network');
  await expect(page.getByText(/the stagenet network, which no longer has any reachable nodes/)).toBeVisible();
  await reloadUntil(page, base + '/dashboard', (html) => html.includes('shop.example.com: payments aren'));
  await page.goto(`${base}/dashboard/stores/${storeId}/pos`);
  expect(await page.content()).not.toContain('payments aren');

  // 8. Restoring the node clears the alert.
  await saveNodes(page, { stagenet: [node] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
  await reloadUntil(page, base + '/dashboard', (html) => !html.includes('payments aren'));
});
