// @ts-check
// A store moving its keys between key custody backends through the UI,
// with the real binaries and a real key-custody-server
// (admin_settings_v2.md task 6.4). Payments after a move are covered by
// the Rust integration test in scanner-test-support: the fake node here
// can't make them.
const { test, expect } = require('@playwright/test');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { useRealStack, fixture, signInAsAdmin, fakeNodeJson, saveEngineSettings, reloadUntil, VIEW_KEY, SPEND_PUBKEY } = require('./real-helpers');

useRealStack(test);

const KEY_CUSTODY_SERVER = path.resolve(__dirname, '..', '..', '..', 'target', 'debug', 'key-custody-server');

function startKeyCustodyServer(socketPath) {
  try { fs.unlinkSync(socketPath); } catch { /* none left over */ }
  const child = spawn(KEY_CUSTODY_SERVER, [socketPath], { stdio: 'ignore' });
  return child;
}

async function waitForSocket(socketPath) {
  await expect.poll(() => fs.existsSync(socketPath), { timeout: 10_000 }).toBe(true);
}

test('a store moves its keys to another backend and keeps working; outages and a disabled backend are reported', async ({ page }) => {
  const { monokulo_url: base, logs } = fixture();
  const socketPath = path.join(logs, 'key-custody.sock');
  let server = startKeyCustodyServer(socketPath);
  await waitForSocket(socketPath);

  try {
    await signInAsAdmin(page);
    // Stagenet on the fake node, and both backends enabled.
    await saveEngineSettings(page, {
      monero_node_stagenet: fakeNodeJson(),
      'key_custody.socket_path': socketPath,
      'key_custody.enabled_backends': 'plain,socket',
      'key_custody.default_backend': 'plain',
    });
    await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();

    // A new store, choosing where its keys go (the form offers the choice
    // once the engine's status says there is one).
    await reloadUntil(page, base + '/dashboard/connect', (html) => html.includes('name="key_custody_backend"'));
    await page.locator('input[name="site_url"]').fill('https://kept.example.com');
    await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
    await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
    await page.locator('select[name="network"]').selectOption('stagenet');
    await page.locator('select[name="key_custody_backend"]').selectOption('plain');
    await page.getByRole('button', { name: 'Connect' }).click();
    await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
    const storeId = (await page.getByRole('link', { name: /View store/ }).getAttribute('href')).split('/').pop();
    const settings = `${base}/dashboard/stores/${storeId}/settings`;

    // An order before the move.
    await page.goto(`${base}/dashboard/stores/${storeId}/orders/new`);
    await page.locator('input[name="amount"]').fill('0.5');
    await page.getByRole('button', { name: 'Create order' }).click();
    await expect(page.locator('h1.order-title')).toBeVisible();
    const firstOrder = page.url();

    // Move the keys to the socket backend, entering them again.
    await page.goto(settings);
    await expect(page.getByRole('heading', { name: 'Key storage' })).toBeVisible();
    await expect(page.getByText('In the engine (simplest)')).toBeVisible();
    await page.locator('select[name="backend"]').selectOption('socket');
    await page.locator('form[action$="/settings/key-custody"] input[name="view_key_hex"]').fill(VIEW_KEY);
    await page.locator('form[action$="/settings/key-custody"] input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
    await page.getByRole('button', { name: 'Move keys' }).click();
    await expect(page.getByText('In a separate key storage service')).toBeVisible();
    expect(await page.content()).not.toContain(VIEW_KEY);

    // The old order still shows; a new one gets an address.
    await page.goto(firstOrder);
    await expect(page.locator('h1.order-title')).toBeVisible();
    await page.goto(`${base}/dashboard/stores/${storeId}/orders/new`);
    await page.locator('input[name="amount"]').fill('0.25');
    await page.getByRole('button', { name: 'Create order' }).click();
    await expect(page.locator('h1.order-title')).toBeVisible();

    // The key storage service stops: the owner is told, and told it's
    // fine again once it's back.
    server.kill('SIGKILL');
    await reloadUntil(page, base + '/dashboard', (html) => html.includes('kept.example.com: payments aren') && (html.includes("isn't answering") || html.includes('isn&#39;t answering')));
    server = startKeyCustodyServer(socketPath);
    await waitForSocket(socketPath);
    await reloadUntil(page, base + '/dashboard', (html) => !html.includes('kept.example.com: payments aren'));
    // And the store takes orders again straight away (its keys are
    // registered again in the restarted service).
    await page.goto(`${base}/dashboard/stores/${storeId}/orders/new`);
    await page.locator('input[name="amount"]').fill('0.75');
    await page.getByRole('button', { name: 'Create order' }).click();
    await expect(page.locator('h1.order-title')).toBeVisible();

    // The socket backend is turned off: the owner is told, and the store's
    // settings page offers to move back.
    await saveEngineSettings(page, { 'key_custody.enabled_backends': 'plain', 'key_custody.default_backend': 'plain' });
    await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
    await reloadUntil(page, base + '/dashboard', (html) => html.includes('turned off'));
    await reloadUntil(page, settings, (html) => html.includes('has been turned off on this instance'));
    await page.locator('select[name="backend"]').selectOption('plain');
    await page.locator('form[action$="/settings/key-custody"] input[name="view_key_hex"]').fill(VIEW_KEY);
    await page.locator('form[action$="/settings/key-custody"] input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
    await page.getByRole('button', { name: 'Move keys' }).click();
    await reloadUntil(page, base + '/dashboard', (html) => !html.includes('kept.example.com: payments aren'));
  } finally {
    server.kill('SIGKILL');
  }
});
