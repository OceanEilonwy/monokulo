const path = require('node:path');
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

const address = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
const qrImage = path.join(__dirname, '../fixtures/refund-qr.png');
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
  return `${fixture.base_url}/pay/${fixture.public_key}/orders/${order_id}`;
}

test('real checkout saves a QR refund address and restores a saved address after editing', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  await input.focus();
  await input.blur();
  await expect(field).not.toHaveClass(/is-saved|is-saving|is-invalid/);
  await expect(page.locator('#upload-refund')).toBeVisible();
  const chooser = page.waitForEvent('filechooser');
  await page.getByRole('button', { name: 'Choose QR image' }).click();
  await (await chooser).setFiles(qrImage);
  await expect(input).toHaveValue(address);
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
});

test('real checkout validates, shows saving, rejects a response, and retries', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  let responseStatus = 400;
  let release;
  await page.route(`${url}/refund-address*`, async route => {
    if (responseStatus === 400) await new Promise(resolve => { release = resolve; });
    await route.fulfill({ status: responseStatus, contentType: 'application/json',
      body: JSON.stringify(responseStatus === 200 ? { ok: true } : { ok: false, error: 'invalid' }) });
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
  responseStatus = 200;
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

test('real checkout keeps retry and camera upload paths after failures', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.addInitScript(() => {
    Object.defineProperty(navigator.mediaDevices, 'getUserMedia', { value: async () => { throw new Error('denied'); } });
    Object.defineProperty(navigator.mediaDevices, 'enumerateDevices', { value: async () => [] });
  });
  await page.route(`${url}/refund-address*`, route => route.abort('failed'));
  await page.goto(url);
  await expect(page.locator('#scan-refund')).toBeVisible();
  await page.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(page.locator('#scan-error')).toContainText('Camera unavailable');
  await expect(page.locator('#upload-refund')).toBeVisible();
  await captureCoverageStage(page, 'checkout-camera-fallback', test.info());
  await page.locator('#refund_address').fill(address);
  await expect(page.locator('#scan-error')).toContainText('Failed to fetch');
  await expect(page.locator('#refund-field')).not.toHaveClass(/is-saved/);
});

test('real checkout copies and selects payment address at narrow and wide widths', async ({ page, context, request }) => {
  const url = await checkoutUrl(request);
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: fixture.base_url });
  await page.goto(url);
  const input = page.locator('#address');
  await input.dblclick();
  expect(await input.evaluate(el => [el.selectionStart, el.selectionEnd])).toEqual([0, (await input.inputValue()).length]);
  await page.getByRole('button', { name: 'Copy payment address' }).click();
  await expect(page.locator('#copy-address')).toHaveAttribute('aria-label', 'Payment address copied');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(await input.inputValue());
  for (const width of [320, 700, 1280]) {
    await page.setViewportSize({ width, height: 900 });
    const geometry = await page.evaluate(() => ({ scrollWidth: document.documentElement.scrollWidth, viewport: innerWidth,
      overflows: Array.from(document.querySelectorAll('*')).filter(el => el.getBoundingClientRect().right > innerWidth + 1)
        .slice(0, 12).map(el => [el.tagName, el.id, el.className, Math.round(el.getBoundingClientRect().right)]) }));
    expect(geometry.scrollWidth, JSON.stringify(geometry)).toBeLessThanOrEqual(geometry.viewport + 1);
  }
});

test('real compact checkout positions refund controls below a full width field', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.setViewportSize({ width: 375, height: 800 });
  await page.goto(`${url}?view=compact`);
  const input = await page.locator('#refund_address').boundingBox();
  const controls = await page.locator('.refund-tools').boundingBox();
  const form = await page.locator('#refund-form').boundingBox();
  expect(input.width).toBeGreaterThan(form.width * .8);
  expect(controls.y).toBeGreaterThanOrEqual(input.y + input.height - 1);
  await captureCoverageStage(page, 'checkout-compact', test.info());
});

test('real tall checkout keeps one background and a readable progress bar', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.setViewportSize({ width: 700, height: 1400 });
  await page.goto(url);
  const layout = await page.evaluate(() => ({
    html: getComputedStyle(document.documentElement).backgroundColor,
    body: getComputedStyle(document.body).backgroundColor,
    bodyHeight: document.body.getBoundingClientRect().height,
    barHeight: document.querySelector('.progress-bar').getBoundingClientRect().height,
    viewportHeight: innerHeight,
  }));
  expect(layout.html).toBe(layout.body);
  expect(layout.bodyHeight).toBeGreaterThanOrEqual(layout.viewportHeight);
  expect(layout.barHeight).toBeGreaterThanOrEqual(14);
});

test('real checkout renders a paid order without opening a live stream', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  const paid = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/paid`);
  expect(paid.status()).toBe(204);
  await page.goto(url);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  // The stage says it's paid over a faded code: nothing invites a second payment.
  await expect(page.locator('.stage-track')).toContainText('Paid.');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  await captureCoverageStage(page, 'checkout-paid', test.info());
});

test('real checkout retains manual refund entry without JavaScript', async ({ browser, request }) => {
  const url = await checkoutUrl(request);
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(url);
    await expect(page.locator('#refund_address')).toBeVisible();
    await expect(page.locator('#scan-refund')).toBeHidden();
    await expect(page.locator('#upload-refund')).toBeHidden();
    await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
    await expect(page.locator('#checkout-refresh')).toHaveCount(1);
    await expect(page.getByRole('link', { name: 'Auto Refresh: ON' })).toBeVisible();
    await captureCoverageStage(page, 'checkout-no-js', test.info());
  } finally { await context.close(); }
});

test('client refund option frames the real checkout and retains its status stream', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  let statusRequests = 0;
  page.on('request', received => {
    if (received.url().startsWith(`${url}/events?`) || received.url().startsWith(`${url}/status`)) statusRequests++;
  });
  await page.goto(`${fixture.base_url}/__coverage/ready`);
  await page.setContent('<div id="mount"></div>');
  await page.addScriptTag({ url: `${fixture.base_url}/static/monokulo-client.js` });
  await page.evaluate(({ base_url, public_key, order_id }) => {
    window.Monokulo.mount('#mount', { orderId: order_id, endpoint: base_url, publicKey: public_key }, { refund: false });
  }, { base_url: fixture.base_url, public_key: fixture.public_key, order_id: url.split('/').pop() });
  await expect(page.locator('#mount iframe')).toHaveAttribute('src', `${url}?refund=false`);
  await expect(page.frameLocator('#mount iframe').locator('#checkout-root')).toBeVisible();
  await expect(page.frameLocator('#mount iframe').locator('#refund-form')).toHaveCount(0);
  await expect.poll(() => statusRequests).toBeGreaterThan(0);
});

test('client falls back to status polling when its stream is refused', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  let refused = 0;
  let polled = 0;
  await page.route(`${url}/events*`, route => {
    if (route.request().frame() !== page.mainFrame()) return route.continue();
    refused++;
    return route.fulfill({ status: 204, body: '' });
  });
  await page.route(`${url}/status`, route => {
    polled++;
    return route.fulfill({ json: { status: 'paid', confirmations: 1 } });
  });
  await page.goto(`${fixture.base_url}/__coverage/ready`);
  await page.setContent('<div id="mount"></div>');
  await page.addScriptTag({ url: `${fixture.base_url}/static/monokulo-client.js` });
  await page.evaluate(({ base_url, public_key, order_id }) => {
    window.Monokulo.mount('#mount', order_id, { endpoint: base_url, publicKey: public_key,
      onStatusChange: status => { document.querySelector('#mount').dataset.status = status; } });
  }, { base_url: fixture.base_url, public_key: fixture.public_key, order_id: url.split('/').pop() });
  await expect(page.locator('#mount iframe')).toHaveAttribute('src', url);
  await expect(page.locator('#mount')).toHaveAttribute('data-status', 'paid', { timeout: 10000 });
  expect(refused).toBeGreaterThan(0);
  expect(polled).toBeGreaterThan(0);
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
  const restricted = await request.post(`${fixture.base_url}/__coverage/embed/restricted`);
  expect(restricted.status()).toBe(204);
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  const flagged = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/browser-created`);
  expect(flagged.status()).toBe(204);
  const top = await page.goto(url);
  expect(top.status()).toBe(403);
  await expect(page.getByText('This store only shows its checkout inside its own website')).toBeVisible();
  await page.goto(`${fixture.base_url}/__coverage/ready`);
  await page.setContent(`<iframe id="payment" title="Payment" src="${url}"></iframe>`);
  await expect(page.frameLocator('#payment').locator('#checkout-root')).toBeVisible();
});

test('real checkout open while the customer pays shows paid and stops following the order', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  let streams = 0;
  page.on('request', sent => { if (sent.url().startsWith(`${url}/events?`)) streams++; });
  await page.goto(url);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'pending');
  await expect.poll(() => streams).toBe(1);
  const paid = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/paid`);
  expect(paid.status()).toBe(204);
  // The live stream carries the new state in; no reload.
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  // The stage says it's paid over a faded code: nothing invites a second payment.
  await expect(page.locator('.stage-track')).toContainText('Paid.');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  // A final order closes its stream for good rather than reconnecting.
  await page.waitForTimeout(5000);
  expect(streams).toBe(1);
});

test('real checkout keeps retrying a refused live stream with backoff and then follows the order', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  // The server refuses the stream while restarting, or past its per-client
  // limit of open streams (429); the page must not give up on live updates.
  const attempts = [];
  await page.route(`${url}/events?*`, route => {
    attempts.push(Date.now());
    if (attempts.length <= 2) return route.fulfill({ status: attempts.length === 1 ? 503 : 429, body: '' });
    return route.continue();
  });
  // The page schedules each retry when it handles the refusal (fixi's
  // fx:after), not when the route sees the request. Counting those lets the
  // test wait for the page before moving the fake clock: on a busy runner a
  // refusal handled after the clock jumped would schedule its retry late.
  await page.addInitScript(() => {
    window.__refusals = 0;
    document.addEventListener('fx:after', event => {
      const response = event.detail?.cfg?.response;
      if (response && !response.ok) window.__refusals++;
    }, true);
  });
  const refusals = () => page.evaluate(() => window.__refusals);
  await page.clock.install();
  await page.goto(url);
  await expect.poll(() => attempts.length).toBe(1);
  await expect.poll(refusals).toBe(1);
  // Each "not yet" check lets a request the fake clock just released reach
  // the route before counting.
  await page.clock.runFor(4000);
  await page.waitForTimeout(500);
  expect(attempts.length, 'waits 5s before the first retry').toBe(1);
  await page.clock.runFor(1500);
  await expect.poll(() => attempts.length).toBe(2);
  await expect.poll(refusals).toBe(2);
  await page.clock.runFor(9000);
  await page.waitForTimeout(500);
  expect(attempts.length, 'then doubles the wait to 10s').toBe(2);
  await page.clock.runFor(1500);
  await expect.poll(() => attempts.length).toBe(3);
  const paid = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/paid`);
  expect(paid.status()).toBe(204);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
});

test('real checkout copies the payment address without the Clipboard API, as over plain-HTTP onion', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  // A plain-HTTP origin (a Tor .onion service) is not a secure context, so
  // navigator.clipboard is absent; the legacy copy command still works.
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, 'clipboard', { get: () => undefined });
    window.__copied = [];
    document.execCommand = command => {
      if (command !== 'copy' || window.__copyBlocked) return false;
      window.__copied.push(document.getSelection().toString() || document.activeElement.value.slice(document.activeElement.selectionStart, document.activeElement.selectionEnd));
      return true;
    };
  });
  await page.goto(url);
  const address = await page.locator('#address').inputValue();
  const copy = page.locator('#copy-address');
  await copy.click();
  await expect(copy).toHaveAttribute('aria-label', 'Payment address copied');
  expect(await page.evaluate(() => window.__copied)).toEqual([address]);

  // Where even that is refused, the address is left selected to copy by hand.
  await page.evaluate(() => { window.__copyBlocked = true; });
  await copy.click();
  await expect(copy).toHaveAttribute('aria-label', 'Could not copy; address selected');
  expect(await page.locator('#address').evaluate(el => el.value.slice(el.selectionStart, el.selectionEnd))).toBe(address);
});

test('real checkout saves the address the customer ends with when they change it mid-save', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const first = address;
  // The Monero General Fund's published address: another valid mainnet one.
  const second = '44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A';
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

test('real checkout saves on Enter without leaving the page', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const input = page.locator('#refund_address');
  // Without script the form posts and the server redirects back; with it,
  // Enter saves in place, so the page (and a live camera or stream) stays.
  await page.evaluate(() => { window.__samePage = true; });
  await input.fill(address);
  await input.press('Enter');
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  await page.waitForTimeout(500);
  expect(await page.evaluate(() => window.__samePage)).toBe(true);
  expect(page.url()).toBe(url);
});

test('real checkout reads a wallet payment-URI QR and explains images it cannot use', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const input = page.locator('#refund_address');
  const scanError = page.locator('#scan-error');
  async function choose(file) {
    const chooser = page.waitForEvent('filechooser');
    await page.getByRole('button', { name: 'Choose QR image' }).click();
    await (await chooser).setFiles(path.join(__dirname, '../fixtures', file));
  }
  // A photo that happens to contain no QR code at all.
  await choose('receipt-photo-no-qr.png');
  await expect(scanError).toHaveText('No QR code found in that image.');
  // A QR code, but not a Monero one (a shop's menu link).
  await choose('menu-link-qr.png');
  await expect(scanError).toHaveText('This QR code does not contain a Monero address.');
  await expect(input).toHaveValue('');
  // A wallet's "receive" QR is a monero: URI with an amount and a label;
  // only the address is the refund address.
  await choose('refund-qr-wallet-uri.png');
  await expect(input).toHaveValue(address);
  await expect(scanError).toBeHidden();
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
});

async function customerSends(request, orderId, fraction, confirmations) {
  const depth = confirmations === undefined ? '' : `&confirmations=${confirmations}`;
  const sent = await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/payment?fraction=${fraction}${depth}`);
  expect(sent.status()).toBe(204);
}

test('real checkout follows the customer payment from the mempool to confirmed and lists it', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  await page.goto(url);
  await expect(page.locator('#status-badge')).toHaveText('Waiting for payment');
  await customerSends(request, orderId, 1);
  await expect(page.locator('#status-badge')).toHaveText('Payment seen, unconfirmed');
  // Seen in full: the track is at Confirm, and the code fades so it isn't paid twice.
  await expect(page.locator('.track .step-now')).toContainText('Confirm');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  // The payment is listed by a shortened txid, not yet confirmed.
  await expect(page.locator('.payments-table')).toContainText(`test-pay…`);
  await expect(page.locator('.progress-fill')).toHaveAttribute('style', /width: 0%/);
  await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/confirm`);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  await expect(page.locator('.progress-fill')).toHaveAttribute('style', /width: 100%/);
  await captureCoverageStage(page, 'checkout-paid-live', test.info());
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
  await expect(page.locator('.stage-track')).toContainText('Send the remaining 0.00075 XMR.');
  await expect(page.locator('.stage-track')).toContainText('0.00025 of 0.001 XMR received.');
  await expect(page.locator('#xmr-amount')).toHaveText('0.00075 XMR');
  await expect(page.locator('.qr-wrap.qr-new svg')).toBeVisible();
  expect(await code()).not.toBe(first);
  // They send the full amount again instead of the rest.
  await customerSends(request, orderId, 1, 20);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'overpaid');
  await expect(page.locator('#status-badge')).toHaveText('Overpaid');
  await expect(page.locator('.stage-track')).toContainText('(0.00025 XMR extra). Do not send more. Contact the merchant about the extra amount.');
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  await expect(page.locator('.payments-table tbody tr')).toHaveCount(2);
});

test('real checkout says so when the chosen refund QR photo cannot be read', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const chooser = page.waitForEvent('filechooser');
  await page.getByRole('button', { name: 'Choose QR image' }).click();
  await (await chooser).setFiles(path.join(__dirname, '../fixtures/corrupt-photo.png'));
  await expect(page.locator('#scan-error')).toHaveText('Could not read that image. Choose another file.');
  await expect(page.locator('#refund_address')).toHaveValue('');
});

test('real checkout keeps the amount and XMR on one line and shows a fiat equivalent only for fiat orders', async ({ page, request }) => {
  const xmrOrder = await checkoutUrl(request);
  const aud = await (await request.post(`${fixture.base_url}/pay/${fixture.public_key}/orders`, { data: { amount: '12.50', currency: 'AUD' } })).json();
  const audOrder = `${fixture.base_url}/pay/${fixture.public_key}/orders/${aud.order_id}`;
  const oneLine = () => page.evaluate(() => {
    const amount = document.getElementById('xmr-amount');
    const value = amount.querySelector('.amount-value').getBoundingClientRect();
    const unit = amount.querySelector('.amount-unit').getBoundingClientRect();
    const column = amount.parentElement.getBoundingClientRect();
    return { sameLine: Math.abs(value.bottom - unit.bottom) < 4, fits: unit.right <= column.right + 1 && value.left >= column.left - 1 };
  });
  for (const view of ['', '?view=compact']) {
    for (const width of [280, 320, 420, 900]) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(xmrOrder + view);
      expect(await oneLine(), `${width}px${view}`).toEqual({ sameLine: true, fits: true });
    }
  }
  // The longest an amount gets: all twelve decimals, in the narrowest frame.
  const long = await (await request.post(`${fixture.base_url}/pay/${fixture.public_key}/orders`, { data: { amount: '0.123456789012', currency: 'XMR' } })).json();
  for (const view of ['', '?view=compact']) {
    await page.setViewportSize({ width: 280, height: 900 });
    await page.goto(`${fixture.base_url}/pay/${fixture.public_key}/orders/${long.order_id}${view}`);
    await expect(page.locator('#xmr-amount')).toHaveText('0.123456789012 XMR');
    expect(await oneLine(), `longest amount at 280px${view}`).toEqual({ sameLine: true, fits: true });
  }
  await page.goto(xmrOrder);
  // The fixture order is 0.001 XMR: shown without its trailing zeros, and
  // with no "≈" line since it is priced in XMR.
  await expect(page.locator('#xmr-amount')).toHaveText('0.001 XMR');
  await expect(page.locator('.fiat-amount')).toHaveCount(0);
  await page.goto(audOrder);
  await expect(page.locator('.fiat-amount')).toHaveText('≈ 12.50 AUD');
  // 12.50 AUD at the fixture's 400 AUD per XMR.
  await expect(page.locator('#xmr-amount')).toHaveText('0.03125 XMR');
});

test('real checkout takes its theme from ?theme=, the embed option, and the signed-in viewer on the share page', async ({ page, context, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  const streams = [];
  page.on('request', sent => { if (sent.url().includes('/events?')) streams.push(sent.url()); });
  const background = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  await page.goto(`${url}?theme=light`);
  const light = await background();
  await page.goto(`${url}?theme=dark`);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  expect(await background(), 'the dark theme actually applies').not.toBe(light);
  // The page's own form and live stream keep the theme.
  await expect(page.locator('#refund-form')).toHaveAttribute('action', /theme=dark/);
  await expect.poll(() => streams.some(u => u.includes('theme=dark'))).toBe(true);
  // Anything but light or dark follows the device.
  await page.goto(`${url}?theme=neon`);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', /./);

  // An integrator's page picks it with the embed option.
  await page.goto(`${fixture.base_url.replace('127.0.0.1', 'localhost')}/__coverage/ready`);
  await page.setContent('<div id="pay"></div>');
  await page.addScriptTag({ url: `${fixture.base_url}/static/monokulo-client.js` });
  await page.evaluate(({ base_url, public_key, order_id }) => {
    window.Monokulo.mount('#pay', order_id, { endpoint: base_url, publicKey: public_key, theme: 'light' });
  }, { base_url: fixture.base_url, public_key: fixture.public_key, order_id: orderId });
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', `${url}?theme=light`);
  await expect(page.frameLocator('#pay iframe').locator('html')).toHaveAttribute('data-theme', 'light');

  // Monokulo's share page frames the checkout in a signed-in merchant's theme.
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
  const pick = theme => page.request.post(`${fixture.base_url}/dashboard/theme`, { form: { theme, next: '/dashboard' }, maxRedirects: 0 });
  await pick('dark');
  await page.goto(`${url}/share`);
  await expect(page.locator('#checkout-frame')).toHaveAttribute('src', `/pay/${fixture.public_key}/orders/${orderId}?theme=dark`);
  await expect(page.frameLocator('#checkout-frame').locator('html')).toHaveAttribute('data-theme', 'dark');
  await pick('system');
  await page.goto(`${url}/share`);
  await expect(page.locator('#checkout-frame')).toHaveAttribute('src', `/pay/${fixture.public_key}/orders/${orderId}`);
});

test('real checkout payment problems as the customer sees them (UI stages)', async ({ page, request }) => {
  const problems = [
    ['checkout-underpaid', async id => customerSends(request, id, 0.4, 20), 'partial'],
    ['checkout-overpaid', async id => customerSends(request, id, 1.5, 20), 'overpaid'],
    ['checkout-double-spend', async id => {
      await customerSends(request, id, 1);
      await request.post(`${fixture.base_url}/__coverage/orders/${id}/double-spend`);
    }, 'unconfirmed'],
    ['checkout-expired', async id => request.post(`${fixture.base_url}/__coverage/orders/${id}/expired`), 'expired'],
  ];
  for (const [stage, happen, status] of problems) {
    const url = await checkoutUrl(request);
    await page.goto(url);
    await happen(url.split('/').pop());
    await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', status);
    if (stage === 'checkout-double-spend') await expect(page.locator('.stage-track.state-double-spend')).toBeVisible();
    else await expect(page.locator(`.stage-track.state-${status}`)).toBeVisible();
    await captureCoverageStage(page, stage, test.info());
  }
});

test('real checkout leads with the stage in one column and fades the code once paid', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  const orderId = url.split('/').pop();
  for (const [width, height, columns] of [[390, 844, 1], [820, 1180, 1], [1280, 800, 1], [844, 390, 2]]) {
    await page.setViewportSize({ width, height });
    await page.goto(url);
    const layout = await page.evaluate(() => {
      const stage = document.querySelector('.stage-track').getBoundingClientRect();
      const code = document.querySelector('.qr-wrap').getBoundingClientRect();
      const refund = document.querySelector('.pay-col-secondary').getBoundingClientRect();
      return { stageAbove: stage.bottom <= code.top, beside: refund.left >= code.right, stageTop: stage.top < code.bottom };
    });
    // One column: the stage over the code, the rest under it. Two columns
    // (a short landscape screen): the stage heads the column beside the code.
    if (columns === 1) expect(layout, `${width}x${height}`).toMatchObject({ stageAbove: true, beside: false });
    else expect(layout, `${width}x${height}`).toMatchObject({ beside: true, stageTop: true });
    await expect(page.locator('.stage-expiry')).toHaveText(/left$/);
  }
  // The code stays black on white while it's payable.
  expect(await page.evaluate(() => getComputedStyle(document.querySelector('.qr-wrap svg path')).fill)).toBe('rgb(0, 0, 0)');
  await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/paid`);
  await expect(page.locator('#checkout-root')).toHaveAttribute('data-status', 'paid');
  // Paid: the code and the address fade in place (the copy button goes),
  // and the progress bar takes the paid colour (--state-paid-border).
  await expect(page.locator('.qr-wrap.is-spent')).toBeVisible();
  await expect.poll(() => page.evaluate(() => Number(getComputedStyle(document.querySelector('.qr-wrap svg')).opacity))).toBeLessThan(0.2);
  await expect(page.locator('.address-block')).toBeVisible();
  await expect(page.locator('#copy-address')).toBeHidden();
  await expect.poll(() => page.evaluate(() => getComputedStyle(document.getElementById('progress-fill')).backgroundColor)).toBe('rgb(26, 127, 55)');
  await captureCoverageStage(page, 'checkout-paid-faded', test.info());
});

test('real checkout shows times in the customer\'s own zone, or the one ?timezone= names', async ({ browser, request }) => {
  const context = await browser.newContext({ timezoneId: 'Asia/Tokyo' });
  try {
    const page = await context.newPage();
    const url = await checkoutUrl(request);
    const orderId = url.split('/').pop();
    await customerSends(request, orderId, 1);
    await request.post(`${fixture.base_url}/__coverage/orders/${orderId}/double-spend`);
    // What the server says, in a given zone, in the page's own shape.
    const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    const shown = (at, timeZone) => {
      const parts = Object.fromEntries(new Intl.DateTimeFormat('en-GB', { timeZone, day: 'numeric', month: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
        .formatToParts(at).map(part => [part.type, part.value]));
      return `${Number(parts.day)} ${MONTHS[Number(parts.month) - 1]}, ${parts.hour}:${parts.minute}`;
    };
    const time = page.locator('#double-spend-time time');

    // No ?timezone=: sent in UTC, then shown in the browser's zone.
    await page.goto(url);
    await expect(time).not.toHaveAttribute('data-local', /.*/);
    const at = new Date(await time.getAttribute('datetime'));
    await expect(time).toHaveText(shown(at, 'Asia/Tokyo'));

    // ?timezone= wins, and the script leaves it alone.
    await page.goto(`${url}?timezone=Europe%2FLondon`);
    await expect(time).toHaveText(shown(at, 'Europe/London'));
    await expect(time).toHaveAttribute('title', /\(Europe\/London\)$/);

    // Without JavaScript: UTC, and it says so.
    const noJs = await browser.newContext({ timezoneId: 'Asia/Tokyo', javaScriptEnabled: false });
    try {
      const plain = await noJs.newPage();
      await plain.goto(url);
      await expect(plain.locator('#double-spend-time time')).toHaveText(shown(at, 'UTC'));
      await expect(plain.locator('#double-spend-time time')).toHaveAttribute('title', /\(UTC\)$/);
    } finally { await noJs.close(); }
  } finally { await context.close(); }
});
