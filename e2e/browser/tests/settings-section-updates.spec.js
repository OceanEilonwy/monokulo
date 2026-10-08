// @ts-check
// Settings pages save one section at a time with fixi (structured_logging.md
// parts 6 and 9): the page isn't reloaded, the scroll position survives,
// and the saved section shows how it went. The admin settings page saves
// a tab all or nothing: its panel is swapped, the tab bar and page-wide
// banners come back whole, a toast says how it went, and a card a save
// refused stays red with what was typed.
const { test, expect } = require('@playwright/test');
const { createStore, useRealStack, fixture, signInAsAdmin, fakeNodeAddress, saveNodes, saveEngineSettings, openSettingsTab, finishStoreSetup, VIEW_KEY, SPEND_PUBKEY, expectSaved } = require('./backend-helpers');

useRealStack(test);

test('saving an admin settings tab swaps only its panel, and the tab bar and banners stay whole', async ({ page }) => {
  await signInAsAdmin(page);
  await page.setViewportSize({ width: 1280, height: 600 });
  await openSettingsTab(page, 'payments');
  await page.evaluate(() => { window.__notReloaded = true; });
  const tabLinks = page.locator('#settings-tabs a');
  const before = await tabLinks.evaluateAll((links) => links.map((a) => a.getAttribute('href')));

  // Nothing changed yet: no save bar.
  const bar = page.locator('#save-bar');
  await expect(bar).toBeHidden();
  const field = page.locator('input[name="webhooks.max_attempts"]');
  await field.scrollIntoViewIfNeeded();
  await field.fill('9');
  // The bar comes up at the bottom of the window, by the change, naming
  // its card; the card is marked.
  await expect(bar).toBeVisible();
  await expect(bar).toContainText('1 unsaved change in Webhooks');
  await expect(page.locator('#card-webhooks [data-unsaved]')).toHaveText('1 unsaved');
  await expect(page.locator('#card-webhooks .setting-field.is-changed .changed-mark')).toBeVisible();
  await expect(bar).toBeInViewport({ ratio: 1 });
  const scrolled = await page.evaluate(() => window.scrollY);
  expect(scrolled).toBeGreaterThan(0);
  await bar.getByRole('button', { name: 'Save', exact: true }).click();

  // A toast says what was saved; the card says when; the bar goes.
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Changes saved and applied');
  await expect(page.locator('#card-webhooks .card-saved')).toBeVisible();
  // Focus lands on what the save did, not lost with the hidden bar.
  await expect(page.locator('#card-webhooks .card-saved')).toBeFocused();
  await expect(bar).toBeHidden();
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
  await expect(field).toHaveValue('9');
  expect(Math.abs((await page.evaluate(() => window.scrollY)) - scrolled)).toBeLessThan(50);
  // One tab bar, one banners area, the same links, Payments still open.
  await expect(page.locator('#settings-tabs')).toHaveCount(1);
  await expect(page.locator('#settings-banners')).toHaveCount(1);
  expect(await tabLinks.evaluateAll((links) => links.map((a) => a.getAttribute('href')))).toEqual(before);
  await expect(page.locator('#settings-tabs a[aria-current="page"]')).toHaveText('Payments');

  // Saving again shows a new toast, not the old one left in place.
  await field.fill('10');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#settings-toasts .toast-success')).toHaveCount(1);
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Changes saved and applied');
  await expect(field).toHaveValue('10');
});

test('a value either check refuses saves nothing, and every change stays to fix or discard', async ({ page }) => {
  await signInAsAdmin(page);
  await openSettingsTab(page, 'server');
  await page.locator('input[name="server.cpus"]').fill('abc');
  await page.locator('input[name="http_cache.max_mb"]').fill('20');
  const bar = page.locator('#save-bar');
  await expect(bar).toContainText('2 unsaved changes in Monokulo and Engine');
  await bar.getByRole('button', { name: 'Save', exact: true }).click();

  // All or nothing: the engine refused its part, so monokulo's isn't saved
  // either.
  const toast = page.locator('#settings-toasts .toast-error');
  await expect(toast).toContainText('Nothing saved');
  await expect(toast).toContainText('server.cpus');
  await expect(page.locator('#card-server-engine')).toHaveClass(/is-failed/);
  await expect(page.locator('#card-server-engine .card-body > p.error')).toContainText('server.cpus');
  await expect(page.locator('input[name="server.cpus"]')).toHaveValue('abc');
  await expect(page.locator('#card-server-monokulo')).not.toHaveClass(/is-failed/);
  await expect(page.locator('#card-server-monokulo .card-saved')).toHaveCount(0);
  await expect(page.locator('input[name="http_cache.max_mb"]')).toHaveValue('20');
  // The bar is red and says why, with a link to the card.
  await expect(bar).toHaveClass(/is-failed/);
  await expect(bar).toContainText('Nothing saved.');
  // An error toast stays until it's closed: past the 12s at which site.css
  // fades one out on a page without JavaScript, it's still shown. Its
  // animations are moved on to 13s rather than waited out.
  await toast.evaluate(element => element.getAnimations().forEach(animation => { animation.currentTime = 13_000; }));
  await expect(toast).toBeVisible();
  await toast.getByRole('button', { name: 'Dismiss' }).click();
  await expect(toast).toHaveCount(0);

  // Both changes are still unsaved: each card discards its own.
  await page.locator('input[name="server.cpus"]').fill('');
  await expect(bar).toContainText('1 unsaved change in Monokulo');
  await page.locator('#card-server-monokulo').getByRole('button', { name: 'Discard' }).click();
  await expect(page.locator('input[name="http_cache.max_mb"]')).toHaveValue('16');
  await expect(bar).toBeHidden();
  await openSettingsTab(page, 'server');
  await expect(page.locator('input[name="http_cache.max_mb"]')).toHaveValue('16');
});

test('leaving a tab with unsaved changes asks first, and Save and go saves then goes', async ({ page }) => {
  await signInAsAdmin(page);
  await openSettingsTab(page, 'abuse');
  await page.locator('input[name="abuse.stream_cap"]').fill('17');
  await page.locator('#settings-tabs a', { hasText: 'Server' }).click();
  const bar = page.locator('#save-bar');
  await expect(bar).toContainText('Abuse protection has unsaved changes. Save or discard them before going to Server.');
  await expect(page.locator('#settings-panel h2')).toHaveText('Abuse protection');
  // Only the three choices: the bar's own Save and Discard step aside.
  await expect(bar.getByRole('button', { name: 'Save', exact: true })).toBeHidden();
  await expect(bar.getByRole('link', { name: 'Discard changes' })).toBeHidden();

  await bar.getByRole('button', { name: 'Stay here' }).click();
  await expect(bar).toContainText('1 unsaved change in Request limits');
  await expect(page.locator('input[name="abuse.stream_cap"]')).toHaveValue('17');

  await page.locator('#settings-tabs a', { hasText: 'Server' }).click();
  await bar.getByRole('button', { name: 'Save and go' }).click();
  await expect(page.locator('#settings-panel h2')).toHaveText('Server');
  await expect(page).toHaveURL(/\?tab=server$/);
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Changes saved and applied');
  await openSettingsTab(page, 'abuse');
  await expect(page.locator('input[name="abuse.stream_cap"]')).toHaveValue('17');

  // Save and go with a value the save refuses: it stays, and says why.
  await openSettingsTab(page, 'server');
  await page.locator('input[name="server.cpus"]').fill('abc');
  await page.locator('#settings-tabs a', { hasText: 'Logging' }).click();
  await bar.getByRole('button', { name: 'Save and go' }).click();
  await expect(bar).toHaveClass(/is-failed/);
  await expect(bar).toContainText('Nothing saved.');
  await page.waitForTimeout(500);
  await expect(page.locator('#settings-panel h2')).toHaveText('Server');
  await expect(page.locator('input[name="server.cpus"]')).toHaveValue('abc');
  await page.locator('#card-server-engine').getByRole('button', { name: 'Discard' }).click();
  await openSettingsTab(page, 'abuse');

  // Discard and go: nothing saved.
  await page.locator('input[name="abuse.stream_cap"]').fill('18');
  await page.locator('#settings-tabs a', { hasText: 'Logging' }).click();
  await bar.getByRole('button', { name: 'Discard and go' }).click();
  await expect(page.locator('#settings-panel h2')).toHaveText('Logging');
  await openSettingsTab(page, 'abuse');
  await expect(page.locator('input[name="abuse.stream_cap"]')).toHaveValue('17');
});

test('a store settings form that is refused shows why inside its own section', async ({ page }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  // A store needs its network to have a node (run on its own, this spec
  // starts from a fresh instance).
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await createStore(page, 'sections.example.com');
  await finishStoreSetup(page);
  await page.goto(base + '/dashboard');
  const store = await page.locator('tr', { hasText: 'sections.example.com' }).first().getByRole('link', { name: 'view →' }).getAttribute('href');
  await page.goto(base + store + '/settings');

  await page.evaluate(() => { window.__notReloaded = true; });
  await page.locator('#webhooks').getByRole('button', { name: 'Edit webhooks' }).click();
  const url = page.locator('#webhooks input[name="url"]');
  await url.fill('https://hooks.example.com/monokulo');
  await page.getByRole('button', { name: 'Add webhook' }).click();
  await expect(page.locator('#webhooks').getByRole('heading', { name: 'Webhook created' })).toBeVisible();

  await page.locator('#webhooks dialog').getByRole('button', { name: 'Close', exact: true }).click();
  const webhookContents = await page.locator('#webhooks').innerHTML();
  await page.locator('#confirmation-thresholds').getByRole('button', { name: 'Edit confirmation thresholds' }).click();
  await page.locator('input[name="confirmations_required"]').fill('abc');
  await page.locator('#confirmation-thresholds button[form="default-confirmations"]').click();
  await expect(page.locator('#confirmation-thresholds [role="alert"]')).toContainText('Enter a whole number');
  // The webhook section, including its dismissed secret, is untouched.
  expect(await page.locator('#webhooks').innerHTML()).toBe(webhookContents);
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
});
