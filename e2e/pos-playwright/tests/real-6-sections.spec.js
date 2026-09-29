// @ts-check
// Settings pages save one section at a time with fixi (structured_logging.md
// parts 6 and 9): the page isn't reloaded, the scroll position survives,
// and the saved section shows how it went. The admin settings page saves
// one tab at a time (nicer_admin_screen.md): its panel is swapped, and the
// tab bar and page-wide banners come back whole.
const { test, expect } = require('@playwright/test');
const { useRealStack, fixture, signInAsAdmin, fakeNodeJson, saveEngineSettings, openSettingsTab, VIEW_KEY, SPEND_PUBKEY } = require('./real-helpers');

useRealStack(test);

test('saving an admin settings tab swaps only its panel, and the tab bar and banners stay whole', async ({ page }) => {
  await signInAsAdmin(page);
  await page.setViewportSize({ width: 1280, height: 600 });
  await openSettingsTab(page, 'payments');
  await page.evaluate(() => { window.__notReloaded = true; });
  const tabLinks = page.locator('#settings-tabs a');
  const before = await tabLinks.evaluateAll((links) => links.map((a) => a.getAttribute('href')));

  await page.locator('input[name="payment.confirmations_required"]').fill('7');
  const save = page.locator('#settings-panel').getByRole('button', { name: 'Save', exact: true });
  await save.scrollIntoViewIfNeeded();
  const scrolled = await page.evaluate(() => window.scrollY);
  expect(scrolled).toBeGreaterThan(0);
  await save.click();

  await expect(page.locator('#settings-banners').getByText('Settings saved and applied.')).toBeVisible();
  // Focus lands by the button that was pressed, without scrolling away.
  await expect(page.locator('#settings-panel .save-status')).toBeFocused();
  await expect(page.locator('#settings-panel .save-status')).toHaveText('Saved.');
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
  await expect(page.locator('input[name="payment.confirmations_required"]')).toHaveValue('7');
  expect(Math.abs((await page.evaluate(() => window.scrollY)) - scrolled)).toBeLessThan(50);
  // One tab bar, one banners area, the same links, Payments still open.
  await expect(page.locator('#settings-tabs')).toHaveCount(1);
  await expect(page.locator('#settings-banners')).toHaveCount(1);
  expect(await tabLinks.evaluateAll((links) => links.map((a) => a.getAttribute('href')))).toEqual(before);
  await expect(page.locator('#settings-tabs a[aria-current="page"]')).toHaveText('Payments');
});

test('a store settings form that is refused shows why inside its own section', async ({ page }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  // A store needs its network to have a node (run on its own, this spec
  // starts from a fresh instance).
  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
  await expect(page.getByText('Settings saved and applied.')).toBeVisible();
  await page.goto(base + '/dashboard/connect');
  await page.locator('input[name="site_url"]').fill('https://sections.example.com');
  await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
  await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
  await page.locator('select[name="network"]').selectOption('stagenet');
  await page.getByRole('button', { name: 'Connect' }).click();
  await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
  await page.goto(base + '/dashboard');
  const store = await page.locator('tr', { hasText: 'sections.example.com' }).first().getByRole('link', { name: 'view →' }).getAttribute('href');
  await page.goto(base + store + '/settings');

  await page.evaluate(() => { window.__notReloaded = true; });
  const url = page.locator('#webhooks input[name="url"]');
  await url.fill('https://hooks.example.com/monokulo');
  await page.getByRole('button', { name: 'Add webhook' }).click();
  await expect(page.locator('#webhooks').getByRole('heading', { name: 'Webhook created' })).toBeVisible();

  await page.locator('input[name="confirmations_required"]').fill('abc');
  await page.locator('#confirmation-thresholds button[form="default-confirmations"]').click();
  await expect(page.locator('#confirmation-thresholds [role="alert"]')).toContainText('Enter a whole number');
  // The webhook section, swapped earlier, is untouched by this one.
  await expect(page.locator('#webhooks').getByRole('heading', { name: 'Webhook created' })).toBeVisible();
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
});
