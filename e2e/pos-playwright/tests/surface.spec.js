// @ts-check
const { test, expect } = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '../../..');
const address = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
const qrImage = path.join(__dirname, '../fixtures/refund-qr.png');
const host = 'http://localhost:8787';

function pageHtml(initialSaved = false) {
  return `<!doctype html><html><head></head><body>
    <div id="checkout-root" data-order-id="order-1" data-status="pending" data-confirmations="0" data-error="">
      <div id="live-status" data-live><span id="status-badge">Waiting for payment</span></div>
      <noscript><p class="refresh-toggle"><a href="/pay/pk/orders/order-1?refresh=false">Auto Refresh: ON</a></p></noscript>
      <form id="refund-form" action="/refund-address" method="post">
        <div id="refund-field" class="refund-field${initialSaved ? ' is-saved' : ''}">
          <input id="refund_address" name="refund_address" value="${initialSaved ? address : ''}">
          <span id="refund-save-state" role="status" aria-label="${initialSaved ? 'Refund address saved' : ''}"></span>
          <button type="button" id="scan-refund" aria-label="Scan refund QR" hidden><svg></svg></button>
          <button type="button" id="upload-refund" aria-label="Choose QR image" hidden><svg></svg></button>
          <input type="file" id="refund-image" accept="image/*" hidden>
        </div>
        <video id="refund-camera" autoplay playsinline hidden></video>
        <p id="scan-error" role="alert" hidden></p>
        <noscript><button type="submit">Save</button></noscript>
      </form>
    </div>
    <script src="/static/jsQR.js"></script><script src="/static/checkout.js"></script>
  </body></html>`;
}

async function routeCheckout(page, getStatus = () => 'pending', save = async () => ({ status: 200, body: { ok: true } }), initialSaved = false) {
  const submissions = [];
  await page.route(`${host}/**`, async route => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === '/refund-address' && route.request().method() === 'POST') {
      submissions.push(new URLSearchParams(route.request().postData()).get('refund_address'));
      const response = await save();
      return route.fulfill({ status: response.status, contentType: 'application/json', body: JSON.stringify(response.body) });
    }
    if (pathname === '/static/jsQR.js') {
      return route.fulfill({ contentType: 'text/javascript', body: fs.readFileSync(path.join(root, 'crates/monokulo/static/jsQR.js')) });
    }
    if (pathname === '/static/checkout.js') {
      return route.fulfill({ contentType: 'text/javascript', body: fs.readFileSync(path.join(root, 'crates/monokulo/static/checkout.js')) });
    }
    if (pathname.endsWith('/events')) {
      const status = getStatus();
      const label = status === 'partial' ? 'Partial payment received' : 'Waiting for payment';
      const data = JSON.stringify({ status, confirmations: 0, confirmations_required: 10, is_terminal: false, error: status === 'partial' ? 'Underpaid' : null });
      return route.fulfill({
        contentType: 'text/event-stream',
        body: `retry: 200\n\nevent: fragment\ndata: <div id="live-status" data-live><span id="status-badge">${label}</span></div>\n\nevent: status\ndata: ${data}\n\n`,
      });
    }
    return route.fulfill({ contentType: 'text/html', body: pageHtml(initialSaved) });
  });
  return submissions;
}

test('QR upload automatically saves and the field can be edited again', async ({ page }) => {
  const submissions = await routeCheckout(page);
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await expect(page.locator('#upload-refund')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Save' })).toBeHidden();
  const chooser = page.waitForEvent('filechooser');
  await page.getByRole('button', { name: 'Choose QR image' }).click();
  await (await chooser).setFiles(qrImage);
  await expect(page.locator('#refund_address')).toHaveValue(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Refund address saved');
  expect(submissions).toEqual([address]);
  await page.locator('#refund_address').click();
  await expect(page.locator('#refund-field')).not.toHaveClass(/is-saved/);
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', '');
});

test('an address already saved on the server starts confirmed and remains editable', async ({ page }) => {
  const submissions = await routeCheckout(page, () => 'pending', async () => ({ status: 200, body: { ok: true } }), true);
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  await expect(page.locator('#refund_address')).toHaveValue(address);
  await page.locator('#refund_address').click();
  await expect(page.locator('#refund-field')).not.toHaveClass(/is-saved/);
  await page.locator('#refund_address').fill('');
  await page.locator('#refund_address').blur();
  await expect(page.locator('#refund_address')).toHaveValue(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Refund address saved');
  expect(submissions).toEqual([]);
});

test('an untouched empty refund field stays neutral and a cleared saved field restores on blur', async ({ page }) => {
  const submissions = await routeCheckout(page);
  await page.goto(`${host}/pay/pk/orders/order-1`);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  const indicator = page.locator('#refund-save-state');
  await input.focus();
  await input.blur();
  await expect(field).not.toHaveClass(/is-saved|is-saving|is-invalid/);
  await expect(indicator).toHaveAttribute('aria-label', '');

  await input.fill(address);
  await expect(field).toHaveClass(/is-saved/);
  await input.fill('');
  await input.blur();
  await expect(input).toHaveValue(address);
  await expect(field).toHaveClass(/is-saved/);
  await expect(indicator).toHaveAttribute('aria-label', 'Refund address saved');
  expect(submissions).toEqual([address]);
});

test('live updates swap changed regions in place without touching a refund address mid-edit', async ({ page }) => {
  let status = 'pending';
  const submissions = await routeCheckout(page, () => status);
  const eventsRequest = page.waitForRequest(request => request.url().includes('/events?') && request.url().includes('fragments=true'));
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await eventsRequest;
  await page.locator('#refund_address').fill('4AdUndXHHZ');
  status = 'partial';
  await expect(page.locator('#status-badge')).toHaveText('Partial payment received');
  await expect(page.locator('#refund_address')).toHaveValue('4AdUndXHHZ');
  await expect(page.locator('#refund_address')).toBeFocused();
  expect(submissions).toEqual([]);
});

test('checkout fills a tall iframe with one background and a thicker progress bar', async ({ page }) => {
  const source = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/checkout.rs'), 'utf8');
  const style = source.match(/const CHECKOUT_STYLE: &str = r#"([\s\S]*?)"#;/)[1];
  const head = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/head.html'), 'utf8');
  await page.setViewportSize({ width: 700, height: 1400 });
  await page.setContent(`${head}<style>${style}</style><div class="pay-wrap"><div class="progress-bar"></div></div>`);
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

test('payment address is unboxed, copies with its button, and selects fully on double click', async ({ page }) => {
  const source = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/checkout.rs'), 'utf8');
  const style = source.match(/const CHECKOUT_STYLE: &str = r#"([\s\S]*?)"#;/)[1];
  const head = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/head.html'), 'utf8');
  await page.setContent(`${head}<style>${style}</style><div id="checkout-root" data-status="paid">
    <div class="address-block"><label class="address-label" id="address-label" for="address">Send exactly this amount to</label>
      <div class="address-row"><button type="button" id="copy-address" class="address-copy-btn" aria-label="Copy payment address" hidden><svg viewBox="0 0 24 24"><rect x="9" y="9" width="12" height="12" rx="1"></rect><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"></path></svg></button>
      <textarea class="address-text" id="address" readonly aria-labelledby="address-label">${address}</textarea></div>
    </div></div>`);
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: async value => { window.copiedAddress = value; } },
    });
  });
  await page.addScriptTag({ path: path.join(root, 'crates/monokulo/static/checkout.js') });
  await expect(page.locator('#copy-address')).toBeVisible();
  const alignment = await page.evaluate(() => {
    const addressField = document.querySelector('#address');
    const icon = document.querySelector('#copy-address svg');
    const lineHeight = parseFloat(getComputedStyle(addressField).lineHeight);
    return Math.abs((icon.getBoundingClientRect().top + icon.getBoundingClientRect().height / 2) - (addressField.getBoundingClientRect().top + lineHeight / 2));
  });
  expect(alignment).toBeLessThan(2);
  for (const width of [1280, 750, 360]) {
    await page.setViewportSize({ width, height: 720 });
    const addressSize = await page.locator('#address').evaluate(element => ({
      visibleHeight: element.clientHeight,
      contentHeight: element.scrollHeight,
    }));
    expect(addressSize.visibleHeight, `address clipped at ${width}px`).toBeGreaterThanOrEqual(addressSize.contentHeight);
  }
  await page.locator('#address').dblclick();
  const selection = await page.locator('#address').evaluate(element => ({
    start: element.selectionStart,
    end: element.selectionEnd,
    length: element.value.length,
    scrollTop: element.scrollTop,
    borderWidth: getComputedStyle(element).borderTopWidth,
    outlineStyle: getComputedStyle(element).outlineStyle,
  }));
  expect(selection).toMatchObject({ start: 0, end: address.length, length: address.length, scrollTop: 0, borderWidth: '0px', outlineStyle: 'none' });
  await page.locator('#copy-address').click();
  await expect.poll(() => page.evaluate(() => window.copiedAddress)).toBe(address);
  await expect(page.locator('#copy-address')).toHaveAttribute('aria-label', 'Payment address copied');
});

test('POS top bar reserves its full height above the payment iframe', async ({ page }) => {
  const source = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/pos.rs'), 'utf8');
  const style = source.match(/const POS_STYLE: &str = r#"([\s\S]*?)"#;/)[1];
  const head = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/head.html'), 'utf8');
  await page.setContent(`${head}<style>${style}</style>
    <div class="pos-topbar">
      <div class="pos-topbar-start"><a class="pos-back" href="#">←</a><span class="pos-title">POS · rachelshandicrafts.com</span></div>
      <button class="secondary-btn pos-topbar-action pos-screen-hidden" id="background-btn">Confirm in background</button>
      <a class="pos-status-link" href="#"><span class="status-dot"></span></a>
    </div>
    <div class="pos-wrap"><div id="payment-screen" class="pos-screen">
      <div class="payment-panel"><iframe title="Monero payment"></iframe><button class="secondary-btn">Cancel</button></div>
    </div></div>`);

  for (const { width, height } of [{ width: 1126, height: 700 }, { width: 667, height: 375 }, { width: 360, height: 740 }]) {
    await page.setViewportSize({ width, height });
    for (const showAction of [false, true]) {
      await page.locator('#background-btn').evaluate((button, show) => button.classList.toggle('pos-screen-hidden', !show), showAction);
      const bounds = await page.evaluate(() => {
        const bar = document.querySelector('.pos-topbar').getBoundingClientRect();
        const panel = document.querySelector('.payment-panel').getBoundingClientRect();
        const frame = document.querySelector('.payment-panel iframe').getBoundingClientRect();
        return { barBottom: bar.bottom, panelTop: panel.top, frameTop: frame.top, frameBottom: frame.bottom, viewportHeight: innerHeight };
      });
      expect(bounds.panelTop).toBeGreaterThanOrEqual(bounds.barBottom);
      expect(bounds.frameTop).toBeGreaterThanOrEqual(bounds.barBottom);
      expect(bounds.frameBottom).toBeLessThanOrEqual(bounds.viewportHeight);
    }
  }
});

test('refund QR controls stay vertically centered within the input', async ({ page }) => {
  const source = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/checkout.rs'), 'utf8');
  const style = source.match(/const CHECKOUT_STYLE: &str = r#"([\s\S]*?)"#;/)[1];
  const head = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/head.html'), 'utf8');
  await page.setContent(`${head}<style>${style}</style><div class="checkout-compact"><div class="refund-field">
    <input type="text" id="refund_address" placeholder="Your Monero refund address">
    <span class="refund-tools"><button class="refund-icon-btn" id="scan-refund"><svg viewBox="0 0 24 24"></svg></button>
    <button class="refund-icon-btn" id="upload-refund"><svg viewBox="0 0 24 24"></svg></button></span>
    <input type="file" hidden></div></div>`);
  for (const width of [360, 700]) {
    await page.setViewportSize({ width, height: 700 });
    const positions = await page.evaluate(() => {
      const input = document.querySelector('#refund_address').getBoundingClientRect();
      return ['#scan-refund', '#upload-refund'].map(selector => {
        const button = document.querySelector(selector).getBoundingClientRect();
        return { offset: button.top + button.height / 2 - (input.top + input.height / 2), inside: button.top >= input.top && button.bottom <= input.bottom };
      });
    });
    for (const position of positions) {
      expect(Math.abs(position.offset)).toBeLessThan(1);
      expect(position.inside).toBe(true);
    }
  }
});

test('rough validation waits for a complete address and shows saving before success', async ({ page }) => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const submissions = await routeCheckout(page, () => 'pending', async () => { await gate; return { status: 200, body: { ok: true } }; });
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await page.locator('#refund_address').fill('invalid');
  await page.waitForTimeout(600);
  expect(submissions).toEqual([]);
  await page.locator('#refund_address').blur();
  await expect(page.locator('#scan-error')).toContainText('95 or 106');
  await page.locator('#refund_address').fill(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saving/);
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Saving refund address');
  expect(submissions).toEqual([address]);
  release();
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
});

test('server validation failure shows an invalid address in red and clears on edit', async ({ page }) => {
  let attempts = 0;
  const submissions = await routeCheckout(page, () => 'pending', async () => {
    attempts++;
    return attempts === 1
      ? { status: 400, body: { error: 'Enter a valid Monero address for this store\'s network.' } }
      : { status: 200, body: { ok: true } };
  });
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await page.locator('#refund_address').fill(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-invalid/);
  await expect(page.locator('#refund_address')).toHaveAttribute('aria-invalid', 'true');
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Invalid refund address');
  await expect(page.locator('#scan-error')).toContainText('Invalid refund address');
  await page.locator('#refund_address').fill(address.slice(0, -1) + 'D');
  await expect(page.locator('#refund-field')).not.toHaveClass(/is-invalid/);
  await expect(page.locator('#refund_address')).toHaveAttribute('aria-invalid', 'false');
  await expect(page.locator('#scan-error')).toBeHidden();
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  expect(submissions).toEqual([address, address.slice(0, -1) + 'D']);
});

test('a network error shows a retryable message', async ({ page }) => {
  await routeCheckout(page, () => 'pending', async () => ({ status: 502, body: { error: 'Something went wrong saving that. Please try again.' } }));
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await page.locator('#refund_address').fill(address);
  await expect(page.locator('#scan-error')).toContainText('Please try again');
  await expect(page.locator('#refund-field')).not.toHaveClass(/is-saved|is-saving|is-invalid/);
});

test('camera failure offers the QR image upload path', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'mediaDevices', {
      value: { getUserMedia: () => Promise.reject(new DOMException('No camera', 'NotFoundError')) },
    });
  });
  await routeCheckout(page);
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await page.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(page.locator('#scan-error')).toHaveText('Camera unavailable. Choose a QR image instead.');
  await expect(page.getByRole('button', { name: 'Choose QR image' })).toBeVisible();
});

test('an empty device list does not hide the camera button before permission', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'mediaDevices', {
      value: { getUserMedia: () => Promise.reject(new Error('No camera')), enumerateDevices: () => Promise.resolve([]) },
    });
  });
  await routeCheckout(page);
  await page.goto(`${host}/pay/pk/orders/order-1`);
  await expect(page.getByRole('button', { name: 'Choose QR image' })).toBeVisible();
  await page.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(page.locator('#scan-error')).toHaveText('Camera unavailable. Choose a QR image instead.');
});

test('manual refund entry remains available without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await routeCheckout(page);
    await page.goto(`${host}/pay/pk/orders/order-1`);
    await expect(page.locator('#refund_address')).toBeVisible();
    await expect(page.locator('#scan-refund')).toBeHidden();
    await expect(page.locator('#upload-refund')).toBeHidden();
    await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Auto Refresh: ON' })).toBeVisible();
  } finally { await context.close(); }
});

test('status indicator shows its rendered health, then follows changes by polling', async ({ page }) => {
  const viewsSource = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/mod.rs'), 'utf8');
  const script = viewsSource.match(/const STATUS_INDICATOR_SCRIPT: &str = r#"([\s\S]*?)"#;/)[1];
  let health = true;
  let polls = 0;
  await page.clock.install();
  await page.route(`${host}/**`, async route => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === '/static/status.js') return route.fulfill({ contentType: 'text/javascript', body: script });
    if (pathname === '/status/summary') {
      polls++;
      return health === null
        ? route.fulfill({ status: 503, contentType: 'text/plain', body: 'Unavailable' })
        : route.fulfill({ contentType: 'application/json', body: JSON.stringify({ healthy: health }) });
    }
    return route.fulfill({ contentType: 'text/html', body: `<!doctype html>
      <a href="/status" id="status-indicator" title="an issue was detected - see the status page"><span class="status-dot status-dot-error"></span></a>
      <script src="/static/status.js"></script>` });
  });
  await page.goto(`${host}/dashboard/stores/conn-1/pos`);
  const dot = page.locator('#status-indicator .status-dot');
  // A known health is shown as rendered; the first poll waits a full interval.
  await expect(dot).toHaveClass(/status-dot-error/);
  expect(polls).toBe(0);
  await page.clock.runFor(30000);
  await expect(dot).toHaveClass(/status-dot-ok/);
  health = null;
  await page.clock.runFor(30000);
  await expect(dot).toHaveClass(/status-dot-unknown/);
  await expect(page.locator('#status-indicator')).toHaveAttribute('title', 'could not check status');
});

test('POS backgrounds a confirming order into the top list and reopens it', async ({ page }) => {
  const posSource = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/pos.rs'), 'utf8');
  const script = posSource.match(/const POS_SCRIPT: &str = r#"\n([\s\S]*?)\n"#;/)[1];
  let status = 'pending';
  await page.route(`${host}/**`, async route => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (pathname === '/static/pos.js') return route.fulfill({ contentType: 'text/javascript', body: script });
    if (pathname === '/status/summary') return route.fulfill({ contentType: 'application/json', body: '{"healthy":true}' });
    if (pathname.endsWith('/pos/orders') && request.method() === 'POST') {
      return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ order_id: 'order-1', amount: '1', currency: 'XMR', xmr_amount: '1.000000000000', address }) });
    }
    if (pathname.endsWith('/pos/events')) {
      expect(new URL(request.url()).searchParams.get('orders')).toBe('order-1');
      const data = JSON.stringify({ order_id: 'order-1', status, confirmations: 0, confirmations_required: 1, is_terminal: false, error: null });
      return route.fulfill({ contentType: 'text/event-stream', body: `retry: 200\n\nevent: status\ndata: ${data}\n\n` });
    }
    if (pathname.startsWith('/pay/')) return route.fulfill({ contentType: 'text/html', body: '<h1>Shared payment view</h1>' });
    return route.fulfill({ contentType: 'text/html', body: `<!doctype html><style>.pos-screen-hidden,[hidden]{display:none!important}</style>
      <div id="pos-config" data-connection-id="conn-1" data-public-key="pk-1" data-decimals="2"></div>
      <button id="background-btn" class="pos-screen-hidden">Confirm in background</button>
      <details id="background-disclosure" hidden><summary id="background-summary">Background orders (0)</summary><div id="bg-stack"></div></details>
      <div id="keypad-screen"><span id="amount-display"></span><button class="key" data-digit="1">1</button>
        <button id="key-backspace">Back</button><button id="key-clear">Clear</button>
        <input id="note-input"><button id="charge-btn" disabled>Charge</button><p id="pos-error"></p></div>
      <div id="payment-screen" class="pos-screen-hidden"><iframe id="payment-frame"></iframe><p id="payment-error" hidden></p>
        <button id="dismiss-btn" class="pos-screen-hidden">Dismiss</button><button id="cancel-btn">Cancel</button></div>
      <script src="/static/pos.js"></script>` });
  });
  await page.goto(`${host}/dashboard/stores/conn-1/pos`);
  await page.getByRole('button', { name: '1' }).click();
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('#payment-frame')).toHaveAttribute('src', '/pay/pk-1/orders/order-1?view=compact');
  status = 'unconfirmed';
  await expect(page.locator('#background-btn')).toBeVisible({ timeout: 5000 });
  await page.locator('#background-btn').click();
  await expect(page.locator('#keypad-screen')).toBeVisible();
  await expect(page.locator('#background-summary')).toHaveText('Background orders (1)');
  await page.locator('#background-summary').click();
  await expect(page.locator('.bg-item')).toBeVisible();
  await page.locator('.bg-item').click();
  await expect(page.locator('#payment-screen')).toBeVisible();
  await expect(page.locator('#background-disclosure')).toBeHidden();
});

test('embed refund option changes the iframe URL without changing status updates', async ({ page }) => {
  let statusRequests = 0;
  await page.route(`${host}/**`, async route => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === '/static/monokulo-client.js') {
      return route.fulfill({ contentType: 'text/javascript', body: fs.readFileSync(path.join(root, 'crates/monokulo/static/monokulo-client.js')) });
    }
    if (pathname.endsWith('/events')) {
      statusRequests++;
      return route.fulfill({ contentType: 'text/event-stream', body: 'event: status\ndata: {"status":"pending","confirmations":0}\n\n' });
    }
    if (pathname.startsWith('/pay/')) return route.fulfill({ contentType: 'text/html', body: '<h1>Payment</h1>' });
    return route.fulfill({ contentType: 'text/html', body: '<div id="mount"></div><script src="/static/monokulo-client.js"></script>' });
  });
  await page.goto(host);
  await page.evaluate(base => { window.Monokulo.mount('#mount', { orderId: 'order-1', endpoint: base, publicKey: 'pk' }, { refund: false }); }, host);
  await expect(page.locator('#mount iframe')).toHaveAttribute('src', `${host}/pay/pk/orders/order-1?refund=false`);
  await expect.poll(() => statusRequests, { timeout: 5000 }).toBeGreaterThan(0);
});
