// @ts-check
// The POS session timeline against the real binaries: a store opts in to
// diagnostics, a POS session goes through what a counter tablet does all day
// (orders with and without a note, one backgrounded and brought back, lost
// coverage, the app hidden and frozen by the browser, a reload), and the
// Logs page shows it as one timeline in the order the tablet recorded it.
// Also: nothing is recorded once the store opts out, and log reports past
// their limit are refused with a 429, never a challenge.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { fixture, signInAsAdmin, connectStore } = require('./real-helpers');

const desktop = { shapes: ['desktop'] };

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

/** Enters an XMR amount in piconero digits and charges, with an optional
 * note; returns the new order's id. */
async function charge(page, digits, note) {
  await page.keyboard.type(digits);
  if (note) await page.locator('#pos-reference').fill(note);
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.url().endsWith('/pos/orders') && r.request().method() === 'POST'),
    page.getByRole('button', { name: 'Charge' }).click(),
  ]);
  expect(response.ok()).toBeTruthy();
  await expect(page.locator('.pos-payment')).toBeVisible();
  return (await response.json()).order_id;
}

/** The page is hidden, frozen by the browser, resumed and shown again, as
 * when the merchant switches apps for a while. Headless Chrome can't hide a
 * page (a minimised window and a tab behind another stay "visible"), and it
 * only freezes hidden pages, so the browser's own events are sent, with the
 * state it would report. */
async function hideFreezeAndShow(page, ms) {
  const set = (state) => page.evaluate((state) => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => state });
    document.dispatchEvent(new Event('visibilitychange'));
  }, state);
  await set('hidden');
  await page.evaluate(() => document.dispatchEvent(new Event('freeze')));
  await page.waitForTimeout(ms);
  await page.evaluate(() => document.dispatchEvent(new Event('resume')));
  await set('visible');
}

test('a POS session reads as one timeline, from opting in to the last event', async ({ page, context }) => {
  test.setTimeout(3 * 60 * 1000);
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  const store = await connectStore(page, 'pos-timeline.example.com');

  // Diagnostics: off until the store turns it on.
  await page.goto(base + store + '/settings');
  const diagnostics = page.locator('#diagnostics');
  await expect(diagnostics).toContainText('This store sends no diagnostic logs.');
  await diagnostics.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-diagnostics-off', test.info(), { group: 'store-settings', ...desktop });
  await page.evaluate(() => { window.__notReloaded = true; });
  await diagnostics.getByRole('button', { name: 'Send diagnostic logs' }).click();
  await expect(diagnostics).toContainText('This store sends diagnostic logs.');
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
  await captureCoverageStage(page, 'store-diagnostics-on', test.info(), { group: 'store-settings', ...desktop });

  // The session.
  await page.goto(base + store + '/pos');
  await expect(page.locator('#pos-root')).toHaveAttribute('data-client-logging', 'true');
  await expect(page.locator('.pos-keypad')).toBeVisible();
  const session = await page.evaluate(() => JSON.parse(sessionStorage.getItem('monokulo-pos-timeline') || '{}').session);
  expect(session).toMatch(/^[0-9a-f-]{36}$/);

  const first = await charge(page, '500000000000', 'Table 4');
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toHaveCount(1);
  const second = await charge(page, '250000000000');

  // Coverage lost for a while, then back.
  await context.setOffline(true);
  await page.waitForTimeout(1500);
  await context.setOffline(false);
  await hideFreezeAndShow(page, 1200);

  // Back to the first order from the stack, and cancel it.
  await page.locator('.pos-stack-card').first().click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Table 4');
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Cancelled');
  await page.reload();
  await expect(page.locator('#pos-root')).toHaveAttribute('data-client-logging', 'true');

  // Everything recorded arrives, however it was held up.
  const timeline = `${base}/dashboard/admin/logs/pos/${session}`;
  const kinds = [
    'pos.opened', 'order.charge', 'order.created', 'order.backgrounded', 'network.offline', 'network.online',
    'page.hidden', 'page.frozen', 'page.resumed', 'page.visible', 'order.foregrounded', 'order.cancelled',
  ];
  await expect.poll(async () => {
    await page.goto(timeline);
    const shown = await page.locator('.timeline-event .kind').allTextContents();
    return kinds.every((kind) => shown.includes(kind)) && shown.filter((k) => k === 'pos.opened').length === 2;
  }, { timeout: 45_000, intervals: [2000] }).toBe(true);

  // In the tablet's order.
  const shown = await page.locator('.timeline-event .kind').allTextContents();
  const at = (kind, from = 0) => shown.indexOf(kind, from);
  for (let i = 1; i < kinds.length; i++) expect(at(kinds[i]), `${kinds[i - 1]} before ${kinds[i]}`).toBeGreaterThan(at(kinds[i - 1]));
  expect(shown.lastIndexOf('pos.opened')).toBeGreaterThan(at('order.cancelled'));
  await expect(page.locator('.timeline-event', { hasText: 'network.online' }).first()).toContainText(/offline for \d/);
  await expect(page.locator('.timeline-event', { hasText: 'page.visible' }).first()).toContainText(/hidden for \d/);
  // A note is recorded as given, never its text.
  await expect(page.locator('.timeline-event', { hasText: 'order.charge' }).first()).toContainText('has_note=true');
  await expect(page.locator('.pos-timeline')).not.toContainText('Table 4');
  await expect(page.locator('.timeline-event', { hasText: 'pos.opened' }).last()).toContainText('navigation=reload');
  await expect(page.locator('.timeline-event', { hasText: 'order.created' }).first().getByRole('link', { name: first })).toBeVisible();
  await expect(page.locator('.timeline-event', { hasText: 'order.created' }).last().getByRole('link', { name: second })).toBeVisible();
  await expect(page.locator('.timeline-summary')).toContainText('Orders created');
  await captureCoverageStage(page, 'pos-timeline-session', test.info(), { group: 'pos-timeline', ...desktop });

  // From the order's own page, and from a line in Logs.
  await page.goto(`${base}${store}/orders/${first}`);
  await page.getByRole('link', { name: 'POS session' }).click();
  await expect(page).toHaveURL(timeline);
  await page.goto(`${base}/dashboard/admin/logs?range=all&q=${encodeURIComponent(`pos.session = '${session}' and pos.kind = 'order.created'`)}`);
  const row = page.locator('#log-rows .log-row').first();
  await row.locator('summary').click();
  await expect(row.getByRole('link', { name: 'Show the POS session timeline' })).toBeVisible();
  await captureCoverageStage(page, 'logs-pos-line', test.info(), { group: 'logs', ...desktop });

  // Opted out: the POS records nothing and sends nothing.
  await page.goto(base + store + '/settings');
  await page.locator('#diagnostics').getByRole('button', { name: 'Turn off' }).click();
  await expect(page.locator('#diagnostics')).toContainText('This store sends no diagnostic logs.');
  const sent = [];
  page.on('request', (request) => { if (request.url().endsWith('/pos/logs')) sent.push(request.url()); });
  await page.goto(base + store + '/pos');
  await expect(page.locator('#pos-root')).toHaveAttribute('data-client-logging', 'false');
  await expect(page.locator('.pos-top')).toBeVisible();
  await page.waitForTimeout(6000);
  expect(sent).toHaveLength(0);
  const refused = await page.request.post(`${base}${store}/pos/logs`, {
    data: { session, events: [{ seq: 999, t: Date.now(), kind: 'page.hidden' }] },
  });
  expect(refused.status()).toBe(403);
});

test('log reports past their limit are dropped with a 429, never a challenge, and nothing else is held up', async ({ playwright }) => {
  const base = fixture().monokulo_url;
  // A client of its own (no session): anonymous, by address.
  const client = await playwright.request.newContext();
  const report = () => client.post(`${base}/telemetry/client`, {
    data: { kind: 'error', message: 'rate limit test', page: '/dashboard/admin/logs' },
  });
  let refused = null;
  for (let i = 0; i < 40 && !refused; i++) {
    const response = await report();
    if (response.status() === 429) refused = response;
    else expect(response.status()).toBe(204);
  }
  expect(refused, 'refused within 40 reports (abuse.client_logs_per_min is 30)').not.toBeNull();
  expect(refused.headers()['retry-after']).toMatch(/^\d+$/);
  expect(refused.headers()['monokulo-challenge']).toBeUndefined();
  // The same client's pages are unaffected.
  const status = await client.get(`${base}/status`);
  expect(status.status()).toBe(200);
  await client.dispose();
});
