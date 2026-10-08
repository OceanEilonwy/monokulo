// @ts-check
// The admin settings page in a real browser (admin_settings_v2.md 4.4, 4.5
// and 4.7), served by the real binaries rather than the coverage fixture:
// descriptions and examples at phone and desktop widths, the confirmation
// before clearing a network stores use, and banners in the error colour in
// both themes.
const { test, expect } = require('@playwright/test');
const {
  createStore, useRealStack, fixture, signInAsAdmin, fakeNodeAddress, saveNodes, fillNodes, saveEngineSettings, openSettingsTab, SETTINGS_TABS, VIEW_KEY, SPEND_PUBKEY,
} = require('./backend-helpers');

useRealStack(test);

// Whatever a test did to the stagenet node, the next one starts with it set.
test.afterEach(async ({ page }) => {
  await signInAsAdmin(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
});

const SIZES = { 'small phone': { width: 320, height: 568 }, phone: { width: 390, height: 844 }, desktop: { width: 1280, height: 900 } };

for (const [name, size] of Object.entries(SIZES)) {
  test(`every setting is described, with examples, and nothing scrolls sideways (${name})`, async ({ page }) => {
    await page.setViewportSize(size);
    await signInAsAdmin(page);
    let total = 0;
    for (const tab of SETTINGS_TABS) {
      await openSettingsTab(page, tab);
      // A key custody backend that's turned off keeps its section hidden.
      const fields = page.locator('.setting-field:visible');
      total += await fields.count();
      for (const field of await fields.all()) {
        await expect(field.locator('.field-help').first()).toBeVisible();
      }
      if (tab === 'nodes') {
        // A network with no nodes and no stores starts closed; opened, its
        // address help carries an example address for that network.
        const closed = page.locator('details[data-network="stagenet"] > summary');
        if (await closed.count()) await closed.click();
        await expect(page.locator('[data-network="stagenet"] .field-help code').first()).toBeVisible();
      }
      const sideways = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(sideways, tab).toBeLessThanOrEqual(0);
    }
    expect(total).toBeGreaterThan(20);
  });
}

test('clearing a network stores use asks first; one no store uses does not', async ({ page }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  // Testnet gets a node no store uses. The fake node says it's on
  // stagenet, so testnet gets one that doesn't answer, which is saved.
  await saveNodes(page, { stagenet: [fakeNodeAddress()], testnet: ['127.0.0.1:9'] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();

  await openSettingsTab(page, 'nodes');
  const stagenet = page.locator('.node-network[data-network="stagenet"]');
  if (Number(await stagenet.getAttribute('data-tenant-count')) === 0) {
    // Run on its own: make a store on stagenet to protect.
    await createStore(page, 'guarded.example.com');
    await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
    await openSettingsTab(page, 'nodes');
  }
  const count = Number(await stagenet.getAttribute('data-tenant-count'));
  expect(count).toBeGreaterThan(0);
  const expected = count === 1 ? '1 store uses the stagenet network' : `${count} stores use the stagenet network`;
  let posts = 0;
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/dashboard/admin/settings')) posts += 1;
  });

  // Dismissed: nothing is sent.
  await fillNodes(page, { stagenet: [] });
  let asked = '';
  page.once('dialog', async (dialog) => { asked = dialog.message(); await dialog.dismiss(); });
  await page.locator('#settings-panel').getByRole('button', { name: 'Save', exact: true }).click();
  expect(asked).toContain(expected);
  await page.waitForTimeout(500);
  expect(posts).toBe(0);

  // Accepted: sent, and the red banner says what happened.
  page.once('dialog', (dialog) => dialog.accept());
  await page.locator('#settings-panel').getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/the stagenet network, which no longer has any reachable nodes/)).toBeVisible();
  expect(posts).toBe(1);

  // Testnet has no stores: no question.
  let testnetAsked = false;
  page.once('dialog', async (dialog) => { testnetAsked = true; await dialog.accept(); });
  await fillNodes(page, { testnet: [] });
  await page.locator('#settings-panel').getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
  expect(testnetAsked).toBe(false);
  expect(posts).toBe(2);

  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
});

for (const scheme of ['light', 'dark']) {
  for (const [name, size] of Object.entries(SIZES)) {
    test(`banners fit and the red one uses the error colour (${scheme}, ${name})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.setViewportSize(size);
      await signInAsAdmin(page);
      // A red banner (a network stores use cleared) and a yellow one (a
      // restart-only setting) on one page.
      await openSettingsTab(page, 'server');
      const threads = Number(await page.locator('input[name="server.worker_threads"]').inputValue());
      page.once('dialog', (dialog) => dialog.accept());
      // The restart-only setting first, then the node: the banners after
      // the second save are the node's; the restart shows on its field.
      await saveEngineSettings(page, { 'server.worker_threads': String((threads % 8) + 1) });
      await expect(page.locator('p.warning').first()).toBeVisible();
      await saveNodes(page, { stagenet: [] });
      const red = page.locator('p.error[role="alert"]').first();
      await expect(red).toBeVisible();
      const [color, token] = await red.evaluate((el) => {
        const probe = document.createElement('span');
        probe.style.color = 'var(--error)';
        document.body.appendChild(probe);
        const expected = getComputedStyle(probe).color;
        probe.remove();
        return [getComputedStyle(el).color, expected];
      });
      expect(color).toBe(token);
      // theme.css's --error for this theme.
      expect(color).toBe(scheme === 'dark' ? 'rgb(255, 107, 107)' : 'rgb(176, 0, 32)');
      const sideways = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(sideways).toBeLessThanOrEqual(0);
      await saveNodes(page, { stagenet: [fakeNodeAddress()] });
      await expect(page.getByText('Settings saved and applied.')).toBeVisible();
    });
  }
}

test('an on/off setting is a switch: on and off both save', async ({ page }) => {
  await signInAsAdmin(page);
  await openSettingsTab(page, 'abuse');
  const input = page.locator('input[role=switch][name="abuse.under_attack"]');
  const control = page.locator('.switch', { has: input });
  await expect(control).toBeVisible();
  await expect(page.getByRole('switch', { name: /under attack/i })).toHaveCount(1);
  const save = async () => {
    await page.locator('#settings-panel').getByRole('button', { name: 'Save', exact: true }).click();
    await expect(page.locator('#settings-panel .save-status')).toBeVisible();
    await openSettingsTab(page, 'abuse');
  };

  await expect(input).not.toBeChecked();
  await expect(control.locator('.switch-off')).toBeVisible();
  await control.click();
  await expect(input).toBeChecked();
  await expect(control.locator('.switch-on')).toBeVisible();
  await save();
  await expect(input).toBeChecked();

  // Off sends no value of its own; the page still saves it as off.
  await control.click();
  await save();
  await expect(input).not.toBeChecked();
});
