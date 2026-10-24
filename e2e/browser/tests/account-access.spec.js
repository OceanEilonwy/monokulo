// A merchant's way in: signing up, logging out and back in, and a shop
// plugin asking to connect. Against the real binaries (real-stack.js), so
// the session cookie, the login check and the connect token are the ones an
// operator runs. The file's tests run in order: the second logs in as the
// merchant the first signed up.
const { test, expect } = require('@playwright/test');
const {
  useRealStack, fixture, signInAsAdmin, saveEngineSettings, expectSaved, saveNodes, fakeNodeAddress, createStore, finishStoreSetup,
} = require('./backend-helpers');

useRealStack(test);

const EMAIL = 'merchant@example.com';
const PASSWORD = 'a merchant password';
const SHOP = 'shop.example';

/** Logs in on the open login page. */
async function logIn(page, password) {
  await page.locator('input[name="email"]').fill(EMAIL);
  await page.locator('input[name="password"]').fill(password);
  await page.getByRole('button', { name: 'Log in' }).click();
}

test('a merchant signs up, logs out, is refused a wrong password, and logs back in', async ({ page, browser }) => {
  const { monokulo_url: base } = fixture();
  // The admin opens sign-up to anyone, gives the instance its address (the
  // plugin connect flow needs it), and stagenet a node for stores.
  await signInAsAdmin(page);
  await saveEngineSettings(page, { 'signup.mode': 'public', public_uri: base });
  await expectSaved(page);
  await saveNodes(page, { stagenet: [fakeNodeAddress()] });
  await expectSaved(page);

  const context = await browser.newContext();
  try {
    const merchant = await context.newPage();
    await merchant.goto(base + '/signup');
    await merchant.locator('input[name="email"]').fill(EMAIL);
    await merchant.locator('input[name="password"]').fill(PASSWORD);
    await merchant.getByRole('button', { name: 'Sign up' }).click();
    // Signed in straight away, and on to setting up a store.
    await expect(merchant).toHaveURL(/\/setup$/);
    await merchant.locator('summary.acct-button').click();
    await merchant.getByRole('button', { name: 'Log out' }).click();
    await expect(merchant).toHaveURL(/\/login$/);
    // Signed out: the dashboard sends them to log in.
    await merchant.goto(base + '/');
    await expect(merchant).toHaveURL(/\/login/);

    await merchant.goto(base + '/login');
    await logIn(merchant, 'not the password');
    await expect(merchant.getByText('Invalid email or password.')).toBeVisible();
    await expect(merchant).toHaveURL(/\/login$/);
    expect((await context.cookies()).some((cookie) => cookie.name === 'session')).toBe(false);

    await logIn(merchant, PASSWORD);
    await expect(merchant).not.toHaveURL(/\/login/);
    await merchant.goto(base + '/account');
    await expect(merchant.locator('input[name="email"]')).toHaveValue(EMAIL);
  } finally { await context.close(); }
});

test('a shop plugin asks to connect: the merchant logs in, connects their store, and the plugin redeems its code once', async ({ page, request }) => {
  const { monokulo_url: base } = fixture();
  // The merchant's store for the shop, set up while signed in.
  await page.goto(base + '/login');
  await logIn(page, PASSWORD);
  await expect(page).not.toHaveURL(/\/login/);
  await createStore(page, SHOP);
  await finishStoreSetup(page);
  await page.locator('summary.acct-button').click();
  await page.getByRole('button', { name: 'Log out' }).click();
  await expect(page).toHaveURL(/\/login$/);

  const ask = new URL(`${base}/plugins/connect/acme-cart`);
  ask.search = new URLSearchParams({
    site_url: `https://${SHOP}`, return_url: `https://${SHOP}/monokulo/return`, nonce: 'n-123', name: 'Acme Cart',
  }).toString();
  // Signed out, the plugin's request waits behind the login page, which
  // says who is asking.
  await page.goto(ask.toString());
  await expect(page).toHaveURL(/\/login\?next=/);
  await expect(page.getByText(`${SHOP} wants to connect to Monokulo.`)).toBeVisible();
  await logIn(page, PASSWORD);
  await expect(page.getByRole('heading', { name: 'Connect your Acme Cart shop' })).toBeVisible();
  // Sent back to the shop with a single-use code and the nonce it sent.
  // (The shop isn't there to answer: where Monokulo sends the browser is
  // what counts.)
  const answer = page.waitForResponse((response) => response.request().method() === 'POST' && response.url().endsWith('/plugins/connect/acme-cart'));
  await page.getByRole('button', { name: `Connect to ${SHOP}` }).click();
  expect((await answer).status()).toBe(302);
  const back = new URL((await answer).headers().location);
  expect(back.origin).toBe(`https://${SHOP}`);
  expect(back.pathname).toBe('/monokulo/return');
  expect(back.searchParams.get('nonce')).toBe('n-123');
  const token = back.searchParams.get('token');
  expect(token).toBeTruthy();

  // The plugin's server redeems it, with no session: the store's keys.
  const finish = `${base}/plugins/connect/acme-cart/finish`;
  const redeemed = await request.post(finish, { data: { token } });
  expect(redeemed.status()).toBe(200);
  const connection = await redeemed.json();
  expect(connection.secret_token).toMatch(/^sk_/);
  expect(connection.public_key).toMatch(/^pk_/);
  // Once only.
  expect((await request.post(finish, { data: { token } })).ok()).toBe(false);
});
