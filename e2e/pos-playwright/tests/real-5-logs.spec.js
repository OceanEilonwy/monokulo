// @ts-check
// The Logs page in a real browser against the real binaries
// (structured_logging.md parts 5 and 9): with JavaScript, searches and
// paging swap in place and keep the URL; without it, the same page works as
// plain forms and links. Lines from monokulo and the engine share traces.
// Its stages go in the coverage gallery's Logs group, in every shape.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { fixture, signInAsAdmin, fakeNodeJson, saveEngineSettings, VIEW_KEY, SPEND_PUBKEY } = require('./real-helpers');

const logsUrl = (query = '') => `${fixture().monokulo_url}/dashboard/admin/logs${query}`;
const stage = (page, name) => captureCoverageStage(page, name, test.info(), { group: 'logs' });

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

test('a search swaps the results in place, keeps the URL, and back returns to the earlier search', async ({ page }) => {
  await signInAsAdmin(page);
  // Something recent in both services: the settings page asks the engine.
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings`);
  await page.goto(logsUrl());
  await expect(page.locator('#log-rows .log-row').first()).toBeVisible();
  await expect(page.locator('.error', { hasText: "engine's lines aren't shown" })).toHaveCount(0);
  await stage(page, 'logs-search');

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

test("a line's properties load when it's first opened, and only then", async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings`);
  await page.goto(logsUrl(`?q=${encodeURIComponent("has http.route")}`));
  const row = page.locator('#log-rows .log-row').first();
  // Closed, with no properties in the page: only a link to them.
  await expect(row).not.toHaveAttribute('open', '');
  await expect(row.locator('.props table')).toHaveCount(0);
  const loads = [];
  page.on('request', (request) => { if (request.url().includes('/dashboard/admin/logs/row/')) loads.push(request.url()); });

  await row.locator('summary').click();
  await expect(row.locator('.props table')).toBeVisible();
  await expect(row.locator('.props th', { hasText: /^http\.route$/ })).toBeVisible();
  expect(loads).toHaveLength(1);
  await stage(page, 'logs-line-opened');

  // Closed and opened again: already there, not fetched again.
  await row.locator('summary').click();
  await row.locator('summary').click();
  await expect(row.locator('.props table')).toBeVisible();
  expect(loads).toHaveLength(1);
});

test('a search that does not parse says where', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(logsUrl());
  await page.locator('#log-q').fill("level >= and service = ");
  await page.locator('#log-q').press('Enter');
  await expect(page.locator('.query-error')).toBeVisible();
  await expect(page.locator('.query-error mark')).toBeVisible();
  await stage(page, 'logs-query-error');
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
  await stage(page, 'logs-trace');
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
  await stage(page, 'logs-live');
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
    await stage(page, 'logs-search-no-js');
    // A line opens without script to a link to its properties' page.
    const row = page.locator('#log-rows .log-row').first();
    await row.locator('summary').click();
    const link = row.locator('.props a', { hasText: "Show this line's properties" });
    await expect(link).toBeVisible();
    await page.goto(fixture().monokulo_url + (await link.getAttribute('href')));
    await expect(page.getByRole('heading', { name: 'Log line' })).toBeVisible();
    await expect(page.locator('.log-row[open] .props table')).toBeVisible();
    await expect(page.locator('.props th', { hasText: /^target$/ })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Back to the search' })).toHaveAttribute('href', /q=service/);
    await stage(page, 'logs-line-page-no-js');
  });
});

test("a caller's traceparent (as the WooCommerce plugin sends it) is followed through monokulo to the engine", async ({ page, request }) => {
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  await saveEngineSettings(page, { monero_node_stagenet: fakeNodeJson() });
  await expect(page.getByText('Engine settings saved and applied.')).toBeVisible();
  await page.goto(base + '/dashboard/connect');
  await page.locator('input[name="site_url"]').fill('https://traced.example.com');
  await page.locator('input[name="view_key_hex"]').fill(VIEW_KEY);
  await page.locator('input[name="spend_pubkey_hex"]').fill(SPEND_PUBKEY);
  await page.locator('select[name="network"]').selectOption('stagenet');
  await page.getByRole('button', { name: 'Connect' }).click();
  await expect(page.getByRole('heading', { name: 'Store connected' })).toBeVisible();
  await page.goto(base + '/dashboard');
  const store = await page.locator('tr', { hasText: 'traced.example.com' }).first().getByRole('link', { name: 'view →' }).getAttribute('href');
  await page.goto(base + store);
  const publicKey = (await page.locator('tr', { hasText: 'Public key' }).locator('code').textContent()).trim();

  // What class-wc-gateway-monokulo.php sends: its own trace, a new span id.
  const traceId = [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, '0')).join('');
  const response = await request.post(`${base}/pay/${publicKey}/orders`, {
    headers: { traceparent: `00-${traceId}-00f067aa0ba902b7-01` },
    data: { amount: '0.5', currency: 'XMR' },
  });
  expect(response.ok()).toBeTruthy();
  expect(response.headers()['traceresponse']).toContain(traceId);

  await expect
    .poll(async () => {
      await page.goto(`${base}/dashboard/admin/logs/trace/${traceId}`);
      const spans = await page.locator('.trace-span').allTextContents();
      return spans.some((s) => s.includes('monokulo')) && spans.some((s) => s.includes('scanner'));
    }, { timeout: 15_000, intervals: [500] })
    .toBe(true);
});
