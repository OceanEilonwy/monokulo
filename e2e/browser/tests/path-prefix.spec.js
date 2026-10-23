// @ts-check
// Monokulo behind a reverse proxy that serves it under a path, as an
// operator's nginx would (`public_uri = "http://127.0.0.1:{port}/monokulo"`,
// the proxy stripping `/monokulo` before passing a request on): the real
// binaries, driven through the main flows. Every link, form, redirect,
// asset, fetch and stream must stay under the path; the proxy turns away
// anything else (real-stack.js startPrefixProxy), and each test checks it
// turned nothing away.
//
// No instrumented assets here (coverage-fixture.js serves them for any
// `/static/` URL, which would hide one given without the path).
const { test, expect } = require('../coverage-test');
const { useRealStack, fixture, connectStore, ADMIN_EMAIL, ADMIN_PASSWORD } = require('./backend-helpers');

const PREFIX = '/monokulo';
useRealStack(test, { prefix: PREFIX });

/** The proxy's origin, `http://127.0.0.1:{port}`: a path an href gives
 * (already under the prefix) goes on it. */
const origin = () => new URL(fixture().monokulo_url).origin;

/** Every request the page makes, to check none went to Monokulo's host
 * outside the prefix (the proxy says the same from its side). */
function watchRequests(page) {
  const seen = [];
  page.on('request', (request) => {
    const url = new URL(request.url());
    if (url.origin === origin()) seen.push(url.pathname);
  });
  return seen;
}

/** Nothing reached the host outside the prefix: neither a request the page
 * made nor one the proxy turned away. */
function expectAllUnderThePrefix(seen) {
  expect(seen.filter((path) => path !== PREFIX && !path.startsWith(PREFIX + '/'))).toEqual([]);
  expect(fixture().proxy_misses).toEqual([]);
}

test.describe.configure({ mode: 'serial' });

test('the admin signs up, logs out and back in, and invites a merchant who signs up', async ({ page, browser }) => {
  const seen = watchRequests(page);
  const base = fixture().monokulo_url;
  // A fresh instance sends its first visitor to make the admin account.
  await page.goto(base + '/');
  await expect(page).toHaveURL(`${base}/admin/setup`);
  await page.locator('input[name="email"]').fill(ADMIN_EMAIL);
  await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
  await page.locator('input[name="confirm_password"]').fill(ADMIN_PASSWORD);
  await page.getByRole('button', { name: 'Create admin account' }).click();
  await expect(page).toHaveURL(new RegExp(`^${base}/admin/settings\\?tab=nodes`));

  // Log out from the account menu, then back in through the form.
  await page.locator('.acct > summary').click();
  await page.locator('.acct-menu').getByRole('button', { name: 'Log out' }).click();
  await expect(page).toHaveURL(new RegExp(`^${base}/login`));
  await page.goto(base + '/admin/logs');
  await expect(page).toHaveURL(new RegExp(`^${base}/login\\?next=`));
  await page.locator('input[name="email"]').fill(ADMIN_EMAIL);
  await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
  await page.getByRole('button', { name: 'Log in' }).click();
  // Back where the login was asked for.
  await expect(page).toHaveURL(`${base}/admin/logs`);
  const cookies = await page.context().cookies();
  expect(cookies.find((cookie) => cookie.name === 'session')?.path).toBe(PREFIX);

  // An invite link, and a merchant signing up with it.
  await page.goto(base + '/admin/invites');
  await page.getByRole('button', { name: 'Create invite link' }).click();
  const link = (await page.locator('#invites pre').textContent())?.trim() || '';
  expect(link.startsWith(`${base}/signup?invite=`)).toBe(true);
  const merchant = await browser.newPage();
  const merchantSeen = watchRequests(merchant);
  await merchant.goto(link);
  await merchant.locator('input[name="email"]').fill('merchant@example.com');
  await merchant.locator('input[name="password"]').fill(ADMIN_PASSWORD);
  await merchant.getByRole('button', { name: 'Sign up' }).click();
  await expect(merchant).toHaveURL(new RegExp(`^${base}/(setup)?$`));
  await merchant.close();
  expectAllUnderThePrefix([...seen, ...merchantSeen]);
});

test('a store is set up, an order made from its page with its checkout following it, and the POS charges and cancels one', async ({ page, context }) => {
  const seen = watchRequests(page);
  const base = fixture().monokulo_url;
  await page.goto(base + '/login');
  await page.locator('input[name="email"]').fill(ADMIN_EMAIL);
  await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
  await page.getByRole('button', { name: 'Log in' }).click();
  await expect(page).not.toHaveURL(/\/login/);

  // Nodes, a wallet and the store, through the admin settings, the account
  // and the setup pages.
  const store = await connectStore(page, 'prefix.example.com');
  expect(store?.startsWith(`${PREFIX}/stores/`)).toBe(true);

  // An order from the store's page.
  await page.goto(origin() + store);
  await page.getByRole('link', { name: /Create an order/ }).click();
  await expect(page).toHaveURL(new RegExp(`^${base}/stores/[^/]+/orders/new`));
  await page.locator('input[name="amount"]').fill('0.01');
  await page.getByRole('button', { name: 'Create order', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`^${base}/stores/[^/]+/orders/order_`));
  const orderPage = page.url();
  // The order page follows the order live (fixi's stream).
  await expect.poll(() => seen.some((path) => /\/orders\/order_[^/]+\/events$/.test(path))).toBe(true);
  const payment = await page.locator('#share-payment-link').getAttribute('href');
  expect(payment?.startsWith(`${base}/checkout/order_`)).toBe(true);

  // The checkout, in a tab of its own, following the order on its stream.
  const checkout = await context.newPage();
  const checkoutSeen = watchRequests(checkout);
  const stream = checkout.waitForResponse((response) => /\/checkout\/order_[^/]+\/events/.test(response.url()));
  await checkout.goto(payment || '');
  const opened = await stream;
  expect(opened.status()).toBe(200);
  expect(opened.headers()['content-type']).toContain('text/event-stream');
  expect(new URL(opened.url()).pathname.startsWith(`${PREFIX}/checkout/`)).toBe(true);
  // The share page frames the checkout, under the prefix too.
  await expect(checkout.locator('#checkout-frame')).toHaveAttribute('src', new RegExp(`^${PREFIX}/checkout/order_`));
  await expect(checkout.frameLocator('#checkout-frame').locator('body')).toContainText('Send the exact amount below');

  // The POS charges an order and cancels it: its fetches, and the stream
  // it follows its orders on, under the prefix.
  await page.goto(origin() + store + '/pos');
  await expect(page.locator('.pos-keypad')).toBeVisible();
  const posStream = page.waitForResponse((response) => /\/pos\/events\?orders=/.test(response.url()));
  await page.keyboard.type('1000000000');
  const [made] = await Promise.all([
    page.waitForResponse((r) => r.url().endsWith('/pos/orders') && r.request().method() === 'POST'),
    page.getByRole('button', { name: 'Charge' }).click(),
  ]);
  expect(made.ok()).toBeTruthy();
  expect(new URL(made.url()).pathname.startsWith(`${PREFIX}/stores/`)).toBe(true);
  await expect(page.locator('.pos-payment')).toBeVisible();
  const followed = await posStream;
  expect(followed.status()).toBe(200);
  expect(new URL(followed.url()).pathname.startsWith(`${PREFIX}/stores/`)).toBe(true);
  await page.getByRole('button', { name: 'Cancel order…' }).click();
  const [cancelled] = await Promise.all([
    page.waitForResponse((r) => r.url().endsWith('/cancel') && r.request().method() === 'POST'),
    page.getByRole('dialog').getByRole('button', { name: 'Cancel order' }).click(),
  ]);
  expect(cancelled.ok()).toBeTruthy();
  expect(new URL(cancelled.url()).pathname.startsWith(`${PREFIX}/stores/`)).toBe(true);
  await expect(page.locator('body')).toContainText('Cancelled');
  expectAllUnderThePrefix([...seen, ...checkoutSeen]);
});

test('the admin searches the logs, opens a line and follows them live', async ({ page }) => {
  const seen = watchRequests(page);
  const base = fixture().monokulo_url;
  await page.goto(base + '/login');
  await page.locator('input[name="email"]').fill(ADMIN_EMAIL);
  await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
  await page.getByRole('button', { name: 'Log in' }).click();
  await expect(page).not.toHaveURL(/\/login/);

  await page.goto(base + '/admin/logs');
  // A search, swapped in by fixi with the address bar kept in step.
  await page.locator('#log-q').fill('level >= info');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`^${base}/admin/logs\\?`));
  const line = page.locator('#log-rows details.log-row').first();
  await expect(line).toBeVisible();
  // Its properties load when it opens.
  await line.locator('summary').click();
  await expect(line.locator('.props table')).toBeVisible();
  // Live: the stream from the results' tail URL.
  const live = page.waitForResponse((response) => response.url().includes('/admin/logs/tail'));
  await page.locator('#log-live').click();
  expect(new URL((await live).url()).pathname).toBe(`${PREFIX}/admin/logs/tail`);
  await expect(page.locator('#log-live')).toHaveAttribute('aria-pressed', 'true');
  await page.locator('#log-live').click();
  // The syntax page, a link of its own.
  await page.goto(base + '/admin/logs/syntax');
  await page.getByRole('link', { name: 'Logs' }).first().click();
  await expect(page).toHaveURL(`${base}/admin/logs`);
  expectAllUnderThePrefix(seen);
});
