const path = require('node:path');
const { test, expect, pauseClockAt, recordEventSources, eventSources } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const address = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
// The Monero General Fund's published address: another valid mainnet one.
const otherAddress = '44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A';
let fixture;

test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});
// The tests share one store; one that restricts it to its verified domains
// must not leave the next test's orders refused.
test.afterEach(async ({ request }) => {
  await request.post(`${fixture.base_url}/__coverage/embed/unrestricted`);
});

async function checkoutUrl(request) {
  const response = await request.post(`${fixture.base_url}/__coverage/orders`);
  expect(response.ok()).toBeTruthy();
  const { order_id } = await response.json();
  return `${fixture.base_url}/checkout/${order_id}`;
}

// An order made as the store's website makes one: the API, with its public key.
async function websiteOrder(request, data) {
  const response = await request.post(`${fixture.base_url}/api/v1/orders`, {
    headers: { authorization: `Bearer ${fixture.public_key}` }, data,
  });
  expect(response.status()).toBe(201);
  return response.json();
}

test('real checkout saves a QR refund address and restores a saved address after editing', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  // The page's own record of what Enter does, below: the form's submit
  // event, as the page's handler left it (a listener on the window runs
  // last), and every save request, with whether it was a navigation (the
  // form posting) or the script's own fetch.
  await page.addInitScript(() => {
    window.__submits = [];
    window.addEventListener('submit', event => window.__submits.push({ form: event.target.id, prevented: event.defaultPrevented }));
  });
  const saves = [];
  await page.route(`${url}/refund-address*`, route => {
    saves.push({ navigation: route.request().isNavigationRequest() });
    return route.continue();
  });
  await page.goto(url);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  const scanError = page.locator('#scan-error');
  await input.focus();
  await input.blur();
  await expect(field).not.toHaveClass(/is-saved|is-saving|is-invalid/);
  await expect(page.locator('#upload-refund')).toBeVisible();
  async function choose(file) {
    const chooser = page.waitForEvent('filechooser');
    await page.getByRole('button', { name: 'Choose QR image' }).click();
    await (await chooser).setFiles(path.join(__dirname, '../fixtures', file));
  }
  // Images it can't use, each said so: a photo with no QR code, a QR code
  // that isn't a Monero one (a shop's menu link), and a file that isn't a
  // picture at all.
  await choose('receipt-photo-no-qr.png');
  await expect(scanError).toHaveText('No QR code found in that image.');
  await choose('menu-link-qr.png');
  await expect(scanError).toHaveText('This QR code does not contain a Monero address.');
  await choose('corrupt-photo.png');
  await expect(scanError).toHaveText('Could not read that image. Choose another file.');
  await expect(input).toHaveValue('');
  // A wallet's "receive" QR is a monero: URI with an amount and a label;
  // only the address is the refund address.
  await choose('refund-qr-wallet-uri.png');
  await expect(input).toHaveValue(address);
  await expect(scanError).toBeHidden();
  await expect(field).toHaveClass(/is-saved/);
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Refund address saved');
  await captureCoverageStage(page, 'checkout-refund-saved', test.info());
  await page.reload();
  await expect(input).toHaveValue(address);
  await expect(field).toHaveClass(/is-saved/);
  await input.click();
  await expect(field).not.toHaveClass(/is-saved/);
  await input.fill('');
  await input.blur();
  await expect(input).toHaveValue(address);
  await expect(field).toHaveClass(/is-saved/);

  // Enter saves in place: without script the form posts and the server
  // redirects back; with it the page (and a live camera or stream) stays.
  await page.evaluate(() => { window.__samePage = true; window.__submits = []; });
  saves.length = 0;
  await input.fill(otherAddress);
  // Enter's implicit submission dispatches the submit event before the key
  // press is done: by now the page has either prevented it or let it post.
  await input.press('Enter');
  expect(await page.evaluate(() => window.__submits), 'the page took over the submit').toEqual([{ form: 'refund-form', prevented: true }]);
  await expect(field).toHaveClass(/is-saved/);
  // Typing alone saves too, after a pause, so there may be one save or
  // two; none of them is the form posting.
  expect(saves.length, 'saved').toBeGreaterThan(0);
  expect(saves.filter(save => save.navigation), 'no save by the form posting').toEqual([]);
  expect(await page.evaluate(() => window.__samePage)).toBe(true);
  expect(page.url()).toBe(url);
});

test('real checkout validates, shows saving, rejects a response, and retries', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  // The customer's camera is refused (or there is none).
  await page.addInitScript(() => {
    Object.defineProperty(navigator.mediaDevices, 'getUserMedia', { value: async () => { throw new Error('denied'); } });
    Object.defineProperty(navigator.mediaDevices, 'enumerateDevices', { value: async () => [] });
  });
  let answer = 400;
  let release;
  await page.route(`${url}/refund-address*`, async route => {
    if (answer === 'offline') return route.abort('failed');
    if (answer === 400) await new Promise(resolve => { release = resolve; });
    await route.fulfill({ status: answer, contentType: 'application/json',
      body: JSON.stringify(answer === 200 ? { ok: true } : { type: 'https://docs.monokulo.com/problems/invalid-refund-address', title: 'Not a refund address for this store', status: 400, detail: 'invalid' }) });
  });
  await page.goto(url);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  await input.fill('short');
  await expect(field).not.toHaveClass(/is-saving|is-saved/);
  await input.blur();
  await expect(page.locator('#scan-error')).toContainText('95 or 106');
  await input.fill(address);
  await expect(field).toHaveClass(/is-saving/);
  release();
  await expect(field).toHaveClass(/is-invalid/);
  await expect(page.locator('#scan-error')).toContainText('Invalid refund address');
  await captureCoverageStage(page, 'checkout-invalid-address', test.info());
  // The connection drops as it saves: said so, and not marked saved.
  answer = 'offline';
  await input.fill(otherAddress);
  await expect(page.locator('#scan-error')).toContainText('Failed to fetch');
  await expect(field).not.toHaveClass(/is-saved/);
  // Without a camera, choosing a photo is still offered.
  await page.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(page.locator('#scan-error')).toContainText('Camera unavailable');
  await expect(page.locator('#upload-refund')).toBeVisible();
  await captureCoverageStage(page, 'checkout-camera-fallback', test.info());
  // Back online, a retry saves.
  answer = 200;
  await input.fill(address.slice(0, -1) + 'D');
  await expect(field).not.toHaveClass(/is-invalid/);
  await expect(field).toHaveClass(/is-saved/);
});

test('real checkout live update preserves a focused partial refund address', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  let release;
  const requested = new Promise(resolve => { release = resolve; });
  let sendUpdate;
  await page.route(`${url}/events?*`, async route => {
    release();
    await new Promise(resolve => { sendUpdate = resolve; });
    const fragment = '<div id="live-status" data-live><span id="status-badge">Partial payment received</span></div>';
    const status = JSON.stringify({ status: 'partial', confirmations: 0, confirmations_required: 1, is_terminal: false, error: 'Underpaid' });
    // The page's own stream: each changed part routed to its element (ssexi).
    const route_ = JSON.stringify({ target: '#live-status', swap: 'outerHTML' });
    await route.fulfill({ contentType: 'text/event-stream', body: `event: ${route_}\ndata: ${fragment}\n\nevent: status\ndata: ${status}\n\n` });
  });
  await page.goto(url);
  await requested;
  await page.locator('#refund_address').fill('4AdUndXHHZ');
  sendUpdate();
  await expect(page.locator('#status-badge')).toHaveText('Partial payment received');
  await expect(page.locator('#refund_address')).toHaveValue('4AdUndXHHZ');
  await expect(page.locator('#refund_address')).toBeFocused();
});

test('real checkout saves a refund address without JavaScript', async ({ browser, request }) => {
  const url = await checkoutUrl(request);
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(url);
    // The scanner needs JavaScript; typing the address and Save don't.
    await expect(page.locator('#scan-refund')).toBeHidden();
    await expect(page.locator('#upload-refund')).toBeHidden();
    await expect(page.locator('#checkout-refresh')).toHaveCount(1);
    await expect(page.getByRole('link', { name: 'Auto Refresh: ON' })).toBeVisible();
    await captureCoverageStage(page, 'checkout-no-js', test.info());
    await page.locator('#refund_address').fill(address);
    await page.getByRole('button', { name: 'Save' }).click();
    // The form posts and the server sends the customer back to the checkout.
    await expect(page).toHaveURL(url);
    await expect(page.locator('#refund_address')).toHaveValue(address);
    // Stored: a fresh load shows it saved.
    await page.goto(url);
    await expect(page.locator('#refund_address')).toHaveValue(address);
    await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  } finally { await context.close(); }
});

test('client refund option frames the real checkout and retains its status stream', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  let statusRequests = 0;
  page.on('request', received => {
    if (received.url().startsWith(`${url}/events?`) || received.url().startsWith(`${fixture.base_url}/api/v1/orders/`)) statusRequests++;
  });
  await page.goto(`${fixture.base_url}/__coverage/ready`);
  await page.setContent('<div id="mount"></div>');
  await page.addScriptTag({ url: `${fixture.base_url}/static/monokulo-client.js` });
  await page.evaluate(({ base_url, public_key, order_id }) => {
    window.Monokulo.mount('#mount', { orderId: order_id, monokulo: base_url, publicKey: public_key }, { refund: false });
  }, { base_url: fixture.base_url, public_key: fixture.public_key, order_id: url.split('/').pop() });
  await expect(page.locator('#mount iframe')).toHaveAttribute('src', `${url}?refund=false`);
  await expect(page.frameLocator('#mount iframe').locator('#checkout-root')).toBeVisible();
  await expect(page.frameLocator('#mount iframe').locator('#refund-form')).toHaveCount(0);
  await expect.poll(() => statusRequests).toBeGreaterThan(0);
  // Monokulo's own share page grows its frame to the whole checkout, so
  // nothing scrolls inside it.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`${url}/share`);
  await expect(page.frameLocator('#checkout-frame').locator('#checkout-root')).toBeVisible();
  await expect.poll(() => page.evaluate(() => {
    const doc = document.getElementById('checkout-frame').contentDocument;
    return doc.documentElement.scrollHeight - doc.documentElement.clientHeight;
  }), 'the checkout fits its frame').toBeLessThanOrEqual(0);
});

test('real restricted checkout permits its own origin and blocks another origin', async ({ page, request }) => {
  const restricted = await request.post(`${fixture.base_url}/__coverage/embed/restricted`);
  expect(restricted.status()).toBe(204);
  const url = await checkoutUrl(request);
  const response = await request.get(url);
  expect(response.headers()['content-security-policy']).toContain("frame-ancestors 'self'");
  await page.goto(`${fixture.base_url}/__coverage/ready`);
  await page.setContent(`<iframe id="allowed" title="Allowed" src="${url}"></iframe>`);
  await expect(page.frameLocator('#allowed').locator('#checkout-root')).toBeVisible();
  const otherOrigin = fixture.base_url.replace('127.0.0.1', 'localhost');
  await page.goto(`${otherOrigin}/__coverage/ready`);
  await page.setContent(`<iframe id="blocked" title="Blocked" src="${url}"></iframe>`);
  await expect(page.frameLocator('#blocked').locator('#checkout-root')).toHaveCount(0);
});

test('real frame-only checkout refuses a top-level navigation', async ({ page, request }) => {
  // An order the store's website made with its public key, before the store
  // took orders only from its verified websites.
  const { order_id } = await websiteOrder(request, { amount: '0.001', currency: 'XMR' });
  const url = `${fixture.base_url}/checkout/${order_id}`;
  const restricted = await request.post(`${fixture.base_url}/__coverage/embed/restricted`);
  expect(restricted.status()).toBe(204);
  const top = await page.goto(url);
  expect(top.status()).toBe(403);
  await expect(page.getByText('This store only shows its checkout inside its own website')).toBeVisible();
  await page.goto(`${fixture.base_url}/__coverage/ready`);
  await page.setContent(`<iframe id="payment" title="Payment" src="${url}"></iframe>`);
  await expect(page.frameLocator('#payment').locator('#checkout-root')).toBeVisible();
});

test('real checkout keeps retrying a refused live stream with backoff and then follows the order', async ({ page, context, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  // The server refuses the stream while restarting, or past its per-client
  // limit of open streams (429); the page must not give up on live updates.
  let served = 0;
  await page.route(`${url}/events?*`, route => {
    served++;
    if (served === 1) return route.fulfill({ status: 503, body: '' });
    if (served === 2) return route.fulfill({ status: 429, body: '' });
    return route.continue();
  });
  // The page's own record of its live stream, from the events fixi sends:
  // fx:config when it starts a request (synchronously, inside the timer that
  // retries), fx:after when it has the response, which is when checkout.js
  // schedules the next retry. Times are the page's fake clock.
  await page.addInitScript(() => {
    const stream = { started: 0, refusedAt: [] };
    window.checkoutStream = stream;
    const isStream = event => event.target instanceof Element && event.target.id === 'checkout-stream';
    document.addEventListener('fx:config', event => { if (isStream(event)) stream.started++; }, true);
    document.addEventListener('fx:after', event => {
      if (isStream(event) && !event.detail.cfg.response.ok) stream.refusedAt.push(Date.now());
    }, true);
  });
  const stream = () => page.evaluate(() => window.checkoutStream);
  const pageNow = () => page.evaluate(() => Date.now());
  // The page's time stands still from the start: only the test moves it.
  const start = new Date('2026-01-01T00:00:00Z');
  await pauseClockAt(page, start);
  await page.goto(url);

  // Refusal n is retried exactly `delay` ms after the page handled it: not a
  // millisecond before, and with one request then.
  const retriedAfter = async (n, delay) => {
    await expect.poll(async () => (await stream()).refusedAt.length, `the page handles refusal ${n}`).toBe(n);
    const { started, refusedAt } = await stream();
    expect(started, `one request per refusal so far`).toBe(n);
    await page.clock.runFor(refusedAt[n - 1] + delay - 1 - (await pageNow()));
    expect((await stream()).started, `no retry before ${delay} ms`).toBe(n);
    await page.clock.runFor(1);
    expect((await stream()).started, `the retry at ${delay} ms`).toBe(n + 1);
  };
  await retriedAfter(1, 5000);
  // The wait doubles after each refusal.
  await retriedAfter(2, 10000);

  // The third request is let through: the stream opens and follows the order.
  await expect.poll(() => served).toBe(3);
  const paid = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/paid`);
  expect(paid.status()).toBe(204);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  expect((await stream()).refusedAt, 'no refusal after the stream opened').toHaveLength(2);

  // A merchant site's widget (monokulo-client.js) whose stream is refused
  // for good reads the order with the store's public key instead. The page's
  // fake clock is the context's: still standing still.
  const site = await context.newPage();
  const siteOrder = (await checkoutUrl(request)).split('/').pop();
  let polled = 0;
  let pollKey;
  await site.route(`${fixture.base_url}/checkout/${siteOrder}/events*`, route => {
    if (route.request().frame() !== site.mainFrame()) return route.continue();
    return route.fulfill({ status: 204, body: '' });
  });
  await site.route(`${fixture.base_url}/api/v1/orders/${siteOrder}`, route => {
    pollKey = route.request().headers().authorization;
    polled++;
    return route.fulfill({ json: { status: 'paid', confirmations: 1 } });
  });
  // The library shows nothing of the refusal until it polls, so its
  // stream's error event, with the stream closed for good, is recorded
  // (recordEventSources): once the test sees it, the library has handled
  // it and set its 3s wait.
  await recordEventSources(site);
  await site.goto(`${fixture.base_url}/__coverage/ready`);
  await site.setContent('<div id="mount"></div>');
  await site.addScriptTag({ url: `${fixture.base_url}/static/monokulo-client.js` });
  await site.evaluate(({ base_url, public_key, order_id }) => {
    window.Monokulo.mount('#mount', order_id, { monokulo: base_url, publicKey: public_key,
      onStatusChange: status => { document.querySelector('#mount').dataset.status = status; } });
  }, { base_url: fixture.base_url, public_key: fixture.public_key, order_id: siteOrder });
  await expect.poll(async () => (await eventSources(site)).map(source => source.refused), 'the library handles the refused stream').toEqual([1]);
  // The first poll comes 3s after the refusal: not a millisecond before. A
  // request of the page's own anchors the count: a poll sent while the
  // clock moved would have reached the route before it.
  await site.clock.runFor(2999);
  await site.evaluate(() => fetch('/__coverage/ready').then(response => response.status));
  expect(polled, 'no poll before 3s').toBe(0);
  await site.clock.runFor(1);
  await expect.poll(() => polled, 'the poll at 3s').toBe(1);
  expect(pollKey, 'the poll reads the order with the public key').toBe(`Bearer ${fixture.public_key}`);
  await expect(site.locator('#mount')).toHaveAttribute('data-status', 'paid');
});

test('real checkout saves the address the customer ends with when they change it mid-save', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const [first, second] = [address, otherAddress];
  const saved = [];
  let releaseFirst;
  await page.route(`${url}/refund-address*`, async route => {
    const value = new URLSearchParams(route.request().postData()).get('refund_address');
    // A slow connection: the first save is still in flight when the
    // customer pastes the address they meant.
    if (!saved.length) await new Promise(resolve => { releaseFirst = resolve; });
    saved.push(value);
    await route.continue();
  });
  await page.goto(url);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  await input.fill(first);
  await expect(field).toHaveClass(/is-saving/);
  await input.fill(second);
  releaseFirst();
  await expect.poll(() => saved).toEqual([first, second]);
  await expect(field).toHaveClass(/is-saved/);
  await expect(input).toHaveValue(second);
  // What the merchant sees after a reload is the corrected address.
  await page.reload();
  await expect(input).toHaveValue(second);
});

async function customerSends(request, orderId, fraction, confirmations) {
  const depth = confirmations === undefined ? '' : `&confirmations=${confirmations}`;
  const sent = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/payment?fraction=${fraction}${depth}`);
  expect(sent.status()).toBe(204);
}

test('real checkout follows the customer payment from the mempool to confirmed and lists it', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  // Every request for the live stream (fixi's first, and any reconnect
  // ssexi makes), and the page's own record of the server ending it: the
  // fx:sse:close event, which is when ssexi decides whether to reconnect.
  let streams = 0;
  page.on('request', sent => { if (sent.url().startsWith(`${url}/events?`)) streams++; });
  await page.addInitScript(() => {
    window.__streamsEnded = 0;
    document.addEventListener('fx:sse:close', event => {
      if (event.target instanceof Element && event.target.id === 'checkout-stream') window.__streamsEnded++;
    }, true);
  });
  const streamsEnded = () => page.evaluate(() => window.__streamsEnded);
  // The page's time stands still from the start: only the test moves it.
  await pauseClockAt(page, new Date('2026-01-01T00:00:00Z'));
  await page.goto(url);
  await expect(page.locator('#status-badge')).toHaveText('Waiting for payment');
  // What the customer is asked to send. The fixture order is 0.001 XMR:
  // shown without its trailing zeros, and with no "≈" line since it is
  // priced in XMR.
  await expect(page.locator('#xmr-amount')).toHaveText('0.001 XMR');
  await expect(page.locator('.fiat-amount')).toHaveCount(0);
  await expect.poll(() => streams).toBe(1);
  await customerSends(request, orderId, 1);
  await expect(page.locator('#status-badge')).toHaveText('Payment seen, unconfirmed');
  // Seen in full: the track is at Confirm, and the code fades so it isn't paid twice.
  await expect(page.locator('#live-status .step-now')).toContainText('Confirm');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  // The payment is listed by a shortened txid, not yet confirmed.
  await expect(page.locator('[data-table="payments"]')).toContainText(`test-p…`);
  await expect(page.locator('.progress-fill')).toHaveAttribute('style', /width: 0%/);
  await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/confirm`);
  // The live stream carries the new state in; no reload.
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  await expect(page.locator('#live-status .mk-stage')).toContainText('Paid.');
  await expect(page.locator('.progress-fill')).toHaveAttribute('style', /width: 100%/);
  await captureCoverageStage(page, 'checkout-paid-live', test.info());
  // A final order closes its stream for good rather than reconnecting:
  // once the page has seen the stream end, no amount of time opens another
  // (ssexi reconnects a dropped stream after 3s; checkout.js retries a
  // refused one after up to a minute). A request of the page's own anchors
  // the count: a reconnect made while the clock moved would have been
  // sent before it.
  await expect.poll(streamsEnded, 'the page sees its stream end').toBe(1);
  await page.clock.runFor(2 * 60 * 1000);
  await page.evaluate(() => fetch('/__coverage/ready').then(response => response.status));
  expect(streams, 'no second stream').toBe(1);
  // Opened again once paid, it says so over a faded code and opens no
  // stream at all.
  await page.reload();
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  await expect(page.locator('#live-status .mk-stage')).toContainText('Paid.');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  await captureCoverageStage(page, 'checkout-paid', test.info());
  await page.evaluate(() => fetch('/__coverage/ready').then(response => response.status));
  expect(streams, 'no stream for a paid order').toBe(1);

  // An order priced in AUD: 12.50 AUD at the fixture's 400 AUD per XMR,
  // with what it is worth beside it.
  const aud = await websiteOrder(request, { amount: '12.50', currency: 'AUD' });
  await page.goto(`${fixture.base_url}/checkout/${aud.order_id}`);
  await expect(page.locator('#xmr-amount')).toHaveText('0.03125 XMR');
  await expect(page.locator('.fiat-amount')).toHaveText('≈ 12.50 AUD');
});

test('real checkout guides a customer who underpays and then sends too much', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  await page.goto(url);
  const code = () => page.locator('.qr-wrap svg').innerHTML();
  const first = await code();
  await customerSends(request, orderId, 0.25, 20);
  await expect(page.locator('#status-badge')).toHaveText('Partial payment received');
  // The stage: what arrived, then the rest, with a new code that asks for it.
  await expect(page.locator('#live-status .mk-stage.state-partial')).toContainText('Send the remaining 0.00075 XMR.');
  await expect(page.locator('#live-status .mk-stage')).toContainText('0.00025 of 0.001 XMR received.');
  await expect(page.locator('#xmr-amount')).toHaveText('0.00075 XMR');
  await expect(page.locator('.qr-wrap.qr-new svg')).toBeVisible();
  expect(await code()).not.toBe(first);
  await captureCoverageStage(page, 'checkout-underpaid', test.info());
  // They send the full amount again instead of the rest.
  await customerSends(request, orderId, 1, 20);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'overpaid');
  await expect(page.locator('#status-badge')).toHaveText('Overpaid');
  await expect(page.locator('#live-status .mk-stage.state-overpaid')).toContainText('(0.00025 XMR extra). Do not send more. Contact the merchant about the extra amount.');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  await expect(page.locator('[data-table="payments"] tbody tr')).toHaveCount(2);
  await captureCoverageStage(page, 'checkout-overpaid', test.info());

  // Another customer's payment is double spent while they watch.
  const spent = (await checkoutUrl(request)).split('/').pop();
  await page.goto(`${fixture.base_url}/checkout/${spent}`);
  await customerSends(request, spent, 1);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'unconfirmed');
  await request.post(`${fixture.base_url}/__coverage/orders/${spent}/double-spend`);
  await expect(page.locator('#live-status .mk-stage.state-double-spend')).toBeVisible();
  await captureCoverageStage(page, 'checkout-double-spend', test.info());

  // And another walks away: the order expires on screen.
  const late = (await checkoutUrl(request)).split('/').pop();
  await page.goto(`${fixture.base_url}/checkout/${late}`);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'pending');
  await request.post(`${fixture.base_url}/__coverage/orders/${late}/expired`);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'expired');
  await expect(page.locator('#live-status .mk-stage.state-expired')).toBeVisible();
  await captureCoverageStage(page, 'checkout-expired', test.info());
});
