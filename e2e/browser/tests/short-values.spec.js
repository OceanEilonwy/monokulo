// @ts-check
// Shortened values (`views::short_id`, site.css `.short-value`, issues #12
// to #14): the short text is what shows; a double-click and a copy take
// exactly the whole value; a screen reader gets the whole value; the browser's
// find matches it; and an order ID link is still a link, underlined, that
// opens the order. On the dashboard's public key and order IDs and the
// orders list.
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

let fixture;
let orderId;
test.describe.configure({ mode: 'default' });
test.beforeAll(async () => {
  fixture = await startCoverageFixture();
  const created = await fetch(`${fixture.base_url}/__coverage/orders`, { method: 'POST' });
  expect(created.ok).toBe(true);
  orderId = (await created.json()).order_id;
});
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context, browserName }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  if (browserName === 'chromium') await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
});

/** What a copy put on the clipboard. Chromium's clipboard is read back;
 * Firefox's can't be read in a test, so there it's the text the copy took,
 * caught from its `copy` event. */
async function copied(page, browserName) {
  if (browserName === 'chromium') return page.evaluate(() => navigator.clipboard.readText());
  return page.evaluate(() => window.__copied);
}

async function watchCopies(page) {
  await page.addInitScript(() => {
    document.addEventListener('copy', () => { window.__copied = String(document.getSelection()); });
  });
}

/** The short form of `value` as `short-id` writes it: `prefix` kept, 6…6. */
const shortOf = (value, prefix = '') => {
  const body = value.slice(prefix.length);
  return `${prefix}${body.slice(0, 6)}…${body.slice(-6)}`;
};

test("the dashboard's public key shows short, and copies, reads and finds whole", async ({ page, browserName }) => {
  await watchCopies(page);
  await page.goto(fixture.base_url + '/');
  const value = page.locator('tr', { hasText: 'shop.localhost' }).first().locator('.short-value');
  const publicKey = await value.getAttribute('title');
  expect(publicKey).toMatch(/^pk_[0-9a-f]{48}$/);
  // Shown short, with one ellipsis.
  await expect(value.locator('.short-value-text')).toHaveText(shortOf(publicKey, 'pk_'));
  await expect(value.locator('.short-value-text')).toBeVisible();
  expect(await value.evaluate((el) => el.getBoundingClientRect().width)).toBeLessThan(200);
  // A double-click and a copy take the whole key and nothing else.
  await value.dblclick();
  expect(await page.evaluate(() => String(document.getSelection()))).toBe(publicKey);
  await page.keyboard.press('ControlOrMeta+c');
  expect(await copied(page, browserName)).toBe(publicKey);
  // A screen reader reads the whole key, never the short one.
  const cell = value.locator('xpath=ancestor::td');
  const read = await cell.ariaSnapshot();
  expect(read).toContain(publicKey);
  expect(read).not.toContain('…');
  await expect(page.getByText(publicKey, { exact: true })).toHaveCount(1);
  // The browser's find matches the whole key, or any part of it.
  await page.evaluate(() => getSelection().removeAllRanges());
  expect(await page.evaluate((key) => window.find(key.slice(10, 30)), publicKey)).toBe(true);
  expect(await page.evaluate(() => getSelection().anchorNode.parentElement.className)).toBe('short-value-full');
});

for (const [where, path] of [['dashboard', '/'], ['orders list', '/dashboard/stores/coverage-store/orders']]) {
  test(`an order ID on the ${where} is a short, underlined link to the order whose name is the whole ID`, async ({ page, browserName }) => {
    await page.goto(fixture.base_url + path);
    // Its accessible name is the whole ID.
    const link = page.getByRole('link', { name: orderId, exact: true });
    await expect(link).toHaveCount(1);
    const value = link.locator('.short-value');
    await expect(value.locator('.short-value-text')).toHaveText(shortOf(orderId.replace(/^order_/, '')));
    await expect(value.locator('.short-value-text')).toBeVisible();
    // Underlined like any link, and the site's hover colour on hover.
    const style = (locator) => locator.evaluate((el) => ({ line: getComputedStyle(el).textDecorationLine, color: getComputedStyle(el).color }));
    const linkStyle = await style(link);
    expect(linkStyle.line).toBe('underline');
    expect((await style(value)).line).toBe('underline');
    await link.hover();
    const hovered = await style(value.locator('.short-value-text'));
    expect(hovered.color).toBe((await style(link)).color);
    expect(hovered.color).not.toBe(linkStyle.color);
    // The browser's find matches the whole ID.
    expect(await page.evaluate((id) => window.find(id), orderId)).toBe(true);
    // A click anywhere on it, the short text or the whole value laid over
    // it, opens the order.
    await value.click();
    await expect(page).toHaveURL(new RegExp(`/orders/${orderId}$`));
    expect(browserName).toBeTruthy();
  });
}

test('a double-click on an order ID link opens the order: the link wins over selecting it', async ({ page }) => {
  // Any link in a browser: its first click follows it. So the whole ID is
  // selected and copied from the order's own page (its Order ID row), or
  // with the browser's own link selection (Alt+drag), not a double-click.
  await page.goto(fixture.base_url + '/dashboard/stores/coverage-store/orders');
  await page.getByRole('link', { name: orderId, exact: true }).locator('.short-value').dblclick();
  await expect(page).toHaveURL(new RegExp(`/orders/${orderId}$`));
});
