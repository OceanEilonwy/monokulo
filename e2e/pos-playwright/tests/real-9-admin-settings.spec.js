// @ts-check
// The tabbed admin settings page and its Monero nodes form
// (nicer_admin_screen.md), in a real browser against the real binaries,
// with JavaScript and without: switching tabs and going back, adding,
// ordering and removing nodes, a node on the wrong network, the TLS box,
// the confirmation before a network stores use loses its last node, and
// the Monero nodes tab's marker while its only node is down. Also captures
// every tab for the gallery.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { startFakeNode } = require('../real-stack');
const {
  useRealStack, fixture, signInAsAdmin, transitionDone, openSettingsTab, saveNodes, nodeAddressBoxes, connectStore, fakeNodeAddress, SETTINGS_TABS,
} = require('./real-helpers');

useRealStack(test);

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

/** Two more nodes that say they're on testnet, and one on mainnet. */
let testnetA;
let testnetB;
let mainnetNode;
test.beforeAll(async () => {
  [testnetA, testnetB, mainnetNode] = await Promise.all([startFakeNode('testnet'), startFakeNode('testnet'), startFakeNode('mainnet')]);
});
test.afterAll(async () => {
  await Promise.all([testnetA, testnetB, mainnetNode].filter(Boolean).map((node) => node.stop()));
});

const tabLink = (page, label) => page.locator('#settings-tabs a', { hasText: label });
const panelHeading = (page) => page.locator('#settings-panel h2');
const saveButton = (page) => page.locator('#settings-panel').getByRole('button', { name: 'Save', exact: true });

/** The addresses in a network's rows, blank "Add a node" row included. */
const rowAddresses = (page, network) => nodeAddressBoxes(page, network).evaluateAll((boxes) => boxes.map((box) => box.value));

/** A browser page without JavaScript, signed in as the admin. */
async function pageWithoutJavaScript(browser) {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await signInAsAdmin(page);
  return { page, context };
}

for (const javaScript of [true, false]) {
  const how = javaScript ? 'with JavaScript' : 'without JavaScript';

  test(`the tab bar switches tabs, and Back returns to the one before (${how})`, async ({ page: jsPage, browser }) => {
    const { page, context } = javaScript ? { page: jsPage, context: null } : await pageWithoutJavaScript(browser);
    try {
      if (javaScript) await signInAsAdmin(page);
      await openSettingsTab(page, 'general');
      await expect(panelHeading(page)).toHaveText('General');
      if (javaScript) await page.evaluate(() => { window.__notReloaded = true; });

      // Without JavaScript a click mustn't land mid view transition.
      await transitionDone(page);
      await tabLink(page, 'Payments').click();
      await expect(panelHeading(page)).toHaveText('Payments');
      await expect(page).toHaveURL(/\?tab=payments$/);
      await expect(page.locator('#settings-tabs a[aria-current="page"]')).toContainText('Payments');
      await expect(page.locator('input[name="exchange_rate.cache_seconds"]')).toBeVisible();
      if (javaScript) expect(await page.evaluate(() => window.__notReloaded)).toBe(true);

      await transitionDone(page);
      await tabLink(page, 'Logging').click();
      await expect(panelHeading(page)).toHaveText('Logging');
      await expect(page).toHaveURL(/\?tab=logging$/);

      await page.goBack();
      await expect(page).toHaveURL(/\?tab=payments$/);
      await expect(panelHeading(page)).toHaveText('Payments');
      await expect(page.locator('#settings-tabs a[aria-current="page"]')).toContainText('Payments');
    } finally {
      if (context) await context.close();
    }
  });

  test(`a node is added, moved up to primary and removed (${how})`, async ({ page: jsPage, browser }) => {
    const { page, context } = javaScript ? { page: jsPage, context: null } : await pageWithoutJavaScript(browser);
    const [a, b] = [testnetA.address, testnetB.address];
    try {
      if (javaScript) await signInAsAdmin(page);
      await saveNodes(page, { testnet: [a] });
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();

      // Adding: with JavaScript through "Add another", without through the
      // blank row the page always has.
      await openSettingsTab(page, 'nodes');
      if (javaScript) {
        await page.locator('[data-node-add-another="testnet"]').click();
        const added = nodeAddressBoxes(page, 'testnet').last();
        await expect(added).toBeFocused();
        await added.fill(b);
      } else {
        await expect(page.locator('[data-node-add-another]').first()).toBeHidden();
        await nodeAddressBoxes(page, 'testnet').last().fill(b);
      }
      await transitionDone(page);
      await saveButton(page).click();
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();
      await page.reload();
      expect(await rowAddresses(page, 'testnet')).toEqual([a, b, '']);

      // Up to primary: one press, saved at once.
      await transitionDone(page);
      await page.locator('#settings-panel button[name="node_action"][value="up:testnet:1"]').click();
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();
      await page.reload();
      expect(await rowAddresses(page, 'testnet')).toEqual([b, a, '']);
      await expect(page.locator('[data-network="testnet"] legend').first()).toHaveText('Primary');

      // Removed.
      await transitionDone(page);
      await page.locator('#settings-panel button[name="node_action"][value="remove:testnet:1"]').click();
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();
      await page.reload();
      expect(await rowAddresses(page, 'testnet')).toEqual([b, '']);

      // Testnet has no stores: removing its last node asks nothing.
      let asked = false;
      page.once('dialog', async (dialog) => { asked = true; await dialog.accept(); });
      await transitionDone(page);
      await page.locator('#settings-panel button[name="node_action"][value="remove:testnet:0"]').click();
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();
      expect(asked).toBe(false);
      await page.reload();
      await expect(page.locator('details[data-network="testnet"] > summary')).toHaveText('Add a node for testnet');
    } finally {
      if (context) await context.close();
    }
  });

  test(`a node on another network is refused and nothing changes (${how})`, async ({ page: jsPage, browser }) => {
    const { page, context } = javaScript ? { page: jsPage, context: null } : await pageWithoutJavaScript(browser);
    try {
      if (javaScript) await signInAsAdmin(page);
      await saveNodes(page, { testnet: [testnetA.address] });
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();

      await saveNodes(page, { testnet: [testnetA.address, mainnetNode.address] });
      const message = `${mainnetNode.address} is on mainnet, not testnet.`;
      await expect(page.locator('[data-network="testnet"] p.error')).toHaveText(message);
      await expect(page.locator('#settings-banners')).toContainText(message);
      // What was typed is still there to fix.
      expect(await rowAddresses(page, 'testnet')).toEqual([testnetA.address, mainnetNode.address, '']);

      await openSettingsTab(page, 'nodes');
      expect(await rowAddresses(page, 'testnet')).toEqual([testnetA.address, '']);
      await saveNodes(page, { testnet: [] });
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();
    } finally {
      if (context) await context.close();
    }
  });
}

test('Use TLS shows and hides its row\'s self-signed box', async ({ page }) => {
  await signInAsAdmin(page);
  await openSettingsTab(page, 'nodes');
  await page.locator('details[data-network="mainnet"] > summary').click();
  const row = page.locator('[data-network="mainnet"] [data-node-add]').last();
  const selfSigned = row.locator('[data-node-self-signed]');
  await expect(selfSigned).toBeHidden();
  await row.locator('[data-node-tls]').check();
  await expect(selfSigned).toBeVisible();
  await expect(selfSigned.locator('input')).toBeChecked();
  await row.locator('[data-node-tls]').uncheck();
  await expect(selfSigned).toBeHidden();
});

test('removing the last node of a network stores use asks first', async ({ page }) => {
  await signInAsAdmin(page);
  await connectStore(page, 'nodes.example.com');
  await openSettingsTab(page, 'nodes');
  await expect(page.locator('.node-network[data-network="stagenet"]')).not.toHaveAttribute('data-tenant-count', '0');
  let posts = 0;
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/dashboard/admin/settings')) posts += 1;
  });
  let asked = '';
  page.once('dialog', async (dialog) => { asked = dialog.message(); await dialog.dismiss(); });
  await page.locator('#settings-panel button[name="node_action"][value="remove:stagenet:0"]').click();
  expect(asked).toMatch(/stores? uses? the stagenet network/);
  await page.waitForTimeout(500);
  expect(posts).toBe(0);
  expect(await rowAddresses(page, 'stagenet')).toEqual([fakeNodeAddress(), '']);
});

test('the Monero nodes tab is marked while the only node of a network stores use is down', async ({ page }) => {
  test.setTimeout(3 * 60 * 1000);
  const { monokulo_url: base, fake_monerod: fake } = fixture();
  await signInAsAdmin(page);
  // A store on stagenet (the spec before this one made it, if it ran).
  await page.goto(base + '/dashboard');
  if (!(await page.getByRole('link', { name: 'view →' }).count())) await connectStore(page, 'marker.example.com');
  const marked = async () => {
    await openSettingsTab(page, 'general');
    return (await page.locator('#settings-tabs a', { hasText: 'Monero nodes' }).locator('.tab-marker').count()) > 0;
  };
  const nodesTabMarked = () => expect.poll(async () => {
    // The Monero nodes tab waits for a fresh status; the others show what's known.
    await openSettingsTab(page, 'nodes');
    return marked();
  }, { timeout: 90_000, intervals: [2000] });

  await nodesTabMarked().toBe(false);
  await fetch(`http://${fake}/fake/offline`, { method: 'POST' });
  try {
    await nodesTabMarked().toBe(true);
    await expect(page.locator('#settings-tabs a', { hasText: 'Monero nodes' })).toContainText('(needs attention)');
  } finally {
    await fetch(`http://${fake}/fake/online`, { method: 'POST' });
  }
  await nodesTabMarked().toBe(false);
});

test('every tab, for the gallery', async ({ page }) => {
  await signInAsAdmin(page);
  // Stagenet with a node, so the Monero nodes tab shows a row and its status.
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
  for (const tab of SETTINGS_TABS) {
    await openSettingsTab(page, tab);
    if (tab === 'nodes') {
      // How the engine performs (docs/engine_scaling.md section 6): both
      // processes' CPU and memory, stacked, and stagenet's scan.
      await expect(page.locator('#resources-title')).toBeVisible();
      await expect(page.locator('.resource-chart')).toHaveCount(2);
      await expect(page.locator('[data-scanning="stagenet"]')).toContainText('Pace set by');
    }
    await captureCoverageStage(page, `admin-settings-${tab}`, test.info(), { group: 'admin-settings', shapes: ['mobile-portrait', 'desktop'] });
  }
});
