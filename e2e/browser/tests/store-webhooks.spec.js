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
const settings = () => `${fixture.base_url}/dashboard/stores/${fixture.connection_id}/settings`;
/** The design's three webhooks (delivering, retrying, gave up), or none. */
async function seed(empty = false) {
  const response = await fetch(`${fixture.base_url}/__coverage/webhooks${empty ? '?empty' : ''}`, { method: 'POST' });
  expect(response.ok).toBe(true);
  return (await response.json()).deliveries;
}
const card = (page) => page.locator('#card-webhooks');
/** Every row's Details at one x, and its second slot at another. */
async function aligned(hook) {
  const rows = hook.locator('.deliveries-results tbody tr');
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
  await page.goto(`${settings()}#card-webhooks`);
  await expect(card(page).locator('.card-meta')).toHaveText('3 webhooks');

  const healthy = webhook(page, 'https://bakery.example/hooks/monokulo');
  await expect(healthy.locator('.wh-health .tag')).toHaveText('delivering');
  await expect(healthy.locator('.wh-health .hint')).toContainText('last 2 min ago · 200 in');
  await expect(healthy.locator('details.wh-deliveries')).not.toHaveAttribute('open', '');
  await healthy.locator('details.wh-deliveries').scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-healthy', test.info(), { group: GROUP });

  const retrying = webhook(page, 'https://erp.bakery.example/payments/in');
  await expect(retrying.locator('.wh-health .tag')).toHaveText('retrying');
  await expect(retrying.locator('.wh-health .hint')).toContainText('next try at');
  await expect(retrying.locator('.wh-health .hint')).toContainText('attempt 5 of 8 failed (503)');
  await expect(retrying.locator('details.wh-deliveries')).toHaveAttribute('open', '');
  await expect(retrying.locator('td[data-label="Attempt"]').first()).toHaveText('4 of 8');
  await retrying.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-retrying', test.info(), { group: GROUP });

  const gaveUp = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await expect(gaveUp.locator('.wh-health .tag')).toHaveText('gave up');
  await expect(gaveUp.locator('.wh-health .hint')).toContainText('after 8 attempts (connection refused)');
  await expect(gaveUp.locator('details.wh-deliveries')).toHaveAttribute('open', '');
  const sendAgain = gaveUp.getByRole('button', { name: 'Send again', exact: true });
  await expect(sendAgain).toHaveCount(1);
  await expect(sendAgain).toHaveAttribute('title', 'Send again');
  // Whole, inside its table, and a target of at least 24px.
  const fits = async () => {
    const button = await sendAgain.boundingBox();
    const table = await gaveUp.locator('.table-scroll').boundingBox();
    expect(button.width).toBeGreaterThanOrEqual(24);
    expect(button.height).toBeGreaterThanOrEqual(24);
    expect(button.x + button.width).toBeLessThanOrEqual(table.x + table.width);
    expect(await gaveUp.locator('.table-scroll').evaluate((t) => t.scrollWidth <= t.clientWidth)).toBe(true);
  };
  await fits();
  await expect(gaveUp.getByRole('button', { name: 'Retry failed (1)' })).toBeVisible();
  // Delete is the site's red danger button, as Retire is on a wallet's page.
  const remove = gaveUp.getByRole('link', { name: 'Delete', exact: true });
  await expect(remove).toHaveClass(/\bbtn-danger\b/);
  const danger = await page.evaluate(() => {
    const probe = document.createElement('a');
    probe.className = 'btn btn-danger';
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
  await page.goto(`${settings()}#card-webhooks`);
  const href = await webhook(page, 'https://old-shop.example/?wc-api=monokulo').getByRole('link', { name: 'All deliveries for this webhook' }).getAttribute('href');
  await page.goto(fixture.base_url + href);
  const list = page.locator('.deliveries-page');
  await expect(list.getByRole('button', { name: 'Send again', exact: true })).toHaveCount(1);
  for (const size of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(size);
    await aligned(list);
    expect(await list.locator('.table-scroll').evaluate((t) => t.scrollWidth <= t.clientWidth)).toBe(true);
  }
});

test('Retry failed queues the given-up deliveries again and says so', async ({ page, context }) => {
  await login(context);
  await seed();
  await page.goto(`${settings()}#card-webhooks`);
  const gaveUp = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await gaveUp.getByRole('button', { name: 'Retry failed (1)' }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Webhooks updated');
  const again = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await expect(again.locator('.wh-health .tag')).toHaveText('sending');
  await expect(again.getByRole('button', { name: /Retry failed/ })).toHaveCount(0);
  await expect(again.locator('td[data-label="Status"] .tag').first()).toHaveText('queued');
});

test("a delivery's details open as a dialog, with its attempts, request and last response", async ({ page, context }) => {
  await login(context);
  await seed();
  await page.goto(`${settings()}#card-webhooks`);
  const retrying = webhook(page, 'https://erp.bakery.example/payments/in');
  await retrying.getByRole('link', { name: 'Details' }).first().click();
  const dialog = page.getByRole('dialog', { name: 'Delivery of order.paid' });
  await expect(dialog).toBeVisible();
  // The status at the top right, on the title's line, beside the close
  // button; the facts under it.
  const title = await dialog.locator('.dialog-title').boundingBox();
  const status = await dialog.locator('.delivery-head-end .tag').boundingBox();
  const close = await dialog.locator('.delivery-head-end .dialog-x').boundingBox();
  expect(status.x).toBeGreaterThan(title.x + title.width);
  expect(Math.abs((status.y + status.height / 2) - (title.y + title.height / 2))).toBeLessThan(4);
  expect(close.x).toBeGreaterThan(status.x + status.width);
  await expect(dialog.locator('.delivery-facts dt')).toHaveText(['Endpoint', 'Order', 'Next try']);
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
  await page.goto(`${settings()}#card-webhooks`);
  const hook = webhook(page, 'https://bakery.example/hooks/monokulo');
  await hook.locator('details.wh-deliveries summary').click();
  const rows = hook.locator('.deliveries-results tbody tr');
  await expect(rows).toHaveCount(20);
  const newest = await rows.first().locator('td[data-label="Order"]').innerText();
  await hook.getByRole('link', { name: 'Older →' }).click();
  // Swapped in place by fixi: still the settings page.
  await expect(page).toHaveURL(/\/settings#card-webhooks$/);
  await expect(hook.getByRole('link', { name: '← Newer' })).toBeVisible();
  await expect(rows).toHaveCount(20);
  await expect(rows.first().locator('td[data-label="Order"]')).not.toHaveText(newest);
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
  await page.goto(`${settings()}#card-webhooks`);
  // In the fold, closed while all is well.
  const older = webhook(page, 'https://bakery.example/hooks/monokulo').locator('a[rel="next"]');
  await expect(older).toHaveText('Older →');
  await page.goto(fixture.base_url + await older.getAttribute('href'));
  await expect(page).toHaveURL(/\/deliveries\?page=1$/);
  await expect(page.locator('.deliveries-results tbody tr')).toHaveCount(5);
  await page.getByRole('link', { name: '← Newer' }).click();
  await expect(page).toHaveURL(/\/deliveries$/);
  await expect(page.locator('.deliveries-results tbody tr')).toHaveCount(20);
  await context.close();
});

test('a store with no webhooks offers to add one', async ({ page, context }) => {
  await login(context);
  await seed(true);
  await page.goto(`${settings()}#card-webhooks`);
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
  await page.goto(`${settings()}#card-webhooks`);
  const gaveUp = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await gaveUp.getByRole('link', { name: 'Details' }).first().click();
  await expect(page).toHaveURL(/\/settings\/webhooks\/wh_[0-9a-f]+\/deliveries\/\d+$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Delivery of order.paid' })).toBeVisible();
  await expect(page.locator('.delivery-page .tag')).toHaveText('gave up');
  await captureCoverageStage(page, 'store-webhooks-detail-page', test.info(), { group: GROUP, shapes: ['desktop', 'mobile-portrait'] });
  await page.getByRole('button', { name: 'Send again now' }).click();
  await expect(page).toHaveURL(/\/settings\?saved=webhooks#card-webhooks$/);
  await expect(webhook(page, 'https://old-shop.example/?wc-api=monokulo').locator('.wh-health .tag')).toHaveText('sending');

  // Followed as a link (without JavaScript the toast and the save bar can
  // sit over it on this page).
  const remove = webhook(page, 'https://bakery.example/hooks/monokulo').getByRole('link', { name: 'Delete', exact: true });
  await page.goto(fixture.base_url + await remove.getAttribute('href'));
  await expect(page.getByRole('heading', { level: 1, name: 'Delete this webhook?' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Delete webhook' })).toHaveClass(/\bbtn-danger\b/);
  await page.getByRole('button', { name: 'Delete webhook' }).click();
  await expect(page).toHaveURL(/\/settings\?saved=webhooks#card-webhooks$/);
  await expect(card(page).locator('.card-meta')).toHaveText('2 webhooks');
  await context.close();
});
