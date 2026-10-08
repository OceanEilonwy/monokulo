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
});

test('the search box, Syntax and Search share one height; Syntax opens the help as a dialog', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(logsUrl());
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 800 });
    const boxes = await page.locator('.logs-search .q-row > :is(input, a, button)').evaluateAll(els => els.map(el => {
      const r = el.getBoundingClientRect();
      return { top: Math.round(r.top), height: Math.round(r.height) };
    }));
    expect(boxes, `${width}px`).toHaveLength(3);
    for (const box of boxes) expect(box, `${width}px`).toEqual(boxes[0]);
  }
  await page.setViewportSize({ width: 1280, height: 800 });

  // With script, Syntax is a dialog, not the page it links to.
  await page.getByRole('link', { name: 'Syntax' }).click();
  const help = page.getByRole('dialog', { name: 'Search syntax' });
  await expect(help).toBeVisible();
  await expect(page).not.toHaveURL(/syntax/);
  await stage(page, 'logs-syntax-dialog');
  // Use puts its example in the box and closes.
  await help.locator('li', { hasText: "message contains 'timeout'" }).getByRole('link', { name: 'Use' }).click();
  await expect(help).toBeHidden();
  await expect(page.locator('#log-q')).toHaveValue("message contains 'timeout'");
  await expect(page.locator('#log-q')).toBeFocused();
  // A name chip adds itself; Escape closes.
  await page.getByRole('link', { name: 'Syntax' }).click();
  await help.getByRole('button', { name: 'level', exact: true }).click();
  await expect(page.locator('#log-q')).toHaveValue("message contains 'timeout' and level ");
  await page.getByRole('link', { name: 'Syntax' }).click();
  await page.keyboard.press('Escape');
  await expect(help).toBeHidden();
});

test('without JavaScript, Syntax is a page whose examples are searches', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await signInAsAdmin(page);
    await page.goto(logsUrl());
    await page.getByRole('link', { name: 'Syntax' }).click();
    await expect(page).toHaveURL(/\/dashboard\/admin\/logs\/syntax$/);
    await expect(page.getByRole('heading', { name: 'Search syntax', level: 1 })).toBeVisible();
    await stage(page, 'logs-syntax-page');
    await transitionDone(page);
    await page.locator('li').filter({ has: page.getByText('level >= warn', { exact: true }) }).getByRole('link', { name: 'Use' }).click();
    await expect(page.locator('#log-q')).toHaveValue('level >= warn');
  } finally { await context.close(); }
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
  await expect(spans.filter({ hasText: 'engine' }).first()).toBeVisible();
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
  // Only this test's request: the stack is shared, and other workers'
  // tests fetch /status too. Only monokulo's line: when its status cache
  // is cold it asks the engine, whose own /status line shares the trace.
  const traceId = [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, '0')).join('');
  await page.goto(logsUrl(`?q=${encodeURIComponent(`service = 'monokulo' and url.path = '/status' and trace_id = '${traceId}'`)}`));
  await page.evaluate(() => { window.__notReloaded = true; });
  const before = await page.locator('#log-rows .log-row').count();
  const live = page.locator('#log-live');
  await live.click();
  // At once, not when the stream first has something to say.
  await expect(live).toHaveText('Pause', { timeout: 1_000 });
  await expect(live).toHaveAttribute('aria-pressed', 'true');
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
    const refresh = page.locator('.logs-head').getByRole('button', { name: 'Refresh' });
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
      return spans.some((s) => s.includes('monokulo')) && spans.some((s) => s.includes('engine'));
    }, { timeout: 15_000, intervals: [500] })
    .toBe(true);
});

test('Refresh and Live sit in the title bar, the histogram above the search, and every line has trace and session buttons', async ({ page }) => {
  await signInAsAdmin(page);
  await page.goto(`${fixture().monokulo_url}/dashboard/admin/settings`);
  await page.goto(logsUrl(`?q=${encodeURIComponent("http.route = '/dashboard/admin/settings'")}`));
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 800 });
    const head = page.locator('.logs-head');
    const title = await head.getByRole('heading', { name: 'Logs', level: 1 }).boundingBox();
    for (const name of ['Refresh', 'Live']) {
      const button = head.getByRole('button', { name });
      await expect(button.locator('svg').first(), `${name} has an icon`).toBeVisible();
      const box = await button.boundingBox();
      expect(Math.abs((box.y + box.height / 2) - (title.y + title.height / 2)), `${name} beside the title at ${width}px`).toBeLessThan(title.height / 2);
    }
    await expect(page.locator('.hint', { hasText: 'Times in' })).toHaveCount(0);
    const histogram = await page.locator('.log-histogram').boundingBox();
    const search = await page.locator('#log-search').boundingBox();
    expect(histogram.y + histogram.height, `histogram above the search at ${width}px`).toBeLessThanOrEqual(search.y);
  }
  await page.setViewportSize({ width: 1280, height: 800 });

  // A line of a signed-in request: its trace and its session, without
  // opening it.
  const row = page.locator('#log-rows .log-row').first();
  await expect(row.getByRole('link', { name: 'Show trace' })).toBeVisible();
  const session = row.getByRole('link', { name: 'Show session' });
  await expect(session).toBeVisible();
  await expect(row).not.toHaveAttribute('open', '');

  // Opened: who it was for first, and Find/Exclude on one line each, even
  // beside a long value.
  await row.locator('summary').click();
  const props = row.locator('.props table');
  await expect(props).toBeVisible();
  const names = await props.locator('th').allTextContents();
  expect(names.slice(0, 5)).toEqual(['target', 'level', 'service', 'session.id', 'user.id']);
  await expect(props.locator('tr', { has: page.locator('th', { hasText: /^user\.id$/ }) })).toContainText('@');
  for (const cell of await props.locator('td.act').all()) {
    const links = cell.locator('a');
    if (await links.count() < 2) continue;
    const [find, exclude] = [await links.nth(0).boundingBox(), await links.nth(1).boundingBox()];
    expect(Math.round(exclude.y), 'Exclude beside Find, not under it').toBe(Math.round(find.y));
  }
  await stage(page, 'logs-line-who');

  // The session: every line of it, and only it.
  await session.click();
  await expect(page).toHaveURL(/q=session\.id/);
  await expect(page.locator('#log-rows .log-row').first()).toBeVisible();
  const sessionId = (await page.locator('#log-q').inputValue()).match(/'([0-9a-f]+)'/)[1];
  const first = page.locator('#log-rows .log-row').first();
  await first.locator('summary').click();
  await expect(first.locator('tr', { has: page.locator('th', { hasText: /^session\.id$/ }) }).locator('code')).toHaveText(sessionId);
  await stage(page, 'logs-session');
});

