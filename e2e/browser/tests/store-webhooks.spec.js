// @ts-check
// A store's webhooks (docs/design/user-testing/webhooks.html, variation 2):
// one health line per webhook, its recent deliveries folded under it and
// open by themselves while one is retrying or gave up, "Send again" and
// "Retry failed (N)" as plain forms, and a delivery's detail as a dialog,
// or a page without JavaScript. Captures each state for the gallery (every
// capture is taken on a phone too).
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const GROUP = 'store-settings';
let fixture;
test.describe.configure({ mode: 'serial' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });

async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}
const settings = () => `${fixture.base_url}/stores/${fixture.connection_id}/settings`;
/** The design's three webhooks (delivering, retrying, gave up), or none. */
async function seed(empty = false) {
  const response = await fetch(`${fixture.base_url}/__coverage/webhooks${empty ? '?empty' : ''}`, { method: 'POST' });
  expect(response.ok).toBe(true);
  return (await response.json()).deliveries;
}
const card = (page) => page.locator('#card-webhooks');
/** Every row's Details at one x, and its second slot at another. */
async function aligned(hook) {
  const rows = hook.locator('table[data-table="deliveries"] tbody tr');
  const xs = [];
  for (let i = 0; i < await rows.count(); i++) {
    const details = await rows.nth(i).getByRole('link', { name: 'Details' }).boundingBox();
    const slot = await rows.nth(i).locator('.act-slots > :nth-child(2)').boundingBox();
    xs.push([Math.round(details.x), Math.round(slot.x)]);
  }
  expect(new Set(xs.map(([d]) => d)).size, JSON.stringify(xs)).toBe(1);
  expect(new Set(xs.map(([, s]) => s)).size, JSON.stringify(xs)).toBe(1);
}
const webhook = (page, url) => card(page).locator('.wh').filter({ has: page.locator('.wh-url', { hasText: url }) });

test('each webhook says how it is doing; the deliveries of one that needs you are open', async ({ page, context }) => {
  await login(context);
  await seed();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  await expect(card(page).locator('.card-meta')).toHaveText('3 webhooks');

  const healthy = webhook(page, 'https://bakery.example/hooks/monokulo');
  await expect(healthy.locator('.wh-health .tag')).toHaveText('delivering');
  await expect(healthy.locator('.wh-health .hint')).toContainText('last 2 min ago · 200 in');
  await expect(healthy.locator('details[data-disclosure="deliveries"]')).not.toHaveAttribute('open', '');
  await healthy.locator('details[data-disclosure="deliveries"]').scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-healthy', test.info(), { group: GROUP });

  const retrying = webhook(page, 'https://erp.bakery.example/payments/in');
  await expect(retrying.locator('.wh-health .tag')).toHaveText('retrying');
  await expect(retrying.locator('.wh-health .hint')).toContainText('next try at');
  await expect(retrying.locator('.wh-health .hint')).toContainText('attempt 5 of 8 failed (503)');
  await expect(retrying.locator('details[data-disclosure="deliveries"]')).toHaveAttribute('open', '');
  await expect(retrying.locator('table[data-table="deliveries"] td[data-label="Attempt"]').first()).toHaveText('4 of 8');
  await retrying.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-retrying', test.info(), { group: GROUP });

  const gaveUp = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await expect(gaveUp.locator('.wh-health .tag')).toHaveText('gave up');
  await expect(gaveUp.locator('.wh-health .hint')).toContainText('after 8 attempts (connection refused)');
  await expect(gaveUp.locator('details[data-disclosure="deliveries"]')).toHaveAttribute('open', '');
  const sendAgain = gaveUp.getByRole('button', { name: 'Send again', exact: true });
  await expect(sendAgain).toHaveCount(1);
  await expect(sendAgain).toHaveAttribute('title', 'Send again');
  // Whole, inside its table, and a target of at least 24px.
  const fits = async () => {
    const button = await sendAgain.boundingBox();
    const table = await gaveUp.locator('.table-scroll:has(table[data-table="deliveries"])').boundingBox();
    expect(button.width).toBeGreaterThanOrEqual(24);
    expect(button.height).toBeGreaterThanOrEqual(24);
    expect(button.x + button.width).toBeLessThanOrEqual(table.x + table.width);
    expect(await gaveUp.locator('.table-scroll:has(table[data-table="deliveries"])').evaluate((t) => t.scrollWidth <= t.clientWidth)).toBe(true);
  };
  await fits();
  await expect(gaveUp.getByRole('button', { name: 'Retry failed (1)' })).toBeVisible();
  // Delete is the site's red danger button, as Retire is on a wallet's page.
  const remove = gaveUp.getByRole('link', { name: 'Delete', exact: true });
  await expect(remove).toHaveAttribute('data-variant', 'danger');
  const danger = await page.evaluate(() => {
    const probe = document.createElement('a');
    probe.className = 'mk-btn';
    probe.dataset.variant = 'danger';
    document.body.append(probe);
    const colour = getComputedStyle(probe).backgroundColor;
    probe.remove();
    return colour;
  });
  expect(await remove.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe(danger);
  await aligned(retrying);
  await aligned(gaveUp);
  await gaveUp.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-gave-up', test.info(), { group: GROUP });

  // Nothing wider than a phone.
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await fits();
  await aligned(retrying);
  await aligned(gaveUp);
  await retrying.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-phone', test.info(), { group: GROUP, asIs: true });
});

test("the All deliveries page keeps Details and Send again in fixed columns, wide and on a phone", async ({ page, context }) => {
  await login(context);
  await seed();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const href = await webhook(page, 'https://old-shop.example/?wc-api=monokulo').getByRole('link', { name: 'All deliveries for this webhook' }).getAttribute('href');
  await page.goto(fixture.base_url + href);
  const list = page.locator('.deliveries-page');
  await expect(list.getByRole('button', { name: 'Send again', exact: true })).toHaveCount(1);
  for (const size of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(size);
    await aligned(list);
    expect(await list.locator('.table-scroll:has(table[data-table="deliveries"])').evaluate((t) => t.scrollWidth <= t.clientWidth)).toBe(true);
  }
});

test('Retry failed queues the given-up deliveries again and says so', async ({ page, context }) => {
  await login(context);
  await seed();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const gaveUp = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await gaveUp.getByRole('button', { name: 'Retry failed (1)' }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Webhooks updated');
  const again = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await expect(again.locator('.wh-health .tag')).toHaveText('sending');
  await expect(again.getByRole('button', { name: /Retry failed/ })).toHaveCount(0);
  await expect(again.locator('table[data-table="deliveries"] td[data-label="Status"] .tag').first()).toHaveText('queued');
});

test("a delivery's details open as a dialog, with its attempts, request and last response", async ({ page, context }) => {
  await login(context);
  await seed();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const retrying = webhook(page, 'https://erp.bakery.example/payments/in');
  await retrying.getByRole('link', { name: 'Details' }).first().click();
  const dialog = page.getByRole('dialog', { name: 'Delivery of order.paid' });
  await expect(dialog).toBeVisible();
  // The status at the top right, on the title's line, beside the close
  // button; the facts under it.
  const title = await dialog.locator('.dialog-title > span:not(.dialog-aside)').boundingBox();
  const status = await dialog.locator('.dialog-title .tag').boundingBox();
  const close = await dialog.locator('.dialog-title .dialog-x').boundingBox();
  expect(status.x).toBeGreaterThan(title.x + title.width);
  expect(Math.abs((status.y + status.height / 2) - (title.y + title.height / 2))).toBeLessThan(4);
  expect(close.x).toBeGreaterThan(status.x + status.width);
  await expect(dialog.locator('[data-facts="delivery"] dt')).toHaveText(['Endpoint', 'Order', 'Next try']);
  await expect(dialog.locator('table tbody tr')).toHaveCount(4);
  await expect(dialog.locator('tbody tr').first().locator('td').first()).toContainText('4');
  await dialog.getByText('Request', { exact: true }).click();
  await expect(dialog.locator('pre').first()).toContainText('X-Monokulo-Signature: t=');
  await expect(dialog.locator('pre').first()).toContainText('"api_version": 2');
  await dialog.getByText('Last response').click();
  await expect(dialog.locator('pre').nth(1)).toContainText('503 Service Unavailable');
  await expect(dialog.getByRole('button', { name: 'Send again now' })).toBeVisible();
  await captureCoverageStage(page, 'store-webhooks-detail-dialog', test.info(), { group: GROUP });
  await dialog.getByRole('link', { name: 'Close' }).click();
  await expect(dialog).toBeHidden();
});

test('a webhook with many deliveries pages through them in place, 20 at a time', async ({ page, context }) => {
  await login(context);
  const response = await fetch(`${fixture.base_url}/__coverage/webhooks?many=45`, { method: 'POST' });
  expect(response.ok).toBe(true);
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const hook = webhook(page, 'https://bakery.example/hooks/monokulo');
  await hook.locator('details[data-disclosure="deliveries"] > summary').click();
  const rows = hook.locator('table[data-table="deliveries"] tbody tr');
  await expect(rows).toHaveCount(20);
  const newest = await rows.first().locator('td[data-label="Order"]').innerText();
  await hook.getByRole('link', { name: 'Older →' }).click();
  // Swapped in place by fixi: still the settings page.
  await expect(page).toHaveURL(/\/settings\?tab=connections#card-webhooks$/);
  await expect(hook.getByRole('link', { name: '← Newer' })).toBeVisible();
  await expect(rows).toHaveCount(20);
  await expect(rows.first().locator('td[data-label="Order"]')).not.toHaveText(newest);
  // A row paged in opens its details as a dialog too.
  await rows.first().getByRole('link', { name: 'Details' }).click();
  const detail = page.getByRole('dialog', { name: 'Delivery of order.paid' });
  await expect(detail).toBeVisible();
  await detail.getByRole('link', { name: 'Close' }).click();
  await expect(detail).toBeHidden();
  await hook.getByRole('link', { name: 'Older →' }).click();
  await expect(rows).toHaveCount(5);
  await expect(hook.getByRole('link', { name: 'Older →' })).toHaveCount(0);
  await hook.locator('.deliveries-results').scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-paged', test.info(), { group: GROUP });
  await hook.getByRole('link', { name: '← Newer' }).click();
  await expect(rows).toHaveCount(20);
});

test('without JavaScript the deliveries page through the All deliveries page', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  const response = await fetch(`${fixture.base_url}/__coverage/webhooks?many=25`, { method: 'POST' });
  expect(response.ok).toBe(true);
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  // In the fold, closed while all is well.
  const older = webhook(page, 'https://bakery.example/hooks/monokulo').locator('a[rel="next"]');
  await expect(older).toHaveText('Older →');
  await page.goto(fixture.base_url + await older.getAttribute('href'));
  await expect(page).toHaveURL(/\/deliveries\?page=1$/);
  await expect(page.locator('table[data-table="deliveries"] tbody tr')).toHaveCount(5);
  await page.getByRole('link', { name: '← Newer' }).click();
  await expect(page).toHaveURL(/\/deliveries$/);
  await expect(page.locator('table[data-table="deliveries"] tbody tr')).toHaveCount(20);
  await context.close();
});

test('a store with no webhooks offers to add one', async ({ page, context }) => {
  await login(context);
  await seed(true);
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  await expect(card(page)).toContainText('No webhooks yet. Monokulo can tell your server when an order is paid, confirming or expired.');
  await expect(card(page).getByRole('button', { name: 'Add webhook' })).toBeVisible();
  await card(page).scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-empty', test.info(), { group: GROUP });
});

test('without JavaScript, Details and Delete are pages and Send again is a form', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await seed();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const gaveUp = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await gaveUp.getByRole('link', { name: 'Details' }).first().click();
  await expect(page).toHaveURL(/\/settings\/webhooks\/wh_[0-9a-f]+\/deliveries\/\d+$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Delivery of order.paid' })).toBeVisible();
  await expect(page.locator('.mk-page-title .tag')).toHaveText('gave up');
  await captureCoverageStage(page, 'store-webhooks-detail-page', test.info(), { group: GROUP, shapes: ['desktop', 'mobile-portrait'] });
  await page.getByRole('button', { name: 'Send again now' }).click();
  await expect(page).toHaveURL(/\/settings\?saved=webhooks#card-webhooks$/);
  await expect(webhook(page, 'https://old-shop.example/?wc-api=monokulo').locator('.wh-health .tag')).toHaveText('sending');

  // Followed as a link (without JavaScript the toast and the save bar can
  // sit over it on this page).
  const remove = webhook(page, 'https://bakery.example/hooks/monokulo').getByRole('link', { name: 'Delete', exact: true });
  await page.goto(fixture.base_url + await remove.getAttribute('href'));
  await expect(page.getByRole('heading', { level: 1, name: 'Delete this webhook?' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Delete webhook' })).toHaveAttribute('data-variant', 'danger');
  await page.getByRole('button', { name: 'Delete webhook' }).click();
  await expect(page).toHaveURL(/\/settings\?saved=webhooks#card-webhooks$/);
  await expect(card(page).locator('.card-meta')).toHaveText('2 webhooks');
  await context.close();
});

/** One webhook, to the fixture's own receiver answering `status`. */
async function seedReceiver(status = '') {
  const response = await fetch(`${fixture.base_url}/__coverage/webhooks?receiver=${status}`, { method: 'POST' });
  expect(response.ok).toBe(true);
}
const receiver = (page) => card(page).locator('.wh').filter({ has: page.locator('.wh-url', { hasText: '/__coverage/webhook-receiver' }) });

test('Send a test webhook delivers one in place and lists it as a test', async ({ page, context }) => {
  await login(context);
  await seedReceiver();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const hook = receiver(page);
  await expect(hook.locator('.wh-health .tag')).toHaveText('no deliveries yet');
  const send = hook.getByRole('button', { name: 'Send a test webhook' });
  // A neutral button beside the red Delete, not a second primary.
  await expect(send).not.toHaveAttribute('data-variant', 'primary');
  await send.click();
  // Swapped in place by fixi: still the settings page.
  const result = hook.locator('[data-test-result]');
  await expect(result).toContainText('Test webhook delivered.');
  await expect(result).toContainText('Your server answered 200 in');
  await expect(page).toHaveURL(/\/settings\?tab=connections#card-webhooks$/);
  await expect(hook.locator('.wh-health .tag')).toHaveText('delivering');
  await expect(hook.locator('details[data-disclosure="deliveries"]')).toHaveAttribute('open', '');
  const row = hook.locator('table[data-table="deliveries"] tbody tr').first();
  await expect(row.locator('td[data-label="Event"]')).toHaveText('webhook.test');
  await expect(row.locator('td[data-label="Order"] [data-test-delivery]')).toHaveText('test');
  await expect(row.locator('td[data-label="Response"]')).toHaveText('200');
  await hook.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-test-delivered', test.info(), { group: GROUP });

  // Its details, though the row was swapped in: a dialog, as for a row the
  // page loaded with. No order, the request as signed.
  await row.getByRole('link', { name: 'Details' }).click();
  const dialog = page.getByRole('dialog', { name: 'Delivery of webhook.test' });
  await expect(dialog).toBeVisible();
  await expect(page).toHaveURL(/\/settings\?tab=connections#card-webhooks$/);
  await expect(dialog.locator('[data-facts="delivery"]')).toContainText('a test delivery');
  await dialog.getByText('Request', { exact: true }).click();
  await expect(dialog.locator('pre').first()).toContainText('X-Monokulo-Event: webhook.test');
  await expect(dialog.locator('pre').first()).toContainText('"event": "webhook.test"');
  await captureCoverageStage(page, 'store-webhooks-test-detail-dialog', test.info(), { group: GROUP });
  await dialog.getByRole('link', { name: 'Close' }).click();
  await expect(dialog).toBeHidden();
  // Delete, in the block swapped in too, still asks first.
  await hook.getByRole('link', { name: 'Delete', exact: true }).click();
  await expect(page.getByRole('dialog', { name: 'Delete this webhook?' })).toBeVisible();
});

test('a test webhook the receiver refuses says what it answered and when it is tried again', async ({ page, context }) => {
  await login(context);
  await seedReceiver(500);
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  const hook = receiver(page);
  await hook.getByRole('button', { name: 'Send a test webhook' }).click();
  const result = hook.locator('[data-test-result]');
  await expect(result).toContainText('Test webhook not delivered.');
  await expect(result).toContainText('Your server answered 500.');
  await expect(result).toContainText('Monokulo tries it again at');
  await expect(hook.locator('.wh-health .tag')).toHaveText('retrying');
  await expect(hook.locator('table[data-table="deliveries"] tbody tr').first().locator('td[data-label="Status"] .tag')).toHaveText('retrying');
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await hook.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-test-failed', test.info(), { group: GROUP, asIs: true });
});

test('without JavaScript, Send a test webhook is a form that comes back showing the delivery', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await seedReceiver();
  await page.goto(`${settings()}?tab=connections#card-webhooks`);
  await receiver(page).getByRole('button', { name: 'Send a test webhook' }).click();
  await expect(page).toHaveURL(/\/settings\?tab=connections&tested=\d+#webhook-wh_[0-9a-f]+$/);
  const hook = receiver(page);
  await expect(hook.locator('[data-test-result]')).toContainText('Test webhook delivered.');
  await expect(hook.locator('table[data-table="deliveries"] tbody tr').first().locator('td[data-label="Event"]')).toHaveText('webhook.test');
  // Details is a link to its page.
  await hook.locator('table[data-table="deliveries"] tbody tr').first().getByRole('link', { name: 'Details' }).click();
  await expect(page).toHaveURL(/\/deliveries\/\d+$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Delivery of webhook.test' })).toBeVisible();
  await expect(page.locator('[data-facts="delivery"]')).toContainText('a test delivery');
  await context.close();
});
