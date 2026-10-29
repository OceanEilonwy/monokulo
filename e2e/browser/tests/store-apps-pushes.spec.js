// @ts-check
// A store's connected apps and their pushes (the push-gaps designs, targets
// B and D): one line per app on the Apps card saying how its pushes are
// doing, with Connect an app in the card's head; the app's own page with
// its details, its push targets by host only (Remove each), Send a test
// push, and its recent pushes. Every action is a plain form, so each is
// also run with JavaScript off. Captures each state for the gallery.
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
const appPage = (id) => `${settings()}/apps/${id}`;
/** The design's three apps: their ids, and the Front counter's targets. */
async function seed() {
  const response = await fetch(`${fixture.base_url}/__coverage/apps`, { method: 'POST' });
  expect(response.ok).toBe(true);
  return response.json();
}
const card = (page) => page.locator('#card-apps');
const row = (page, title) => card(page).locator('.int-rows li').filter({ has: page.locator('.int-name', { hasText: title }) });

/** Nothing of an endpoint, a key or an app key is on the page. */
async function expectNoSecrets(page) {
  const html = await page.content();
  expect(html).not.toContain('/__coverage/push-receiver');
  expect(html).not.toContain('p256dh');
  expect(html).not.toMatch(/ak_[0-9a-f]{8}/);
  await expect(page.getByText(/pt_[0-9a-f]{32}/)).toHaveCount(0);
}

test('the Apps card says how each app\'s pushes are doing', async ({ page, context }) => {
  await login(context);
  const apps = await seed();
  await page.goto(`${settings()}?tab=apps#card-apps`);
  await expect(card(page).locator('#card-apps-title')).toHaveText('Connected apps');
  await expect(card(page).locator('.card-meta')).toHaveText('3 devices');
  await expect(card(page).getByRole('link', { name: 'Connect an app' })).toBeVisible();

  const front = row(page, 'Monokular · Front counter');
  await expect(front.locator('.app-pushes')).toHaveAttribute('data-pushes', 'healthy');
  await expect(front.locator('.app-pushes')).toContainText('2 push targets · last push 2 min ago');
  await expect(front.locator('.app-pushes-ok')).toHaveText('healthy');
  await expect(front.getByRole('link', { name: 'Pushes of Monokular · Front counter' })).toHaveAttribute('href', new RegExp(`/apps/${apps.front}$`));

  const kitchen = row(page, 'Tillwatch · Kitchen');
  await expect(kitchen.locator('.app-pushes-bad').last()).toHaveText('1 failing (12 in a row, last 502 14 min ago)');

  const back = row(page, 'Monokular · Back office iPad');
  await expect(back.locator('.app-pushes')).toHaveAttribute('data-pushes', 'none');
  await expect(back.getByRole('link', { name: 'Details of Monokular · Back office iPad' })).toBeVisible();
  await expectNoSecrets(page);
  await card(page).scrollIntoViewIfNeeded();
  await captureCoverageStage(page, 'store-apps-summary', test.info(), { group: GROUP });
});

test('an app\'s page: its targets by host, a test push, Remove', async ({ page, context }) => {
  await login(context);
  const apps = await seed();
  await page.goto(`${settings()}?tab=apps#card-apps`);
  await row(page, 'Monokular · Front counter').getByRole('link', { name: /^Pushes of/ }).click();
  await expect(page).toHaveURL(appPage(apps.front));
  await expect(page.locator('h1')).toHaveText('Monokular · Front counter');
  await expect(page.locator('.mk-breadcrumbs a').nth(1)).toHaveText('Settings');
  await expect(page.locator('.mk-breadcrumbs a').nth(2)).toHaveText('Apps');
  await expect(page.getByRole('button', { name: 'Disconnect Monokular · Front counter' })).toBeVisible();
  await expect(page.locator('[data-facts="app"]')).toContainText('An app key of its own');
  const targets = page.locator('#card-push-targets');
  await expect(targets.locator('.card-meta')).toHaveText('2 of 8 · both healthy');
  await expect(targets.locator('article[data-push-target]')).toHaveCount(2);
  await expect(targets.locator('article[data-push-target="encrypted"] .mk-item-card-meta')).toContainText('To 127.0.0.1 · since');
  const recent = page.locator('#card-recent-pushes');
  await expect(recent.locator('tbody tr')).not.toHaveCount(0);
  await expect(recent.locator('[data-push-state="retrying"]')).toContainText('429, next in');
  await expect(recent.locator('[data-push-state="gaveup"]')).toContainText('timed out');
  await expectNoSecrets(page);
  await captureCoverageStage(page, 'store-app-page', test.info(), { group: GROUP });

  // A test push: sent at once, as any push, to the fixture's receiver.
  await page.locator('#test-target').selectOption(apps.alerts, { force: true });
  await page.locator('#test-event').selectOption('sale.paid', { force: true });
  await page.getByRole('button', { name: 'Send a test push' }).click();
  await expect(page).toHaveURL(/\?tested=\d+/);
  await expect(page.locator('#settings-toasts')).toContainText('Test push delivered.');
  await expect(page.locator('#settings-toasts')).toContainText('127.0.0.1 answered 201 in');
  const top = recent.locator('tbody tr').first();
  await expect(top.locator('[data-test-push]')).toHaveText('test');
  await expect(top.locator('td[data-label="Result"]')).toContainText('Delivered');
  await expectNoSecrets(page);
  await captureCoverageStage(page, 'store-app-page-tested', test.info(), { group: GROUP });

  // Remove the Live Activity.
  await page.getByRole('button', { name: 'Remove the Live Activity target to 127.0.0.1' }).click();
  await expect(page).toHaveURL(/\?removed=1/);
  await expect(page.locator('#settings-toasts')).toContainText('Push target removed.');
  await expect(targets.locator('.card-meta')).toHaveText('1 of 8 · healthy');
});

test('a failing app stays failing after a test, and one with no targets has no test form', async ({ page, context }) => {
  await login(context);
  const apps = await seed();
  await page.goto(appPage(apps.kitchen));
  const targets = page.locator('#card-push-targets');
  await expect(targets.locator('.mk-item-card .tag').first()).toHaveText('Failing · 12 in a row');
  await page.getByRole('button', { name: 'Send a test push' }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Test push failed.');
  await expect(page.locator('#settings-toasts')).toContainText('answered 502');
  await expect(targets.locator('.mk-item-card .tag').first()).toHaveText('Failing · 13 in a row');
  await captureCoverageStage(page, 'store-app-page-failing', test.info(), { group: GROUP });

  await page.goto(appPage(apps.back));
  await expect(page.locator('#card-push-targets .card-meta')).toHaveText('none of 8');
  await expect(page.locator('#card-push-targets')).toContainText('No push targets yet');
  await expect(page.getByRole('button', { name: 'Send a test push' })).toHaveCount(0);
  await captureCoverageStage(page, 'store-app-page-no-targets', test.info(), { group: GROUP });
});

test('without JavaScript the page, a test push and Remove work as plain forms', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const apps = await seed();
  const page = await context.newPage();
  await page.goto(`${settings()}?tab=apps#card-apps`);
  await expect(row(page, 'Monokular · Front counter').locator('.app-pushes')).toHaveAttribute('data-pushes', 'healthy');
  await page.goto(appPage(apps.front));
  await page.locator('#test-target').selectOption(apps.alerts);
  await page.locator('#test-event').selectOption('sale.expired');
  await page.getByRole('button', { name: 'Send a test push' }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Test push delivered.');
  await expect(page.locator('#card-recent-pushes tbody tr').first().locator('code')).toHaveText('sale.expired');
  await page.getByRole('button', { name: 'Remove the encrypted push target to 127.0.0.1' }).click();
  await expect(page.locator('#settings-toasts')).toContainText('Push target removed.');
  await expect(page.locator('#card-push-targets .card-meta')).toHaveText(/^1 of 8/);
  await expectNoSecrets(page);
  await context.close();
});
