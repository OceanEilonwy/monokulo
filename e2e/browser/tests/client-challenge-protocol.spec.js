// @ts-check
const { test, expect } = require('../coverage-test');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '../../..');

function authoredAsset(name) {
  const directory = process.env.COVERAGE_ASSETS_DIR || path.join(root, 'crates/monokulo/static');
  return path.join(directory, name);
}

function challengeServer({ difficulty = 8 } = {}) {
  const http = require('node:http');
  const crypto = require('node:crypto');
  const state = { orderAttempts: [], orderKeys: [] };
  const challenge = 'c0ffee.coverage'; // First valid nonce is 419 at eight bits.
  const zeroBits = buf => {
    let bits = 0;
    for (const byte of buf) { if (byte === 0) { bits += 8; continue; } return bits + Math.clz32(byte) - 24; }
    return bits;
  };
  const solved = proof => {
    const at = proof.lastIndexOf('.');
    return proof.slice(0, at) === challenge && zeroBits(crypto.createHash('sha256').update(proof.slice(0, at) + proof.slice(at + 1)).digest()) >= difficulty;
  };
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const cors = { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'authorization, content-type, monokulo-proof', 'access-control-expose-headers': 'monokulo-challenge, retry-after' };
    if (url.pathname === '/static/monokulo-client.js') {
      res.writeHead(200, { 'content-type': 'text/javascript', ...cors });
      return res.end(fs.readFileSync(authoredAsset(url.pathname.slice('/static/'.length))));
    }
    if (url.pathname === '/merchant') {
      res.writeHead(200, { 'content-type': 'text/html' });
      return res.end(`<script src="http://checkout.localhost:${server.address().port}/static/monokulo-client.js"></script>`);
    }
    if (url.pathname === '/api/v1/orders') {
      if (req.method === 'OPTIONS') { res.writeHead(204, cors); return res.end(); }
      const proof = req.headers['monokulo-proof'];
      state.orderAttempts.push(proof || null);
      state.orderKeys.push(req.headers.authorization || null);
      if (proof && solved(proof)) {
        res.writeHead(201, { 'content-type': 'application/json', location: '/api/v1/orders/order-9', ...cors });
        return res.end(JSON.stringify({
          order_id: 'order-9', address: '8addr', xmr_amount_piconero: 1, amount: '1', currency: 'XMR', expires_at: 0,
          _links: { self: { href: '/api/v1/orders/order-9' }, 'mk:checkout': { href: '/checkout/order-9' } },
        }));
      }
      res.writeHead(429, { 'content-type': 'application/problem+json', 'monokulo-challenge': `${challenge}; difficulty=${difficulty}`, ...cors });
      return res.end(JSON.stringify({
        type: 'https://docs.monokulo.com/problems/challenge-required', title: 'Solve a challenge to continue', status: 429,
        detail: 'Too many requests', challenge: { challenge, difficulty, expires_in: 300 },
      }));
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
      monokulo: `http://checkout.localhost:${port}`, publicKey: 'pk', amount: 1, currency: 'XMR',
    }), port);
    expect(order.orderId).toBe('order-9');
    expect(order.checkoutUrl).toBe(`http://checkout.localhost:${port}/checkout/order-9`);
    expect(state.orderKeys).toEqual(['Bearer pk', 'Bearer pk']);
    expect(state.orderAttempts.length).toBe(2);
    expect(state.orderAttempts[0]).toBeNull();
    expect(state.orderAttempts[1]).toMatch(/^c0ffee\.coverage\.\d+$/);
  } finally {
    server.close();
  }
});
