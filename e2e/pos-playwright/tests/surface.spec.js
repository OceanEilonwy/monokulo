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
  const style = fs.readFileSync(path.join(root, 'crates/monokulo/static/pos-app.css'), 'utf8');
  const head = fs.readFileSync(path.join(root, 'crates/monokulo/src/views/head.html'), 'utf8');
  await page.setContent(`${head}<style>${style}</style>
    <div id="pos-root"><header class="pos-top"><span class="pos-store">rachelshandicrafts.com</span><strong>POS</strong><span class="pos-health"></span></header>
      <main class="pos-payment"><div class="pos-order-heading"><h1>Coffee</h1></div><div class="pos-checkout-card"><iframe title="Monero payment"></iframe></div><button class="pos-primary">Background order</button><button class="pos-cancel">Cancel order</button></main>
    </div>`);

  for (const { width, height } of [{ width: 1126, height: 700 }, { width: 667, height: 375 }, { width: 360, height: 740 }]) {
    await page.setViewportSize({ width, height });
    const bounds = await page.evaluate(() => {
      const bar = document.querySelector('.pos-top').getBoundingClientRect();
      const panel = document.querySelector('.pos-checkout-card').getBoundingClientRect();
      const frame = document.querySelector('.pos-checkout-card iframe').getBoundingClientRect();
      return { barBottom: bar.bottom, panelTop: panel.top, frameTop: frame.top, frameBottom: frame.bottom, viewportHeight: innerHeight };
    });
    expect(bounds.panelTop).toBeGreaterThanOrEqual(bounds.barBottom);
    expect(bounds.frameTop).toBeGreaterThanOrEqual(bounds.barBottom);
    if (height >= 620) {
      expect(bounds.frameBottom).toBeLessThanOrEqual(bounds.viewportHeight);
    } else {
      await page.locator('.pos-cancel').scrollIntoViewIfNeeded();
      await expect(page.locator('.pos-cancel')).toBeInViewport();
    }
  }
});

test('compact refund QR controls sit below the full-width input', async ({ page }) => {
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
        return { gap: button.top - input.bottom, width: button.width, inputWidth: input.width };
      });
    });
    for (const position of positions) {
      expect(position.gap).toBeGreaterThanOrEqual(0);
      expect(position.width).toBeLessThan(position.inputWidth);
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

test('POS backgrounds a pending order, restores it after reload, and reopens it', async ({ page }) => {
  let backgrounded = false;
  let cancelled = false;
  const order = {
    order_id: 'order-1', merchant_order_id: 'Mia coffee', amount: '1.00', currency: 'XMR',
    xmr_amount: '1.000000000000', address, status: 'pending', confirmations: 0,
    confirmations_required: 1, error: null, cancelled_at: null,
    created_at: 1000, expires_at: 9999999999,
  };
  await page.addInitScript(() => { window.EventSource = class { addEventListener() {} close() {} }; });
  await page.route(`${host}/**`, async route => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    if (pathname === '/static/pos-app.js' || pathname === '/static/pos-app.css') {
      const name = path.basename(pathname);
      return route.fulfill({ contentType: name.endsWith('.js') ? 'text/javascript' : 'text/css', body: fs.readFileSync(path.join(root, 'crates/monokulo/static', name)) });
    }
    if (pathname.endsWith('/pos/orders') && request.method() === 'POST') return route.fulfill({ json: order });
    if (pathname.endsWith('/pos/orders') && request.method() === 'GET') return route.fulfill({ json: { orders: backgrounded ? [{ ...order, backgrounded, cancelled_at: cancelled ? 2000 : null }] : [], total: backgrounded ? 1 : 0 } });
    if (pathname.endsWith('/pos/orders/order-1') && request.method() === 'GET') return route.fulfill({ json: { ...order, backgrounded, cancelled_at: cancelled ? 2000 : null } });
    if (pathname.endsWith('/pos/orders/order-1/background')) { backgrounded = true; return route.fulfill({ status: 204 }); }
    if (pathname.endsWith('/pos/orders/order-1/cancel')) { cancelled = true; return route.fulfill({ status: 204 }); }
    if (pathname.startsWith('/pay/')) return route.fulfill({ contentType: 'text/html', body: '<h1>Shared payment view</h1>' });
    return route.fulfill({ contentType: 'text/html', body: `<!doctype html><html><head><link rel="stylesheet" href="/static/pos-app.css"></head><body>
      <div id="pos-root" data-connection-id="conn-1" data-public-key="pk-1" data-currency="XMR" data-decimals="12" data-store-name="example.com"></div>
      <script type="module" src="/static/pos-app.js"></script></body></html>` });
  });
  await page.goto(`${host}/dashboard/stores/conn-1/pos`);
  await page.getByRole('button', { name: '1', exact: true }).click();
  await page.locator('#pos-reference').fill('Mia coffee');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-checkout-card iframe')).toHaveAttribute('src', '/pay/pk-1/orders/order-1?view=compact');
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toContainText('Mia coffee');
  await page.reload();
  await expect(page.locator('.pos-stack-card')).toBeVisible();
  await page.locator('.pos-stack-card').click();
  await expect(page.locator('.pos-checkout-card iframe')).toHaveAttribute('src', '/pay/pk-1/orders/order-1?view=compact');
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Cancelled');
  await expect(page.locator('.pos-checkout-card iframe')).toBeHidden();
  await page.getByRole('button', { name: 'New order' }).click();
  await page.getByRole('button', { name: 'View all →' }).click();
  await page.getByRole('tab', { name: /Finished/ }).click();
  await expect(page.locator('.pos-order-card .pos-badge')).toContainText('Cancelled');
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

test('a restricted store\'s framing header keeps its checkout off other websites', async ({ page }) => {
  // The header `EmbedPolicy::frame_ancestors` sends, for a store whose only
  // verified domain is the shop's (allowed) or another one (blocked).
  const header = domain => `frame-ancestors 'self' ${domain} ${domain.replace('://', '://*.')}`;
  await page.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.pathname === '/verified-here') {
      return route.fulfill({ contentType: 'text/html', headers: { 'content-security-policy': header('http://shop.localhost:8787') }, body: '<h1>Checkout</h1>' });
    }
    if (url.pathname === '/verified-elsewhere') {
      return route.fulfill({ contentType: 'text/html', headers: { 'content-security-policy': header('https://store-home.example') }, body: '<h1>Checkout</h1>' });
    }
    return route.fulfill({
      contentType: 'text/html',
      body: '<iframe id="here" src="http://checkout.localhost:8787/verified-here"></iframe>' +
        '<iframe id="elsewhere" src="http://checkout.localhost:8787/verified-elsewhere"></iframe>',
    });
  });
  await page.goto('http://shop.localhost:8787/');
  await expect(page.frameLocator('#here').locator('h1')).toHaveText('Checkout');
  // The browser refuses the other one outright and shows its error page instead.
  await expect.poll(() => page.frames().map(frame => frame.url())).toContain('chrome-error://chromewebdata/');
  await expect(page.frameLocator('#elsewhere').locator('h1')).toHaveCount(0);
});

test('a browser says whether it is loading a page as a frame, which the frame-only checkout rule relies on', async ({ page }) => {
  // A real server (not a routed fake), so the browser's own Sec-Fetch-Dest
  // header reaches it. It applies the same rule as monokulo's
  // `must_open_from_shop` for a restricted store's browser-created order:
  // render only for `iframe`/`frame` (or no header), refuse a full page.
  const http = require('node:http');
  const seen = [];
  const server = http.createServer((req, res) => {
    if (req.url === '/shop') {
      res.writeHead(200, { 'content-type': 'text/html' });
      return res.end(`<iframe id="checkout" src="http://checkout.localhost:${server.address().port}/pay"></iframe>`);
    }
    if (req.url !== '/pay') {
      res.writeHead(404);
      return res.end();
    }
    const dest = req.headers['sec-fetch-dest'];
    seen.push(dest);
    const framed = dest === undefined || dest === 'iframe' || dest === 'frame';
    res.writeHead(framed ? 200 : 403, { 'content-type': 'text/html' });
    res.end(framed ? '<h1>Checkout</h1>' : "<h1>Open this payment from the shop's website</h1>");
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  try {
    await page.goto(`http://checkout.localhost:${port}/pay`);
    await expect(page.locator('h1')).toHaveText("Open this payment from the shop's website");
    await page.goto(`http://shop.localhost:${port}/shop`);
    await expect(page.frameLocator('#checkout').locator('h1')).toHaveText('Checkout');
    expect(seen).toEqual(['document', 'iframe']);
  } finally {
    server.close();
  }
});

// --- Abuse protection: the "Checking your connection" interstitial ------
//
// A small real HTTP server stands in for monokulo's challenge protocol
// (crates/monokulo/src/abuse/challenge.rs, http/abuse.rs): the first visit to
// /pay gets the interstitial (same markup contract as
// views::challenge::challenge_page, whose Rust test pins it), a correct
// `monokulo_proof` or a wait token presented after 10 seconds redirects back
// to /pay, which then shows the checkout. The real challenge.js and
// monokulo-client.js are served from the repo.
function challengeServer({ difficulty = 8 } = {}) {
  const http = require('node:http');
  const crypto = require('node:crypto');
  const state = { passed: false, waitIssuedAt: 0, orderAttempts: [] };
  const challenge = 'c0ffee.' + crypto.randomBytes(8).toString('hex');
  const zeroBits = buf => {
    let bits = 0;
    for (const byte of buf) { if (byte === 0) { bits += 8; continue; } return bits + Math.clz32(byte) - 24; }
    return bits;
  };
  const solved = proof => {
    const at = proof.lastIndexOf('.');
    return proof.slice(0, at) === challenge && zeroBits(crypto.createHash('sha256').update(proof.slice(0, at) + proof.slice(at + 1)).digest()) >= difficulty;
  };
  const interstitial = () => `<!doctype html><html><head><title>Checking your connection</title>
    <noscript><meta http-equiv="refresh" content="10;url=/pay?monokulo_wait=tok"></noscript></head><body>
    <main id="challenge" aria-busy="true" data-challenge="${challenge}" data-difficulty="${difficulty}" data-continue="/pay" data-wait="/pay?monokulo_wait=tok">
    <h1>Checking your connection</h1>
    <noscript><p role="status">Checking your connection, this page continues in 10 seconds.</p></noscript>
    <p id="challenge-progress" role="status" aria-live="polite" hidden>Checking your connection…</p></main>
    <script src="/static/challenge.js"></script></body></html>`;
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const cors = { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'content-type, monokulo-proof', 'access-control-expose-headers': 'monokulo-challenge, retry-after' };
    if (url.pathname === '/static/challenge.js' || url.pathname === '/static/monokulo-client.js') {
      res.writeHead(200, { 'content-type': 'text/javascript', ...cors });
      return res.end(fs.readFileSync(path.join(root, 'crates/monokulo/static', url.pathname.slice('/static/'.length))));
    }
    if (url.pathname === '/shop') {
      res.writeHead(200, { 'content-type': 'text/html' });
      return res.end(`<iframe id="checkout" src="http://checkout.localhost:${server.address().port}/pay"></iframe>`);
    }
    if (url.pathname === '/merchant') {
      res.writeHead(200, { 'content-type': 'text/html' });
      return res.end(`<script src="http://checkout.localhost:${server.address().port}/static/monokulo-client.js"></script>`);
    }
    if (url.pathname === '/pay/pk/orders') {
      if (req.method === 'OPTIONS') { res.writeHead(204, cors); return res.end(); }
      const proof = req.headers['monokulo-proof'];
      state.orderAttempts.push(proof || null);
      if (proof && solved(proof)) {
        res.writeHead(200, { 'content-type': 'application/json', ...cors });
        return res.end(JSON.stringify({ order_id: 'order-9', address: '8addr', xmr_amount_piconero: 1, amount: '1', currency: 'XMR', expires_at: 0 }));
      }
      res.writeHead(429, { 'content-type': 'application/json', 'monokulo-challenge': `${challenge}; difficulty=${difficulty}`, ...cors });
      return res.end(JSON.stringify({ error: 'Too many requests', challenge: { challenge, difficulty, expires_in: 300 } }));
    }
    if (url.pathname === '/pay') {
      const proof = url.searchParams.get('monokulo_proof');
      const wait = url.searchParams.get('monokulo_wait');
      if ((proof && solved(proof)) || (wait === 'tok' && Date.now() - state.waitIssuedAt >= 10000)) {
        state.passed = true;
        res.writeHead(303, { location: '/pay' });
        return res.end();
      }
      if (state.passed) {
        res.writeHead(200, { 'content-type': 'text/html' });
        return res.end('<h1>Checkout</h1>');
      }
      if (!state.waitIssuedAt) state.waitIssuedAt = Date.now();
      res.writeHead(429, { 'content-type': 'text/html', 'cache-control': 'no-store' });
      return res.end(interstitial());
    }
    res.writeHead(404);
    res.end();
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve({ server, state, port: server.address().port })));
}

test('the interstitial solves its challenge with JavaScript and continues by itself', async ({ page }) => {
  const { server, port } = await challengeServer();
  try {
    await page.goto(`http://checkout.localhost:${port}/pay`);
    await expect(page.locator('h1')).toHaveText('Checkout', { timeout: 10000 });
    expect(new URL(page.url()).search).toBe('');
  } finally {
    server.close();
  }
});

test('without JavaScript the interstitial waits ten seconds and continues', async ({ browser }) => {
  test.setTimeout(40000);
  const { server, port } = await challengeServer();
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(`http://checkout.localhost:${port}/pay`);
    await expect(page.getByRole('status').first()).toContainText('continues in 10 seconds');
    await expect(page.locator('h1')).toHaveText('Checkout', { timeout: 20000 });
  } finally {
    await context.close();
    server.close();
  }
});

test('the interstitial works inside a cross-site frame, with and without JavaScript', async ({ browser }) => {
  test.setTimeout(40000);
  for (const javaScriptEnabled of [true, false]) {
    const { server, port } = await challengeServer();
    const context = await browser.newContext({ javaScriptEnabled });
    try {
      const page = await context.newPage();
      await page.goto(`http://shop.localhost:${port}/shop`);
      await expect(page.frameLocator('#checkout').locator('h1')).toHaveText('Checkout', { timeout: 20000 });
    } finally {
      await context.close();
      server.close();
    }
  }
});

test('monokulo-client.js solves an order-creation challenge on its own', async ({ page }) => {
  const { server, state, port } = await challengeServer();
  try {
    await page.goto(`http://shop.localhost:${port}/merchant`);
    const order = await page.evaluate(port => window.Monokulo.createOrder({
      endpoint: `http://checkout.localhost:${port}`, publicKey: 'pk', amount: 1, currency: 'XMR',
    }), port);
    expect(order.orderId).toBe('order-9');
    expect(state.orderAttempts.length).toBe(2);
    expect(state.orderAttempts[0]).toBeNull();
    expect(state.orderAttempts[1]).toMatch(/^c0ffee\.[0-9a-f]+\.\d+$/);
  } finally {
    server.close();
  }
});
