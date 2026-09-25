// @ts-check
// Real stagenet + real browser e2e for the POS terminal screen
// (views/pos.rs + views/checkout.rs). See ../README.md for what this needs and how to
// run it - deliberately never wired into any default `npm test`/CI.
//
// The backend (a real, network-bound engine against the real public
// stagenet node, plus a real, network-bound monokulo with one account/store
// already connected) is booted once by ../global-setup.js
// (crates/scanner/src/bin/e2e_harness.rs) and shared by both tests below -
// they run sequentially (see playwright.config.js: fullyParallel: false,
// workers: 1), never concurrently, since they share one real backend and one
// real customer wallet.
//
// Login happens through the real `/dashboard/login` form in the browser
// (not injected cookies) - the one part of setup worth actually driving
// through the UI, since it's cheap and it's real production code a merchant
// depends on before they ever reach the POS screen. Account/store creation
// itself is not (that's `e2e_harness.rs`'s job, over plain HTTP, not the
// thing under test here).
//
// What this suite deliberately does NOT attempt: triggering a real error
// state (double-spend/under/overpaid/expired) by crafting a real wrong
// payment on-chain. That's `derive_payment_error`'s own job
// (`crates/monokulo/src/http/pos.rs::pure_logic_tests`) - deterministic,
// free, and fast; reproducing it with a real transaction here would only add
// real cost and wall-clock time for the same coverage.

const { test, expect } = require('@playwright/test');
const { loadFixture, piconeroFromXmrDisplay, sendStagenetPayment, enterAmount } = require('../helpers');

/** @type {ReturnType<typeof loadFixture>} */
let fixture;

test.beforeAll(() => {
  fixture = loadFixture();
});

/** Real login through the real form, then navigates to the real POS page. */
async function loginAndOpenPos(page) {
  await page.goto(`${fixture.monokulo_base_url}/dashboard/login`);
  await page.fill('input[name="email"]', fixture.email);
  await page.fill('input[name="password"]', fixture.password);
  await page.click('button[type="submit"]');
  await expect(page).toHaveURL(/\/dashboard$/);

  await page.goto(`${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}/pos`);
  await expect(page.locator('#keypad-screen')).toBeVisible();
}

/**
 * `context.request` shares the browser context's own cookie jar - once
 * `loginAndOpenPos` has run in this same `context`, this reaches the real
 * authenticated settings endpoint (`orders::update_confirmations_required`)
 * exactly as the store detail page's own form would.
 */
async function setConfirmationsRequired(context, value) {
  const response = await context.request.post(
    `${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}/settings/confirmations`,
    { form: { confirmations_required: String(value) } },
  );
  expect(response.ok()).toBeTruthy();
}

/** Enters `digits` on the real keypad, charges, and returns the real order JSON. */
async function chargeAndGetOrder(page, digits) {
  await enterAmount(page, digits);
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.url().includes('/pos/orders') && r.request().method() === 'POST'),
    page.click('#charge-btn'),
  ]);
  expect(response.ok()).toBeTruthy();
  const order = await response.json();
  await expect(page.locator('#payment-screen')).toBeVisible();
  return order;
}

test.describe.serial('POS terminal - real stagenet payments', () => {
  test('the store disables POS without JavaScript and a direct visit explains why', async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    try {
      await context.addCookies([{ name: 'session', value: fixture.session_cookie.slice(fixture.session_cookie.indexOf('=') + 1), url: fixture.monokulo_base_url }]);
      const page = await context.newPage();
      await page.goto(`${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}`);
      const launcher = page.locator('#pos-launch');
      await expect(launcher).toHaveAttribute('aria-disabled', 'true');
      await expect(launcher).not.toHaveAttribute('href', /./);
      await expect(page.locator('#pos-launch-hint')).toHaveText('Requires JS');
      await page.goto(`${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}/pos`);
      await expect(page.getByText('POS requires JavaScript.')).toBeVisible();
      await expect(page.locator('#keypad-screen')).toBeHidden();
    } finally { await context.close(); }
  });

  test('a 0-conf-trusted payment shows shared success then auto-returns to the keypad', async ({ page, context }) => {
    // Each connect+send attempt is bounded by StagenetTestWallet's own 60s
    // reqwest client timeout (decoy selection is served from a cached
    // snapshot now, not a live fetch - see that crate's own doc comment for
    // why that's both faster and safe), but send_payment_handler retries the
    // whole sequence up to 5 times with a 5s backoff on real node flakiness -
    // budget well past a single attempt's own worst case. Real observed runs
    // finish in ~1-2 minutes.
    test.setTimeout(8 * 60 * 1000);

    await loginAndOpenPos(page);
    // The harness connects this store with confirmations_required=0. The
    // next test switches to 1 before creating its own order.

    const order = await chargeAndGetOrder(page, '335000000'); // 0.000335 XMR
    const piconero = piconeroFromXmrDisplay(order.xmr_amount);
    expect(piconero).toBe(335_000_000n);

    // The POS displays the same payment page as public checkout.
    const checkout = page.frameLocator('#payment-frame');
    await expect(page.locator('#payment-frame')).toHaveAttribute('src', new RegExp(`/pay/.*/orders/${order.order_id}\\?view=pos$`));
    await expect(checkout.locator('.qr-wrap svg')).toBeVisible();
    await expect(checkout.locator('#address')).toHaveValue(order.address);
    // An image of the displayed QR can fill the refund address without typing.
    const qrImage = await checkout.locator('.qr-wrap svg').screenshot();
    await checkout.locator('#refund-image').setInputFiles({ name: 'refund.png', mimeType: 'image/png', buffer: qrImage });
    await expect(checkout.locator('#refund_address')).toHaveValue(order.address);
    await expect(checkout.locator('#refund-field')).toHaveClass(/is-saved/);
    await expect(checkout.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Refund address saved');

    console.log(`sending real stagenet payment: ${piconero} piconero to ${order.address}`);
    const txHash = await sendStagenetPayment(fixture.send_payment_url, order.address, piconero);
    console.log(`sent - tx ${txHash}`);

    // The shared confirmation state appears when a real tx is seen in the
    // mempool - 0-conf, before any confirmations at all.
    await expect(checkout.locator('#payment-state')).toBeVisible({ timeout: 90_000 });
    await expect(checkout.locator('#payment-state')).not.toHaveClass(/is-error/);

    // This order's native 0-conf threshold settles it to "paid" with no real
    // confirmations needed - the checkout closes and the keypad
    // returns on its own, no merchant action needed. Not necessarily on the
    // very same scan tick that first saw it in the mempool (status can take
    // one more tick to settle from "unconfirmed" to "paid"), so this gets
    // real margin, not just the ~3s scan interval plus the client's own
    // 3.5s auto-dismiss delay.
    await expect(page.locator('#keypad-screen')).toBeVisible({ timeout: 60_000 });
    await expect(page.locator('#payment-screen')).toBeHidden();

    // Spec point 5: a POS-created order is a real order, visible on the
    // normal dashboard orders list, not something private to this screen.
    const ordersPage = await context.request.get(`${fixture.monokulo_base_url}/dashboard/stores/${fixture.connection_id}/orders`);
    expect(await ordersPage.text()).toContain(order.order_id);
  });

  test('a confirming payment can be backgrounded and completes in the top stack', async ({ page, context }) => {
    // The send itself is fast now (see the first test's own comment on why),
    // but this one *also* waits for one real stagenet confirmation
    // (~2min average, up to ~5min budgeted below) - the one genuinely slow
    // step left in this whole suite.
    test.setTimeout(8 * 60 * 1000);

    await loginAndOpenPos(page);
    await setConfirmationsRequired(context, 1);

    const order = await chargeAndGetOrder(page, '336000000'); // 0.000336 XMR - a distinct amount from the first test's order
    const piconero = piconeroFromXmrDisplay(order.xmr_amount);

    console.log(`sending real stagenet payment: ${piconero} piconero to ${order.address}`);
    const txHash = await sendStagenetPayment(fixture.send_payment_url, order.address, piconero);
    console.log(`sent - tx ${txHash}`);

    // While confirming, the shared state and wrapper's background action appear.
    const checkout = page.frameLocator('#payment-frame');
    await expect(checkout.locator('#payment-state')).toBeVisible({ timeout: 90_000 });
    await expect(page.locator('#background-btn')).toBeVisible({ timeout: 30_000 });

    // Backgrounding closes the payment view and returns to the keypad.
    await page.click('#background-btn');
    await expect(page.locator('#keypad-screen')).toBeVisible();
    await expect(page.locator('#payment-screen')).toBeHidden();

    await expect(page.locator('#background-disclosure')).toBeVisible();
    await page.locator('#background-summary').click();
    const bgItem = page.locator('.bg-item').first();
    await expect(bgItem).toBeVisible();

    // The one genuinely slow step in this whole suite - real stagenet blocks
    // land roughly every ~2 minutes.
    await expect(bgItem).toHaveClass(/is-paid/, { timeout: 5 * 60 * 1000 });
    await expect(bgItem).not.toHaveClass(/is-error/);

    // Spec point 11's own lifecycle: the stacked box clears itself a few
    // seconds after showing paid, rather than lingering forever.
    await expect(bgItem).toBeHidden({ timeout: 15_000 });
  });
});
