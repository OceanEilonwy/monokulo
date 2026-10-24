// @ts-check
// The admin settings page saves with fixi (structured_logging.md parts 6
// and 9): the page isn't reloaded, the scroll position survives, and the
// saved cards show how it went. It saves
// a tab all or nothing: its panel is swapped, the tab bar and page-wide
// banners come back whole, a toast says how it went, and a card a save
// refused stays red with what was typed.
const { test, expect } = require('@playwright/test');
const { useRealStack, signInAsAdmin, openSettingsTab } = require('./backend-helpers');

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
  // Hold the response so feedback is observable even on a fast local save.
  const action = await page.locator('#settings-form').getAttribute('action');
  const saveUrl = new URL(action, page.url()).href;
  let releaseSave;
  const held = new Promise(resolve => { releaseSave = resolve; });
  let submissions = 0;
  await page.route(saveUrl, async route => {
    submissions++;
    await held;
    await route.continue();
  });
  await bar.getByRole('button', { name: 'Save', exact: true }).click();
  const saving = bar.getByRole('button', { name: 'Saving…', exact: true });
  try {
    await expect(saving).toBeDisabled();
    await expect(saving).toHaveAttribute('aria-busy', 'true');
    expect(await saving.evaluate(button => getComputedStyle(button, '::before').animationName)).toBe('save-spin');
    // Enter/requestSubmit must not send another request while Save is disabled.
    await page.locator('#settings-form').evaluate(form => form.requestSubmit());
    await expect.poll(() => submissions).toBe(1);
  } finally {
    releaseSave();
  }

  // A toast says what was saved; the card says when; the bar goes.
  await expect(page.locator('#settings-toasts .toast-success')).toContainText('Changes saved and applied');
  await expect(page.locator('#card-webhooks .card-saved')).toBeVisible();
  // Focus lands on what the save did, not lost with the hidden bar.
  await expect(page.locator('#card-webhooks .card-saved')).toBeFocused();
  await expect(bar).toBeHidden();
  await expect(bar.locator('[data-save]')).toBeEnabled();
  await expect(bar.locator('[data-save]')).toHaveText('Save');
  await expect(bar.locator('[data-save]')).not.toHaveAttribute('aria-busy');
  await page.unroute(saveUrl);
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

  // A value either check refuses saves nothing, and every change stays to
  // fix or discard.
  await openSettingsTab(page, 'server');
  await page.locator('input[name="server.cpus"]').fill('abc');
  await page.locator('input[name="http_cache.max_mb"]').fill('20');
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
  await expect(bar.getByRole('button', { name: 'Save', exact: true })).toBeEnabled();
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

test('a http or network failure restores Save and allows retry', async ({ page }) => {
  await signInAsAdmin(page);
  for (const failure of ['http', 'network']) {
    await openSettingsTab(page, 'payments');
    const value = failure === 'http' ? '11' : '12';
    await page.locator('input[name="webhooks.max_attempts"]').fill(value);
    const action = await page.locator('#settings-form').getAttribute('action');
    const saveUrl = new URL(action, page.url()).href;
    let releaseSave;
    const held = new Promise(resolve => { releaseSave = resolve; });
    await page.route(saveUrl, async route => {
      await held;
      if (failure === 'network') await route.abort('failed');
      else await route.fulfill({ status: 500, body: 'Save failed' });
    });
    const save = page.locator('#save-bar [data-save]');
    await save.click();
    try {
      await expect(save).toBeDisabled();
      await expect(save).toHaveText('Saving…');
    } finally {
      releaseSave();
    }
    await expect(page.locator('#fx-error')).toBeVisible();
    await expect(save).toBeEnabled();
    await expect(save).toHaveText('Save');
    await expect(save).not.toHaveAttribute('aria-busy');
    await expect(page.locator('input[name="webhooks.max_attempts"]')).toHaveValue(value);
    await page.unroute(saveUrl);
    await save.click();
    await expect(page.locator('#settings-toasts .toast-success')).toContainText('Changes saved and applied');
  }
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
  const leaving = [];
  page.on('request', (request) => { if (request.url().includes('tab=logging')) leaving.push(request.url()); });
  await page.locator('input[name="server.cpus"]').fill('abc');
  await page.locator('#settings-tabs a', { hasText: 'Logging' }).click();
  await bar.getByRole('button', { name: 'Save and go' }).click();
  await expect(bar).toHaveClass(/is-failed/);
  await expect(bar).toContainText('Nothing saved.');
  await expect(page.locator('#settings-panel h2')).toHaveText('Server');
  await expect(page.locator('input[name="server.cpus"]')).toHaveValue('abc');
  await page.locator('#card-server-engine').getByRole('button', { name: 'Discard' }).click();
  await openSettingsTab(page, 'abuse');
  // It never went: the page asked for Logging neither before the refusal
  // nor after it, or that request would have gone out before this page's.
  expect(leaving).toEqual([]);

  // Discard and go: nothing saved.
  await page.locator('input[name="abuse.stream_cap"]').fill('18');
  await page.locator('#settings-tabs a', { hasText: 'Logging' }).click();
  await bar.getByRole('button', { name: 'Discard and go' }).click();
  await expect(page.locator('#settings-panel h2')).toHaveText('Logging');
  await openSettingsTab(page, 'abuse');
  await expect(page.locator('input[name="abuse.stream_cap"]')).toHaveValue('17');
});
