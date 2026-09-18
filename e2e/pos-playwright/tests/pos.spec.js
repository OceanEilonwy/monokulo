// @ts-check
// Real stagenet + real browser e2e for the POS terminal screen
// (templates/pos.html.hbs). See ../README.md for what this needs and how to
// run it - deliberately never wired into any default `npm test`/CI.
//
// The backend (a real, network-bound engine against the real public
// stagenet node, plus a real, network-bound monokulo with one account/store
// already connected) is booted once by ../global-setup.js
// (crates/scanner/src/bin/pos_e2e_server.rs) and shared by both tests below -
// they run sequentially (see playwright.config.js: fullyParallel: false,
// workers: 1), never concurrently, since they share one real backend and one
// real customer wallet.
//
// Login happens through the real `/dashboard/login` form in the browser
// (not injected cookies) - the one part of setup worth actually driving
// through the UI, since it's cheap and it's real production code a merchant
// depends on before they ever reach the POS screen. Account/store creation
// itself is not (that's `pos_e2e_server.rs`'s job, over plain HTTP, not the
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

  await page.goto(`${fixture.monokulo_base_url}/dashboard/connections/${fixture.connection_id}/pos`);
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
    `${fixture.monokulo_base_url}/dashboard/connections/${fixture.connection_id}/settings/confirmations`,
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
  test('a 0-conf-trusted payment shows the tick then auto-returns to the keypad', async ({ page, context }) => {
    // Real decoy selection alone can take up to ~600s in the worst case
    // (see StagenetSpendWallet::connect's own comment on why its reqwest
    // client's timeout is that generous) - budget well past that, not just
    // past the "observed ~250s typical" figure.
    test.setTimeout(12 * 60 * 1000);

    await loginAndOpenPos(page);
    await setConfirmationsRequired(context, 0);
    // The threshold change above doesn't affect an already-open POS screen's
    // own in-memory state (there isn't any to affect - it only matters at
    // order-creation time), so no reload is needed before charging.

    const order = await chargeAndGetOrder(page, '335000000'); // 0.000335 XMR
    const piconero = piconeroFromXmrDisplay(order.xmr_amount);
    expect(piconero).toBe(335_000_000n);

    // Spec point 6: a real QR code and a real, amount-carrying monero: URI.
    await expect(page.locator('#qr-holder svg')).toBeVisible();
    expect(order.monero_uri).toBe(`monero:${order.address}?tx_amount=${order.xmr_amount}`);

    console.log(`sending real stagenet payment: ${piconero} piconero to ${order.address}`);
    const txHash = await sendStagenetPayment(fixture.send_payment_url, order.address, piconero);
    console.log(`sent - tx ${txHash}`);

    // Spec point 7: the tick appears the moment a real tx is seen in the
    // mempool - 0-conf, before any confirmations at all.
    await expect(page.locator('#tick-overlay')).toBeVisible({ timeout: 90_000 });
    await expect(page.locator('#tick-overlay')).not.toHaveClass(/is-error/);

    // Spec point 8: this store's confirmations_required is 0, so the order
    // settles to "paid" with no further real confirmations needed - the
    // overlay closes and the keypad returns on its own, no merchant action
    // needed. Not necessarily on the very same scan tick that first saw it
    // in the mempool (status can take one more tick to settle from
    // "unconfirmed" to "paid" even at a 0 threshold), so this gets real
    // margin, not just the ~3s scan interval plus the client's own 2.5s
    // auto-dismiss delay.
    await expect(page.locator('#keypad-screen')).toBeVisible({ timeout: 60_000 });
    await expect(page.locator('#tick-overlay')).toBeHidden();

    // Spec point 5: a POS-created order is a real order, visible on the
    // normal dashboard orders list, not something private to this screen.
    const ordersPage = await context.request.get(`${fixture.monokulo_base_url}/dashboard/connections/${fixture.connection_id}/orders`);
    expect(await ordersPage.text()).toContain(order.payment_id);
  });

  test('a confirming payment shows the progress ring, can be backgrounded, and completes in the stack', async ({ page, context }) => {
    // Real decoy selection (up to ~600s worst case) *plus* waiting for one
    // real stagenet confirmation (up to ~5min budgeted below) - the two
    // genuinely slow steps in this whole suite, back to back.
    test.setTimeout(18 * 60 * 1000);

    await loginAndOpenPos(page);
    await setConfirmationsRequired(context, 1);

    const order = await chargeAndGetOrder(page, '336000000'); // 0.000336 XMR - a distinct amount from the first test's order
    const piconero = piconeroFromXmrDisplay(order.xmr_amount);

    console.log(`sending real stagenet payment: ${piconero} piconero to ${order.address}`);
    const txHash = await sendStagenetPayment(fixture.send_payment_url, order.address, piconero);
    console.log(`sent - tx ${txHash}`);

    // Spec point 7 again, then point 9: not yet fully confirmed (this
    // store's threshold is now 1), so a progress ring around the tick and a
    // "confirm in background" option must be offered rather than
    // auto-dismissing like the first test's 0-conf order did.
    await expect(page.locator('#tick-overlay')).toBeVisible({ timeout: 90_000 });
    await expect(page.locator('#background-btn')).toBeVisible({ timeout: 30_000 });

    // Spec point 10: backgrounding closes the overlay and returns to the keypad.
    await page.click('#background-btn');
    await expect(page.locator('#keypad-screen')).toBeVisible();
    await expect(page.locator('#tick-overlay')).toBeHidden();

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
