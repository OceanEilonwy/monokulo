// @ts-check
// The tabbed admin settings page and its Monero nodes form
// (nicer_admin_screen.md), in a real browser against the real binaries:
// adding, ordering and removing nodes, and the confirmation before a
// network stores use loses its last node. Also captures every tab for the
// gallery.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { startFakeNode } = require('../real-stack');
const {
  useRealStack, signInAsAdmin, openSettingsTab, saveNodes, nodeAddressBoxes, connectStore, fakeNodeAddress, SETTINGS_TABS, expectSaved, openNodes, pressSave,
  saveEngineSettings,
  answerNodeQuestion,
} = require('./backend-helpers');

useRealStack(test);

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

/** Two more nodes that say they're on testnet. */
let testnetA;
let testnetB;
test.beforeAll(async () => {
  [testnetA, testnetB] = await Promise.all([startFakeNode('testnet'), startFakeNode('testnet')]);
});
test.afterAll(async () => {
  await Promise.all([testnetA, testnetB].filter(Boolean).map((node) => node.stop()));
});

/** The addresses in a network's rows, blank "Add a node" row included. */
const rowAddresses = (page, network) => nodeAddressBoxes(page, network).evaluateAll((boxes) => boxes.map((box) => box.value));

test('a node is added, moved up to primary and removed (with JavaScript)', async ({ page }) => {
  const [a, b] = [testnetA.address, testnetB.address];
  await signInAsAdmin(page);
  await saveNodes(page, { testnet: [a] });
  await expectSaved(page);

  // Adding, through "Add another".
  await openSettingsTab(page, 'nodes');
  await openNodes(page, 'testnet');
  await page.locator('[data-node-add-another="testnet"]').click();
  const added = nodeAddressBoxes(page, 'testnet').last();
  await expect(added).toBeFocused();
  await added.fill(b);
  await pressSave(page);
  await expectSaved(page);
  await page.reload();
  expect(await rowAddresses(page, 'testnet')).toEqual([a, b, '']);

  // Up to primary, dragged by its handle: a change, saved with the bar,
  // counted once.
  await openNodes(page, 'testnet');
  await expect(page.locator('#settings-panel button[name="node_action"][value="up:testnet:1"]')).toBeHidden();
  const testnet = page.locator('#card-network-testnet');
  // The rows reorder while the handle is still held, not on release.
  const handle = await testnet.locator('[data-node-row="1"] [data-node-handle]').boundingBox();
  const primary = await testnet.locator('[data-node-row="0"] summary').boundingBox();
  if (!handle || !primary) throw new Error('node rows not laid out');
  await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
  await page.mouse.down();
  await page.mouse.move(handle.x + handle.width / 2, primary.y + 2, { steps: 8 });
  await expect(testnet.locator('.node-row-address').first()).toHaveText(b);
  await expect(testnet.locator('[data-node-place]').first()).toHaveText('Primary');
  await expect(testnet.locator('[data-node-row].is-dragging [data-node-place]')).toHaveText('Primary');
  await page.mouse.up();
  await expect(testnet.locator('[data-node-row].is-dragging')).toHaveCount(0);
  await expect(testnet.locator('[data-node-place]').first()).toHaveText('Primary');
  await expect(testnet.locator('.node-row-address').first()).toHaveText(b);
  await expect(page.locator('#save-bar')).toContainText('1 unsaved change in Testnet');
  await expect(testnet.locator('[data-node-mark]').first()).toHaveText('moved');
  await pressSave(page);
  await expectSaved(page);
  await page.reload();
  expect(await rowAddresses(page, 'testnet')).toEqual([b, a, '']);
  await expect(page.locator('#card-network-testnet [data-node-place]').first()).toHaveText('Primary');

  // Removed: the row goes, saved with the bar.
  await openNodes(page, 'testnet');
  await page.locator('#settings-panel button[name="node_action"][value="remove:testnet:1"]').click();
  await expect(page.locator('#save-bar')).toContainText('1 unsaved change in Testnet');
  await pressSave(page);
  await expectSaved(page);
  await page.reload();
  expect(await rowAddresses(page, 'testnet')).toEqual([b, '']);

  // Testnet has no stores: removing its last node asks nothing.
  await openNodes(page, 'testnet');
  await page.locator('#settings-panel button[name="node_action"][value="remove:testnet:0"]').click();
  await pressSave(page);
  await expectSaved(page);
  await expect(page.locator('#node-clear-dialog')).toBeHidden();
  await page.reload();
  await expect(page.locator('#card-network-testnet details[data-node-network-closed] > summary')).toHaveText('Add a node for testnet');
});

test('saving without the last node of a network stores use asks first', async ({ page }) => {
  await signInAsAdmin(page);
  await connectStore(page, 'nodes.example.com');
  await openSettingsTab(page, 'nodes');
  await expect(page.locator('.node-network[data-network="stagenet"]')).not.toHaveAttribute('data-tenant-count', '0');
  let posts = 0;
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/admin/settings')) posts += 1;
  });
  // Remove takes the row out; the question comes with Save.
  const removeStagenet = page.locator('#settings-panel button[name="node_action"][value="remove:stagenet:0"]');
  await openNodes(page, 'stagenet');
  await removeStagenet.click();
  expect(posts).toBe(0);
  expect(await rowAddresses(page, 'stagenet')).toEqual(['']);
  await pressSave(page);
  expect(await answerNodeQuestion(page, 'Keep editing')).toMatch(/stores? uses? the stagenet network/);
  // Still unsaved; Discard brings the node back.
  await page.locator('#card-network-stagenet').getByRole('button', { name: 'Discard' }).click();
  await expect.poll(() => rowAddresses(page, 'stagenet')).toEqual([fakeNodeAddress(), '']);
  await expect(page.locator('#save-bar')).toBeHidden();
  // The dismissed question sent nothing: a save with nothing changed is
  // the first post the page makes, and one the question had sent would
  // have gone out before it.
  await pressSave(page);
  await expectSaved(page);
  expect(posts).toBe(1);

  // Saved anyway: sent, and the red banner says what happened.
  await openNodes(page, 'stagenet');
  await removeStagenet.click();
  await pressSave(page);
  await answerNodeQuestion(page, 'Save anyway');
  await expect(page.getByText(/the stagenet network, which no longer has any reachable nodes/)).toBeVisible();
  expect(posts).toBe(2);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
});

test('every tab, for the gallery @gallery', async ({ page }) => {
  await signInAsAdmin(page);
  // Stagenet with a node, so the Monero nodes tab shows a row and its status.
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  for (const tab of SETTINGS_TABS) {
    await openSettingsTab(page, tab);
    if (tab === 'nodes') {
      // How the engine performs is on the
      // engine page: the tab keeps the nodes and their status, and each
      // network's card links there.
      await expect(page.locator('#card-network-stagenet .node-row-status').first()).toBeVisible();
      await expect(page.locator('.resource-chart')).toHaveCount(0);
      await expect(page.locator('[data-scanning]')).toHaveCount(0);
      const link = page.locator('#card-network-stagenet .card-head a.engine-link');
      await expect(link).toHaveText('See it on the engine page');
      await expect(link).toHaveAttribute('href', '/status/engine?network=stagenet');
    }
    await captureCoverageStage(page, `admin-settings-${tab}`, test.info(), { group: 'admin-settings', shapes: ['mobile-portrait', 'desktop'] });
  }
  // The options files, each with its Reload, are a tab of their own.
  await expect(page.locator('#settings-panel form[action="/admin/settings/reload"]')).toHaveCount(0);
  await page.locator('#settings-tabs').getByRole('link', { name: 'Files' }).click();
  await expect(page).toHaveURL(/\?tab=files$/);
  await expect(page.locator('#settings-panel').getByRole('button', { name: 'Reload options file' })).toHaveCount(2);
  await captureCoverageStage(page, 'admin-settings-files', test.info(), { group: 'admin-settings', shapes: ['mobile-portrait', 'desktop'] });
});

// The CoinMarketCap API key: the one secret the page sets. Asked for in the
// CoinMarketCap part of the exchange rates card, checked at CoinMarketCap
// (here a stand-in on this machine), then shown only as set: never back in
// the page. Typing one is an unsaved change like any other; Remove takes
// it away.
test('the CoinMarketCap API key is entered, shown only as set, and removed', async ({ page }) => {
  const http = require('node:http');
  const KEY = 'b54bcf4d-1bca-4e8e-9a24-22ff2c3d462c';
  const seen = [];
  const standIn = http.createServer((request, response) => {
    seen.push([request.url, request.headers['x-cmc_pro_api_key'] || null]);
    const ok = request.url === '/v1/key/info' && request.headers['x-cmc_pro_api_key'] === KEY;
    response.writeHead(ok ? 200 : 401, { 'content-type': 'application/json' });
    response.end(ok
      ? '{"data":{"plan":{}},"status":{"error_code":0}}'
      : '{"status":{"error_code":1001,"error_message":"This API Key is invalid."}}');
  });
  await new Promise((resolve) => standIn.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${standIn.address().port}/public-api`;
  try {
    await signInAsAdmin(page);
    await saveEngineSettings(page, { 'exchange_rate.coinmarketcap_base_url': base });
    await expectSaved(page);

    await openSettingsTab(page, 'payments');
    const card = page.getByRole('region', { name: 'Exchange rates' });
    const box = card.locator('input[type=password][name="exchange_rate.coinmarketcap_api_key"]');
    const field = card.locator('.setting-field').filter({ has: page.locator('input[name="exchange_rate.coinmarketcap_api_key"]') });
    await expect(box).toBeVisible();
    await expect(box).toHaveValue('');
    await expect(field).toContainText('No key is set.');
    await expect(field.getByRole('checkbox', { name: 'Remove the key' })).toHaveCount(0);

    // A wrong key is an unsaved change, refused with CoinMarketCap's
    // reason, and not kept.
    await box.fill('00000000-0000-0000-0000-000000000000');
    await expect(field).toHaveClass(/is-changed/);
    await pressSave(page);
    await expect(page.locator('#settings-toasts .toast-error')).toBeVisible();
    await expect(field).toContainText('CoinMarketCap refused this key (HTTP 401: This API Key is invalid.)');

    await openSettingsTab(page, 'payments');
    await box.fill(KEY);
    await pressSave(page);
    await expectSaved(page);
    expect(seen.at(-1)).toEqual(['/v1/key/info', KEY]);
    await openSettingsTab(page, 'payments');
    await expect(field).toContainText('A key is set.');
    await expect(box).toHaveValue('');
    expect(await page.content()).not.toContain(KEY);

    await field.getByRole('checkbox', { name: 'Remove the key' }).check();
    await expect(field).toHaveClass(/is-changed/);
    await pressSave(page);
    await expectSaved(page);
    await openSettingsTab(page, 'payments');
    await expect(field).toContainText('No key is set.');
  } finally {
    await saveEngineSettings(page, { 'exchange_rate.coinmarketcap_base_url': 'https://pro-api.coinmarketcap.com/public-api' });
    standIn.close();
  }
});
