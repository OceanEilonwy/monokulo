const { test, expect, pauseClockAt } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

let fixture;
test.beforeEach(async ({ context }) => {
  fixture = await startCoverageFixture();
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
});
test.afterEach(async () => { await stopCoverageFixture(fixture?.process); });

function posUrl() { return `${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`; }

test('real POS backgrounds, reloads, reopens, cancels, and searches an order', async ({ page }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card .pos-qr svg')).toBeVisible();
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toContainText('Fixture o');
  await captureCoverageStage(page, 'pos-background-stack', test.info());
  await page.reload();
  await expect(page.locator('.pos-stack-card')).toBeVisible();
  await page.locator('.pos-stack-card').click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Fixture order');
  await expect(page.locator('.pos-pay-card .pos-qr svg')).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Cancelled');
  await captureCoverageStage(page, 'pos-cancelled', test.info());
  // The card stays, its code faded, and the stage says what happened.
  await expect(page.locator('.pos-pay-card.is-spent')).toBeVisible();
  await expect(page.locator('.pos-stage')).toContainText('Cancelled.');
  await page.getByRole('button', { name: 'New order' }).click();
  // Nothing is backgrounded any more, so the stack is gone; the top bar
  // still reaches the list.
  await expect(page.locator('.pos-stack')).toHaveCount(0);
  await page.getByRole('button', { name: 'All orders' }).click();
  await page.getByRole('tab', { name: /Finished/ }).click();
  await expect(page.locator('.pos-order-card .pos-badge')).toContainText('Cancelled');
  await page.getByRole('searchbox', { name: 'Search reference or order ID' }).fill('fixture');
  await expect(page.locator('.pos-order-card')).toContainText('Fixture order');
});

test('real POS header stays above its payment card and its actions stay reachable', async ({ page }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  for (const { width, height } of [{ width: 1126, height: 700 }, { width: 667, height: 375 }, { width: 360, height: 740 }]) {
    await page.setViewportSize({ width, height });
    const bounds = await page.evaluate(() => ({
      barBottom: document.querySelector('.pos-top').getBoundingClientRect().bottom,
      cardTop: document.querySelector('.pos-pay-card').getBoundingClientRect().top,
      pageScrolls: document.documentElement.scrollHeight > innerHeight + 1,
    }));
    expect(bounds.cardTop).toBeGreaterThanOrEqual(bounds.barBottom);
    expect(bounds.pageScrolls, 'the page itself never scrolls; the payment panel does').toBe(false);
    await page.getByRole('button', { name: 'Cancel order' }).scrollIntoViewIfNeeded();
    await expect(page.getByRole('button', { name: 'Cancel order' })).toBeInViewport();
  }
});

test('real POS displays a shortened ID for an order without a reference', async ({ page }) => {
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await page.getByRole('button', { name: '1', exact: true }).click();
  await expect(page.locator('#pos-reference')).toHaveValue('');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText(/^#[0-9a-f]{4}…[0-9a-f]{4}$/);
  await expect(page.locator('.pos-pay-card .pos-qr svg')).toBeVisible();
});

test('real POS opens an empty keypad when its order list is empty', async ({ page }) => {
  await page.route('**/pos/orders?*', route => route.fulfill({ json: { orders: [], total: 0 } }));
  await page.goto(posUrl());
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await expect(page.locator('.pos-stack-card')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Charge' })).toBeDisabled();
});

test('real POS shows pending, partial, confirming, and terminal badge symbols', async ({ page }) => {
  const states = [
    ['pending', 'pending'], ['unconfirmed', 'unconfirmed'], ['confirming', 'confirming'],
    ['partial', 'partial'], ['paid', 'paid'], ['overpaid', 'overpaid'], ['expired', 'expired'],
    ['double', 'pending'], ['cancelled', 'pending'],
  ];
  // Every order starts open, as the server lists them; the final ones then
  // arrive over the live stream, as they would at the counter.
  const orders = states.map(([id], index) => ({
    order_id: id, merchant_order_id: id, address: '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC',
    amount: '1.00', currency: 'XMR', xmr_amount: '1.000000000000', status: ['paid', 'overpaid', 'expired'].includes(id) ? 'pending' : states[index][1],
    confirmations: id === 'confirming' ? 3 : 0, confirmations_required: 10,
    error: id === 'double' ? 'Double-spend detected on this payment.' : null,
    backgrounded: true, cancelled_at: null,
    created_at: 1000 + index, expires_at: 9999999999, updated_at: 1000 + index,
  }));
  await page.addInitScript(() => {
    window.EventSource = class {
      constructor() { window.__statusListeners = []; }
      addEventListener(name, listener) { if (name === 'status') window.__statusListeners.push(listener); }
      close() {}
    };
    window.__status = update => window.__statusListeners.forEach(listener => listener({ data: JSON.stringify(update) }));
  });
  await page.route('**/pos/orders?*', route => route.fulfill({ json: { orders, total: orders.length } }));
  await page.goto(posUrl());
  await expect(page.locator('.pos-stack-card')).toHaveCount(9);
  await page.evaluate(() => {
    for (const status of ['paid', 'overpaid', 'expired']) window.__status({ order_id: status, status, updated_at: 5000, is_terminal: true });
    window.__status({ order_id: 'cancelled', status: 'pending', cancelled_at: 2000, updated_at: 5000, is_terminal: false });
  });
  await expect(page.locator('.pos-stack-card')).toHaveCount(5);
  await expect(page.locator('.state-partial .pos-icon use')).toHaveAttribute('href', '#pos-coin-partial');
  await expect(page.locator('.state-confirming .pos-disc')).toHaveAttribute('style', /--progress: 30%/);
  await page.getByRole('button', { name: 'View all →' }).click();
  await expect(page.locator('.pos-order-card .pos-badge.state-partial use')).toHaveAttribute('href', '#pos-coin-partial');
  await page.getByRole('tab', { name: /Finished/ }).click();
  for (const name of ['paid', 'overpaid', 'expired', 'cancelled']) {
    await expect(page.locator(`.pos-order-card .pos-badge.state-${name}`)).toHaveCount(1);
  }
});

test('real POS payment card copies the address and saves a refund address', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto(posUrl());
  const card = page.locator('.pos-pay-card');
  await expect(card.locator('.pos-stage-msg')).toContainText(/(59m|1h) left/);
  const address = await card.locator('.pos-address code').getAttribute('title');
  await card.getByRole('button', { name: 'Copy payment address' }).click();
  await expect(card.getByRole('button', { name: 'Copy payment address' })).toHaveText('Copied');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(address);
  // The order's own address is a valid address on the store's network.
  await card.locator('#pos-refund').fill(address);
  await expect(card.locator('.pos-refund-state')).toHaveAttribute('aria-label', 'Refund address saved');
  await page.reload();
  await expect(page.locator('#pos-refund')).toHaveValue(address);
  await expect(page.locator('.pos-refund-state')).toHaveAttribute('aria-label', 'Refund address saved');
});

test('real POS uses the site theme toggle, applies it in place and remembers it', async ({ page }) => {
  await page.goto(posUrl());
  const toggle = page.locator('.pos-top .theme-toggle');
  await expect(toggle).toHaveClass(/theme-toggle-system/);
  await page.getByRole('button', { name: 'Light theme' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(toggle).toHaveClass(/theme-toggle-light/);
  // Applied in place: the terminal is not reloaded, so the order on screen stays.
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  // Remembered: the choice is saved to the account, and a reload shows it.
  const saved = page.waitForResponse(response => response.url().endsWith('/dashboard/theme')
    && response.request().method() === 'POST' && new URLSearchParams(response.request().postData()).get('theme') === 'dark');
  await page.getByRole('button', { name: 'Dark theme' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(page.getByRole('button', { name: 'Dark theme' })).toHaveAttribute('aria-pressed', 'true');
  const paper = await page.evaluate(() => getComputedStyle(document.getElementById('pos-root')).backgroundColor);
  expect(paper).toBe('rgb(30, 30, 30)');
  expect((await saved).status(), 'the theme is saved').toBeLessThan(400);
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(toggle).toHaveClass(/theme-toggle-dark/);
  await page.getByRole('button', { name: 'System theme' }).click();
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', /./);
});

test('real POS header is the site app bar: the mark, the store, a POS label and the status indicator', async ({ page }) => {
  await page.route('**/status/summary', route => route.fulfill({ json: { healthy: true, state: 'ok' } }));
  await page.goto(posUrl());
  const top = page.locator('.pos-top');
  // The site's own mark and name first, linking to the dashboard, as the
  // nav does on every page.
  await expect(top.locator('.pos-brand')).toHaveAttribute('href', '/dashboard');
  await expect(top.locator('.pos-brand svg.logo-mark')).toBeVisible();
  await expect(top.locator('.pos-store')).toHaveAttribute('href', `/dashboard/stores/${fixture.connection_id}`);
  await expect(top.locator('.pos-mode')).toHaveText('POS');
  expect(await page.locator('#pos-site-brand').count()).toBe(0);
  const status = top.locator('#status-indicator');
  await expect(status).toHaveAttribute('href', '/status');
  // Rightmost in the bar, after the theme toggle, as on every page.
  expect(await page.evaluate(() => {
    const items = [...document.querySelectorAll('.pos-top a, .pos-top button, .pos-top .theme-toggle')].filter(el => el.getBoundingClientRect().width);
    return items.sort((a, b) => a.getBoundingClientRect().right - b.getBoundingClientRect().right).pop().id;
  })).toBe('status-indicator');
  await expect(status.locator('.status-dot')).toHaveClass(/status-dot-ok/);
  await status.click();
  await expect(page).toHaveURL(/\/status$/);
});

test('real dashboard health indicator follows healthy and unavailable polls', async ({ page }) => {
  let health = true;
  let polls = 0;
  let releaseFirst;
  await page.clock.install();
  await page.route('**/status/summary', async route => {
    polls++;
    if (polls === 1) await new Promise(resolve => { releaseFirst = resolve; });
    return health === null
      ? route.fulfill({ status: 503, contentType: 'text/plain', body: 'Unavailable' })
      : route.fulfill({ json: { healthy: health, state: health ? 'ok' : 'error' } });
  });
  await page.goto(`${fixture.base_url}/dashboard`);
  const dot = page.locator('#status-indicator .status-dot');
  await expect(dot).toBeVisible();
  await expect(dot).toHaveClass(/status-dot-unknown/);
  await expect.poll(() => polls).toBeGreaterThan(0);
  releaseFirst();
  await expect(dot).toHaveClass(/status-dot-ok/);
  health = null;
  await page.clock.runFor(30000);
  await expect(dot).toHaveClass(/status-dot-unknown/);
  await expect(page.locator('#status-indicator')).toHaveAttribute('title', 'could not check status');
});

/** A payment of `fraction` of the order's amount, with `confirmations`
 * (undefined: seen in the mempool only). */
async function payment(request, orderId, fraction, confirmations) {
  const depth = confirmations === undefined ? '' : `&confirmations=${confirmations}`;
  const response = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/payment?fraction=${fraction}${depth}`);
  expect(response.status()).toBe(204);
}

test('merchant rings up a sale on a physical keyboard', async ({ page }) => {
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  const amount = page.locator('.pos-amount');
  await expect(amount).toContainText('0.000000000000');
  // A typo, cleared with Backspace; then a wrong amount, cleared with Escape.
  await page.keyboard.type('129');
  await page.keyboard.press('Backspace');
  await expect(amount).toContainText('0.000000000012');
  await page.keyboard.press('Escape');
  await expect(amount).toContainText('0.000000000000');
  // Enter with nothing entered does nothing.
  await page.keyboard.press('Enter');
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await page.keyboard.type('250000000000');
  await expect(amount).toContainText('0.250000000000');
  await captureCoverageStage(page, 'pos-keypad', test.info());
  // Typing a reference and pressing Enter there charges too.
  await page.getByPlaceholder('E.g. customer name or note').fill('Table 4');
  await page.getByPlaceholder('E.g. customer name or note').press('Enter');
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Table 4');
  await expect(page.locator('.pos-pay-xmr')).toContainText('0.25');
  await captureCoverageStage(page, 'pos-awaiting-payment', test.info());
  // An XMR store's order has no "≈" fiat line.
  await expect(page.locator('.pos-pay-fiat')).toHaveCount(0);
  // Digits typed on the payment screen do not leak into a new sale.
  await page.keyboard.type('9');
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(amount).toContainText('0.000000000000');
});

test('customer pays while the order is on screen and the merchant starts the next sale', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  // Seen in the mempool first: the card says so and cancelling is no longer offered.
  await payment(request, fixture.order_id, 1);
  await expect(page.locator('.pos-stage-msg')).toHaveText('Waiting for confirmation. Payment seen. Waiting for its first confirmation.');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  // Seen in full: the code fades so it isn't paid twice.
  await expect(page.locator('.pos-pay-card.is-spent')).toBeVisible();
  await expect(page.locator('.pos-pay-caption')).toHaveText('Order total');
  await captureCoverageStage(page, 'pos-unconfirmed', test.info());
  // Then mined into a block: the stage says it's paid, on the same card.
  expect((await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/confirm`)).status()).toBe(204);
  await expect(page.locator('.pos-stage')).toContainText('received and confirmed.');
  await expect(page.locator('.pos-track .step-done')).toHaveCount(3);
  await captureCoverageStage(page, 'pos-paid', test.info());
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Paid');
  await page.getByRole('button', { name: 'New order' }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
});

test('a delayed order read cannot undo a live payment, but a later reorg can', async ({ page, request }) => {
  const initial = await (await request.get(`${posUrl()}/orders/${fixture.order_id}`,
    { headers: { cookie: `session=${fixture.session}` } })).json();
  await page.addInitScript(() => {
    window.EventSource = class {
      constructor() { window.__statusListeners = []; }
      addEventListener(name, listener) { if (name === 'status') window.__statusListeners.push(listener); }
      close() {}
    };
    window.__status = update => window.__statusListeners.forEach(listener => listener({ data: JSON.stringify(update) }));
  });
  let release;
  const delayed = new Promise(resolve => { release = resolve; });
  let started;
  const reading = new Promise(resolve => { started = resolve; });
  await page.route(`**/pos/orders/${fixture.order_id}`, async route => {
    started();
    await delayed;
    await route.fulfill({ json: initial });
  });
  await page.goto(posUrl());
  await reading;
  await page.waitForFunction(() => window.__statusListeners?.length > 0);
  // Equal timestamps reproduce a read and a payment within the same second.
  await page.evaluate(({ qr_svg, ...order }) => window.__status({ ...order, status: 'unconfirmed',
    received_xmr: order.xmr_amount, remaining_xmr: '0.000000000000', is_terminal: false }), initial);
  await expect(page.locator('.pos-stage-msg')).toHaveText('Waiting for confirmation. Payment seen. Waiting for its first confirmation.');
  const response = page.waitForResponse(`**/pos/orders/${fixture.order_id}`);
  release();
  await (await response).finished();
  // Wait for the application's fetch/JSON continuation, not just the response headers.
  await page.waitForFunction(() => document.querySelector('.pos-qr svg'));
  await expect(page.locator('.pos-stage-msg')).toHaveText('Waiting for confirmation. Payment seen. Waiting for its first confirmation.');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  // No monotonic-status assumption: a subsequent stream snapshot may regress.
  await page.evaluate(order => window.__status({ ...order, is_terminal: false }), initial);
  await expect(page.locator('.pos-stage-msg')).toContainText('Send 0.001 XMR.');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toBeVisible();
});

test('customer underpays: the card asks for the rest and the order cannot be cancelled', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  const code = () => page.locator('.pos-qr').innerHTML();
  const first = await code();
  await payment(request, fixture.order_id, 0.4, 20);
  await expect(page.locator('.pos-stage')).toContainText('0.0004 of 0.001 XMR received');
  await expect(page.locator('.pos-pay-card')).toContainText('0.0006');
  // The live update brings a new code, for the rest, and says so.
  await expect(page.locator('.pos-qr-new-tab')).toHaveText('New code · 0.0006 XMR');
  expect(await code()).not.toBe(first);
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  await expect(page.locator('.pos-action-hint')).toContainText('Background keeps this payment open');
  await captureCoverageStage(page, 'pos-underpaid', test.info());
  // The customer sends the rest.
  await payment(request, fixture.order_id, 0.6, 20);
  await expect(page.locator('.pos-stage')).toContainText('received and confirmed.');
});

test('customer walks away: the order on screen expires', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/expired`);
  await expect(page.locator('.pos-stage')).toContainText('This payment expired before it was completed.');
  await expect(page.locator('.pos-pay-card.is-spent')).toBeVisible();
  await captureCoverageStage(page, 'pos-expired', test.info());
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Expired');
});

test('counter loses its connection: the order shows connection lost, then recovers by itself', async ({ page, request }) => {
  let online = true;
  // The Wi-Fi is down: every attempt to (re)open the update stream fails.
  await page.route('**/pos/events?*', route => (online ? route.continue() : route.abort('internetdisconnected')));
  online = false;
  // The page's own record of the stream failing: its EventSource's error
  // events. A listener added in the constructor runs before the app's, in
  // the same dispatch, so once the test sees a failure the app has handled
  // it (and started its 6s count).
  await page.addInitScript(() => {
    const Native = window.EventSource;
    window.__streamFailures = 0;
    window.EventSource = class extends Native {
      constructor(...args) {
        super(...args);
        this.addEventListener('error', () => { window.__streamFailures++; });
      }
    };
  });
  // The page's time stands still from the start: only the test moves it.
  // The browser retries the stream on its own clock, which is real; the
  // 6s after which the merchant is told is the page's.
  const start = new Date('2026-01-01T00:00:00Z');
  await pauseClockAt(page, start);
  await page.goto(posUrl());
  const badge = page.locator('.pos-order-heading .pos-badge');
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.__streamFailures), 'the app sees the stream fail').toBeGreaterThan(0);
  // Retries fail every few seconds; once 6s have passed without a
  // connection the merchant is told, however many retries that took: not a
  // millisecond before.
  await page.clock.runFor(5999);
  await expect(badge).toContainText('Awaiting payment');
  await page.clock.runFor(1);
  await expect(badge).toContainText('Connection lost');
  await captureCoverageStage(page, 'pos-connection-lost', test.info());
  // The customer pays meanwhile; back online, the stream reconnects (the
  // browser's own retry, a few real seconds) and brings the missed payment
  // in without a reload.
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/payment?fraction=1`);
  online = true;
  await expect(badge).toContainText('Unconfirmed', { timeout: 15000 });
  await expect(page.locator('.pos-stage-msg')).toContainText('Payment seen. Waiting for its first confirmation.');
});

test('merchant opens Cancel order then changes their mind: nothing happens until they confirm', async ({ page }) => {
  const writes = [];
  page.on('request', sent => { if (sent.method() !== 'GET') writes.push(sent.url()); });
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  page.once('dialog', dialog => dialog.dismiss());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-error')).toHaveCount(0);
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Awaiting payment');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toBeEnabled();
  // Confirmed the second time: the one write the server gets is that one.
  // A write the dismissed question had sent would have gone out first, so
  // this proves it sent none without waiting for nothing to happen.
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Cancelled');
  expect(writes).toEqual([`${posUrl()}/orders/${fixture.order_id}/cancel`]);
});

test('payment lands just before the merchant confirms a cancel: the server refuses and says why', async ({ page, request }) => {
  // The live update for the payment has not reached this screen yet.
  await page.route('**/pos/events?*', () => {});
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/payment?fraction=0.5`);
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-error')).toHaveText('This order has payment activity and cannot be cancelled. Background it for review instead.');
  // The screen catches up with the payment that caused the refusal.
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Partially paid');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  // Backgrounding it instead works, and it waits in the stack for review.
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toContainText('Fixture o');
});

async function ringUp(request, count, prefix) {
  for (let i = 1; i <= count; i++) {
    const created = await request.post(`${posUrl()}/orders`, { headers: { cookie: `session=${fixture.session}` },
      data: { amount: `0.00${i}`, merchant_order_id: `${prefix} ${i}`, request_key: `${prefix}-${i}` } });
    expect(created.ok()).toBeTruthy();
  }
}

test('busy store sees every open order at once and narrows them by search', async ({ page, request }) => {
  test.setTimeout(60000);
  await ringUp(request, 44, 'Table');
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'All orders' }).click();
  const cards = page.locator('.pos-order-card');
  // All 45 open orders (the fixture order and 44 tables): no paging.
  await expect(cards).toHaveCount(45);
  await expect(page.getByRole('tab', { name: /Active · 45/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Load more/ })).toHaveCount(0);
  await page.getByRole('searchbox', { name: 'Search reference or order ID' }).fill('Table 17');
  await expect(cards).toHaveCount(1);
  await cards.getByRole('button', { name: 'Open →' }).click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Table 17');
});

test('an order list that failed to load when the POS opened comes back with Retry', async ({ page }) => {
  let failures = 1;
  await page.route('**/pos/orders?state=active', route => (failures-- > 0
    ? route.fulfill({ status: 503, json: { error: 'The payment engine is busy or unreachable. Try again in a moment.' } })
    : route.continue()));
  await page.goto(posUrl());
  await expect(page.locator('.pos-error')).toContainText('The payment engine is busy');
  // The list is reachable even with nothing loaded, and Retry reloads it.
  await page.getByRole('button', { name: 'All orders' }).click();
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.locator('.pos-error')).toHaveCount(0);
  // The sale that was in progress when the POS opened is back on screen.
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Fixture order');
  await expect(page.locator('.pos-pay-card')).toBeVisible();
});

test('merchant records the customer refund address from a QR image, with clear failures', async ({ page }) => {
  const fixtures = require('node:path').join(__dirname, '../fixtures');
  const refundAddress = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
  await page.goto(posUrl());
  const card = page.locator('.pos-pay-card');
  const message = card.locator('.pos-refund-message');
  const input = card.locator('#pos-refund');
  async function choose(file) {
    const chooser = page.waitForEvent('filechooser');
    await card.getByRole('button', { name: 'Choose QR image' }).click();
    await (await chooser).setFiles(require('node:path').join(fixtures, file));
  }
  await choose('receipt-photo-no-qr.png');
  await expect(message).toHaveText('No QR code found in that image.');
  await choose('menu-link-qr.png');
  await expect(message).toHaveText('That QR code does not contain a Monero address.');
  // The till's connection drops as it saves.
  await page.route('**/refund-address', route => route.abort('internetdisconnected'));
  await choose('refund-qr-wallet-uri.png');
  await expect(input).toHaveValue(refundAddress);
  await expect(message).toHaveText('Could not save. Check the connection and try again.');
  // Back online. The customer first reads out a stagenet (test wallet)
  // address, which the server rejects for this mainnet store.
  await page.unroute('**/refund-address');
  await input.fill('54F1KdjaAtnL6Fb4SbLUM1AMQSjSERjYUgYRtVgwjBirA26RyJCzxc4TbWPW65ZvRC6bifBfrTTv3fyu25BFQuvA2ogNiXg');
  await expect(input).toHaveAttribute('aria-invalid', 'true');
  await expect(message).toHaveText("Enter a valid Monero address for this store's network.");
  // Then their real one.
  await input.fill('');
  await input.fill(refundAddress);
  await expect(card.locator('.pos-refund-state')).toHaveAttribute('aria-label', 'Refund address saved');
  await expect(message).toHaveCount(0);
});

test('charge whose response is lost is retried without creating a second order', async ({ page, request }) => {
  const created = [];
  let dropResponse = true;
  await page.route('**/pos/orders', async route => {
    if (route.request().method() !== 'POST') return route.continue();
    const body = JSON.parse(route.request().postData());
    // The server creates the order, but the till's connection drops before
    // the answer arrives.
    const response = await route.fetch();
    created.push({ key: body.request_key, id: (await response.json()).order_id });
    if (dropResponse) { dropResponse = false; return route.abort('connectionreset'); }
    return route.fulfill({ response });
  });
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await page.keyboard.type('70000000000');
  await page.getByPlaceholder('E.g. customer name or note').fill('Walk-in');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-keypad .pos-error')).toBeVisible();
  // The merchant taps Charge again for the same sale.
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Walk-in');
  expect(created).toHaveLength(2);
  expect(created[1].key).toBe(created[0].key);
  expect(created[1].id).toBe(created[0].id);
  const list = await (await request.get(`${posUrl()}/orders?search=Walk-in`, { headers: { cookie: `session=${fixture.session}` } })).json();
  expect(list.total).toBe(1);
  // A different sale afterwards gets its own order.
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await page.keyboard.type('5');
  await page.getByPlaceholder('E.g. customer name or note').fill('Walk-in 2');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Walk-in 2');
  expect(created[2].key).not.toBe(created[0].key);
});

test('payment waiting on the store\'s confirmations shows its progress', async ({ page, request }) => {
  // The merchant asks for 3 confirmations on the store's settings page.
  const saved = await request.post(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/settings/confirmations`,
    { headers: { cookie: `session=${fixture.session}` }, form: { confirmations_required: '3' }, maxRedirects: 0 });
  expect(saved.status()).toBeLessThan(400);
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await page.keyboard.type('30000000000');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  const heading = await page.locator('.pos-order-heading p').textContent();
  const list = await (await request.get(`${posUrl()}/orders`, { headers: { cookie: `session=${fixture.session}` } })).json();
  const order = list.orders.find(o => !o.backgrounded && o.status === 'pending');
  expect(heading).toContain(order.order_id.slice(-4));
  expect(order.confirmations_required).toBe(3);
  await payment(request, order.order_id, 1, 1);
  await expect(page.locator('.pos-stage-msg')).toHaveText('Confirming. 1 of 3 confirmations.');
  await expect(page.locator('.pos-track .step-now')).toContainText('Confirm');
  await captureCoverageStage(page, 'pos-confirming', test.info());
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Confirming');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
});

test('double spend on the order on screen warns the merchant not to hand over goods', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await payment(request, fixture.order_id, 1);
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Unconfirmed');
  const flagged = await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/double-spend`);
  expect(flagged.status()).toBe(204);
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Double spend');
  await expect(page.locator('.pos-stage')).toContainText('Do not treat it as paid');
  await captureCoverageStage(page, 'pos-double-spend', test.info());
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
});

test('store priced in AUD: the merchant keys in dollars and cents and sees both amounts throughout', async ({ page, request }) => {
  // The merchant switches the store to Australian dollars on its settings page.
  const saved = await request.post(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/settings/base-currency`,
    { headers: { cookie: `session=${fixture.session}` }, form: { base_currency: 'AUD' }, maxRedirects: 0 });
  expect(saved.status()).toBeLessThan(400);
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  const amount = page.locator('.pos-amount');
  await expect(amount).toContainText('0.00');
  await expect(amount).toContainText('AUD');
  await page.keyboard.type('1250');
  await expect(amount).toContainText('12.50');
  await page.getByRole('button', { name: 'Charge' }).click();
  const card = page.locator('.pos-pay-card');
  // 12.50 AUD at 400 AUD per XMR.
  await expect(card.locator('.pos-pay-xmr')).toContainText('0.03125');
  await expect(card.locator('.pos-pay-fiat')).toHaveText('≈ 12.50 AUD');
  const list = await (await request.get(`${posUrl()}/orders`, { headers: { cookie: `session=${fixture.session}` } })).json();
  const order = list.orders.find(o => o.currency === 'AUD');
  expect(order.amount).toBe('12.50');
  // Seen in the mempool, then confirmed: the order total and what it's worth stay.
  await request.post(`${fixture.base_url}/__coverage/orders/${order.order_id}/payment?fraction=1`);
  await expect(card.locator('.pos-pay-caption')).toHaveText('Order total');
  await expect(card.locator('.pos-pay-fiat')).toHaveText('≈ 12.50 AUD');
  await request.post(`${fixture.base_url}/__coverage/orders/${order.order_id}/confirm`);
  await expect(page.locator('.pos-stage')).toContainText('0.03125 XMR received and confirmed.');
  await expect(card.locator('.pos-pay-xmr')).toContainText('0.03125');
});

test('payment countdown keeps ticking down while the customer finds their wallet', async ({ page, request }) => {
  // The fixture order expires an hour after it was made, moments ago.
  const order = await (await request.get(`${posUrl()}/orders/${fixture.order_id}`, { headers: { cookie: `session=${fixture.session}` } })).json();
  // The page's time stands still from the start: only the test moves it.
  const start = new Date();
  await pauseClockAt(page, start);
  await page.goto(posUrl());
  // In the stage's message.
  const expiry = page.locator('.pos-stage-msg');
  await expect(expiry).toContainText(/(59m|1h) left/);
  await page.clock.runFor(20 * 60 * 1000);
  await expect(expiry).toContainText(/(39|40)m left/);
  // To half a minute before it expires. The message follows a clock that
  // ticks every 15s, so it shows between 30 and 45 seconds left.
  await page.clock.runFor(order.expires_at * 1000 - 30_000 - (await page.evaluate(() => Date.now())));
  await expect(expiry).toContainText('less than a minute left');
});

test('merchant moves between the keypad, the stack and the order list', async ({ page, request }) => {
  await ringUp(request, 12, 'Queue');
  // A narrow desktop window: the stack is a strip above the keypad (on a
  // wider screen it is a sidebar), and a mouse wheel scrolls it sideways.
  await page.setViewportSize({ width: 480, height: 820 });
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  // On a desktop till the mouse wheel scrolls the stack of background orders sideways.
  const stack = page.locator('.pos-stack-scroll');
  await expect(stack).toBeVisible();
  const before = await stack.evaluate(el => el.scrollLeft);
  await stack.hover();
  await page.mouse.wheel(0, 300);
  await expect.poll(() => stack.evaluate(el => el.scrollLeft)).toBeGreaterThan(before);
  await page.getByRole('button', { name: 'View all →' }).click();
  await expect(page.locator('.pos-list h1')).toHaveText('Orders');
  await page.getByRole('tab', { name: /Finished/ }).click();
  await expect(page.locator('.pos-empty')).toHaveText('No finished orders yet.');
  await page.getByRole('tab', { name: /Active/ }).click();
  await expect(page.locator('.pos-order-card').first()).toBeVisible();
  await page.getByRole('button', { name: 'Back to POS' }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
});

test('merchant denies the camera or picks a broken photo: the card says what to do instead', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator.mediaDevices, 'getUserMedia', { value: async () => { throw new DOMException('Permission denied', 'NotAllowedError'); } });
  });
  await page.goto(posUrl());
  const card = page.locator('.pos-pay-card');
  await card.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(card.locator('.pos-refund-message')).toHaveText('Camera unavailable. Choose a QR image instead.');
  await expect(card.getByRole('button', { name: 'Scan refund QR' })).toBeVisible();
  const chooser = page.waitForEvent('filechooser');
  await card.getByRole('button', { name: 'Choose QR image' }).click();
  await (await chooser).setFiles(require('node:path').join(__dirname, '../fixtures/corrupt-photo.png'));
  await expect(card.locator('.pos-refund-message')).toHaveText('Could not read that image. Choose another file.');
});

test('clearing a search brings the whole order list back', async ({ page, request }) => {
  await ringUp(request, 3, 'Table');
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'All orders' }).click();
  const cards = page.locator('.pos-order-card');
  await expect(cards).toHaveCount(4);
  const search = page.getByRole('searchbox', { name: 'Search reference or order ID' });
  await search.fill('Table 2');
  await expect(cards).toHaveCount(1);
  await search.fill('nothing like this');
  await expect(page.locator('.pos-empty')).toHaveText('No matches from this session. Search all orders →');
  await search.fill('');
  await expect(cards).toHaveCount(4);
});

async function openFinishedTab(page) {
  await page.getByRole('button', { name: 'All orders' }).click();
  await page.getByRole('tab', { name: /Finished/ }).click();
}

test('an order finished at the counter moves to Finished for this session only', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/paid`);
  await expect(page.locator('.pos-stage')).toContainText('Paid.');
  await page.getByRole('button', { name: 'New order' }).click();
  await openFinishedTab(page);
  await expect(page.getByRole('tab', { name: 'Finished · 1' })).toBeVisible();
  await expect(page.locator('.pos-order-card')).toContainText('Fixture order');
  const note = page.locator('.pos-list-note');
  await expect(note).toContainText('Completed on this device since the POS was opened. They clear after 24 hours or when the page reloads.');
  await expect(note.getByRole('link', { name: 'See all orders →' })).toHaveAttribute('href', `/dashboard/stores/${fixture.connection_id}/orders`);
  await captureCoverageStage(page, 'pos-finished-tab', test.info());
  await page.getByRole('tab', { name: /Active/ }).click();
  await expect(page.locator('.pos-order-card')).toHaveCount(0);
  // Nothing is loaded into Finished: after a reload it starts empty.
  await page.reload();
  await openFinishedTab(page);
  await expect(page.getByRole('tab', { name: 'Finished · 0' })).toBeVisible();
  await expect(page.locator('.pos-empty')).toHaveText('No finished orders yet.');
  await expect(page.locator('.pos-list-note')).toBeVisible();
});

test('a backgrounded order that is paid while the merchant serves someone else moves to Finished', async ({ page, request }) => {
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toHaveCount(1);
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/paid`);
  await expect(page.locator('.pos-stack-card')).toHaveCount(0);
  await openFinishedTab(page);
  await expect(page.locator('.pos-order-card .pos-badge')).toContainText('Paid');
});

test('finished orders drop off the tab 24 hours after they finished', async ({ page, request }) => {
  await page.clock.install();
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/paid`);
  await expect(page.locator('.pos-stage')).toContainText('Paid.');
  await page.getByRole('button', { name: 'New order' }).click();
  await openFinishedTab(page);
  await expect(page.locator('.pos-order-card')).toHaveCount(1);
  await page.clock.fastForward((23 * 60 + 58) * 60 * 1000);
  await expect(page.locator('.pos-order-card')).toHaveCount(1);
  await page.clock.fastForward(3 * 60 * 1000);
  await expect(page.locator('.pos-order-card')).toHaveCount(0);
  await expect(page.getByRole('tab', { name: 'Finished · 0' })).toBeVisible();
});

test('a search with no match in this session links to searching all orders', async ({ page }) => {
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'All orders' }).click();
  await page.getByRole('searchbox', { name: 'Search reference or order ID' }).fill('wc-1042');
  const empty = page.locator('.pos-empty');
  await expect(empty).toHaveText('No matches from this session. Search all orders →');
  await expect(empty.getByRole('link', { name: 'Search all orders →' })).toHaveAttribute('href', `/dashboard/stores/${fixture.connection_id}/orders?q=wc-1042`);
  await empty.getByRole('link', { name: 'Search all orders →' }).click();
  await expect(page.getByRole('heading', { name: 'Orders' })).toBeVisible();
  await expect(page.getByRole('searchbox', { name: 'Search orders' })).toHaveValue('wc-1042');
  await expect(page.getByText('No orders match “wc-1042”.')).toBeVisible();
});

test('on a tablet or desktop the POS uses the whole screen, with the open orders beside the keypad and a one-column payment', async ({ page, request }) => {
  await ringUp(request, 2, 'Table');
  const box = selector => page.locator(selector).first().evaluate(el => { const r = el.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, right: r.right }; });
  const noOuterScroll = () => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1 && document.documentElement.scrollHeight <= innerHeight + 1);
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toHaveCount(3);
  for (const [shape, width, height, size] of [['iPad portrait', 820, 1180, 'tablet-portrait'], ['iPad landscape', 1180, 820, 'tablet-landscape'], ['desktop', 1280, 800, 'desktop']]) {
    await page.setViewportSize({ width, height });
    await expect(page.locator('.pos-keypad')).toBeVisible();
    const top = await box('.pos-top');
    expect(top.w, `${shape}: top bar across the screen`).toBeGreaterThanOrEqual(width - 1);
    const side = await box('.pos-stack');
    const keypad = await box('.pos-keypad');
    expect(side.x, `${shape}: sidebar at the left`).toBeLessThan(1);
    expect(side.h, `${shape}: sidebar full height`).toBeGreaterThan(height - top.h - 2);
    expect(keypad.x, `${shape}: keypad beside the sidebar`).toBeGreaterThanOrEqual(side.right - 1);
    expect(await noOuterScroll(), `${shape}: keypad`).toBe(true);
    await page.locator('.pos-stack-card').first().click();
    await expect(page.locator('.pos-pay-card')).toBeVisible();
    expect((await box('.pos-stack')).x, `${shape}: sidebar beside the payment`).toBeLessThan(1);
    const card = await box('.pos-pay-card');
    const heading = await box('.pos-order-heading');
    // One column wherever it fits: the heading, the card, then its actions.
    expect(heading.y, `${shape}: card under the heading`).toBeLessThan(card.y);
    expect((await box('.pos-actions')).y, `${shape}: actions under the card`).toBeGreaterThan(card.y + card.h - 1);
    await expect(page.getByRole('button', { name: 'Background order', exact: true }), `${shape}: actions on screen`).toBeInViewport({ ratio: 1 });
    expect(await noOuterScroll(), `${shape}: payment`).toBe(true);
    // One stage, at this size only: each size is one of the gallery's.
    await captureCoverageStage(page, 'pos-tablet-payment', test.info(), { shapes: [size] });
    await page.getByRole('button', { name: 'Background order', exact: true }).click();
    await expect(page.locator('.pos-keypad')).toBeVisible();
  }
  await page.getByRole('button', { name: 'All orders' }).click();
  const cards = await page.locator('.pos-order-card').evaluateAll(els => els.map(el => el.getBoundingClientRect().y));
  expect(new Set(cards.map(Math.round)).size, 'several cards to a row').toBeLessThan(cards.length);
  // On a phone the payment view has no room for the sidebar.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator('.pos-order-card').first().getByRole('button', { name: 'Open →' }).click();
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await expect(page.locator('.pos-stack')).toBeHidden();
});
