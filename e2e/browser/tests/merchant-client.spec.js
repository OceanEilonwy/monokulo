// The embed library as a merchant's static site uses it: a page on the
// merchant's own origin loads monokulo-client.js with a <script src>,
// creates an order, mounts the checkout, and reacts to the customer paying.
const { test, expect, pauseClockAt } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

let fixture;
let shopOrigin;
test.beforeAll(async () => {
  fixture = await startCoverageFixture();
  // Same server, different origin: stands in for the merchant's own site.
  shopOrigin = fixture.base_url.replace('127.0.0.1', 'localhost');
});
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

async function openShop(page) {
  // The shop page's own record of the library's streams: each EventSource
  // it opens, and (a listener added in the constructor runs before the
  // library's, in the same dispatch) each refusal the library has handled.
  await page.addInitScript(() => {
    const Native = window.EventSource;
    window.streams = [];
    window.EventSource = class extends Native {
      constructor(...args) {
        super(...args);
        const record = { url: String(args[0]), refused: 0, source: this };
        window.streams.push(record);
        this.addEventListener('error', () => { if (this.readyState === Native.CLOSED) record.refused++; });
      }
    };
  });
  await page.goto(`${shopOrigin}/__coverage/ready`);
  await page.setContent(`<!doctype html><div id="pay"></div><p id="log"></p>
    <script src="${fixture.base_url}/static/monokulo-client.js"></script>`);
  await page.waitForFunction(() => window.Monokulo);
  await page.evaluate(() => {
    window.events = [];
    window.track = name => (status, data) => window.events.push([name, typeof status === 'string' ? status : status.status]);
  });
}

/** The shop page's streams: each one's `readyState` and the refusals it handled. */
const streams = page => page.evaluate(() => window.streams.map(({ source, refused }) => ({ readyState: source.readyState, refused })));
/** `EventSource.CLOSED`: the stream is over for good, by the server's refusal or the library's close(). */
const CLOSED = 2;

test('merchant site creates an order, mounts the checkout, and is told once when it is paid', async ({ page, request }) => {
  await openShop(page);
  // No endpoint given: the library infers monokulo's origin from its own script tag.
  const order = await page.evaluate(async publicKey => {
    const created = await window.Monokulo.createOrder({ publicKey, amount: '0.5', currency: 'XMR', merchantOrderId: 'cart-1042' });
    window.Monokulo.mount('#pay', created, { onStatusChange: window.track('change'), onPaid: window.track('paid'), onExpired: window.track('expired') });
    return created;
  }, fixture.public_key);
  expect(order.endpoint).toBe(fixture.base_url);
  expect(order.merchantOrderId).toBe('cart-1042');
  expect(order.currency).toBe('XMR');
  await expect(page.frameLocator('#pay iframe').locator('#checkout-root')).toHaveAttribute('data-status', 'pending');
  await expect.poll(() => page.evaluate(() => window.events)).toEqual([['change', 'pending']]);

  const paid = await request.post(`${fixture.base_url}/__coverage/orders/${order.orderId}/paid`);
  expect(paid.status()).toBe(204);
  await expect.poll(() => page.evaluate(() => window.events)).toEqual([['change', 'pending'], ['change', 'paid'], ['paid', 'paid']]);
  await expect(page.frameLocator('#pay iframe').locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  // Nothing fires twice for the same state: a final order's stream is
  // closed for good (the browser would otherwise reopen it and get the
  // state again), so the merchant hears no more.
  await expect.poll(() => streams(page), 'the library closed its one stream').toEqual([{ readyState: CLOSED, refused: 0 }]);
  expect(await page.evaluate(() => window.events)).toHaveLength(3);
});

test('merchant site is told when an unpaid order expires', async ({ page, request }) => {
  await openShop(page);
  const order = await page.evaluate(async publicKey => {
    const created = await window.Monokulo.createOrder({ publicKey, amount: 1, currency: 'XMR' });
    window.Monokulo.mount('#pay', created, { onPaid: window.track('paid'), onExpired: window.track('expired') });
    return created;
  }, fixture.public_key);
  await expect(page.frameLocator('#pay iframe').locator('#checkout-root')).toBeVisible();
  const expired = await request.post(`${fixture.base_url}/__coverage/orders/${order.orderId}/expired`);
  expect(expired.status()).toBe(204);
  await expect.poll(() => page.evaluate(() => window.events)).toEqual([['expired', 'expired']]);
});

test('merchant site gets the server\'s reason when an order cannot be created', async ({ page }) => {
  await openShop(page);
  const errors = await page.evaluate(async publicKey => {
    const reason = promise => promise.then(() => 'created', error => error.message);
    return {
      // A fiat currency this store has no exchange rate provider for.
      unsupported: await reason(window.Monokulo.createOrder({ publicKey, amount: '10', currency: 'EUR' })),
      noKey: await reason(window.Monokulo.createOrder({ amount: '10', currency: 'XMR' })),
      noAmount: await reason(window.Monokulo.createOrder({ publicKey, currency: 'XMR' })),
    };
  }, fixture.public_key);
  expect(errors.unsupported).not.toBe('created');
  expect(errors.unsupported).not.toMatch(/^HTTP \d+$/);
  expect(errors.noKey).toContain('publicKey is required');
  expect(errors.noAmount).toContain('amount and currency are required');
});

test('merchant single-page app re-rendering the payment widget keeps one live checkout', async ({ page, request }) => {
  await openShop(page);
  const { first, second } = await page.evaluate(async publicKey => {
    const first = await window.Monokulo.createOrder({ publicKey, amount: 1, currency: 'XMR' });
    const second = await window.Monokulo.createOrder({ publicKey, amount: 2, currency: 'XMR' });
    // The customer changes their cart: the app mounts the new order into
    // the same container without cleaning up the old one.
    window.Monokulo.mount('#pay', first, { onPaid: window.track('first') });
    window.handle = window.Monokulo.mount('#pay', second, { onPaid: window.track('second') });
    return { first, second };
  }, fixture.public_key);
  await expect(page.locator('#pay iframe')).toHaveCount(1);
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', new RegExp(`${second.orderId}$`));
  // The replaced order's callbacks no longer fire.
  await request.post(`${fixture.base_url}/__coverage/orders/${first.orderId}/paid`);
  await request.post(`${fixture.base_url}/__coverage/orders/${second.orderId}/paid`);
  await expect.poll(() => page.evaluate(() => window.events)).toEqual([['second', 'paid']]);
  // Leaving the payment step: destroy() removes the checkout.
  await page.evaluate(() => window.handle.destroy());
  await expect(page.locator('#pay iframe')).toHaveCount(0);
});

test('merchant site keeps following an order through a status outage when its stream is refused', async ({ page, request }) => {
  // The page's time stands still from the start: only the test moves it.
  const start = new Date('2026-01-01T00:00:00Z');
  await pauseClockAt(page, start);
  // The shop page's own record of the library's following: each delay it
  // schedules (its next poll, set once it has handled a refusal or a
  // poll's answer), so the test moves the clock only once the library is
  // waiting on it, and each poll as it's sent (synchronously, inside the
  // timer that polls). Added after the clock's own script, so it wraps the
  // fake setTimeout.
  await page.addInitScript(() => {
    window.scheduled = [];
    window.polled = 0;
    const setTimeout_ = window.setTimeout;
    window.setTimeout = function (callback, delay, ...args) {
      window.scheduled.push(delay);
      return setTimeout_.call(this, callback, delay, ...args);
    };
    const fetch_ = window.fetch;
    window.fetch = function (resource, ...rest) {
      if (String(resource instanceof Request ? resource.url : resource).endsWith('/status')) window.polled++;
      return fetch_.call(this, resource, ...rest);
    };
  });
  const scheduled = () => page.evaluate(() => window.scheduled);
  const polled = () => page.evaluate(() => window.polled);
  await openShop(page);
  let polls = 0;
  await page.route(`${fixture.base_url}/pay/*/orders/*/events`, route => route.fulfill({ status: 503, body: '' }));
  await page.route(`${fixture.base_url}/pay/*/orders/*/status`, route => {
    polls++;
    // The first poll lands during a server hiccup.
    return polls === 1 ? route.fulfill({ status: 502, json: { error: 'bad gateway' } }) : route.continue();
  });
  const order = await page.evaluate(async publicKey => {
    const created = await window.Monokulo.createOrder({ publicKey, amount: 1, currency: 'XMR' });
    window.Monokulo.mount('#pay', created, { onStatusChange: window.track('change'), onPaid: window.track('paid') });
    return created;
  }, fixture.public_key);
  // The poll that takes over from the refused stream comes 3s after the
  // refusal, not a millisecond before.
  await expect.poll(() => streams(page), 'the library handles the refused stream').toEqual([{ readyState: CLOSED, refused: 1 }]);
  await expect.poll(scheduled).toEqual([3000]);
  await page.clock.runFor(2999);
  expect(await polled(), 'no poll before 3s').toBe(0);
  await page.clock.runFor(1);
  expect(await polled(), 'the poll at 3s').toBe(1);
  // A failed poll says nothing to the merchant and tries again after 5s.
  await expect.poll(scheduled, 'the library handles the failed poll').toEqual([3000, 5000]);
  expect(polls).toBe(1);
  expect(await page.evaluate(() => window.events)).toEqual([]);
  await page.clock.runFor(4999);
  expect(await polled(), 'no retry before 5s').toBe(1);
  await page.clock.runFor(1);
  expect(await polled(), 'the retry at 5s').toBe(2);
  await expect.poll(() => page.evaluate(() => window.events)).toEqual([['change', 'pending']]);
  // Following the order: every 3s while it's open.
  await expect.poll(scheduled, 'the library handles the answer').toEqual([3000, 5000, 3000]);
  await request.post(`${fixture.base_url}/__coverage/orders/${order.orderId}/paid`);
  await page.clock.runFor(2999);
  expect(await polled(), 'no poll before 3s').toBe(2);
  await page.clock.runFor(1);
  expect(await polled(), 'the poll at 3s').toBe(3);
  await expect.poll(() => page.evaluate(() => window.events)).toEqual([['change', 'pending'], ['change', 'paid'], ['paid', 'paid']]);
  expect(polls).toBe(3);
  // Paid: the library stops following the order.
  expect(await scheduled(), 'no poll after the order is final').toEqual([3000, 5000, 3000]);
});

test('a visitor past the rate limit, or asked for proof on a plain-HTTP shop, gets a clear failure', async ({ page }) => {
  let answer;
  await page.route(`${fixture.base_url}/pay/*/orders`, route => route.fulfill(answer()));
  await openShop(page);
  const attempt = () => page.evaluate(publicKey => window.Monokulo.createOrder({ publicKey, amount: 1, currency: 'XMR' })
    .then(() => 'created', error => error.message), fixture.public_key);
  const cors = { 'access-control-allow-origin': '*' };

  // Past the hard limit monokulo answers 429 without a challenge: nothing to
  // solve, so the call fails with the server's reason for the merchant to retry.
  answer = () => ({ status: 429, headers: { ...cors, 'retry-after': '60' }, json: { error: 'Too many requests. Try again later.' } });
  expect(await attempt()).toBe('Too many requests. Try again later.');

  // A shop served over plain HTTP is not a secure context: no Web Crypto,
  // so a proof-of-work challenge cannot be solved in this browser.
  await page.evaluate(() => { Object.defineProperty(window.crypto, 'subtle', { value: undefined }); });
  answer = () => ({ status: 429, headers: cors, json: { error: 'Too many requests', challenge: { challenge: 'c0ffee', difficulty: 8 } } });
  expect(await attempt()).toContain("can't solve monokulo's challenge");
});

test('a merchant wiring up the library wrongly gets an error saying what to fix', async ({ page }) => {
  await openShop(page);
  const problems = await page.evaluate(({ publicKey, endpoint }) => {
    const attempt = fn => { try { fn(); return 'mounted'; } catch (error) { return error.message; } };
    return {
      // The container id was mistyped.
      missing: attempt(() => window.Monokulo.mount('#paymnet', { orderId: 'order_x', endpoint, publicKey })),
      // An order id kept from a previous visit, without its store's key.
      noKey: attempt(() => window.Monokulo.mount('#pay', 'order_from_last_visit')),
      // With the key it mounts: the endpoint comes from the script tag.
      withKey: attempt(() => window.Monokulo.mount('#pay', 'order_from_last_visit', { publicKey })),
    };
  }, { publicKey: fixture.public_key, endpoint: fixture.base_url });
  expect(problems.missing).toBe('Monokulo.mount: target element not found: #paymnet');
  expect(problems.noKey).toContain('could not resolve orderId/endpoint/publicKey');
  expect(problems.noKey).toContain('{endpoint, publicKey} in options for an orderId from a previous visit');
  expect(problems.withKey).toBe('mounted');
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', `${fixture.base_url}/pay/${fixture.public_key}/orders/order_from_last_visit`);
});
