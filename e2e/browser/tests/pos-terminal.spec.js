const { test, expect, pauseClockAt, recordEventSources, eventSources, EVENT_SOURCE_CLOSED } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

let fixture;
test.beforeEach(async ({ context }) => {
  fixture = await startCoverageFixture();
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
});
test.afterEach(async () => { await stopCoverageFixture(fixture?.process); });

function posUrl() { return `${fixture.base_url}/stores/${fixture.connection_id}/pos`; }

/** Rings up `reference` and backgrounds it, so the background orders
 * sidebar (the POS's only way to its order list) stays on screen. */
async function keepOneInBackground(request, reference = 'Kept open') {
  const headers = { cookie: `session=${fixture.session}` };
  const created = await request.post(`${posUrl()}/orders`, { headers, data: { amount: '0.001', merchant_order_id: reference, request_key: reference.replace(/\W+/g, '-') } });
  expect(created.ok()).toBeTruthy();
  const { order_id } = await created.json();
  expect((await request.post(`${posUrl()}/orders/${encodeURIComponent(order_id)}/background`, { headers })).ok()).toBeTruthy();
}

async function openOrderList(page) {
  await page.getByRole('button', { name: 'View all →' }).click();
}

test('real POS backgrounds, reloads, reopens, cancels, and searches an order', async ({ page, request }) => {
  test.setTimeout(90_000);
  // A busy store: an order kept open in the background, and 44 tables.
  await keepOneInBackground(request);
  await ringUp(request, 44, 'Table');
  const writes = [];
  page.on('request', sent => { if (sent.method() !== 'GET') writes.push(sent.url()); });
  // The counter's tablet, on its side: the POS uses the whole screen, the
  // page itself never scrolls, the bar stays above the card, and the open
  // orders sit beside the payment, its actions on screen.
  await page.setViewportSize({ width: 1180, height: 820 });
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card .pos-qr svg')).toBeVisible();
  const box = selector => page.locator(selector).first().evaluate(el => { const r = el.getBoundingClientRect(); return { x: r.x, y: r.y, bottom: r.bottom }; });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1 && document.documentElement.scrollHeight <= innerHeight + 1), 'no outer scroll').toBe(true);
  expect((await box('.pos-pay-card')).y, 'card under the bar').toBeGreaterThanOrEqual((await box('.pos-top')).bottom);
  expect((await box('.pos-stack')).x, 'open orders beside the payment').toBeLessThan(1);
  await expect(page.getByRole('button', { name: 'Background order', exact: true })).toBeInViewport({ ratio: 1 });
  await captureCoverageStage(page, 'pos-tablet-payment', test.info(), { shapes: ['tablet-landscape'] });

  // Cancel order, then keep it: nothing happens until they confirm.
  await page.getByRole('button', { name: 'Cancel order…' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Keep it' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('.pos-error')).toHaveCount(0);
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Awaiting payment');

  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  const fixtureCard = page.locator('.pos-stack-card', { hasText: 'Fixture o' });
  await expect(fixtureCard).toBeVisible();
  await captureCoverageStage(page, 'pos-background-stack', test.info());
  await page.reload();
  await expect(fixtureCard).toBeVisible();
  await fixtureCard.click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Fixture order');
  await expect(page.locator('.pos-pay-card .pos-qr svg')).toBeVisible();
  writes.length = 0;
  await page.getByRole('button', { name: 'Cancel order…' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Cancelled');
  // The one write the server got is the confirmed cancel: the question kept
  // earlier sent none.
  expect(writes).toEqual([`${posUrl()}/orders/${fixture.order_id}/cancel`]);
  await captureCoverageStage(page, 'pos-cancelled', test.info());
  // The card stays, its code faded, and the stage says what happened.
  await expect(page.locator('.pos-pay-card.is-spent')).toBeVisible();
  await expect(page.locator('.mk-stage')).toContainText('Cancelled.');
  await page.getByRole('button', { name: 'New order' }).click();
  await expect(fixtureCard).toHaveCount(0);

  // A narrow desktop window: the stack is a strip above the keypad, and a
  // mouse wheel scrolls it sideways.
  await page.setViewportSize({ width: 480, height: 820 });
  const stack = page.locator('.pos-stack-scroll');
  await expect(stack).toBeVisible();
  const before = await stack.evaluate(el => el.scrollLeft);
  // Cards arriving from the live list can keep the strip moving: the
  // pointer goes over it as it is.
  await stack.hover({ force: true });
  await page.mouse.wheel(0, 300);
  await expect.poll(() => stack.evaluate(el => el.scrollLeft)).toBeGreaterThan(before);

  // The order list: the cancelled order under Finished.
  await openOrderList(page);
  await expect(page.locator('.pos-list h1')).toHaveText('Orders');
  const search = page.getByRole('searchbox', { name: 'Search reference or order ID' });
  const cards = page.locator('.mk-card');
  await page.getByRole('button', { name: /Finished/ }).click();
  await expect(page.locator('.mk-card .tag')).toContainText('Cancelled');
  await search.fill('fixture');
  await expect(cards).toContainText('Fixture order');
  await search.fill('');
  // Every open order at once (the one kept open and 44 tables): no paging.
  await page.getByRole('button', { name: /Active/ }).click();
  await expect(cards).toHaveCount(45);
  await expect(page.getByRole('button', { name: /Active · 45/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /Load more/ })).toHaveCount(0);
  await page.getByRole('button', { name: '‹ Back to the till' }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
  // Search narrows them, and clearing it brings them all back.
  await openOrderList(page);
  await search.fill('Table 17');
  await expect(cards).toHaveCount(1);
  await search.fill('');
  await expect(cards).toHaveCount(45);
  await search.fill('Table 17');
  await cards.getByRole('button', { name: 'Open →' }).click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText('Table 17');
  // A search with no match in this session links to searching all orders.
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await openOrderList(page);
  await search.fill('wc-1042');
  const empty = page.locator('.pos-empty');
  await expect(empty).toHaveText('No matches from this session. Search all orders →');
  await expect(empty.getByRole('link', { name: 'Search all orders →' })).toHaveAttribute('href', `/stores/${fixture.connection_id}/orders?q=wc-1042`);
  await empty.getByRole('link', { name: 'Search all orders →' }).click();
  await expect(page.getByRole('heading', { name: 'Orders' })).toBeVisible();
  await expect(page.getByRole('searchbox', { name: 'Search orders' })).toHaveValue('wc-1042');
  await expect(page.getByText('No orders match “wc-1042”.')).toBeVisible();
});

/** What the customer's checkout must show once the order is cancelled:
 * told so, nothing to pay (the code faded, the address struck through), and
 * no more refreshing for a browser without JavaScript. */
async function expectCancelledCheckout(checkout) {
  await expect(checkout.locator('#checkout-root')).toHaveAttribute('data-status', 'cancelled');
  await expect(checkout.locator('#live-status .mk-stage')).toContainText('This order was cancelled.');
  await expect(checkout.locator('#live-pay .qr-wrap.is-spent')).toHaveCount(1);
  await expect(checkout.locator('#live-address [data-void]')).toHaveCount(1);
  await expect(checkout.locator('#checkout-refresh')).toHaveCount(0);
}

test('cancelling on the POS reaches the customer\'s checkout live, after a reload, and without JavaScript', async ({ page, context, browser }) => {
  // The customer's checkout, open beside the till.
  const checkoutUrl = `${fixture.base_url}/checkout/${fixture.order_id}`;
  const checkout = await context.newPage();
  await checkout.goto(checkoutUrl);
  await expect(checkout.locator('#checkout-root')).toHaveAttribute('data-status', 'pending');
  await expect(checkout.locator('#live-address [data-void]')).toHaveCount(0);

  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await page.getByRole('button', { name: 'Cancel order…' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Cancelled');

  // Live, without the customer doing anything.
  await expectCancelledCheckout(checkout);
  await captureCoverageStage(checkout, 'checkout-cancelled', test.info());
  // And after a reload.
  await checkout.reload();
  await expectCancelledCheckout(checkout);
  // And for a customer with JavaScript off (Tor Browser's safest setting).
  const noJs = await browser.newContext({ javaScriptEnabled: false });
  try {
    const plain = await noJs.newPage();
    await plain.goto(checkoutUrl);
    await expectCancelledCheckout(plain);
  } finally { await noJs.close(); }
});

/** A payment of `fraction` of the order's amount, with `confirmations`
 * (undefined: seen in the mempool only). */
async function payment(request, orderId, fraction, confirmations) {
  const depth = confirmations === undefined ? '' : `&confirmations=${confirmations}`;
  const response = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/payment?fraction=${fraction}${depth}`);
  expect(response.status()).toBe(204);
}

test('merchant rings up a sale on a physical keyboard', async ({ page }) => {
  // No open orders: the POS opens on an empty keypad.
  await page.route('**/pos/orders?*', route => route.fulfill({ json: { orders: [], total: 0 } }));
  await page.goto(posUrl());
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await expect(page.locator('.pos-stack-card')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Charge' })).toBeDisabled();
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
  await expect(page.locator('.mk-stage-msg')).toHaveText('Waiting for confirmation. Payment seen. Waiting for its first confirmation.');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  // Seen in full: the code fades so it isn't paid twice.
  await expect(page.locator('.pos-pay-card.is-spent')).toBeVisible();
  await expect(page.locator('.pos-pay-caption')).toHaveText('Order total');
  await captureCoverageStage(page, 'pos-unconfirmed', test.info());
  // Then mined into a block: the stage says it's paid, on the same card.
  expect((await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/confirm`)).status()).toBe(204);
  await expect(page.locator('.mk-stage')).toContainText('received and confirmed.');
  await expect(page.locator('.mk-track .step-done')).toHaveCount(3);
  await captureCoverageStage(page, 'pos-paid', test.info());
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Paid');
  await page.getByRole('button', { name: 'New order' }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();

  // The merchant asks for 3 confirmations on the store's settings page;
  // the next payment shows its progress towards them.
  const saved = await request.post(`${fixture.base_url}/stores/${fixture.connection_id}/settings`,
    { headers: { cookie: `session=${fixture.session}` }, form: { confirmations_required: '3' }, maxRedirects: 0 });
  expect(saved.status()).toBeLessThan(400);
  await page.keyboard.type('30000000000');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  const heading = await page.locator('.pos-order-heading h1 + p').textContent();
  const list = await (await request.get(`${posUrl()}/orders`, { headers: { cookie: `session=${fixture.session}` } })).json();
  const order = list.orders.find(o => !o.backgrounded && o.status === 'pending');
  expect(heading).toContain(order.order_id.slice(-4));
  expect(order.confirmations_required).toBe(3);
  await payment(request, order.order_id, 1, 1);
  await expect(page.locator('.mk-stage-msg')).toHaveText('Confirming. 1 of 3 confirmations.');
  await expect(page.locator('.mk-track .step-now')).toContainText('Confirm');
  await captureCoverageStage(page, 'pos-confirming', test.info());
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Confirming');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
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
  await expect(page.locator('.mk-stage-msg')).toHaveText('Waiting for confirmation. Payment seen. Waiting for its first confirmation.');
  const response = page.waitForResponse(`**/pos/orders/${fixture.order_id}`);
  release();
  await (await response).finished();
  // Wait for the application's fetch/JSON continuation, not just the response headers.
  await page.waitForFunction(() => document.querySelector('.pos-qr svg'));
  await expect(page.locator('.mk-stage-msg')).toHaveText('Waiting for confirmation. Payment seen. Waiting for its first confirmation.');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  // No monotonic-status assumption: a subsequent stream snapshot may regress.
  await page.evaluate(order => window.__status({ ...order, is_terminal: false }), initial);
  await expect(page.locator('.mk-stage-msg')).toHaveText('Send the exact amount below. Scan the code with your wallet, or copy the address.');
  await expect(page.getByRole('button', { name: 'Cancel order' })).toBeVisible();
});

test('customer underpays: the card asks for the rest and the order cannot be cancelled', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  const code = () => page.locator('.pos-qr').innerHTML();
  const first = await code();
  await payment(request, fixture.order_id, 0.4, 20);
  await expect(page.locator('.mk-stage')).toContainText('0.0004 of 0.001 XMR received');
  await expect(page.locator('.pos-pay-card')).toContainText('0.0006');
  // The live update brings a new code, for the rest, and says so.
  await expect(page.locator('.pos-qr-new-tab')).toHaveText('New code · 0.0006 XMR');
  expect(await code()).not.toBe(first);
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
  await expect(page.locator('.pos-action-hint')).toContainText('Background keeps this payment open');
  await captureCoverageStage(page, 'pos-underpaid', test.info());
  // The customer sends the rest.
  await payment(request, fixture.order_id, 0.6, 20);
  await expect(page.locator('.mk-stage')).toContainText('received and confirmed.');
});

test('customer walks away: the order on screen expires', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/expired`);
  await expect(page.locator('.mk-stage')).toContainText('This payment expired before it was completed.');
  await expect(page.locator('.pos-pay-card.is-spent')).toBeVisible();
  await captureCoverageStage(page, 'pos-expired', test.info());
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Expired');
});

test('counter loses its connection: the order shows connection lost, then recovers by itself', async ({ page, context, request }) => {
  let online = true;
  // The Wi-Fi is down: every attempt to (re)open the update stream fails.
  await page.route('**/pos/events?*', route => (online ? route.continue() : route.abort('internetdisconnected')));
  online = false;
  // The POS shows nothing of the stream failing until its 6s are up, so
  // the stream's own error events are recorded (recordEventSources): once
  // the test sees one, the POS has handled it and started its count.
  await recordEventSources(page);
  // The page's time stands still from the start: only the test moves it.
  // The browser retries the stream on its own clock, which is real; the
  // 6s after which the merchant is told is the page's.
  const start = new Date('2026-01-01T00:00:00Z');
  await pauseClockAt(page, start);
  await page.goto(posUrl());
  const badge = page.locator('.pos-order-heading .tag');
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await expect.poll(async () => (await eventSources(page)).some(stream => stream.errors > 0), 'the POS handles the stream failing').toBe(true);
  // Retries fail every few seconds; once 6s have passed without a
  // connection the merchant is told, however many retries that took: not a
  // millisecond before.
  await page.clock.runFor(5999);
  await expect(badge).toContainText('Awaiting payment');
  await page.clock.runFor(1);
  await expect(badge).toContainText('Connection lost');
  await captureCoverageStage(page, 'pos-connection-lost', test.info());
  // The customer pays meanwhile; back online, the stream reconnects and
  // brings the missed payment in without a reload. The browser's retry runs
  // on its own clock, a few real seconds; the POS's on the page's, moved a
  // second at a time until it has.
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/payment?fraction=1`);
  online = true;
  await expect.poll(async () => { await page.clock.runFor(1000); return badge.textContent(); }, { timeout: 15000 }).toContain('Unconfirmed');
  await expect(page.locator('.mk-stage-msg')).toContainText('Payment seen. Waiting for its first confirmation.');
  const streams = await eventSources(page);
  // Chromium retries a stream that failed to connect itself.
  expect(streams, 'one stream, retried by the browser').toHaveLength(1);
  expect(streams[0].refused).toBe(0);
  expect(streams.at(-1).readyState, 'connected').toBe(1);

  // The till is opened again while the server restarts. The page's fake
  // clock is the context's: still standing still.
  const headers = { cookie: `session=${fixture.session}` };
  expect((await request.post(`${posUrl()}/orders/${encodeURIComponent(fixture.order_id)}/background`, { headers })).ok()).toBeTruthy();
  const created = await request.post(`${posUrl()}/orders`, { headers, data: { amount: '0.001', merchant_order_id: 'Next sale', request_key: 'next-sale' } });
  expect(created.ok()).toBeTruthy();
  const next = (await created.json()).order_id;
  const till = await context.newPage();
  // The order list fails to load once...
  let failures = 1;
  await till.route('**/pos/orders?state=active', route => (failures-- > 0
    ? route.fulfill({ status: 503, contentType: 'application/problem+json', body: JSON.stringify({ type: 'https://docs.monokulo.com/problems/engine-unavailable', title: 'Payment engine unavailable', status: 503, detail: 'The payment engine is busy or unreachable. Try again in a moment.' }) })
    : route.continue()));
  // ...and the update stream is refused: every browser closes a refused
  // stream for good.
  let refusing = true;
  await till.route('**/pos/events?*', route => (refusing ? route.fulfill({ status: 503, body: 'restarting' }) : route.continue()));
  await recordEventSources(till);
  await till.goto(posUrl());
  await expect(till.locator('.pos-error')).toContainText('The payment engine is busy');
  // With nothing loaded there is no sidebar, so the keypad offers Retry,
  // and the sale in progress comes back on screen.
  await till.getByRole('button', { name: 'Retry' }).click();
  await expect(till.locator('.pos-error')).toHaveCount(0);
  await expect(till.locator('.pos-order-heading h1')).toHaveText('Next sale');
  const tag = till.locator('.pos-order-heading .tag');
  await expect.poll(async () => (await eventSources(till)).map(stream => stream.refused), 'the browser gives up on the stream').toEqual([1]);
  // Refused again after the POS's first wait (5s), so it waits twice as long.
  await till.clock.runFor(5000);
  await expect.poll(async () => (await eventSources(till)).map(stream => stream.refused)).toEqual([1, 1]);
  // Down for 6s: the merchant is told.
  await till.clock.runFor(1000);
  await expect(tag).toContainText('Connection lost');
  refusing = false;
  await payment(request, next, 1);
  await till.clock.runFor(8999);
  expect(await eventSources(till), 'not yet: it waits 10s this time').toHaveLength(2);
  await till.clock.runFor(1);
  await expect(tag).toContainText('Unconfirmed');
  expect((await eventSources(till)).map(stream => stream.readyState)).toEqual([EVENT_SOURCE_CLOSED, EVENT_SOURCE_CLOSED, 1]);
});

test('payment lands just before the merchant confirms a cancel: the server refuses and says why', async ({ page, request }) => {
  // The live update for the payment has not reached this screen yet.
  await page.route('**/pos/events?*', () => {});
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/payment?fraction=0.5`);
  await page.getByRole('button', { name: 'Cancel order…' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-error')).toHaveText('This order has payment activity and cannot be cancelled. Background it for review instead.');
  // The screen catches up with the payment that caused the refusal.
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Partially paid');
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

test('merchant records the customer refund address from a QR image, with clear failures', async ({ page, context }) => {
  const fixtures = require('node:path').join(__dirname, '../fixtures');
  const refundAddress = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
  // The merchant denies the camera. Writing to the clipboard granted too,
  // for the test to read it back as the merchant's next paste would: a real
  // Chrome lets the page in front write unasked; Playwright's asks.
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.addInitScript(() => {
    Object.defineProperty(navigator.mediaDevices, 'getUserMedia', { value: async () => { throw new DOMException('Permission denied', 'NotAllowedError'); } });
    document.addEventListener('copy', () => { window.__copied = String(document.getSelection()); });
  });
  await page.goto(posUrl());
  const card = page.locator('.pos-pay-card');
  const message = card.locator('.pos-refund-message');
  const input = card.locator('#pos-refund');
  // The stage says what to do; the time left is under the status.
  await expect(card.locator('.mk-stage-msg')).toHaveText('Send the exact amount below. Scan the code with your wallet, or copy the address.');
  await expect(page.locator('[data-pos="time-left"] .mk-countdown-clock')).toHaveText(/^(59:\d\d|60:00)$/);

  // The payment address copies whole.
  const copy = card.getByRole('button', { name: 'Copy payment address' });
  const copied = card.locator('.pos-address [role="status"]');
  const address = await card.locator('.pos-address [data-copy-value]').textContent();
  await copy.click();
  await expect(copied).toHaveText('Copied');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(address);
  // A tablet reaching the POS over plain http on the shop's network: the
  // page isn't a secure context, so it has no navigator.clipboard. The
  // address is copied from a selection...
  await page.evaluate(() => Object.defineProperty(Navigator.prototype, 'clipboard', { configurable: true, get: () => undefined }));
  await copy.click();
  await expect(copied).toHaveText('Copied');
  expect(await page.evaluate(() => window.__copied)).toBe(address);
  // ...and where the browser copies nothing even then: never a claim it
  // copied, the whole address left selected to copy by hand.
  await page.evaluate(() => { document.execCommand = () => false; });
  await copy.click();
  await expect(copied).toHaveText('Select and copy the full value using your device’s copy command.');
  expect(await page.evaluate(() => String(document.getSelection()))).toBe(address);

  // No camera: the card says to choose a photo instead.
  await card.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(message).toHaveText('Camera unavailable. Choose a QR image instead.');
  await expect(card.getByRole('button', { name: 'Scan refund QR' })).toBeVisible();
  async function choose(file) {
    const chooser = page.waitForEvent('filechooser');
    await card.getByRole('button', { name: 'Choose QR image' }).click();
    await (await chooser).setFiles(require('node:path').join(fixtures, file));
  }
  await choose('corrupt-photo.png');
  await expect(message).toHaveText('Could not read that image. Choose another file.');
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
  // Saved: it's there after a reload.
  await page.reload();
  await expect(page.locator('#pos-refund')).toHaveValue(refundAddress);
  await expect(page.locator('.pos-refund-state')).toHaveAttribute('aria-label', 'Refund address saved');
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

test('double spend on the order on screen warns the merchant not to hand over goods', async ({ page, request }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await payment(request, fixture.order_id, 1);
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Unconfirmed');
  const flagged = await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/double-spend`);
  expect(flagged.status()).toBe(204);
  await expect(page.locator('.pos-order-heading .tag')).toContainText('Double spend');
  await expect(page.locator('.mk-stage')).toContainText('Do not treat it as paid');
  await captureCoverageStage(page, 'pos-double-spend', test.info());
  await expect(page.getByRole('button', { name: 'Cancel order' })).toHaveCount(0);
});

test('store priced in AUD: the merchant keys in dollars and cents and sees both amounts throughout', async ({ page, request }) => {
  // The merchant switches the store to Australian dollars on its settings page.
  const saved = await request.post(`${fixture.base_url}/stores/${fixture.connection_id}/settings`,
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
  await expect(page.locator('.mk-stage')).toContainText('0.03125 XMR received and confirmed.');
  await expect(card.locator('.pos-pay-xmr')).toContainText('0.03125');
});

test('payment countdown keeps ticking down while the customer finds their wallet', async ({ page, request }) => {
  // The fixture order expires an hour after it was made, moments ago.
  const order = await (await request.get(`${posUrl()}/orders/${fixture.order_id}`, { headers: { cookie: `session=${fixture.session}` } })).json();
  // The page's time stands still from the start: only the test moves it.
  const start = new Date();
  await pauseClockAt(page, start);
  await page.goto(posUrl());
  // Under the status, with a bar that empties; the stage's message leaves
  // the time to it.
  const expiry = page.locator('.pos-status-row [data-pos="time-left"]');
  const clock = expiry.locator('.mk-countdown-clock');
  const bar = expiry.locator('progress');
  await expect(clock).toHaveText(/^(59:\d\d|60:00)$/);
  await expect(page.locator('.mk-stage-msg')).not.toContainText(' left');
  const full = Number(await bar.getAttribute('value'));
  await page.clock.fastForward(20 * 60 * 1000);
  await expect(clock).toHaveText(/^(39|40):\d\d$/);
  expect(Number(await bar.getAttribute('value'))).toBeLessThan(full);
  // To half a minute before it expires (the order's deadline is in whole
  // seconds, so a second either way).
  await page.clock.fastForward(order.expires_at * 1000 - 30_000 - (await page.evaluate(() => Date.now())));
  await expect(clock).toHaveText(/^0:(29|30|31)$/);
  await expect(expiry).toHaveAttribute('data-tone', 'urgent');
});

async function openFinishedTab(page) {
  await openOrderList(page);
  await page.getByRole('button', { name: /Finished/ }).click();
}

test('an order finished at the counter moves to Finished for this session only', async ({ page, request }) => {
  // The page's time stands still from the start: only the test moves it,
  // so the order finishes at a moment the test knows exactly.
  await pauseClockAt(page, new Date());
  await keepOneInBackground(request);
  await page.goto(posUrl());
  await expect(page.locator('.pos-pay-card')).toBeVisible();
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/paid`);
  await expect(page.locator('.mk-stage')).toContainText('Paid.');
  await page.getByRole('button', { name: 'New order' }).click();
  await openFinishedTab(page);
  await expect(page.getByRole('button', { name: 'Finished · 1' })).toBeVisible();
  await expect(page.locator('.mk-card')).toContainText('Fixture order');
  const note = page.locator('.pos-list-note');
  await expect(note).toContainText('Completed on this device since the POS was opened. They clear after 24 hours or when the page reloads.');
  await expect(note.getByRole('link', { name: 'See all orders →' })).toHaveAttribute('href', `/stores/${fixture.connection_id}/orders`);
  await captureCoverageStage(page, 'pos-finished-tab', test.info());
  await page.getByRole('button', { name: /Active/ }).click();
  await expect(page.locator('.mk-card')).toHaveCount(1);
  await expect(page.locator('.mk-card')).toContainText('Kept open');
  // It drops off the tab 24 hours after it finished.
  await page.getByRole('button', { name: /Finished/ }).click();
  await page.clock.fastForward((23 * 60 + 58) * 60 * 1000);
  await expect(page.locator('.mk-card')).toHaveCount(1);
  await page.clock.fastForward(3 * 60 * 1000);
  await expect(page.locator('.mk-card')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Finished · 0' })).toBeVisible();
  // Nothing is loaded into Finished: after a reload it starts empty.
  await page.reload();
  await openFinishedTab(page);
  await expect(page.getByRole('button', { name: 'Finished · 0' })).toBeVisible();
  await expect(page.locator('.pos-empty')).toHaveText('No finished orders yet.');
  await expect(page.locator('.pos-list-note')).toBeVisible();
});

test('a backgrounded order that is paid while the merchant serves someone else moves to Finished', async ({ page, request }) => {
  await keepOneInBackground(request);
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toHaveCount(2);
  await request.post(`${fixture.base_url}/__coverage/orders/${fixture.order_id}/paid`);
  await expect(page.locator('.pos-stack-card')).toHaveCount(1);
  await openFinishedTab(page);
  await expect(page.locator('.mk-card .tag')).toContainText('Paid');
});
