// @ts-check
// The Logs page in a real browser against the real binaries
// (structured_logging.md parts 5 and 9): with JavaScript, searches and
// paging swap in place and keep the URL; without it, the same page works as
// plain forms and links. Lines from monokulo and the engine share traces.
// Its stages go in the coverage gallery's Logs group, in every shape.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { createStore, useRealStack, fixture, signInAsAdmin, transitionDone, fakeNodeAddress, saveNodes, saveEngineSettings, VIEW_KEY, SPEND_PUBKEY, expectSaved } = require('./backend-helpers');

useRealStack(test);

const logsUrl = (query = '') => `${fixture().monokulo_url}/admin/logs${query}`;
const stage = (page, name) => captureCoverageStage(page, name, test.info(), { group: 'logs' });

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

test('a search swaps the results in place, keeps the URL, and back returns to the earlier search', async ({ page }) => {
  await signInAsAdmin(page);
  // Something recent in both services: the settings page asks the engine.
  await page.goto(`${fixture().monokulo_url}/admin/settings`);
  await page.goto(logsUrl());
  await expect(page.locator('#log-rows .log-row').first()).toBeVisible();
  await expect(page.locator('.error', { hasText: "engine's lines aren't shown" })).toHaveCount(0);
  await stage(page, 'logs-search');

  // A marker that only survives if the page is never reloaded.
  await page.evaluate(() => { window.__notReloaded = true; });
  await page.locator('#log-q').fill("service = 'engine'");
  await page.locator('#log-q').press('Enter');
  await expect(page).toHaveURL(/q=service/);
  await expect(page.locator('#log-rows .log-row').first()).toBeVisible();
  for (const service of await page.locator('#log-rows .log-row .svc').allTextContents()) {
    expect(service).toBe('engine');
  }
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);

  // A quick filter applies as soon as it changes: chosen from the
  // compact dropdown, which keeps the select's value.
  await page.getByRole('combobox', { name: 'Level' }).click();
  await page.getByRole('option', { name: 'Warnings and errors' }).click();
  await expect(page).toHaveURL(/level=warn/);
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);

  await page.goBack();
  await expect(page).toHaveURL(/q=service/);
  await expect(page).not.toHaveURL(/level=warn/);
  await expect(page.locator('select[name="level"]')).toHaveValue('');

  // A search that does not parse says where.
  await page.locator('#log-q').fill("level >= and service = ");
  await page.locator('#log-q').press('Enter');
  await expect(page.locator('[data-query-error]')).toBeVisible();
  await expect(page.locator('[data-query-error] mark')).toBeVisible();
  await stage(page, 'logs-query-error');

  // Find narrows the search to a property of a line.
  await page.goto(logsUrl(`?q=${encodeURIComponent("has http.route")}`));
  const row = page.locator('#log-rows .log-row').first();
  await row.locator('summary').click();
  const property = row.locator('tr', { has: page.locator('th', { hasText: /^http\.route$/ }) });
  const value = (await property.locator('td code').textContent()) || '';
  await property.getByRole('link', { name: 'Find' }).click();
  await expect(page).toHaveURL(/http\.route/);
  await expect(page.locator('#log-q')).toHaveValue(`has http.route and http.route = '${value}'`);
});

test('a request from monokulo to the engine is one trace with spans from both', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(`${fixture().monokulo_url}/admin/settings`);
  await page.goto(logsUrl(`?q=${encodeURIComponent("http.route = '/admin/settings'")}`));
  const row = page.locator('#log-rows .log-row').first();
  await row.locator('summary').click();
  await row.getByRole('link', { name: 'Show the whole trace' }).click();
  await expect(page.getByRole('heading', { name: /Trace/ })).toBeVisible();
  const spans = page.locator('.trace-span');
  await expect(spans.filter({ hasText: 'monokulo' }).first()).toBeVisible();
  await expect(spans.filter({ hasText: 'engine' }).first()).toBeVisible();
  await stage(page, 'logs-trace');
});

test('live adds new lines at the top without a reload, and pauses', async ({ page, request }) => {
  await signInAsAdmin(page);
  // Only this test's request: the stack is shared, and other workers'
  // tests fetch /status too. Only monokulo's line: when its status cache
  // is cold it asks the engine, whose own /status line shares the trace.
  const traceId = [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, '0')).join('');
  await page.goto(logsUrl(`?q=${encodeURIComponent(`service = 'monokulo' and url.path = '/status' and trace_id = '${traceId}'`)}`));
  await page.evaluate(() => { window.__notReloaded = true; });
  const before = await page.locator('#log-rows .log-row').count();
  const live = page.locator('#log-live');
  // The stream is held back until the button has been checked: Pause
  // shows at once, not when the stream first answers.
  const tail = new URL(await page.locator('#log-rows').getAttribute('data-tail-url'), fixture().monokulo_url).href;
  let release;
  const held = new Promise((resolve) => { release = resolve; });
  await page.route((url) => url.href === tail, async (route) => { await held; await route.continue(); });
  await live.click();
  await expect(live).toHaveText('Pause');
  await expect(live).toHaveAttribute('aria-pressed', 'true');
  release();
  await request.get(`${fixture().monokulo_url}/status`, { headers: { traceparent: `00-${traceId}-00f067aa0ba902b7-01` } });
  await expect(page.locator('#log-rows .log-row')).toHaveCount(before + 1, { timeout: 15_000 });
  await expect(page.getByText('No lines match.')).toBeHidden();
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
    // Refresh submits the search as it stands: a plain GET.
    const refresh = page.locator('.mk-page-header').getByRole('button', { name: 'Refresh' });
    await expect(refresh).toHaveAttribute('form', 'log-search');
    await transitionDone(page);
    await refresh.click();
    await expect(page).toHaveURL(/q=service\+%3D\+%27monokulo%27/);
    await stage(page, 'logs-search-no-js');
    // A line opens without script to a link to its properties' page.
    const row = page.locator('#log-rows .log-row').first();
    await transitionDone(page);
    await row.locator('summary').click();
    const link = row.locator('.props a', { hasText: "Show this line's properties" });
    await expect(link).toBeVisible();
    await link.click();
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
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);
  await createStore(page, 'traced.example.com');
  await expect(page.getByRole('heading', { name: 'traced.example.com is set up' })).toBeVisible();
  await page.goto(base + '/');
  const store = await page.locator('[data-store]', { hasText: 'traced.example.com' }).first().locator('a[data-store-link]').getAttribute('href');
  await page.goto(base + store);
  const publicKey = (await page.locator('tr', { hasText: 'Public key' }).locator('code').textContent()).trim();

  // What class-wc-gateway-monokulo.php sends: its own trace, a new span id.
  const traceId = [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, '0')).join('');
  const response = await request.post(`${base}/api/v1/orders`, {
    headers: { authorization: `Bearer ${publicKey}`, traceparent: `00-${traceId}-00f067aa0ba902b7-01` },
    data: { amount: '0.5', currency: 'XMR' },
  });
  expect(response.status()).toBe(201);
  expect(response.headers()['traceresponse']).toContain(traceId);

  await expect
    .poll(async () => {
      await page.goto(`${base}/admin/logs/trace/${traceId}`);
      const spans = await page.locator('.trace-span').allTextContents();
      return spans.some((s) => s.includes('monokulo')) && spans.some((s) => s.includes('engine'));
    }, { timeout: 15_000, intervals: [500] })
    .toBe(true);
});

