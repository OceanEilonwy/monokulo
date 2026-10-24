// @ts-check
// A store's webhooks (docs/design/user-testing/webhooks.html, variation 2):
// one health line per webhook, its recent deliveries folded under it and
// open by themselves while one is retrying or gave up, "Send again" and
// "Retry failed (N)" as plain forms, a delivery's detail as a dialog, and
// a long list paged in place. Captures each state for the gallery.
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
/** The design's three webhooks (delivering, retrying, gave up). */
async function seed() {
  const response = await fetch(`${fixture.base_url}/__coverage/webhooks`, { method: 'POST' });
  expect(response.ok).toBe(true);
  return (await response.json()).deliveries;
}
const card = (page) => page.locator('#card-webhooks');
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
  await expect(gaveUp.getByRole('button', { name: 'Retry failed (1)' })).toBeVisible();
  // Delete is the site's danger button, as Retire is on a wallet's page.
  await expect(gaveUp.getByRole('link', { name: 'Delete', exact: true })).toHaveAttribute('data-variant', 'danger');
  await gaveUp.scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-webhooks-gave-up', test.info(), { group: GROUP });

  // A delivery's details open as a dialog: its attempts, the request and
  // the last response.
  await retrying.getByRole('link', { name: 'Details' }).first().click();
  const dialog = page.getByRole('dialog', { name: 'Delivery of order.paid' });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.dialog-title .tag')).toBeVisible();
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

  // Retry failed queues the given-up deliveries again and says so.
  await gaveUp.getByRole('button', { name: 'Retry failed (1)' }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Webhooks updated');
  const again = webhook(page, 'https://old-shop.example/?wc-api=monokulo');
  await expect(again.locator('.wh-health .tag')).toHaveText('sending');
  await expect(again.getByRole('button', { name: /Retry failed/ })).toHaveCount(0);
  await expect(again.locator('td[data-label="Status"] .tag').first()).toHaveText('queued');

  // A webhook with many deliveries pages through them in place, 20 at a time.
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
