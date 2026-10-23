// @ts-check
// The embed library from a Monokulo a reverse proxy serves under a path
// (`https://pay.example.com/monokulo/`): it takes Monokulo's address, path
// and all, from its own script URL, makes the order there, and frames the
// checkout at the link the order gives. Monokulo is stood in for by
// routes.
const { test, expect } = require('../coverage-test');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '../../..');
const MONOKULO = 'https://pay.example.com/monokulo';
const SHOP = 'https://shop.example.com';
const CORS = {
  'access-control-allow-origin': '*',
  'access-control-allow-headers': 'authorization, content-type, monokulo-proof',
  'access-control-expose-headers': 'location',
};

function library() {
  const directory = process.env.COVERAGE_ASSETS_DIR || path.join(root, 'crates/monokulo/static');
  return fs.readFileSync(path.join(directory, 'monokulo-client.js'));
}

test('a Monokulo served under a path is called, and framed, under it', async ({ page }) => {
  const calls = [];
  await page.route('**/*', async route => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin === SHOP) {
      return route.fulfill({ contentType: 'text/html', body: `<!doctype html><div id="pay"></div><script src="${MONOKULO}/static/monokulo-client.js"></script>` });
    }
    if (request.method() === 'OPTIONS') return route.fulfill({ status: 204, headers: CORS });
    if (url.pathname === '/monokulo/static/monokulo-client.js') {
      return route.fulfill({ contentType: 'text/javascript', headers: CORS, body: library() });
    }
    calls.push(`${request.method()} ${request.url()}`);
    if (url.pathname === '/monokulo/api/v1/orders' && request.method() === 'POST') {
      const links = { self: { href: '/monokulo/api/v1/orders/order_1' }, 'mk:checkout': { href: '/monokulo/checkout/order_1' } };
      return route.fulfill({
        status: 201, contentType: 'application/prs.hal-forms+json', headers: { ...CORS, location: '/monokulo/api/v1/orders/order_1' },
        body: JSON.stringify({ order_id: 'order_1', address: '8addr', xmr_amount_piconero: 1, amount: '1', currency: 'XMR', expires_at: 0, _links: links }),
      });
    }
    if (url.pathname.endsWith('/events')) {
      return route.fulfill({ contentType: 'text/event-stream', headers: CORS, body: 'retry: 60000\n\nevent: status\ndata: {"status":"pending"}\n\n' });
    }
    if (url.pathname.startsWith('/monokulo/checkout/')) {
      return route.fulfill({ contentType: 'text/html', body: '<!doctype html><h1>Checkout</h1>' });
    }
    return route.fulfill({ status: 404, headers: CORS, body: '' });
  });
  await page.goto(`${SHOP}/`);
  await page.waitForFunction(() => window.Monokulo);
  const order = await page.evaluate(async () => {
    const created = await window.Monokulo.createOrder({ publicKey: 'pk_test', amount: '1', currency: 'XMR' });
    window.Monokulo.mount('#pay', created);
    return created;
  });
  expect(order.endpoint).toBe(MONOKULO);
  expect(order.checkoutUrl).toBe(`${MONOKULO}/checkout/order_1`);
  expect(order.orderUrl).toBe(`${MONOKULO}/api/v1/orders/order_1`);
  expect(calls[0]).toBe(`POST ${MONOKULO}/api/v1/orders`);
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', new RegExp(`^${MONOKULO}/checkout/order_1`));
  expect(calls.every(call => call.includes('/monokulo/'))).toBe(true);
});
