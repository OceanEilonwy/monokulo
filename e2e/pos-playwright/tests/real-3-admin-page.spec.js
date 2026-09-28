// @ts-check
// The admin settings page in a real browser (admin_settings_v2.md 4.4, 4.5
// and 4.7), served by the real binaries rather than the coverage fixture:
// descriptions and examples at phone and desktop widths, the confirmation
// before clearing a network stores use, and banners in the error colour in
// both themes.
const { test, expect } = require('@playwright/test');
const { useRealStack, fixture, signInAsAdmin, fakeNodeJson, saveEngineSettings, VIEW_KEY, SPEND_PUBKEY } = require('./real-helpers');

useRealStack(test);

// Whatever a test did to the stagenet node, the next one starts with it set.
test.afterEach(async ({ page }) => {
  await signInAsAdmin(page);
  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
});

const SIZES = { phone: { width: 390, height: 844 }, desktop: { width: 1280, height: 900 } };

for (const [name, size] of Object.entries(SIZES)) {
  test(`every setting is described, the node example opens, and nothing scrolls sideways (${name})`, async ({ page }) => {
    await page.setViewportSize(size);
    await signInAsAdmin(page);
    await page.goto(fixture().monokulo_url + '/dashboard/admin/settings');
    // A key custody backend that's turned off keeps its section hidden.
    const fields = page.locator('.setting-field:visible');
    expect(await fields.count()).toBeGreaterThan(20);
    for (const field of await fields.all()) {
      await expect(field.locator('.field-help').first()).toBeVisible();
    }
    const example = page.locator('details.field-help').first();
    await example.locator('summary').click();
    await expect(example.locator('pre')).toBeVisible();
    const sideways = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(sideways).toBeLessThanOrEqual(0);
  });
}

test('clearing a network stores use asks first; one no store uses does not', async ({ page }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson(), monero_node_testnet: fakeNodeJson() });
  await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();

  await page.goto(base + '/dashboard/admin/settings');
  if (Number(await page.locator('textarea[name="monero_node_stagenet"]').getAttribute('data-tenant-count')) === 0) {
    // Run on its own: make a store on stagenet to protect.
    await page.goto(base + '/dashboard/connect');
    await page.locator('input[name="site_url"]').fill('https://guarded.example.com');
    await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
    await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
    await page.locator('select[name="network"]').selectOption('stagenet');
    await page.getByRole('button', { name: 'Connect' }).click();
    await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
    await page.goto(base + '/dashboard/admin/settings');
  }
  const stagenet = page.locator('textarea[name="monero_node_stagenet"]');
  const count = Number(await stagenet.getAttribute('data-tenant-count'));
  expect(count).toBeGreaterThan(0);
  const expected = count === 1 ? '1 store uses the stagenet network' : `${count} stores use the stagenet network`;
  let posts = 0;
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/dashboard/admin/scanner-settings')) posts += 1;
  });

  // Dismissed: nothing is sent.
  await stagenet.fill('');
  let asked = '';
  page.once('dialog', async (dialog) => { asked = dialog.message(); await dialog.dismiss(); });
  await page.getByRole('button', { name: 'Save engine settings' }).click();
  expect(asked).toContain(expected);
  await page.waitForTimeout(500);
  expect(posts).toBe(0);

  // Accepted: sent, and the red banner says what happened.
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Save engine settings' }).click();
  await expect(page.getByText(/the stagenet network, which no longer has any reachable nodes/)).toBeVisible();
  expect(posts).toBe(1);

  // Testnet has no stores: no question.
  let testnetAsked = false;
  page.once('dialog', async (dialog) => { testnetAsked = true; await dialog.accept(); });
  await page.locator('textarea[name="monero_node_testnet"]').fill('');
  await page.getByRole('button', { name: 'Save engine settings' }).click();
  await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
  expect(testnetAsked).toBe(false);
  expect(posts).toBe(2);

  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
  await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
});

for (const scheme of ['light', 'dark']) {
  for (const [name, size] of Object.entries(SIZES)) {
    test(`banners fit and the red one uses the error colour (${scheme}, ${name})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.setViewportSize(size);
      await signInAsAdmin(page);
      // A red banner (a network stores use cleared) and a yellow one (a
      // restart-only setting) on one page.
      await page.goto(fixture().monokulo_url + '/dashboard/admin/settings');
      const threads = Number(await page.locator('input[name="server.worker_threads"]').inputValue());
      page.once('dialog', (dialog) => dialog.accept());
      await saveEngineSettings(page, { monero_node_stagenet: '', 'server.worker_threads': String((threads % 8) + 1) });
      const red = page.locator('p.error[role="alert"]').first();
      await expect(red).toBeVisible();
      await expect(page.locator('p.warning').first()).toBeVisible();
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
      await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
      await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
    });
  }
}
