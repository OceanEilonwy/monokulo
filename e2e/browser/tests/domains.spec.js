// A store's verified domains (crates/monokulo/src/views/domains.rs): the
// settings card's banner and one card per domain, and adding a domain on
// pages of its own (the domain, the record, the check). Every step works
// without JavaScript; with it, a check answers in place. The fixture's DNS
// always fails, so a check here never verifies (in-process tests cover
// that: http/embed_domains.rs).
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
let fixture;
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
async function login(context) {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
}
const settings = '/stores/coverage-store/settings';

/** The page doesn't scroll sideways. */
async function fits(page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
}

test('a domain is added on its own pages, and its check answers in place', async ({ page, context }) => {
  await login(context);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(fixture.base_url + settings + '?tab=connections#card-verified-domains');
  const card = page.locator('#card-verified-domains');
  // The banner says the state; nothing verified, its button can't be used.
  const banner = card.locator('.mk-status-banner');
  await expect(banner).toHaveAttribute('data-tone', 'warning');
  await expect(banner).toContainText('Any website can show this checkout.');
  await expect(banner.getByRole('button', { name: 'Only allow my domains' })).toBeDisabled();

  await card.getByRole('link', { name: 'Add a domain' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Add a domain' })).toBeVisible();
  // A refused domain is said beside the box, which keeps what was typed.
  await page.getByRole('textbox', { name: 'Domain' }).fill('rachelshandicrafts');
  await page.getByRole('button', { name: 'Next' }).click();
  await expect(page.getByRole('textbox', { name: 'Domain' })).toHaveAttribute('aria-invalid', 'true');
  await expect(page.getByRole('textbox', { name: 'Domain' })).toHaveValue('rachelshandicrafts');
  await expect(page.locator('#new-domain-problem')).toContainText('Enter a full domain');
  await captureCoverageStage(page, 'domains-add-refused', test.info(), { group: 'domains' });

  // A pasted URL is trimmed to its domain: next, the record.
  await page.getByRole('textbox', { name: 'Domain' }).fill('https://Layout-Check.example/cart');
  await page.getByRole('button', { name: 'Next' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Add the record' })).toBeVisible();
  const record = page.locator('table.mk-dns-record');
  await expect(record.locator('td[data-label="Type"]')).toHaveText('TXT');
  await expect(record.locator('td[data-label="Host"]')).toContainText('_monokulo');
  await expect(record.locator('td[data-label="Value"]')).toContainText('monokulo-verify=');
  await expect(page.getByText('_monokulo.layout-check.example', { exact: true })).toBeVisible();
  await captureCoverageStage(page, 'domains-record', test.info(), { group: 'domains' });

  // Checked: not found yet, why, and it keeps checking; no "saved" toast.
  await page.getByRole('button', { name: "I've added it, check now" }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Check the record' })).toBeVisible();
  const result = page.locator('#domain-check');
  await expect(result).toContainText('Not found yet.');
  await expect(result).toContainText('The DNS lookup failed');
  await expect(result).toContainText('Checking again by itself every 30 seconds');
  await expect(page.locator('.toast-success')).toHaveCount(0);
  await captureCoverageStage(page, 'domains-check-missing', test.info(), { group: 'domains' });
  // Check again straight away: the result says when, in place.
  const url = page.url();
  await result.getByRole('button', { name: 'Check again' }).click();
  await expect(result).toContainText('Checked moments ago: you can check again in');
  expect(page.url()).toBe(url);

  // Back in Settings it waits as Not verified, with its record; its card's
  // Check again answers in the card.
  await page.goto(fixture.base_url + settings + '?tab=connections#card-verified-domains');
  const domain = page.locator('article.mk-item-card', { hasText: 'layout-check.example' });
  await expect(domain.locator('.tag')).toHaveText('Not verified');
  await expect(domain.locator('table.mk-dns-record')).toBeVisible();
  await expect(domain.getByRole('button', { name: 'Remove' })).toHaveAttribute('data-variant', 'danger');
  await domain.getByRole('button', { name: 'Check again' }).click();
  await expect(domain).toContainText('Checked moments ago: you can check again in');
  await expect(page.locator('.toast-success')).toHaveCount(0);

  // Removing asks first.
  await domain.getByRole('button', { name: 'Remove' }).click();
  await page.getByRole('dialog', { name: 'Remove layout-check.example?' }).getByRole('button', { name: 'Remove domain' }).click();
  await expect(page.locator('#card-verified-domains')).not.toContainText('layout-check.example');
  expect(errors).toEqual([]);
});

test('without JavaScript a domain is added, checked and removed by posting', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  await login(context);
  const page = await context.newPage();
  await page.goto(fixture.base_url + settings + '/domains/new');
  await page.getByRole('textbox', { name: 'Domain' }).fill('nojs-check.example');
  await page.getByRole('button', { name: 'Next' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Add the record' })).toBeVisible();
  await page.getByRole('button', { name: "I've added it, check now" }).click();
  await expect(page.locator('#domain-check')).toContainText('Not found yet.');
  // The card's Check again lands on the check page too: never a toast.
  await page.goto(fixture.base_url + settings + '?tab=connections#card-verified-domains');
  const domain = page.locator('article.mk-item-card', { hasText: 'nojs-check.example' });
  await expect(domain.locator('.tag')).toHaveText('Not verified');
  await domain.getByRole('button', { name: 'Check again' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Check the record' })).toBeVisible();
  await expect(page.locator('#domain-check')).toContainText('Checked moments ago');
  // Removed without the question (no JavaScript, no dialog).
  await page.goto(fixture.base_url + settings + '?tab=connections#card-verified-domains');
  await page.locator('article.mk-item-card', { hasText: 'nojs-check.example' }).getByRole('button', { name: 'Remove' }).click();
  await expect(page.locator('#card-verified-domains')).not.toContainText('nojs-check.example');
  await context.close();
});

test('on a phone the domain pages fit, and a card stacks its actions under its name', async ({ page, context }) => {
  await login(context);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(fixture.base_url + settings + '/domains/new');
  await fits(page);
  await page.getByRole('textbox', { name: 'Domain' }).fill('phone-check.example');
  await page.getByRole('button', { name: 'Next' }).click();
  await fits(page);
  // The record stacks: one line per field, each named.
  const host = page.locator('table.mk-dns-record td[data-label="Host"]');
  const value = page.locator('table.mk-dns-record td[data-label="Value"]');
  expect((await value.boundingBox()).y).toBeGreaterThan((await host.boundingBox()).y);
  await captureCoverageStage(page, 'domains-record-phone', test.info(), { group: 'domains' });
  await page.goto(fixture.base_url + settings + '?tab=connections#card-verified-domains');
  await fits(page);
  const domain = page.locator('article.mk-item-card', { hasText: 'phone-check.example' });
  const name = await domain.locator('.mk-item-card-title').boundingBox();
  const actions = await domain.locator('.mk-item-card-actions').boundingBox();
  expect(actions.y).toBeGreaterThanOrEqual(name.y + name.height - 1);
  await domain.getByRole('button', { name: 'Remove' }).click();
  await page.getByRole('dialog', { name: 'Remove phone-check.example?' }).getByRole('button', { name: 'Remove domain' }).click();
  await expect(page.locator('#card-verified-domains')).not.toContainText('phone-check.example');
});
