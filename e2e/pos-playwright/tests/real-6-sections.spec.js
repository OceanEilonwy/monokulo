// @ts-check
// Settings pages save one section at a time with fixi (structured_logging.md
// parts 6 and 9): the page isn't reloaded, the scroll position and unsaved
// edits elsewhere survive, and the saved section shows its own banner.
const { test, expect } = require('@playwright/test');
const { useRealStack, fixture, signInAsAdmin, fakeNodeJson, saveEngineSettings, VIEW_KEY, SPEND_PUBKEY } = require('./real-helpers');

useRealStack(test);

test('saving the engine half of the admin settings keeps edits in the monokulo half', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings`);
  await page.evaluate(() => { window.__notReloaded = true; });
  // An edit in the other section, not saved.
  const retention = page.locator('#monokulo-settings input[name="logging.retention_days"]');
  await retention.fill('21');

  const confirmations = page.locator('input[name="payment.confirmations_required"]');
  await confirmations.fill('7');
  const save = page.getByRole('button', { name: 'Save engine settings' });
  await save.scrollIntoViewIfNeeded();
  const scrolled = await page.evaluate(() => window.scrollY);
  expect(scrolled).toBeGreaterThan(0);
  await save.click();

  await expect(page.locator('#engine-settings').getByText('Engine settings saved and applied.')).toBeVisible();
  // Focus lands by the button that was pressed, without scrolling away.
  await expect(page.locator('#engine-settings .save-status')).toBeFocused();
  await expect(page.locator('#engine-settings .save-status')).toHaveText('Saved.');
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
  await expect(retention).toHaveValue('21');
  await expect(page.locator('input[name="payment.confirmations_required"]')).toHaveValue('7');
  expect(Math.abs((await page.evaluate(() => window.scrollY)) - scrolled)).toBeLessThan(50);
});

test('a store settings form that is refused shows why inside its own section', async ({ page }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  // A store needs its network to have a node (run on its own, this spec
  // starts from a fresh instance).
  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
  await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
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
