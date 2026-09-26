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
