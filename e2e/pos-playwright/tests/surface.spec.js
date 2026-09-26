// @ts-check
const { test, expect, installCoverageContext, collectCoverageContext } = require('../coverage-test');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '../../..');
const address = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
const qrImage = path.join(__dirname, '../fixtures/refund-qr.png');
const host = 'http://localhost:8787';

function authoredAsset(name) {
  const directory = process.env.COVERAGE_ASSETS_DIR || path.join(root, 'crates/monokulo/static');
  return path.join(directory, name);
}

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
      return route.fulfill({ contentType: 'text/javascript', body: fs.readFileSync(authoredAsset('checkout.js')) });
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
      return route.fulfill({ contentType: name.endsWith('.js') ? 'text/javascript' : 'text/css', body: fs.readFileSync(process.env.COVERAGE_ASSETS_DIR && name.endsWith('.js') ? authoredAsset(name) : path.join(root, 'crates/monokulo/static', name)) });
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
      return res.end(fs.readFileSync(authoredAsset(url.pathname.slice('/static/'.length))));
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
      await installCoverageContext(context);
      const page = await context.newPage();
      await page.goto(`http://shop.localhost:${port}/shop`);
      await expect(page.frameLocator('#checkout').locator('h1')).toHaveText('Checkout', { timeout: 20000 });
    } finally {
      await collectCoverageContext(context, test.info());
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
