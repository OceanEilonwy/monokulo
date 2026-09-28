// @ts-check
// The Logs page in a real browser against the real binaries
// (structured_logging.md parts 5 and 9): with JavaScript, searches and
// paging swap in place and keep the URL; without it, the same page works as
// plain forms and links. Lines from monokulo and the engine share traces.
const { test, expect } = require('@playwright/test');
const { fixture, signInAsAdmin } = require('./real-helpers');

const logsUrl = (query = '') => `${fixture().monokulo_url}/dashboard/admin/logs${query}`;

test('a search swaps the results in place, keeps the URL, and back returns to the earlier search', async ({ page }) => {
  await signInAsAdmin(page);
  // Something recent in both services: the settings page asks the engine.
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings`);
  await page.goto(logsUrl());
  await expect(page.locator('#log-rows .log-row').first()).toBeVisible();
  await expect(page.locator('.error', { hasText: "engine's lines aren't shown" })).toHaveCount(0);

  // A marker that only survives if the page is never reloaded.
  await page.evaluate(() => { window.__notReloaded = true; });
  await page.locator('#log-q').fill("service = 'scanner'");
  await page.locator('#log-q').press('Enter');
  await expect(page).toHaveURL(/q=service/);
  await expect(page.locator('#log-rows .log-row').first()).toBeVisible();
  for (const service of await page.locator('#log-rows .log-row .svc').allTextContents()) {
    expect(service).toBe('scanner');
  }
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);

  // A quick filter applies as soon as it changes.
  await page.locator('select[name="level"]').selectOption('warn');
  await expect(page).toHaveURL(/level=warn/);
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);

  await page.goBack();
  await expect(page).toHaveURL(/q=service/);
  await expect(page).not.toHaveURL(/level=warn/);
  await expect(page.locator('select[name="level"]')).toHaveValue('');
});

test('a request from monokulo to the engine is one trace with spans from both', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings`);
  await page.goto(logsUrl(`?q=${encodeURIComponent("http.route = '/dashboard/admin/settings'")}`));
  const row = page.locator('#log-rows .log-row').first();
  await row.locator('summary').click();
  await row.getByRole('link', { name: 'Show the whole trace' }).click();
  await expect(page.getByRole('heading', { name: /Trace/ })).toBeVisible();
  const spans = page.locator('.trace-span');
  await expect(spans.filter({ hasText: 'monokulo' }).first()).toBeVisible();
  await expect(spans.filter({ hasText: 'scanner' }).first()).toBeVisible();
});

test('find narrows the search to a property of a line', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(logsUrl(`?q=${encodeURIComponent("has http.route")}`));
  const row = page.locator('#log-rows .log-row').first();
  await row.locator('summary').click();
  const property = row.locator('tr', { has: page.locator('th', { hasText: /^http\.route$/ }) });
  const value = (await property.locator('td code').textContent()) || '';
  await property.getByRole('link', { name: 'Find' }).click();
  await expect(page).toHaveURL(/http\.route/);
  await expect(page.locator('#log-q')).toHaveValue(`has http.route and http.route = '${value}'`);
});

test('live adds new lines at the top without a reload, and pauses', async ({ page, request }) => {
  await signInAsAdmin(page);
  await page.goto(logsUrl(`?q=${encodeURIComponent("url.path = '/status'")}`));
  await page.evaluate(() => { window.__notReloaded = true; });
  const before = await page.locator('#log-rows .log-row').count();
  const live = page.locator('#log-live');
  await live.click();
  await expect(live).toHaveText('Pause');
  await request.get(`${fixture().monokulo_url}/status`);
  await expect(page.locator('#log-rows .log-row')).toHaveCount(before + 1, { timeout: 15_000 });
  await live.click();
  await expect(live).toHaveText('Live');
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
});

test.describe('without JavaScript', () => {
  test.use({ javaScriptEnabled: false });

  test('the search form and Refresh work as plain navigation, and lines expand', async ({ page }) => {
    await signInAsAdmin(page);
    await page.goto(logsUrl());
    await expect(page.locator('#log-live')).toBeHidden();
    await page.locator('#log-q').fill("service = 'monokulo'");
    // Enter, not the button: headless Chrome keeps the box's suggestion
    // list open over the next page after a click.
    await page.locator('#log-q').press('Enter');
    await expect(page).toHaveURL(/q=service/);
    for (const service of await page.locator('#log-rows .log-row .svc').allTextContents()) {
      expect(service).toBe('monokulo');
    }
    // Refresh is a plain link to the same search. (Followed with goto:
    // headless Chrome with JavaScript off hit-tests <html> instead of the
    // page for a while after a form submission, so clicks don't land.)
    const refresh = await page.getByRole('link', { name: 'Refresh' }).getAttribute('href');
    expect(refresh).toContain("q=service+%3D+%27monokulo%27");
    await page.goto(fixture().monokulo_url + refresh);
    await expect(page).toHaveURL(/q=service/);
    // A line expands without script.
    const row = page.locator('#log-rows .log-row').first();
    await row.locator('summary').click();
    await expect(row.locator('.props')).toBeVisible();
  });
});
